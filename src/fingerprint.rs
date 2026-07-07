//! AST subtree fingerprinting — the primitive behind cross-corpus clone
//! detection (Tier 5, `todo.db` task 17). Computes a structural hash per
//! function-like subtree, ignoring identifier and literal text so that a
//! function renamed or with different constants still hashes identically to
//! its original (Type-2 clone detection, in the PMD-CPD/BlackDuck sense).
//! Byte-for-byte (Type-1) matching falls out for free too, since identical
//! text obviously has identical structure.
//!
//! Like [`crate::calls`] and [`crate::imports`], the hashing itself only
//! sees one file — a tree-sitter `Node` doesn't carry any notion of "which
//! file, relative to the rest of the corpus." But *grouping* fingerprints by
//! hash across files needs no such context, only the hashes themselves, so
//! unlike those modules' cross-file resolution (which genuinely can't happen
//! without corpus-wide name context this crate doesn't have), that part
//! lives here too: see [`duplicate_groups`]. The `todo.db` entry for this
//! task notes it operates at corpus level in recursive mode only, and is
//! heavier than the other tiers — likely a separate opt-in pass in
//! consumers rather than part of their default per-file walk, but the
//! grouping step itself doesn't need to be reimplemented per consumer.
//!
//! The hash folds in each node's `kind()` *and* `child_count()` in a
//! deterministic pre-order walk, not just a flat multiset of kinds — two
//! subtrees with the same kinds in a different shape (e.g. `a` nested three
//! deep vs. three `a` siblings) must not collide. `std::hash::DefaultHasher`
//! is used deliberately over `RandomState`-seeded hashing: fingerprints are
//! meant to be persisted (e.g. tools_sqc's SQLite store) and compared across
//! separate process runs, so the hash must be stable, not per-process-random.
//!
//! One deliberate exception to "ignore identifier text": the hashed node's
//! own declared/return type, if its grammar exposes one under a recognized
//! field name (see [`declared_type_text`]). Two functions with the identical
//! "new accumulator, delegate, return" skeleton but different declared
//! return types (`-> Vec<String>` vs. `-> Vec<RuleViolation>`) previously
//! hashed identically, since `type_identifier` is the same *kind* regardless
//! of which name it holds — a real false-positive surfaced by an actual
//! clone-detection pass on a ~11k-function corpus (`todo.db` task 62).
//! Parameter/variable *names* are still ignored, preserving Type-2
//! (renamed-identifier) clone matching; only the type annotation's text is
//! folded in, and only for the top-level node being hashed, not every
//! descendant.

use crate::calls::{get_function_name, is_function_kind};
use crate::query::find_descendants;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use tree_sitter::Node;

/// One function-like subtree's structural fingerprint.
///
/// `kind` and byte/line ranges locate the subtree for reporting; `hash` is
/// the value to group on for duplicate detection; `node_count` is the
/// subtree's size in AST nodes, useful for filtering trivial matches (e.g. a
/// corpus-wide caller typically drops single-digit-`node_count` fingerprints
/// since a hash collision between two one-line getters isn't a meaningful
/// clone) and for ranking matches by how much code they actually cover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    pub name: Option<String>,
    pub kind: &'static str,
    pub hash: u64,
    pub node_count: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub end_line: usize,
}

/// A [`Fingerprint`] tagged with whatever the caller uses to identify its
/// source file (a path, a DB row id, ...). `S` is left generic rather than
/// fixed to e.g. `PathBuf` since callers already have their own preferred
/// file-identifier type (tools_sqc's SQLite store keys by path+mtime;
/// knots/moldy likely just use a path) and forcing a conversion at this
/// boundary would be pure overhead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusFingerprint<S> {
    pub source: S,
    pub fingerprint: Fingerprint,
}

