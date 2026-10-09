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
| `cpp_header` | Best-effort C-vs-C++ disambiguation for `.h` files from their content (`looks_like_cpp`; the registry's `language_for_header_content` uses it) |
| `classify` | Cheap **heuristic** pre-parse file classification from a bounded byte prefix + size: `SourceText` / `Binary` / `Oversize` / `Empty`, so consumers can skip a 2 GB zip named `.c` before it reaches tree-sitter |
| `query` | Iterative tree-sitter traversal: `walk_preorder` (one cursor per walk), `find_descendants`, `find_first_descendant`, root-down ancestor lookups (`ancestors`, `find_ancestor_from_root`), linear `child_nodes`, `node_text` |
| `flat` | A whole parse tree as flat, index-linked columns (`flatten` → `FlatTree`), so a consumer that cannot hold a `tree_sitter::Node` (Python) can still walk every node |
| `tsquery` | Run a tree-sitter query and return owned captures (`run_query`), plus each grammar's bundled tags query (`tags_query`) |
| `imports` | Per-file import/use-statement extraction, for building efferent-coupling (Ce) edges |
| `calls` | Per-file call-graph edge extraction (`caller` → `callee`), with external-call detection |
| `cfg` | Control-flow graph / basic-block construction for a function body (`c`, `cpp`, `rust`) |
| `c_standard` | Best-effort lower bound on the C standard (C99/C11/C23) a file's syntax requires |
| `fingerprint` | Structural hashing of function-like subtrees, for duplicate/clone detection across a corpus |
| `regions` | `tools:off` / `tools:on` ignored-region markers |
| `suppressions` | `tools:suppress TOOL:RULE` single-line suppression comments |
| `dead_code` | Preprocessor dead-code regions for C/C++ (`#if 0`, `__cplusplus`-gated branches, locally-provable macro definedness) |
| `dead_code_swift` | Dead-code regions for Swift's `#if`/`#elseif`/`#else` conditional compilation (compile-time-constant boolean conditions only — see module docs for why the C/C++ macro-definedness sub-problem doesn't apply to Swift) |
| `dead_code_csharp` | Dead-code regions for C#'s `#if`/`#elif`/`#else` conditional compilation — AST-based like `dead_code_swift`, but ports both C/C++ sub-problems (constant conditions and locally-provable `#define`/`#undef` symbol definedness) since C# has real nested preprocessor nodes and real `#define` |
| `path_ignore` | Compiled glob ignore-pattern sets for path filtering |
| `isr` | C/C++ interrupt-handler detection (`interrupt_handlers`), with the evidence for each |

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

A few modules exist only when the languages they model are compiled in:
`cpp_header` needs `lang-c` and `lang-cpp`; `dead_code` needs `lang-c`,
`lang-cpp` or `lang-csharp`; `isr` needs `lang-c` or `lang-cpp`;
`dead_code_swift` needs `lang-swift`; `dead_code_csharp` needs `lang-csharp`.
Every other module is always compiled and returns `None` or an empty result
for a language it does not model.

A consumer that only cares about C/C++, for example, would declare:

```toml
lang-parsing-substrate = { version = "0.11.1", default-features = false, features = ["lang-c", "lang-cpp"] }
```

## Usage

```toml
# Cargo.toml — full language set (default)
lang-parsing-substrate = "0.11.1"

# Cargo.toml — C/C++ only
lang-parsing-substrate = { version = "0.11.1", default-features = false, features = ["lang-c", "lang-cpp"] }
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

### Early exit before parsing

`language_for_file` trusts the extension. To skip archives, executables,
images and oversized files before they reach tree-sitter, classify first —
it reads only a bounded prefix, never the whole file:

```rust
use lang_parsing_substrate::{classify_file, ClassifyLimits, FileClass};

