//! Language-agnostic control-flow graph / basic-block construction on top of
//! the tree-sitter parse — Tier 3 of the substrate's capability model (see
//! the crate README/CLAUDE.md). Generalizes the shape of tools_sqc's C-only
//! CFG builder (`tools_sqc/src/analyze/cfg.rs`) for shared use.
//!
//! v1 scope covers `c`, `cpp`, and `rust`. Like `language_for_file`,
//! [`build_function_cfg`] never fabricates a result for a language it
//! doesn't model — it returns `None` rather than a fallback. Extending to
//! more languages is a matter of adding another [`Shapes`] table entry; see
//! `docs/` or `todo.db` for tracking further language coverage.
//!
//! Deliberately out of scope for v1 (kept simple to avoid the correctness
//! risk of guessing at un-verified per-language quirks): `switch`/`match`
//! decomposition (treated as one opaque statement, matching tools_sqc's own
//! existing precedent for C `switch`), `goto`/labeled statements, and
//! constant-condition dead-branch folding (tools_sqc's `MacroConstantMap` is
//! a C-preprocessor-specific concept; a generic constant-folding hook is
//! left to a future task if a second language needs it).
//!
//! This module does not migrate tools_sqc's or knots' existing call sites —
//! see the substrate's task history for why that migration, if ever done,
//! belongs to its own follow-up task rather than this one.

use tree_sitter::Node;

pub type BlockId = usize;

/// A single-entry, single-exit sequence of statements.
#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub id: BlockId,
    /// Byte ranges of the opaque statements appended to this block, in order.
    pub statements: Vec<(usize, usize)>,
    /// Overall byte range spanned by this block's content.
    pub byte_range: (usize, usize),
    /// Byte range of the controlling condition expression, for blocks that
    /// end in a branch (`if`/`while`/`for`/`do-while` headers).
    pub condition_range: Option<(usize, usize)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CfgEdge {
    Fallthrough,
    TrueBranch,
    FalseBranch,
    BackEdge,
    Return,
    Break,
    Continue,
}

#[derive(Debug, Clone)]
pub struct FunctionCfg {
    pub blocks: Vec<BasicBlock>,
    pub edges: Vec<(BlockId, BlockId, CfgEdge)>,
    pub entry: BlockId,
    pub exits: Vec<BlockId>,
}

impl FunctionCfg {
    pub fn successors(&self, id: BlockId) -> Vec<(BlockId, CfgEdge)> {
        self.edges
            .iter()
            .filter(|(from, _, _)| *from == id)
            .map(|(_, to, edge)| (*to, *edge))
            .collect()
    }

    pub fn predecessors(&self, id: BlockId) -> Vec<(BlockId, CfgEdge)> {
        self.edges
            .iter()
            .filter(|(_, to, _)| *to == id)
            .map(|(from, _, edge)| (*from, *edge))
            .collect()
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    pub fn get_block(&self, id: BlockId) -> Option<&BasicBlock> {
        self.blocks.get(id)
    }
}

/// Per-language node-kind vocabulary. Field names (`condition`,
/// `consequence`, `alternative`, `body`, `initializer`, `update`) are shared
/// across `c`/`cpp`/`rust` and are looked up directly rather than through
/// this table; only which node *kinds* mean "this is an if/while/..." varies.
struct Shapes {
    if_kinds: &'static [&'static str],
    while_kinds: &'static [&'static str],
    /// Pre-test loops: `body` required, `condition`/`initializer`/`update`
    /// all optional. Modeled as a header block with `TrueBranch` (enter
    /// body) / `FalseBranch` (fall through) edges, matching a `for`-in
    /// loop's implicit "more elements?" test even when there's no literal
    /// boolean condition node.
    for_kinds: &'static [&'static str],
    /// Loops with no implicit exit test at all (Rust's bare `loop {}`) —
    /// the header has no `FalseBranch`; the only way out is `break`.
    unconditional_loop_kinds: &'static [&'static str],
    /// Post-test loops: `body` runs once unconditionally, then `condition`
    /// gates the back edge.
    do_while_kinds: &'static [&'static str],
    return_kinds: &'static [&'static str],
    break_kinds: &'static [&'static str],
    continue_kinds: &'static [&'static str],
    /// Bare nested blocks (e.g. a C `{ }` used only for scoping) — flattened
    /// transparently rather than treated as an opaque statement or a new
    /// control-flow-relevant block boundary.
    block_kinds: &'static [&'static str],
    /// Transparent single-child wrappers to unwrap before classifying a
    /// statement (Rust wraps most statement-position expressions, including
    /// `if`/`while`/`return`/`break`/`continue`, in `expression_statement`).
    stmt_wrapper_kinds: &'static [&'static str],
    /// Transparent single-child wrapper around an `if`'s `alternative` field
    /// (both `c`/`cpp` and `rust` wrap it in `else_clause`, verified against
    /// the vendored grammars rather than assumed).
    else_wrapper_kinds: &'static [&'static str],
}

