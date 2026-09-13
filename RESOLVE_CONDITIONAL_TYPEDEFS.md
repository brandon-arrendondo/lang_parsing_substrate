# Platform-conditional typedef/macro resolution

**Status:** IMPLEMENTED — `src/dead_code.rs`, `dead_code_ranges_with_assumptions(source:
&str, assumed: &PlatformAssumptions) -> Vec<DeadCodeRegion>` plus a
`posix_default_assumptions()` convenience table, gated the same as
`dead_code_ranges` (`lang-c` OR `lang-cpp` OR `lang-csharp`). Shares the same
state machine and `DeadCodeReason` enum as the unseeded function — no new
variant was needed; `AlwaysDefined`/`NeverDefined` now also cover the
assumption-seeded case (their doc comments say so explicitly). Decisions
made on the two open questions below: assumption-vs-local-evidence
precedence went to "local wins" (an assumed name's `defined` map entry is
simply overwritten by any local `#define`/`#undef`, the same map either
way), and a small default table (`posix_default_assumptions`) was added here
rather than left to each consumer. Consumer-side wiring in tools_sqc
(`typedef_types` et al., see below) is still open — not part of this ask.

Written up from tools_sqc's side after a concrete recall regression traced to
a real bug; this doc was the handoff so an agent here could scope and design
it without re-deriving the background from tools_sqc. Sibling doc to
`DETECT_DEAD_CODE_REGIONS.md` — read that one first if you haven't; this ask
reuses its machinery and its own documentation explains why the two are
**not** the same problem, which matters more than it looks like it should.

## The ask

Given a C/C++ file, and a small fixed table of "which platform-detection
macros are assumed defined/undefined for this scan" (not derived from the
file — supplied by the caller), extend the already-implemented
`dead_code_ranges` machinery so a consumer walking the file for
name-to-type/name-to-value facts (typedefs, struct fields, object-like macro
constants, ...) can tell which of several textually-present, same-named
conditional (re)definitions is the one that would actually survive
preprocessing under that assumed platform — instead of blindly taking
whichever one appears first in the file, which is what every such collector
in tools_sqc does today and is measurably wrong.

**Explicitly not asked for:** enumerating every possible `#ifdef`
combination, evaluating arbitrary boolean guard expressions precisely, or
making a single scan multi-configuration-aware (re-analyzing one codebase
once per platform). See "Why this doesn't need path enumeration" below —
that's the actual insight this doc exists to hand off, not just the bug
report.

## Where this bites tools_sqc today (concrete, measured)

`tools_sqc/src/analyze/prescan.rs::collect_from_simple_typedef` builds
`ProjectContext::typedef_types` (a flat `HashMap<String, String>`, one
project-wide alias name → its RHS type text) with:

```rust
typedef_types
    .entry(name)
    .or_insert_with(|| type_text.clone());
```

First occurrence in the file wins, unconditionally. hostap's
`src/utils/common.h` (lines 86-121) defines `u8`/`u16`/`u32`/`u64` (and
everything built on them: `be16`, `le16`, ...) three times:

```c
#ifdef _MSC_VER
typedef UINT16 u16;        // <- textually first: this is the one kept
...
#endif
#ifdef __vxworks
typedef UINT16 u16;
...
#endif
#ifndef WPA_TYPES_DEFINED
#include <stdint.h>
typedef uint16_t u16;      // <- the branch that's actually live on this build
...
#endif
```

aurora-lint has no preprocessor, so it sees all three unconditionally in file
order and keeps `u16 -> UINT16` — a Windows-only type (`<windows.h>`) that is
never itself defined anywhere in the corpus, so it stays permanently
unresolved. Every rule that asks "is this operand's type narrower than
`int`" for anything typed `u16` (or `u8`/`u32`/`u64`, or anything built on
them) on hostap gets `None` back and silently treats it as unresolvable.

**Measured impact:** delta-adjudicating a just-shipped EXP14-C fix (aurora
promotion-safety rule — "beware of integer promotion when performing bitwise
operations on integer types smaller than int") against hostap's existing,
already-labeled ground truth showed only 3 of 62 known-real violations still
detected after the fix landed — 59 real, previously-confirmed bugs stopped
being flagged, not because the fix's own logic is wrong (verified directly:
a minimal single-definition `typedef unsigned short u16;` resolves and fires
correctly), but because the type resolution it depends on silently returns
the wrong answer for every hostap-native narrow type. Filed on the tools_sqc
side as a P1 task (`RULE_FIX_BACKLOG.md` §4 / `PENDING_COORDINATOR_SYNC.md`
§2.10 in that repo, if either is still there when you read this) with the
explicit note that landing a fix needs a full benchmark comparison across
every consumer, not just the one rule that happened to expose it.

