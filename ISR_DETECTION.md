# Interrupt-handler (ISR) detection

**Status:** IMPLEMENTED (2026-08-27) — see `src/isr.rs`
(`interrupt_handlers`, `InterruptHandler`, `InterruptEvidence`), gated on
`any(feature = "lang-c", feature = "lang-cpp")`. Covers the v1 scope this doc
recommended: GNU `__attribute__((interrupt))` (both no-arg and
vector-argument forms), C23 `[[gnu::interrupt]]`-style attributes, and the
AVR-libc `ISR(vector) { ... }` macro-invocation shape (reported for *any*
macro-shaped function definition, per the "leave macro-name filtering to the
caller" resolution of the open question below — not hardcoded to `ISR`
specifically). IAR/Keil vendor-keyword forms remain a documented gap, not
implemented, per this doc's own recommendation. `calls.rs`'s
macro-function-definition exclusion (see "Cross-cutting note" below) was
left as-is — flagged there and in `isr.rs`'s docs, not resolved, since it's
tools_sqc-side judgment call.

Originally written up from tools_sqc's side (task 608, gated on task 607,
both follow-ons of task 151 —
`tools_sqc/docs/design/concurrency-rule-evaluation.md` is the source design
doc) so an agent here could scope and design it without re-deriving the
background from tools_sqc. Smaller and more self-contained than
`DETECT_DEAD_CODE_REGIONS.md`'s ask: this is a pure per-file syntactic-fact
extractor, shaped like `c_standard.rs`, not a state machine.

## The ask

