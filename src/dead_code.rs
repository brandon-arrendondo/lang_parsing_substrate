//! Preprocessor dead-code region detection for C/C++.
//!
//! Also compiled under `lang-csharp` alone (see this module's `cfg` in
//! `lib.rs`): [`crate::dead_code_csharp`] reuses this module's
//! comment-scanning helper for its own commented-out-`#define` detection
//! (which C and C++ no longer use: see below)
//! rather than duplicating it — the two languages share the same `//`/`/*
//! */` comment syntax, and the helper itself is pure text scanning with no
//! grammar dependency either way.
//!
//! Computes the 1-based inclusive line ranges of a C/C++ translation unit
//! that a preprocessor would strip before the compiler ever sees them. This
//! module covers two sub-problems in one pass, sharing a
//! single nesting/depth state machine so they don't disagree at edge cases
//! (e.g. a `#if 0` nested inside a dead `#ifdef MACRO` region, or vice
//! versa):
//!
//! 1. `#if 0` and `__cplusplus`-gated branches — dead when the translation
//!    unit is compiled as C.
//! 2. `#ifdef MACRO` / `#if defined(MACRO)` where `MACRO`'s definedness is
//!    locally provable from this file: `#define`d on every path to this
//!    point (branch always live, its `#else` dead), or `#undef`d on every
//!    path (branch always dead). A macro this file never mentions at all
//!    (e.g. a build-system flag like `_WIN32`) is left unclassified — there's
//!    no local evidence either way, and guessing would turn every such branch
//!    into a false positive. A commented-out `#define` is not evidence either:
//!    the build can still pass `-DNAME`.
//!
//! Deliberately line-based, not tree-sitter-based: a header shaped like
//!
//! ```c
//! #ifdef __cplusplus
//! extern "C" {
//! #endif
//! /* ... C declarations ... */
//! #ifdef __cplusplus
//! }
//! #endif
//! ```
//!
//! has an unbalanced `{` when parsed as C (the opening brace's `#ifdef` is
//! invisible to the parser, but the brace itself is real text) — tree-sitter
//! error recovery then mis-nests everything that follows, which is exactly
//! the construct this feature exists to handle. A plain-text line scanner
//! sidesteps that entirely, the same way [`mod@crate::suppressions`] and
//! [`mod@crate::regions`] do.
//!
//! `#define`/`#undef` evidence follows the paths through each conditional,
//! not literal nesting depth. Inside an arm, the arm's own `#define` holds
//! for the rest of that arm — so a file wrapped in one module guard
//! (`#if SUPPORT_MODULE_X`) that this scanner can't decide still has its
//! `#define`s counted inside the guard, since nothing inside it compiles
//! unless the guard is taken. At the `#endif` the arms meet again: a name
//! keeps a value only if every arm that may be compiled, and the implicit
//! empty `#else` when there is none, agree on it. So a `#define` under an
//! undecided `#ifdef _WIN32` leaves the name undecided after the `#endif`
//! (the POSIX build never ran it), while one in both arms of an `#if`/`#else`
//! holds. A conditional that tests one macro alone also tells each path
//! about that macro: the path that skips `#ifndef X` / `#define X 1` /
//! `#endif` had X defined already, so X is defined after it either way. An
//! arm this scanner has proven dead contributes nothing, and an arm proven
//! taken (the C side of `#ifndef __cplusplus`, the first arm of `#if 1`) is
//! the only path.
//! Directives inside comments, including multi-line `/* */` comments, are
//! not directives.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

#[cfg(feature = "lang-csharp")]
use crate::regions::marker_text;
#[cfg(feature = "lang-csharp")]
use crate::registry::SlocMode;

use BranchKind::{ElseDead, Neutral, ThenDead};

/// Why a [`DeadCodeRegion`] is provably never compiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadCodeReason {
    /// `#if 0`.
    IfZero,
    /// C++-only branch, dead when the translation unit is built as C.
    /// Covers `#ifdef __cplusplus`, `#if defined(__cplusplus) [&&...]`, and
    /// the dead `#else` of `#ifndef __cplusplus` / `#if !defined(__cplusplus)`.
    CppOnly,
    /// The macro is `#define`d on every path to this point with no later
    /// `#undef`, so the `#ifdef`/`#if defined(MACRO)` branch is always
    /// live and its `#else` is dead. Also produced, under
    /// [`dead_code_ranges_with_assumptions`], when the macro has no local
    /// `#define`/`#undef` of its own but the caller's [`PlatformAssumptions`]
    /// asserts it defined.
    AlwaysDefined,
    /// The macro is `#undef`d on every path to this point with no later
    /// `#define`, so the
    /// `#ifdef`/`#if defined(MACRO)` branch itself is dead. Also produced,
    /// under [`dead_code_ranges_with_assumptions`], when the macro has no
    /// local `#define`/`#undef` of its own but the caller's
    /// [`PlatformAssumptions`] asserts it undefined.
    NeverDefined,
}

/// A preprocessor-dead region: source that a C/C++ preprocessor strips
/// before the compiler ever sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeadCodeRegion {
    /// 1-based, inclusive first line of the dead region.
    pub start_line: usize,
    /// 1-based, inclusive last line of the dead region.
    pub end_line: usize,
    /// Why this region is dead.
    pub reason: DeadCodeReason,
}

/// Which branch of a preprocessor conditional is never compiled.
#[derive(Clone, Copy)]
enum BranchKind {
    /// The then-branch is dead; the `#else` branch (if any) is live.
    ThenDead(DeadCodeReason),
    /// The then-branch is live; the `#else` branch is dead.
    ElseDead(DeadCodeReason),
    /// Not a recognized dead conditional — both branches are live as far as
    /// this scanner knows.
    Neutral,
}

struct Frame {
    kind: BranchKind,
    /// Whether the dead region currently open (if any) was opened by this
    /// frame itself, as opposed to inherited from an enclosing dead frame
    /// (in which case this frame's own classification is moot — everything
    /// in it is already dead).
    self_caused: bool,
    /// The `#if` itself is proven true (its `#else` is proven dead, or it is
    /// a literal `#if 1`), so its first arm is the only path through the
    /// conditional: no `#elif`/`#else` arm and no skipping path joins it.
    then_taken: bool,
    has_else: bool,
    /// Whether an `#elif`/`#else` has been reached, so the current arm is no
    /// longer the first.
    left_first_arm: bool,
    /// Whether the current arm can be compiled at all, as far as this
    /// scanner knows. An arm inside a dead region writes nothing.
    arm_live: bool,
    /// Each name any arm of this conditional has written so far, with its
    /// value at the `#if` (`None`: no evidence).
    entry: HashMap<String, Option<Def>>,
    /// The names the current arm has written.
    arm_writes: HashSet<String>,
    /// The macro a bare `#ifdef`/`#ifndef`/`#if [!]defined(NAME)` tests, and
    /// whether the first arm is the one where it is defined. Every later arm,
    /// and the path that skips the conditional, has the opposite.
    tested: Option<(String, bool)>,
    /// What each finished live arm left behind.
    live_arms: Vec<Arm>,
}

