//! Dead-code region detection for C#'s `#if`/`#elif`/`#else`/`#endif`
//! conditional compilation.
//!
//! Tree-sitter-based, like [`crate::dead_code_swift`] and unlike
//! [`crate::dead_code`]'s C/C++ line scanner — but structurally closer to
//! C/C++'s *problem shape* than to Swift's. Verified directly against
//! tree-sitter-c-sharp 0.23's grammar and a parse dump (not assumed):
//! `preproc_if`/`preproc_elif` are real nested nodes with a `condition`
//! field and an `alternative` field (chaining to the next `preproc_elif` or
//! a trailing `preproc_else`), and the guarded content is nested as
//! ordinary children *underneath* them — not spliced in as flat siblings
//! the way Swift's `directive` nodes are. That's possible because C#, like
//! Swift, requires each branch's content to be syntactically complete
//! (`repeat($.declaration)`-shaped in the grammar), so there's no
//! `extern "C" {`-style brace-imbalance failure mode to work around; a
//! proper AST walk following `condition`/`alternative` fields is
//! straightforward and doesn't need Swift's flat-sibling depth-tracking
//! trick either, since nesting is real here.
//!
//! Unlike Swift, C# *does* have `#define`/`#undef` — real top-level nodes
//! (confirmed: `preproc_define` appears as a direct child of
//! `compilation_unit`, sibling to `using_directive`/`class_declaration`),
//! so locally-provable symbol definedness ports over: a symbol
//! `#define`d on every path to a condition is always-defined there, and one
//! `#undef`d on every path is never-defined. The evidence follows the paths
//! through each conditional, as in [`crate::dead_code`]: after `#endif` a
//! symbol keeps a value only if every branch that may compile, and the path
//! that skips the conditional when it has no `#else`, agree on it. A branch
//! of `#if FOO` or `#if !FOO` also knows `FOO` from the test, so `#if !FOO` /
//! `#define FOO` / `#endif` leaves `FOO` defined on both paths. A dead
//! branch's `#define` counts on no path. A commented-out `#define` is no
//! evidence, as in [`crate::dead_code`]: the build can still define the
//! symbol (`-define:`, `DefineConstants`).
//!
//! C# also has no `#ifdef`/`#ifndef` — only `#if`/`#elif` with a boolean
//! condition expression (`identifier`, `true`/`false` literals, `!`, `&&`,
//! `||`, `==`, `!=`, and parens; confirmed against the grammar), so
//! `!SYMBOL` covers what C spells `#ifndef SYMBOL`. Because each operand of
//! `&&`/`||` is a real sub-tree (not a flat token stream like Swift's),
//! short-circuit evaluation falls out for free: `false && SYMBOL` is
//! provably dead even though `SYMBOL` alone isn't classifiable.

use std::collections::HashMap;

use tree_sitter::{Node, Parser};

/// A C# preprocessor-dead region: an `#if`/`#elif`/`#else` branch proven
/// unreachable by a compile-time-constant condition or locally-provable
/// symbol definedness. See the module documentation for exactly what is and
/// isn't recognized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CSharpDeadCodeRegion {
    /// 1-based, inclusive first line of the dead region.
    pub start_line: usize,
    /// 1-based, inclusive last line of the dead region.
    pub end_line: usize,
}

/// Computes the dead-code regions of `source`, a C# file. See the module
/// documentation for exactly what is and isn't recognized.
pub fn csharp_dead_code_regions(source: &[u8]) -> Vec<CSharpDeadCodeRegion> {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_c_sharp::LANGUAGE.into())
        .is_err()
    {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };

    let mut defined = Symbols::new();
    let mut regions = Vec::new();
    walk(tree.root_node(), source, &mut defined, &mut regions);
    regions
}

/// What the file itself proves about each symbol at one point: `true`
/// defined, `false` undefined. A symbol with no entry has no evidence.
type Symbols = HashMap<String, bool>;

/// Recurses through `node`'s named children in document order. A dead
/// branch is never recursed into at all, so a `#define`/`#undef` reached
/// here is on some path that may compile.
fn walk(node: Node, source: &[u8], defined: &mut Symbols, out: &mut Vec<CSharpDeadCodeRegion>) {
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    walk_siblings(&children, source, defined, out);
}

