# Shared dead-preprocessor-region detection

**Status:** IMPLEMENTED — `src/dead_code.rs`, `dead_code_ranges(source: &str) ->
Vec<DeadCodeRegion>`, gated on `lang-c` OR `lang-cpp` (no grammar dependency
needed, per the "worth confirming" note below). Covers both sub-problems in one
pass as suggested. `#undef` tracking (open question below) was implemented,
not skipped — the state machine needed a "currently defined at this point in
the file" lookup either way, and tracking `#undef` was no extra structural
cost once that existed. Migration of tools_sqc's local
`compute_dead_code_ranges` to call this is a tools_sqc-side follow-up, not
done here.

Originally written up from tools_sqc's side after a concrete
false-positive investigation surfaced a gap that, on inspection, also affects knots.
No substrate code exists yet; this doc is the handoff of what's known so an agent
here can scope and design it without re-deriving the background from tools_sqc.

## The ask

Add a shared, line-based (**not** tree-sitter-based — see "Why not tree-sitter"
below) utility that computes the 1-based inclusive line ranges of a C/C++ file
that are never compiled — i.e. that a preprocessor would strip before the
compiler ever sees them. Two sub-problems, one already solved in tools_sqc,
one not:

1. **Solved (in tools_sqc only today):** literal `#if 0` and `__cplusplus`-gated
   C++-only branches (`#ifdef __cplusplus`, `#if defined(__cplusplus) [&&…]`,
   and the dead `#else` of `#ifndef __cplusplus`).
2. **Not yet solved anywhere:** `#ifdef MACRO` / `#if defined(MACRO)` where
   `MACRO`'s definedness is *locally provable* from the same file — either
   unconditionally `#define`d earlier with no matching `#undef` (branch is
   always live, `#else` is dead), or never validly `#define`d in scope, e.g.
   commented out (branch is always dead).

Consumers: **tools_sqc** (already has #1, needs #2) and **knots** (has neither,
confirmed gap — see below). **moldy does not need this** — see "Why moldy is
out" below; don't build for it.

## Where #1 already lives, today, in tools_sqc

`tools_sqc/src/analyze/suppression.rs`, `compute_dead_code_ranges` +
`classify_conditional` + `classify_cpp_if` (filed as task 229). Full current
logic, reproduced here so the shape is visible without cloning that repo:

```rust
/// Which branch of a preprocessor conditional is never compiled when the
/// translation unit is built as C (sqc has no preprocessor, so it would
/// otherwise analyze both branches and flag the inactive one).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BranchKind {
    /// The then-branch is dead in C; the `#else` branch (if any) is live.
    /// Covers `#if 0`, `#ifdef __cplusplus`, `#if defined(__cplusplus)`.
    ThenDead,
    /// The then-branch is live; the `#else` branch is dead in C.
    /// Covers `#ifndef __cplusplus`, `#if !defined(__cplusplus)`.
    ElseDead,
    /// Not a recognized dead-in-C conditional — both branches are analyzed.
    Neutral,
}

fn classify_conditional(directive: &str, rest: &str) -> BranchKind {
    match directive {
        "ifdef" => if first_ident(rest) == "__cplusplus" { ThenDead } else { Neutral },
        "ifndef" => if first_ident(rest) == "__cplusplus" { ElseDead } else { Neutral },
        "if" => if is_zero_condition(rest) { ThenDead } else { classify_cpp_if(rest) },
        _ => Neutral,
    }
}
```

`compute_dead_code_ranges` walks the file line-by-line, tracks `#if`/`#ifdef`/
`#ifndef` nesting depth plus a parallel `kinds` stack, opens a dead region when
`classify_conditional` returns non-`Neutral`, and closes it at the matching
`#elif`/`#else`/`#endif` (nested conditionals inside a dead block don't
prematurely end it — depth is tracked). An unterminated dead block (no
`#endif`) runs to EOF. `sqc`'s `SuppressionManager::should_suppress` then
treats any violation landing in one of these ranges as unfixable noise and
suppresses it with a fixed justification string, same code path as inline
`SQC-SUPPRESS` comments and `suppress.toml` entries.

## Why line-based, not tree-sitter — this is not a style preference

`compute_dead_code_ranges`'s own doc comment states the reason directly:

> This is line-based on purpose: tree-sitter mis-nests these blocks because
> the C++ `extern "C" {` brace is unbalanced in a C parse.

