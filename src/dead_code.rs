//! Preprocessor dead-code region detection for C/C++.
//!
//! Also compiled under `lang-csharp` alone (see this module's `cfg` in
//! `lib.rs`): [`crate::dead_code_csharp`] reuses this module's
//! comment-scanning helper for its own commented-out-`#define` detection
//! rather than duplicating it — the two languages share the same `//`/`/*
//! */` comment syntax, and the helper itself is pure text scanning with no
//! grammar dependency either way.
//!
//! Computes the 1-based inclusive line ranges of a C/C++ translation unit
//! that a preprocessor would strip before the compiler ever sees them. See
//! this repo's `DETECT_DEAD_CODE_REGIONS.md` for the full design handoff;
//! this module covers both sub-problems from that doc in one pass, sharing a
//! single nesting/depth state machine so they don't disagree at edge cases
//! (e.g. a `#if 0` nested inside a dead `#ifdef MACRO` region, or vice
//! versa):
//!
//! 1. `#if 0` and `__cplusplus`-gated branches — dead when the translation
//!    unit is compiled as C.
//! 2. `#ifdef MACRO` / `#if defined(MACRO)` where `MACRO`'s definedness is
//!    locally provable from this file: unconditionally `#define`d earlier
//!    with no later `#undef` (branch always live, its `#else` dead), or
//!    never validly `#define`d in scope at this point — either not
//!    mentioned at all except as a commented-out `#define`, or
//!    unconditionally `#undef`d with no later `#define` (branch always
//!    dead). A macro this file never mentions at all (e.g. a build-system
//!    flag like `_WIN32`) is left unclassified — there's no local evidence
//!    either way, and guessing would turn every such branch into a false
//!    positive.
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
//! sidesteps that entirely, the same way [`crate::suppressions`] and
//! [`crate::regions`] do.
//!
//! `#define`/`#undef` tracking is intentionally scoped to lines with no
//! enclosing `#if`/`#ifdef`/`#ifndef` at all (preprocessor-conditional
//! nesting depth zero) — not C brace/scope nesting, which `#define` doesn't
//! respect anyway. A function-local `#define` (legal in C, and exactly what
//! motivated sub-problem 2 — see the doc) still counts, since it isn't
//! wrapped in any conditional; a `#define` inside a live branch of some
//! *other* conditional does not, keeping the "unconditional" claim honest
//! without needing brace-awareness.

use std::collections::{HashMap, HashSet};

use crate::regions::marker_text;
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
    /// The macro is unconditionally `#define`d earlier in the file with no
    /// later `#undef`, so the `#ifdef`/`#if defined(MACRO)` branch is always
    /// live and its `#else` is dead.
    AlwaysDefined,
    /// The macro is never validly `#define`d in scope at this point — either
    /// not mentioned at all except as a commented-out `#define`, or
    /// unconditionally `#undef`d with no later `#define` — so the
    /// `#ifdef`/`#if defined(MACRO)` branch itself is dead.
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
}

