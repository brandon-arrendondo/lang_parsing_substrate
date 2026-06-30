# Migrating knots to lang-parsing-substrate

**Status:** DONE (working tree, uncommitted). knots builds against the substrate; full workspace test
suite green (149 lib + 126 main + 5 member); zero new clippy warnings.
**Effort:** small (~1 day). Mostly deleting knots' copies of code the substrate already owns.

**Deviations from this plan, applied during the migration:**
- The substrate's `tree-sitter-fixed-form-fortran` was a broken `path = "../tree-sitter-fixed-form-fortran-fork"`
  dep (missing on this machine), so the substrate itself didn't build. Switched it to the same **git dep**
  (pinned rev) knots already uses — Cargo dedups them. See substrate CLAUDE.md.
- Added `#[derive(Eq, Debug)]` to the substrate's `SlocMode` (needed by `assert_eq!` in the new tests).
- Ported the registry tests into the substrate (`every_registered_extension_maps_to_its_grammar`,
  `fixed_form_fortran_sloc_mode`, `unknown_extension_has_no_language`, `python_sloc_mode`).
- `complexity.rs` test modules also held bare `tree_sitter_*` grammar refs (not just `main.rs`); qualified
  them to `crate::tree_sitter_*`.
**Why it's small:** the substrate's `LanguageInfo`, `SlocMode`, `languages()`, `language_for_file`,
`is_source_extension`, `is_parseable_extension`, and `supported_languages_report` were *extracted from
knots*. They are the same code. This migration removes the duplication and reconciles two deliberate
behavior differences (decided below).

This is a **Tier 1 only** migration: language detection + grammar dispatch + SLOC-mode lookup move to
the substrate. knots' metrics domain logic (function discovery, name extraction, complexity) stays put.

---

## Decisions (made)

1. **Unknown-extension behavior changes.** knots' current `language_for_file` returns a bare
   `tree_sitter::Language` with a `_ => tree_sitter_c::LANGUAGE.into()` fallback — every unknown
   extension silently parses as C. The substrate returns `Option<Language>` with **no fallback**.
   **Decision: adopt the substrate's `None` semantics.** Files whose extension is unknown, or whose
   language feature is disabled, are **skipped** rather than mis-parsed as C. This is a behavior change
   for any user who relied on knots force-parsing odd extensions as C.

2. **Fixed-form Fortran SLOC.** The substrate gains a `sloc_mode_for_file()` helper (see prerequisite
   below) so knots can delete its local `sloc_mode_for_file` entirely.

---

## Prerequisite: substrate change

Add to `lang_parsing_substrate/src/registry.rs`:

```rust
/// Returns the [`SlocMode`] for `path`, special-casing fixed-form Fortran
/// (`.f`/`.for`/`.f77` and uppercase variants), which share the "fortran"
/// LanguageInfo but require a distinct comment-stripping strategy.
pub fn sloc_mode_for_file(path: &Path) -> Option<SlocMode> {
    let ext = path.extension().and_then(|e| e.to_str())?;
    match ext {
        "f" | "for" | "f77" | "F" | "FOR" | "F77" => Some(SlocMode::FortranFixed),
        _ => language_info_for_file(path).map(|l| l.sloc_mode),
    }
}
```

Export it from `src/lib.rs` alongside the other registry re-exports. Rationale: the single Fortran
`LanguageInfo` carries `sloc_mode: Fortran` (free-form) for *all* its extensions, so
`language_info_for_file("x.f").sloc_mode` is wrong for fixed-form. `language_for_file` already dispatches
the grammar correctly (`tree_sitter_fixed_form_fortran`); this closes the matching SLOC gap.

---

## knots changes

### `knots/Cargo.toml`