/// Same as [`walk`], but over an arbitrary already-collected slice of
/// siblings (used for the body of a live/unknown branch, which is a
/// sub-slice of some ancestor's children — not a node's own
/// `named_children()`). Dispatches each sibling by its own kind, since a
/// sibling might itself be a `preproc_if` chain that needs [`walk_chain`],
/// not a plain container to recurse into via [`walk`].
fn walk_siblings(
    children: &[Node],
    source: &[u8],
    defined: &mut Symbols,
    out: &mut Vec<CSharpDeadCodeRegion>,
) {
    for child in children {
        match child.kind() {
            "preproc_define" | "preproc_undef" => {
                if let Some(name) = preproc_arg_name(*child, source) {
                    defined.insert(name, child.kind() == "preproc_define");
                }
            }
            "preproc_if" => walk_chain(*child, source, defined, out),
            _ => walk(*child, source, defined, out),
        }
    }
}

/// Walks one `#if`/`#elif`/`#else` chain, following the `alternative` field
/// from `#if` through any `#elif`s to a trailing `#else`, and leaves in
/// `defined` what every path through the chain agrees on.
fn walk_chain(
    node: Node,
    source: &[u8],
    defined: &mut Symbols,
    out: &mut Vec<CSharpDeadCodeRegion>,
) {
    // The symbol a bare `#if FOO` / `#if !FOO` tests, and its value in the
    // first branch. Every later branch, and the path that skips the chain,
    // has the opposite.
    let tested = node
        .child_by_field_name("condition")
        .and_then(|cond| tested_symbol(cond, source));
    // Whether an earlier branch in this chain was proven to always run —
    // if so, every later branch (elif/else) is dead regardless of its own
    // condition.
    let mut chain_resolved_true = false;
    let mut has_else = false;
    // What each branch that may compile leaves behind at `#endif`.
    let mut paths: Vec<Symbols> = Vec::new();
    let branches = chain_branches(node);
    let mut first = true;

    for (condition, body) in branches {
        has_else |= condition.is_none();
        // Every condition is evaluated against the chain's entry state: an
        // earlier branch's writes are not on this branch's path.
        let live = !chain_resolved_true
            && match condition.map(|cond| eval_condition(cond, source, defined)) {
                Some(Some(false)) => false,
                Some(Some(true)) => {
                    chain_resolved_true = true;
                    true
                }
                Some(None) | None => true,
            };
        if live {
            let mut state = with_tested(defined, tested.as_ref(), first);
            walk_siblings(&body, source, &mut state, out);
            paths.push(state);
        } else {
            push_dead_range(&body, out);
        }
        first = false;
    }

    // A missing `#else` is an empty branch, taken whenever every condition
    // is false; no path skips a chain with a proven-true branch.
    if !has_else && !chain_resolved_true {
        paths.push(with_tested(defined, tested.as_ref(), false));
    }
    if let Some(joined) = paths.into_iter().reduce(|mut acc, path| {
        acc.retain(|name, value| path.get(name) == Some(value));
        acc
    }) {
        *defined = joined;
    }
}

/// `defined` as one branch of a chain sees it on entry: with the chain's
/// tested symbol (if any) set to its value in the first branch when `first`,
/// and the opposite otherwise.
fn with_tested(defined: &Symbols, tested: Option<&(String, bool)>, first: bool) -> Symbols {
    let mut state = defined.clone();
    if let Some((name, value_in_first)) = tested {
        state.insert(name.clone(), *value_in_first == first);
    }
    state
}

/// The symbol whose definedness alone decides a `#if FOO` or `#if !FOO`
/// condition (parentheses allowed), and whether the condition is true when
/// it is defined. `None` for any other condition.
fn tested_symbol(node: Node, source: &[u8]) -> Option<(String, bool)> {
    match node.kind() {
        "identifier" => Some((node.utf8_text(source).ok()?.to_string(), true)),
        "parenthesized_expression" => tested_symbol(node.named_child(0)?, source),
        "unary_expression" if node.child_by_field_name("operator")?.kind() == "!" => {
            let (name, when) = tested_symbol(node.child_by_field_name("argument")?, source)?;
            Some((name, !when))
        }
        _ => None,
    }
}

