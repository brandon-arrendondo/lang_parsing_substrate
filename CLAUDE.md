# lang-parsing-substrate — developer guide for Claude

@AGENTS.md

Shared Rust **library crate** — the common parsing substrate for knots, moldy, aurora-lint, and
clew. Provides language detection, tree-sitter grammar dispatch, a `LanguageInfo` registry across
16 languages (compiled in at build time via Cargo feature flags), and per-file analysis
primitives built on top of it. Also ships optional PyO3 bindings for Python consumers.

## Repository layout

The README's **"What's in the substrate"** table is the module-to-purpose map — read it first,
and keep it current when adding a module. What follows is only what that table doesn't say.

| Path | Notes for Claude |
|------|------------------|
| `src/lib.rs` | Module wiring and re-exports; cfg-gated grammar re-exports (`pub use tree_sitter_*`). A module's feature gate lives here — check it before assuming a module is always compiled. |
| `src/registry.rs` | `languages()`, `language_for_file()`, `language_for_header_content()` and friends — see invariants below |
| `src/py.rs` | PyO3 bindings, `pyo3` feature only. The wrappers that analyse code take `(language_key, source)` and parse internally — Python can't hand in a tree-sitter node. `classify` / `classify_file` take a path instead; `languages`, `supported_languages_report` and `tags_query` take no source. |
| `tests/smoke.rs` | Parses a valid snippet in every compiled-in language |
| `tests/python/` | Binding tests; need an installed wheel (`invoke build-wheel && pip install target/wheels/*.whl`) |
| `docs/releasing.md` | crates.io + PyPI release process (the wheel is released via git tag) |
| `tasks.py` | `invoke build / test / check / build-wheel / bump-version / publish / clean` |

Feature gates that aren't obvious from the module name:

- `cpp_header` — `lang-c` AND `lang-cpp`
- `dead_code` — `lang-c` OR `lang-cpp` OR `lang-csharp`
- `isr` — `lang-c` OR `lang-cpp`
- `dead_code_swift` — `lang-swift`; `dead_code_csharp` — `lang-csharp`
- everything else is always compiled and returns `None` / empty for languages it doesn't model

## Key invariants

- **`language_for_file` returns `Option<Language>`** — never a fallback. If a language feature is disabled, its extensions return `None`. It is extension-only (no I/O): `.h` always resolves to the C grammar regardless of content. Callers that already have the file's bytes and want best-effort C-vs-C++ disambiguation for `.h` should use `language_for_header_content(path, source)` instead (see `src/cpp_header.rs`) — it defers to `language_for_file` for every other extension and only overrides `.h` when the content contains an unambiguous C++-only construct. Neither looks at whether the file is text at all — consumers that want to skip archives, executables or oversized files before parsing call `classify` / `classify_file` first (heuristic; the caller sets the size limit and the skip policy).
- **`languages()` is runtime-constructed** via `OnceLock<Vec<LanguageInfo>>`. It cannot be a `const` because its contents vary by compiled feature set. Do not attempt to make it `const`.
- **Feature flags are the language gate** — every language is an optional Cargo dep. The `all-languages` feature enables all 16. Consumers use `default-features = false` to opt into a subset (e.g. aurora-lint only needs `lang-c,lang-cpp`).
- **Grammar re-exports are cfg-gated** — `pub use tree_sitter_rust` is `#[cfg(feature = "lang-rust")]`. Consumers reach grammars transitively without their own direct deps.
- **Analysis modules are per-file** — a module extracts what one parse tree contains; assembling a corpus-wide graph, dedup report, or metric is the consumer's job.

## Adding a new language

1. **`Cargo.toml`** — add `tree-sitter-<lang> = { version = "...", optional = true }` and a `lang-<name>` feature under `[features]`. Add it to `all-languages`.
2. **`src/registry.rs` — `languages()`** — add a `#[cfg(feature = "lang-<name>")] v.push(LanguageInfo { ... })` block.
3. **`src/registry.rs` — `language_for_file()`** — add a `#[cfg(feature = "lang-<name>")] Some("ext" | ...) => Some(...LANGUAGE.into())` arm. The arms match disjoint extensions, so their order does not change the result; C's arm is last by convention.
4. **`src/registry.rs` — `language_for_key()`** — add a `#[cfg(feature = "lang-<name>")] "<key>" => Some(...LANGUAGE.into())` arm. The Python bindings parse through it.
5. **`src/lib.rs`** — add `#[cfg(feature = "lang-<name>")] pub use tree_sitter_<name>;`

## Testing and commits

- `invoke test` runs `cargo test --all-features`, then the `lang-c,lang-cpp` subset, then the Python binding tests if a wheel is installed.
- The pre-commit hooks run fmt, `clippy --all-targets --all-features -D warnings`, **both** cargo test passes, the knots complexity hook on changed Rust files, and the DCO and agent-guard checks. Because the hook runs both full test passes, a commit takes a while.
- `cargo clippy --no-default-features --all-targets -- -D warnings` currently fails on unused test helpers in several modules. That failure predates any open branch; don't treat it as a regression.

## Consumers

| Tool | Execution model | Cross-file features | Storage need |
|------|----------------|--------------------|----|
| knots | pre-commit or `--recursive` | OFF in single-file; ON in recursive (the Ce/Ca/Instability file-coupling pass, plus opt-in `--find-duplicates`) | in-memory |
| moldy | pre-commit or `--recursive` | none (formats one file at a time) | in-memory |
| aurora-lint | full scan, or `--diff` for changed files | ON (project pre-scan) | none by default; optional bincode pre-scan cache (`--save-prescan` / `--load-prescan`), not mtime-keyed |
| clew | full-repo index (Python, via the `pyo3` bindings on its develop branch; its 1.0.39 release does not use them yet) | always ON | SQLite graph |

## Substrate capability tiers

| Tier | Content | Status |
|------|---------|--------|
| 1 | Parse layer — language detection, tree-sitter dispatch, source bytes | **Done** |
| 2 | Graph layer — per-file import edges (`imports`) and call edges (`calls`); consumers assemble the graph | **Done** (per-file) |
| 3 | Control flow / basic blocks (`cfg`: c, cpp, rust) | **Done** for those languages |
| 4 | Pattern matching — generalized rule engine (from aurora-lint) | Pending |
| 5 | Fingerprinting / similarity (`fingerprint`, function + block tiers) | **Done** |

## Related projects

- `../knots/` — complexity metrics tool; CLAUDE.md there is the knots developer guide
- `../moldy/` — formatting tool, funky's successor (funky still exists)
- `../aurora-lint/` — CERT-C compliance tool; has the rule engine that becomes Tier 4
- `../clew/` — repository → SQLite graph indexer served over MCP; its develop branch consumes the Python bindings

## Fixed-form Fortran

Fixed-form Fortran (`.f`/`.for`/`.f77`) is not supported: `lang-fortran` is free-form only because
the fixed-form grammar exists only as a git dependency, which `cargo publish` rejects. Re-adding
it waits on a fixed-form grammar published to crates.io.