/// Computes the dead-code regions of `source`, a C or C++ translation unit.
/// See the module documentation for exactly what is and isn't recognized.
pub fn dead_code_ranges(source: &str) -> Vec<DeadCodeRegion> {
    let commented_out = commented_out_define_names(source);
    let mut defined: HashMap<String, bool> = HashMap::new();
    let mut regions = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut dead_start: Option<usize> = None;

    let lines: Vec<&str> = source.lines().collect();
    let total_lines = lines.len();

    for (idx, line) in lines.iter().enumerate() {
        let line_no = idx + 1;
        let Some((directive, rest)) = parse_directive(line) else {
            continue;
        };

        match directive {
            "if" | "ifdef" | "ifndef" => {
                let kind = if dead_start.is_some() {
                    Neutral
                } else {
                    classify_conditional(directive, rest, &defined, &commented_out)
                };
                let self_caused = dead_start.is_none() && matches!(kind, ThenDead(_));
                if self_caused {
                    dead_start = Some(line_no + 1);
                }
                stack.push(Frame { kind, self_caused });
            }
            "elif" => {
                if let Some(frame) = stack.last_mut() {
                    if frame.self_caused {
                        let kind = frame.kind;
                        close_region(&mut regions, &mut dead_start, line_no - 1, kind);
                        frame.self_caused = false;
                    }
                }
            }
            "else" => {
                if let Some(frame) = stack.last_mut() {
                    if frame.self_caused {
                        let kind = frame.kind;
                        close_region(&mut regions, &mut dead_start, line_no - 1, kind);
                        frame.self_caused = false;
                    } else if dead_start.is_none() && matches!(frame.kind, ElseDead(_)) {
                        dead_start = Some(line_no + 1);
                        frame.self_caused = true;
                    }
                }
            }
            "endif" => {
                if let Some(frame) = stack.pop() {
                    if frame.self_caused {
                        close_region(&mut regions, &mut dead_start, line_no - 1, frame.kind);
                    }
                }
            }
            "define" if stack.is_empty() => {
                if let Some(name) = first_ident(rest) {
                    defined.insert(name.to_string(), true);
                }
            }
            "undef" if stack.is_empty() => {
                if let Some(name) = first_ident(rest) {
                    defined.insert(name.to_string(), false);
                }
            }
            _ => {}
        }
    }

    if let Some(start) = dead_start {
        let reason = stack
            .iter()
            .rev()
            .find(|f| f.self_caused)
            .and_then(|f| match f.kind {
                ThenDead(r) | ElseDead(r) => Some(r),
                Neutral => None,
            })
            .unwrap_or(DeadCodeReason::IfZero);
        push_region(&mut regions, start, total_lines, reason);
    }

    regions
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
fn classify_conditional(
    directive: &str,
    rest: &str,
    defined: &HashMap<String, bool>,
    commented_out: &HashSet<String>,
) -> BranchKind {
    match directive {
        "ifdef" => {
            first_ident(rest).map_or(Neutral, |n| classify_name(n, false, defined, commented_out))
        }
        "ifndef" => {
            first_ident(rest).map_or(Neutral, |n| classify_name(n, true, defined, commented_out))
        }
        "if" => {
            if is_zero_condition(rest) {
                ThenDead(DeadCodeReason::IfZero)
            } else {
                classify_if_condition(rest, defined, commented_out)
            }
        }
        _ => Neutral,
    }
}

/// Classifies a bare macro name from `#ifdef`/`#ifndef` (`is_ifndef` tells
/// which) or from `defined(NAME)`/`!defined(NAME)` inside `#if`.
fn classify_name(
    name: &str,
    is_ifndef: bool,
    defined: &HashMap<String, bool>,
    commented_out: &HashSet<String>,
) -> BranchKind {
    if name == "__cplusplus" {
        return if is_ifndef {
            ElseDead(DeadCodeReason::CppOnly)
        } else {
            ThenDead(DeadCodeReason::CppOnly)
        };
    }
    match defined.get(name) {
        Some(true) => {
            if is_ifndef {
                ThenDead(DeadCodeReason::AlwaysDefined)
            } else {
                ElseDead(DeadCodeReason::AlwaysDefined)
            }
        }
        Some(false) => {
            if is_ifndef {
                ElseDead(DeadCodeReason::NeverDefined)
            } else {
                ThenDead(DeadCodeReason::NeverDefined)
            }
        }
        None if commented_out.contains(name) => {
            if is_ifndef {
                ElseDead(DeadCodeReason::NeverDefined)
            } else {
                ThenDead(DeadCodeReason::NeverDefined)
            }
        }
        None => Neutral,
    }
}

/// Classifies an `#if` condition that isn't the literal `0` — looks for
/// `defined(NAME)` / `defined NAME`, optionally negated with a leading `!`,
/// optionally followed by `&& ...` (only the `ThenDead` side survives a
/// trailing `&&`, since a live-but-unproven first clause can't prove the
/// whole condition either way).
fn classify_if_condition(
    rest: &str,
    defined: &HashMap<String, bool>,
    commented_out: &HashSet<String>,
) -> BranchKind {
    let trimmed = strip_trailing_comment(rest).trim();

    if let Some(after_bang) = trimmed.strip_prefix('!') {
        return match parse_defined(after_bang.trim_start()) {
            Some((name, remainder)) if remainder.trim().is_empty() => {
                classify_name(name, true, defined, commented_out)
            }
            _ => Neutral,
        };
    }

    match parse_defined(trimmed) {
        Some((name, remainder)) => {
            let kind = classify_name(name, false, defined, commented_out);
            let remainder = remainder.trim_start();
            let bare = remainder.is_empty();
            let leading_and = remainder.starts_with("&&") && matches!(kind, ThenDead(_));
            if bare || leading_and {
                kind
            } else {
                Neutral
            }
        }
        None => Neutral,
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

fn is_zero_condition(rest: &str) -> bool {
    strip_trailing_comment(rest).trim() == "0"
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
/// that the macro was meant to be locally defined but currently isn't.
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

    // Raylib rtext.c-shaped case 1 (task 540): macro unconditionally
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

    // Raylib rtext.c-shaped case 2 (task 540): macro's #define is commented
    // out — the #if defined(...) branch itself is dead.
    #[test]
    fn commented_out_define_makes_branch_dead() {
        let src = concat!(
            "//#define SUPPORT_FONT_DATA_COPY\n",    // 1
            "void f(void) {\n",                      // 2
            "#if defined(SUPPORT_FONT_DATA_COPY)\n", // 3
            "    sprintf(buf, \"%d\", n);\n",        // 4
            "#endif\n",                              // 5
            "}\n",                                   // 6
        );
        assert_eq!(ranges(src), vec![(4, 4, DeadCodeReason::NeverDefined)]);
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
    fn define_inside_conditional_does_not_count_as_unconditional() {
        // FOO is only #define'd inside a (live) #ifndef BAR branch, so it's
        // never provably unconditional — no region should be reported.
        let src = "#ifndef BAR\n#define FOO\n#endif\n#ifdef FOO\nx();\n#endif\n";
        assert!(ranges(src).is_empty());
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
}
