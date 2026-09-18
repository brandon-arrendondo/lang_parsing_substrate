# Tiered similarity granularity + line-level duplicate reporting

**Status:** IMPLEMENTED — `src/fingerprint.rs` now has a `FingerprintTier`
enum (`Function`/`Block`), a `block_fingerprints()` walk sibling to
`function_fingerprints()`, and `duplicate_groups()` groups by `(hash, tier)`
so the two tiers can never cross-match. See "Resolved" below for what was
decided on each open question. Ask 1 needed no code change, as predicted —
see its section below, unchanged from the original scoping.

Written up from a design conversation about `src/fingerprint.rs`'s original
limits (Tier 5, `todo.db` task 17, done 2026-07-04): similarity was computed
and reported strictly per whole function. Two separable asks came out of
that conversation; the second was the one worth real design attention.

## Ask 1: expose both reporting granularities (small, mostly already unlocked)

Today a `duplicate_groups` match reads like "function X duplicates function
Y." Two audiences want different things from the same match:

- **Humans** (a review, a PR comment) want the function-level framing —
  it's the unit they think in.
- **Tools/CI/AI consumers** want the line-level framing — `path:40-58`
  duplicates `path:12-30` — because that's what's directly actionable
  (jump to it, diff it, fail a check on it) without a name-resolution step.

This is nearly free: `Fingerprint` already carries `start_line`/`end_line`
for every hashed subtree (`src/fingerprint.rs`, the `Fingerprint` struct) —
a consumer collapsing a match down to "function X vs function Y" is
throwing away data it already has, not working around a substrate gap.
**No substrate change is strictly required** — this is a consumer-side
reporting flag (report by name vs. by line span) in knots/moldy/aurora-lint's
own output formatting. Worth a one-line callout in `fingerprint.rs`'s module
doc so a future reader doesn't assume the line data isn't there; a small
`ReportMode`-style formatting helper here is optional convenience, not a
requirement (same posture as `posix_default_assumptions` in
`dead_code.rs` — a small default is nice to have, not load-bearing).

## Ask 2: sub-function similarity, tiered — the actual design ask

**The concrete use case driving this** (not "find all clones in the
codebase" — that's the already-shipped Tier 5 job): a caller has *already*
flagged a specific region of code as a problem (e.g. an aurora-lint CERT-C
violation, or a manually-flagged region) and wants to know **where else in
the corpus does something structurally similar to just this one region
appear** — turning "grep the whole codebase" (needle in a haystack) into "do
any of these already-computed shapes match this one shape" (red sock in a
pile of socks: still a search, but over a small set of known candidates
rather than free-text scanning everything).

That framing matters for scoping because it is *not* "run corpus-wide
nested-block dedup and list every pair" — the earlier conversation flagged
exactly why that naive version is bad: hashing every loop/if body in a
codebase floods matches with boilerplate (two `for (i=0; i<n; i++)` loops
that do nothing else "duplicate" trivially). The targeted-search framing
sidesteps that: you don't need every block-level match in the corpus, only
the ones that match *one specific already-interesting* fingerprint. The
existing `min_nodes` floor (already in `function_fingerprints`) already does
the noise-suppression job needed here — this ask reuses it at a smaller
granularity, it doesn't need a new filtering mechanism.

### What's already there vs. what's missing

- Fingerprinting *one specific node* the caller already has in hand: already
  possible today via `structural_hash(node, source)`, no change needed —
  that half of "hash the flagged region" is a solved problem.
- What's missing is the **other side of the search**: an index of
  same-granularity fingerprints to search that one hash against. Today that
  index only exists at whole-function granularity (`function_fingerprints`
  run per file, collected into `Vec<CorpusFingerprint<S>>`). There's no
  equivalent walk that fingerprints sub-function block-shaped subtrees
  (loop bodies, if/else bodies, match arms, ...) so there's nothing for a
  block-level flagged region to search against except other whole functions.

### Suggested shape (not prescriptive)

Extend, don't replace, the existing primitives:

- A tier/granularity concept — e.g. a `FingerprintTier` enum: `Function`
  (today's default, unchanged) and `Block` (loop/conditional/match bodies —
  see open question below on exactly which nodes qualify). A `Statement`
  tier is conceivable but speculative; don't build it until `Block` is
  proven useful.