// The size limit is the caller's policy; the default is no limit.
let limits = ClassifyLimits { max_size: Some(16 << 20), ..Default::default() };
match classify_file(path, &limits)? {
    FileClass::SourceText(_) => { /* parse it */ }
    FileClass::Binary { kind, mime } => { /* skip-and-report, or try anyway */ }
    FileClass::Oversize { size, limit } => { /* caller's call */ }
    _ => { /* Empty, or a future variant */ }
}
```

The result is a heuristic and can be wrong in both directions (see the
`classify` module docs). Magic numbers come from the dependency-free
[`infer`](https://crates.io/crates/infer) crate. Short ASCII-looking
signatures such as `MZ` or `BM` only count when the bytes also look binary.
`classify(path, prefix, size, &limits)` is the I/O-free core, for callers
that have already read the bytes.
The thresholds are calibrated in `docs/classify-calibration.md`; re-run
that check on a new corpus with `cargo run --release --example
classify_calibrate -- ROOT...`.

### Analysis primitives

```rust
use lang_parsing_substrate::{build_function_cfg, call_edges, detect_min_c_standard, import_sources};

// Call-graph edges for every named function/macro in a parsed file
let edges = call_edges(tree.root_node(), source);

// Import/use-statement sources, for Ce/Ca coupling metrics
let imports = import_sources(&tree, source.as_bytes(), "rust");