- Add: `lang-parsing-substrate = { path = "../lang_parsing_substrate" }`
  (default features = all 16 languages, matching knots' current set).
- **Remove all 18 direct `tree-sitter-*` grammar deps** (lines ~20-36): `tree-sitter-ada`,
  `tree-sitter-c`, `tree-sitter-cpp`, … `tree-sitter-lua`, `tree-sitter-fixed-form-fortran`.
  knots reaches every grammar transitively through the substrate.
- **Keep** `tree-sitter = "0.25"` — knots uses `Node`/`Tree`/`Parser`/`TreeCursor` directly.
- Version alignment is otherwise a non-issue: knots already pins the exact grammar versions the
  substrate uses. The only coordination point is `tree-sitter-fixed-form-fortran` (knots = git rev,
  substrate = `path = "../tree-sitter-fixed-form-fortran-fork"`); they must resolve to the same fork.

### `knots/src/lib.rs` — delete and re-import

Delete these definitions and import the identical substrate items instead:

| Delete (knots/src/lib.rs) | Replace with |
|---|---|
| `SlocMode` enum (43-58) | `use lang_parsing_substrate::SlocMode;` |
| `LanguageInfo` struct (60-71) | `use lang_parsing_substrate::LanguageInfo;` |
| `LANGUAGES` const (77-94) | `use lang_parsing_substrate::languages;` (now a fn, not a const) |
| `supported_languages_report` (136) | `use lang_parsing_substrate::supported_languages_report;` |
| `language_for_file` (163-188) | `use lang_parsing_substrate::language_for_file;` (now `-> Option`) |
| `is_source_extension` (191) | `use lang_parsing_substrate::is_source_extension;` |
| `language_info_for_ext` (197) | `use lang_parsing_substrate::language_info_for_file;` (takes `&Path`, not `&str`) |
| `is_parseable_extension` (205) | `use lang_parsing_substrate::is_parseable_extension;` |
| `sloc_mode_for_file` (212-219) | `use lang_parsing_substrate::sloc_mode_for_file;` |

Re-exports at `lib.rs:23-41` (consumed by the `knots-test-complexity` workspace member) become
re-exports of the substrate's: `pub use lang_parsing_substrate::tree_sitter_c;` etc.

Note the two API-shape changes at call sites:
- `languages()` is a function returning `&'static [LanguageInfo]`, not a `const` slice. Any
  `LANGUAGES.iter()` becomes `languages().iter()`.
- `language_info_for_ext(&str)` → `language_info_for_file(&Path)`. Callers that have only an extension
  string must construct a `Path`, or keep a thin local `&str` wrapper.

### `knots/src/main.rs` — handle `Option`

`parse_file` (311-320) currently does `parser.set_language(&language_for_file(file))?`. With the
substrate it becomes (per decision 1, skip on `None`):

```rust
fn parse_file(file: &Path, source_code: &str) -> Result<Option<Tree>> {
    let Some(lang) = language_for_file(file) else { return Ok(None); };
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).context("Failed to set language")?;
    let tree = parser.parse(source_code, None)
        .with_context(|| format!("Failed to parse {}", file.display()))?;
    Ok(Some(tree))
}
```

The caller skips files that return `None`. Because file discovery (`collect_files`) already filters by
`is_source_extension` / `is_parseable_extension`, `None` here should only occur for a file whose
language feature is compiled out — log-and-skip is appropriate.

### Stays in knots (NOT substrate's responsibility)

These are knots' metrics domain logic and are untouched by the migration:
- `visit_functions` (lib.rs:344-386), `get_function_name` (478-539), `collect_local_names`,
  the external-call node-kind sets, state-coupling node kinds.
- All of `complexity.rs` (SLOC calculators, McCabe, cognitive, etc.). The SLOC *calculators* stay;
  only the SLOC-*mode lookup* moves to the substrate.

(These may eventually be absorbed by substrate Tier 2 — call graph — but not in this migration.)

---

## Validation

- `cargo build` / `cargo test` in knots, including the `knots-test-complexity` workspace member.
- Spot-check a fixed-form Fortran file (`.f`) reports `FortranFixed` SLOC behavior unchanged.
- Confirm an unknown extension is now **skipped** (previously parsed as C) — this is the one
  intentional behavior change; note it in knots' changelog.
- `--supported-languages` output unchanged.