- A sibling walk next to `function_fingerprints` — e.g.
  `block_fingerprints(tree, source, min_nodes) -> Vec<Fingerprint>` — that
  walks block-shaped nodes the same way `function_fingerprints` walks
  `is_function_kind` nodes, reusing the same internal `hash_and_count`/
  `structural_hash` machinery. `Fingerprint`'s existing fields (`kind`,
  `hash`, `node_count`, byte/line range) don't need to change shape.
- `duplicate_groups` itself likely doesn't need a signature change — it
  already just groups by hash — **but** mixing function-tier and
  block-tier fingerprints in one `Vec<CorpusFingerprint<S>>` risks a
  block coincidentally matching some other file's whole (tiny) function
  under exact hash equality, which is a confusing result to hand back
  ("your flagged loop matches this one-line function"). Keep the two tiers'
  fingerprint sets separate per search, or tag which tier produced a given
  `Fingerprint` so a consumer can filter. That tagging is probably the one
  real new field needed (`kind` already encodes *which specific node kind*,
  e.g. `"for_statement"` vs. `"function_item"`, but not *which walk pass*
  produced it — those aren't quite the same thing across 16 languages'
  differently-named node kinds).
- The "find similar to this one flagged region" workflow itself likely
  doesn't need a new primitive beyond the above — it's "compute one
  `Fingerprint` for the flagged node, then filter/search the corpus's
  block-tier `Vec<CorpusFingerprint<S>>` for the same hash," which
  `duplicate_groups`'s existing hash-equality grouping already expresses
  (a consumer can just run the flagged fingerprint's hash through the same
  grouping, or a trivial `HashMap` lookup — decide whether that's worth its
  own convenience function or is a one-liner every consumer writes itself).

## Non-goals

- No fuzzy/windowed Type-3 similarity (PMD-CPD-style near-miss matching) —
  same posture as `duplicate_groups`'s existing doc comment. This ask only
  extends the existing *exact*-hash approach to more/smaller subtrees; it
  does not add tolerance for a few added/removed statements.
- No corpus-wide "list every block-level duplicate pair" pass as a default
  behavior — see the noise-flooding concern above. If that turns out to be
  wanted later, it needs its own scoping (probably with a much higher
  `min_nodes` floor and/or per-node-kind allowlist), not assumed as a
  byproduct of this ask.

## Resolved

- **Syntactic-block, not `cfg.rs`'s CFG basic blocks.** `Block` tier walks
  whole AST subtrees rooted at loop/conditional/switch-like nodes
  (`is_block_kind`), unsplit by internal branches — not split-at-every-branch
  CFG basic blocks. Reasoning held up as scoped: the targeted "does this
  flagged region recur elsewhere" search doesn't need CFG-level precision,
  and syntactic-block covers all 16 languages instead of only the three
  `cfg.rs` models (`c`/`cpp`/`rust`).
- **New `tier: FingerprintTier` field on `Fingerprint`**, not a caller-side
  `kind` allowlist. `FingerprintTier` is `Function`/`Block`.
  `function_fingerprints()` always tags `Function`; `block_fingerprints()`
  always tags `Block`. `duplicate_groups()` was changed to group by
  `(hash, tier)` instead of `hash` alone — a Block-tier fingerprint can now
  never cross-match a Function-tier one on a coincidental hash collision,
  without requiring the caller to separate corpora by tier itself (the
  concern the doc originally raised as "keep the two tiers' fingerprint sets
  separate per search"). Existing single-tier callers see no behavior
  change, since every fingerprint they produce shares one tier already.
- **No dedicated `find_similar` convenience was added** — the one-liner
  (`structural_hash` the flagged node directly, then filter a `Block`-tier
  corpus by `hash` and `tier`, or run it through `duplicate_groups`) is
  documented in `fingerprint.rs`'s module doc as sufficient. Revisit only if
  a real consumer's usage shows the one-liner is annoying to repeat.
- **Per-grammar node-kind survey for `Block` tier** — done against each
  language's vendored `tree-sitter-*` `node-types.json` (not guessed), see
  `is_block_kind`'s doc comment in `fingerprint.rs` for the full per-language
  list and citations. Covers `if`, every loop form (`while`/`for`/`foreach`/
  `do-while`/`repeat`/unconditional `loop`), and `switch`/`match`/`when`/
  `select`-style dispatch — both the whole dispatching construct and, where
  the grammar gives each case/arm its own node kind, the individual arms.
  Deliberately excludes plain scoping blocks (a bare `{ }`, a function body)
  — not "a flagged region" in the sense this tier targets.