/// One finished, live arm of a conditional.
struct Arm {
    /// The value at the arm's end of each name the arm wrote.
    writes: HashMap<String, Option<Def>>,
    /// Whether the tested macro (if any) is defined throughout this arm.
    tested_defined: bool,
}

impl Frame {
    /// The value `name` has at the end of a path through this conditional
    /// that wrote `writes` and on which the tested macro's definedness is
    /// `tested_defined`; `before` is its value at the `#if`.
    fn path_value(
        &self,
        name: &str,
        before: Option<Def>,
        writes: Option<&HashMap<String, Option<Def>>>,
        tested_defined: bool,
    ) -> Option<Def> {
        if let Some(value) = writes.and_then(|w| w.get(name)) {
            return *value;
        }
        match &self.tested {
            Some((tested, _)) if tested == name => Some(Def::from(tested_defined)),
            _ => before,
        }
    }
}

/// Assumed defined/undefined state for macros this file will never itself
/// settle — typically compiler- or build-system-predefined platform flags
/// (`_MSC_VER`, `_WIN32`, `__APPLE__`, `__vxworks`, ...) that no portable
/// source file `#define`s or `#undef`s itself. Distinct from the
/// locally-inferred `#define`/`#undef` evidence [`dead_code_ranges`] already
/// tracks: a caller-supplied fact standing in for evidence the file itself
/// will never contain, not something inferred from it. See
/// [`dead_code_ranges_with_assumptions`].
pub type PlatformAssumptions = HashMap<String, bool>;

/// A suggested starting point for [`PlatformAssumptions`]: "POSIX/Linux, no
/// Windows/vxworks compatibility shims active" — assumes `_WIN32`,
/// `_MSC_VER`, `__CYGWIN__`, and `__vxworks` undefined, and `__linux__` and
/// `__unix__` defined. A convenience default for the common case, not a
/// universal one — a caller targeting a different platform profile should
/// build its own table instead of starting from this one.
pub fn posix_default_assumptions() -> PlatformAssumptions {
    [
        ("_WIN32", false),
        ("_MSC_VER", false),
        ("__CYGWIN__", false),
        ("__vxworks", false),
        ("__linux__", true),
        ("__unix__", true),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value))
    .collect()
}

/// Computes the dead-code regions of `source`, a C or C++ translation unit.
/// See the module documentation for exactly what is and isn't recognized.
pub fn dead_code_ranges(source: &str) -> Vec<DeadCodeRegion> {
    dead_code_ranges_impl(source, None)
}

/// Like [`dead_code_ranges`], but seeds macro definedness with a
/// caller-supplied [`PlatformAssumptions`] table before walking the file —
/// standing in for evidence a compiler-predefined platform macro (e.g.
/// `_MSC_VER`, `__vxworks`) will never leave in the file's own text, which
/// [`dead_code_ranges`] correctly declines to guess about (see the module
/// documentation on sub-problem 2).
///
/// **Precedence:** a local `#define`/`#undef` for an assumed name overrides
/// the assumption from the point it takes effect onward — this is the same
/// `defined` map either way, and local evidence is simply written into it
/// after the assumption seeds it. This is a deliberate choice, not an
/// accident of implementation order: for the compiler-predefined platform
/// flags this is aimed at, a portable file that itself `#define`s
/// `_MSC_VER` or the like is unusual enough that it's more likely a bug in
/// the caller's assumption table than a real signal worth suppressing —
/// but since the file *did* say something concrete, trust it over an
/// external guess.
pub fn dead_code_ranges_with_assumptions(
    source: &str,
    assumed: &PlatformAssumptions,
) -> Vec<DeadCodeRegion> {
    dead_code_ranges_impl(source, Some(assumed))
}

fn dead_code_ranges_impl(
    source: &str,
    assumed: Option<&PlatformAssumptions>,
) -> Vec<DeadCodeRegion> {
    let mut scan = Scan {
        defined: assumed
            .map(|a| a.iter().map(|(k, &v)| (k.clone(), Def::from(v))).collect())
            .unwrap_or_default(),
        ..Scan::default()
    };
    let mut lex = Lex::Code;
    let mut total_lines = 0;

    for (idx, raw) in source.lines().enumerate() {
        total_lines = idx + 1;
        let line = strip_comments(raw, &mut lex);
        if let Some((directive, rest)) = parse_directive(&line) {
            scan.directive(idx + 1, directive, rest);
        }
    }

    scan.finish(total_lines)
}

/// What this file proves about one macro name at a point in the scan.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Def {
    Defined,
    Undefined,
    /// Defined on some path to this point and not on another: an arm this
    /// scanner can't decide wrote it. Unlike a name with no entry at all,
    /// this also overrides a caller's [`PlatformAssumptions`].
    Unknown,
}

impl From<bool> for Def {
    fn from(defined: bool) -> Self {
        if defined {
            Def::Defined
        } else {
            Def::Undefined
        }
    }
}

/// The value a name has on each of two paths that meet again: equal values
/// survive, anything else is [`Def::Unknown`].
fn join(a: Option<Def>, b: Option<Def>) -> Option<Def> {
    if a == b {
        a
    } else {
        Some(Def::Unknown)
    }
}

#[derive(Default)]
struct Scan {
    defined: HashMap<String, Def>,
    regions: Vec<DeadCodeRegion>,
    stack: Vec<Frame>,
    dead_start: Option<usize>,
}

impl Scan {
    fn directive(&mut self, line_no: usize, directive: &str, rest: &str) {
        match directive {
            "if" | "ifdef" | "ifndef" => self.open(line_no, directive, rest),
            "elif" => self.next_arm(line_no, false),
            "else" => self.next_arm(line_no, true),
            "endif" => self.close(line_no),
            "define" | "undef" if self.dead_start.is_none() => {
                if let Some(name) = first_ident(rest) {
                    self.set(name, Some(Def::from(directive == "define")));
                }
            }
            _ => {}
        }
    }

    fn open(&mut self, line_no: usize, directive: &str, rest: &str) {
        let kind = if self.dead_start.is_some() {
            Neutral
        } else {
            classify_conditional(directive, rest, &self.defined)
        };
        let self_caused = self.dead_start.is_none() && matches!(kind, ThenDead(_));
        if self_caused {
            self.dead_start = Some(line_no + 1);
        }
        self.stack.push(Frame {
            kind,
            self_caused,
            then_taken: matches!(kind, ElseDead(_))
                || (self.dead_start.is_none() && directive == "if" && is_one_condition(rest)),
            has_else: false,
            left_first_arm: false,
            arm_live: self.dead_start.is_none(),
            entry: HashMap::new(),
            arm_writes: HashSet::new(),
            tested: tested_macro(directive, rest).map(|(n, d)| (n.to_string(), d)),
            live_arms: Vec::new(),
        });
    }