const C_SHAPES: Shapes = Shapes {
    if_kinds: &["if_statement"],
    while_kinds: &["while_statement"],
    for_kinds: &["for_statement"],
    unconditional_loop_kinds: &[],
    do_while_kinds: &["do_statement"],
    return_kinds: &["return_statement"],
    break_kinds: &["break_statement"],
    continue_kinds: &["continue_statement"],
    block_kinds: &["compound_statement"],
    stmt_wrapper_kinds: &[],
    else_wrapper_kinds: &["else_clause"],
};

const RUST_SHAPES: Shapes = Shapes {
    if_kinds: &["if_expression"],
    while_kinds: &["while_expression"],
    for_kinds: &["for_expression"],
    unconditional_loop_kinds: &["loop_expression"],
    do_while_kinds: &[],
    return_kinds: &["return_expression"],
    break_kinds: &["break_expression"],
    continue_kinds: &["continue_expression"],
    block_kinds: &["block"],
    stmt_wrapper_kinds: &["expression_statement"],
    else_wrapper_kinds: &["else_clause"],
};

fn shapes_for(key: &str) -> Option<&'static Shapes> {
    match key {
        "c" | "cpp" => Some(&C_SHAPES),
        "rust" => Some(&RUST_SHAPES),
        _ => None,
    }
}

/// Builds a CFG for `func_node`'s body. `key` is the substrate registry
/// language key (see [`crate::language_info_for_file`]). Returns `None` when
/// the language isn't modeled yet, or `func_node` has no `body` field.
pub fn build_function_cfg<'a>(
    func_node: Node<'a>,
    source: &'a [u8],
    key: &str,
) -> Option<FunctionCfg> {
    let shapes = shapes_for(key)?;
    let body = func_node.child_by_field_name("body")?;
    let mut builder = CfgBuilder::new(shapes, body.start_byte());
    builder.visit(body, source);
    Some(builder.finish())
}

/// One unit of pending work in `CfgBuilder::visit`'s explicit continuation
/// stack. `Visit` mirrors a call to the old recursive `visit`/`build_block`;
/// each `After*` variant is a resumption point holding exactly the state a
/// `process_*` method used to keep in local variables across its own
/// (now-removed) recursive call — see `visit`'s doc comment for how they're
/// sequenced.
enum Task<'a> {
    Visit(Node<'a>),
    AfterIfTrue {
        if_node: Node<'a>,
        header: BlockId,
    },
    AfterIfFalse {
        if_node: Node<'a>,
        true_end: BlockId,
    },
    AfterWhileBody {
        header: BlockId,
        after: BlockId,
    },
    AfterForBody {
        for_node: Node<'a>,
        header: BlockId,
        after: BlockId,
    },
    AfterUnconditionalLoopBody {
        header: BlockId,
        after: BlockId,
    },
    AfterDoWhileBody {
        do_node: Node<'a>,
        body_start: BlockId,
        footer: BlockId,
        after: BlockId,
    },
}

struct CfgBuilder {
    shapes: &'static Shapes,
    blocks: Vec<BasicBlock>,
    edges: Vec<(BlockId, BlockId, CfgEdge)>,
    terminated: Vec<bool>,
    current: BlockId,
    /// (continue target, break target) for the innermost enclosing loop.
    loop_stack: Vec<(BlockId, BlockId)>,
}

impl CfgBuilder {
    fn new(shapes: &'static Shapes, start: usize) -> Self {
        let entry = BasicBlock {
            id: 0,
            statements: Vec::new(),
            byte_range: (start, start),
            condition_range: None,
        };
        Self {
            shapes,
            blocks: vec![entry],
            edges: Vec::new(),
            terminated: vec![false],
            current: 0,
            loop_stack: Vec::new(),
        }
    }

