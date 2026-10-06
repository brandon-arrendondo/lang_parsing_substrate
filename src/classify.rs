//! Cheap, **heuristic** file-type classification, so a consumer can exit early
//! before handing a file to tree-sitter.
//!
//! [`language_for_file`](crate::language_for_file) dispatches purely on the
//! extension, so today a 2 GB zip named `foo.c` goes straight to the C parser.
//! [`classify`] looks at a bounded prefix of the file's bytes (never the whole
//! file) plus its size, and answers one of:
//!
//! - [`FileClass::Empty`] — zero-length file.
//! - [`FileClass::Binary`] — a recognised magic number (zip, gzip, xz, zstd,
//!   tar, ELF, PE, Mach-O, PNG, JPEG, PDF, … — the table is the [`infer`]
//!   crate's) or a byte distribution that does not look like text (NUL bytes,
//!   C0 control bytes, a high invalid-UTF-8 ratio).
//! - [`FileClass::Oversize`] — looks like text, but is larger than the
//!   caller-supplied limit.
//! - [`FileClass::SourceText`] — looks like text, with its probable
//!   [`TextEncoding`] and language hints.
//!
//! # This is a heuristic, and it is wrong both ways
//!
//! A text file can be called binary (e.g. a source file with a run of
//! embedded NUL bytes, or UTF-16 without a BOM), and a binary file can be
//! called text (e.g. a format with no magic number `infer` knows and
//! mostly-ASCII content, or a tiny PDF with no binary streams). Treat the result as a cheap early-exit
//! signal, not a verdict.
//!
//! The substrate deliberately sets **no policy**: whether a `Binary` or
//! `Oversize` result means "skip and report" or "try anyway" is the caller's
//! decision, and so is the size limit ([`ClassifyLimits::max_size`] defaults
//! to no limit).
//!
//! # Language hints
//!
//! [`FileClass::SourceText`] carries two independent hints: the language the
//! *extension* implies (via the registry) and the language the *content*
//! implies (a `#!` shebang line or a leading `<?php`). Disagreement between
//! the two is reported, never acted on — a `.c` file whose first line is
//! `#!/usr/bin/env python3` is still `SourceText`, and only the caller decides
//! what that means. Only languages compiled into this build are ever named.
//! For the `.h` C-vs-C++ question, use
//! `language_for_header_content` (with `lang-c` + `lang-cpp`) instead.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use crate::registry::{language_info_for_file, languages, LanguageInfo};

/// Suggested prefix length for [`classify_file`]: 8 KiB, the same window git
/// uses for its binary-file check. Callers may pass any length; longer
/// prefixes make the byte-distribution ratios more reliable.
pub const DEFAULT_PREFIX_LEN: usize = 8 * 1024;

/// What [`classify`] thinks a file is. See the [module docs](self) for why
/// this is only a hint.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum FileClass {
    /// The file is zero bytes long.
    Empty,
    /// The file looks like text but exceeds the caller's size limit.
    /// Binary files are reported as [`FileClass::Binary`] regardless of size.
    Oversize {
        /// The file's size in bytes.
        size: u64,
        /// The caller-supplied limit it exceeded.
        limit: u64,
    },
    /// The file does not look like text.
    Binary {
        /// The kind of binary; [`BinaryKind::Unknown`] if no magic number
        /// identified it.
        kind: BinaryKind,
        /// MIME type from the magic-number match (e.g. `"application/zip"`),
        /// for reporting; `None` when only the byte distribution decided.
        mime: Option<&'static str>,
    },
    /// The file looks like text.
    SourceText(SourceText),
}

impl FileClass {
    /// `true` for [`FileClass::SourceText`] — the only class worth parsing
    /// under the most conservative policy.
    pub fn is_source_text(&self) -> bool {
        matches!(self, FileClass::SourceText(_))
    }
}

