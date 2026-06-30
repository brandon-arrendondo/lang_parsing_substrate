use std::path::Path;
use std::sync::OnceLock;
use tree_sitter::Language;

/// Which comment style a language uses (drives SLOC calculation).
#[derive(Clone, Copy, PartialEq)]
pub enum SlocMode {
    /// `//` and `/* */` — C, C++, Rust, JS, TS, Go, Java, …
    Default,
    /// Additionally strips `#`-prefixed comment lines.
    Python,
    /// Strips `--` comment lines.
    Ada,
    /// Strips `--` comment lines (same prefix as Ada, distinct grammar).
    Lua,
    /// Strips `!` comment lines (free-form Fortran: `.f90`, `.f95`, …).
    Fortran,
    /// Strips fixed-form comment lines: `*`/`C`/`c` at column 1, plus `!` anywhere.
    FortranFixed,
}

/// A language the substrate can parse, with its display name, file extensions,
/// and comment style.
pub struct LanguageInfo {
    /// Human-facing name, e.g. "C++", "Ada".
    pub name: &'static str,
    /// Canonical machine key — matches the Cargo feature suffix and config-file
    /// section names (e.g. `[funky.cpp]`, `[knots.csharp.thresholds]`).
    /// Always lowercase ASCII; e.g. `"cpp"`, `"csharp"`, `"javascript"`.
    pub key: &'static str,
    /// Extensions used during recursive discovery (no leading dot).
    pub extensions: &'static [&'static str],
    /// Extensions parsed only when a file is passed explicitly — never
    /// discovered recursively (e.g. headers, fixed-form Fortran).
    pub explicit_only: &'static [&'static str],
    /// Comment style for this language.
    pub sloc_mode: SlocMode,
}

/// Returns the set of languages compiled into this build.
///
/// The returned slice reflects only the languages enabled via Cargo features
/// at compile time. Iterate this to discover what the current binary supports
/// rather than hard-coding extension lists in each tool.
pub fn languages() -> &'static [LanguageInfo] {
    static LANGS: OnceLock<Vec<LanguageInfo>> = OnceLock::new();
    LANGS.get_or_init(|| {
        #[allow(unused_mut)]
        let mut v: Vec<LanguageInfo> = Vec::new();

        #[cfg(feature = "lang-c")]
        v.push(LanguageInfo {
            name: "C",
            key: "c",
            extensions: &["c"],
            explicit_only: &["h"],
            sloc_mode: SlocMode::Default,
        });

        #[cfg(feature = "lang-cpp")]
        v.push(LanguageInfo {
            name: "C++",
            key: "cpp",
            extensions: &["cpp", "cc", "cxx", "hpp", "hxx"],
            explicit_only: &[],
            sloc_mode: SlocMode::Default,
        });

        #[cfg(feature = "lang-rust")]
        v.push(LanguageInfo {
            name: "Rust",
            key: "rust",
            extensions: &["rs"],
            explicit_only: &[],
            sloc_mode: SlocMode::Default,
        });

        #[cfg(feature = "lang-python")]
        v.push(LanguageInfo {
            name: "Python",
            key: "python",
            extensions: &["py"],
            explicit_only: &[],
            sloc_mode: SlocMode::Python,
        });

        #[cfg(feature = "lang-javascript")]
        v.push(LanguageInfo {
            name: "JavaScript",
            key: "javascript",
            extensions: &["js", "mjs", "cjs", "jsx"],
            explicit_only: &[],
            sloc_mode: SlocMode::Default,
        });

        #[cfg(feature = "lang-typescript")]
        v.push(LanguageInfo {
            name: "TypeScript",
            key: "typescript",
            extensions: &["ts", "tsx"],
            explicit_only: &[],
            sloc_mode: SlocMode::Default,
        });

        #[cfg(feature = "lang-ada")]
        v.push(LanguageInfo {
            name: "Ada",
            key: "ada",
            extensions: &["adb", "ada"],
            explicit_only: &["ads"],
            sloc_mode: SlocMode::Ada,
        });

        #[cfg(feature = "lang-go")]
        v.push(LanguageInfo {
            name: "Go",
            key: "go",
            extensions: &["go"],
            explicit_only: &[],
            sloc_mode: SlocMode::Default,
        });

        #[cfg(feature = "lang-java")]
        v.push(LanguageInfo {
            name: "Java",
            key: "java",
            extensions: &["java"],
            explicit_only: &[],
            sloc_mode: SlocMode::Default,
        });

        #[cfg(feature = "lang-csharp")]
        v.push(LanguageInfo {
            name: "C#",
            key: "csharp",
            extensions: &["cs"],
            explicit_only: &[],
            sloc_mode: SlocMode::Default,
        });

        #[cfg(feature = "lang-kotlin")]
        v.push(LanguageInfo {
            name: "Kotlin",
            key: "kotlin",
            extensions: &["kt", "kts"],
            explicit_only: &[],
            sloc_mode: SlocMode::Default,
        });

        #[cfg(feature = "lang-swift")]
        v.push(LanguageInfo {
            name: "Swift",
            key: "swift",
            extensions: &["swift"],
            explicit_only: &[],
            sloc_mode: SlocMode::Default,
        });

        #[cfg(feature = "lang-php")]
        v.push(LanguageInfo {
            name: "PHP",
            key: "php",
            extensions: &["php"],
            explicit_only: &[],
            sloc_mode: SlocMode::Default,
        });

        #[cfg(feature = "lang-fortran")]
        v.push(LanguageInfo {
            name: "Fortran",
            key: "fortran",
            extensions: &["f90", "f95", "f03", "f08", "F90", "F95", "F03", "F08"],
            explicit_only: &["f", "for", "f77", "F", "FOR", "F77"],
            sloc_mode: SlocMode::Fortran,
        });

        #[cfg(feature = "lang-scala")]
        v.push(LanguageInfo {
            name: "Scala",
            key: "scala",
            extensions: &["scala", "sc"],
            explicit_only: &[],
            sloc_mode: SlocMode::Default,
        });

        #[cfg(feature = "lang-lua")]
        v.push(LanguageInfo {
            name: "Lua",
            key: "lua",
            extensions: &["lua"],
            explicit_only: &[],
            sloc_mode: SlocMode::Lua,
        });

        v
    })
}

