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
pub fn find_descendants<'a>(root: Node<'a>, predicate: impl Fn(Node<'a>) -> bool) -> Vec<Node<'a>> {
    let mut out = Vec::new();
    collect_descendants(root, &predicate, &mut out);
    out
}

fn collect_descendants<'a>(
    node: Node<'a>,
    predicate: &impl Fn(Node<'a>) -> bool,
    out: &mut Vec<Node<'a>>,
) {
    if predicate(node) {
        out.push(node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_descendants(child, predicate, out);
    }
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
pub fn find_first_descendant<'a>(
    root: Node<'a>,
    predicate: impl Fn(Node<'a>) -> bool,
) -> Option<Node<'a>> {
    search_first(root, &predicate)
}

fn search_first<'a>(node: Node<'a>, predicate: &impl Fn(Node<'a>) -> bool) -> Option<Node<'a>> {
    if predicate(node) {
        return Some(node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = search_first(child, predicate) {
            return Some(found);
        }
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
