use std::path::Path;
use std::sync::OnceLock;
use tree_sitter::Language;

/// Which comment style a language uses (drives SLOC calculation).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
// vec![] can't express per-element #[cfg(...)] gates, so languages() below must
// stay Vec::new() + push despite clippy::vec_init_then_push.
#[allow(clippy::vec_init_then_push)]
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
            explicit_only: &[],
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
        Some("cpp" | "cc" | "cxx" | "hpp" | "hxx") => Some(tree_sitter_cpp::LANGUAGE.into()),

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

        #[cfg(feature = "lang-scala")]
        Some("scala" | "sc") => Some(tree_sitter_scala::LANGUAGE.into()),

        #[cfg(feature = "lang-lua")]
        Some("lua") => Some(tree_sitter_lua::LANGUAGE.into()),

        #[cfg(feature = "lang-c")]
        Some("c" | "h") => Some(tree_sitter_c::LANGUAGE.into()),

        _ => None,
    }
}

/// Best-effort content-aware variant of [`language_for_file`] for callers
/// that already have the file's bytes in hand (e.g. because they're about
/// to parse it anyway). Identical to [`language_for_file`] for every
/// extension except `.h`: a `.h` file is ambiguous between C and C++, and
/// this checks `source` for an unambiguous C++-only construct
/// ([`crate::looks_like_cpp`]) before falling back to `language_for_file`'s
/// default of C. See that function's module docs for what counts as
/// unambiguous and why a "no" doesn't mean "confirmed C".
#[cfg(all(feature = "lang-c", feature = "lang-cpp"))]
pub fn language_for_header_content(path: &Path, source: &[u8]) -> Option<Language> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("h") if crate::cpp_header::looks_like_cpp(source) => {
            Some(tree_sitter_cpp::LANGUAGE.into())
        }
        _ => language_for_file(path),
    }
}