/// Returns the tree-sitter `Language` for `path` based on its extension.
///
/// Returns `None` if the extension is unknown or if the corresponding language
/// feature was not compiled in.
pub fn language_for_file(path: &Path) -> Option<Language> {
    match path.extension().and_then(|e| e.to_str()) {
        #[cfg(feature = "lang-ada")]
        Some("adb" | "ada" | "ads") => Some(tree_sitter_ada::LANGUAGE.into()),

        #[cfg(feature = "lang-cpp")]
        Some("cpp" | "cc" | "cxx" | "hpp" | "hxx") => {
            Some(tree_sitter_cpp::LANGUAGE.into())
        }

        #[cfg(feature = "lang-rust")]
        Some("rs") => Some(tree_sitter_rust::LANGUAGE.into()),

        #[cfg(feature = "lang-python")]
        Some("py") => Some(tree_sitter_python::LANGUAGE.into()),

        #[cfg(feature = "lang-javascript")]
        Some("js" | "mjs" | "cjs" | "jsx") => Some(tree_sitter_javascript::LANGUAGE.into()),

        #[cfg(feature = "lang-typescript")]
        Some("ts") => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),

        #[cfg(feature = "lang-typescript")]
        Some("tsx") => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),

        #[cfg(feature = "lang-go")]
        Some("go") => Some(tree_sitter_go::LANGUAGE.into()),

        #[cfg(feature = "lang-java")]
        Some("java") => Some(tree_sitter_java::LANGUAGE.into()),

        #[cfg(feature = "lang-csharp")]
        Some("cs") => Some(tree_sitter_c_sharp::LANGUAGE.into()),

        #[cfg(feature = "lang-kotlin")]
        Some("kt" | "kts") => Some(tree_sitter_kotlin_ng::LANGUAGE.into()),

        #[cfg(feature = "lang-swift")]
        Some("swift") => Some(tree_sitter_swift::LANGUAGE.into()),

        #[cfg(feature = "lang-php")]
        Some("php") => Some(tree_sitter_php::LANGUAGE_PHP.into()),

        #[cfg(feature = "lang-fortran")]
        Some("f90" | "f95" | "f03" | "f08" | "F90" | "F95" | "F03" | "F08") => {
            Some(tree_sitter_fortran::LANGUAGE.into())
        }

        #[cfg(feature = "lang-fortran")]
        Some("f" | "for" | "f77" | "F" | "FOR" | "F77") => {
            Some(tree_sitter_fixed_form_fortran::LANGUAGE.into())
        }

        #[cfg(feature = "lang-scala")]
        Some("scala" | "sc") => Some(tree_sitter_scala::LANGUAGE.into()),

        #[cfg(feature = "lang-lua")]
        Some("lua") => Some(tree_sitter_lua::LANGUAGE.into()),

        #[cfg(feature = "lang-c")]
        Some("c" | "h") => Some(tree_sitter_c::LANGUAGE.into()),

        _ => None,
    }
}

/// Returns the [`LanguageInfo`] for `path` based on its extension.
///
/// Unlike [`language_for_file`], this does not require a tree-sitter grammar —
/// it is suitable for tools (e.g. formatters) that dispatch per-language
/// without parsing. Returns `None` if the extension is unknown or the
/// corresponding language feature was not compiled in.
pub fn language_info_for_file(path: &Path) -> Option<&'static LanguageInfo> {
    let ext = path.extension().and_then(|e| e.to_str())?;
    languages()
        .iter()
        .find(|l| l.extensions.contains(&ext) || l.explicit_only.contains(&ext))
}

/// Returns `true` if `ext` is in the recursive-discovery set for any
/// compiled-in language.
pub fn is_source_extension(ext: &std::ffi::OsStr) -> bool {
    ext.to_str()
        .map(|e| {
            languages()
                .iter()
                .any(|l| l.extensions.contains(&e))
        })
        .unwrap_or(false)
}

/// Returns `true` if the substrate can parse files with `ext` — includes both
/// recursive-discovery extensions and explicit-only ones (headers, fixed-form
/// Fortran).
pub fn is_parseable_extension(ext: &std::ffi::OsStr) -> bool {
    ext.to_str()
        .map(|e| {
            languages().iter().any(|l| {
                l.extensions.contains(&e) || l.explicit_only.contains(&e)
            })
        })
        .unwrap_or(false)
}

/// Renders a human-readable summary of compiled-in languages, suitable for
/// a `--supported-languages` flag.
pub fn supported_languages_report() -> String {
    let langs = languages();
    let width = langs.iter().map(|l| l.name.len()).max().unwrap_or(0);
    let mut out = String::from("Supported languages:\n");
    for lang in langs {
        let exts = lang
            .extensions
            .iter()
            .map(|e| format!(".{e}"))
            .collect::<Vec<_>>()
            .join(" ");
        out.push_str(&format!("  {:<width$}  {exts}", lang.name, width = width));
        if !lang.explicit_only.is_empty() {
            let extra = lang
                .explicit_only
                .iter()
                .map(|e| format!(".{e}"))
                .collect::<Vec<_>>()
                .join(" ");
            out.push_str(&format!("  (also {extra} when passed explicitly)"));
        }
        out.push('\n');
    }
    out
}