/// The kind of a [`FileClass::Binary`] file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BinaryKind {
    /// Multi-file archive: zip (incl. jar/apk), tar, 7z, rar, `ar` (static
    /// libraries), deb, rpm, cab, cpio.
    Archive,
    /// Single-stream compressed data: gzip, xz, zstd, bzip2, lz4, lzip, `.Z`.
    Compressed,
    /// ELF executable, shared object, or object file.
    Elf,
    /// Windows PE/COFF (`MZ` … `PE\0\0`) executable or DLL.
    Pe,
    /// Mach-O executable (thin or fat/universal).
    MachO,
    /// Image: PNG, JPEG, GIF, WebP, TIFF, BMP, ICO, PSD, …
    Image,
    /// PDF document.
    Pdf,
    /// Office / e-book document: doc/xls/ppt, docx/xlsx/pptx, odt/ods/odp,
    /// epub, mobi.
    Document,
    /// Audio or video.
    Media,
    /// Font file: woff, woff2, ttf, otf.
    Font,
    /// Some other recognised binary (wasm, Java class, sqlite, …), or no
    /// magic number matched but the byte distribution is not text-like.
    Unknown,
}

/// The probable encoding of a [`FileClass::SourceText`] file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TextEncoding {
    /// Valid UTF-8 (which includes pure ASCII), no BOM.
    Utf8,
    /// Valid UTF-8 after a leading `EF BB BF` byte-order mark.
    Utf8Bom,
    /// UTF-16 little-endian, identified by its `FF FE` BOM. (BOM-less UTF-16
    /// is indistinguishable from binary here and classifies as `Binary`.)
    Utf16Le,
    /// UTF-16 big-endian, identified by its `FE FF` BOM.
    Utf16Be,
    /// Not valid UTF-8, but text-like: some legacy 8-bit encoding. Decoding
    /// as Latin-1 (ISO-8859-1) is lossless, though the true encoding may be
    /// Windows-1252, another ISO-8859 part, or a multi-byte legacy encoding.
    Latin1,
}

/// Details of a [`FileClass::SourceText`] result.
#[derive(Debug, Clone, Copy)]
pub struct SourceText {
    /// Probable encoding of the bytes.
    pub encoding: TextEncoding,
    /// The language the file's extension maps to, if any (registry lookup).
    pub by_extension: Option<&'static LanguageInfo>,
    /// The language the file's content suggests (shebang / `<?php`), if any.
    pub by_content: Option<&'static LanguageInfo>,
}

impl SourceText {
    /// Best single guess: the extension's language, else the content's.
    pub fn likely_language(&self) -> Option<&'static LanguageInfo> {
        self.by_extension.or(self.by_content)
    }

    /// `Some(true)` / `Some(false)` when both hints exist and agree /
    /// disagree; `None` when either is missing. A hint only — never a gate.
    pub fn extension_agrees(&self) -> Option<bool> {
        Some(self.by_extension?.key == self.by_content?.key)
    }
}

/// Caller policy for [`classify`] / [`classify_file`].
#[derive(Debug, Clone, Copy)]
pub struct ClassifyLimits {
    /// How many leading bytes [`classify_file`] reads. Ignored by
    /// [`classify`], which takes whatever prefix it is given.
    pub prefix_len: usize,
    /// Text files larger than this many bytes are reported as
    /// [`FileClass::Oversize`]. `None` (the default) means no limit.
    pub max_size: Option<u64>,
}

impl Default for ClassifyLimits {
    fn default() -> Self {
        Self {
            prefix_len: DEFAULT_PREFIX_LEN,
            max_size: None,
        }
    }
}

