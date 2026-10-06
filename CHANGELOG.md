# Changelog

All notable changes to this crate are documented here.

## Unreleased

### Changed

- `Cargo.toml` description and the `Suppression::tool` / `hash` doc comments
  now name the current consumers: moldy (replaced funky), aurora-lint
  (formerly sqc / tools_sqc), and clew.

### Added

- `classify` module: cheap, **heuristic** pre-parse file classification so
  consumers can exit early before parsing (e.g. a 2 GB zip named `.c`).
  - `classify(path, prefix, file_size, &ClassifyLimits) -> FileClass` is
    I/O-free. `classify_file(path, &ClassifyLimits)` reads metadata and at most
    `prefix_len` bytes (default `DEFAULT_PREFIX_LEN` = 8 KiB). It rejects
    non-regular files.
  - `FileClass::{Empty, Binary { kind, mime }, Oversize { size, limit },
    SourceText(SourceText)}`. `BinaryKind` covers archive, compressed, ELF, PE,
    Mach-O, image, PDF, document, media, font and unknown. `TextEncoding`
    covers UTF-8, UTF-8 with BOM, UTF-16 LE/BE (BOM only) and Latin-1 (legacy
    8-bit).
  - Signals: the magic-number table of the `infer` crate (new dependency,
    `default-features = false`, no transitive deps), plus the ratios of NUL
    bytes, control bytes and invalid UTF-8 bytes. Text formats `infer` knows
    (HTML, XML, `#!`, PEM, RTF, PostScript) are ignored. Weak ASCII-like
    signatures only count when the bytes also look binary.
  - Language hints: `SourceText::by_extension` (registry) and `by_content`
    (shebang / `<?php`), with `likely_language()` and `extension_agrees()`.
    These are hints only and never gate the result.
  - Known limitations are listed in the `classify` module docs. Among them:
    the decision uses the prefix only, the thresholds were reasoned rather
    than measured on a corpus, BOM-less UTF-16 and all UTF-32 come back as
    `Binary`, and without `infer`'s `std` feature every OLE2 file reports as
    `Document`.
  - No policy is built in. The caller sets the size limit (default: none) and
    decides what to do with `Binary` and `Oversize`.
