//! A flat, index-linked snapshot of a whole tree-sitter tree.
//!
//! The PyO3 bindings can never hand Python a `tree_sitter::Node`: this crate's
//! `tree-sitter` has no ABI relationship to tree-sitter's separate Python
//! package. A consumer that walks the tree itself (clew's call, thread and
//! lock harvesters) therefore used to need its own grammar packages, pinned to
//! its own tree-sitter version. [`flatten`] closes that gap. It copies every
//! node, named or anonymous, into parallel columns, so a consumer can rebuild
//! the `Node` API it walks without parsing again. Python reads each column
//! with `memoryview(...).cast("I")`, so building it costs one pass in Rust and
//! nothing per node in Python until a node is actually touched.
//!
//! # Layout
//!
//! Node `i` is row `i` of every column, numbered in pre-order with the root at
//! row 0. A parent therefore always precedes its children, and the rows from
//! `i` up to the next node outside `i`'s subtree are exactly that subtree.
//! Links (`parent`, `first_child`, `next_sibling`, `prev_sibling`) are row
//! indices, with [`NONE`] meaning "no such node". `kind` and `field` index into
//! the [`FlatTree::kinds`] / [`FlatTree::fields`] string tables, and field 0 is
//! always `""` (the node is not a named field of its parent). Positions follow
//! tree-sitter's own conventions: rows are 0-based, columns and offsets are
//! UTF-8 bytes.

use std::collections::HashMap;

use tree_sitter::Tree;

/// The link value meaning "no such node".
pub const NONE: u32 = u32::MAX;

/// `flags` bit: the node is named (`Node::is_named`).
pub const FLAG_NAMED: u32 = 1;
/// `flags` bit: the node is an `ERROR` node (`Node::is_error`).
pub const FLAG_ERROR: u32 = 1 << 1;
/// `flags` bit: the node was inserted by error recovery (`Node::is_missing`).
pub const FLAG_MISSING: u32 = 1 << 2;
/// `flags` bit: the node is an extra, such as a comment (`Node::is_extra`).
pub const FLAG_EXTRA: u32 = 1 << 3;
/// `flags` bit: the node's subtree contains an error (`Node::has_error`).
pub const FLAG_HAS_ERROR: u32 = 1 << 4;

/// Every node of one tree, as parallel columns. See the module docs for the
/// layout.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlatTree {
    /// Node-kind names, indexed by [`FlatTree::kind`].
    pub kinds: Vec<String>,
    /// Field names, indexed by [`FlatTree::field`]. Index 0 is `""`.
    pub fields: Vec<String>,
    /// Index into `kinds`.
    pub kind: Vec<u32>,
    /// Index into `fields`: the field under which this node hangs off its parent.
    pub field: Vec<u32>,
    /// `FLAG_*` bits.
    pub flags: Vec<u32>,
    /// Parent row, or [`NONE`] for the root.
    pub parent: Vec<u32>,
    /// First child row, or [`NONE`].
    pub first_child: Vec<u32>,
    /// Next sibling row, or [`NONE`].
    pub next_sibling: Vec<u32>,
    /// Previous sibling row, or [`NONE`].
    pub prev_sibling: Vec<u32>,
    /// Start offset in bytes.
    pub start_byte: Vec<u32>,
    /// End offset in bytes (exclusive).
    pub end_byte: Vec<u32>,
    /// Start row, 0-based.
    pub start_row: Vec<u32>,
    /// Start column in bytes.
    pub start_col: Vec<u32>,
    /// End row, 0-based.
    pub end_row: Vec<u32>,
    /// End column in bytes.
    pub end_col: Vec<u32>,
    /// Children of row `i` are `child_list[child_offset[i]..child_offset[i + 1]]`,
    /// in order. Redundant with `first_child`/`next_sibling`, but lets a consumer
    /// take every child of a node as one slice instead of chasing links.
    /// `len() + 1` entries.
    pub child_offset: Vec<u32>,
    /// Child rows, grouped by parent: see `child_offset`.
    pub child_list: Vec<u32>,
}

impl FlatTree {
    /// Number of nodes.
    pub fn len(&self) -> usize {
        self.kind.len()
    }