// Control-flow graph for a single function body (c/cpp/rust)
if let Some(cfg) = build_function_cfg(func_node, source.as_bytes(), "rust") {
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
- `sloc_mode_for_file(path: &Path) -> Option<SlocMode>` — comment style for SLOC counting
- `language_for_header_content(path, source)` / `looks_like_cpp` — `.h` C-vs-C++ disambiguation (`registry` and `cpp_header`; needs `lang-c` and `lang-cpp`)
- `classify` / `classify_file` / `FileClass` / `BinaryKind` / `TextEncoding` / `SourceText` / `ClassifyLimits` — heuristic pre-parse file classification (`classify`)
- `is_source_extension` / `is_parseable_extension(ext: &OsStr) -> bool` — recursive-discovery gates
- `is_extension_for_language(ext: &OsStr, key: &str) -> bool` — discovery for one language (e.g. C only)
- `supported_languages_report() -> String` — human-readable language summary
- `LanguageInfo` / `SlocMode` — registry metadata and comment-style enum (drives SLOC calculation)
- `walk_preorder` / `find_descendants` / `find_first_descendant` / `find_ancestor` / `node_text` and friends — traversal helpers (`query`)
- `flatten` / `FlatTree` — a whole tree as flat columns (`flat`)
- `run_query` / `Capture` / `tags_query` — tree-sitter queries and bundled tags queries (`tsquery`)
- `import_sources` / `distinct_import_count` — import extraction (`imports`)
- `call_edges` / `CallEdge` / `is_function_kind` / `get_function_name` / `collect_local_names` — call-graph extraction (`calls`)
- `build_function_cfg` / `FunctionCfg` / `BasicBlock` / `CfgEdge` — control-flow graphs (`cfg`)
- `detect_min_c_standard` / `CStandard` — C standard lower-bound detection (`c_standard`)
- `function_fingerprints` / `block_fingerprints` / `structural_hash` / `duplicate_groups` / `Fingerprint` / `CorpusFingerprint` — structural hashing (`fingerprint`)
- `ignored_regions` / `IgnoredRegion` — `tools:off`/`tools:on` markers (`regions`)
- `suppressions` / `Suppression` — `tools:suppress` comments (`suppressions`)
- `dead_code_ranges` / `dead_code_ranges_with_assumptions` / `PlatformAssumptions` / `posix_default_assumptions` / `DeadCodeRegion` / `DeadCodeReason` — preprocessor dead-code regions (`dead_code`; needs `lang-c`, `lang-cpp` or `lang-csharp`)
- `swift_dead_code_regions` / `SwiftDeadCodeRegion` — Swift `#if`/`#elseif`/`#else` dead-code regions (`dead_code_swift`; needs `lang-swift`)
- `csharp_dead_code_regions` / `CSharpDeadCodeRegion` — C# `#if`/`#elif`/`#else` dead-code regions (`dead_code_csharp`; needs `lang-csharp`)
- `interrupt_handlers` / `InterruptHandler` / `InterruptEvidence` — interrupt-handler detection (`isr`; needs `lang-c` or `lang-cpp`)
- `PathIgnore` — compiled glob ignore sets (`path_ignore`)

## Building

```bash
cargo build                                          # all languages (default)
cargo build --no-default-features --features lang-c,lang-cpp  # subset
cargo test
```

Needs a C compiler, which the grammar crates call through the `cc` crate, but
not the tree-sitter CLI: the grammar crates ship pre-generated C sources.

## Python bindings

The `pyo3` Cargo feature (off by default) exposes the substrate's
language-agnostic analysis primitives as a Python extension module, published
to PyPI as prebuilt `abi3` wheels (CPython 3.10+, one wheel per platform —
see `docs/releasing.md`):

```bash
pip install lang-parsing-substrate
```

Building it yourself from source uses [maturin](https://www.maturin.rs/):

```bash
pip install maturin
maturin build --release          # writes target/wheels/lang_parsing_substrate-*.whl
pip install target/wheels/lang_parsing_substrate-*.whl
```

```python
import lang_parsing_substrate as lps

src = "fn helper(x: i32) -> i32 { x + 1 }\nfn main() { helper(41); }\n"
edges = lps.call_edges("rust", src)          # [CallEdge]; edges[0].caller == "main"
cfg = lps.function_cfg("rust", src, "main")  # FunctionCfg | None
fps = lps.function_fingerprints("rust", src, min_nodes=1)
caps = lps.query("rust", src, "(function_item name: (identifier) @name)")  # [Capture]
tags = lps.tags_query("tsx")                 # the grammar's bundled tags query, or None
tree = lps.parse_tree("rust", src.encode())  # FlatTree: every node as columns
cls = lps.classify_file("src/main.rs", max_size=4 << 20)  # FileClass: kind, encoding, ...
skip = lps.PathIgnore(["vendor/**"]).is_ignored("vendor/x.c")  # True
```

Python can't hand this crate a `tree_sitter::Node`/`Tree` directly — this
crate's `tree-sitter` version has no ABI relationship to tree-sitter's own,
separate Python bindings — so every function that analyses code takes
`(language_key, source)`, parses internally, and returns owned data. The
classes are `CallEdge`, `FunctionCfg`, `BasicBlock`, `Fingerprint`,
`Suppression`, `IgnoredRegion`, `LanguageInfo`, `Capture`, `FlatTree`,
`Node`, `FileClass` and `PathIgnore`. `classify` / `classify_file` take a
path, `PathIgnore` takes glob patterns, and `languages`,
`supported_languages_report` and `tags_query` take no source.

A consumer that walks the tree itself (e.g. for domain-specific semantics this
crate doesn't model) uses `parse_tree`. It returns every node, named or not, in
pre-order (row 0 is the root). Each column (`kind`, `field`, `flags`, `parent`,
`first_child`, `next_sibling`, `prev_sibling`, `start_byte`, `end_byte`,
`start_row`, `start_col`, `end_row`, `end_col`, `child_offset`, `child_list`)
is native-endian `u32` `bytes`, so `memoryview(tree.kind).cast("I")[i]` is node
`i`'s kind. `kind` and `field` index into the `kinds` / `fields` string tables,
and links are `lps.NONE` when absent. A node's children are
`child_list[child_offset[i]:child_offset[i + 1]]`. `tree.root_node` returns a
native `Node` that answers the walking subset of py-tree-sitter's `Node` API
(`type`, `children`, `child_by_field_name`, `parent`, siblings, positions,
`text`, `id`, error flags), so a walker written for py-tree-sitter runs on it
unchanged. clew's development branch parses every grammar this way. Every
function that takes a language key also accepts `"tsx"` for the JSX-aware
TypeScript grammar (`function_cfg` returns `None` for it, since CFGs cover
only C, C++ and Rust).

`invoke build-wheel` builds the wheel locally for testing. The actual PyPI
release happens in CI on a `vX.Y.Z` tag push, via Trusted Publishing (OIDC,
no API token) — see `docs/releasing.md`.

## AI Assistance

The substrate was developed with assistance from [Claude](https://claude.ai) (Anthropic), used for code generation across its language extractors, bug fixes, packaging, and documentation. From October 2026, [Codex](https://openai.com/codex/) (OpenAI) also contributed documentation fixes and the tooling that enforces the agent and commit guidelines. Each of its changes was reviewed before it was merged.

Many earlier commits have a `Co-Authored-By: Claude` trailer, but not every AI-assisted commit does, so the trailers are not a complete record. From October 2026 the contribution is acknowledged once, here, and not with a co-author trailer on each commit.

## License

MIT. Copyright 2026 BISSELL Homecare, Inc. See [LICENSE](LICENSE).