/// Classifies a file from its path, a bounded prefix of its bytes, and its
/// total size. **Heuristic** — see the [module docs](self).
///
/// `prefix` must be the file's first bytes (any length; [`DEFAULT_PREFIX_LEN`]
/// is a reasonable choice) and `file_size` its full length in bytes. `path` is
/// used only for the extension hint — no I/O happens here.
///
/// Order of checks: zero size → `Empty`; strong magic number → `Binary`;
/// byte distribution (NUL ratio, control-byte ratio, invalid-UTF-8 ratio)
/// not text-like → `Binary` (kind from a weak magic hit, else `Unknown`);
/// size above `limits.max_size` → `Oversize`; otherwise `SourceText`. A
/// UTF-16 BOM skips the byte-distribution check. Binary always wins over
/// `Oversize`, so a huge archive is reported as an archive.
///
/// Magic numbers come from the [`infer`] crate's table. Its text-format
/// matchers (HTML, XML, `#!`, PEM, RTF, PostScript) are ignored, and its
/// short ASCII-looking signatures are only trusted alongside binary-looking
/// bytes, since e.g. a source file can legitimately start with `MZ` or `BM`.
pub fn classify(path: &Path, prefix: &[u8], file_size: u64, limits: &ClassifyLimits) -> FileClass {
    if file_size == 0 {
        return FileClass::Empty;
    }
    let magic = magic(prefix);
    if let Some(m) = magic.as_ref().filter(|m| m.strong) {
        return binary(m.kind, Some(m.mime));
    }

    let truncated = (prefix.len() as u64) < file_size;
    let (encoding, body) = if let Some(rest) = prefix.strip_prefix(b"\xEF\xBB\xBF") {
        (TextEncoding::Utf8Bom, rest)
    } else if prefix.starts_with(b"\xFF\xFE") {
        (TextEncoding::Utf16Le, &[][..])
    } else if prefix.starts_with(b"\xFE\xFF") {
        (TextEncoding::Utf16Be, &[][..])
    } else {
        (TextEncoding::Utf8, prefix)
    };

    let stats = ByteStats::of(body, truncated);
    if stats.looks_binary() {
        return match magic {
            Some(m) => binary(m.kind, Some(m.mime)),
            None => binary(BinaryKind::Unknown, None),
        };
    }
    if let Some(limit) = limits.max_size {
        if file_size > limit {
            return FileClass::Oversize {
                size: file_size,
                limit,
            };
        }
    }
    let encoding = if stats.invalid_utf8 == 0 {
        encoding
    } else {
        TextEncoding::Latin1
    };
    FileClass::SourceText(SourceText {
        encoding,
        by_extension: language_info_for_file(path),
        by_content: content_language(body),
    })
}

fn binary(kind: BinaryKind, mime: Option<&'static str>) -> FileClass {
    FileClass::Binary { kind, mime }
}

/// Reads `path`'s metadata and at most `limits.prefix_len` leading bytes,
/// then calls [`classify`]. Never reads past the prefix.
///
/// Returns an `InvalidInput` error for anything that is not a regular file
/// (directories, FIFOs, devices), since reading those can block or never end.
pub fn classify_file(path: &Path, limits: &ClassifyLimits) -> io::Result<FileClass> {
    let file = File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    let mut prefix = Vec::with_capacity(limits.prefix_len.min(meta.len() as usize));
    file.take(limits.prefix_len as u64)
        .read_to_end(&mut prefix)?;
    Ok(classify(path, &prefix, meta.len(), limits))
}

// ---------------------------------------------------------------------------
// Magic numbers (via `infer`)
// ---------------------------------------------------------------------------

/// A magic-number hit from `infer`.
struct Magic {
    kind: BinaryKind,
    mime: &'static str,
    /// `true` when the signature contains bytes no text file starts with
    /// (NUL, C0 controls, invalid UTF-8), so the hit is trusted outright.
    /// Weak hits — `infer` matches `MZ`, `BM`, `GIF`, `BZh`, `ustar` at 257,
    /// `%PDF-` anywhere in the first KiB, … — only count when the byte
    /// distribution independently looks binary.
    strong: bool,
}

/// `infer` extensions whose signatures are strong (see [`Magic::strong`]).
const STRONG: &[&str] = &[
    "zip", "docx", "xlsx", "pptx", "odt", "ods", "odp", "epub", "ora", "gz", "xz", "zst", "lz4",
    "7z", "rar", "rpm", "doc", "xls", "ppt", "msi", "elf", "mach", "class", "wasm", "png", "jpg",
    "woff", "woff2", "mkv", "webm",
];

fn magic(prefix: &[u8]) -> Option<Magic> {
    let t = infer::get(prefix)?;
    let ext = t.extension();
    let kind = match (t.matcher_type(), ext) {
        // Text formats `infer` also recognises (html, xml, `#!` scripts, PEM,
        // RTF, PostScript): not binary — let the byte statistics decide.
        (infer::MatcherType::Text, _) | (_, "pem" | "rtf" | "ps") => return None,
        (_, "elf") => BinaryKind::Elf,
        (_, "exe" | "dll") => BinaryKind::Pe,
        (_, "mach") => BinaryKind::MachO,
        (_, "pdf") => BinaryKind::Pdf,
        (_, "gz" | "bz2" | "bz3" | "xz" | "zst" | "lz4" | "Z" | "lz") => BinaryKind::Compressed,
        (
            _,
            "zip" | "tar" | "rar" | "7z" | "ar" | "deb" | "rpm" | "cab" | "crx" | "cpio" | "par2",
        ) => BinaryKind::Archive,
        (infer::MatcherType::Image, _) => BinaryKind::Image,
        (infer::MatcherType::Doc | infer::MatcherType::Book, _) => BinaryKind::Document,
        (infer::MatcherType::Audio | infer::MatcherType::Video, _) => BinaryKind::Media,
        (infer::MatcherType::Font, _) => BinaryKind::Font,
        _ => BinaryKind::Unknown,
    };
    // `infer` accepts `%PDF-` anywhere in the first KiB; only offset 0 is strong.
    let strong = STRONG.contains(&ext) || (ext == "pdf" && prefix.starts_with(b"%PDF-"));
    Some(Magic {
        kind,
        mime: t.mime_type(),
        strong,
    })
}

