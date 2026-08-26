//! Dead-code region detection for Swift's `#if`/`#elseif`/`#else`/`#endif`
//! conditional compilation.
//!
//! Unlike [`crate::dead_code`] (C/C++), this is tree-sitter-based, not
//! line-based. tree-sitter-swift's grammar splices its `directive` node
//! (produced for each `#if`/`#elseif`/`#else`/`#endif` line) into every rule
//! that can contain one — `statements`, `class_body`, `protocol_body`,
//! `enum_class_body`, `source_file`, and others — as an ordinary sibling
//! alongside real content, rather than nesting the guarded content
//! underneath it. Swift's language spec also requires each `#if` branch to
//! contain syntactically complete code, so — unlike C, where a stray
//! `extern "C" {` brace desyncs tree-sitter's error recovery (see
//! [`crate::dead_code`]'s module docs for why that forces a line-based
//! scanner there) — a valid Swift file always parses cleanly regardless of
//! which branch is logically live, and sibling order is reliable. That
//! makes an AST sibling walk both viable and preferable to a text scanner
//! here.
//!
//! Scope is narrower than the C/C++ module's: Swift has no `#define`, so
//! there is no analog to that module's sub-problem 2 (locally-provable
//! named-macro definedness) at all. `os()`, `arch()`, `swift()`,
//! `compiler()`, `canImport()`, `targetEnvironment()`, and bare custom flags
//! (set only via external `-D` compiler flags, never in source) are all
//! determined by the build target, not the file, and are left unclassified
//! rather than guessed at. The one thing that *is* locally provable —
//! Swift's direct analog to C's `#if 0` — is a condition built purely from
//! boolean literals and boolean connectives: `#if false`, `#if true &&
//! false`, `#if !true`, and so on. A condition that mixes a literal with a
//! non-literal (e.g. `#if false && DEBUG`) is intentionally left
//! unclassified too, even though short-circuiting could in principle prove
//! it — that combination doesn't show up in practice, and evaluating it
//! soundly would mean modeling `&&`/`||` short-circuit semantics over
//! partially-unknown operands for no real-world benefit.

use tree_sitter::{Node, Parser};

/// A Swift preprocessor-dead region: a `#if`/`#elseif`/`#else` branch whose
/// condition is a compile-time-constant `false` (built only from
/// `true`/`false` boolean literals, `!`, `&&`, `||`, and parens).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwiftDeadCodeRegion {
    /// 1-based, inclusive first line of the dead region.
    pub start_line: usize,
    /// 1-based, inclusive last line of the dead region.
    pub end_line: usize,
}

/// Computes the dead-code regions of `source`, a Swift file: branches of a
/// `#if`/`#elseif`/`#else` chain that a compile-time-constant condition
/// proves unreachable. See the module documentation for exactly what is and
/// isn't recognized.
pub fn swift_dead_code_regions(source: &[u8]) -> Vec<SwiftDeadCodeRegion> {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_swift::LANGUAGE.into())
        .is_err()
    {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };

    let mut regions = Vec::new();
    scan_children(tree.root_node(), source, &mut regions);
    regions
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DirectiveKind {
    If,
    ElseIf,
    Else,
    EndIf,
}

fn directive_kind(node: Node) -> Option<DirectiveKind> {
    if node.kind() != "directive" {
        return None;
    }
    match node.child(0)?.kind() {
        "#if" => Some(DirectiveKind::If),
        "#elseif" => Some(DirectiveKind::ElseIf),
        "#else" => Some(DirectiveKind::Else),
        "#endif" => Some(DirectiveKind::EndIf),
        _ => None,
    }
}

/// Recurses through `parent`'s named children, matching up
/// `#if`/`#elseif`/`#else`/`#endif` chains among them (they appear as
/// ordinary siblings — see module docs) and recursing into every live
/// child, whether or not it's part of a chain.
fn scan_children(parent: Node, source: &[u8], out: &mut Vec<SwiftDeadCodeRegion>) {
    let mut cursor = parent.walk();
    let children: Vec<Node> = parent.named_children(&mut cursor).collect();
    scan_slice(&children, source, out);
}

/// Same as [`scan_children`], but over an arbitrary already-collected slice
/// of siblings (used for the body of a live branch, which is a sub-slice of
/// some ancestor's children — not a node's own `named_children()`).
fn scan_slice(children: &[Node], source: &[u8], out: &mut Vec<SwiftDeadCodeRegion>) {
    let mut i = 0;
    while i < children.len() {
        if directive_kind(children[i]) == Some(DirectiveKind::If) {
            i = scan_chain(children, i, source, out);
        } else {
            scan_children(children[i], source, out);
            i += 1;
        }
    }
}

