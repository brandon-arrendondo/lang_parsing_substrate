# lang-parsing-substrate — developer guide for Claude

Shared Rust **library crate** — the common parsing substrate for knots, moldy, and tools_sqc.
Provides language detection, tree-sitter grammar dispatch, and `LanguageInfo` registry across
16 languages, compiled in at build time via Cargo feature flags.

## Repository layout

| Path | Purpose |
|------|---------|
| `src/lib.rs` | Crate root — module wiring, cfg-gated grammar re-exports (`pub use tree_sitter_*`) |
| `src/registry.rs` | `LanguageInfo`, `SlocMode`, `languages()`, `language_for_file()`, `language_for_header_content()`, `language_info_for_file()`, `sloc_mode_for_file()`, `is_source_extension`, `is_parseable_extension`, `is_extension_for_language`, `supported_languages_report()` |
| `src/cpp_header.rs` | `looks_like_cpp()` — best-effort content sniff for `.h` (C vs C++), gated on `lang-c` + `lang-cpp` |
| `src/dead_code.rs` | `dead_code_ranges()` — line-based preprocessor dead-code region detection for C/C++ (`#if 0`, `__cplusplus`-gated branches, locally-provable macro definedness), gated on `lang-c` OR `lang-cpp`; see `DETECT_DEAD_CODE_REGIONS.md` for the design handoff |
| `src/dead_code_swift.rs` | `swift_dead_code_regions()` — tree-sitter-based (not line-based) dead-code detection for Swift's `#if`/`#elseif`/`#else`, gated on `lang-swift`; narrower scope than the C/C++ module — Swift has no `#define`, so only compile-time-constant boolean conditions (`#if false`, etc.) are locally provable, see module docs |
| `src/dead_code_csharp.rs` | `csharp_dead_code_regions()` — tree-sitter-based dead-code detection for C#'s `#if`/`#elif`/`#else`, gated on `lang-csharp`; ports both C/C++ sub-problems (unlike Swift) since C# has real nested `preproc_if` nodes and real `#define`/`#undef` — see module docs |
| `src/isr.rs` | `interrupt_handlers()` — identifies C/C++ function definitions carrying real syntactic evidence of being an interrupt/ISR handler (GNU `__attribute__((interrupt))`, C23 `[[gnu::interrupt]]`, AVR-libc `ISR(vector) { ... }` macro shape), gated on `lang-c` OR `lang-cpp`; deliberately no name-substring fallback — see `ISR_DETECTION.md` for the design handoff and why |
| `Cargo.toml` | 16 optional `lang-*` features + `all-languages` + `default = ["all-languages"]` |
| `tasks.py` | `invoke build / test / check / bump-version / publish / clean` |
| `todo.db` | Task tracking — run `todo-sqlite-cli list` to see open work |

## Key invariants

- **`language_for_file` returns `Option<Language>`** — never a fallback. If a language feature is disabled, its extensions return `None`. It is extension-only (no I/O): `.h` always resolves to the C grammar regardless of content. Callers that already have the file's bytes and want best-effort C-vs-C++ disambiguation for `.h` should use `language_for_header_content(path, source)` instead (see `src/cpp_header.rs`) — it defers to `language_for_file` for every other extension and only overrides `.h` when the content contains an unambiguous C++-only construct.
- **`languages()` is runtime-constructed** via `OnceLock<Vec<LanguageInfo>>`. It cannot be a `const` because its contents vary by compiled feature set. Do not attempt to make it `const`.
- **Feature flags are the language gate** — every language is an optional Cargo dep. The `all-languages` feature enables all 16. Consumers use `default-features = false` to opt into a subset (e.g. tools_sqc only needs `lang-c,lang-cpp`).
- **Grammar re-exports are cfg-gated** — `pub use tree_sitter_rust` is `#[cfg(feature = "lang-rust")]`. Consumers reach grammars transitively without their own direct deps.

## Adding a new language

1. **`Cargo.toml`** — add `tree-sitter-<lang> = { version = "...", optional = true }` and a `lang-<name>` feature under `[features]`. Add it to `all-languages`.
2. **`src/registry.rs` — `languages()`** — add a `#[cfg(feature = "lang-<name>")] v.push(LanguageInfo { ... })` block.
3. **`src/registry.rs` — `language_for_file()`** — add a `#[cfg(feature = "lang-<name>")] Some("ext" | ...) => Some(...LANGUAGE.into())` arm. Keep C last (it matches `.h` which could shadow other languages if placed first).
4. **`src/lib.rs`** — add `#[cfg(feature = "lang-<name>")] pub use tree_sitter_<name>;`

## Three-tier consumer model

| Tool | Execution model | Cross-file features | Storage need |
|------|----------------|--------------------|----|
| knots | pre-commit or `--recursive` | OFF in single-file; ON in recursive | in-memory |
| moldy | pre-commit or `--recursive` | OFF in single-file; ON in recursive | in-memory |
| tools_sqc | always full-scan | always ON | SQLite (path+mtime keyed) |

Cross-file features (Tier 2: import graph, call graph; Tier 3: CFG; Tier 4: pattern matching)
are not yet implemented — see `todo.db` for open tasks.

## Substrate capability tiers

| Tier | Content | Status |
|------|---------|--------|
| 1 | Parse layer — language detection, tree-sitter dispatch, source bytes | **Done** |
| 2 | Graph layer — import graph (Ce/Ca/Instability), call graph | Pending |
| 3 | Control flow / basic blocks | Pending |
| 4 | Pattern matching — generalized rule engine (from tools_sqc) | Pending |
| 5 | Fingerprinting / similarity (code dedup) | Speculative |

## Related projects

- `../knots/` — complexity metrics tool; CLAUDE.md there is the knots developer guide
- `../moldy/` — formatting tool
- `../tools_sqc/` — CERT-C compliance tool; has the rule engine that becomes Tier 4

## Fixed-form Fortran dependency note

Fixed-form Fortran (`.f`/`.for`/`.f77`) support was **dropped from `lang-fortran`** ahead of the
v0.1.0 crates.io release — `cargo publish` rejects any manifest containing a `git` dependency,
even on a fully optional feature, and `tree-sitter-fixed-form-fortran` only exists as a git dep on
the `brandon-arrendondo/tree-sitter-fixed-form-fortran` fork. `lang-fortran` now covers only
free-form Fortran (`tree-sitter-fortran`, a real crates.io dep). Re-add fixed-form once
`stadelmanma/tree-sitter-fixed-form-fortran` merges PR #4 (real crates.io version) or the fork is
itself published to crates.io.