    /// `#elif` or `#else`: closes the current arm and opens the next.
    fn next_arm(&mut self, line_no: usize, is_else: bool) {
        if self.stack.is_empty() {
            return;
        }
        self.end_arm();
        let Some(frame) = self.stack.last_mut() else {
            return;
        };
        if frame.self_caused {
            let kind = frame.kind;
            frame.self_caused = false;
            close_region(&mut self.regions, &mut self.dead_start, line_no - 1, kind);
        } else if is_else && self.dead_start.is_none() && matches!(frame.kind, ElseDead(_)) {
            self.dead_start = Some(line_no + 1);
            frame.self_caused = true;
        }
        frame.has_else |= is_else;
        frame.arm_live = self.dead_start.is_none();
    }

    fn close(&mut self, line_no: usize) {
        if self.stack.is_empty() {
            return;
        }
        self.end_arm();
        let Some(frame) = self.stack.pop() else {
            return;
        };
        if frame.self_caused {
            close_region(
                &mut self.regions,
                &mut self.dead_start,
                line_no - 1,
                frame.kind,
            );
        }
        // A missing `#else` is an empty arm, taken whenever the condition
        // is false: on that path every name keeps its value from the `#if`,
        // except that a tested macro has the definedness that skipped the
        // arm (`#ifndef X` / `#define X 1` / `#endif` leaves X defined on
        // both paths).
        let implicit_else = !frame.has_else && !frame.then_taken;
        let paths = frame.live_arms.len() + usize::from(implicit_else);
        let tested = frame.tested.as_ref().map(|(name, _)| name.as_str());
        // Join each written name over the paths that wrote it, then with its
        // value at the `#if` if some path did not. Linear in the writes, so
        // a long `#elif` chain stays cheap.
        let mut joined: HashMap<&str, (Option<Def>, usize)> = HashMap::new();
        for arm in &frame.live_arms {
            for (name, value) in &arm.writes {
                if Some(name.as_str()) == tested {
                    continue;
                }
                joined
                    .entry(name)
                    .and_modify(|(acc, n)| {
                        *acc = join(*acc, *value);
                        *n += 1;
                    })
                    .or_insert((*value, 1));
            }
        }
        let mut after: Vec<(String, Option<Def>)> = joined
            .into_iter()
            .map(|(name, (value, n))| {
                let before = frame.entry.get(name).copied().flatten();
                let value = if n < paths {
                    join(value, before)
                } else {
                    value
                };
                (name.to_string(), value)
            })
            .collect();
        // The tested macro: a path that did not write it still knows its
        // definedness from the test (`#ifndef X` / `#define X 1` / `#endif`
        // leaves X defined on both paths).
        if let Some(name) = tested.filter(|n| frame.entry.contains_key(*n)) {
            let before = frame.entry[name];
            let skip_defined = frame.tested.as_ref().is_some_and(|(_, first)| !first);
            let mut values: Vec<Option<Def>> = frame
                .live_arms
                .iter()
                .map(|arm| frame.path_value(name, before, Some(&arm.writes), arm.tested_defined))
                .collect();
            if implicit_else {
                values.push(frame.path_value(name, before, None, skip_defined));
            }
            after.push((
                name.to_string(),
                values.into_iter().reduce(join).unwrap_or(before),
            ));
        }
        for (name, value) in after {
            self.set(&name, value);
        }
    }

    /// Records the current arm's writes (if it can be compiled at all) and
    /// rewinds every name it wrote to its value at the
    /// `#if`, so the next arm starts where the conditional did.
    fn end_arm(&mut self) {
        let Some(frame) = self.stack.last_mut() else {
            return;
        };
        let first_arm = !frame.left_first_arm;
        frame.left_first_arm = true;
        let written: Vec<String> = frame.arm_writes.drain().collect();
        if frame.arm_live && (first_arm || !frame.then_taken) {
            let writes = written
                .iter()
                .map(|name| (name.clone(), self.defined.get(name).copied()))
                .collect();
            let tested_defined = frame.tested.as_ref().is_some_and(|(_, d)| *d == first_arm);
            frame.live_arms.push(Arm {
                writes,
                tested_defined,
            });
        }
        // Only this arm's writes can differ from the `#if`'s values: every
        // arm starts from them.
        for name in written {
            match frame.entry.get(&name).copied().flatten() {
                Some(value) => self.defined.insert(name, value),
                None => self.defined.remove(&name),
            };
        }
    }

    /// Writes `name`'s value, first noting what it was at the innermost
    /// open conditional's `#if` so that conditional can rewind and join it.
    fn set(&mut self, name: &str, value: Option<Def>) {
        if let Some(frame) = self.stack.last_mut() {
            if !frame.entry.contains_key(name) {
                frame
                    .entry
                    .insert(name.to_string(), self.defined.get(name).copied());
            }
            frame.arm_writes.insert(name.to_string());
        }
        match value {
            Some(value) => self.defined.insert(name.to_string(), value),
            None => self.defined.remove(name),
        };
    }

    fn finish(mut self, total_lines: usize) -> Vec<DeadCodeRegion> {
        if let Some(start) = self.dead_start {
            let reason = self
                .stack
                .iter()
                .rev()
                .find(|f| f.self_caused)
                .and_then(|f| match f.kind {
                    ThenDead(r) | ElseDead(r) => Some(r),
                    Neutral => None,
                })
                .unwrap_or(DeadCodeReason::IfZero);
            push_region(&mut self.regions, start, total_lines, reason);
        }
        self.regions
    }
}

fn close_region(
    regions: &mut Vec<DeadCodeRegion>,
    dead_start: &mut Option<usize>,
    end_line: usize,
    kind: BranchKind,
) {
    if let Some(start) = dead_start.take() {
        if let ThenDead(reason) | ElseDead(reason) = kind {
            push_region(regions, start, end_line, reason);
        }
    }
}

fn push_region(
    regions: &mut Vec<DeadCodeRegion>,
    start: usize,
    end: usize,
    reason: DeadCodeReason,
) {
    if start <= end {
        regions.push(DeadCodeRegion {
            start_line: start,
            end_line: end,
            reason,
        });
    }
}

/// Classifies an `#if`/`#ifdef`/`#ifndef` line. `rest` is everything after
/// the directive keyword, unparsed.
fn classify_conditional(directive: &str, rest: &str, defined: &HashMap<String, Def>) -> BranchKind {
    match directive {
        "ifdef" => first_ident(rest).map_or(Neutral, |n| classify_name(n, false, defined)),
        "ifndef" => first_ident(rest).map_or(Neutral, |n| classify_name(n, true, defined)),
        "if" => {
            if is_zero_condition(rest) {
                ThenDead(DeadCodeReason::IfZero)
            } else {
                classify_if_condition(rest, defined)
            }
        }
        _ => Neutral,
    }
}