/// Processes one `#if...#endif` chain starting at `children[start]` (an
/// `#if` directive). Returns the index just past the chain's `#endif`.
fn scan_chain(
    children: &[Node],
    start: usize,
    source: &[u8],
    out: &mut Vec<SwiftDeadCodeRegion>,
) -> usize {
    let mut i = start;
    // Whether an earlier branch in this chain was proven to always run —
    // if so, every later branch (elseif/else) is dead regardless of its
    // own condition.
    let mut chain_resolved_true = false;

    loop {
        let directive_node = children[i];
        let Some(kind) = directive_kind(directive_node) else {
            return i;
        };
        if kind == DirectiveKind::EndIf {
            return i + 1;
        }

        // A nested #if...#endif chain's directives are flat siblings too
        // (same reason as this chain's own), so the body boundary search
        // has to track nesting depth — otherwise a nested chain's own
        // #if/#endif would be mistaken for this branch's terminator.
        let body_start = i + 1;
        let mut body_end = body_start;
        let mut depth = 0usize;
        while body_end < children.len() {
            match directive_kind(children[body_end]) {
                Some(DirectiveKind::If) => depth += 1,
                Some(DirectiveKind::EndIf) => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                Some(DirectiveKind::ElseIf | DirectiveKind::Else) if depth == 0 => break,
                _ => {}
            }
            body_end += 1;
        }
        let body = &children[body_start..body_end];

        if chain_resolved_true {
            push_dead_range(body, out);
        } else {
            match kind {
                DirectiveKind::Else => scan_slice(body, source, out),
                DirectiveKind::If | DirectiveKind::ElseIf => {
                    let tokens = condition_tokens(directive_node, source);
                    match classify_constant_condition(&tokens) {
                        Some(true) => {
                            chain_resolved_true = true;
                            scan_slice(body, source, out);
                        }
                        Some(false) => push_dead_range(body, out),
                        None => scan_slice(body, source, out),
                    }
                }
                DirectiveKind::EndIf => unreachable!(),
            }
        }

        i = body_end;
    }
}