    /// True for a tree with no nodes (never the case for a parsed tree, which
    /// always has a root).
    pub fn is_empty(&self) -> bool {
        self.kind.is_empty()
    }

    /// Kind name of row `i`.
    pub fn kind_name(&self, i: usize) -> &str {
        &self.kinds[self.kind[i] as usize]
    }

    /// Field name of row `i`, or `None` when it is not a named field.
    pub fn field_name(&self, i: usize) -> Option<&str> {
        match self.field[i] {
            0 => None,
            f => Some(&self.fields[f as usize]),
        }
    }

    /// Child rows of row `i`, in order.
    pub fn children(&self, i: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut c = self.first_child[i];
        while c != NONE {
            out.push(c as usize);
            c = self.next_sibling[c as usize];
        }
        out
    }
}

/// Interns strings into a table, returning each one's index.
struct Interner {
    table: Vec<String>,
    index: HashMap<&'static str, u32>,
}

impl Interner {
    fn new(seed: &[&'static str]) -> Self {
        let mut interner = Self {
            table: Vec::new(),
            index: HashMap::new(),
        };
        for s in seed {
            interner.intern(s);
        }
        interner
    }

    fn intern(&mut self, s: &'static str) -> u32 {
        if let Some(&i) = self.index.get(s) {
            return i;
        }
        let i = self.table.len() as u32;
        self.table.push(s.to_string());
        self.index.insert(s, i);
        i
    }
}

fn to_u32(v: usize) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

/// Flatten every node of `tree` into a [`FlatTree`].
///
/// Iterative (a `TreeCursor` walk), so a deeply nested file cannot overflow
/// the stack.
pub fn flatten(tree: &Tree) -> FlatTree {
    let mut flat = FlatTree::default();
    let mut kinds = Interner::new(&[]);
    let mut fields = Interner::new(&[""]);
    let mut cursor = tree.walk();
    // The open ancestors of the cursor's position, and the last child row
    // seen under each, so siblings can be linked as they are visited.
    let mut parents: Vec<u32> = Vec::new();
    let mut last_child: Vec<u32> = Vec::new();

    loop {
        let node = cursor.node();
        let row = to_u32(flat.kind.len());
        let mut flags = 0;
        for (on, bit) in [
            (node.is_named(), FLAG_NAMED),
            (node.is_error(), FLAG_ERROR),
            (node.is_missing(), FLAG_MISSING),
            (node.is_extra(), FLAG_EXTRA),
            (node.has_error(), FLAG_HAS_ERROR),
        ] {
            if on {
                flags |= bit;
            }
        }
        let (start, end) = (node.start_position(), node.end_position());
        flat.kind.push(kinds.intern(node.kind()));
        flat.field
            .push(cursor.field_name().map_or(0, |f| fields.intern(f)));
        flat.flags.push(flags);
        flat.parent.push(parents.last().copied().unwrap_or(NONE));
        flat.first_child.push(NONE);
        flat.next_sibling.push(NONE);
        flat.prev_sibling.push(NONE);
        flat.start_byte.push(to_u32(node.start_byte()));
        flat.end_byte.push(to_u32(node.end_byte()));
        flat.start_row.push(to_u32(start.row));
        flat.start_col.push(to_u32(start.column));
        flat.end_row.push(to_u32(end.row));
        flat.end_col.push(to_u32(end.column));

        if let (Some(&parent), Some(last)) = (parents.last(), last_child.last_mut()) {
            if *last == NONE {
                flat.first_child[parent as usize] = row;
            } else {
                flat.next_sibling[*last as usize] = row;
                flat.prev_sibling[row as usize] = *last;
            }
            *last = row;
        }

        if cursor.goto_first_child() {
            parents.push(row);
            last_child.push(NONE);
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                flat.kinds = kinds.table;
                flat.fields = fields.table;
                index_children(&mut flat);
                return flat;
            }
            parents.pop();
            last_child.pop();
        }
    }
}