/// Groups `fingerprints` by `hash`, keeping only groups with two or more
/// members — a lone fingerprint isn't a duplicate of anything. This is the
/// "similarity detection" half of clone detection: [`function_fingerprints`]
/// tells you what one file's subtrees hash to, this tells you which of
/// those hashes recur elsewhere in the corpus.
///
/// Only exact hash equality is grouped — there is no near-miss/fuzzy
/// similarity threshold here (e.g. two functions differing by one extra
/// statement do not group). That's a real limitation for Type-3 clones, but
/// matches this crate's other primitives in staying to the mechanical,
/// unambiguous case and leaving fuzzier heuristics as a consumer concern.
///
/// Group and within-group order is deterministic (sorted by hash, then by
/// source-file position) rather than following `HashMap` iteration order,
/// since callers may snapshot-test or otherwise rely on stable output.
pub fn duplicate_groups<S: Ord + Clone>(
    fingerprints: &[CorpusFingerprint<S>],
) -> Vec<Vec<&CorpusFingerprint<S>>> {
    let mut by_hash: HashMap<u64, Vec<&CorpusFingerprint<S>>> = HashMap::new();
    for fp in fingerprints {
        by_hash.entry(fp.fingerprint.hash).or_default().push(fp);
    }

    let mut groups: Vec<Vec<&CorpusFingerprint<S>>> = by_hash
        .into_iter()
        .filter(|(_, members)| members.len() > 1)
        .map(|(_, mut members)| {
            members.sort_by(|a, b| {
                (a.source.clone(), a.fingerprint.start_byte)
                    .cmp(&(b.source.clone(), b.fingerprint.start_byte))
            });
            members
        })
        .collect();
    groups.sort_by_key(|members| members[0].fingerprint.hash);
    groups
}

/// Structural hash of `node`'s subtree — the same primitive
/// [`function_fingerprints`] uses internally, exposed directly for callers
/// that want to fingerprint an arbitrary subtree rather than every function
/// in a file (e.g. hashing a single already-located node). `source` is the
/// full file text `node` was parsed from, needed to read `node`'s declared
/// type text (see the module doc comment).
pub fn structural_hash(node: Node, source: &[u8]) -> u64 {
    hash_and_count(node, source).0
}

/// Fingerprints every function-like subtree in `tree` (per
/// [`is_function_kind`]), skipping any whose subtree has fewer than
/// `min_nodes` AST nodes. Nested functions (a closure defined inside another
/// function) are fingerprinted independently, both as part of their
/// enclosing function's subtree and again on their own — matching
/// [`crate::calls::call_edges`]'s existing behavior of not stopping the walk
/// at function boundaries.
pub fn function_fingerprints(root: Node, source: &str, min_nodes: usize) -> Vec<Fingerprint> {
    find_descendants(root, |n| is_function_kind(n.kind()))
        .into_iter()
        .filter_map(|node| {
            let (hash, node_count) = hash_and_count(node, source.as_bytes());
            if node_count < min_nodes {
                return None;
            }
            Some(Fingerprint {
                name: get_function_name(node, source),
                kind: node.kind(),
                hash,
                node_count,
                start_byte: node.start_byte(),
                end_byte: node.end_byte(),
                start_line: node.start_position().row + 1,
                end_line: node.end_position().row + 1,
            })
        })
        .collect()
}

/// Iterative pre-order walk (explicit stack, matching [`crate::query`]'s
/// depth-safety rationale — a real-world deeply-nested file must not
/// overflow the call stack here any more than it does in `find_descendants`)
/// that folds each node's `kind()` and `child_count()` into a single hash,
/// returning it alongside the subtree's total node count. Also folds in
/// `root`'s own declared type text, if any (see [`declared_type_text`]).
fn hash_and_count(root: Node, source: &[u8]) -> (u64, usize) {
    let mut hasher = DefaultHasher::new();
    let mut count = 0usize;
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        count += 1;
        node.kind().hash(&mut hasher);
        node.child_count().hash(&mut hasher);
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    if let Some(type_text) = declared_type_text(root, source) {
        type_text.hash(&mut hasher);
    }
    (hasher.finish(), count)
}

