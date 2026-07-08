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

This doc is the complete brief — it does not depend on or reference any
external tracker. When the research and implementation plan below are done,
whoever picks this back up on the original machine is responsible for
recording it in that machine's task tracker; no action needed here beyond
writing the findings into this file (or a sibling file, per "What to
produce" below).

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
  ready to hand to a build phase. Append it to this file under a `## Findings
  and implementation plan` heading (or a new `docs/c-standard-detection-
  findings.md` that this file links to) so it travels with the repo and
  needs no separate tracker entry to be usable.

## Findings and implementation plan

### 1. Prior art: nobody infers a C standard from source alone

- **clang**: has no source-sniffing inference. It defaults to `gnu17`
  (`gnu99` on PS4) whenever `-std=` is omitted, full stop. `-Wc99-extensions`
  / `-Wc11-extensions` run in the *opposite* direction from what we want:
  they warn when code uses a later-standard feature *while a specific
  earlier `-std=` was explicitly requested*. That requires the target
  standard as an input, not an output — it's a compliance check, not a
  detector. The useful takeaway is structural, not behavioral: clang
  necessarily maintains an internal "this syntax requires standard X" table
  to drive those diagnostics, which validates that such a table is the right
  shape for this feature — just used in reverse.
- **cppcheck**: explicit `--std=` only; defaults to the latest standard
  (`c11`) if unspecified. Documented as a manual override, not an inference.
- **SonarQube C/C++**: relies entirely on `compile_commands.json` /
  build-system-provided flags for standard configuration. No source-level
  inference found.
- No academic or OSS prior art surfaced for "infer the minimum required C
  standard from source text alone" as a standalone problem. Every tool in
  practice treats the standard as build-config input.
- **Conclusion**: nothing to reuse or wrap — this genuinely isn't a solved
  problem elsewhere, which is a point *in favor* of the cautious design,
  since there's no external ground-truth detector to benchmark against.
  Our three consumers (knots/moldy in single-file mode especially) are in a
  different position than a compiler or SonarQube: they often have no build
  config at all, so imperfect source-only inference is the only signal
  available, not a redundant one.

### 2. Design intent: holds, and more strongly than assumed

The "lower bound / `Option` / never fabricate" design is correct, and the
research sharpens *why*:

- Every existing tool refuses to guess and instead demands the standard as
  input — reinforcing that guessing a "target" standard is not a
  well-posed problem, only "what's the provable floor" is.
- The pinned grammar itself (`tree-sitter-c 0.24.2`, confirmed as the latest
  version published to crates.io — this isn't a "bump the dep" fix) simply
  does not implement several of the constructs the original research
  brief hoped to key off (`_Static_assert` as a keyword, `typeof`,
  `_BitInt`, `_Bool`, `_Complex` are **absent from `grammar.js` entirely**).
  For those, "return `None`" isn't a conservative choice, it's the only
  honest one — the substrate cannot see a distinct node kind that isn't in
  the grammar it ships.

### 3. Grammar reality check (tree-sitter-c 0.24.2)

Verified directly against `grammar.js` / `src/node-types.json` in the pinned
`tree-sitter-c = "0.24.2"` dependency (not assumed from general knowledge).

**C89 → C99** (from the brief: `//` comments, VLAs, mixed decls/code,
`_Bool`, `_Complex`, `inline`, designated initializers, compound literals,
`restrict`):

| Marker | Grammar support | Verdict |
|---|---|---|
| bare `restrict` | distinct anonymous token, separate from `__restrict__`/`ms_restrict_modifier` | **Keep, high confidence** — no pre-C99 GNU spelling collides with the exact token `restrict` |
| designated initializers | `initializer_pair` with `subscript_designator` (`[i] =`) / `field_designator` (`.f =`) children | **Keep, high confidence** — no pre-C99 equivalent in this grammar |
| `[*]` unspecified VLA size in a prototype | literal `*` as the `size` field of `array_declarator`/`abstract_array_declarator` | **Keep, high confidence** — a bare `*` token as an array size has no meaning before C99 |
| VLA (general, non-constant size) | `array_declarator` always exists (arrays predate C89); "VLA-ness" depends on the size expression being non-constant, which requires constant-folding/macro-expansion tree-sitter cannot do | **Drop** — `int a[N]` (macro constant) and `int a[n]` (true VLA) are structurally identical to tree-sitter; this would be a false-positive machine, not a floor |
| bare `inline` | distinct anonymous token vs `__inline`/`__inline__`/`__forceinline` | **Heuristic-tier, not v1** — GCC accepted bare `inline` as an extension before C99 ratified it, so presence doesn't *prove* C99 |
| compound literals | `compound_literal_expression`, distinct node | **Heuristic-tier, not v1** — long-standing GNU/gnu89 extension predating C99 standardization |
| mixed declarations/code | representable (would need to walk `compound_statement`'s `_block_item` children and check a `declaration` appears after a preceding non-declaration statement) — no single node-kind query | **Heuristic-tier, not v1** — also a pre-existing GNU extension |
| `//` comments | grammar has one `comment` token covering both `//` and `/* */` forms — no distinct node kind, would need text sniffing (`starts_with("//")`) | **Heuristic-tier, not v1** — GCC accepted `//` in C89/gnu89 mode long before C99 too |
| `_Bool` | **absent from grammar** — no token at all; bare `_Bool` parses as an ordinary `type_identifier`, indistinguishable from a user typedef | **Drop** — ungrammatical to detect |
| `_Complex` | **absent from grammar** — same as `_Bool` | **Drop** |

**C99 → C11** (`_Generic`, `_Static_assert`, `_Alignas`/`_Alignof`,
`_Atomic`, anonymous structs/unions, `_Noreturn`):

| Marker | Grammar support | Verdict |
|---|---|---|
| `_Generic` | dedicated `generic_expression` node | **Keep, high confidence** — no pre-C11 meaning whatsoever |
| `_Alignas`/`alignas` | `alignas_qualifier` covers both spellings | **Keep, high confidence** — `<stdalign.h>` supplying `alignas` is itself C11+, so either spelling implies C11 |
| `_Alignof`/`alignof` | `alignof_expression` covers `_Alignof`/`alignof`, **but also** the GNU pre-C11 spellings `__alignof__`/`__alignof`/`_alignof` in the same rule | **Keep, high confidence, but only match the exact `_Alignof`/`alignof` tokens** — must explicitly exclude the three GNU spellings, which are separate anonymous node kinds and safe to skip |
| `_Atomic` | dedicated anonymous token in `type_qualifier` | **Keep, high confidence** — no pre-C11 spelling |
| `_Noreturn`/`noreturn` | dedicated tokens in `type_qualifier` | **Keep, high confidence** — `noreturn` only exists via the C11+ `<stdnoreturn.h>` macro, no earlier form |
| `_Static_assert`/`static_assert` | **no dedicated node** — parses as a plain `call_expression`/`declaration` with an ordinary identifier as the "function" name, indistinguishable from a user symbol of the same name | **Keep as a text-match heuristic only** (identifier name equals `_Static_assert` or `static_assert`) — not structural, but both names are reserved identifiers so real-world collision risk is negligible |
| anonymous struct/union members | representable (`field_declaration` with no declarator and a `struct_specifier`/`union_specifier` type) but needs a structural "declarator absent" check, not a single node kind | **Heuristic-tier, not v1** — GNU supported this as an extension (gcc ~4.6+) well before C11 |
| **trap**: `_Nonnull` | present in `type_qualifier` alongside real C11 qualifiers | **Not a standard marker at all** — this is a Clang nullability extension, must be explicitly excluded from any query |

**C11 → C17**: confirmed nothing to detect — C17 added no syntax (defect-fix
release only), and the grammar has no construct that distinguishes C11 from
C17 parses. **Drop this transition entirely.** Any C11-tier marker should
just report `CStandard::C11`, standing in for "C11 or C17" — the two are
indistinguishable and behaviorally identical for this purpose.

**C17 → C23** (`nullptr`, `constexpr`, `typeof`/`typeof_unqual`, `#embed`,
`[[...]]` attributes, `_BitInt`, boolean keywords):

| Marker | Grammar support | Verdict |
|---|---|---|
| `nullptr`/`nullptr_t` | dedicated `null` token choice and `primitive_type` entry | **Keep, high confidence** — no pre-C23 meaning in C |
| `constexpr` | dedicated token in `type_qualifier` | **Keep, high confidence** — no pre-C23 meaning in C (unlike C++) |
| `[[...]]` attributes | dedicated `attribute_declaration` node, structurally distinct from GNU `attribute_specifier` (`__attribute__`) | **Keep, high confidence** — no pre-C23 spelling collides with `[[`/`]]` in C |
| `#embed` | **no dedicated node** — falls through the grammar's generic `preproc_directive` regex catch-all, so it parses as a `preproc_call` whose `preproc_directive` text is literally `#embed` | **Keep, text-match heuristic** — no pre-C23 directive is spelled this way, safe despite not being a bespoke node kind |
| `typeof`/`typeof_unqual` | **absent from grammar** — and critically, GNU's long-standing `typeof`/`__typeof__` extension keyword predates C23 and would produce the *identical* unrecognized-identifier parse, so even a text-match heuristic can't distinguish "C23 typeof" from "pre-existing GNU typeof extension" | **Drop** — structurally infeasible, not just absent |
| `_BitInt` | **absent from grammar**, would misparse/fall to `ERROR` or stray identifier | **Drop** |
| bare `true`/`false`/`bool` as keywords | grammar unconditionally tokenizes these as dedicated `true`/`false` nodes and a `bool` primitive_type regardless of whether `<stdbool.h>` was included — tree-sitter has no preprocessor, so it cannot distinguish "C23 native keyword" from "pre-C99... C99+`<stdbool.h>` macro expansion" | **Drop** — this is exactly the macro-guarded-code trap the brief called out in §3; can't be resolved without preprocessor awareness |

### 4. False-positive/negative traps (brief §3)

- **GNU pre-standard spellings** (`__inline`, `__typeof__`, `__restrict__`,
  `__alignof__`, `__attribute__`): already structurally excluded, not just
  documented — the grammar gives each of these its own distinct anonymous
  node kind separate from the standardized spelling, so a query that only
  matches the standard spelling's token never sees them. This is a property
  of matching on `node.kind()` rather than matching by regex/text over
  keyword spellings generally.
- **Macro-guarded code** (`#if __STDC_VERSION__ >= 201112L`): tree-sitter
  has no preprocessor, so both arms of an `#ifdef`/`#if` are present as
  sibling nodes in the parse tree and a plain tree walk will descend into a
  dead branch and flag a marker that would never actually compile at that
  standard. Full resolution (real macro evaluation) is out of scope for a
  syntax-only substrate. Recommended handling for v1: document as a known
  limitation, consistent with "lower bound is best-effort, not sound."
  Optional v2 refinement: specifically skip descending into a
  `preproc_if`/`preproc_ifdef` subtree whose condition text mentions
  `__STDC_VERSION__` or `__STDC__` — that's the one case that is
  self-describing (the author is already branching on the standard) and
  cheap to special-case without building a general preprocessor.
- **stdbool.h / `true`/`false`/`bool` ambiguity**: resolved by dropping it
  as a marker entirely (§3 above) rather than trying to special-case it —
  there is no reliable way to tell keyword-use from macro-expansion-use from
  syntax alone.

### 5. Implementation plan

**Module shape**: new `src/c_standard.rs`. Scoped to C only — no generic
multi-language "standard detection" framework, since C is the only language
with this concept among the current 16 (C++ has an analogous problem but
it's out of scope here; don't build for it speculatively).

**Public API**:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CStandard {
    C99,
    C11, // stands in for "C11 or C17" — the two are syntactically identical
    C23,
}

/// Best-effort lower bound: the earliest C standard this file's syntax
/// requires. Returns `None` when no diagnostic marker is present (this
/// means "consistent with C89", not "confirmed C89").
pub fn detect_min_c_standard(tree: &tree_sitter::Tree, source: &[u8]) -> Option<CStandard>;
```

**Marker set for v1** — only markers with no pre-standard GNU/macro
ambiguity ("hard implication" markers), per the "never fabricate" principle:
a heuristic-tier marker only makes a standard *likely*, not required, and
including it would break the function's contract that `Some(X)` means "this
file cannot satisfy any earlier standard."

- **C99 floor**: bare `restrict`, designated initializers
  (`subscript_designator`/`field_designator`), `[*]` unspecified VLA size.
- **C11 floor**: `_Generic`, `_Alignas`/`alignas` (`alignas_qualifier`),
  `_Alignof`/`alignof` (excluding GNU `__alignof`/`__alignof__`/`_alignof`
  spellings), `_Atomic`, `_Noreturn`/`noreturn`, `_Static_assert`/
  `static_assert` (identifier-name text match).
- **C23 floor**: `nullptr`, `constexpr`, `attribute_declaration` (`[[...]]`),
  `#embed` (text match on `preproc_directive`).
- **Explicitly out of v1** (documented as known gaps, not silently missing):
  general VLA-by-nonconstant-size, `_Bool`, `_Complex`, mixed
  declarations/code, `//` comments, anonymous struct/union members, bare
  `inline`, compound literals, `typeof`/`typeof_unqual`, `_BitInt`, bare
  `true`/`false`/`bool`. Each is either unsupported by the pinned grammar or
  ambiguous with a pre-existing GNU extension/macro and would violate the
  "hard lower bound" contract.
- No C11-vs-C17 distinction is attempted or possible.

**Traversal strategy**: a single iterative tree walk (explicit stack, not
recursion) matching on `node.kind()` for the token-level markers, consistent
with this repo's existing `CfgBuilder::visit` iterative pattern (see `1a50130
fix: make CfgBuilder::visit iterative, not recursive`) rather than compiling
separate `tree_sitter::Query` objects per marker — most markers are simple
kind checks, not multi-capture patterns, so a query engine is unneeded
overhead. Track the highest standard seen; short-circuit once `C23` is hit.

**Test strategy**:

- Fixture-per-marker `.c` snippets under `tests/fixtures/c_standard/`
  (or inline `&str` sources in `#[cfg(test)]`, matching this crate's existing
  test conventions — check `src/registry.rs`'s test module for the house
  style before choosing), each asserting the exact expected
  `Some(CStandard::_)`.
- Negative fixtures:
  - A plain C89-compatible file → `None`.
  - A GNU-extension-heavy file using only `__inline`, `__typeof__`,
    `__restrict__`, and a gnu89-style compound literal → `None` — proves
    GNU pre-standard spellings don't leak into the signal.
  - A file using `bool`/`true`/`false` via `<stdbool.h>`-style usage with no
    other marker → `None` — proves the dropped stdbool ambiguity doesn't
    silently resurface.
- One documented known-limitation fixture: a `_Generic` expression guarded
  behind `#if __STDC_VERSION__ >= 201112L ... #endif` — assert the *current*
  (imprecise) behavior (`Some(C11)` even though it's conditionally compiled)
  with a comment pointing at §4 above, so a future preprocessor-aware
  refinement has a regression test to update deliberately rather than
  silently.
- Monotonicity property test: combining fixtures (concatenating a C99-floor
  snippet with a C11-floor snippet) must never return a *lower* standard
  than either fixture alone.

This plan is ready to hand to a build phase.
