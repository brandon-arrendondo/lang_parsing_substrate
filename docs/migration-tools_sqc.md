# Migrating tools_sqc to lang-parsing-substrate

**Status:** planned — not started.
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