    fn new_block(&mut self, start: usize) -> BlockId {
        let id = self.blocks.len();
        self.blocks.push(BasicBlock {
            id,
            statements: Vec::new(),
            byte_range: (start, start),
            condition_range: None,
        });
        self.terminated.push(false);
        id
    }

    fn add_edge(&mut self, from: BlockId, to: BlockId, edge: CfgEdge) {
        let key = (from, to, edge);
        if !self.edges.contains(&key) {
            self.edges.push(key);
        }
    }

    /// Adds `edge` from `from` to `to` unless `from` is already terminated
    /// (ends in a `Return`/`Break`/`Continue`) — a terminated block can't
    /// also fall or loop through to another block.
    fn join(&mut self, from: BlockId, to: BlockId, edge: CfgEdge) {
        if !self.terminated[from] {
            self.add_edge(from, to, edge);
        }
    }

    fn append_stmt(&mut self, node: Node) {
        let cur = self.current;
        let block = &mut self.blocks[cur];
        if block.statements.is_empty() {
            block.byte_range.0 = node.start_byte();
        }
        block.byte_range.1 = node.end_byte();
        block.statements.push((node.start_byte(), node.end_byte()));
    }

    fn unwrap<'a>(&self, mut node: Node<'a>, wrappers: &[&str]) -> Node<'a> {
        while wrappers.contains(&node.kind()) {
            match node.named_child(0) {
                Some(inner) => node = inner,
                None => break,
            }
        }
        node
    }

    /// Iterative (explicit continuation stack), not recursive: a real-world
    /// function with a multi-thousand-deep nested control structure
    /// overflowed the call stack under the naive recursive version this
    /// replaces (the same failure mode `crate::query`'s traversal helpers
    /// guard against). `Task::Visit` mirrors the old `visit`/`build_block`
    /// dispatch; the `Task::After*` variants carry exactly the state each
    /// `process_*` method used to hold in local variables across its own
    /// (now-removed) recursive `self.visit(...)` call — e.g. `AfterIfTrue`
    /// resumes exactly where `process_if` used to continue after visiting
    /// the consequence, with `header` in hand to wire up the false branch.
    /// Pushing an `After*` task and then (optionally) a `Visit` task on top
    /// of it reproduces "call, then continue here when it returns": the
    /// `Visit` and everything it pushes drains first (LIFO), only then does
    /// the `After*` task run — same order a real call stack would give.
    fn visit(&mut self, node: Node, source: &[u8]) {
        let mut stack = vec![Task::Visit(node)];
        while let Some(task) = stack.pop() {
            match task {
                Task::Visit(node) => self.dispatch(node, source, &mut stack),
                Task::AfterIfTrue { if_node, header } => {
                    let true_end = self.current;
                    let false_start = self.new_block(if_node.start_byte());
                    self.add_edge(header, false_start, CfgEdge::FalseBranch);
                    self.current = false_start;
                    stack.push(Task::AfterIfFalse { if_node, true_end });
                    if let Some(alt) = if_node.child_by_field_name("alternative") {
                        let alt = self.unwrap(alt, self.shapes.else_wrapper_kinds);
                        stack.push(Task::Visit(alt));
                    }
                }
                Task::AfterIfFalse { if_node, true_end } => {
                    let false_end = self.current;
                    let join_block = self.new_block(if_node.end_byte());
                    self.join(true_end, join_block, CfgEdge::Fallthrough);
                    self.join(false_end, join_block, CfgEdge::Fallthrough);
                    self.current = join_block;
                }
                Task::AfterWhileBody { header, after } => {
                    self.loop_stack.pop();
                    self.join(self.current, header, CfgEdge::BackEdge);
                    self.current = after;
                }
                Task::AfterForBody {
                    for_node,
                    header,
                    after,
                } => {
                    self.loop_stack.pop();
                    if let Some(update) = for_node.child_by_field_name("update") {
                        if !self.terminated[self.current] {
                            self.append_stmt(update);
                        }
                    }
                    self.join(self.current, header, CfgEdge::BackEdge);
                    self.current = after;
                }
                Task::AfterUnconditionalLoopBody { header, after } => {
                    self.loop_stack.pop();
                    self.join(self.current, header, CfgEdge::BackEdge);
                    self.current = after;
                }
                Task::AfterDoWhileBody {
                    do_node,
                    body_start,
                    footer,
                    after,
                } => {
                    self.loop_stack.pop();
                    self.join(self.current, footer, CfgEdge::Fallthrough);
                    if let Some(cond) = do_node.child_by_field_name("condition") {
                        self.blocks[footer].condition_range =
                            Some((cond.start_byte(), cond.end_byte()));
                    }
                    self.add_edge(footer, body_start, CfgEdge::BackEdge);
                    self.add_edge(footer, after, CfgEdge::FalseBranch);
                    self.current = after;
                }
            }
        }
    }

    fn dispatch<'a>(&mut self, node: Node<'a>, _source: &[u8], stack: &mut Vec<Task<'a>>) {
        let node = self.unwrap(node, self.shapes.stmt_wrapper_kinds);
        let kind = node.kind();
        let shapes = self.shapes;

        if shapes.block_kinds.contains(&kind) {
            self.push_block_children(node, stack);
        } else if shapes.if_kinds.contains(&kind) {
            self.process_if(node, stack);
        } else if shapes.while_kinds.contains(&kind) {
            self.process_while(node, stack);
        } else if shapes.for_kinds.contains(&kind) {
            self.process_for(node, stack);
        } else if shapes.unconditional_loop_kinds.contains(&kind) {
            self.process_unconditional_loop(node, stack);
        } else if shapes.do_while_kinds.contains(&kind) {
            self.process_do_while(node, stack);
        } else if shapes.return_kinds.contains(&kind) {
            self.append_stmt(node);
            let next = self.new_block(node.end_byte());
            self.add_edge(self.current, next, CfgEdge::Return);
            self.terminated[self.current] = true;
            self.current = next;
            self.terminated[next] = true;
        } else if shapes.break_kinds.contains(&kind) {
            self.append_stmt(node);
            if let Some(&(_, brk)) = self.loop_stack.last() {
                self.add_edge(self.current, brk, CfgEdge::Break);
            }
            self.terminated[self.current] = true;
            self.current = self.new_block(node.end_byte());
            self.terminated[self.current] = true;
        } else if shapes.continue_kinds.contains(&kind) {
            self.append_stmt(node);
            if let Some(&(cont, _)) = self.loop_stack.last() {
                self.add_edge(self.current, cont, CfgEdge::Continue);
            }
            self.terminated[self.current] = true;
            self.current = self.new_block(node.end_byte());
            self.terminated[self.current] = true;
        } else {
            self.append_stmt(node);
        }
    }

    /// Pushes a block's named children as `Visit` tasks in reverse order, so
    /// popping the (LIFO) stack visits them in original left-to-right order —
    /// the same sequence `build_block`'s `for` loop used to produce.
    fn push_block_children<'a>(&self, node: Node<'a>, stack: &mut Vec<Task<'a>>) {
        let mut cursor = node.walk();
        let children: Vec<Node<'a>> = node
            .children(&mut cursor)
            .filter(|c| c.is_named())
            .collect();
        stack.extend(children.into_iter().rev().map(Task::Visit));
    }

    fn process_if<'a>(&mut self, node: Node<'a>, stack: &mut Vec<Task<'a>>) {
        let header = self.current;
        if let Some(cond) = node.child_by_field_name("condition") {
            self.blocks[header].condition_range = Some((cond.start_byte(), cond.end_byte()));
        }

        let true_start = self.new_block(node.start_byte());
        self.add_edge(header, true_start, CfgEdge::TrueBranch);
        self.current = true_start;

        stack.push(Task::AfterIfTrue {
            if_node: node,
            header,
        });
        if let Some(cons) = node.child_by_field_name("consequence") {
            stack.push(Task::Visit(cons));
        }
    }

    fn process_while<'a>(&mut self, node: Node<'a>, stack: &mut Vec<Task<'a>>) {
        let prev = self.current;
        let header = self.new_block(node.start_byte());
        self.join(prev, header, CfgEdge::Fallthrough);
        if let Some(cond) = node.child_by_field_name("condition") {
            self.blocks[header].condition_range = Some((cond.start_byte(), cond.end_byte()));
        }

        let after = self.new_block(node.end_byte());
        self.add_edge(header, after, CfgEdge::FalseBranch);
        let body_start = self.new_block(node.start_byte());
        self.add_edge(header, body_start, CfgEdge::TrueBranch);
        self.current = body_start;

        self.loop_stack.push((header, after));
        stack.push(Task::AfterWhileBody { header, after });
        if let Some(body) = node.child_by_field_name("body") {
            stack.push(Task::Visit(body));
        }
    }

    /// Pre-test loop with optional `initializer`/`condition`/`update`
    /// (covers C's `for (init; cond; update)` and Rust's `for pat in val`,
    /// which has neither initializer/update nor a literal condition node
    /// but the same "test, maybe enter body, maybe exit" shape).
    fn process_for<'a>(&mut self, node: Node<'a>, stack: &mut Vec<Task<'a>>) {
        if let Some(init) = node.child_by_field_name("initializer") {
            self.append_stmt(init);
        }
        let prev = self.current;
        let header = self.new_block(node.start_byte());
        self.join(prev, header, CfgEdge::Fallthrough);
        if let Some(cond) = node.child_by_field_name("condition") {
            self.blocks[header].condition_range = Some((cond.start_byte(), cond.end_byte()));
        }

        let after = self.new_block(node.end_byte());
        self.add_edge(header, after, CfgEdge::FalseBranch);
        let body_start = self.new_block(node.start_byte());
        self.add_edge(header, body_start, CfgEdge::TrueBranch);
        self.current = body_start;

        self.loop_stack.push((header, after));
        stack.push(Task::AfterForBody {
            for_node: node,
            header,
            after,
        });
        if let Some(body) = node.child_by_field_name("body") {
            stack.push(Task::Visit(body));
        }
    }

    /// A loop with no implicit exit test (Rust's `loop { .. }`) — the header
    /// doubles as the body-entry block, and `after` is reachable only via an
    /// explicit `break` inside the body.
    fn process_unconditional_loop<'a>(&mut self, node: Node<'a>, stack: &mut Vec<Task<'a>>) {
        let prev = self.current;
        let header = self.new_block(node.start_byte());
        self.join(prev, header, CfgEdge::Fallthrough);
        let after = self.new_block(node.end_byte());
        self.current = header;

        self.loop_stack.push((header, after));
        stack.push(Task::AfterUnconditionalLoopBody { header, after });
        if let Some(body) = node.child_by_field_name("body") {
            stack.push(Task::Visit(body));
        }
    }

    fn process_do_while<'a>(&mut self, node: Node<'a>, stack: &mut Vec<Task<'a>>) {
        let prev = self.current;
        let body_start = self.new_block(node.start_byte());
        self.join(prev, body_start, CfgEdge::Fallthrough);
        let footer = self.new_block(node.end_byte());
        let after = self.new_block(node.end_byte());
        self.current = body_start;

        self.loop_stack.push((footer, after));
        stack.push(Task::AfterDoWhileBody {
            do_node: node,
            body_start,
            footer,
            after,
        });
        if let Some(body) = node.child_by_field_name("body") {
            stack.push(Task::Visit(body));
        }
    }

    fn finish(self) -> FunctionCfg {
        let last = self.blocks.len() - 1;
        let mut exits: Vec<BlockId> = self
            .edges
            .iter()
            .filter(|(_, _, edge)| *edge == CfgEdge::Return)
            .map(|(_, to, _)| *to)
            .collect();
        let last_has_outgoing = self.edges.iter().any(|(from, _, _)| *from == last);
        if !last_has_outgoing && !exits.contains(&last) {
            exits.push(last);
        }
        exits.sort_unstable();
        exits.dedup();

        FunctionCfg {
            blocks: self.blocks,
            edges: self.edges,
            entry: 0,
            exits,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str, language: tree_sitter::Language) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        parser.parse(source, None).unwrap()
    }

    fn find<'a>(node: Node<'a>, kind: &str) -> Node<'a> {
        fn search<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
            if node.kind() == kind {
                return Some(node);
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(found) = search(child, kind) {
                    return Some(found);
                }
            }
            None
        }
        search(node, kind).unwrap_or_else(|| panic!("no {kind} node found"))
    }

    fn c_cfg(source: &str) -> FunctionCfg {
        let tree = parse(source, tree_sitter_c::LANGUAGE.into());
        let bytes = source.as_bytes();
        let func = find(tree.root_node(), "function_definition");
        build_function_cfg(func, bytes, "c").unwrap()
    }

    #[cfg(feature = "lang-rust")]
    fn rust_cfg(source: &str) -> FunctionCfg {
        let tree = parse(source, tree_sitter_rust::LANGUAGE.into());
        let bytes = source.as_bytes();
        let func = find(tree.root_node(), "function_item");
        build_function_cfg(func, bytes, "rust").unwrap()
    }

    #[cfg(feature = "lang-cpp")]
    fn cpp_cfg(source: &str) -> FunctionCfg {
        let tree = parse(source, tree_sitter_cpp::LANGUAGE.into());
        let bytes = source.as_bytes();
        let func = find(tree.root_node(), "function_definition");
        build_function_cfg(func, bytes, "cpp").unwrap()
    }

    fn edge_kinds(cfg: &FunctionCfg) -> Vec<CfgEdge> {
        cfg.edges.iter().map(|(_, _, e)| *e).collect()
    }

    #[test]
    #[cfg(feature = "lang-python")]
    fn unmapped_language_returns_none() {
        let tree = parse(
            "def f():\n    return 1\n",
            tree_sitter_python::LANGUAGE.into(),
        );
        let func = find(tree.root_node(), "function_definition");
        assert!(build_function_cfg(func, "".as_bytes(), "python").is_none());
    }

    #[test]
    fn c_straight_line_is_a_single_block() {
        let cfg = c_cfg("int f(void) { int x = 1; int y = 2; }");
        assert_eq!(cfg.block_count(), 1);
        assert_eq!(cfg.entry, 0);
        assert_eq!(cfg.exits, vec![0]);
    }

    #[test]
    fn c_if_else_has_true_false_and_fallthrough_join() {
        let cfg = c_cfg("int f(int x) { if (x) { x = 1; } else { x = 2; } }");
        let kinds = edge_kinds(&cfg);
        assert!(kinds.contains(&CfgEdge::TrueBranch));
        assert!(kinds.contains(&CfgEdge::FalseBranch));
        assert_eq!(
            kinds.iter().filter(|k| **k == CfgEdge::Fallthrough).count(),
            2
        );
    }

    #[test]
    fn c_if_without_else_still_joins() {
        let cfg = c_cfg("int f(int x) { if (x) { x = 1; } x = 2; }");
        let kinds = edge_kinds(&cfg);
        assert!(kinds.contains(&CfgEdge::TrueBranch));
        assert!(kinds.contains(&CfgEdge::FalseBranch));
    }

    #[test]
    fn c_return_in_branch_does_not_join() {
        // The true branch returns, so it must NOT get a Fallthrough edge
        // into the join block — only the false branch does.
        let cfg = c_cfg("int f(int x) { if (x) { return 1; } x = 2; }");
        let kinds = edge_kinds(&cfg);
        assert!(kinds.contains(&CfgEdge::Return));
        assert_eq!(
            kinds.iter().filter(|k| **k == CfgEdge::Fallthrough).count(),
            1
        );
    }

    #[test]
    fn c_while_with_break_and_backedge() {
        let cfg = c_cfg("int f(int x) { while (x) { if (x) { break; } x = x - 1; } }");
        let kinds = edge_kinds(&cfg);
        assert!(kinds.contains(&CfgEdge::BackEdge));
        assert!(kinds.contains(&CfgEdge::Break));
        assert!(kinds.contains(&CfgEdge::FalseBranch));
    }

    #[test]
    fn c_for_loop_has_condition_and_backedge() {
        let cfg = c_cfg("int f(void) { for (int i = 0; i < 10; i = i + 1) { continue; } }");
        let kinds = edge_kinds(&cfg);
        assert!(kinds.contains(&CfgEdge::TrueBranch));
        assert!(kinds.contains(&CfgEdge::FalseBranch));
        assert!(kinds.contains(&CfgEdge::Continue));
    }

    #[test]
    fn c_do_while_body_runs_before_condition_check() {
        let cfg = c_cfg("int f(int x) { do { x = x - 1; } while (x); }");
        let kinds = edge_kinds(&cfg);
        assert!(kinds.contains(&CfgEdge::BackEdge));
        assert!(kinds.contains(&CfgEdge::FalseBranch));
    }

    #[test]
    fn c_nested_compound_statement_flattens() {
        // A bare `{ }` scoping block should not create extra branch edges.
        let cfg = c_cfg("int f(void) { { int x = 1; } { int y = 2; } }");
        assert_eq!(cfg.block_count(), 1);
    }

    #[test]
    #[cfg(feature = "lang-cpp")]
    fn cpp_if_else_and_loop_share_c_shapes() {
        let cfg = cpp_cfg(
            "int f(int x) { if (x) { return 1; } else { while (x) { break; } } return 0; }",
        );
        let kinds = edge_kinds(&cfg);
        assert!(kinds.contains(&CfgEdge::TrueBranch));
        assert!(kinds.contains(&CfgEdge::FalseBranch));
        assert!(kinds.contains(&CfgEdge::Break));
        assert_eq!(kinds.iter().filter(|k| **k == CfgEdge::Return).count(), 2);
    }

    #[test]
    fn c_deeply_nested_if_chain_does_not_overflow_the_stack() {
        // Regression: the pre-iterative CfgBuilder::visit recursed once per
        // nesting level. A real caller (knots) hit this via a syntactically
        // valid but deeply nested C file — reproduced here at a depth well
        // past what a recursive implementation survives on a normal thread
        // stack (see query.rs's analogous 20k-deep regression test).
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
        let cfg = c_cfg(&source);
        let kinds = edge_kinds(&cfg);
        assert_eq!(
            kinds.iter().filter(|k| **k == CfgEdge::TrueBranch).count(),
            depth
        );
    }

    #[test]
    #[cfg(feature = "lang-rust")]
    fn rust_if_else_unwraps_expression_statement_and_else_clause() {
        let cfg = rust_cfg("fn f(x: i32) -> i32 { if x > 0 { return x; } else { return -x; } }");
        let kinds = edge_kinds(&cfg);
        assert!(kinds.contains(&CfgEdge::TrueBranch));
        assert!(kinds.contains(&CfgEdge::FalseBranch));
        assert_eq!(kinds.iter().filter(|k| **k == CfgEdge::Return).count(), 2);
    }

    #[test]
    #[cfg(feature = "lang-rust")]
    fn rust_for_loop_models_implicit_test() {
        let cfg = rust_cfg("fn f() { for i in 0..10 { continue; } }");
        let kinds = edge_kinds(&cfg);
        assert!(kinds.contains(&CfgEdge::TrueBranch));
        assert!(kinds.contains(&CfgEdge::FalseBranch));
        assert!(kinds.contains(&CfgEdge::Continue));
    }

    #[test]
    #[cfg(feature = "lang-rust")]
    fn rust_unconditional_loop_has_no_false_branch() {
        // `loop {}` has no implicit exit test — FalseBranch would be wrong.
        // The conditional break (not the last statement) leaves a live path
        // that falls through to the end of the body and loops back. The one
        // FalseBranch edge present belongs to the inner `if`, not the loop
        // header — a bare `loop` never gets a FalseBranch of its own.
        let cfg = rust_cfg("fn f(mut x: i32) { loop { if x > 0 { break; } x = x + 1; } }");
        let kinds = edge_kinds(&cfg);
        assert_eq!(
            kinds.iter().filter(|k| **k == CfgEdge::FalseBranch).count(),
            1
        );
        assert!(kinds.contains(&CfgEdge::Break));
        assert!(kinds.contains(&CfgEdge::BackEdge));
    }

    #[test]
    #[cfg(feature = "lang-rust")]
    fn rust_unconditional_loop_exit_only_reachable_via_break() {
        let cfg = rust_cfg("fn f() -> i32 { loop { return 1; } }");
        // No break anywhere, so some block (the loop's unreachable "after")
        // must exist with no predecessors and no content.
        let has_unreachable_after = cfg.blocks.iter().any(|b| {
            b.id != cfg.entry && cfg.predecessors(b.id).is_empty() && b.statements.is_empty()
        });
        assert!(
            has_unreachable_after,
            "unreachable after-loop block should exist with no predecessors"
        );
    }
}
