# lang-parsing-substrate

A shared language-parsing substrate for [knots](https://github.com/brandon-arrendondo/knots),
[moldy](https://github.com/brandon-arrendondo/moldy), and
[tools_sqc](https://github.com/brandon-arrendondo/tools_sqc). Provides tree-sitter grammar
dispatch, language detection, and a unified `LanguageInfo` registry across 16 languages —
compiled into each consumer at build time via Cargo feature flags.

## Design

Each of the three tools is a *lens* on top of a common parsing layer:

| Tool | Lens | Execution model |
|------|------|----------------|
| knots | complexity metrics (McCabe, Cognitive, AIRD, AICP, …) | fast; pre-commit or full scan |
| moldy | code formatting | fast; pre-commit or full scan |
| tools_sqc | CERT-C compliance + custom rules | batch; full scan |

The substrate owns what all three share: language detection, grammar dispatch,
file extension gating, and (eventually) the import/call graph layer that powers
coupling metrics and cross-file analysis.

## Feature flags

Not every consumer needs all 16 languages. Each language is an optional Cargo
feature; the `all-languages` convenience feature (enabled by default) pulls in
the full set.

| Feature | Language | Grammar crate |
|---------|----------|---------------|
| `lang-c` | C | `tree-sitter-c` |
| `lang-cpp` | C++ | `tree-sitter-cpp` |
| `lang-rust` | Rust | `tree-sitter-rust` |
| `lang-python` | Python | `tree-sitter-python` |
| `lang-javascript` | JavaScript | `tree-sitter-javascript` |
| `lang-typescript` | TypeScript | `tree-sitter-typescript` |
| `lang-go` | Go | `tree-sitter-go` |
| `lang-java` | Java | `tree-sitter-java` |
| `lang-csharp` | C# | `tree-sitter-c-sharp` |
| `lang-kotlin` | Kotlin | `tree-sitter-kotlin-ng` |
| `lang-swift` | Swift | `tree-sitter-swift` |
| `lang-php` | PHP | `tree-sitter-php` |
| `lang-ada` | Ada | `tree-sitter-ada` |
| `lang-fortran` | Fortran (free-form) | `tree-sitter-fortran` |
| `lang-scala` | Scala | `tree-sitter-scala` |
| `lang-lua` | Lua | `tree-sitter-lua` |
| `all-languages` | All of the above | — |

tools_sqc, which today only analyses C/C++ for CERT-C, would declare:

```toml
lang-parsing-substrate = { version = "0.1", default-features = false, features = ["lang-c", "lang-cpp"] }
```

## Usage

```toml
# Cargo.toml — full language set (default)
lang-parsing-substrate = "0.1"

# Cargo.toml — C/C++ only
lang-parsing-substrate = { version = "0.1", default-features = false, features = ["lang-c", "lang-cpp"] }
```

```rust
use lang_parsing_substrate::{language_for_file, languages, supported_languages_report};
use std::path::Path;

// Detect language for a file
if let Some(lang) = language_for_file(Path::new("main.c")) {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).unwrap();
    // parse...
}

// Enumerate compiled-in languages (reflects feature flags)
for info in languages() {
    println!("{}: {:?}", info.name, info.extensions);
}

// Human-readable summary (for --supported-languages flags)
print!("{}", supported_languages_report());
```

Grammar crates are re-exported so consumers reach them transitively:

```rust
// No direct tree-sitter-rust dependency needed in your Cargo.toml
use lang_parsing_substrate::tree_sitter_rust;
```

## API

- `languages() -> &'static [LanguageInfo]` — compiled-in language set
- `language_for_file(path: &Path) -> Option<Language>` — grammar dispatch by extension
- `is_source_extension(ext: &OsStr) -> bool` — recursive-discovery gate
- `is_parseable_extension(ext: &OsStr) -> bool` — includes explicit-only extensions
- `supported_languages_report() -> String` — human-readable language summary
- `LanguageInfo` — name, extensions, explicit_only, sloc_mode
- `SlocMode` — comment style enum (drives SLOC calculation in consumers)

## Building

```bash
cargo build                                          # all languages (default)
cargo build --no-default-features --features lang-c,lang-cpp  # subset
cargo test
```

Requires no C compiler — tree-sitter grammar crates ship pre-generated C sources
and compile via the `cc` crate.

## License

MIT — see [LICENSE](LICENSE).
