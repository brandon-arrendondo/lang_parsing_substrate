//! Generic AST pattern-matching primitives shared across the substrate's
//! consumers — a thin "find nodes matching a predicate" query layer over
//! tree-sitter, generalizing the ad hoc recursive search helpers duplicated
//! across aurora-lint's ~290 CERT-C rules (see its `utility/cert_c/ast_utils.rs`).
//!
//! v1 deliberately stays pattern-language-free: no rule registry, no
//! severity/violation vocabulary, no DSL. Those are tool-specific (CERT-C
//! IDs and severities for aurora-lint, metric thresholds for knots, style
//! knobs for moldy) and stay in each consumer. What's shared is just the
//! mechanical "find descendants/ancestors matching a predicate" traversal —
//! genuinely language-agnostic since it only touches node kinds and byte
//! ranges, with no per-language vocabulary table needed (contrast
//! [`crate::cfg`], which needs one because control-flow node kinds vary by
//! grammar; a "does this node's kind equal X" predicate does not).
//!
//! This module does not migrate aurora-lint's own rule engine. That
//! migration, if ever done, belongs to its own change, as the CFG
//! generalization did.

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

/// What a [`walk_preorder`] visit tells the walk to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Walk {
    /// Visit this node's children, then carry on.
    Continue,
    /// Do not descend into this node; carry on with its next sibling.
    SkipChildren,
    /// End the walk now.
    Stop,
}

/// Pre-order walk of `root`'s subtree (root included) with ONE `TreeCursor`.
///
/// This is the substrate's single traversal primitive. It is iterative, so
/// nesting depth never becomes call-stack depth (a real-world config file with
/// a multi-thousand-deep else-if chain overflowed a recursive walk of this
/// shape in aurora-lint). It also allocates nothing per node. The
/// previous explicit-stack walks created a `TreeCursor` and a `Vec` for every
/// visited node, which aurora-lint measured at about 5% of CPU on sqlite's
/// `btree.c`.
pub fn walk_preorder<'a>(root: Node<'a>, mut visit: impl FnMut(Node<'a>) -> Walk) {
    let mut cursor = root.walk();
    // Tracked here, not read from `cursor.depth()`: that recounts the cursor's stack,
    // so calling it per step made a deeply nested tree quadratic again.
    let mut depth = 0usize;
    loop {
        match visit(cursor.node()) {
            Walk::Stop => return,
            Walk::Continue if cursor.goto_first_child() => {
                depth += 1;
                continue;
            }
            Walk::Continue | Walk::SkipChildren => {}
        }
        // Next sibling, or the nearest ancestor's next sibling, never leaving `root`.
        loop {
            if depth == 0 {
                return;
            }
            if cursor.goto_next_sibling() {
                break;
            }
            cursor.goto_parent();
            depth -= 1;
        }
    }
}

/// Depth-first search (root included) for every node matching `predicate`,
/// in pre-order. Descends into a matched node's children too, so nested
/// matches (a call expression inside a call expression's arguments) are all
/// returned. One cursor for the whole search: see [`walk_preorder`].
pub fn find_descendants<'a>(root: Node<'a>, predicate: impl Fn(Node<'a>) -> bool) -> Vec<Node<'a>> {
    let mut out = Vec::new();
    walk_preorder(root, |node| {
        if predicate(node) {
            out.push(node);
        }
        Walk::Continue
    });
    out
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
/// `predicate`, short-circuiting once found. Cheaper than [`find_descendants`]
/// when only existence or the first match matters.
pub fn find_first_descendant<'a>(
    root: Node<'a>,
    predicate: impl Fn(Node<'a>) -> bool,
) -> Option<Node<'a>> {
    let mut found = None;
    walk_preorder(root, |node| {
        if predicate(node) {
            found = Some(node);
            Walk::Stop
        } else {
            Walk::Continue
        }
    });
    found
}

/// `node`'s children in order, collected with one cursor.
///
/// Prefer this, or `node.children(&mut cursor)`, to an index loop:
/// `Node::child(i)` walks from the first child, so
/// `for i in 0..n.child_count() { n.child(i) }` is quadratic in the child count
/// (aurora-lint found about 1,090 such loops).
pub fn child_nodes(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

/// `node`'s named children in order, collected with one cursor. See
/// [`child_nodes`].
pub fn named_child_nodes(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// The ancestors of `node` within `root`, root first and `node`'s parent last
/// (`node` itself excluded), found with ONE descent from `root`.
///
/// tree-sitter does not store parent links: `Node::parent()` re-descends from
/// the tree root, so it is O(depth), and a climb of k steps is O(k · depth).
/// That made [`find_ancestor`] aurora-lint's top hotspot on long else-if chains.
/// Descending with `child_with_descendant` costs O(depth) for the
/// whole chain. Empty when `node` is `root` or does not lie inside it.
pub fn ancestors<'a>(root: Node<'a>, node: Node<'a>) -> Vec<Node<'a>> {
    let mut chain = Vec::new();
    let mut current = root;
    while current != node {
        chain.push(current);
        match current.child_with_descendant(node) {
            Some(next) => current = next,
            None => return Vec::new(),
        }
    }
    chain
}

/// The nearest ancestor of `node` (within `root`) matching `predicate`: the
/// root-down equivalent of [`find_ancestor`], at O(depth) instead of
/// O(depth²). Use it whenever the tree's root is to hand.
pub fn find_ancestor_from_root<'a>(
    root: Node<'a>,
    node: Node<'a>,
    predicate: impl Fn(Node<'a>) -> bool,
) -> Option<Node<'a>> {
    ancestors(root, node)
        .into_iter()
        .rev()
        .find(|n| predicate(*n))
}

/// Root-down form of [`nearest_ancestor_of_kind`]; see [`find_ancestor_from_root`].
pub fn nearest_ancestor_of_kind_from_root<'a>(
    root: Node<'a>,
    node: Node<'a>,
    kind: &str,
) -> Option<Node<'a>> {
    find_ancestor_from_root(root, node, |n| n.kind() == kind)
}