// ---------------------------------------------------------------------------
// Byte distribution
// ---------------------------------------------------------------------------

struct ByteStats {
    len: usize,
    nul: usize,
    /// C0 controls other than \t \n \v \f \r, ESC and SUB (DOS EOF), plus DEL.
    control: usize,
    invalid_utf8: usize,
}

impl ByteStats {
    fn of(b: &[u8], truncated: bool) -> Self {
        let mut nul = 0;
        let mut control = 0;
        for &c in b {
            match c {
                0 => nul += 1,
                b'\t' | b'\n' | 0x0B | 0x0C | b'\r' | 0x1A | 0x1B => {}
                0x01..=0x1F | 0x7F => control += 1,
                _ => {}
            }
        }
        Self {
            len: b.len(),
            nul,
            control,
            invalid_utf8: invalid_utf8_bytes(b, truncated),
        }
    }

    /// Thresholds (as fractions of the prefix):
    /// - NUL > 0.1% — one stray NUL in an 8 KiB source file passes; random or
    ///   structured binary data (~0.4% NUL when uniform) does not.
    /// - other control bytes > 5% — uniform random data is ~10%.
    /// - invalid UTF-8 > 30% *and* any NUL/control byte — legacy multi-byte
    ///   text (GBK, Shift-JIS) can be mostly invalid UTF-8 but has no control
    ///   bytes, while binary data essentially always has some.
    fn looks_binary(&self) -> bool {
        let len = self.len;
        self.nul * 1000 > len
            || self.control * 100 > len * 5
            || (self.invalid_utf8 * 100 > len * 30 && self.nul + self.control > 0)
    }
}