/// Classifies a bare macro name from `#ifdef`/`#ifndef` (`is_ifndef` tells
/// which) or from `defined(NAME)`/`!defined(NAME)` inside `#if`.
fn classify_name(name: &str, is_ifndef: bool, defined: &HashMap<String, Def>) -> BranchKind {
    if name == "__cplusplus" {
        return if is_ifndef {
            ElseDead(DeadCodeReason::CppOnly)
        } else {
            ThenDead(DeadCodeReason::CppOnly)
        };
    }
    match defined.get(name) {
        Some(Def::Defined) => {
            if is_ifndef {
                ThenDead(DeadCodeReason::AlwaysDefined)
            } else {
                ElseDead(DeadCodeReason::AlwaysDefined)
            }
        }
        Some(Def::Undefined) => {
            if is_ifndef {
                ElseDead(DeadCodeReason::NeverDefined)
            } else {
                ThenDead(DeadCodeReason::NeverDefined)
            }
        }
        Some(Def::Unknown) | None => Neutral,
    }
}

/// Classifies an `#if` condition that isn't the literal `0` — looks for
/// `defined(NAME)` / `defined NAME`, optionally negated with a leading `!`,
/// optionally followed by `&& ...` (only the `ThenDead` side survives a
/// trailing `&&`, since a live-but-unproven first clause can't prove the
/// whole condition either way).
fn classify_if_condition(rest: &str, defined: &HashMap<String, Def>) -> BranchKind {
    let trimmed = strip_trailing_comment(rest).trim();

    if let Some(after_bang) = trimmed.strip_prefix('!') {
        return match parse_defined(after_bang.trim_start()) {
            Some((name, remainder)) if remainder.trim().is_empty() => {
                classify_name(name, true, defined)
            }
            _ => Neutral,
        };
    }

    match parse_defined(trimmed) {
        Some((name, remainder)) => {
            let kind = classify_name(name, false, defined);
            let remainder = remainder.trim_start();
            let bare = remainder.is_empty();
            // `defined(X) && ...` is false whenever X is undefined, unless an
            // `||` lets the rest be true on its own.
            let leading_and = remainder.starts_with("&&")
                && !remainder.contains("||")
                && matches!(kind, ThenDead(_));
            if bare || leading_and {
                kind
            } else {
                Neutral
            }
        }
        None => Neutral,
    }
}

/// The macro whose definedness alone decides an `#ifdef NAME`, `#ifndef
/// NAME`, `#if defined(NAME)` or `#if !defined(NAME)`, and whether the first
/// arm is the one where it is defined. `None` for any other condition.
fn tested_macro<'a>(directive: &str, rest: &'a str) -> Option<(&'a str, bool)> {
    match directive {
        "ifdef" => first_ident(rest).map(|n| (n, true)),
        "ifndef" => first_ident(rest).map(|n| (n, false)),
        "if" => {
            let trimmed = strip_trailing_comment(rest).trim();
            let (negated, cond) = match trimmed.strip_prefix('!') {
                Some(after) => (true, after.trim_start()),
                None => (false, trimmed),
            };
            match parse_defined(cond) {
                Some((name, after)) if after.trim().is_empty() => Some((name, !negated)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Parses a leading `defined(NAME)` or `defined NAME` off `s`, returning the
/// name and whatever text follows it.
fn parse_defined(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    let rest = s.strip_prefix("defined")?;
    let rest = rest.trim_start();
    if let Some(inner) = rest.strip_prefix('(') {
        let close = inner.find(')')?;
        let name = inner[..close].trim();
        if name.is_empty() {
            return None;
        }
        Some((name, &inner[close + 1..]))
    } else {
        let name = first_ident(rest)?;
        Some((name, &rest[name.len()..]))
    }
}

/// `#if 1`: used only to know that its first arm is always taken. Its
/// `#elif`/`#else` arms are not reported dead.
fn is_one_condition(rest: &str) -> bool {
    let stripped: String = strip_trailing_comment(rest)
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '(' && *c != ')')
        .collect();
    stripped == "1"
}

fn is_zero_condition(rest: &str) -> bool {
    let stripped: String = strip_trailing_comment(rest)
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '(' && *c != ')')
        .collect();
    stripped == "0"
}

fn strip_trailing_comment(s: &str) -> &str {
    let line_pos = s.find("//");
    let block_pos = s.find("/*");
    match (line_pos, block_pos) {
        (Some(a), Some(b)) => &s[..a.min(b)],
        (Some(a), None) => &s[..a],
        (None, Some(b)) => &s[..b],
        (None, None) => s,
    }
}

fn first_ident(s: &str) -> Option<&str> {
    let s = s.trim_start();
    let end = s
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    if end == 0 {
        None
    } else {
        Some(&s[..end])
    }
}

/// Where a line starts, lexically, given the lines before it.
#[derive(Default, Clone, PartialEq, Eq, Debug)]
enum Lex {
    #[default]
    Code,
    /// Inside a `/* */` comment.
    Block,
    /// A `//` comment continued onto this line by a trailing backslash.
    LineComment,
    /// A string or character literal (its quote) continued onto this line
    /// by a trailing backslash.
    Quote(u8),
    /// A C++ raw string `R"delim( ... )delim"`, closed by `)delim"`.
    Raw(String),
}

/// Removes the comments from one line of C or C++ source, replacing each
/// with a space as the preprocessor does. `state` carries a comment or
/// literal left open at the end of one line into the next, so text wholly
/// inside a block comment, a raw string or a backslash-continued literal
/// comes back blank, and a `#define` or `#if` written inside one is never
/// read as a directive. Literals that open on this line are kept as written,
/// and the `/*` in `"/*"` opens nothing.
fn strip_comments<'a>(line: &'a str, state: &mut Lex) -> Cow<'a, str> {
    if *state == Lex::Code && !line.contains(['/', '"', '\'']) {
        return Cow::Borrowed(line);
    }
    let bytes = line.as_bytes();
    let continued = line.ends_with('\\');
    let mut out = String::with_capacity(line.len());
    // A literal carried in from an earlier line is blanked up to its close.
    let mut carried = matches!(state, Lex::Quote(_) | Lex::Raw(_));
    // Start of the text not yet copied (or skipped). Every index it takes
    // sits next to an ASCII byte, so it is a char boundary.
    let mut keep_from = if carried || *state != Lex::Code {
        line.len()
    } else {
        0
    };
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        let next = bytes.get(i + 1).copied();
        match state {
            Lex::LineComment => break,
            Lex::Block => {
                if c == b'*' && next == Some(b'/') {
                    *state = Lex::Code;
                    out.push(' ');
                    keep_from = i + 2;
                    i += 1;
                }
            }
            Lex::Quote(q) => {
                if c == b'\\' {
                    i += 1;
                } else if c == *q {
                    *state = Lex::Code;
                    if carried {
                        carried = false;
                        out.push(' ');
                        keep_from = i + 1;
                    }
                }
            }
            Lex::Raw(delim) => {
                let close = i + 1 + delim.len();
                if c == b')'
                    && line.get(i + 1..close) == Some(delim.as_str())
                    && bytes.get(close) == Some(&b'"')
                {
                    *state = Lex::Code;
                    if carried {
                        carried = false;
                        out.push(' ');
                        keep_from = close + 1;
                    }
                    i = close;
                }
            }
            Lex::Code => {
                if c == b'"' {
                    *state = match raw_string_delimiter(line, i) {
                        Some((delim, open)) => {
                            i = open;
                            Lex::Raw(delim.to_string())
                        }
                        None => Lex::Quote(c),
                    };
                } else if c == b'\'' && !(i > 0 && bytes[i - 1].is_ascii_digit()) {
                    // (A `'` after a digit is a C23/C++14 digit separator.)
                    *state = Lex::Quote(c);
                } else if c == b'/' && (next == Some(b'/') || next == Some(b'*')) {
                    out.push_str(&line[keep_from..i]);
                    out.push(' ');
                    keep_from = line.len();
                    *state = if next == Some(b'/') {
                        Lex::LineComment
                    } else {
                        Lex::Block
                    };
                    i += 1;
                }
            }
        }
        i += 1;
    }
    if keep_from < line.len() && !carried {
        out.push_str(&line[keep_from..]);
    }
    // A literal or `//` comment ends with its line unless a backslash
    // splices the next line on; a block comment or raw string does not.
    if matches!(state, Lex::LineComment | Lex::Quote(_)) && !continued {
        *state = Lex::Code;
    }
    Cow::Owned(out)
}