/// Root-down form of [`nearest_ancestor_of_kinds`]; see [`find_ancestor_from_root`].
pub fn nearest_ancestor_of_kinds_from_root<'a>(
    root: Node<'a>,
    node: Node<'a>,
    kinds: &[&str],
) -> Option<Node<'a>> {
    find_ancestor_from_root(root, node, |n| kinds.contains(&n.kind()))
}

/// Walks `node`'s ancestor chain (parent, grandparent, ... — `node` itself is
/// not checked) for the nearest one matching `predicate`.
///
/// Costs O(depth²): every `parent()` call re-descends from the tree root. Kept
/// for callers that hold only the node; prefer [`find_ancestor_from_root`].
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
/// function", "nearest enclosing loop"). O(depth²); see
/// [`nearest_ancestor_of_kind_from_root`].
pub fn nearest_ancestor_of_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    find_ancestor(node, |n| n.kind() == kind)
}

/// Like [`nearest_ancestor_of_kind`] but matches any of several kinds.
/// O(depth²); see [`nearest_ancestor_of_kinds_from_root`].
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
        // exact shape in aurora-lint. 20k levels is well past any
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

    fn recursive_preorder<'a>(node: Node<'a>, out: &mut Vec<Node<'a>>) {
        out.push(node);
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            recursive_preorder(child, out);
        }
    }

    const NESTED_C: &str =
        "int f(int x) { if (x) { g(h(x)); } else { while (x) { x--; } } return x; }\n\
                            struct S { int a; }; int k = 3;\n";

    #[test]
    #[cfg(feature = "lang-c")]
    fn walk_preorder_visits_exactly_the_recursive_preorder() {
        let tree = parse(NESTED_C, tree_sitter_c::LANGUAGE.into());
        let mut expected = Vec::new();
        recursive_preorder(tree.root_node(), &mut expected);
        let mut seen = Vec::new();
        walk_preorder(tree.root_node(), |n| {
            seen.push(n);
            Walk::Continue
        });
        assert_eq!(seen, expected);
        // A subtree walk stays inside its subtree.
        let body =
            find_first_descendant(tree.root_node(), |n| n.kind() == "compound_statement").unwrap();
        let mut inner = Vec::new();
        recursive_preorder(body, &mut inner);
        assert_eq!(find_descendants(body, |_| true), inner);
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn walk_preorder_skip_and_stop() {
        let tree = parse(NESTED_C, tree_sitter_c::LANGUAGE.into());
        let mut kinds = Vec::new();
        walk_preorder(tree.root_node(), |n| {
            kinds.push(n.kind());
            if n.kind() == "function_definition" {
                Walk::SkipChildren
            } else {
                Walk::Continue
            }
        });
        assert!(kinds.contains(&"function_definition"));
        assert!(
            !kinds.contains(&"if_statement"),
            "a skipped subtree is not visited"
        );
        assert!(
            kinds.contains(&"struct_specifier"),
            "the walk resumes at the next sibling"
        );

        let mut visited = 0;
        walk_preorder(tree.root_node(), |n| {
            visited += 1;
            if n.kind() == "if_statement" {
                Walk::Stop
            } else {
                Walk::Continue
            }
        });
        let mut all = Vec::new();
        recursive_preorder(tree.root_node(), &mut all);
        let at = all.iter().position(|n| n.kind() == "if_statement").unwrap();
        assert_eq!(visited, at + 1, "stop ends the walk at the matching node");
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn ancestors_match_the_parent_chain_and_from_root_matches_find_ancestor() {
        let tree = parse(NESTED_C, tree_sitter_c::LANGUAGE.into());
        let root = tree.root_node();
        for node in find_descendants(root, |_| true) {
            let mut climbed = Vec::new();
            let mut p = node.parent();
            while let Some(n) = p {
                climbed.push(n);
                p = n.parent();
            }
            climbed.reverse();
            assert_eq!(ancestors(root, node), climbed);
            let kinds = ["compound_statement", "function_definition"];
            assert_eq!(
                nearest_ancestor_of_kinds_from_root(root, node, &kinds),
                nearest_ancestor_of_kinds(node, &kinds)
            );
        }
        let other = parse("int z;", tree_sitter_c::LANGUAGE.into());
        assert!(
            ancestors(root, other.root_node()).is_empty(),
            "a node from another tree"
        );
        assert!(ancestors(root, root).is_empty());
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn child_nodes_match_indexing() {
        let tree = parse(NESTED_C, tree_sitter_c::LANGUAGE.into());
        for node in find_descendants(tree.root_node(), |_| true) {
            let indexed: Vec<Node> = (0..node.child_count())
                .filter_map(|i| node.child(i))
                .collect();
            assert_eq!(child_nodes(node), indexed);
            let named: Vec<Node> = (0..node.named_child_count())
                .filter_map(|i| node.named_child(i))
                .collect();
            assert_eq!(named_child_nodes(node), named);
        }
    }
}