fn push_dead_range(body: &[Node], out: &mut Vec<SwiftDeadCodeRegion>) {
    let (Some(first), Some(last)) = (body.first(), body.last()) else {
        return;
    };
    out.push(SwiftDeadCodeRegion {
        start_line: first.start_position().row + 1,
        end_line: last.end_position().row + 1,
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tok {
    True,
    False,
    Not,
    And,
    Or,
    LParen,
    RParen,
    /// Anything else — `os(...)`, a bare custom flag, an integer literal,
    /// `.`, etc. Its presence anywhere in a condition makes the whole
    /// condition unclassifiable.
    Other,
}

/// The condition tokens of an `#if`/`#elseif` `directive` node — its
/// children after the leading `#if`/`#elseif` keyword token.
fn condition_tokens(directive: Node, source: &[u8]) -> Vec<Tok> {
    let mut cursor = directive.walk();
    directive
        .children(&mut cursor)
        .skip(1)
        .map(|n| classify_token(n, source))
        .collect()
}

fn classify_token(node: Node, source: &[u8]) -> Tok {
    match node.kind() {
        "boolean_literal" => match node.utf8_text(source) {
            Ok("true") => Tok::True,
            Ok("false") => Tok::False,
            _ => Tok::Other,
        },
        "!" => Tok::Not,
        "&&" => Tok::And,
        "||" => Tok::Or,
        "(" => Tok::LParen,
        ")" => Tok::RParen,
        _ => Tok::Other,
    }
}

/// Evaluates `tokens` as a boolean expression (`!` > `&&` > `||`, with
/// parens) if and only if every token is a literal or connective — `None`
/// as soon as an `Other` token appears anywhere, or on malformed syntax.
fn classify_constant_condition(tokens: &[Tok]) -> Option<bool> {
    let mut pos = 0;
    let value = eval_or(tokens, &mut pos)?;
    if pos == tokens.len() {
        Some(value)
    } else {
        None
    }
}

fn eval_or(tokens: &[Tok], pos: &mut usize) -> Option<bool> {
    let mut acc = eval_and(tokens, pos)?;
    while tokens.get(*pos) == Some(&Tok::Or) {
        *pos += 1;
        acc = eval_and(tokens, pos)? || acc;
    }
    Some(acc)
}

fn eval_and(tokens: &[Tok], pos: &mut usize) -> Option<bool> {
    let mut acc = eval_unary(tokens, pos)?;
    while tokens.get(*pos) == Some(&Tok::And) {
        *pos += 1;
        acc = eval_unary(tokens, pos)? && acc;
    }
    Some(acc)
}

fn eval_unary(tokens: &[Tok], pos: &mut usize) -> Option<bool> {
    if tokens.get(*pos) == Some(&Tok::Not) {
        *pos += 1;
        return Some(!eval_unary(tokens, pos)?);
    }
    eval_primary(tokens, pos)
}

fn eval_primary(tokens: &[Tok], pos: &mut usize) -> Option<bool> {
    match tokens.get(*pos)? {
        Tok::True => {
            *pos += 1;
            Some(true)
        }
        Tok::False => {
            *pos += 1;
            Some(false)
        }
        Tok::LParen => {
            *pos += 1;
            let value = eval_or(tokens, pos)?;
            if tokens.get(*pos) == Some(&Tok::RParen) {
                *pos += 1;
                Some(value)
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(source: &str) -> Vec<(usize, usize)> {
        swift_dead_code_regions(source.as_bytes())
            .into_iter()
            .map(|r| (r.start_line, r.end_line))
            .collect()
    }

    #[test]
    fn if_false_is_dead() {
        let src = "func f() {\n#if false\n    live()\n#endif\n}\n";
        assert_eq!(ranges(src), vec![(3, 3)]);
    }

    #[test]
    fn if_true_else_branch_dead() {
        let src = "func f() {\n#if true\n    live()\n#else\n    dead()\n#endif\n}\n";
        assert_eq!(ranges(src), vec![(5, 5)]);
    }

    #[test]
    fn boolean_combination_true_and_false_is_dead() {
        let src = "#if true && false\ndead()\n#endif\n";
        assert_eq!(ranges(src), vec![(2, 2)]);
    }

    #[test]
    fn negated_false_makes_branch_live() {
        let src = "#if !false\nlive()\n#endif\n";
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn elseif_true_kills_trailing_else() {
        let src = concat!(
            "#if false\n",    // 1
            "dead1()\n",      // 2
            "#elseif true\n", // 3
            "live()\n",       // 4
            "#else\n",        // 5
            "dead2()\n",      // 6
            "#endif\n",       // 7
        );
        assert_eq!(ranges(src), vec![(2, 2), (6, 6)]);
    }

    #[test]
    fn os_condition_is_not_locally_provable() {
        let src = "#if os(iOS)\nmaybe_dead()\n#endif\n";
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn custom_flag_is_not_locally_provable() {
        let src = "#if DEBUG\nmaybe_dead()\n#endif\n";
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn mixed_literal_and_flag_is_not_classified() {
        let src = "#if false && DEBUG\nmaybe_dead()\n#endif\n";
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn nested_dead_chain_inside_live_branch_is_still_found() {
        let src = concat!(
            "#if true\n",  // 1
            "a()\n",       // 2
            "#if false\n", // 3
            "b()\n",       // 4
            "#endif\n",    // 5
            "c()\n",       // 6
            "#endif\n",    // 7
        );
        assert_eq!(ranges(src), vec![(4, 4)]);
    }

    #[test]
    fn nested_dead_chain_inside_dead_branch_does_not_split_region() {
        let src = concat!(
            "#if false\n", // 1
            "a()\n",       // 2
            "#if true\n",  // 3
            "b()\n",       // 4
            "#endif\n",    // 5
            "c()\n",       // 6
            "#endif\n",    // 7
        );
        assert_eq!(ranges(src), vec![(2, 6)]);
    }

    #[test]
    fn dead_branch_inside_live_class_body_is_found() {
        let src = concat!(
            "class C {\n",     // 1
            "#if false\n",     // 2
            "    var x = 1\n", // 3
            "#endif\n",        // 4
            "}\n",             // 5
        );
        assert_eq!(ranges(src), vec![(3, 3)]);
    }

    #[test]
    fn empty_dead_branch_yields_no_region() {
        let src = "#if false\n#endif\n";
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn no_directives_yields_no_regions() {
        assert!(ranges("func f() -> Int { return 1 }\n").is_empty());
    }

    #[test]
    fn empty_source_yields_no_regions() {
        assert!(ranges("").is_empty());
    }
}