/// Best-effort source text of `node`'s own declared/return type, tried
/// across the handful of field names different grammars use for it: Rust's
/// `function_item` names it `return_type`, Go's `function_declaration`
/// names it `result`, and C/Java/several others just call it `type` (which
/// is safe to read here since we only ever query it on `node` itself, never
/// descend into unrelated fields like a parameter's own `type`). Returns
/// `None` for languages/nodes with no such field (e.g. Python without a
/// `-> T` annotation) — those get no additional disambiguation, same as
/// before this fold was added.
fn declared_type_text<'a>(node: Node, source: &'a [u8]) -> Option<&'a str> {
    ["return_type", "result", "type"].iter().find_map(|field| {
        let type_node = node.child_by_field_name(field)?;
        std::str::from_utf8(&source[type_node.start_byte()..type_node.end_byte()]).ok()
    })
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
    fn identical_functions_hash_identically() {
        let a = "int f(int x) { return x + 1; }";
        let b = "int g(int y) { return y + 1; }";
        let tree_a = parse(a, tree_sitter_c::LANGUAGE.into());
        let tree_b = parse(b, tree_sitter_c::LANGUAGE.into());
        let fp_a = function_fingerprints(tree_a.root_node(), a, 0);
        let fp_b = function_fingerprints(tree_b.root_node(), b, 0);
        assert_eq!(fp_a.len(), 1);
        assert_eq!(fp_b.len(), 1);
        // Renamed function/parameter, but structurally identical body.
        assert_eq!(fp_a[0].hash, fp_b[0].hash);
        assert_eq!(fp_a[0].name, Some("f".to_string()));
        assert_eq!(fp_b[0].name, Some("g".to_string()));
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn structurally_different_functions_hash_differently() {
        let a = "int f(int x) { return x + 1; }";
        let b = "int f(int x) { if (x) { return x; } return 0; }";
        let tree_a = parse(a, tree_sitter_c::LANGUAGE.into());
        let tree_b = parse(b, tree_sitter_c::LANGUAGE.into());
        let fp_a = function_fingerprints(tree_a.root_node(), a, 0);
        let fp_b = function_fingerprints(tree_b.root_node(), b, 0);
        assert_ne!(fp_a[0].hash, fp_b[0].hash);
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn min_nodes_filters_out_trivial_subtrees() {
        let source = "int f(void) { return 1; }";
        let tree = parse(source, tree_sitter_c::LANGUAGE.into());
        let all = function_fingerprints(tree.root_node(), source, 0);
        assert_eq!(all.len(), 1);
        let filtered = function_fingerprints(tree.root_node(), source, 1_000);
        assert!(filtered.is_empty());
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn hash_is_stable_across_separate_calls() {
        // Fingerprints are meant to be persisted and compared across process
        // runs, so the hash must not vary run-to-run (ruling out a
        // RandomState-seeded hasher).
        let source = "int f(int x) { return x + 1; }";
        let tree1 = parse(source, tree_sitter_c::LANGUAGE.into());
        let tree2 = parse(source, tree_sitter_c::LANGUAGE.into());
        let h1 = function_fingerprints(tree1.root_node(), source, 0)[0].hash;
        let h2 = function_fingerprints(tree2.root_node(), source, 0)[0].hash;
        assert_eq!(h1, h2);
    }

    #[test]
    #[cfg(feature = "lang-rust")]
    fn nested_functions_are_fingerprinted_independently_of_their_enclosing_fn() {
        let source = "fn f() { fn g() { 1 + 1; } g(); }";
        let tree = parse(source, tree_sitter_rust::LANGUAGE.into());
        let fps = function_fingerprints(tree.root_node(), source, 0);
        // The outer `fn f` and the inner `fn g` both count — is_function_kind
        // doesn't special-case closures (`|| {}` is `closure_expression`,
        // not one of its matched kinds), only named function items.
        assert_eq!(fps.len(), 2);
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn structural_hash_matches_function_fingerprints_hash() {
        let source = "int f(int x) { return x + 1; }";
        let tree = parse(source, tree_sitter_c::LANGUAGE.into());
        let fn_node = find_descendants(tree.root_node(), |n| is_function_kind(n.kind()))
            .into_iter()
            .next()
            .unwrap();
        let direct = structural_hash(fn_node, source.as_bytes());
        let via_fingerprints = function_fingerprints(tree.root_node(), source, 0)[0].hash;
        assert_eq!(direct, via_fingerprints);
    }

    #[test]
    #[cfg(feature = "lang-rust")]
    fn same_skeleton_different_return_type_hashes_differently() {
        // The exact false positive from todo.db task 62: a "new accumulator,
        // delegate, return" skeleton that's structurally the same AST shape
        // whether it collects names or collects violations — only the
        // declared return type's *text* distinguishes them, since
        // `type_identifier` is the same node kind regardless of which name
        // it holds.
        let names = "fn collect_names(&self, node: &Node) -> Vec<String> { \
            let mut names = Vec::new(); self.collect_names_recursive(node, &mut names); names }";
        let violations = "fn check(&self, node: &Node) -> Vec<RuleViolation> { \
            let mut violations = Vec::new(); self.check_x(node, &mut violations); violations }";
        let tree_a = parse(names, tree_sitter_rust::LANGUAGE.into());
        let tree_b = parse(violations, tree_sitter_rust::LANGUAGE.into());
        let fp_a = function_fingerprints(tree_a.root_node(), names, 0);
        let fp_b = function_fingerprints(tree_b.root_node(), violations, 0);
        assert_ne!(fp_a[0].hash, fp_b[0].hash);
    }

    #[test]
    #[cfg(feature = "lang-rust")]
    fn same_skeleton_same_return_type_still_hashes_identically() {
        // Renamed identifiers/params still match when the declared return
        // type text is the same — the fold must not break Type-2 clone
        // detection for the common case.
        let a = "fn collect_names(&self, node: &Node) -> Vec<String> { \
            let mut out = Vec::new(); self.walk(node, &mut out); out }";
        let b = "fn gather_ids(&self, root: &Node) -> Vec<String> { \
            let mut acc = Vec::new(); self.walk(root, &mut acc); acc }";
        let tree_a = parse(a, tree_sitter_rust::LANGUAGE.into());
        let tree_b = parse(b, tree_sitter_rust::LANGUAGE.into());
        let fp_a = function_fingerprints(tree_a.root_node(), a, 0);
        let fp_b = function_fingerprints(tree_b.root_node(), b, 0);
        assert_eq!(fp_a[0].hash, fp_b[0].hash);
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn function_with_no_type_annotation_still_hashes() {
        // Sanity check that the best-effort field lookup doesn't panic or
        // change behavior for grammars/nodes with no matching field —
        // `duplicate_groups_finds_clones_across_files` below already
        // exercises the same-return-type case; this exercises a node kind
        // (`if_statement`) with none of the three candidate fields at all.
        let source = "int f(int x) { if (x) { return x; } return 0; }";
        let tree = parse(source, tree_sitter_c::LANGUAGE.into());
        let if_node = find_descendants(tree.root_node(), |n| n.kind() == "if_statement")
            .into_iter()
            .next()
            .unwrap();
        let _ = structural_hash(if_node, source.as_bytes());
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn duplicate_groups_finds_clones_across_files() {
        let a_src = "int f(int x) { return x + 1; }";
        let b_src = "int g(int y) { return y + 1; }";
        let c_src = "int h(int z) { if (z) { return z; } return 0; }";
        let a_tree = parse(a_src, tree_sitter_c::LANGUAGE.into());
        let b_tree = parse(b_src, tree_sitter_c::LANGUAGE.into());
        let c_tree = parse(c_src, tree_sitter_c::LANGUAGE.into());

        let mut all = Vec::new();
        for (source_id, tree, src) in [
            ("a.c", &a_tree, a_src),
            ("b.c", &b_tree, b_src),
            ("c.c", &c_tree, c_src),
        ] {
            for fingerprint in function_fingerprints(tree.root_node(), src, 0) {
                all.push(CorpusFingerprint {
                    source: source_id,
                    fingerprint,
                });
            }
        }

        let groups = duplicate_groups(&all);
        // a.c and b.c are structural clones; c.c's differently-shaped body
        // has no match, so it forms no group at all.
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 2);
        let sources: Vec<&str> = groups[0].iter().map(|m| m.source).collect();
        assert_eq!(sources, vec!["a.c", "b.c"]);
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn duplicate_groups_excludes_unique_fingerprints() {
        let source = "int f(int x) { return x + 1; }";
        let tree = parse(source, tree_sitter_c::LANGUAGE.into());
        let all: Vec<CorpusFingerprint<&str>> = function_fingerprints(tree.root_node(), source, 0)
            .into_iter()
            .map(|fingerprint| CorpusFingerprint {
                source: "a.c",
                fingerprint,
            })
            .collect();
        assert!(duplicate_groups(&all).is_empty());
    }
}