/// Counts bytes that are not part of a valid UTF-8 sequence. When the prefix
/// is `truncated` (shorter than the file), an incomplete sequence at the very
/// end is the cut, not an error.
fn invalid_utf8_bytes(mut b: &[u8], truncated: bool) -> usize {
    let mut invalid = 0;
    loop {
        match std::str::from_utf8(b) {
            Ok(_) => return invalid,
            Err(e) => {
                let rest = &b[e.valid_up_to()..];
                match e.error_len() {
                    Some(n) => {
                        invalid += n;
                        b = &rest[n..];
                    }
                    None => {
                        if !truncated {
                            invalid += rest.len();
                        }
                        return invalid;
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Content language hint
// ---------------------------------------------------------------------------

fn content_language(body: &[u8]) -> Option<&'static LanguageInfo> {
    let key = if body.starts_with(b"#!") {
        let line_end = body.iter().position(|&c| c == b'\n').unwrap_or(body.len());
        shebang_key(&String::from_utf8_lossy(&body[2..line_end]))?
    } else if body.trim_ascii_start().starts_with(b"<?php") {
        "php"
    } else {
        return None;
    };
    languages().iter().find(|l| l.key == key)
}

/// Maps a shebang line (without `#!`) to a registry key: takes the
/// interpreter's basename — or, for `env`, the first non-flag argument — and
/// strips trailing version digits (`python3.12` → `python`).
fn shebang_key(line: &str) -> Option<&'static str> {
    let mut words = line.split_whitespace();
    let mut interp = words.next()?.rsplit('/').next()?;
    if interp == "env" {
        interp = words.find(|w| !w.starts_with('-'))?;
    }
    let name = interp.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    Some(match name {
        "python" | "pypy" => "python",
        "node" | "nodejs" | "bun" => "javascript",
        "deno" | "ts-node" | "tsx" => "typescript",
        "lua" | "luajit" => "lua",
        "php" => "php",
        "swift" => "swift",
        "kotlin" | "kotlinc" | "kscript" => "kotlin",
        "scala" => "scala",
        "java" => "java",
        "rust-script" => "rust",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(name: &str, bytes: &[u8]) -> FileClass {
        classify(
            Path::new(name),
            bytes,
            bytes.len() as u64,
            &ClassifyLimits::default(),
        )
    }

    fn binary_kind(name: &str, bytes: &[u8]) -> Option<BinaryKind> {
        match class(name, bytes) {
            FileClass::Binary { kind, .. } => Some(kind),
            _ => None,
        }
    }

    fn text(name: &str, bytes: &[u8]) -> SourceText {
        match class(name, bytes) {
            FileClass::SourceText(t) => t,
            other => panic!("{name}: expected SourceText, got {other:?}"),
        }
    }

    /// Deterministic pseudo-random bytes (xorshift), standing in for
    /// compressed / encrypted payloads after a header.
    fn noise(n: usize) -> Vec<u8> {
        let mut x: u32 = 0x9E37_79B9;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect()
    }

    fn with_header(header: &[u8]) -> Vec<u8> {
        let mut v = header.to_vec();
        v.extend(noise(4096));
        v
    }

    fn tar() -> Vec<u8> {
        let mut v = vec![0u8; 512];
        v[..10].copy_from_slice(b"hello.txt\0");
        v[257..263].copy_from_slice(b"ustar\0");
        v.extend(b"hello\n");
        v
    }

    fn pe() -> Vec<u8> {
        let mut v = vec![0u8; 0x80];
        v[..2].copy_from_slice(b"MZ");
        v[0x3C] = 0x80;
        v.extend(b"PE\0\0");
        v.extend(noise(512));
        v
    }

    #[test]
    fn every_binary_kind_renamed_to_a_source_extension() {
        let cases: Vec<(Vec<u8>, BinaryKind)> = vec![
            (with_header(b"PK\x03\x04"), BinaryKind::Archive),
            (tar(), BinaryKind::Archive),
            (with_header(b"!<arch>\n"), BinaryKind::Archive),
            (with_header(b"7z\xBC\xAF\x27\x1C"), BinaryKind::Archive),
            (with_header(b"\x1F\x8B\x08\x00"), BinaryKind::Compressed),
            (with_header(b"\xFD7zXZ\x00"), BinaryKind::Compressed),
            (with_header(b"\x28\xB5\x2F\xFD"), BinaryKind::Compressed),
            (with_header(b"BZh91AY&SY"), BinaryKind::Compressed),
            (with_header(b"\x7FELF\x02\x01\x01"), BinaryKind::Elf),
            (pe(), BinaryKind::Pe),
            (with_header(b"\xCF\xFA\xED\xFE"), BinaryKind::MachO),
            (
                with_header(b"\xCA\xFE\xBA\xBE\x00\x00\x00\x02"),
                BinaryKind::MachO,
            ),
            (with_header(b"\x89PNG\r\n\x1A\n"), BinaryKind::Image),
            (with_header(b"\xFF\xD8\xFF\xE0"), BinaryKind::Image),
            (with_header(b"GIF89a"), BinaryKind::Image),
            (with_header(b"RIFF\0\0\0\0WEBPVP8 "), BinaryKind::Image),
            (with_header(b"%PDF-1.7\n"), BinaryKind::Pdf),
            (
                with_header(b"\xCA\xFE\xBA\xBE\x00\x00\x00\x41"),
                BinaryKind::Unknown,
            ),
            (noise(8192), BinaryKind::Unknown),
        ];
        for (bytes, want) in &cases {
            for name in ["evil.c", "evil.rs", "evil.py"] {
                assert_eq!(
                    binary_kind(name, bytes),
                    Some(*want),
                    "{name} with header {:02X?}",
                    &bytes[..8]
                );
            }
        }
    }

    #[test]
    fn unidentified_noise_is_binary() {
        for name in ["evil.c", "evil.rs", "evil.py"] {
            assert!(binary_kind(name, &noise(8192)).is_some(), "{name}");
        }
        assert!(matches!(
            class("x.c", &[0x7F; 64]),
            FileClass::Binary {
                kind: BinaryKind::Unknown,
                mime: None
            }
        ));
    }

    #[test]
    fn binary_reports_mime() {
        assert!(matches!(
            class("a.c", &with_header(b"PK\x03\x04")),
            FileClass::Binary {
                mime: Some("application/zip"),
                ..
            }
        ));
    }

    /// `infer` signatures that are plausible starts of real source files
    /// must not turn text into `Binary` on their own.
    #[test]
    fn weak_or_text_signatures_on_text_stay_text() {
        let cases: &[(&str, &[u8])] = &[
            ("mz.py", b"MZ = 3\nprint(MZ)\n"),                // PE "MZ"
            ("bm.py", b"BM_SIZE = 4\n"),                      // BMP "BM"
            ("bc.c", b"BC_MAX = 1;\n"),                       // LLVM bitcode "BC"
            ("g.py", b"GIF_DIR = 'x'\n"),                     // GIF "GIF"
            ("b.py", b"BZh = 1\n"),                           // bzip2 "BZh"
            ("fws.py", b"FWS = 2\n"),                         // SWF "FWS"
            ("pdf.c", b"const char *h = \"%PDF-1.4\";\n"),    // %PDF- not at 0
            ("ps.c", b"%!\n"),                                // PostScript
            ("sh.rs", b"#!/bin/sh\necho\n"),                  // shell script
            ("page.php", b"<html><?php echo 1; ?></html>\n"), // HTML
            ("x.c", b"<?xml version=\"1.0\"?>\n"),            // XML
            ("k.c", b"-----BEGIN CERTIFICATE-----\nMII\n"),   // PEM
        ];
        for (name, src) in cases {
            assert!(class(name, src).is_source_text(), "{name}");
        }
    }

    #[test]
    fn binary_beats_oversize() {
        // The motivating case: a 2 GB zip named .c, with a 1 MiB limit.
        let limits = ClassifyLimits {
            max_size: Some(1 << 20),
            ..Default::default()
        };
        let got = classify(
            Path::new("huge.c"),
            &with_header(b"PK\x03\x04"),
            2 << 30,
            &limits,
        );
        assert!(matches!(
            got,
            FileClass::Binary {
                kind: BinaryKind::Archive,
                ..
            }
        ));
    }

    #[test]
    fn weak_magic_binary_beats_oversize() {
        // "MZ" is a weak signature, but a real PE's NULs confirm it.
        let limits = ClassifyLimits {
            max_size: Some(1 << 20),
            ..Default::default()
        };
        let got = classify(Path::new("huge.c"), &pe(), 2 << 30, &limits);
        assert!(matches!(
            got,
            FileClass::Binary {
                kind: BinaryKind::Pe,
                ..
            }
        ));
    }

    #[test]
    fn utf8_source() {
        let t = text(
            "main.rs",
            "fn main() { println!(\"héllo — ✓\"); }\n".as_bytes(),
        );
        assert_eq!(t.encoding, TextEncoding::Utf8);
        #[cfg(feature = "lang-rust")]
        assert_eq!(t.likely_language().unwrap().key, "rust");
    }

    #[test]
    fn latin1_source() {
        // "/* Grüße, café */" in ISO-8859-1.
        let src = b"/* Gr\xFC\xDFe, caf\xE9 */\nint main(void) { return 0; }\n";
        assert_eq!(text("main.c", src).encoding, TextEncoding::Latin1);
    }

    #[test]
    fn legacy_multibyte_text_is_not_binary() {
        // GBK-encoded comment: mostly invalid UTF-8, but no control bytes.
        let mut src = b"// ".to_vec();
        for _ in 0..200 {
            src.extend(b"\xD6\xD0\xCE\xC4");
        }
        src.extend(b"\nint x;\n");
        assert_eq!(text("gbk.c", &src).encoding, TextEncoding::Latin1);
    }

    #[test]
    fn boms() {
        let t = text("bom.py", b"\xEF\xBB\xBFprint('hi')\n");
        assert_eq!(t.encoding, TextEncoding::Utf8Bom);
        let t = text("w.c", b"\xFF\xFEi\0n\0t\0 \0x\0;\0");
        assert_eq!(t.encoding, TextEncoding::Utf16Le);
        let t = text("w.c", b"\xFE\xFF\0i\0n\0t");
        assert_eq!(t.encoding, TextEncoding::Utf16Be);
        // BOM-less UTF-16 is a known false "binary".
        assert_eq!(
            binary_kind("w.c", b"i\0n\0t\0 \0x\0;\0"),
            Some(BinaryKind::Unknown)
        );
    }

    #[test]
    fn large_source_at_caller_limit() {
        let line = b"int f(int x) { return x + 1; }\n";
        let src: Vec<u8> = line.iter().copied().cycle().take(64 * 1024).collect();
        let size = src.len() as u64;
        let prefix = &src[..DEFAULT_PREFIX_LEN];
        let at = ClassifyLimits {
            max_size: Some(size),
            ..Default::default()
        };
        assert!(classify(Path::new("big.c"), prefix, size, &at).is_source_text());
        let under = ClassifyLimits {
            max_size: Some(size - 1),
            ..Default::default()
        };
        assert!(matches!(
            classify(Path::new("big.c"), prefix, size, &under),
            FileClass::Oversize { size: s, limit: l } if s == size && l == size - 1
        ));
        // No limit: never Oversize.
        assert!(classify(
            Path::new("big.c"),
            prefix,
            u64::MAX,
            &ClassifyLimits::default()
        )
        .is_source_text());
    }

    #[test]
    fn empty_file() {
        assert!(matches!(class("empty.c", b""), FileClass::Empty));
    }

    #[test]
    fn truncated_prefix_mid_codepoint_is_still_utf8() {
        let src = "// ✓✓✓\n".as_bytes();
        let cut = &src[..5]; // "// " + first 2 bytes of ✓
        let got = classify(Path::new("a.c"), cut, 100, &ClassifyLimits::default());
        match got {
            FileClass::SourceText(t) => assert_eq!(t.encoding, TextEncoding::Utf8),
            other => panic!("{other:?}"),
        }
        // ...but at true EOF the same bytes are invalid.
        assert_eq!(text("a.c", cut).encoding, TextEncoding::Latin1);
    }

    #[test]
    fn single_stray_nul_in_large_source_is_text() {
        let mut src: Vec<u8> = b"int x;\n".iter().copied().cycle().take(4096).collect();
        src[100] = 0;
        assert!(class("nul.c", &src).is_source_text());
    }

    #[test]
    fn shebang_hint_and_agreement() {
        let t = text("tool", b"#!/usr/bin/env python3\nprint(1)\n");
        assert!(t.by_extension.is_none());
        #[cfg(feature = "lang-python")]
        {
            assert_eq!(t.likely_language().unwrap().key, "python");
            assert_eq!(t.extension_agrees(), None);
            let t = text("tool.py", b"#!/usr/bin/python3.12 -u\nprint(1)\n");
            assert_eq!(t.extension_agrees(), Some(true));
        }
        #[cfg(all(feature = "lang-python", feature = "lang-c"))]
        {
            // Disagreement is a hint only: still SourceText, extension wins.
            let t = text("odd.c", b"#!/usr/bin/env -S python3 -u\nprint(1)\n");
            assert_eq!(t.extension_agrees(), Some(false));
            assert_eq!(t.likely_language().unwrap().key, "c");
        }
        #[cfg(feature = "lang-php")]
        assert_eq!(
            text("page", b"\n<?php echo 1;").by_content.unwrap().key,
            "php"
        );
        assert!(text("x.c", b"#!/bin/sh\n").by_content.is_none());
    }

    #[test]
    fn classify_file_reads_bounded_prefix() {
        let dir = std::env::temp_dir().join(format!("lps-classify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let zip = dir.join("evil.c");
        std::fs::write(&zip, with_header(b"PK\x03\x04")).unwrap();
        let src = dir.join("ok.c");
        std::fs::write(&src, b"int main(void) { return 0; }\n").unwrap();
        let empty = dir.join("empty.c");
        std::fs::write(&empty, b"").unwrap();
        let limits = ClassifyLimits {
            prefix_len: 16,
            max_size: None,
        };
        assert!(matches!(
            classify_file(&zip, &limits).unwrap(),
            FileClass::Binary {
                kind: BinaryKind::Archive,
                ..
            }
        ));
        assert!(classify_file(&src, &limits).unwrap().is_source_text());
        assert!(matches!(
            classify_file(&empty, &limits).unwrap(),
            FileClass::Empty
        ));
        assert_eq!(
            classify_file(&dir, &limits).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