/// Returns the tree-sitter `Language` for the given canonical language key.
///
/// The key is the [`LanguageInfo::key`] value defined by this registry — e.g.
/// `"c"`, `"cpp"`, `"rust"`. Returns `None` if the key is unknown or the
/// corresponding language feature was not compiled in.
///
/// This is the complement to [`language_for_file`]: use it when you already
/// have a key (from [`LanguageInfo::key`]) and want the grammar without
/// constructing a fake file path.
pub fn language_for_key(key: &str) -> Option<Language> {
    match key {
        #[cfg(feature = "lang-ada")]
        "ada" => Some(tree_sitter_ada::LANGUAGE.into()),

        #[cfg(feature = "lang-c")]
        "c" => Some(tree_sitter_c::LANGUAGE.into()),

        #[cfg(feature = "lang-cpp")]
        "cpp" => Some(tree_sitter_cpp::LANGUAGE.into()),

        #[cfg(feature = "lang-csharp")]
        "csharp" => Some(tree_sitter_c_sharp::LANGUAGE.into()),

        #[cfg(feature = "lang-fortran")]
        "fortran" => Some(tree_sitter_fortran::LANGUAGE.into()),

        #[cfg(feature = "lang-go")]
        "go" => Some(tree_sitter_go::LANGUAGE.into()),

        #[cfg(feature = "lang-java")]
        "java" => Some(tree_sitter_java::LANGUAGE.into()),

        #[cfg(feature = "lang-javascript")]
        "javascript" => Some(tree_sitter_javascript::LANGUAGE.into()),

        #[cfg(feature = "lang-kotlin")]
        "kotlin" => Some(tree_sitter_kotlin_ng::LANGUAGE.into()),

        #[cfg(feature = "lang-lua")]
        "lua" => Some(tree_sitter_lua::LANGUAGE.into()),

        #[cfg(feature = "lang-php")]
        "php" => Some(tree_sitter_php::LANGUAGE_PHP.into()),

        #[cfg(feature = "lang-python")]
        "python" => Some(tree_sitter_python::LANGUAGE.into()),

        #[cfg(feature = "lang-rust")]
        "rust" => Some(tree_sitter_rust::LANGUAGE.into()),

        #[cfg(feature = "lang-scala")]
        "scala" => Some(tree_sitter_scala::LANGUAGE.into()),

        #[cfg(feature = "lang-swift")]
        "swift" => Some(tree_sitter_swift::LANGUAGE.into()),

        #[cfg(feature = "lang-typescript")]
        "typescript" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),

        #[cfg(feature = "lang-typescript")]
        "tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),

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

/// Returns the [`SlocMode`] for `path`.
///
/// Returns `None` if the extension is unknown or the corresponding language
/// feature was not compiled in. Consumers that want a fallback should use
/// `sloc_mode_for_file(path).unwrap_or(SlocMode::Default)`.
pub fn sloc_mode_for_file(path: &Path) -> Option<SlocMode> {
    language_info_for_file(path).map(|l| l.sloc_mode)
}

/// Returns `true` if `ext` is in the recursive-discovery set for any
/// compiled-in language.
pub fn is_source_extension(ext: &std::ffi::OsStr) -> bool {
    ext.to_str()
        .map(|e| languages().iter().any(|l| l.extensions.contains(&e)))
        .unwrap_or(false)
}

/// Returns `true` if the substrate can parse files with `ext` — includes both
/// recursive-discovery extensions and explicit-only ones (headers, fixed-form
/// Fortran).
pub fn is_parseable_extension(ext: &std::ffi::OsStr) -> bool {
    ext.to_str()
        .map(|e| {
            languages()
                .iter()
                .any(|l| l.extensions.contains(&e) || l.explicit_only.contains(&e))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Every registered extension (recursive and explicit-only) must map to a
    /// grammar via `language_for_file`, and only C extensions may map to the C
    /// grammar. Guards against an extension silently failing to dispatch.
    #[cfg(feature = "lang-c")]
    #[test]
    fn every_registered_extension_maps_to_its_grammar() {
        let c_lang: Language = tree_sitter_c::LANGUAGE.into();
        for lang in languages() {
            for ext in lang.extensions.iter().chain(lang.explicit_only) {
                let mapped = language_for_file(Path::new(&format!("f.{ext}")))
                    .unwrap_or_else(|| panic!(".{ext} ({}) did not map to a grammar", lang.name));
                if lang.key != "c" {
                    assert_ne!(
                        mapped, c_lang,
                        ".{ext} ({}) fell through to the C grammar",
                        lang.name
                    );
                }
            }
        }
    }

    #[test]
    fn unknown_extension_has_no_language() {
        assert!(language_for_file(Path::new("notes.txt")).is_none());
        assert!(sloc_mode_for_file(Path::new("notes.txt")).is_none());
        assert!(language_for_file(Path::new("noext")).is_none());
    }

    #[cfg(feature = "lang-fortran")]
    #[test]
    fn fortran_sloc_mode() {
        for ext in ["f90", "f95", "F90"] {
            assert_eq!(
                sloc_mode_for_file(Path::new(&format!("modern.{ext}"))),
                Some(SlocMode::Fortran),
                ".{ext} should be free-form Fortran",
            );
        }
    }

    #[cfg(feature = "lang-python")]
    #[test]
    fn python_sloc_mode() {
        assert_eq!(
            sloc_mode_for_file(Path::new("a.py")),
            Some(SlocMode::Python)
        );
    }

    /// Every `LanguageInfo.key` must be lowercase ASCII and unique — consumers
    /// use it as a config-section name (e.g. `[funky.cpp]`) and would silently
    /// collide on duplicates or mismatched case.
    #[test]
    fn keys_are_lowercase_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for lang in languages() {
            assert!(
                lang.key.chars().all(|c| c.is_ascii_lowercase()),
                "{} key {:?} is not lowercase ASCII",
                lang.name,
                lang.key
            );
            assert!(
                seen.insert(lang.key),
                "duplicate LanguageInfo.key {:?}",
                lang.key
            );
        }
    }

    /// `language_info_for_file` must agree with `language_for_file` on every
    /// registered extension — same set of extensions resolve in both, with no
    /// grammar requirement.
    #[test]
    fn language_info_for_file_matches_extensions() {
        for lang in languages() {
            for ext in lang.extensions.iter().chain(lang.explicit_only) {
                let info =
                    language_info_for_file(Path::new(&format!("f.{ext}"))).unwrap_or_else(|| {
                        panic!(".{ext} ({}) did not resolve to a LanguageInfo", lang.name)
                    });
                assert_eq!(
                    info.key, lang.key,
                    ".{ext} resolved to the wrong LanguageInfo"
                );
            }
        }
        assert!(language_info_for_file(Path::new("notes.txt")).is_none());
        assert!(language_info_for_file(Path::new("noext")).is_none());
    }

    /// `is_source_extension` covers only recursive-discovery extensions;
    /// `is_parseable_extension` additionally covers explicit-only ones
    /// (headers, etc). An explicit-only extension must never be reported as a
    /// source (discoverable) extension.
    #[test]
    fn source_vs_parseable_extension_distinction() {
        for lang in languages() {
            for ext in lang.extensions {
                let os_ext = std::ffi::OsStr::new(ext);
                assert!(
                    is_source_extension(os_ext),
                    ".{ext} ({}) should be a source extension",
                    lang.name
                );
                assert!(
                    is_parseable_extension(os_ext),
                    ".{ext} ({}) should be parseable",
                    lang.name
                );
            }
            for ext in lang.explicit_only {
                let os_ext = std::ffi::OsStr::new(ext);
                assert!(
                    !is_source_extension(os_ext),
                    ".{ext} ({}) is explicit-only and must not be a source extension",
                    lang.name
                );
                assert!(
                    is_parseable_extension(os_ext),
                    ".{ext} ({}) should still be parseable",
                    lang.name
                );
            }
        }
        assert!(!is_source_extension(std::ffi::OsStr::new("txt")));
        assert!(!is_parseable_extension(std::ffi::OsStr::new("txt")));
    }

    /// `language_for_key` must round-trip with `LanguageInfo.key` for every
    /// compiled-in language: `language_for_key(info.key)` must return the same
    /// `Language` as `language_for_file` on a file with one of that language's
    /// registered extensions.
    #[test]
    fn language_for_key_round_trips_with_language_info() {
        for lang in languages() {
            let by_key = language_for_key(lang.key).unwrap_or_else(|| {
                panic!(
                    "language_for_key({:?}) returned None for compiled-in language {}",
                    lang.key, lang.name
                )
            });
            // Pick any registered extension to get the canonical Language via
            // the file-based path and verify both routes agree.
            let ext = lang
                .extensions
                .first()
                .or_else(|| lang.explicit_only.first())
                .unwrap();
            let by_file = language_for_file(Path::new(&format!("f.{ext}"))).unwrap_or_else(|| {
                panic!(
                    ".{ext} ({}) did not resolve via language_for_file",
                    lang.name
                )
            });
            assert_eq!(
                by_key, by_file,
                "language_for_key({:?}) disagrees with language_for_file for {}",
                lang.key, lang.name
            );
        }
    }

    /// Keys not in the registry return None.
    #[test]
    fn language_for_key_unknown_returns_none() {
        assert!(language_for_key("").is_none());
        assert!(language_for_key("txt").is_none());
        assert!(language_for_key("C").is_none()); // case-sensitive
    }

    /// The report must mention every compiled-in language's name and every one
    /// of its recursive-discovery extensions, with explicit-only extensions
    /// called out separately.
    #[test]
    fn supported_languages_report_lists_every_language() {
        let report = supported_languages_report();
        for lang in languages() {
            assert!(
                report.contains(lang.name),
                "report missing language {}",
                lang.name
            );
            for ext in lang.extensions {
                assert!(
                    report.contains(&format!(".{ext}")),
                    "report missing extension .{ext} for {}",
                    lang.name
                );
            }
        }
    }
}
