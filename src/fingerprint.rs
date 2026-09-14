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
//!
//! ## Reporting granularity (line vs. function)
//!
//! A [`Fingerprint`] already carries `start_line`/`end_line` alongside its
//! `name`, so a consumer can report a match either as "function X duplicates
//! function Y" (the name) or as "path:40-58 duplicates path:12-30" (the line
//! range) purely from data already on this type — no substrate change is
//! needed for that choice; it's a per-consumer reporting decision (see
//! `DETECT_FINE_GRAINED_DUPLICATES.md`, Ask 1).
//!
//! ## Granularity tiers ([`FingerprintTier`])
//!
//! Every [`Fingerprint`] is tagged with the granularity its walk produced it
//! at: [`FingerprintTier::Function`] (whole function-like subtrees, via
//! [`function_fingerprints`]) or [`FingerprintTier::Block`] (loop/conditional/
//! switch-like subtrees *inside* a function, via [`block_fingerprints`]).
//! `Block` exists for a narrower use case than corpus-wide clone detection:
//! a caller that already has one flagged region (e.g. a tools_sqc violation)
//! and wants to search the corpus for other structurally similar regions,
//! not just whole-function duplicates (`DETECT_FINE_GRAINED_DUPLICATES.md`,
//! Ask 2). No new search primitive is needed for that: fingerprint the
//! flagged node directly with [`structural_hash`] (already possible on any
//! node), then look it up against a corpus's `Block`-tier fingerprints the
//! same way [`duplicate_groups`] already groups by hash — either run the
//! flagged hash through `duplicate_groups` alongside the corpus, or filter
//! the corpus directly: `corpus.iter().filter(|fp| fp.fingerprint.tier ==
//! FingerprintTier::Block && fp.fingerprint.hash == flagged_hash)`. A
//! dedicated `find_similar` convenience wasn't added — that one-liner is
//! documented as sufficient until a real caller's usage shows otherwise.
//!
//! `Block` tier is **syntactic-block**, not `crate::cfg`'s CFG basic blocks:
//! it walks whole AST subtrees rooted at loop/conditional/switch-like nodes
//! (see [`is_block_kind`]), unsplit by internal branches, rather than
//! `cfg.rs`'s split-at-every-branch basic blocks. That trades CFG-level
//! precision for coverage across all 16 languages `is_function_kind`-style
//! walks already support, instead of only the three `cfg.rs` models
//! (`c`/`cpp`/`rust`) — the targeted "does this flagged region recur
//! elsewhere" search doesn't need CFG-level precision to be useful.
//!
//! Mixing tiers in one `Vec<CorpusFingerprint<S>>` is intentionally safe:
//! [`duplicate_groups`] groups by `(hash, tier)`, not `hash` alone, so a
//! `Block`-tier fingerprint can never coincidentally group with an unrelated
//! `Function`-tier one just because they hash equal (e.g. a small flagged
//! loop matching some other file's whole one-line function) — see that
//! function's doc comment.

use crate::calls::{get_function_name, is_function_kind};
use crate::query::find_descendants;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use tree_sitter::Node;

/// The granularity a [`Fingerprint`] was produced at — see the module doc's
/// "Granularity tiers" section for the design rationale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FingerprintTier {
    /// A whole function-like subtree, from [`function_fingerprints`].
    Function,
    /// A loop/conditional/switch-like subtree inside a function, from
    /// [`block_fingerprints`].
    Block,
}

