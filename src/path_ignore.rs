//! External-config path ignoring — glob patterns such as `toolchain.toml`'s
//! `[ignore].paths` (per-tool `[<tool>.ignore]` sections are proposed).
//!
//! One glob-matching implementation, used by moldy, aurora-lint and clew's
//! Python side. See `docs/unified-config-spec.md`.

use globset::{Glob, GlobSet, GlobSetBuilder};
use std::borrow::Cow;
use std::path::Path;

/// A compiled set of glob ignore patterns, ready for repeated `is_ignored`
/// lookups against candidate paths.
pub struct PathIgnore {
    set: GlobSet,
}

impl PathIgnore {
    /// Compiles `patterns` (e.g. `"vendor/**"`, `"third_party/**"`) into a
    /// matcher. Returns an error if any pattern is not a valid glob.
    pub fn new<I, S>(patterns: I) -> Result<Self, globset::Error>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut builder = GlobSetBuilder::new();
        for pattern in patterns {
            builder.add(Glob::new(pattern.as_ref())?);
        }
        Ok(Self {
            set: builder.build()?,
        })
    }

    /// Like [`PathIgnore::new`], but a pattern's file extension matches in
    /// any ASCII case, the way file discovery treats extensions: `*.c` and
    /// `src/*.c` also ignore `A.C` and `src/B.C`. Only the extension folds.
    /// Directory names and the rest of the file name stay case-sensitive, so
    /// `Vendor/**` still does not match `vendor/x.c`.
    ///
    /// The extension is the text after the last `.` of the pattern's last
    /// component, when that `.` is not its first character. It folds only
    /// when it is plain text (letters, digits, `_`, `-`, `+`); an extension
    /// with glob syntax in it, such as `*.{c,h}` or `*.[ch]`, is compiled as
    /// written. Each pattern is checked as written before folding, so an
    /// invalid glob is an error here just as in [`PathIgnore::new`].
    pub fn with_extension_case_folded<I, S>(patterns: I) -> Result<Self, globset::Error>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut folded = Vec::new();
        for pattern in patterns {
            let pattern = pattern.as_ref();
            // Folding an invalid glob (`[/1.a`) can yield a valid one.
            Glob::new(pattern)?;
            folded.push(fold_extension_case(pattern).into_owned());
        }
        Self::new(folded)
    }

    /// An ignore set that matches nothing — the default when no `[ignore]`
    /// config is present.
    pub fn empty() -> Self {
        Self {
            set: GlobSetBuilder::new()
                .build()
                .expect("empty GlobSet always builds"),
        }
    }

    /// Whether `path` matches any of the compiled patterns.
    pub fn is_ignored(&self, path: &Path) -> bool {
        self.set.is_match(path)
    }
}

