//! Generic AST pattern-matching primitives shared across the substrate's
//! consumers — a thin "find nodes matching a predicate" query layer over
//! tree-sitter, generalizing the ad hoc recursive search helpers duplicated
//! across tools_sqc's ~290 CERT-C rules (see its `utility/cert_c/ast_utils.rs`).
//!
//! v1 deliberately stays pattern-language-free: no rule registry, no
//! severity/violation vocabulary, no DSL. Those are tool-specific (CERT-C
//! IDs and severities for tools_sqc, metric thresholds for knots, style
//! knobs for moldy) and stay in each consumer. What's shared is just the
//! mechanical "find descendants/ancestors matching a predicate" traversal —
//! genuinely language-agnostic since it only touches node kinds and byte
//! ranges, with no per-language vocabulary table needed (contrast
//! [`crate::cfg`], which needs one because control-flow node kinds vary by
//! grammar; a "does this node's kind equal X" predicate does not).
//!
//! This module does not migrate tools_sqc's own rule engine — see the
//! substrate's task history (task 14's CFG generalization) for why that
//! migration, if ever done, belongs to its own follow-up task rather than
//! this one.

use tree_sitter::Node;

/// The source text spanned by `node`, decoded as UTF-8. Returns `""` for an
/// out-of-range or non-UTF-8 span rather than panicking — tree-sitter node
/// ranges are always valid for well-formed input, but callers shouldn't have
/// to thread a `Result` through every query for a case that should not occur.
pub fn node_text<'a>(node: Node, source: &'a [u8]) -> &'a str {
    source
        .get(node.start_byte()..node.end_byte())
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .unwrap_or("")
}

/// Depth-first search (root included) for every node matching `predicate`.
/// Descends into a matched node's children too, so nested matches (e.g. a
/// call expression inside a call expression's arguments) are all returned.
///
/// Iterative (explicit stack), not recursive: a real-world config file with
/// a multi-thousand-deep else-if chain overflowed the call stack under a
/// naive recursive walk of this same shape in a consumer (tools_sqc task
/// 153) — this module must not reintroduce that risk for any language/AST
/// shape with unbounded nesting depth.
pub fn find_descendants<'a>(root: Node<'a>, predicate: impl Fn(Node<'a>) -> bool) -> Vec<Node<'a>> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if predicate(node) {
            out.push(node);
        }
        push_children_reversed(node, &mut stack);
    }
    out
}

/// Pushes `node`'s children onto `stack` in reverse order, so popping the
/// stack (LIFO) visits them in original left-to-right order — preserving
/// the same pre-order sequence a recursive descent would produce.
fn push_children_reversed<'a>(node: Node<'a>, stack: &mut Vec<Node<'a>>) {
    let mut cursor = node.walk();
    let children: Vec<Node<'a>> = node.children(&mut cursor).collect();
    stack.extend(children.into_iter().rev());
}

/// Convenience wrapper over [`find_descendants`] for the common case of
/// matching by node kind alone.
pub fn find_descendants_of_kind<'a>(root: Node<'a>, kind: &str) -> Vec<Node<'a>> {
    find_descendants(root, |n| n.kind() == kind)
}

/// Like [`find_descendants_of_kind`] but matches any of several kinds.
pub fn find_descendants_of_kinds<'a>(root: Node<'a>, kinds: &[&str]) -> Vec<Node<'a>> {
    find_descendants(root, |n| kinds.contains(&n.kind()))
}

/// Depth-first, pre-order search (root included) for the first node matching
/// `predicate`, short-circuiting once found — cheaper than
/// [`find_descendants`] when only existence or the first match matters.
///
/// Iterative for the same reason as [`find_descendants`]: no call-stack
/// depth tied to AST nesting depth.
pub fn find_first_descendant<'a>(
    root: Node<'a>,
    predicate: impl Fn(Node<'a>) -> bool,
) -> Option<Node<'a>> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if predicate(node) {
            return Some(node);
        }
        push_children_reversed(node, &mut stack);
    }
    None
}

/// Walks `node`'s ancestor chain (parent, grandparent, ... — `node` itself is
/// not checked) for the nearest one matching `predicate`.
pub fn find_ancestor<'a>(node: Node<'a>, predicate: impl Fn(Node<'a>) -> bool) -> Option<Node<'a>> {
    let mut current = node.parent();
    while let Some(n) = current {
        if predicate(n) {
            return Some(n);
        }
        current = n.parent();
    }
    None
}

/// Convenience wrapper over [`find_ancestor`] for the common case of the
/// nearest enclosing node of a given kind (e.g. "nearest enclosing
/// function", "nearest enclosing loop").
pub fn nearest_ancestor_of_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    find_ancestor(node, |n| n.kind() == kind)
}