Add a per-file primitive that identifies which function definitions in a
C/C++ translation unit are interrupt/ISR handlers, based on **real syntactic
evidence** (compiler attributes, well-known macros) — not name-substring
matching. Name matching alone is demonstrably unreliable (see "Why
name-matching alone is not enough" below); the whole point of doing this at
the substrate/AST layer instead of a regex is to do better than that.

## Why tools_sqc needs this — and why it isn't tools_sqc-specific

tools_sqc's CON03-C/CON07-C/CON33-C (CERT-C concurrency rules) currently fire
on a "shared state lacks synchronization" or "call to a non-reentrant
function" pattern with **zero check on whether the flagged code is ever
reachable from a second thread, ISR, or signal handler.** Confirmed directly
by reading the rule source, not inferred:

- `tools_sqc/src/rules/cert_c/CONC/CON03-C/con03_c.rs` — flags every
  non-`volatile`/non-`_Atomic` file-scope-or-cross-function-shared variable.
  No `isr`/`irq`/`interrupt`/`signal` string appears anywhere in the file.
- `tools_sqc/src/rules/cert_c/CONC/CON07-C/con07_c.rs` — same gap; only
  checks for a mutex/atomic type or a lock/unlock call nearby.
- `tools_sqc/src/rules/cert_c/CONC/CON33-C/con33_c.rs` — fires on a fixed
  non-reentrant-function name list (`strtok`, `asctime`, `rand`, ...) with no
  context check of any kind.

So **"ISR detection" is something to build from scratch here, not a bug fix
to an existing (broken) heuristic** — despite how task 151's original body
phrased it (see the "Catapult" section below); that phrasing described an
external audit's own manual classification, not code that exists in this
repo or tools_sqc's.

The underlying capability — "does this function definition carry real
syntactic evidence of being an ISR/interrupt entry point" — is C/C++ syntax
knowledge, not CERT-C rule logic, and squarely matches what this crate
already owns for unrelated purposes (see "What already exists" below). Any
other consumer built on this substrate that cares about concurrency,
embedded-systems bugs, or call-graph reachability would want the same fact.

**What should stay tools_sqc-side, deliberately not asked for here:**
assembling the corpus-wide call graph, deciding what counts as a
thread-spawn API (`pthread_create`/`thrd_create`/`CreateThread` — POSIX/Win32
library semantics, not language syntax), and the actual CERT-C reachability
judgment ("is this flagged race reachable from an ISR/thread root"). That's
consistent with this crate's stated per-file-only scope (README: "leaves
assembling a corpus-wide graph... to the caller").

## Evidence this pays off — reproducible, from tools_sqc's own data

tools_sqc's task 607 built a crude stopgap classifier
(`tools_sqc/bench/concurrency_context.py`) — a whole-file regex for
`pthread_create`/`thrd_create`/`CreateThread`, `signal`/`sigaction`, and a
**bare name-substring match** for `isr`/`irq`/`interrupt` in a function
definition — and retroactively re-scored 76 already-adjudicated
CON03/07/33/34/37-C real-world findings (mosquitto/curl/sqlite/hostap/lua/
raylib, pinned commits; labels in that repo's
`data/precision_audit/DELTA_BATCHC_TASK546.md` /
`DELTA_BATCHD_TASK547.md` / `DELTA_LT2_TASK549.md`) split by whether the
enclosing file shows *any* of that evidence:

| Bucket | Precision | TP | FP |
|---|---|---|---|
| Context present | 26.3% | 5 | 14 |
| Context absent | 1.8% | 1 | 56 |

A ~15x precision gap from a same-file, name-substring-tier regex proxy. A
real per-function attribute/macro primitive (this ask), combined with
call-graph reachability (tools_sqc-side follow-on, task 608), is expected to
sharpen this further — the name-substring tier in that stopgap classifier is
exactly the unreliable signal the next section is about.

## Why name-matching alone is not enough — a real, previously-observed failure

An external firmware audit (Catapult RC624 MCU codebase — not one of this
crate's or tools_sqc's checked-out benchmark projects; reported second-hand
via tools_sqc's task 151, not independently reproducible from either repo)
found ~130/176 (76%) of that codebase's CON03-C findings and ~52/65 (80%) of
its CON07-C findings traced to one pattern: whoever/whatever classified
"concurrency context" there treated any file containing a function whose
*name* matched `isr`/`irq`/`interrupt` as context-present. One poller
function, `DEV_APPROX_IRQProcess`, demonstrated the failure mode directly —
named like an ISR, actually a plain main-loop function, no attribute or macro
marking it as one.

Verified this exact failure mode still applies to tree-sitter-c 0.24.2 (the
version this crate currently pins) directly — see the next section: a plain
`function_definition` named `DEV_APPROX_IRQProcess`-style with zero
attributes parses identically to any other function. There is no AST-level
distinction to catch that a regex would provide either — the point is that
*neither* a name regex *nor* a naive AST walk helps without checking for
actual attribute/macro evidence.

## What already exists in this crate to build on

- **`src/calls.rs`** — `CallEdge { caller, callee, is_external }`,
  `call_edges(root: Node, source: &str) -> Vec<CallEdge>`,
  `is_function_kind(kind: &str) -> bool`,
  `get_function_name(node: Node, source: &str) -> Option<String>`. This is
  exactly the per-file call-graph primitive tools_sqc's reachability
  follow-on (task 608) would combine with the ISR-detection primitive
  proposed here — no new work needed there.
- **`src/c_standard.rs`** — already distinguishes the two attribute syntaxes
  this ask needs to handle, for an unrelated purpose (C-standard-version
  detection): `"attribute_declaration" => Some(CStandard::C23)` (the `[[...]]`
  form) with an explicit comment that it's "structurally distinct from the
  GNU `__attribute__((...))` extension (a different node entirely)". Good
  precedent for how to write and comment this kind of node-kind dispatch in
  this crate's style.
- **`fn is_macro_function_definition(node: Node) -> bool`** in `calls.rs`
  (currently private, used to *exclude* macro-shaped function definitions
  from being treated as call-graph callers) checks
  `node.kind() == "function_definition" && declarator.kind() ==
  "parenthesized_declarator"`. This is precisely the shape a macro-defined
  ISR (AVR-libc's `ISR(vector) { ... }`, see next section) parses to — see
  "cross-cutting note" below, this interacts with the new primitive.

## Empirically verified: what each ISR-marking convention parses to (tree-sitter-c 0.24.2)

Verified directly against this crate's pinned `tree-sitter-c = "0.24"` (not
guessed from grammar docs) with a throwaway probe binary
(`tree_sitter::Parser` + `tree_sitter_c::LANGUAGE`), full source:

```c
ISR(TIMER1_COMPA_vect) {
    counter++;
}

__attribute__((interrupt)) void real_isr(void) {
    counter++;
}

void __attribute__((interrupt("IRQ"))) arm_isr(void) {
    counter++;
}

[[gnu::interrupt]] void c23_isr(void *frame) {
    counter++;
}

void DEV_APPROX_IRQProcess(void) {
    poll();
}
```

produces (S-expressions, reformatted per-function for readability; the
probe's actual output is one flat `translation_unit` sexp):

1. **AVR-libc `ISR(vector) { ... }` macro invocation** —
   `(function_definition type: (type_identifier) declarator:
   (parenthesized_declarator (identifier)) body: ...)`. `ISR` is misparsed as
   a return-type identifier (tree-sitter-c has no macro expansion, so it has
   no idea `ISR` isn't a type name) and `TIMER1_COMPA_vect` sits inside a
   `parenthesized_declarator`. **This is exactly the shape
   `is_macro_function_definition` already detects** (see above) — confirms
   the hypothesis, not a guess.
2. **GNU `__attribute__((interrupt))`** (no-arg form) —
   `(function_definition (attribute_specifier (argument_list (identifier)))
   type: ... declarator: (function_declarator ...))`. The `attribute_specifier`
   is a direct child of `function_definition`; its `argument_list`'s child is
   a bare `identifier` ("interrupt") for the no-argument form.
3. **GNU `__attribute__((interrupt("IRQ")))`** (with argument, ARM-style
   vector name) — same `attribute_specifier` position, but its
   `argument_list` child is a `call_expression` (`function:` field =
   identifier "interrupt", `arguments:` field = argument_list containing the
   string literal). So: bare `identifier` child = no-arg attribute,
   `call_expression` child = attribute name is the `function` field, args are
   in its `arguments` field.
4. **C23 `[[gnu::interrupt]]`** — `(function_definition (attribute_declaration
   (attribute prefix: (identifier) name: (identifier))) type: ...)`. Matches
   `node-types.json`'s documented schema exactly: `attribute` has `prefix`
   (optional) and `name` (required) fields, both `identifier`s.
5. **Bare ISR-ish name, no marking (`DEV_APPROX_IRQProcess`)** — an ordinary
   `function_definition`, structurally identical to any other function.
   Confirms the Catapult failure mode is real and current: nothing in the
   tree distinguishes it.

**Vendor keyword-style annotations do not parse cleanly** — also verified,
separate probe:

```c
#pragma vector=TIMER1_COMPA_vect
__interrupt void iar_isr(void) {
    counter++;
}

__irq void keil_isr(void) {
    counter++;
}
```

- `#pragma vector=...` parses as a standalone `preproc_call` node with **no
  AST link** to the function definition that follows it — IAR's convention is
  purely textual/positional (pragma line immediately precedes the function),
  the same "not a real parent-child relationship" problem
  `DETECT_DEAD_CODE_REGIONS.md` ran into with unbalanced-brace preprocessor
  conditionals. Detecting this convention means adjacency text-scanning, not
  a tree-sitter query.
- `__interrupt` (IAR) and `__irq` (Keil) both produced an `ERROR` node in the
  surrounding `function_definition` and set `tree.root_node().has_error() ==
  true` for the whole probe file (tested together in one file, not isolated
  per-keyword — worth re-verifying in isolation before relying on the exact
  error shape, but the qualitative finding — these keywords are not
  recognized C syntax to this grammar — is solid).

This means an implementation covering only `__attribute__(...)` and
`[[...]]` gets clean, error-free parses; covering IAR/Keil-style keyword
annotations means either accepting `ERROR`-node recovery (same category of
problem tools_sqc's own EXP33-C GNU-asm fix dealt with post tree-sitter
upgrade — see `docs/migration-tools_sqc.md`) or textual pattern matching
instead of AST queries for just that subset.

## Cross-cutting note for `calls.rs`

`collect_functions` (the internal helper `call_edges` uses to find callers)
currently **excludes** macro-shaped function definitions
(`is_macro_function_definition` returns true) from the call graph entirely —
so today, `ISR(TIMER1_COMPA_vect) { do_stuff(); }`-style AVR-libc handlers
are invisible to `call_edges()`: neither a caller nor (if called, which it
normally wouldn't be directly) a callee. Whoever builds ISR detection should
decide whether that's still fine (tools_sqc's reachability analysis roots at
ISR entry points and needs to see *out-edges* from inside the ISR body, not
treat the ISR as a caller elsewhere) or whether `call_edges`/
`collect_functions` needs a matching update to stop excluding
macro-function-shaped ISRs specifically. Flagging, not prescribing — may be a
non-issue depending on how tools_sqc's reachability analysis is structured.

## Suggested shape (not prescriptive — substrate agent's call)

Mirroring `c_standard.rs`'s single-pass, `Option`/`Vec`-returning style:

```rust
pub struct InterruptHandler {
    pub name: Option<String>,       // via get_function_name, when resolvable
    pub node_range: (usize, usize), // byte or point range of the function_definition
    pub evidence: InterruptEvidence,
}

pub enum InterruptEvidence {
    GnuAttribute { attribute_name: String },      // __attribute__((interrupt)), (("IRQ"))
    C23Attribute { prefix: Option<String>, name: String }, // [[gnu::interrupt]]
    MacroInvocation { macro_name: String },        // ISR(vector) — ISR, IRQ, etc.
    // Deliberately no NameHeuristic variant by default — see open questions.
}

pub fn interrupt_handlers(root: Node, source: &str) -> Vec<InterruptHandler>
```

feature-gated `lang-c` OR `lang-cpp` (same gate as `cpp_header::looks_like_cpp`
and the proposed `dead_code_ranges`), since both attribute syntaxes and the
macro-invocation shape are shared across C and C++ grammars.

## Open questions for whoever picks this up

- **Which macro names count as "ISR-defining"?** `ISR` (AVR-libc) is the one
  concretely cited in tools_sqc's evidence (Catapult, AVR-style embedded).
  Structurally, *any* macro invocation shaped like
  `SOMENAME(args) { body }` is indistinguishable from `ISR(args) { body }` at
  the parse level — there's no way to know from syntax alone that `ISR`
  specifically means "interrupt handler" vs. some unrelated project macro
  with the same call-like shape. Options: (a) ship a configurable/parameterized
  macro-name list (`ISR`, maybe vendor variants) the caller supplies rather
  than hardcoding it substrate-side, since "which macros mean ISR" is a
  project-convention fact, not a language fact; or (b) treat *any*
  macro-shaped function definition as a `MacroInvocation` evidence variant
  and let the caller (tools_sqc) filter by name — this fits the crate's
  per-file/leave-judgment-to-caller philosophy better than baking in a
  hardcoded macro name.
- **Is a low-confidence name-substring fallback tier worth including at
  all**, given the Catapult/`DEV_APPROX_IRQProcess` evidence argues it's
  actively misleading? Leaning no — tools_sqc already has its own crude
  version of that (task 607's stopgap) and can keep using it standalone if it
  wants a fallback; this primitive's whole value proposition is *not* being
  that. Recommend leaving name-substring matching out of this module
  entirely.
- **IAR `#pragma vector=`/`__interrupt`, Keil `__irq`** — in scope for v1, or
  a documented gap? Given the `ERROR`-node parse and the pragma's lack of an
  AST link to its target function, this is meaningfully more work than the
  attribute/macro cases. Suggest scoping v1 to GNU attributes + C23
  attributes + the AVR-libc-macro shape (covers the concrete evidence in
  hand), and filing the vendor-keyword forms as a documented follow-on gap
  rather than blocking v1 on them — consistent with
  `DETECT_DEAD_CODE_REGIONS.md`'s own "an imperfect v1 isn't silently wrong
  forever, tools_sqc's review process delta-adjudicates before claiming a
  precision win" reasoning.
- **`collect_functions`'s macro-function exclusion** (see "Cross-cutting
  note" above) — worth resolving before or alongside this, since it affects
  whether tools_sqc's downstream reachability analysis can see calls made
  *from inside* an `ISR(vector) { ... }` body.
- **C++ scope**: everything verified above was tested as C. `__attribute__`
  and `[[...]]` both exist in C++ too (feature-gated `lang-cpp` here already
  covers the grammar); worth a quick re-verification pass against
  `tree-sitter-cpp` before shipping the `lang-cpp` gate, rather than assuming
  the C shapes transfer unchanged.