/// Rewrites the plain-text extension of `pattern`'s last component so each
/// ASCII letter matches either case: `*.c` becomes `*.[cC]`.
fn fold_extension_case(pattern: &str) -> Cow<'_, str> {
    let name_start = pattern.rfind('/').map_or(0, |i| i + 1);
    let Some(dot) = pattern[name_start..].rfind('.').filter(|&d| d > 0) else {
        return Cow::Borrowed(pattern);
    };
    let ext_start = name_start + dot + 1;
    let ext = &pattern[ext_start..];
    let plain = ext
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'+'));
    if !plain || !ext.bytes().any(|b| b.is_ascii_alphabetic()) {
        return Cow::Borrowed(pattern);
    }
    let mut folded = String::with_capacity(pattern.len() + 3 * ext.len());
    folded.push_str(&pattern[..ext_start]);
    for c in ext.chars() {
        if c.is_ascii_alphabetic() {
            folded.extend(['[', c.to_ascii_lowercase(), c.to_ascii_uppercase(), ']']);
        } else {
            folded.push(c);
        }
    }
    Cow::Owned(folded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_recursive_glob() {
        let ignore = PathIgnore::new(["vendor/**", "third_party/**"]).unwrap();
        assert!(ignore.is_ignored(Path::new("vendor/lib/foo.c")));
        assert!(ignore.is_ignored(Path::new("third_party/x.rs")));
        assert!(!ignore.is_ignored(Path::new("src/main.rs")));
    }

    #[test]
    fn empty_ignore_matches_nothing() {
        let ignore = PathIgnore::empty();
        assert!(!ignore.is_ignored(Path::new("vendor/lib/foo.c")));
    }

    #[test]
    fn invalid_glob_is_an_error() {
        assert!(PathIgnore::new(["["]).is_err());
    }

    fn folded(patterns: &[&str]) -> PathIgnore {
        PathIgnore::with_extension_case_folded(patterns).unwrap()
    }

    #[test]
    fn new_stays_case_sensitive() {
        let ignore = PathIgnore::new(["*.c"]).unwrap();
        assert!(ignore.is_ignored(Path::new("a.c")));
        assert!(!ignore.is_ignored(Path::new("A.C")));
    }

    #[test]
    fn folding_matches_upper_and_mixed_case_extensions() {
        let ignore = folded(&["*.c", "src/*.c", "*.cpp"]);
        for path in [
            "A.C", "a.c", "src/B.c", "src/B.C", "x.CPP", "x.Cpp", "x.cPp",
        ] {
            assert!(ignore.is_ignored(Path::new(path)), "{path}");
        }
        assert!(!ignore.is_ignored(Path::new("a.h")));
    }

    #[test]
    fn folding_also_applies_to_an_upper_case_pattern() {
        let ignore = folded(&["**/*.H"]);
        for path in ["include/a.h", "include/a.H", "deep/er/b.h", "c.h"] {
            assert!(ignore.is_ignored(Path::new(path)), "{path}");
        }
        assert!(!ignore.is_ignored(Path::new("include/a.hpp")));
    }

    #[test]
    fn folding_leaves_directories_case_sensitive() {
        let ignore = folded(&["Vendor/**", "Src/*.c"]);
        assert!(ignore.is_ignored(Path::new("Vendor/x.c")));
        assert!(!ignore.is_ignored(Path::new("vendor/x.c")));
        assert!(ignore.is_ignored(Path::new("Src/a.C")));
        assert!(!ignore.is_ignored(Path::new("src/a.c")));
    }

    #[test]
    fn folding_leaves_the_file_stem_case_sensitive() {
        let ignore = folded(&["config.h"]);
        assert!(ignore.is_ignored(Path::new("config.H")));
        assert!(!ignore.is_ignored(Path::new("CONFIG.h")));
    }

    #[test]
    fn folding_leaves_a_pattern_without_an_extension_alone() {
        let ignore = folded(&["Makefile", "build/**", ".gitignore", "**/.env"]);
        assert!(ignore.is_ignored(Path::new("Makefile")));
        assert!(!ignore.is_ignored(Path::new("MAKEFILE")));
        assert!(!ignore.is_ignored(Path::new("BUILD/x.c")));
        assert!(!ignore.is_ignored(Path::new(".GITIGNORE")));
        assert!(ignore.is_ignored(Path::new("app/.env")));
        assert!(!ignore.is_ignored(Path::new("app/.ENV")));
    }

    #[test]
    fn folding_leaves_an_extension_with_glob_syntax_alone() {
        assert_eq!(fold_extension_case("*.{c,h}"), "*.{c,h}");
        assert_eq!(fold_extension_case("*.[ch]"), "*.[ch]");
        assert_eq!(fold_extension_case("x[0-9].c"), "x[0-9].[cC]");
        assert_eq!(fold_extension_case("[ab]/x.c"), "[ab]/x.[cC]");
        assert_eq!(fold_extension_case("*.c++"), "*.[cC]++");
        assert_eq!(fold_extension_case("a.b/*.gen.c"), "a.b/*.gen.[cC]");
        assert!(folded(&["*.{c,h}"]).is_ignored(Path::new("x.h")));
    }

    #[test]
    fn folding_reports_an_invalid_glob() {
        for pattern in ["[.c", "[/1.a", "src/[x/*.c"] {
            assert!(PathIgnore::new([pattern]).is_err(), "{pattern}");
            assert!(
                PathIgnore::with_extension_case_folded([pattern]).is_err(),
                "{pattern}"
            );
        }
    }
}
