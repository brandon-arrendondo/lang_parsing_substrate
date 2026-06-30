# Migrating tools_sqc to lang-parsing-substrate

**Status:** IMPLEMENTED locally (uncommitted working tree); **benchmark validation pending.**
Full local suite green: 3404 + 39 + 13 tests pass (was 3404 + 39 + 13 on the 0.22 baseline), zero new
clippy warnings, binary smoke-tested (detects on `.c`/`.h`, rejects non-C). The tree-sitter
`0.22 → 0.25` bump surfaced exactly **one** node-kind regression, now fixed (see below). The remaining
gate is a benchmark run over real C codebases to catch subtler finding-count drift the fixtures don't
cover.

**What was actually done (smaller than the worst case below):**
- The query/`StreamingIterator` break was **1 production file** (EXP42-C), not many.
- The old `tree_sitter_c::language()` API appeared at **22 sites**; all now route through a single
  `crate::parser::c_language()` helper that sources the grammar from the substrate. tools_sqc has **no
  direct `tree-sitter-c` dep** anymore (added `lang-parsing-substrate` with `default-features=false,
  features=["lang-c"]`, kept `tree-sitter = "0.25"` core, added `streaming-iterator`).
- Detection centralized: `.c`/`.h` checks in `files/directory.rs` (×2) and `files/git.rs` now call
  `lang_parsing_substrate::is_parseable_extension` (behavior-identical for C-only).
- **Node-kind regression fixed — EXP33-C GNU asm.** The rule had reverse-engineered the *0.21* misparse
  of `__asm("..":"=r"(v)..)` (output operand became `call_expression("=r",[v])`). 0.24 misparses it
  entirely differently (operands collapse into an `ERROR`/`concatenated_string` that swallows the
  following statement), causing a false-positive uninitialized read on the `__ASM` upper-case form.
  Fixed by replacing the shape-specific match with a version-agnostic `is_in_asm_call()` check: any
  identifier inside an asm-keyword (`asm`/`__asm`/`__asm__`, any case) `call_expression` is treated as
  asm-opaque (never a genuine read). See `src/rules/cert_c/EXP/EXP33-C/exp33_c.rs`.

**Effort:** large (several days), but **the cost is a tree-sitter version upgrade, not language detection.**
The detection-centralization part is small; adopting the substrate forces tools_sqc off its pinned
`tree-sitter 0.22` / `tree-sitter-c 0.21` onto the substrate's `0.25` / `tree-sitter-c 0.24`, and that
upgrade ripples through ~150 rules.

This is a **Tier 1 only** migration. Generalizing the rule engine across languages is **Tier 4** — a
separate, much larger future effort that is explicitly **out of scope** here (see below).

---

## Part A — the small part: adopt the substrate for detection + dispatch

### `tools_sqc/Cargo.toml`

- Add: `lang-parsing-substrate = { path = "../lang_parsing_substrate", default-features = false, features = ["lang-c"] }`
  - `default-features = false` is **essential** — without it the substrate enables all 16 languages and
    file discovery starts matching `.py`, `.rs`, etc. tools_sqc is C-only.
  - Add `"lang-cpp"` only if/when C++ support is actually wanted. Not required for the migration.
- Remove the direct `tree-sitter = "0.22"` and `tree-sitter-c = "0.21"` pins; reach the grammar through
  the substrate (`lang_parsing_substrate::tree_sitter_c`) or keep a `tree-sitter = "0.25"` dep for the
  core types only.

### Centralize the `.c`/`.h` checks

Replace the scattered hardcoded extension checks:
- `src/files/directory.rs:28-42` (single-file validation) and `:62-63` (directory scan)
- `src/files/git.rs:30-31` (git repo scan)

with `lang_parsing_substrate::is_parseable_extension(ext)`.

**Trap: use `is_parseable_extension`, not `is_source_extension`.** `.h` is `explicit_only` for C, so
`is_source_extension(".h") == false` — using it would silently drop every header from discovery.
`is_parseable_extension` returns true for both `.c` and `.h`.

### Replace `CParser`

`src/parser/mod.rs:10-14` hardcodes `tree_sitter_c::language()` (old 0.21 API). Replace with the
substrate's grammar. C-only means the existing "one parser, reused per thread" structure can stay; just
source the `Language` from `lang_parsing_substrate::tree_sitter_c::LANGUAGE.into()` (or
`language_for_file(path)` if/when multi-language).

---

## Part B — the large part: tree-sitter 0.22 → 0.25 upgrade

This is the real work and the real risk. Touches the parser, all query-using rules, and every rule that
hardcodes C node kinds.

### B1. Streaming query iterator (breaking)

In tree-sitter 0.25, `QueryCursor::matches` returns a **`StreamingIterator`**, not a plain iterator.
Every `for m in matches { ... }` must become:

```rust
use streaming_iterator::StreamingIterator;
let mut it = query_cursor.matches(&query, *node, source.as_bytes());
while let Some(m) = it.next() { /* ... */ }
```

Known site: `src/rules/cert_c/EXP/EXP42-C/exp42_c.rs:41-55`. Grep the whole `rules/` tree for
`.matches(` and `QueryCursor` to find every one. `Query::new` signature also changed (now takes
`&Language`).

### B2. Node-kind revalidation (tree-sitter-c 0.21 → 0.24)

Grammar updates can rename/restructure node kinds. All ~150 rules and the AST helpers hardcode C node
kinds — `function_definition`, `array_declarator`, `pointer_declarator`, `cast_expression`,
`for_statement`/`while_statement`/`do_statement`, etc. (e.g. `src/utility/cert_c/ast_utils.rs:29-48`).
Every one must be re-validated against the 0.24 grammar.

**Safety net:** `build.rs:315-779` generates integration tests from C test fixtures. Run the full suite
after the bump and chase every failure — these tests are exactly what catches a node-kind that moved.

### B3. Parser/Tree API deltas

Minor signature changes across the 0.22→0.25 core API (e.g. `set_language` taking `&Language`). Mechanical.

---

## Explicitly out of scope (Tier 4, future)

The exploration surfaced these as "coupling bottlenecks," but they are **rule-engine generalization,
not Tier 1**, and must not creep into this migration:

- A `ParserFactory` / per-language parser dispatch beyond what C needs.
- A node-kind **alias layer** abstracting `function_definition` etc. across languages.
- A declarative rule plugin system (replacing the manual `RuleRegistry::new()` list in
  `rules/cert_c/mod.rs`).
- Language-agnostic AST utilities (replacing `utility/cert_c/`).

These belong to substrate **Tier 4** (the rule engine that tools_sqc eventually *donates upward*).
For now, rule node-kind logic stays in tools_sqc and is touched **only** where B2 forces it.

---

## Doc correction

The substrate's `CLAUDE.md` lists tools_sqc's storage as "SQLite (path+mtime keyed)". This is **wrong**:
tools_sqc uses a **`bincode`** prescan cache (`src/analyze/prescan.rs`, load/save at
`analyze/mod.rs:237-285`) keyed by **scan scope, with no mtime/hash tracking**. Unaffected by this
migration, but the table in CLAUDE.md should be corrected.

---

## Validation

- `cargo build` with `default-features = false, features = ["lang-c"]` — confirm no other-language
  grammars are pulled in.
- Full `cargo test` including the generated fixture-driven integration tests (the node-kind safety net).
- Diff a baseline analysis run (before/after) over a representative C codebase — violation counts and
  locations should be identical. Any drift points at a node-kind that moved in the grammar bump.
- Confirm `.h` files are still discovered in directory and git scan modes.
