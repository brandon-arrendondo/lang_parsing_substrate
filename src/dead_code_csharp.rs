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
//! so [`crate::dead_code`]'s sub-problem 2 (locally-provable named-symbol
//! definedness) ports over directly: a symbol unconditionally `#define`d
//! earlier in the file with no later `#undef` is always-defined; a symbol
//! that's `#undef`d with no later `#define`, or never validly `#define`d
//! (only as a commented-out `#define`), is never-defined. As in
//! [`crate::dead_code`], "unconditional" means not nested inside *any*
//! `#if`/`#elif`/`#else` at all — not C-brace/scope nesting, which
//! `#define` doesn't respect anyway.
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

use crate::dead_code::commented_out_define_names;

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

    let commented_out = match std::str::from_utf8(source) {
        Ok(text) => commented_out_define_names(text),
        Err(_) => Default::default(),
    };
    let mut defined: HashMap<String, bool> = HashMap::new();
    let mut regions = Vec::new();
    walk(
        tree.root_node(),
        source,
        0,
        &mut defined,
        &commented_out,
        &mut regions,
    );
    regions
}

/// Recurses through `node`'s named children in document order. `depth` is
/// the number of enclosing `#if`/`#elif`/`#else` branches (of any kind —
/// dead, live, or unknown; a dead one is never recursed into at all, so
/// `depth` only ever grows here for branches we do walk).
fn walk(
    node: Node,
    source: &[u8],
    depth: usize,
    defined: &mut HashMap<String, bool>,
    commented_out: &std::collections::HashSet<String>,
    out: &mut Vec<CSharpDeadCodeRegion>,
) {
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    walk_siblings(&children, source, depth, defined, commented_out, out);
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
    depth: usize,
    defined: &mut HashMap<String, bool>,
    commented_out: &std::collections::HashSet<String>,
    out: &mut Vec<CSharpDeadCodeRegion>,
) {
    for child in children {
        match child.kind() {
            "preproc_define" if depth == 0 => {
                if let Some(name) = preproc_arg_name(*child, source) {
                    defined.insert(name, true);
                }
            }
            "preproc_undef" if depth == 0 => {
                if let Some(name) = preproc_arg_name(*child, source) {
                    defined.insert(name, false);
                }
            }
            "preproc_define" | "preproc_undef" => {}
            "preproc_if" => walk_chain(*child, source, depth, defined, commented_out, out),
            _ => walk(*child, source, depth, defined, commented_out, out),
        }
    }
}

/// Walks one `#if`/`#elif`/`#else` chain, following the `alternative` field
/// from `#if` through any `#elif`s to a trailing `#else`.
fn walk_chain(
    node: Node,
    source: &[u8],
    depth: usize,
    defined: &mut HashMap<String, bool>,
    commented_out: &std::collections::HashSet<String>,
    out: &mut Vec<CSharpDeadCodeRegion>,
) {
    // Whether an earlier branch in this chain was proven to always run —
    // if so, every later branch (elif/else) is dead regardless of its own
    // condition.
    let mut chain_resolved_true = false;
    let mut current = Some(node);

    while let Some(n) = current {
        let condition = n.child_by_field_name("condition");
        let alternative = n.child_by_field_name("alternative");
        let body = body_children(n, condition, alternative);

        if chain_resolved_true {
            push_dead_range(&body, out);
        } else {
            match condition {
                None => {
                    // #else has no condition of its own.
                    walk_siblings(&body, source, depth + 1, defined, commented_out, out);
                }
                Some(cond) => match eval_condition(cond, source, defined, commented_out) {
                    Some(true) => {
                        chain_resolved_true = true;
                        walk_siblings(&body, source, depth + 1, defined, commented_out, out);
                    }
                    Some(false) => push_dead_range(&body, out),
                    None => {
                        walk_siblings(&body, source, depth + 1, defined, commented_out, out);
                    }
                },
            }
        }

        current = alternative;
    }
}

/// `node`'s named children other than its `condition`/`alternative`
/// field values (i.e. its guarded content).
fn body_children<'a>(
    node: Node<'a>,
    condition: Option<Node>,
    alternative: Option<Node>,
) -> Vec<Node<'a>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|c| {
            Some(c.id()) != condition.map(|n| n.id()) && Some(c.id()) != alternative.map(|n| n.id())
        })
        .collect()
}

fn push_dead_range(body: &[Node], out: &mut Vec<CSharpDeadCodeRegion>) {
    let (Some(first), Some(last)) = (body.first(), body.last()) else {
        return;
    };
    out.push(CSharpDeadCodeRegion {
        start_line: first.start_position().row + 1,
        end_line: last.end_position().row + 1,
    });
}

/// Evaluates a `#if`/`#elif` condition expression node to a constant
/// boolean, if locally provable — `None` as soon as any operand can't be
/// resolved (an unrecognized construct, or a symbol with no local
/// evidence), except where `&&`/`||` short-circuits past it.
fn eval_condition(
    node: Node,
    source: &[u8],
    defined: &HashMap<String, bool>,
    commented_out: &std::collections::HashSet<String>,
) -> Option<bool> {
    match node.kind() {
        "boolean_literal" => match node.utf8_text(source) {
            Ok("true") => Some(true),
            Ok("false") => Some(false),
            _ => None,
        },
        "identifier" => {
            let name = node.utf8_text(source).ok()?;
            match defined.get(name) {
                Some(v) => Some(*v),
                None if commented_out.contains(name) => Some(false),
                None => None,
            }
        }
        "parenthesized_expression" => {
            eval_condition(node.named_child(0)?, source, defined, commented_out)
        }
        "unary_expression" => {
            if node.child_by_field_name("operator")?.kind() != "!" {
                return None;
            }
            let arg = node.child_by_field_name("argument")?;
            Some(!eval_condition(arg, source, defined, commented_out)?)
        }
        "binary_expression" => {
            let op = node.child_by_field_name("operator")?.kind();
            let left = node.child_by_field_name("left")?;
            let right = node.child_by_field_name("right")?;
            let l = eval_condition(left, source, defined, commented_out);
            let r = eval_condition(right, source, defined, commented_out);
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
    fn commented_out_define_makes_branch_dead() {
        let src = concat!(
            "//#define FOO\n", // 1
            "#if FOO\n",       // 2
            "int a = 1;\n",    // 3
            "#endif\n",        // 4
        );
        assert_eq!(ranges(src), vec![(3, 3)]);
    }

    #[test]
    fn undef_with_no_later_define_makes_if_dead() {
        let src = "#define FOO\n#undef FOO\n#if FOO\nint a = 1;\n#endif\n";
        assert_eq!(ranges(src), vec![(4, 4)]);
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
