# Research prompt: C standard detection (C89/C99/C11/C17/C23)

## Context

`lang-parsing-substrate` provides language detection and tree-sitter grammar
dispatch for C (and 15 other languages), but has no notion of *which C
standard* a given file targets. That's come up as potentially relevant to
three downstream consumers:

- **tools_sqc** — CERT-C rule applicability differs by standard (some rules
  only apply pre-C99, others are new in C11, e.g. around `_Generic` or
  optional VLA support).
- **moldy** — formatting has standard-dependent edge cases (`//` comments
  are pre-standard/C99+, `_Bool`/`bool`, etc.).
- **knots** — possibly relevant to complexity metrics, though this is the
  weakest of the three motivating cases.

See `todo.db` task 22 (tags: `research`, `c`, `registry`) for the tracked
task this research feeds into.

## Design intent already agreed on (do not re-litigate without cause)

The substrate's existing invariant (see `CLAUDE.md`) is: **never fabricate a
result a module isn't confident in** — `language_for_file` returns `None`
rather than guess, and other modules like `cfg` return `None`/empty rather
than approximate for languages/constructs they don't model.

C-standard detection from source text alone is inherently a heuristic in the
general case (most real-world files are syntactically valid under multiple
standards). The working design intent from prior discussion is:

- Expose it as a distinct, explicitly-named, best-effort function — not
  folded into `LanguageInfo`/`language_for_file`'s core dispatch.
- Something like `detect_min_c_standard(tree, source) -> Option<CStandard>`
  — a **lower bound** (the earliest standard the syntax requires), not a
  claim about the "true" target standard.
- Returns `None` when no diagnostic marker is present, rather than
  defaulting to a guess (e.g. "assume C17").

This research phase is meant to sanity-check that design against prior art
before writing an implementation plan — not to decide from scratch whether
the feature is worth building.

## What to research

1. **Existing tools/approaches for C-standard detection or inference:**
   - Does `clang`/`clang-tidy` have a documented heuristic for inferring
     `-std=` when none is given, beyond "defaults to gnu17"? Does it ever
     *detect* a required minimum standard from syntax and warn on mismatch
     (e.g. `-Wc99-extensions`-style diagnostics used as a detection signal)?
   - Do other static analysis tools (cppcheck, IWYU, include-what-you-use,
     PC-lint/PC-lint Plus, SonarC/C++) attempt to infer or require an
     explicit C standard, and if so, how (config file, build system
     integration, syntax sniffing)?
   - Any academic or OSS work specifically on "which C standard does this
     file require" as a standalone problem (as opposed to "which standard
     was this project built with," which is usually solved by reading build
     config, not source).
   - Precedent in `compile_commands.json` / `configure`-script style
     approaches: is standard detection from source ever actually attempted,
     or does everyone in practice treat it as a build-config input rather
     than an inferable property? This matters for validating (or
     invalidating) the whole premise.

2. **Concrete syntax markers per standard transition, if pursuing this:**
   - C89 → C99: `//` comments, VLAs, mixed declarations/code, `_Bool`,
     `_Complex`, `inline`, designated initializers, compound literals,
     `restrict`.
   - C99 → C11: `_Generic`, `_Static_assert`, `_Alignas`/`_Alignof`,
     `_Atomic`, anonymous structs/unions, `_Noreturn`. (Note: VLAs became
     *optional* in C11 — a VLA doesn't imply C11+, it implies C99 at
     minimum only.)
   - C11 → C17: no new syntax (C17 is a bugfix/clarification release) —
     confirm there's genuinely nothing here to detect.
   - C17 → C23: `nullptr`, `constexpr`, `typeof`/`typeof_unqual`,
     `#embed`, attributes (`[[...]]`), `_BitInt`, boolean literals as
     keywords (`true`/`false`/`bool` without `<stdbool.h>`).
   - For each marker, confirm whether tree-sitter-c's grammar (the version
     pinned in this crate's `Cargo.toml`) actually parses it as a distinct,
     queryable node kind, or silently accepts/rejects it in a way that
     would make detection unreliable.

3. **Known false-positive/false-negative traps:**
   - GNU extensions and compiler-specific keywords that predate their
     standardized form (e.g. `__inline`, `__typeof__`) and shouldn't be
     mistaken for standard-version signals.
   - Macro-guarded code (`#if __STDC_VERSION__ >= 201112L`) where a feature
     appears in the token stream but is inside a branch that may not be
     compiled — does a tree-sitter-based (not preprocessor-aware) approach
     produce misleading results here, and if so, how should that be scoped
     out or documented as a known limitation?

## What to produce

A short findings write-up (append to this file or a new
`docs/c-standard-detection-findings.md`) covering:

- Whether any existing tool already solves this well enough to reuse/wrap
  rather than reimplement.
- Whether the "lower bound, `Option`, never fabricate" design still holds up
  against what you find, or needs revision.
- A concrete list of tree-sitter-c node kinds/queries per standard
  transition that are reliable enough to build on, and which transitions (if
  any) should be dropped as infeasible.
- An implementation plan (module shape, function signature, test strategy)
  ready to hand to a build phase — save this as the next update to `todo.db`
  task 22.