/// One function-like or block-like subtree's structural fingerprint.
///
/// `kind` and byte/line ranges locate the subtree for reporting; `hash` is
/// the value to group on for duplicate detection; `node_count` is the
/// subtree's size in AST nodes, useful for filtering trivial matches (e.g. a
/// corpus-wide caller typically drops single-digit-`node_count` fingerprints
/// since a hash collision between two one-line getters isn't a meaningful
/// clone) and for ranking matches by how much code they actually cover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    /// The function's name, or `None` for an anonymous closure/lambda and
    /// always `None` at [`FingerprintTier::Block`] (blocks aren't named).
    pub name: Option<String>,
    /// Which walk produced this fingerprint — see [`FingerprintTier`].
    pub tier: FingerprintTier,
    /// Tree-sitter node kind the subtree was rooted at.
    pub kind: &'static str,
    /// Structural hash to group on for duplicate detection.
    pub hash: u64,
    /// Number of AST nodes in the subtree.
    pub node_count: usize,
    /// Start byte offset of the subtree in its source file.
    pub start_byte: usize,
    /// End byte offset of the subtree in its source file.
    pub end_byte: usize,
    /// 1-indexed start line of the subtree.
    pub start_line: usize,
    /// 1-indexed end line of the subtree.
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
    /// The caller's identifier for this fingerprint's source file.
    pub source: S,
    /// The fingerprint itself.
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
/// Grouped by `(hash, tier)`, not `hash` alone, so passing a corpus that
/// mixes [`FingerprintTier::Function`] and [`FingerprintTier::Block`]
/// fingerprints (e.g. the output of both [`function_fingerprints`] and
/// [`block_fingerprints`] concatenated) can never produce a group spanning
/// both tiers — a small flagged loop coincidentally hashing the same as some
/// other file's whole one-line function would otherwise be a confusing
/// result to hand back. Callers who only ever build single-tier corpora see
/// no behavior change from this.
///
/// Group and within-group order is deterministic (sorted by hash, then by
/// source-file position) rather than following `HashMap` iteration order,
/// since callers may snapshot-test or otherwise rely on stable output.
pub fn duplicate_groups<S: Ord + Clone>(
    fingerprints: &[CorpusFingerprint<S>],
) -> Vec<Vec<&CorpusFingerprint<S>>> {
    let mut by_hash: HashMap<(u64, FingerprintTier), Vec<&CorpusFingerprint<S>>> = HashMap::new();
    for fp in fingerprints {
        by_hash
            .entry((fp.fingerprint.hash, fp.fingerprint.tier))
            .or_default()
            .push(fp);
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
    groups.sort_by_key(|members| (members[0].fingerprint.hash, members[0].fingerprint.tier));
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
                tier: FingerprintTier::Function,
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

/// Fingerprints every loop/conditional/switch-like subtree in `tree` (per
/// [`is_block_kind`]), skipping any whose subtree has fewer than `min_nodes`
/// AST nodes — the same noise-suppression floor [`function_fingerprints`]
/// uses, applied at the smaller granularity (see the module doc's
/// "Granularity tiers" section for why `Block` exists and what it's for).
///
/// Like [`function_fingerprints`] with nested functions, a nested block (a
/// loop inside a loop, an `if` inside a `for` body) is fingerprinted both as
/// part of its enclosing block's subtree and again independently — the walk
/// doesn't stop at a block boundary once it's matched one.
///
/// Every returned [`Fingerprint`] has `name: None` — blocks aren't named —
/// and `tier: `[`FingerprintTier::Block`].
pub fn block_fingerprints(root: Node, source: &str, min_nodes: usize) -> Vec<Fingerprint> {
    find_descendants(root, |n| is_block_kind(n.kind()))
        .into_iter()
        .filter_map(|node| {
            let (hash, node_count) = hash_and_count(node, source.as_bytes());
            if node_count < min_nodes {
                return None;
            }
            Some(Fingerprint {
                name: None,
                tier: FingerprintTier::Block,
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

/// Returns `true` if `kind` is a loop/conditional/switch-like node this
/// module treats as a `Block`-tier fingerprint root.
///
/// A flat match across all 16 languages, same shape as
/// [`is_function_kind`] — node kind strings don't collide across grammars,
/// so no per-language dispatch is needed to tell them apart. Each
/// language's list below was verified against that language's vendored
/// `tree-sitter-*` `node-types.json` (not guessed), covering: `if`,
/// every loop form (`while`/`for`/`foreach`/`do-while`/`repeat`/
/// unconditional `loop`), and `switch`/`match`/`when`/`select`-style
/// dispatch — both the dispatching statement itself and, where the grammar
/// gives each case/arm its own node kind, the individual arms (so two
/// identically-shaped `case`/`match` arms in unrelated switches can match
/// each other directly, not just as part of the whole switch).
///
/// Deliberately excludes plain scoping blocks (C's bare `{ }`, a function
/// body) — those aren't "a flagged region" in the sense this tier targets
/// (see the module doc); a bare block's contents are still reachable, just
/// via whatever loop/conditional/function actually roots them.
pub fn is_block_kind(kind: &str) -> bool {
    matches!(
        kind,
        // C / C++ (tree-sitter-c 0.24.2, tree-sitter-cpp 0.23.4)
        "if_statement"
            | "while_statement"
            | "for_statement"
            | "for_range_loop" // cpp
            | "do_statement"
            | "switch_statement"
            | "case_statement"
            // Rust (tree-sitter-rust 0.24.2)
            | "if_expression"
            | "while_expression"
            | "for_expression"
            | "loop_expression"
            | "match_expression"
            | "match_arm"
            // Python (tree-sitter-python 0.25.0)
            | "match_statement"
            | "case_clause" // also Scala's match-arm kind, see below
            // JavaScript / TypeScript (tree-sitter-javascript 0.25.0,
            // tree-sitter-typescript 0.23.2)
            | "for_in_statement"
            | "switch_case"
            | "switch_default"
            // Go (tree-sitter-go 0.25.0)
            | "expression_switch_statement"
            | "type_switch_statement"
            | "select_statement"
            | "expression_case"
            | "type_case"
            | "communication_case"
            // Java (tree-sitter-java 0.23.5)
            | "enhanced_for_statement"
            | "switch_expression" // also C#'s switch-expression kind
            | "switch_block_statement_group"
            | "switch_rule" // Java's arrow-style case label
            // C# (tree-sitter-c-sharp 0.23.5)
            | "foreach_statement" // also PHP's foreach kind
            | "switch_expression_arm"
            | "switch_section"
            // Kotlin (tree-sitter-kotlin-ng 1.1.0)
            | "do_while_statement"
            | "when_expression"
            | "when_entry"
            // Swift (tree-sitter-swift 0.7.3)
            | "repeat_while_statement"
            | "switch_entry"
            // PHP (tree-sitter-php 0.24.2)
            | "match_conditional_expression"
            // Fortran (tree-sitter-fortran 0.6.0)
            | "do_loop"
            | "select_case_statement"
            | "forall_statement"
            // Scala (tree-sitter-scala 0.26.2)
            | "do_while_expression"
            | "type_case_clause"
            // Lua (tree-sitter-lua 0.5.0)
            | "repeat_statement"
            // Ada (tree-sitter-ada 0.1.0)
            | "loop_statement"
            | "case_statement_alternative"
    )
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

    #[test]
    #[cfg(feature = "lang-c")]
    fn block_tier_finds_match_between_non_duplicate_functions() {
        // The driving use case from DETECT_FINE_GRAINED_DUPLICATES.md: two
        // *whole functions* that are structurally unrelated (different
        // statements before/after, different return type) share one
        // identical `for` loop. Only Block-tier fingerprinting surfaces that
        // match — Function-tier fingerprinting of these two functions must
        // NOT match, since the point is that the match is only visible at
        // sub-function granularity.
        let a_src = "int f(int *arr, int n) { \
            int total = 0; \
            for (int i = 0; i < n; i++) { total = total + arr[i]; } \
            return total; }";
        let b_src = "void g(int *arr, int n) { \
            printf(\"start\"); \
            for (int i = 0; i < n; i++) { total = total + arr[i]; } \
            printf(\"end\"); }";
        let tree_a = parse(a_src, tree_sitter_c::LANGUAGE.into());
        let tree_b = parse(b_src, tree_sitter_c::LANGUAGE.into());

        let fn_a = function_fingerprints(tree_a.root_node(), a_src, 0);
        let fn_b = function_fingerprints(tree_b.root_node(), b_src, 0);
        assert_ne!(
            fn_a[0].hash, fn_b[0].hash,
            "f and g must not be function-tier duplicates of each other"
        );

        let block_a = block_fingerprints(tree_a.root_node(), a_src, 0);
        let block_b = block_fingerprints(tree_b.root_node(), b_src, 0);
        let for_a = block_a
            .iter()
            .find(|fp| fp.kind == "for_statement")
            .expect("f's for-loop should be a Block-tier fingerprint");
        let for_b = block_b
            .iter()
            .find(|fp| fp.kind == "for_statement")
            .expect("g's for-loop should be a Block-tier fingerprint");
        assert_eq!(
            for_a.hash, for_b.hash,
            "the two functions' identical for-loop bodies should match at Block tier"
        );
        assert_eq!(for_a.tier, FingerprintTier::Block);
        assert!(for_a.name.is_none(), "blocks are never named");
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn block_fingerprints_min_nodes_filters_out_trivial_subtrees() {
        let source = "int f(int x) { if (x) { x = 1; } return x; }";
        let tree = parse(source, tree_sitter_c::LANGUAGE.into());
        let all = block_fingerprints(tree.root_node(), source, 0);
        assert!(!all.is_empty());
        let filtered = block_fingerprints(tree.root_node(), source, 1_000);
        assert!(filtered.is_empty());
    }

    fn fp_with(tier: FingerprintTier, hash: u64, start_byte: usize) -> Fingerprint {
        Fingerprint {
            name: None,
            tier,
            kind: "for_statement",
            hash,
            node_count: 5,
            start_byte,
            end_byte: start_byte + 1,
            start_line: 1,
            end_line: 1,
        }
    }

    #[test]
    fn duplicate_groups_keeps_function_and_block_tiers_separate() {
        // Two Function-tier fingerprints and two Block-tier fingerprints
        // deliberately share the same hash (a contrived collision — the
        // scenario Ask 2 flags: a flagged loop shouldn't group with an
        // unrelated whole function just because they hash equal). They must
        // form two separate two-member groups, never one four-member group.
        const COLLIDING_HASH: u64 = 42;
        let all = vec![
            CorpusFingerprint {
                source: "a.c",
                fingerprint: fp_with(FingerprintTier::Function, COLLIDING_HASH, 0),
            },
            CorpusFingerprint {
                source: "b.c",
                fingerprint: fp_with(FingerprintTier::Function, COLLIDING_HASH, 10),
            },
            CorpusFingerprint {
                source: "c.c",
                fingerprint: fp_with(FingerprintTier::Block, COLLIDING_HASH, 0),
            },
            CorpusFingerprint {
                source: "d.c",
                fingerprint: fp_with(FingerprintTier::Block, COLLIDING_HASH, 10),
            },
        ];

        let groups = duplicate_groups(&all);
        assert_eq!(
            groups.len(),
            2,
            "cross-tier hash collision must not merge into one group"
        );
        for group in &groups {
            assert_eq!(group.len(), 2);
            let tier = group[0].fingerprint.tier;
            assert!(
                group.iter().all(|m| m.fingerprint.tier == tier),
                "a group must not mix tiers"
            );
        }
        let tiers: std::collections::HashSet<FingerprintTier> =
            groups.iter().map(|g| g[0].fingerprint.tier).collect();
        assert_eq!(
            tiers.len(),
            2,
            "expected one Function group and one Block group"
        );
    }
}