**Blast radius beyond this one rule:** `typedef_types` (via
`resolve_typedef_chain`) is the single shared primitive backing "is this type
narrow / unsigned / what width" for INT08-C, INT10-C, INT16-C, INT31-C,
INT32-C, EXP36-C, API00-C, and now EXP14-C. None of those were re-audited for
this bug in this pass — some may already be silently wrong in the *other*
direction (a rule that defaults an unresolvable type to "assume narrow" or
"assume unsigned" would have the opposite bias from EXP14-C's "assume
int-width", and would be over-firing on hostap rather than under-firing).
`ProjectContext::function_macros` (`prescan.rs` line ~415,
`function_macros.entry(name).or_insert(m)`) has the identical first-wins
shape for same-named function-like macros redefined per platform — not
confirmed to bite in practice yet, but the same fix should cover it if it's
exposed to the same resolution path. `struct_field_types` wasn't audited for
conditional struct redefinition (rarer in practice than a typedef alias, but
worth a look once this lands).

## Why this is a *different* problem from `dead_code_ranges`, not the same one

It's tempting to reach for the already-implemented `dead_code_ranges` and
call this done. It doesn't apply here, and its own module doc says exactly
why:

> A macro this file never mentions at all (e.g. a build-system flag like
> `_WIN32`) is left unclassified — there's no local evidence either way, and
> guessing would turn every such branch into a false positive.

`_MSC_VER` and `__vxworks` are compiler-predefined macros — no portable C
file ever writes `#define _MSC_VER` or `#undef _MSC_VER` itself, so
`dead_code_ranges` correctly finds **zero local textual evidence** for either
and, by design, declines to classify either branch as dead. That restraint is
exactly right for *its* job (an FP-safe suppression filter, wrong 0% of the
time it fires): a scanner exists in tools_sqc's `suppression.rs` today for
the general case, and its whole value is not guessing on a macro the file
gives no opinion about, hostap's `common.h` is the textbook example of a file
that gives no local opinion about `_MSC_VER`. What this ask needs is the
complementary case: an *externally supplied* opinion ("we know this scan
targets a POSIX/Linux build, so `_MSC_VER` is false and `__vxworks` is
false") standing in for local evidence the file will never contain. Same
state machine, different (and non-optional, supplied-not-inferred) seed data
for the one category of macro `dead_code_ranges` deliberately refuses to
guess about.

## Why this doesn't need path enumeration (the actual scoping insight)

cppcheck's approach — enumerate every macro-configuration path a file could
plausibly compile under and analyze each — is the general solution to a
harder problem (does *any* configuration make this line reachable/buggy) and
buys real combinatorial cost for it. That's not the question here. The
question is narrower: for `typedef`/`struct`/enum/macro-constant
*declarations* specifically, which are almost never nested inside loops or
deep control flow (they're overwhelmingly top-level-in-a-header, or at worst
one level of `#ifdef` for a platform split) —

1. **the number of distinct guard-sets competing for one name is small in
   practice** (2-4: a "the special platforms" handful plus one default/
   fallback branch), not exponential — nothing here asks for the cross
   product of every macro in the file, only "which of the *specific*
   textually-competing definitions for *this one name* wins";
2. **tools_sqc's own benchmark corpus is single-platform per codebase
   already** — eleven of twelve real-world oracles are POSIX/Linux, and the
   twelfth (Ventoy) was deliberately onboarded as its *own* codebase
   specifically to get Win32-API visibility, rather than trying to make an
   existing POSIX oracle multi-platform-aware. If some other platform's
   type/macro visibility ever turns out to matter, the tools_sqc-side answer
   is "onboard a benchmark for that platform" (Ventoy's own precedent), not
   "make one scan carry several CFLAGS-varied interpretations of the same
   codebase" — so this substrate feature only ever needs to resolve **one**
   assumed platform profile per scan, supplied once by the caller, never a
   set of profiles to reconcile against each other.

So: no path explosion to worry about, and no path *enumeration* to build in
the first place — just "seed the existing dead-branch classifier with a
caller-supplied platform-assumption table in addition to what it infers
locally," then let a consumer skip any competing definition that lands in a
now-dead region.

## Suggested shape (not prescriptive — substrate agent's call)

Extend, don't fork, the existing state machine in `src/dead_code.rs`:

```rust
/// Assumed defined/undefined state for macros this file will never itself
/// settle (typically compiler- or build-system-predefined platform flags:
/// `_MSC_VER`, `_WIN32`, `__APPLE__`, `__vxworks`, ...). Distinct from the
/// locally-inferred `#define`/`#undef` evidence `dead_code_ranges` already
/// tracks -- a caller-supplied fact standing in for evidence the file
/// itself will never contain, not something inferred from it.
pub type PlatformAssumptions = HashMap<String, bool>;

pub fn dead_code_ranges(source: &str) -> Vec<DeadCodeRegion>; // unchanged, assumes nothing

pub fn dead_code_ranges_with_assumptions(
    source: &str,
    assumed: &PlatformAssumptions,
) -> Vec<DeadCodeRegion>; // seeds `defined` from `assumed` before the same walk
```

Open design question worth resolving up front: should a file's own local
`#define`/`#undef` for an assumed-name override the caller's assumption, or
should the assumption win unconditionally? Locally-inferred evidence
overriding an external assumption seems obviously right in principle, but
for the specific macros this is aimed at (compiler-predefined platform
flags), a portable file legitimately `#define`-ing `_MSC_VER` itself would be
so unusual it's fair to treat as more likely a bug in the caller's
assumption table than a real signal — decide once, document it, don't leave
it implicit in whichever order the code happens to check things.

A reasonable default `PlatformAssumptions` table for "POSIX/Linux, no
Windows/vxworks compatibility shims active" (the profile every tools_sqc
real-world oracle except Ventoy wants) is a small, static, hand-curated list
— `_WIN32`/`_MSC_VER`/`__vxworks`/`__CYGWIN__` assumed false, `__linux__`/
`__unix__` assumed true, and so on. Whether that table lives here (as a
suggested default a caller can start from) or entirely in each consumer is
open — a small curated default here seems more useful than every consumer
re-deriving the same list, but it's genuinely a call for whoever designs the
API, not a requirement.

## Consumers, once this exists (tools_sqc side, not part of this ask)

Once `dead_code_ranges_with_assumptions` (or whatever shape you land on)
exists, tools_sqc's job is to feed the resulting regions into its own
first-wins collectors so a definition landing in a dead region is skipped
rather than raced against a live one:

- `prescan.rs::collect_from_simple_typedef` (`typedef_types`) — the
  confirmed, measured case.
- `prescan.rs::collect_from_struct_specifier`/`collect_from_typedef`
  (`struct_field_types`) — not yet confirmed to bite, worth auditing once
  the mechanism exists.
- Wherever `function_macros` is collected (same first-wins shape,
  unconfirmed impact).
- Any object-like macro *value* table used for constant evaluation across
  files, if one takes the same "first `#define` wins" shortcut — not
  inventoried in this pass; whoever picks up the tools_sqc side should grep
  for the same `.entry(...).or_insert` shape against a raw per-line walk of
  `#define`s before assuming typedefs are the only place this bites.

None of that consumer-side wiring is part of this substrate task — landing a
change to `typedef_types`'s resolution semantics needs its own full Juliet +
real-world benchmark comparison across all seven-plus rules that read it
before merging, independent of whatever API shape ships here.

## Non-goals

- No general variability-aware / path-sensitive analysis (the TypeChef-style
  "analyze every configuration" problem) — deliberately out of scope, see
  "Why this doesn't need path enumeration" above.
- No attempt to evaluate arbitrary `#if EXPR` boolean logic precisely beyond
  what `dead_code_ranges` already parses (`defined(X)`, `!defined(X)`,
  simple `&&`-chains of same) — if a guard is too complex to classify today,
  it stays `Neutral` under this extension too, same as it does now for a
  file-internal macro.
- No per-codebase multi-CFLAGS re-scanning in tools_sqc's benchmark harness
  — see the scoping insight above; a platform visibility gap gets a new
  benchmark oracle (Ventoy's precedent), not a second scan of an existing
  one under different assumed defines.

## Resolved (substrate side)

- **Assumption-vs-local-evidence precedence:** local wins. A local
  `#define`/`#undef` for an assumed name overwrites the seeded `defined` map
  entry from that point on — same map, local evidence just written after
  the assumption seeds it. Documented on `dead_code_ranges_with_assumptions`.
- **Default table:** added here, not left to each consumer —
  `posix_default_assumptions()` in `src/dead_code.rs`, covering exactly the
  names this doc named (`_WIN32`/`_MSC_VER`/`__CYGWIN__`/`__vxworks` false,
  `__linux__`/`__unix__` true).
- **Separate function vs. breaking signature change:** went with a separate
  function, `dead_code_ranges_with_assumptions`, leaving `dead_code_ranges`'s
  signature untouched — no existing caller needs to change.

## Open questions for whoever picks up the tools_sqc side

- Wiring `typedef_types` (`prescan.rs::collect_from_simple_typedef`) to skip
  a definition landing in a region `dead_code_ranges_with_assumptions`
  reports dead — the confirmed, measured case this doc exists for.
- Whether `function_macros` and any macro-constant-value tables should be
  folded into the same follow-up or scoped as their own separate asks once
  someone confirms they actually exhibit the bug (unlike `typedef_types`,
  neither has a measured, ground-truth-backed repro yet — don't spend design
  effort generalizing for a problem that turns out not to exist).