/// Each branch of the chain starting at `node`, in order: its condition
/// (`None` for `#else`) and its guarded content.
///
/// The content is assigned by line, not by tree parent. tree-sitter-c-sharp
/// parses directives as extras, so a branch's trailing `#define`/`#undef`
/// can hang off an outer node of the chain (an `#else` holding only
/// `#undef FOO` comes out empty, with the `#undef` a child of the `#if`
/// after its `alternative`). A branch owns everything from its own
/// directive's line to the next branch's.
fn chain_branches(node: Node) -> Vec<(Option<Node>, Vec<Node>)> {
    let mut heads = Vec::new();
    let mut fields = Vec::new();
    let mut current = Some(node);
    while let Some(n) = current {
        let condition = n.child_by_field_name("condition");
        current = n.child_by_field_name("alternative");
        fields.extend(condition.iter().chain(current.iter()).map(|f| f.id()));
        heads.push((n, condition));
    }
    let mut content: Vec<Node> = heads
        .iter()
        .flat_map(|(n, _)| {
            let mut cursor = n.walk();
            n.named_children(&mut cursor).collect::<Vec<_>>()
        })
        .filter(|c| !fields.contains(&c.id()))
        .collect();
    content.sort_by_key(|c| c.start_byte());
    let starts: Vec<usize> = heads.iter().map(|(n, _)| n.start_position().row).collect();
    heads
        .iter()
        .enumerate()
        .map(|(i, (_, condition))| {
            let next = starts.get(i + 1).copied().unwrap_or(usize::MAX);
            let body = content
                .iter()
                .filter(|c| (starts[i]..next).contains(&c.start_position().row))
                .copied()
                .collect();
            (*condition, body)
        })
        .collect()
}

fn push_dead_range(body: &[Node], out: &mut Vec<CSharpDeadCodeRegion>) {
    let (Some(first), Some(last)) = (body.first(), body.last()) else {
        return;
    };
    out.push(CSharpDeadCodeRegion {
        start_line: first.start_position().row + 1,
        end_line: last_line(*last),
    });
}

/// The 1-based last line of `node`'s own text. A directive node's range
/// takes in its line ending, so it ends at column 0 of the next line.
fn last_line(node: Node) -> usize {
    let end = node.end_position();
    if end.column == 0 && end.row > node.start_position().row {
        end.row
    } else {
        end.row + 1
    }
}