/// Like [`nearest_ancestor_of_kind`] but matches any of several kinds.
pub fn nearest_ancestor_of_kinds<'a>(node: Node<'a>, kinds: &[&str]) -> Option<Node<'a>> {
    find_ancestor(node, |n| kinds.contains(&n.kind()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str, language: tree_sitter::Language) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        parser.parse(source, None).unwrap()
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn node_text_returns_the_span() {
        let source = "int f(void) { return 1; }";
        let tree = parse(source, tree_sitter_c::LANGUAGE.into());
        let call =
            find_first_descendant(tree.root_node(), |n| n.kind() == "return_statement").unwrap();
        assert_eq!(node_text(call, source.as_bytes()), "return 1;");
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn find_descendants_of_kind_collects_every_match_including_nested() {
        let source = "int f(void) { return g(h(1)); }";
        let tree = parse(source, tree_sitter_c::LANGUAGE.into());
        let calls = find_descendants_of_kind(tree.root_node(), "call_expression");
        // Both g(...) and the nested h(1) are call_expressions.
        assert_eq!(calls.len(), 2);
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn find_descendants_of_kinds_matches_any_listed_kind() {
        let source = "int f(int x) { if (x) { return 1; } while (x) { break; } }";
        let tree = parse(source, tree_sitter_c::LANGUAGE.into());
        let branches =
            find_descendants_of_kinds(tree.root_node(), &["if_statement", "while_statement"]);
        assert_eq!(branches.len(), 2);
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn find_first_descendant_short_circuits_on_first_match() {
        let source = "int f(void) { return 1; }";
        let tree = parse(source, tree_sitter_c::LANGUAGE.into());
        let found = find_first_descendant(tree.root_node(), |n| n.kind() == "return_statement");
        assert!(found.is_some());
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn find_first_descendant_returns_none_when_absent() {
        let source = "int f(void) { int x = 1; }";
        let tree = parse(source, tree_sitter_c::LANGUAGE.into());
        assert!(
            find_first_descendant(tree.root_node(), |n| n.kind() == "return_statement").is_none()
        );
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn nearest_ancestor_of_kind_finds_enclosing_function_not_self() {
        let source = "int f(void) { if (1) { return 1; } }";
        let tree = parse(source, tree_sitter_c::LANGUAGE.into());
        let ret =
            find_first_descendant(tree.root_node(), |n| n.kind() == "return_statement").unwrap();
        let enclosing = nearest_ancestor_of_kind(ret, "function_definition");
        assert!(enclosing.is_some());
        assert_ne!(enclosing.unwrap().kind(), "return_statement");
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn nearest_ancestor_of_kind_returns_none_when_no_such_ancestor() {
        let source = "int f(void) { return 1; }";
        let tree = parse(source, tree_sitter_c::LANGUAGE.into());
        let ret =
            find_first_descendant(tree.root_node(), |n| n.kind() == "return_statement").unwrap();
        assert!(nearest_ancestor_of_kind(ret, "while_statement").is_none());
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn nearest_ancestor_of_kinds_finds_the_closest_enclosing_loop() {
        let source = "int f(void) { while (1) { for (;;) { break; } } }";
        let tree = parse(source, tree_sitter_c::LANGUAGE.into());
        let brk =
            find_first_descendant(tree.root_node(), |n| n.kind() == "break_statement").unwrap();
        let enclosing =
            nearest_ancestor_of_kinds(brk, &["while_statement", "for_statement"]).unwrap();
        // The nearest loop is the `for`, not the outer `while`.
        assert_eq!(enclosing.kind(), "for_statement");
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn find_descendants_handles_deeply_nested_input_without_overflowing_the_stack() {
        // Regression: a multi-thousand-deep else-if chain in a real config
        // file overflowed the call stack under a recursive walk of this
        // exact shape (tools_sqc task 153). 20k levels is well past any
        // depth a recursive implementation on a normal thread stack survives.
        let depth = 20_000;
        let mut source = String::new();
        source.push_str("int f(int x) {\n");
        for _ in 0..depth {
            source.push_str("if (x) {\n");
        }
        source.push_str("return 1;\n");
        for _ in 0..depth {
            source.push_str("}\n");
        }
        source.push('}');
        let tree = parse(&source, tree_sitter_c::LANGUAGE.into());
        let ifs = find_descendants_of_kind(tree.root_node(), "if_statement");
        assert_eq!(ifs.len(), depth);
        let first = find_first_descendant(tree.root_node(), |n| n.kind() == "if_statement");
        assert!(first.is_some());
    }

    #[test]
    #[cfg(feature = "lang-rust")]
    fn works_identically_across_languages_no_table_needed() {
        // Unlike cfg.rs, this module has no per-language vocabulary at all —
        // the same generic predicate-based search works for any grammar.
        let source = "fn f() { if true { return 1; } }";
        let tree = parse(source, tree_sitter_rust::LANGUAGE.into());
        let found = find_first_descendant(tree.root_node(), |n| n.kind() == "return_expression");
        assert!(found.is_some());
    }
}
