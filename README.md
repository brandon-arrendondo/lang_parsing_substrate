# lang-parsing-substrate

A shared language-parsing substrate for static-analysis tools: tree-sitter grammar
dispatch, language detection, and a growing set of language-agnostic analysis
primitives (import/call graphs, control-flow graphs, structural fingerprinting,
suppression comments) built on top of a unified `LanguageInfo` registry across
16 languages — compiled in at build time via Cargo feature flags.

## What's in the substrate

| Module | Provides |
|--------|----------|
| `registry` | Language detection by extension, the `LanguageInfo` table, SLOC comment-style metadata |
| `query` | Iterative (non-recursive) tree-sitter traversal helpers: `find_descendants`, `find_first_descendant`, `node_text`, ancestor lookups |
| `imports` | Per-file import/use-statement extraction, for building efferent-coupling (Ce) edges |
| `calls` | Per-file call-graph edge extraction (`caller` → `callee`), with external-call detection |
| `cfg` | Control-flow graph / basic-block construction for a function body (`c`, `cpp`, `rust`) |
| `c_standard` | Best-effort lower bound on the C standard (C99/C11/C23) a file's syntax requires |
| `fingerprint` | Structural hashing of function-like subtrees, for duplicate/clone detection across a corpus |
| `regions` | `tools:off` / `tools:on` ignored-region markers |
| `suppressions` | `tools:suppress TOOL:RULE` single-line suppression comments |
| `path_ignore` | Compiled glob ignore-pattern sets for path filtering |

Everything below the registry is deliberately per-file: a module extracts what
one parse tree contains, and leaves assembling a corpus-wide graph, dedup
report, or coupling metric to the caller. This keeps the substrate's job
narrow (one authoritative, correct answer per file) and lets each consumer
choose its own storage model (in-memory, SQLite, whatever) without the
substrate needing to know about it.

Language coverage varies by module — the registry knows about all 16
languages, but modules like `cfg` only model the languages they've been
built out for. A module never fabricates a result for a language it doesn't
support; it returns `None` (or an empty result) instead of guessing.

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

A consumer that only cares about C/C++, for example, would declare:

```toml
lang-parsing-substrate = { version = "0.4", default-features = false, features = ["lang-c", "lang-cpp"] }
```

## Usage

```toml
# Cargo.toml — full language set (default)
lang-parsing-substrate = "0.4"

# Cargo.toml — C/C++ only
lang-parsing-substrate = { version = "0.4", default-features = false, features = ["lang-c", "lang-cpp"] }
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

### Analysis primitives

```rust
use lang_parsing_substrate::{call_edges, import_sources, build_function_cfg, structural_hash};

// Call-graph edges for every named function/macro in a parsed file
let edges = call_edges(tree.root_node(), source);

// Import/use-statement sources, for Ce/Ca coupling metrics
let imports = import_sources(&tree, source.as_bytes(), "rust");

// Control-flow graph for a single function body (c/cpp/rust)
if let Some(cfg) = build_function_cfg(func_node, source, "rust") {
    println!("{} basic blocks", cfg.block_count());
}

// Best-effort lower bound on the C standard a file requires
if let Some(standard) = detect_min_c_standard(&tree, source.as_bytes()) {
    println!("requires at least {standard:?}");
}
```

## API

- `languages() -> &'static [LanguageInfo]` — compiled-in language set
- `language_for_file(path: &Path) -> Option<Language>` — grammar dispatch by extension
- `language_for_key(key: &str) -> Option<Language>` — grammar dispatch by registry key
- `language_info_for_file(path: &Path) -> Option<&'static LanguageInfo>`
- `is_source_extension` / `is_parseable_extension(ext: &OsStr) -> bool` — recursive-discovery gates
- `supported_languages_report() -> String` — human-readable language summary
- `LanguageInfo` / `SlocMode` — registry metadata and comment-style enum (drives SLOC calculation)
- `find_descendants` / `find_first_descendant` / `find_ancestor` / `node_text` and friends — traversal helpers (`query`)
- `import_sources` / `distinct_import_count` — import extraction (`imports`)
- `call_edges` / `CallEdge` / `is_function_kind` / `get_function_name` — call-graph extraction (`calls`)
- `build_function_cfg` / `FunctionCfg` / `BasicBlock` / `CfgEdge` — control-flow graphs (`cfg`)
- `detect_min_c_standard` / `CStandard` — C standard lower-bound detection (`c_standard`)
- `function_fingerprints` / `duplicate_groups` / `Fingerprint` / `CorpusFingerprint` — structural hashing (`fingerprint`)
- `ignored_regions` / `IgnoredRegion` — `tools:off`/`tools:on` markers (`regions`)
- `suppressions` / `Suppression` — `tools:suppress` comments (`suppressions`)
- `PathIgnore` — compiled glob ignore sets (`path_ignore`)

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
