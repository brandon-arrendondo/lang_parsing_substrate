//! Calibration run for `classify`: classifies every file under one or more
//! corpus roots and prints **aggregates only** (no file lists), so it can be
//! run on private codebases and the output shared.
//!
//! ```text
//! cargo run --release --example classify_calibrate -- [--show-failures] ROOT...
//! ```
//!
//! A ROOT that is a git checkout is scanned with `git ls-files` (exactly the
//! tracked, pinned source); anything else is walked recursively without
//! following symlinks. Prefix `binary:` to a ROOT (e.g. `binary:/tmp/planted`)
//! to declare that every file under it is a binary: those files must all come
//! back `Binary`.
//!
//! The gate for text roots: no C-family file (`.c .h .inc .cpp .cc .cxx .hpp
//! .hxx`) is classified `Binary`. For C-family files that come back as
//! `SourceText`, the run also reports the maximum NUL / control /
//! invalid-UTF-8 counts and ratios seen, which is the headroom the
//! thresholds in `classify` must keep.
//!
//! `--show-failures` prints the paths of gate failures to stderr. Leave it
//! off when the corpus is private.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use lang_parsing_substrate::classify::__byte_counts;
use lang_parsing_substrate::{
    classify, language_info_for_file, ClassifyLimits, FileClass, DEFAULT_PREFIX_LEN,
};

const C_FAMILY: &[&str] = &["c", "h", "inc", "cpp", "cc", "cxx", "hpp", "hxx"];

#[derive(Default)]
struct Max {
    count: usize,
    ratio: f64,
}

impl Max {
    fn see(&mut self, n: usize, len: usize) {
        self.count = self.count.max(n);
        if len > 0 {
            self.ratio = self.ratio.max(n as f64 / len as f64);
        }
    }
}

#[derive(Default)]
struct Tally {
    files: usize,
    unreadable: usize,
    /// class name -> (C-family count, other count)
    classes: BTreeMap<&'static str, (usize, usize)>,
    c_text: usize,
    c_text_with_nul: usize,
    c_text_with_control: usize,
    c_text_with_invalid: usize,
    max_nul: Max,
    max_control: Max,
    max_invalid: Max,
    gate_failures: usize,
    /// Non-C files with a registered source extension that came back `Binary`.
    other_source_binary: usize,
    /// Extension -> count, for every `Binary` result in a text root.
    binary_exts: BTreeMap<String, usize>,
}

fn class_name(c: &FileClass) -> &'static str {
    match c {
        FileClass::Empty => "Empty",
        FileClass::Oversize { .. } => "Oversize",
        FileClass::Binary { .. } => "Binary",
        FileClass::SourceText(_) => "SourceText",
        _ => "Other",
    }
}

fn list_files(root: &Path) -> Vec<PathBuf> {
    if root.join(".git").exists() {
        if let Ok(out) = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["ls-files", "-z"])
            .output()
        {
            if out.status.success() {
                return out
                    .stdout
                    .split(|&b| b == 0)
                    .filter(|p| !p.is_empty())
                    .map(|p| root.join(String::from_utf8_lossy(p).as_ref()))
                    .collect();
            }
        }
    }
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() && e.file_name() != ".git" {
                stack.push(e.path());
            } else if ft.is_file() {
                files.push(e.path());
            }
        }
    }
    files
}

fn read_prefix(path: &Path) -> Option<(Vec<u8>, u64)> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() {
        return None; // symlinks, gitlinks (submodules), FIFOs: not scanned
    }
    let mut buf = Vec::new();
    File::open(path)
        .ok()?
        .take(DEFAULT_PREFIX_LEN as u64)
        .read_to_end(&mut buf)
        .ok()?;
    Some((buf, meta.len()))
}

