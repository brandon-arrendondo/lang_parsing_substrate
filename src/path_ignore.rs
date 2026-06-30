//! External-config path ignoring — glob patterns from `toolchain.toml`
//! `[ignore].paths` and per-tool `[<tool>.ignore]` sections.
//!
//! One glob-matching implementation shared by knots, moldy, and tools_sqc
//! instead of three reimplementations. See `docs/unified-config-spec.md`.

use globset::{Glob, GlobSet, GlobSetBuilder};
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
}