/// Evaluates a `#if`/`#elif` condition expression node to a constant
/// boolean, if locally provable — `None` as soon as any operand can't be
/// resolved (an unrecognized construct, or a symbol with no local
/// evidence), except where `&&`/`||` short-circuits past it.
fn eval_condition(node: Node, source: &[u8], defined: &Symbols) -> Option<bool> {
    match node.kind() {
        "boolean_literal" => match node.utf8_text(source) {
            Ok("true") => Some(true),
            Ok("false") => Some(false),
            _ => None,
        },
        "identifier" => {
            let name = node.utf8_text(source).ok()?;
            defined.get(name).copied()
        }
        "parenthesized_expression" => eval_condition(node.named_child(0)?, source, defined),
        "unary_expression" => {
            if node.child_by_field_name("operator")?.kind() != "!" {
                return None;
            }
            let arg = node.child_by_field_name("argument")?;
            Some(!eval_condition(arg, source, defined)?)
        }
        "binary_expression" => {
            let op = node.child_by_field_name("operator")?.kind();
            let left = node.child_by_field_name("left")?;
            let right = node.child_by_field_name("right")?;
            let l = eval_condition(left, source, defined);
            let r = eval_condition(right, source, defined);
            match op {
                "&&" => match (l, r) {
                    (Some(false), _) | (_, Some(false)) => Some(false),
                    (Some(a), Some(b)) => Some(a && b),
                    _ => None,
                },
                "||" => match (l, r) {
                    (Some(true), _) | (_, Some(true)) => Some(true),
                    (Some(a), Some(b)) => Some(a || b),
                    _ => None,
                },
                "==" => Some(l? == r?),
                "!=" => Some(l? != r?),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Extracts the leading identifier from a `preproc_define`/`preproc_undef`
/// node's `preproc_arg` child (which matches arbitrary trailing text, not
/// just an identifier, so this trims to the symbol name itself).
fn preproc_arg_name(node: Node, source: &[u8]) -> Option<String> {
    let arg = node.named_child(0)?;
    let text = arg.utf8_text(source).ok()?.trim_start();
    let end = text
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(text.len());
    if end == 0 {
        None
    } else {
        Some(text[..end].to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(source: &str) -> Vec<(usize, usize)> {
        csharp_dead_code_regions(source.as_bytes())
            .into_iter()
            .map(|r| (r.start_line, r.end_line))
            .collect()
    }

    #[test]
    fn if_false_is_dead() {
        let src = "class C {\n#if false\n    int x = 1;\n#endif\n}\n";
        assert_eq!(ranges(src), vec![(3, 3)]);
    }

    #[test]
    fn if_true_else_branch_dead() {
        let src = "#if true\nint x = 1;\n#else\nint y = 2;\n#endif\n";
        assert_eq!(ranges(src), vec![(4, 4)]);
    }

    #[test]
    fn negated_false_makes_branch_live() {
        let src = "#if !false\nint x = 1;\n#endif\n";
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn elif_true_kills_trailing_else() {
        let src = concat!(
            "#if false\n",  // 1
            "int a = 1;\n", // 2
            "#elif true\n", // 3
            "int b = 2;\n", // 4
            "#else\n",      // 5
            "int c = 3;\n", // 6
            "#endif\n",     // 7
        );
        assert_eq!(ranges(src), vec![(2, 2), (6, 6)]);
    }

    #[test]
    fn always_defined_symbol_else_branch_dead() {
        let src = concat!(
            "#define FOO\n", // 1
            "#if FOO\n",     // 2
            "int a = 1;\n",  // 3
            "#else\n",       // 4
            "int b = 2;\n",  // 5
            "#endif\n",      // 6
        );
        assert_eq!(ranges(src), vec![(5, 5)]);
    }

    #[test]
    fn commented_out_define_is_not_evidence() {
        // The build can still define FOO (`-define:FOO`, `DefineConstants`).
        for comment in ["//#define FOO\n", "// #define FOO\n", "/* #define FOO */\n"] {
            let src = format!("{comment}#if FOO\nint a = 1;\n#else\nint b = 2;\n#endif\n");
            assert!(ranges(&src).is_empty(), "{comment}");
        }
    }

    #[test]
    fn real_define_still_decides_next_to_a_commented_one() {
        let src = concat!(
            "// #define BAR\n", // 1
            "#define FOO\n",    // 2
            "#if FOO\n",        // 3
            "int a = 1;\n",     // 4
            "#else\n",          // 5
            "int b = 2;\n",     // 6
            "#endif\n",         // 7
            "#if !FOO\n",       // 8
            "int c = 3;\n",     // 9
            "#endif\n",         // 10
        );
        assert_eq!(ranges(src), vec![(6, 6), (9, 9)]);
    }

    #[test]
    fn undef_with_no_later_define_makes_if_dead() {
        let src = "#define FOO\n#undef FOO\n#if FOO\nint a = 1;\n#endif\n";
        assert_eq!(ranges(src), vec![(4, 4)]);
    }

    #[test]
    fn undef_inside_an_undecided_branch_leaves_the_symbol_undecided() {
        // With X defined, FOO is undefined at line 5, so the #else compiles.
        let src = concat!(
            "#define FOO\n", // 1
            "#if X\n",       // 2
            "#undef FOO\n",  // 3
            "#endif\n",      // 4
            "#if FOO\n",     // 5
            "int a = 1;\n",  // 6
            "#else\n",       // 7
            "int b = 2;\n",  // 8
            "#endif\n",      // 9
        );
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn undef_nested_two_branches_deep_leaves_the_symbol_undecided() {
        let src = concat!(
            "#define FOO\n", // 1
            "#if X\n",       // 2
            "#if Y\n",       // 3
            "#undef FOO\n",  // 4
            "#endif\n",      // 5
            "#endif\n",      // 6
            "#if !FOO\n",    // 7
            "int a = 1;\n",  // 8
            "#endif\n",      // 9
        );
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn a_define_on_every_branch_holds_after_endif() {
        let src = concat!(
            "#if X\n",       // 1
            "#define FOO\n", // 2
            "#elif Y\n",     // 3
            "#define FOO\n", // 4
            "#else\n",       // 5
            "#define FOO\n", // 6
            "#endif\n",      // 7
            "#if FOO\n",     // 8
            "int a = 1;\n",  // 9
            "#else\n",       // 10
            "int b = 2;\n",  // 11
            "#endif\n",      // 12
        );
        assert_eq!(ranges(src), vec![(11, 11)]);
    }

    #[test]
    fn a_define_on_some_branches_is_undecided_after_endif() {
        let src = concat!(
            "#if X\n",       // 1
            "#define FOO\n", // 2
            "#elif Y\n",     // 3
            "#define FOO\n", // 4
            "#endif\n",      // 5
            "#if FOO\n",     // 6
            "int a = 1;\n",  // 7
            "#else\n",       // 8
            "int b = 2;\n",  // 9
            "#endif\n",      // 10
        );
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn the_default_value_idiom_defines_the_symbol_on_both_paths() {
        for (test, write) in [("!FOO", "#define FOO"), ("(FOO)", "#undef BAR")] {
            let src = format!(
                "#if {test}\n{write}\n#endif\n#if FOO\nint a = 1;\n#else\nint b = 2;\n#endif\n"
            );
            let expected = if test == "!FOO" { vec![(7, 7)] } else { vec![] };
            assert_eq!(ranges(&src), expected, "{test}");
        }
    }

    #[test]
    fn an_undef_in_the_else_of_a_tested_symbol_leaves_it_undefined() {
        let src = concat!(
            "#if FOO\n",    // 1
            "#undef FOO\n", // 2
            "#else\n",      // 3
            "#endif\n",     // 4
            "#if FOO\n",    // 5
            "int a = 1;\n", // 6
            "#endif\n",     // 7
        );
        assert_eq!(ranges(src), vec![(6, 6)]);
    }

    #[test]
    fn a_define_in_a_proven_branch_holds_after_endif() {
        let src = concat!(
            "#if true\n",    // 1
            "#define FOO\n", // 2
            "#else\n",       // 3
            "#undef FOO\n",  // 4
            "#endif\n",      // 5
            "#if !FOO\n",    // 6
            "int a = 1;\n",  // 7
            "#endif\n",      // 8
        );
        assert_eq!(ranges(src), vec![(4, 4), (7, 7)]);
    }

    #[test]
    fn a_define_in_a_dead_branch_counts_on_no_path() {
        let src = concat!(
            "#if false\n",   // 1
            "#define FOO\n", // 2
            "#endif\n",      // 3
            "#if FOO\n",     // 4
            "int a = 1;\n",  // 5
            "#else\n",       // 6
            "int b = 2;\n",  // 7
            "#endif\n",      // 8
        );
        assert_eq!(ranges(src), vec![(2, 2)]);
    }

    #[test]
    fn a_later_branch_condition_ignores_an_earlier_branch_define() {
        let src = concat!(
            "#if X\n",       // 1
            "#define FOO\n", // 2
            "#elif FOO\n",   // 3
            "int a = 1;\n",  // 4
            "#endif\n",      // 5
        );
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn an_else_holding_only_a_directive_is_found_dead() {
        let src = concat!(
            "#if true\n",   // 1
            "int a = 1;\n", // 2
            "#else\n",      // 3
            "#undef FOO\n", // 4
            "#endif\n",     // 5
        );
        assert_eq!(ranges(src), vec![(4, 4)]);
    }

    #[test]
    fn a_dead_branch_ending_in_a_directive_stops_before_the_next_branch() {
        let src = concat!(
            "#if false\n",   // 1
            "int a = 1;\n",  // 2
            "#define FOO\n", // 3
            "#elif BAR\n",   // 4
            "#define BAZ\n", // 5
            "#endif\n",      // 6
            "#if !BAZ\n",    // 7
            "int b = 2;\n",  // 8
            "#endif\n",      // 9
        );
        assert_eq!(ranges(src), vec![(2, 3)]);
    }

    #[test]
    fn symbol_never_mentioned_is_neutral() {
        let src = "#if DEBUG\nint a = 1;\n#endif\n";
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn short_circuit_and_with_false_literal_is_dead() {
        let src = "#if false && DEBUG\nint a = 1;\n#endif\n";
        assert_eq!(ranges(src), vec![(2, 2)]);
    }

    #[test]
    fn short_circuit_or_with_true_literal_makes_else_dead() {
        let src = "#if true || DEBUG\nint a = 1;\n#else\nint b = 2;\n#endif\n";
        assert_eq!(ranges(src), vec![(4, 4)]);
    }

    #[test]
    fn define_inside_conditional_does_not_count_as_unconditional() {
        let src = "#if BAR\n#define FOO\n#endif\n#if FOO\nint a = 1;\n#endif\n";
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn nested_dead_chain_inside_dead_branch_does_not_split_region() {
        let src = concat!(
            "#if false\n",  // 1
            "int a = 1;\n", // 2
            "#if true\n",   // 3
            "int b = 2;\n", // 4
            "#endif\n",     // 5
            "int c = 3;\n", // 6
            "#endif\n",     // 7
        );
        assert_eq!(ranges(src), vec![(2, 6)]);
    }

    #[test]
    fn nested_dead_chain_inside_live_branch_is_still_found() {
        let src = concat!(
            "#if true\n",   // 1
            "int a = 1;\n", // 2
            "#if false\n",  // 3
            "int b = 2;\n", // 4
            "#endif\n",     // 5
            "int c = 3;\n", // 6
            "#endif\n",     // 7
        );
        assert_eq!(ranges(src), vec![(4, 4)]);
    }

    #[test]
    fn empty_dead_branch_yields_no_region() {
        let src = "#if false\n#endif\n";
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn no_directives_yields_no_regions() {
        assert!(ranges("class C {}\n").is_empty());
    }

    #[test]
    fn empty_source_yields_no_regions() {
        assert!(ranges("").is_empty());
    }
}