fn scan(root: &Path, expect_binary: bool, show_failures: bool) -> Tally {
    let limits = ClassifyLimits::default();
    let mut t = Tally::default();
    for path in list_files(root) {
        let Some((prefix, size)) = read_prefix(&path) else {
            t.unreadable += 1;
            continue;
        };
        t.files += 1;
        let is_c = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| C_FAMILY.contains(&e.to_ascii_lowercase().as_str()));
        let class = classify(&path, &prefix, size, &limits);
        let entry = t.classes.entry(class_name(&class)).or_default();
        if is_c {
            entry.0 += 1;
        } else {
            entry.1 += 1;
        }

        let failed = if expect_binary {
            !matches!(class, FileClass::Binary { .. })
        } else {
            is_c && matches!(class, FileClass::Binary { .. })
        };
        if failed {
            t.gate_failures += 1;
            if show_failures {
                eprintln!("GATE FAILURE: {} -> {class:?}", path.display());
            }
        }

        if !expect_binary && matches!(class, FileClass::Binary { .. }) {
            let ext = path.extension().map_or_else(
                || "(none)".into(),
                |e| e.to_string_lossy().to_ascii_lowercase(),
            );
            *t.binary_exts.entry(ext).or_default() += 1;
            if !is_c && language_info_for_file(&path).is_some() {
                t.other_source_binary += 1;
                if show_failures {
                    eprintln!("NOTE non-C source -> Binary: {}", path.display());
                }
            }
        }

        if is_c && class.is_source_text() {
            let (len, nul, control, invalid) = __byte_counts(&prefix, size);
            t.c_text += 1;
            t.c_text_with_nul += usize::from(nul > 0);
            t.c_text_with_control += usize::from(control > 0);
            t.c_text_with_invalid += usize::from(invalid > 0);
            t.max_nul.see(nul, len);
            t.max_control.see(control, len);
            t.max_invalid.see(invalid, len);
        }
    }
    t
}

fn main() {
    let mut show_failures = false;
    let mut roots = Vec::new();
    for arg in std::env::args().skip(1) {
        if arg == "--show-failures" {
            show_failures = true;
        } else {
            roots.push(arg);
        }
    }
    if roots.is_empty() {
        eprintln!("usage: classify_calibrate [--show-failures] [binary:]ROOT...");
        std::process::exit(2);
    }

    let mut total_failures = 0;
    for arg in &roots {
        let (expect_binary, root) = match arg.strip_prefix("binary:") {
            Some(r) => (true, r),
            None => (false, arg.as_str()),
        };
        let root = Path::new(root);
        let name = root.file_name().map_or_else(
            || root.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let t = scan(root, expect_binary, show_failures);
        total_failures += t.gate_failures;

        let kind = if expect_binary {
            "planted-binary"
        } else {
            "text"
        };
        println!(
            "== {name} ({kind}): {} files, {} skipped",
            t.files, t.unreadable
        );
        for (class, (c, other)) in &t.classes {
            println!("   {class:<10} C-family {c:>7}   other {other:>7}");
        }
        if expect_binary {
            println!("   gate (all Binary): {} failure(s)", t.gate_failures);
        } else {
            println!(
                "   gate (no C-family Binary): {} failure(s)",
                t.gate_failures
            );
            println!(
                "   other registered-source extensions classified Binary: {}",
                t.other_source_binary
            );
            let mut exts: Vec<_> = t.binary_exts.iter().collect();
            exts.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            let top: Vec<String> = exts
                .iter()
                .take(12)
                .map(|(e, n)| format!("{e}:{n}"))
                .collect();
            if !top.is_empty() {
                println!("   Binary by extension (top 12): {}", top.join(" "));
            }
            println!(
                "   C-family SourceText: {} files; with any NUL {}, control {}, invalid UTF-8 {}",
                t.c_text, t.c_text_with_nul, t.c_text_with_control, t.c_text_with_invalid
            );
            println!(
                "   max per file:  NUL {} ({:.4}%)  control {} ({:.4}%)  invalid UTF-8 {} ({:.4}%)",
                t.max_nul.count,
                t.max_nul.ratio * 100.0,
                t.max_control.count,
                t.max_control.ratio * 100.0,
                t.max_invalid.count,
                t.max_invalid.ratio * 100.0
            );
        }
    }
    println!("TOTAL gate failures: {total_failures}");
    if total_failures > 0 {
        std::process::exit(1);
    }
}