/// If the `"` at `quote` opens a C++ raw string (`R"`, `LR"`, `uR"`, `UR"`
/// or `u8R"`, then a delimiter of at most 16 characters and `(`), returns
/// the delimiter and the index of that `(`.
fn raw_string_delimiter(line: &str, quote: usize) -> Option<(&str, usize)> {
    let bytes = line.as_bytes();
    let mut start = quote;
    while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
        start -= 1;
    }
    if !matches!(&line[start..quote], "R" | "LR" | "uR" | "UR" | "u8R") {
        return None;
    }
    let rest = &line[quote + 1..];
    let open = rest.find('(')?;
    let delim = &rest[..open];
    let valid = delim.len() <= 16
        && delim
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b'(' | b')' | b'\\' | b'"'));
    valid.then_some((delim, quote + 1 + open))
}

/// Splits `line` into a leading `#`-directive keyword and the rest of the
/// line, if `line` is a preprocessor directive at all.
fn parse_directive(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim_start();
    let after_hash = trimmed.strip_prefix('#')?;
    let after_hash = after_hash.trim_start();
    let word_end = after_hash
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(after_hash.len());
    if word_end == 0 {
        return None;
    }
    Some((&after_hash[..word_end], &after_hash[word_end..]))
}

/// Collects every macro name that appears as a commented-out `#define` (a
/// whole line that is entirely a `//` or single-line `/* */` comment whose
/// content is a `#define NAME` directive) anywhere in `source` — evidence
/// that the macro was meant to be locally defined but currently isn't. Only
/// the C# scanner uses it: for C and C++ a commented-out `#define` is no
/// evidence, since the build can still pass `-DNAME`.
#[cfg(feature = "lang-csharp")]
pub(crate) fn commented_out_define_names(source: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    for line in source.lines() {
        let Some(text) = marker_text(line, SlocMode::Default) else {
            continue;
        };
        let Some(rest) = text.trim_start().strip_prefix('#') else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix("define") else {
            continue;
        };
        if let Some(name) = first_ident(rest) {
            out.insert(name.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(source: &str) -> Vec<(usize, usize, DeadCodeReason)> {
        dead_code_ranges(source)
            .into_iter()
            .map(|r| (r.start_line, r.end_line, r.reason))
            .collect()
    }

    #[test]
    fn if_zero_is_dead() {
        let src = "a();\n#if 0\nb();\nc();\n#endif\nd();\n";
        assert_eq!(ranges(src), vec![(3, 4, DeadCodeReason::IfZero)]);
    }

    #[test]
    fn if_zero_with_trailing_comment() {
        let src = "#if 0 // disabled\nx();\n#endif\n";
        assert_eq!(ranges(src), vec![(2, 2, DeadCodeReason::IfZero)]);
    }

    #[test]
    fn if_parenthesized_zero_is_dead() {
        let src = "#if (0)\nx();\n#endif\n";
        assert_eq!(ranges(src), vec![(2, 2, DeadCodeReason::IfZero)]);
    }

    #[test]
    fn ifdef_cplusplus_then_branch_dead() {
        let src = "#ifdef __cplusplus\nextern \"C\" {\n#endif\nint f(void);\n";
        assert_eq!(ranges(src), vec![(2, 2, DeadCodeReason::CppOnly)]);
    }

    #[test]
    fn extern_c_header_pair_produces_two_separate_regions() {
        let src = concat!(
            "#ifdef __cplusplus\n", // 1
            "extern \"C\" {\n",     // 2
            "#endif\n",             // 3
            "int f(void);\n",       // 4
            "#ifdef __cplusplus\n", // 5
            "}\n",                  // 6
            "#endif\n",             // 7
        );
        assert_eq!(
            ranges(src),
            vec![
                (2, 2, DeadCodeReason::CppOnly),
                (6, 6, DeadCodeReason::CppOnly),
            ]
        );
    }

    #[test]
    fn ifndef_cplusplus_else_branch_dead() {
        let src = "#ifndef __cplusplus\nlive();\n#else\ndead();\n#endif\n";
        assert_eq!(ranges(src), vec![(4, 4, DeadCodeReason::CppOnly)]);
    }

    #[test]
    fn if_defined_cplusplus_with_and_is_dead() {
        let src = "#if defined(__cplusplus) && SOMETHING\nx();\n#endif\n";
        assert_eq!(ranges(src), vec![(2, 2, DeadCodeReason::CppOnly)]);
    }

    #[test]
    fn if_not_defined_cplusplus_else_branch_dead() {
        let src = "#if !defined(__cplusplus)\nlive();\n#else\ndead();\n#endif\n";
        assert_eq!(ranges(src), vec![(4, 4, DeadCodeReason::CppOnly)]);
    }

    // Raylib rtext.c-shaped case 1: macro unconditionally
    // #define'd two lines above the #if defined(...) — the #else is dead.
    #[test]
    fn always_defined_macro_else_branch_dead() {
        let src = concat!(
            "#define SUPPORT_COMPRESSED_FONT_ATLAS\n",      // 1
            "void f(void) {\n",                             // 2
            "#if defined(SUPPORT_COMPRESSED_FONT_ATLAS)\n", // 3
            "    live_call();\n",                           // 4
            "#else\n",                                      // 5
            "    sprintf(buf, \"%d\", n);\n",               // 6
            "#endif\n",                                     // 7
            "}\n",                                          // 8
        );
        assert_eq!(ranges(src), vec![(6, 6, DeadCodeReason::AlwaysDefined)]);
    }

    // A commented-out `#define` is not evidence: `-DSUPPORT_FONT_DATA_COPY`
    // still compiles the branch.
    #[test]
    fn commented_out_define_is_not_evidence() {
        let src = concat!(
            "//#define SUPPORT_FONT_DATA_COPY\n",    // 1
            "void f(void) {\n",                      // 2
            "#if defined(SUPPORT_FONT_DATA_COPY)\n", // 3
            "    sprintf(buf, \"%d\", n);\n",        // 4
            "#endif\n",                              // 5
            "}\n",                                   // 6
        );
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn block_commented_define_is_not_evidence() {
        let src = "/* #define HAVE_FOO */\n#ifdef HAVE_FOO\n#include \"foo.h\"\n#endif\n";
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn directives_inside_a_multi_line_comment_are_ignored() {
        let src = concat!(
            "/*\n",                 // 1
            "#define HAVE_FOO\n",   // 2
            "#if 0\n",              // 3
            "*/\n",                 // 4
            "#ifdef HAVE_FOO\n",    // 5
            "#include \"foo.h\"\n", // 6
            "#else\n",              // 7
            "bar();\n",             // 8
            "#endif\n",             // 9
        );
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn string_literal_holding_comment_or_define_text_changes_nothing() {
        let src = concat!(
            "const char *s = \"/*\";\n",          // 1
            "const char *t = \"#define FOO\";\n", // 2
            "#define FOO\n",                      // 3
            "#ifdef FOO\n",                       // 4
            "live();\n",                          // 5
            "#else\n",                            // 6
            "dead();\n",                          // 7
            "#endif\n",                           // 8
        );
        assert_eq!(ranges(src), vec![(7, 7, DeadCodeReason::AlwaysDefined)]);
    }

    #[test]
    fn define_after_trailing_block_comment_close_still_counts() {
        let src = "/* note\n */ #define FOO\n#ifndef FOO\ndead();\n#endif\n";
        assert_eq!(ranges(src), vec![(4, 4, DeadCodeReason::AlwaysDefined)]);
    }

    #[test]
    fn ifdef_macro_never_mentioned_is_neutral() {
        let src = "#ifdef _WIN32\nmaybe_dead();\n#endif\n";
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn undef_with_no_later_define_makes_ifdef_dead() {
        let src = "#define FOO\n#undef FOO\n#ifdef FOO\ndead();\n#endif\n";
        assert_eq!(ranges(src), vec![(4, 4, DeadCodeReason::NeverDefined)]);
    }

    #[test]
    fn undef_with_no_later_define_makes_ifndef_live() {
        let src = "#define FOO\n#undef FOO\n#ifndef FOO\nlive();\n#else\ndead();\n#endif\n";
        assert_eq!(ranges(src), vec![(6, 6, DeadCodeReason::NeverDefined)]);
    }

    #[test]
    fn define_inside_dead_branch_does_not_count() {
        // FOO is only #define'd inside a *proven-dead* `#if 0` branch, which
        // never executes — it must not be treated as having defined FOO.
        // Line 2 (the #define itself) is legitimately dead as part of the
        // `#if 0` block; the later `#ifdef FOO` block (lines 4-6) must NOT
        // additionally be reported as dead just because it saw the #define.
        let src = "#if 0\n#define FOO\n#endif\n#ifdef FOO\nx();\n#endif\n";
        assert_eq!(ranges(src), vec![(2, 2, DeadCodeReason::IfZero)]);
    }

    // A #define inside an arm this scanner can't decide holds only on the
    // path through that arm: after the #endif, the path that skipped the arm
    // has FOO undefined, so neither branch of `#ifdef FOO` is dead.
    #[test]
    fn define_inside_unproven_branch_is_undecided_after_it() {
        let src = "#ifndef BAR\n#define FOO\n#endif\n\
                   #ifdef FOO\nlive();\n#else\nlive_too();\n#endif\n";
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn define_under_platform_arm_leaves_posix_else_live() {
        let src = concat!(
            "#ifdef _WIN32\n",        // 1
            "#define USE_W\n",        // 2
            "#endif\n",               // 3
            "#ifdef USE_W\n",         // 4
            "#include <windows.h>\n", // 5
            "#else\n",                // 6
            "#include \"posix.h\"\n", // 7
            "#endif\n",               // 8
        );
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn define_in_every_arm_of_an_undecided_conditional_counts() {
        let src = concat!(
            "#ifdef _WIN32\n",      // 1
            "#define SEP '\\\\'\n", // 2
            "#else\n",              // 3
            "#define SEP '/'\n",    // 4
            "#endif\n",             // 5
            "#ifndef SEP\n",        // 6
            "dead();\n",            // 7
            "#endif\n",             // 8
        );
        assert_eq!(ranges(src), vec![(7, 7, DeadCodeReason::AlwaysDefined)]);
    }

    #[test]
    fn define_in_one_elif_arm_only_is_undecided() {
        let src = concat!(
            "#if defined(A)\n",   // 1
            "#define X\n",        // 2
            "#elif defined(B)\n", // 3
            "#define X\n",        // 4
            "#else\n",            // 5
            "#endif\n",           // 6
            "#ifdef X\n",         // 7
            "x();\n",             // 8
            "#endif\n",           // 9
        );
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn define_inside_proven_live_arm_counts_after_it() {
        // `#ifndef __cplusplus` is proven taken when built as C, so its
        // #define holds after the #endif.
        let src = concat!(
            "#ifndef __cplusplus\n", // 1
            "#define C_ONLY\n",      // 2
            "#endif\n",              // 3
            "#ifdef C_ONLY\n",       // 4
            "live();\n",             // 5
            "#else\n",               // 6
            "dead();\n",             // 7
            "#endif\n",              // 8
        );
        assert_eq!(ranges(src), vec![(7, 7, DeadCodeReason::AlwaysDefined)]);
    }

    #[test]
    fn define_inside_live_else_of_if_zero_counts_after_it() {
        let src = "#if 0\n#define X 1\n#else\n#define Y\n#endif\n\
                   #ifdef Y\nlive();\n#else\ndead();\n#endif\n\
                   #ifdef X\nx();\n#endif\n";
        assert_eq!(
            ranges(src),
            vec![
                (2, 2, DeadCodeReason::IfZero),
                (9, 9, DeadCodeReason::AlwaysDefined),
            ]
        );
    }

    #[test]
    fn undef_inside_undecided_arm_is_undecided_after_it() {
        let src = "#define FOO\n#ifdef _WIN32\n#undef FOO\n#endif\n\
                   #ifdef FOO\nmaybe();\n#else\nmaybe_too();\n#endif\n";
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn include_guard_defines_hold_inside_the_guard() {
        let src = concat!(
            "#ifndef FOO_H\n", // 1
            "#define FOO_H\n", // 2
            "#define USE_X\n", // 3
            "#ifdef USE_X\n",  // 4
            "live();\n",       // 5
            "#else\n",         // 6
            "dead();\n",       // 7
            "#endif\n",        // 8
            "#ifndef FOO_H\n", // 9
            "dead_too();\n",   // 10
            "#endif\n",        // 11
            "#endif\n",        // 12
        );
        assert_eq!(
            ranges(src),
            vec![
                (7, 7, DeadCodeReason::AlwaysDefined),
                (10, 10, DeadCodeReason::AlwaysDefined),
            ]
        );
    }

    /// The `#else` of `#ifdef FAST ... #else ... #endif` after `default`.
    fn else_of_fast_after(default: &str) -> Vec<(usize, usize, DeadCodeReason)> {
        let src = format!("{default}#ifdef FAST\nfast();\n#else\nslow();\n#endif\n");
        let slow = default.lines().count() + 4;
        let got = ranges(&src);
        assert!(
            got.iter().all(|&(a, b, _)| a <= slow && slow <= b) || got.is_empty(),
            "{default}: {got:?}"
        );
        got.into_iter().filter(|&(a, _, _)| a == slow).collect()
    }

    #[test]
    fn default_value_guard_leaves_the_macro_defined_on_every_path() {
        for default in [
            "#ifndef FAST\n#define FAST 1\n#endif\n",
            "#if !defined(FAST)\n#define FAST\n#endif\n",
            "#if ! defined FAST /* default */\n#define FAST 1\n#endif\n",
            "#ifndef FAST\n#define FAST 1\n#else\n#endif\n",
            "#ifdef FAST\n#else\n#define FAST 1\n#endif\n",
            "#ifdef WIN\n#define FAST 1\n#endif\n#ifndef FAST\n#define FAST 1\n#endif\n",
            "#if 0\n#else\n#ifndef FAST\n#define FAST 1\n#endif\n#endif\n",
            "#ifndef FAST\n#if 1\n#define FAST 1\n#endif\n#endif\n",
            "#if 1\n#ifndef FAST\n#define FAST 1\n#endif\n#elif defined(WIN)\n#define W 1\n#endif\n",
            "#ifndef __cplusplus\n#define FAST 1\n#elif defined(X)\n#undef FAST\n#endif\n",
            "#ifndef FAST\n#define FAST 1\n#endif\n#ifdef WIN\n#undef FAST\n#define FAST 2\n#endif\n",
        ] {
            let slow = default.lines().count() + 4;
            assert_eq!(
                else_of_fast_after(default),
                vec![(slow, slow, DeadCodeReason::AlwaysDefined)],
                "{default}"
            );
        }
    }

    #[test]
    fn a_guard_that_does_not_always_define_the_macro_proves_nothing() {
        for default in [
            // Defined only under an unrelated undecided macro.
            "#ifndef OTHER\n#define FAST 1\n#endif\n",
            // A default that a platform then removes.
            "#ifndef FAST\n#define FAST 1\n#endif\n#ifdef WIN\n#undef FAST\n#endif\n",
            // Defined in the arm where it already was, absent in the other.
            "#ifdef FAST\n#define FAST 2\n#else\n#endif\n#ifdef WIN\n#undef FAST\n#endif\n",
            // `#elif`: the default holds only in one arm.
            "#ifndef FAST\n#elif defined(X)\n#undef FAST\n#endif\n",
        ] {
            assert!(else_of_fast_after(default).is_empty(), "{default}");
        }
    }

    #[test]
    fn a_quote_inside_a_raw_string_does_not_open_a_comment() {
        let src = concat!(
            "#ifdef __cplusplus\n",                  // 1
            "const char *r = R\"x(a \" /* b)x\";\n", // 2
            "#endif\n",                              // 3
            "int live(void);\n",                     // 4
            "#if 0\n",                               // 5
            "dead();\n",                             // 6
            "#endif\n",                              // 7
        );
        assert_eq!(
            ranges(src),
            vec![
                (2, 2, DeadCodeReason::CppOnly),
                (6, 6, DeadCodeReason::IfZero)
            ]
        );
    }

    #[test]
    fn a_continued_string_does_not_open_a_comment_on_its_next_line() {
        let src = concat!(
            "#ifdef __cplusplus\n",     // 1
            "const char *s = \"a \\\n", // 2
            "/* b\";\n",                // 3
            "#endif\n",                 // 4
            "int live(void);\n",        // 5
        );
        assert_eq!(ranges(src), vec![(2, 3, DeadCodeReason::CppOnly)]);
    }

    #[test]
    fn a_define_inside_a_multi_line_raw_string_is_not_a_directive() {
        let src = concat!(
            "const char *r = R\"sql(\n", // 1
            "#define FOO\n",             // 2
            "#if 0\n",                   // 3
            ")sql\";\n",                 // 4
            "#ifdef FOO\n",              // 5
            "maybe();\n",                // 6
            "#else\n",                   // 7
            "maybe_too();\n",            // 8
            "#endif\n",                  // 9
        );
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn a_define_inside_a_continued_string_is_not_a_directive() {
        let src = "const char *s = \"a \\\n#define FOO\";\n\
                   #ifdef FOO\nmaybe();\n#else\nmaybe_too();\n#endif\n";
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn a_continued_line_comment_hides_the_next_line() {
        let src = "// note \\\n#define FOO\n#ifdef FOO\nmaybe();\n#else\nmaybe_too();\n#endif\n";
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn a_digit_separator_does_not_open_a_char_literal() {
        let src = "int n = 1'000; /* c */\n#define FOO\n#ifndef FOO\ndead();\n#endif\n";
        assert_eq!(ranges(src), vec![(4, 4, DeadCodeReason::AlwaysDefined)]);
    }

    #[test]
    fn defined_and_rest_with_an_or_decides_nothing() {
        let src = "#define A\n#undef A\n#if defined(A) && B || C\nmaybe();\n#endif\n";
        assert!(ranges(src).is_empty());
        let and_only = "#define A\n#undef A\n#if defined(A) && B\ndead();\n#endif\n";
        assert_eq!(ranges(and_only), vec![(4, 4, DeadCodeReason::NeverDefined)]);
    }

    #[test]
    fn a_long_elif_chain_of_distinct_defines_joins_to_undecided() {
        let mut src = String::from("#if A0\n#define X0\n");
        for i in 1..5000 {
            src.push_str(&format!("#elif A{i}\n#define X{i}\n"));
        }
        src.push_str("#endif\n#ifdef X4999\nmaybe();\n#endif\n#ifndef X0\nmaybe_too();\n#endif\n");
        let start = std::time::Instant::now();
        assert!(ranges(&src).is_empty());
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn the_else_of_if_1_is_not_reported_dead() {
        assert!(ranges("#if 1\na();\n#else\nb();\n#endif\n").is_empty());
    }

    #[test]
    fn a_tested_macro_nobody_writes_keeps_its_assumption() {
        // `#ifdef _MSC_VER ... #else ... #endif` writes nothing, so the
        // caller's assumption about _MSC_VER still decides a later test.
        let src = "#ifdef _MSC_VER\nx();\n#else\ny();\n#endif\n\
                   #ifdef _MSC_VER\nwin();\n#endif\n";
        let mut assumed = PlatformAssumptions::new();
        assumed.insert("_MSC_VER".to_string(), false);
        assert_eq!(
            ranges_with(src, &assumed),
            vec![
                (2, 2, DeadCodeReason::NeverDefined),
                (7, 7, DeadCodeReason::NeverDefined),
            ]
        );
    }

    #[test]
    fn nested_if_zero_inside_live_arm_is_dead() {
        let src = "#ifdef _WIN32\na();\n#if 0\nb();\n#endif\nc();\n#endif\n";
        assert_eq!(ranges(src), vec![(4, 4, DeadCodeReason::IfZero)]);
    }

    #[test]
    fn local_undecided_define_overrides_assumption() {
        // An assumption is a stand-in for missing evidence; once an undecided
        // arm writes the name, the file has said something and it is unknown.
        let src = "#ifdef CONFIG_X\n#define _MSC_VER 1900\n#endif\n\
                   #ifdef _MSC_VER\nmaybe();\n#endif\n";
        let mut assumed = PlatformAssumptions::new();
        assumed.insert("_MSC_VER".to_string(), false);
        assert!(ranges_with(src, &assumed).is_empty());
    }

    // Raylib rtext.c's actual shape: the whole file is
    // wrapped in a single top-level `#if SUPPORT_MODULE_RTEXT` module guard
    // this scanner can't prove either way, so the unconditional #define
    // inside it sits at preprocessor-nesting depth 1, not 0.
    #[test]
    fn always_defined_macro_under_unprovable_module_guard_else_is_dead() {
        let src = concat!(
            "#if SUPPORT_MODULE_RTEXT\n",                   // 1
            "#define SUPPORT_COMPRESSED_FONT_ATLAS\n",      // 2
            "void f(void) {\n",                             // 3
            "#if defined(SUPPORT_COMPRESSED_FONT_ATLAS)\n", // 4
            "    live_call();\n",                           // 5
            "#else\n",                                      // 6
            "    sprintf(buf, \"%d\", n);\n",               // 7
            "#endif\n",                                     // 8
            "}\n",                                          // 9
            "#endif\n",                                     // 10
        );
        assert_eq!(ranges(src), vec![(7, 7, DeadCodeReason::AlwaysDefined)]);
    }

    #[test]
    fn nested_if_zero_inside_dead_region_does_not_split_it() {
        let src = concat!(
            "#if 0\n",              // 1
            "a();\n",               // 2
            "#ifdef __cplusplus\n", // 3
            "b();\n",               // 4
            "#endif\n",             // 5
            "c();\n",               // 6
            "#endif\n",             // 7
            "d();\n",               // 8
        );
        assert_eq!(ranges(src), vec![(2, 6, DeadCodeReason::IfZero)]);
    }

    #[test]
    fn unterminated_dead_block_runs_to_eof() {
        let src = "a();\n#if 0\nb();\nc();\n";
        assert_eq!(ranges(src), vec![(3, 4, DeadCodeReason::IfZero)]);
    }

    #[test]
    fn empty_if_zero_block_yields_no_region() {
        let src = "#if 0\n#endif\n";
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn no_directives_yields_no_regions() {
        assert!(ranges("int x = 0;\nint y = 1;\n").is_empty());
    }

    #[test]
    fn empty_source_yields_no_regions() {
        assert!(ranges("").is_empty());
    }

    fn ranges_with(
        source: &str,
        assumed: &PlatformAssumptions,
    ) -> Vec<(usize, usize, DeadCodeReason)> {
        dead_code_ranges_with_assumptions(source, assumed)
            .into_iter()
            .map(|r| (r.start_line, r.end_line, r.reason))
            .collect()
    }

    // hostap's src/utils/common.h shape:
    // an #ifdef on a macro the file never itself defines is Neutral without
    // an assumption, but resolves once the caller supplies one.
    #[test]
    fn assumption_makes_never_mentioned_macro_dead() {
        let src = "#ifdef _MSC_VER\ntypedef UINT16 u16;\n#endif\n";
        assert!(ranges(src).is_empty());

        let mut assumed = PlatformAssumptions::new();
        assumed.insert("_MSC_VER".to_string(), false);
        assert_eq!(
            ranges_with(src, &assumed),
            vec![(2, 2, DeadCodeReason::NeverDefined)]
        );
    }

    #[test]
    fn assumption_true_keeps_ifdef_branch_live_and_else_dead() {
        let src = "#ifdef __linux__\nlive();\n#else\ndead();\n#endif\n";
        let mut assumed = PlatformAssumptions::new();
        assumed.insert("__linux__".to_string(), true);
        assert_eq!(
            ranges_with(src, &assumed),
            vec![(4, 4, DeadCodeReason::AlwaysDefined)]
        );
    }

    #[test]
    fn local_define_overrides_assumption() {
        // The file itself #defines _MSC_VER despite the caller assuming it
        // undefined — local evidence wins from that point on.
        let src = "#define _MSC_VER\n#ifdef _MSC_VER\nlive();\n#endif\n";
        let mut assumed = PlatformAssumptions::new();
        assumed.insert("_MSC_VER".to_string(), false);
        assert!(ranges_with(src, &assumed).is_empty());
    }

    #[test]
    fn posix_default_assumptions_resolves_hostap_style_platform_split() {
        let src = concat!(
            "#ifdef _MSC_VER\n",             // 1
            "typedef UINT16 u16;\n",         // 2
            "#endif\n",                      // 3
            "#ifdef __vxworks\n",            // 4
            "typedef UINT16 u16;\n",         // 5
            "#endif\n",                      // 6
            "#ifndef WPA_TYPES_DEFINED\n",   // 7
            "typedef unsigned short u16;\n", // 8
            "#endif\n",                      // 9
        );
        assert_eq!(
            ranges_with(src, &posix_default_assumptions()),
            vec![
                (2, 2, DeadCodeReason::NeverDefined),
                (5, 5, DeadCodeReason::NeverDefined),
            ]
        );
    }
}