/// Fill `child_offset`/`child_list` from the sibling links.
fn index_children(flat: &mut FlatTree) {
    let n = flat.len();
    flat.child_offset = Vec::with_capacity(n + 1);
    flat.child_list = Vec::with_capacity(n.saturating_sub(1));
    for i in 0..n {
        flat.child_offset.push(to_u32(flat.child_list.len()));
        let mut c = flat.first_child[i];
        while c != NONE {
            flat.child_list.push(c);
            c = flat.next_sibling[c as usize];
        }
    }
    flat.child_offset.push(to_u32(flat.child_list.len()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::{Node, Parser};

    fn parse(key: &str, src: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&crate::registry::language_for_key(key).unwrap())
            .unwrap();
        parser.parse(src, None).unwrap()
    }

    /// Every row must agree with the live tree it was copied from: same kind,
    /// field, flags, positions, and the same parent/children/sibling shape.
    fn assert_mirrors(flat: &FlatTree, node: Node, row: usize, next: &mut usize) {
        assert_eq!(row, *next, "rows are pre-order");
        *next += 1;
        assert_eq!(flat.kind_name(row), node.kind());
        assert_eq!(flat.start_byte[row] as usize, node.start_byte());
        assert_eq!(flat.end_byte[row] as usize, node.end_byte());
        assert_eq!(flat.start_row[row] as usize, node.start_position().row);
        assert_eq!(flat.end_col[row] as usize, node.end_position().column);
        assert_eq!(flat.flags[row] & FLAG_NAMED != 0, node.is_named());
        assert_eq!(flat.flags[row] & FLAG_HAS_ERROR != 0, node.has_error());
        let children = flat.children(row);
        assert_eq!(children.len(), node.child_count());
        let (lo, hi) = (
            flat.child_offset[row] as usize,
            flat.child_offset[row + 1] as usize,
        );
        let listed: Vec<usize> = flat.child_list[lo..hi]
            .iter()
            .map(|&c| c as usize)
            .collect();
        assert_eq!(listed, children, "child_list agrees with the sibling links");
        let mut cursor = node.walk();
        for (i, child) in node.children(&mut cursor).enumerate() {
            let child_row = children[i];
            assert_eq!(flat.parent[child_row] as usize, row);
            assert_eq!(
                flat.field_name(child_row),
                node.field_name_for_child(i as u32)
            );
            assert_mirrors(flat, child, child_row, next);
        }
    }

    #[test]
    #[cfg(feature = "lang-rust")]
    fn rust_tree_is_mirrored_exactly() {
        let tree = parse(
            "rust",
            "/// doc\n#[inline]\npub fn add(x: i32) -> i32 { x + 1 }\nimpl Foo { fn new() -> Self { Foo } }\n",
        );
        let flat = flatten(&tree);
        let mut next = 0;
        assert_mirrors(&flat, tree.root_node(), 0, &mut next);
        assert_eq!(next, flat.len());
        assert_eq!(flat.kind_name(0), "source_file");
        assert_eq!(flat.parent[0], NONE);
    }

    #[test]
    #[cfg(feature = "lang-python")]
    fn python_fields_and_siblings() {
        let tree = parse("python", "def f(a, b):\n    return g(a)\n");
        let flat = flatten(&tree);
        let mut next = 0;
        assert_mirrors(&flat, tree.root_node(), 0, &mut next);
        let def = flat.first_child[0] as usize;
        assert_eq!(flat.kind_name(def), "function_definition");
        let named: Vec<_> = flat
            .children(def)
            .into_iter()
            .filter_map(|c| flat.field_name(c).map(|f| (f, flat.kind_name(c))))
            .collect();
        assert!(named.contains(&("name", "identifier")));
        assert!(named.contains(&("body", "block")));
        for c in flat.children(def) {
            let prev = flat.prev_sibling[c];
            if prev != NONE {
                assert_eq!(flat.next_sibling[prev as usize] as usize, c);
            }
        }
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn errors_and_missing_nodes_are_flagged() {
        let tree = parse("c", "int f( { return 1 }");
        let flat = flatten(&tree);
        assert!(flat.flags[0] & FLAG_HAS_ERROR != 0);
        assert!((0..flat.len()).any(|i| flat.flags[i] & (FLAG_ERROR | FLAG_MISSING) != 0));
    }

    #[test]
    #[cfg(feature = "lang-typescript")]
    fn tsx_parses_jsx_without_errors() {
        let tree = parse("tsx", "const a = <div>{x}</div>;\n");
        let flat = flatten(&tree);
        assert_eq!(flat.flags[0] & FLAG_HAS_ERROR, 0);
    }
}