Concretely: a header with

```c
#ifdef __cplusplus
extern "C" {
#endif
/* ... C declarations ... */
#ifdef __cplusplus
}
#endif
```

has an unbalanced `{` when tree-sitter parses it as C (the opening brace's
`#ifdef` is invisible to the parser, but the brace itself is real text) —
tree-sitter's error recovery then attaches following siblings to the wrong
parent, and a naive "is this line inside a `preproc_ifdef` node" query gives
wrong answers exactly on the construct this feature exists to handle. This is
also consistent with `src/cfg.rs`'s existing scope note that constant-condition
dead-branch folding was deliberately left out of the generic CFG builder as
"a C-preprocessor-specific concept." A dead-region scanner belongs in this
crate as a **plain-text utility gated under `lang-c`/`lang-cpp`**, parallel to
`suppressions()` (already exactly that shape — plain-text, non-AST, shared
across tools via `suppress.toml`'s `tool` field) — not as a tree-sitter query.

## The gap that isn't solved anywhere: named-macro definedness (#2)

### Where it bit tools_sqc: MSC24-C delta-adjudication, task 540

Full writeup: `tools_sqc/data/precision_audit/DELTA_MSC24_TASK540.md`. Summary:
adjudicating MSC24-C ("do not use deprecated/obsolescent functions") findings
on raylib found 14 FPs (of 228 in-scope), **all** in one function,
`ExportFontAsCode()` in `src/rtext.c`, all sharing one root cause. Two concrete
shapes, both at real line numbers from that pass:

- **6 FPs** (`rtext.c:1092-1096`, `1133`): `sprintf()` calls inside the `#else`
  branch of `#if defined(SUPPORT_COMPRESSED_FONT_ATLAS)`, where
  `SUPPORT_COMPRESSED_FONT_ATLAS` is unconditionally `#define`d two lines
  above **in the same function** (a function-local `#define`, not file-scope —
  worth confirming the design handles `#define`/`#undef` appearing inside
  function bodies, which is unusual but legal and is exactly what raylib
  does here). The `#else` is provably dead; sqc's `sprintf`-ban scan still
  flags the call inside it because sqc has no preprocessor and evaluates
  both branches.
- **8 FPs** (`rtext.c:1146-1154`): `sprintf()` calls inside
  `#if defined(SUPPORT_FONT_DATA_COPY)`, where the corresponding `#define` is
  commented out (`//#define SUPPORT_FONT_DATA_COPY`) — the macro is never
  actually defined, so the `#if` branch itself is provably dead.

Adjudication reasoning recorded per-finding (from
`tools_sqc/data/precision_audit/raylib/import_delta_msc24_task540.csv`):

```
rtext.c,1092,FP,"sprintf() call is inside the #else branch of '#if defined(SUPPORT_COMPRESSED_FONT_ATLAS)',
  but that macro is unconditionally #defined two lines above, so this branch is dead code never compiled."
rtext.c,1146,FP,"sprintf() call is inside '#if defined(SUPPORT_FONT_DATA_COPY)', but that macro's #define
  is commented out ('//#define SUPPORT_FONT_DATA_COPY'), so this branch is dead code never compiled."
```

tools_sqc filed this as **task 560** (`todo-sqlite-cli show 560` there), scoped
as: recognize `#ifdef MACRO`/`#if defined(MACRO)` as dead when `MACRO` is
provably always-defined (unconditional `#define` earlier in file, no matching
`#undef`) or provably never-defined (commented-out `#define`, or no `#define`
anywhere in scope). All 14 measured FPs would be eliminated by this fix.
Current plan on the tools_sqc side is to fix this **locally** in
`suppression.rs` first (it's a real, already-measured FP and shouldn't wait on
cross-repo coordination) — this substrate doc is about *not* duplicating that
same logic a second time when knots needs it too.

### Where it independently bites knots: confirmed, not hypothetical

Checked `knots/src/complexity.rs` directly: it treats `#`-prefixed
preprocessor lines as code (there's a test,
`test_python_sloc_c_sloc_counts_hash_preprocessor`, specifically guarding that
`#` isn't miscounted as a comment in C/C++/Rust), but there is **no**
dead-branch exclusion logic anywhere in the crate — grepped for
`if 0|dead.code|ifdef|preprocessor` across `src/` and found nothing relevant.
Concretely: today, a raylib-shaped function like `ExportFontAsCode()` — two
`#if defined(MACRO)`/`#else` alternates where one side is provably dead — has
its McCabe/cognitive/SLOC counted through **both** branches, inflating the
function's apparent complexity by however much dead code sits in the never-
compiled side. This is the same class of noise sqc had pre-task-229, just
showing up as a skewed metric instead of a false-positive finding.

### Why moldy is out — checked, don't build for it

`moldy/CLAUDE.md` states a "preprocessor-is-opaque invariant": preprocessor
lines (`#include`, `#define`, conditionals) are treated as opaque tokens and
every branch is formatted uniformly regardless of which one a preprocessor
would keep — `emit_preproc` in `src/formatter/c_cpp.rs` doesn't evaluate
condition truth at all, by design (same reason clang-format formats `#if 0`
blocks). There is no "acting on dead code" decision for a formatter to skip —
formatting isn't a scored judgment the way a lint finding or a complexity
metric is, so dead-region detection has no consumer in moldy. Don't scope
this work around a third consumer that doesn't exist.

## Reusable pieces already in tools_sqc for the macro-definedness question

Not tree-sitter-based, already plain-regex-over-source-text (same style this
crate's `suppressions()` uses), in
`tools_sqc/src/utility/cert_c/ast_utils.rs`:

```rust
/// True if `#define name ...` appears anywhere in `source`, regardless of
/// what it expands to.
pub fn is_defined_macro_name(name: &str, source: &str) -> bool {
    let re = regex::Regex::new(&format!(r"(?m)^\s*#\s*define\s+{}\b", regex::escape(name)));
    re.is_match(source)
}

/// Collect every `#define NAME ...` object-macro name in `source`.
pub fn collect_defined_macro_names(source: &str, out: &mut HashSet<String>) { ... }
```

These answer "is MACRO ever `#define`d in this file" but **not** the two
things task 560 (and thus this design) actually need:
- position-sensitivity (`#define` must appear *before* the `#if`/`#ifdef`
  line being classified, not merely "anywhere in the file"), and
- `#undef` tracking (an unconditional `#define` followed later by an
  unconditional `#undef` before the `#if` should *not* count as
  always-defined).

Neither existing helper is a drop-in; both are useful reference points for
what "regex over raw text, deliberately not AST-based" looks like in this
codebase family, and for the exact commented-out-`#define` detection needed
for the never-defined case (`//#define MACRO` — a regex for the `#define`
pattern preceded only by `//`/`/* ... */` on its line, not a real directive).

## Suggested shape (not prescriptive — substrate agent's call)

Mirroring the existing `suppressions(source, SlocMode)` entry point:

```rust
pub fn dead_code_ranges(source: &str) -> Vec<(usize, usize)>
```

feature-gated the same way `cpp_header::looks_like_cpp` already is
(`lang-c`/`lang-cpp`, no grammar dependency needed since this is pure text
scanning — worth confirming whether it needs its own feature flag or can ride
under the existing `lang-c` one). Scope covering both sub-problems (#1 port +
#2 new) in one pass keeps a single nesting/depth state machine instead of two
scanners disagreeing at edge cases (e.g. a `#if 0` nested inside a dead
`#ifdef MACRO` region, or vice versa).

## Open questions for whoever picks this up

- Migration path: does tools_sqc's `compute_dead_code_ranges` get deleted in
  favor of calling this, or does it stay as a local fallback? (Given the
  substrate migration precedent in `docs/migration-tools_sqc.md`, likely the
  former, but that's a tools_sqc-side decision once this exists.)
- Function-local `#define`/`#undef` (the raylib case is inside a function
  body, not file scope) — confirm the line scanner doesn't need brace/scope
  awareness, since `#define` is lexically file-wide in C regardless of where
  it textually appears; the "position-sensitive, before this `#if`" rule
  should be sufficient without knowing it's inside a function.
- Whether `#undef` tracking is worth the complexity for v1, or whether v1
  ships "unconditional `#define` earlier in file, ignore `#undef`" (slightly
  unsound but likely fine in practice — the CLAUDE.md-recorded review process
  on the tools_sqc side would delta-adjudicate any resulting FPs before
  claiming a precision win, so an imperfect v1 isn't silently wrong forever).
