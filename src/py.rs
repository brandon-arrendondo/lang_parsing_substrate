//! PyO3 bindings, feature-gated behind `pyo3`. Every public function here
//! owns its parse: Python cannot hand in a `tree_sitter::Node`/`Tree` (this
//! crate's `tree-sitter` version has no ABI relationship to tree-sitter's own
//! separate Python bindings), so each wrapper takes `(language_key, source)`,
//! parses internally, walks the tree, and returns only owned data.
//!
//! Consumers that need the tree itself for their own semantic walks (e.g.
//! clew's thread/lock harvesting) call `parse_tree`, which returns every node
//! as flat columns ([`crate::flat`]) they can wrap in their own node type. The
//! rest of this module exposes the substrate's analysis primitives.
//!
//! `useless_conversion` is allowed crate-wide-in-this-module: pyo3 0.22's
//! `#[pyfunction]` expansion applies a `?`/`From<PyErr>` conversion clippy
//! flags as a no-op on functions that already return `PyResult` — a
//! macro-generated false positive, not something callable here can fix.
#![allow(clippy::useless_conversion)]

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyString};
use std::sync::atomic::{AtomicU64, Ordering};
use tree_sitter::Parser;

use crate::calls;
use crate::calls::CallEdge;
use crate::cfg;
use crate::cfg::{BasicBlock, CfgEdge, FunctionCfg};
use crate::fingerprint;
use crate::fingerprint::Fingerprint;
use crate::flat::{self, FlatTree};
use crate::imports;
use crate::regions;
use crate::regions::IgnoredRegion;
use crate::registry;
use crate::registry::LanguageInfo;
use crate::suppressions as suppressions_mod;
use crate::suppressions::Suppression;
use crate::tsquery::{self, Capture};

fn parse(language_key: &str, source: &[u8]) -> PyResult<tree_sitter::Tree> {
    let language = registry::language_for_key(language_key)
        .ok_or_else(|| PyValueError::new_err(format!("unknown language key: {language_key}")))?;
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    parser
        .parse(source, None)
        .ok_or_else(|| PyValueError::new_err("tree-sitter failed to parse source"))
}

fn sloc_mode_for_key(language_key: &str) -> PyResult<registry::SlocMode> {
    // `tsx` is a grammar key (`language_for_key`) but not a `LanguageInfo` key:
    // the registry files `.tsx` under `typescript`, whose comment syntax it shares.
    let info_key = if language_key == "tsx" {
        "typescript"
    } else {
        language_key
    };
    registry::languages()
        .iter()
        .find(|l| l.key == info_key)
        .map(|l| l.sloc_mode)
        .ok_or_else(|| PyValueError::new_err(format!("unknown language key: {language_key}")))
}

#[pyclass(name = "LanguageInfo")]
#[derive(Clone)]
struct PyLanguageInfo {
    #[pyo3(get)]
    name: String,
    #[pyo3(get)]
    key: String,
    #[pyo3(get)]
    extensions: Vec<String>,
    #[pyo3(get)]
    explicit_only: Vec<String>,
}

impl From<&LanguageInfo> for PyLanguageInfo {
    fn from(info: &LanguageInfo) -> Self {
        Self {
            name: info.name.to_string(),
            key: info.key.to_string(),
            extensions: info.extensions.iter().map(|s| s.to_string()).collect(),
            explicit_only: info.explicit_only.iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[pyclass(name = "CallEdge")]
#[derive(Clone)]
struct PyCallEdge {
    #[pyo3(get)]
    caller: String,
    #[pyo3(get)]
    callee: String,
    #[pyo3(get)]
    is_external: bool,
}

impl From<CallEdge> for PyCallEdge {
    fn from(e: CallEdge) -> Self {
        Self {
            caller: e.caller,
            callee: e.callee,
            is_external: e.is_external,
        }
    }
}

#[pyclass(name = "BasicBlock")]
#[derive(Clone)]
struct PyBasicBlock {
    #[pyo3(get)]
    id: usize,
    #[pyo3(get)]
    statements: Vec<(usize, usize)>,
    #[pyo3(get)]
    byte_range: (usize, usize),
    #[pyo3(get)]
    condition_range: Option<(usize, usize)>,
}

impl From<&BasicBlock> for PyBasicBlock {
    fn from(b: &BasicBlock) -> Self {
        Self {
            id: b.id,
            statements: b.statements.clone(),
            byte_range: b.byte_range,
            condition_range: b.condition_range,
        }
    }
}

fn cfg_edge_name(edge: CfgEdge) -> &'static str {
    match edge {
        CfgEdge::Fallthrough => "fallthrough",
        CfgEdge::TrueBranch => "true_branch",
        CfgEdge::FalseBranch => "false_branch",
        CfgEdge::BackEdge => "back_edge",
        CfgEdge::Return => "return",
        CfgEdge::Break => "break",
        CfgEdge::Continue => "continue",
    }
}

#[pyclass(name = "FunctionCfg")]
#[derive(Clone)]
struct PyFunctionCfg {
    #[pyo3(get)]
    blocks: Vec<PyBasicBlock>,
    #[pyo3(get)]
    edges: Vec<(usize, usize, String)>,
    #[pyo3(get)]
    entry: usize,
    #[pyo3(get)]
    exits: Vec<usize>,
}

impl From<FunctionCfg> for PyFunctionCfg {
    fn from(cfg: FunctionCfg) -> Self {
        Self {
            blocks: cfg.blocks.iter().map(PyBasicBlock::from).collect(),
            edges: cfg
                .edges
                .iter()
                .map(|(from, to, e)| (*from, *to, cfg_edge_name(*e).to_string()))
                .collect(),
            entry: cfg.entry,
            exits: cfg.exits,
        }
    }
}

#[pyclass(name = "Fingerprint")]
#[derive(Clone)]
struct PyFingerprint {
    #[pyo3(get)]
    name: Option<String>,
    #[pyo3(get)]
    kind: String,
    #[pyo3(get)]
    hash: u64,
    #[pyo3(get)]
    node_count: usize,
    #[pyo3(get)]
    start_byte: usize,
    #[pyo3(get)]
    end_byte: usize,
    #[pyo3(get)]
    start_line: usize,
    #[pyo3(get)]
    end_line: usize,
}

impl From<Fingerprint> for PyFingerprint {
    fn from(f: Fingerprint) -> Self {
        Self {
            name: f.name,
            kind: f.kind.to_string(),
            hash: f.hash,
            node_count: f.node_count,
            start_byte: f.start_byte,
            end_byte: f.end_byte,
            start_line: f.start_line,
            end_line: f.end_line,
        }
    }
}

#[pyclass(name = "Suppression")]
#[derive(Clone)]
struct PySuppression {
    #[pyo3(get)]
    comment_line: usize,
    #[pyo3(get)]
    target_line: Option<usize>,
    #[pyo3(get)]
    tool: String,
    #[pyo3(get)]
    rule: String,
}

impl From<Suppression> for PySuppression {
    fn from(s: Suppression) -> Self {
        Self {
            comment_line: s.comment_line,
            target_line: s.target_line,
            tool: s.tool,
            rule: s.rule,
        }
    }
}

#[pyclass(name = "IgnoredRegion")]
#[derive(Clone)]
struct PyIgnoredRegion {
    #[pyo3(get)]
    byte_range: (usize, usize),
    #[pyo3(get)]
    line_range: (usize, usize),
    #[pyo3(get)]
    tools: Option<Vec<String>>,
}

impl From<IgnoredRegion> for PyIgnoredRegion {
    fn from(r: IgnoredRegion) -> Self {
        Self {
            byte_range: (r.byte_range.start, r.byte_range.end),
            line_range: (r.line_range.start, r.line_range.end),
            tools: r.tools,
        }
    }
}

/// Every node of a parsed file as parallel columns: see [`crate::flat`].
/// Each column getter returns native-endian `u32`s as `bytes`, for
/// `memoryview(col).cast("I")`; links use `NONE` (`0xFFFF_FFFF`).
///
/// `root_node` returns a [`PyNode`]: a py-tree-sitter-compatible node over the
/// same columns, implemented here so walking it costs what walking a
/// py-tree-sitter tree costs. A wrapper written in Python over the columns
/// measured 1.7-2.3x slower on clew's harvest.
#[pyclass(name = "FlatTree", frozen)]
struct PyFlatTree {
    inner: FlatTree,
    source: Py<PyBytes>,
    /// One interned Python string per kind, so `Node.type` allocates nothing.
    kind_strs: Vec<Py<PyString>>,
    /// Folded into `Node.id` so ids from two trees never collide.
    serial: u64,
}

static TREE_SERIAL: AtomicU64 = AtomicU64::new(1);

/// One node of a [`PyFlatTree`]. Answers the subset of py-tree-sitter's `Node`
/// API a tree walker needs: `type`, `children`, `named_children`,
/// `child_by_field_name`, `children_by_field_name`, `parent`, the sibling
/// links, byte and point positions, `text`, `id` and the error flags.
#[pyclass(name = "Node", frozen)]
struct PyNode {
    tree: Py<PyFlatTree>,
    row: u32,
}

impl PyNode {
    fn t(&self) -> &PyFlatTree {
        self.tree.get()
    }

    fn at(&self, py: Python<'_>, row: u32) -> Option<PyNode> {
        (row != flat::NONE).then(|| PyNode {
            tree: self.tree.clone_ref(py),
            row,
        })
    }

    fn i(&self) -> usize {
        self.row as usize
    }

    fn child_rows(&self) -> &[u32] {
        let f = &self.t().inner;
        let (lo, hi) = (f.child_offset[self.i()], f.child_offset[self.i() + 1]);
        &f.child_list[lo as usize..hi as usize]
    }

    fn flag(&self, bit: u32) -> bool {
        self.t().inner.flags[self.i()] & bit != 0
    }

    fn field_id(&self, name: &str) -> Option<u32> {
        let fields = &self.t().inner.fields;
        fields
            .iter()
            .skip(1)
            .position(|f| f == name)
            .map(|p| (p + 1) as u32)
    }

    fn step_named(&self, links: &[u32]) -> u32 {
        let flags = &self.t().inner.flags;
        let mut row = links[self.i()];
        while row != flat::NONE && flags[row as usize] & flat::FLAG_NAMED == 0 {
            row = links[row as usize];
        }
        row
    }
}

#[pymethods]
impl PyNode {
    #[getter(r#type)]
    fn kind(&self, py: Python<'_>) -> Py<PyString> {
        self.t().kind_strs[self.t().inner.kind[self.i()] as usize].clone_ref(py)
    }

    #[getter]
    fn id(&self) -> u64 {
        self.t().serial << 32 | u64::from(self.row)
    }

    #[getter]
    fn start_byte(&self) -> u32 {
        self.t().inner.start_byte[self.i()]
    }

    #[getter]
    fn end_byte(&self) -> u32 {
        self.t().inner.end_byte[self.i()]
    }

    #[getter]
    fn start_point(&self) -> (u32, u32) {
        let f = &self.t().inner;
        (f.start_row[self.i()], f.start_col[self.i()])
    }

    #[getter]
    fn end_point(&self) -> (u32, u32) {
        let f = &self.t().inner;
        (f.end_row[self.i()], f.end_col[self.i()])
    }

    #[getter]
    fn text<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        let source = self.t().source.as_bytes(py);
        let (s, e) = (self.start_byte() as usize, self.end_byte() as usize);
        PyBytes::new_bound(py, &source[s.min(source.len())..e.min(source.len())])
    }

    #[getter]
    fn is_named(&self) -> bool {
        self.flag(flat::FLAG_NAMED)
    }

    #[getter]
    fn is_error(&self) -> bool {
        self.flag(flat::FLAG_ERROR)
    }

    #[getter]
    fn is_missing(&self) -> bool {
        self.flag(flat::FLAG_MISSING)
    }

    #[getter]
    fn is_extra(&self) -> bool {
        self.flag(flat::FLAG_EXTRA)
    }

    #[getter]
    fn has_error(&self) -> bool {
        self.flag(flat::FLAG_HAS_ERROR)
    }

    #[getter]
    fn parent(&self, py: Python<'_>) -> Option<PyNode> {
        self.at(py, self.t().inner.parent[self.i()])
    }

    #[getter]
    fn prev_sibling(&self, py: Python<'_>) -> Option<PyNode> {
        self.at(py, self.t().inner.prev_sibling[self.i()])
    }

    #[getter]
    fn next_sibling(&self, py: Python<'_>) -> Option<PyNode> {
        self.at(py, self.t().inner.next_sibling[self.i()])
    }

    #[getter]
    fn prev_named_sibling(&self, py: Python<'_>) -> Option<PyNode> {
        self.at(py, self.step_named(&self.t().inner.prev_sibling))
    }

    #[getter]
    fn next_named_sibling(&self, py: Python<'_>) -> Option<PyNode> {
        self.at(py, self.step_named(&self.t().inner.next_sibling))
    }

    #[getter]
    fn children(&self, py: Python<'_>) -> Vec<PyNode> {
        self.child_rows()
            .iter()
            .filter_map(|&r| self.at(py, r))
            .collect()
    }

    #[getter]
    fn named_children(&self, py: Python<'_>) -> Vec<PyNode> {
        let flags = &self.t().inner.flags;
        self.child_rows()
            .iter()
            .filter(|&&r| flags[r as usize] & flat::FLAG_NAMED != 0)
            .filter_map(|&r| self.at(py, r))
            .collect()
    }

    #[getter]
    fn child_count(&self) -> usize {
        self.child_rows().len()
    }

    #[getter]
    fn named_child_count(&self) -> usize {
        let flags = &self.t().inner.flags;
        self.child_rows()
            .iter()
            .filter(|&&r| flags[r as usize] & flat::FLAG_NAMED != 0)
            .count()
    }

    fn child_by_field_name(&self, py: Python<'_>, name: &str) -> Option<PyNode> {
        let id = self.field_id(name)?;
        let field = &self.t().inner.field;
        let row = *self
            .child_rows()
            .iter()
            .find(|&&r| field[r as usize] == id)?;
        self.at(py, row)
    }

    fn children_by_field_name(&self, py: Python<'_>, name: &str) -> Vec<PyNode> {
        let Some(id) = self.field_id(name) else {
            return Vec::new();
        };
        let field = &self.t().inner.field;
        self.child_rows()
            .iter()
            .filter(|&&r| field[r as usize] == id)
            .filter_map(|&r| self.at(py, r))
            .collect()
    }

    fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
        other
            .downcast::<PyNode>()
            .is_ok_and(|o| o.get().row == self.row && o.get().tree.is(&self.tree))
    }

    fn __hash__(&self) -> u64 {
        self.id()
    }

    fn __repr__(&self, py: Python<'_>) -> String {
        let (s, e) = (self.start_point(), self.end_point());
        format!(
            "<Node type={}, start_point=({}, {}), end_point=({}, {})>",
            self.kind(py),
            s.0,
            s.1,
            e.0,
            e.1
        )
    }
}

fn column<'py>(py: Python<'py>, values: &[u32]) -> Bound<'py, PyBytes> {
    let mut bytes = Vec::with_capacity(values.len() * 4);
    for v in values {
        bytes.extend_from_slice(&v.to_ne_bytes());
    }
    PyBytes::new_bound(py, &bytes)
}

#[pymethods]
impl PyFlatTree {
    /// Number of nodes. Row 0 is the root.
    fn __len__(&self) -> usize {
        self.inner.len()
    }

    /// The root node (row 0).
    #[getter]
    fn root_node(slf: &Bound<'_, Self>) -> PyNode {
        PyNode {
            tree: slf.clone().unbind(),
            row: 0,
        }
    }

    /// The source bytes the tree was parsed from.
    #[getter]
    fn source(&self, py: Python<'_>) -> Py<PyBytes> {
        self.source.clone_ref(py)
    }

    /// Node-kind names, indexed by the `kind` column.
    #[getter]
    fn kinds(&self) -> Vec<String> {
        self.inner.kinds.clone()
    }

    /// Field names, indexed by the `field` column; index 0 is `""` (no field).
    #[getter]
    fn fields(&self) -> Vec<String> {
        self.inner.fields.clone()
    }

    #[getter]
    fn kind<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        column(py, &self.inner.kind)
    }

    #[getter]
    fn field<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        column(py, &self.inner.field)
    }

    /// `FLAG_*` bits: 1 named, 2 error, 4 missing, 8 extra, 16 has_error.
    #[getter]
    fn flags<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        column(py, &self.inner.flags)
    }

    #[getter]
    fn parent<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        column(py, &self.inner.parent)
    }

    #[getter]
    fn first_child<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        column(py, &self.inner.first_child)
    }

    #[getter]
    fn next_sibling<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        column(py, &self.inner.next_sibling)
    }

    #[getter]
    fn prev_sibling<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        column(py, &self.inner.prev_sibling)
    }

    #[getter]
    fn start_byte<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        column(py, &self.inner.start_byte)
    }

    #[getter]
    fn end_byte<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        column(py, &self.inner.end_byte)
    }

    #[getter]
    fn start_row<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        column(py, &self.inner.start_row)
    }

    #[getter]
    fn start_col<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        column(py, &self.inner.start_col)
    }

    #[getter]
    fn end_row<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        column(py, &self.inner.end_row)
    }

    #[getter]
    fn end_col<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        column(py, &self.inner.end_col)
    }

    /// `len + 1` offsets into `child_list`: row `i`'s children are
    /// `child_list[child_offset[i]:child_offset[i + 1]]`.
    #[getter]
    fn child_offset<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        column(py, &self.inner.child_offset)
    }

    #[getter]
    fn child_list<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        column(py, &self.inner.child_list)
    }
}

#[pyclass(name = "Capture")]
#[derive(Clone)]
struct PyCapture {
    #[pyo3(get)]
    pattern_index: usize,
    #[pyo3(get)]
    name: String,
    #[pyo3(get)]
    kind: &'static str,
    #[pyo3(get)]
    start_byte: usize,
    #[pyo3(get)]
    end_byte: usize,
    #[pyo3(get)]
    start_point: (usize, usize),
    #[pyo3(get)]
    end_point: (usize, usize),
}

impl From<Capture> for PyCapture {
    fn from(c: Capture) -> Self {
        Self {
            pattern_index: c.pattern_index,
            name: c.name,
            kind: c.kind,
            start_byte: c.start_byte,
            end_byte: c.end_byte,
            start_point: c.start_point,
            end_point: c.end_point,
        }
    }
}

/// Compiled-in languages, reflecting the Cargo features this wheel was built
/// with.
#[pyfunction]
fn languages() -> Vec<PyLanguageInfo> {
    registry::languages()
        .iter()
        .map(PyLanguageInfo::from)
        .collect()
}

/// Human-readable language summary, for `--supported-languages`-style flags.
#[pyfunction]
fn supported_languages_report() -> String {
    registry::supported_languages_report()
}

/// Call-graph edges for every named function/macro in `source`, parsed as
/// `language_key`.
#[pyfunction]
fn call_edges(language_key: &str, source: &str) -> PyResult<Vec<PyCallEdge>> {
    let tree = parse(language_key, source.as_bytes())?;
    Ok(calls::call_edges(tree.root_node(), source)
        .into_iter()
        .map(PyCallEdge::from)
        .collect())
}

/// Import/use-statement sources in `source`, for Ce/Ca coupling metrics.
#[pyfunction]
fn import_sources(language_key: &str, source: &[u8]) -> PyResult<Vec<String>> {
    let tree = parse(language_key, source)?;
    Ok(imports::import_sources(&tree, source, language_key))
}

/// Control-flow graph for the first function named `function_name` found in
/// `source`. Returns `None` if the language isn't modeled by `build_function_cfg`
/// (only `c`, `cpp`, `rust` today) or no matching function is found.
#[pyfunction]
fn function_cfg(
    language_key: &str,
    source: &str,
    function_name: &str,
) -> PyResult<Option<PyFunctionCfg>> {
    let tree = parse(language_key, source.as_bytes())?;
    let source_bytes = source.as_bytes();
    let target = crate::query::find_descendants(tree.root_node(), |n| {
        calls::is_function_kind(n.kind())
            && calls::get_function_name(n, source).as_deref() == Some(function_name)
    });
    Ok(target
        .into_iter()
        .find_map(|node| cfg::build_function_cfg(node, source_bytes, language_key))
        .map(PyFunctionCfg::from))
}

/// Structural fingerprints for every function-like subtree in `source`,
/// ignoring identifier/literal text (Type-2 clone detection).
#[pyfunction]
fn function_fingerprints(
    language_key: &str,
    source: &str,
    min_nodes: usize,
) -> PyResult<Vec<PyFingerprint>> {
    let tree = parse(language_key, source.as_bytes())?;
    Ok(
        fingerprint::function_fingerprints(tree.root_node(), source, min_nodes)
            .into_iter()
            .map(PyFingerprint::from)
            .collect(),
    )
}

/// `tools:suppress TOOL:RULE` single-line suppression comments in `source`.
#[pyfunction]
fn suppressions(language_key: &str, source: &str) -> PyResult<Vec<PySuppression>> {
    let sloc_mode = sloc_mode_for_key(language_key)?;
    Ok(suppressions_mod::suppressions(source, sloc_mode)
        .into_iter()
        .map(PySuppression::from)
        .collect())
}

/// `tools:off` / `tools:on` ignored regions in `source`.
#[pyfunction]
fn ignored_regions(language_key: &str, source: &str) -> PyResult<Vec<PyIgnoredRegion>> {
    let sloc_mode = sloc_mode_for_key(language_key)?;
    Ok(regions::ignored_regions(source, sloc_mode)
        .into_iter()
        .map(PyIgnoredRegion::from)
        .collect())
}

/// Parse `source` (bytes) as `language_key` and return every node as a
/// `FlatTree`, whose `root_node` walks like a py-tree-sitter tree. Accepts the
/// grammar keys of `language_for_key`, so `"tsx"` selects the JSX-aware
/// TypeScript grammar.
#[pyfunction]
fn parse_tree(
    py: Python<'_>,
    language_key: &str,
    source: Bound<'_, PyBytes>,
) -> PyResult<PyFlatTree> {
    let tree = parse(language_key, source.as_bytes())?;
    let inner = flat::flatten(&tree);
    let kind_strs = inner
        .kinds
        .iter()
        .map(|k| PyString::intern_bound(py, k).unbind())
        .collect();
    Ok(PyFlatTree {
        inner,
        source: source.unbind(),
        kind_strs,
        serial: TREE_SERIAL.fetch_add(1, Ordering::Relaxed),
    })
}

/// Run a tree-sitter query over `source` and return its captures in document
/// order. Byte offsets are into `source` encoded as UTF-8. Raises `ValueError`
/// when the query does not compile against the grammar.
#[pyfunction]
fn query(language_key: &str, source: &str, query: &str) -> PyResult<Vec<PyCapture>> {
    let language = registry::language_for_key(language_key)
        .ok_or_else(|| PyValueError::new_err(format!("unknown language key: {language_key}")))?;
    let tree = parse(language_key, source.as_bytes())?;
    tsquery::run_query(&language, tree.root_node(), source.as_bytes(), query)
        .map(|caps| caps.into_iter().map(PyCapture::from).collect())
        .map_err(|e| PyValueError::new_err(e.to_string()))
}

/// The grammar's bundled tags query (definitions and references), or `None`.
#[pyfunction]
fn tags_query(language_key: &str) -> Option<String> {
    tsquery::tags_query(language_key)
}

#[pymodule]
fn lang_parsing_substrate(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyLanguageInfo>()?;
    m.add_class::<PyCallEdge>()?;
    m.add_class::<PyBasicBlock>()?;
    m.add_class::<PyFunctionCfg>()?;
    m.add_class::<PyFingerprint>()?;
    m.add_class::<PySuppression>()?;
    m.add_class::<PyIgnoredRegion>()?;
    m.add_class::<PyFlatTree>()?;
    m.add_class::<PyNode>()?;
    m.add_class::<PyCapture>()?;
    m.add("NONE", flat::NONE)?;
    m.add_function(wrap_pyfunction!(languages, m)?)?;
    m.add_function(wrap_pyfunction!(supported_languages_report, m)?)?;
    m.add_function(wrap_pyfunction!(call_edges, m)?)?;
    m.add_function(wrap_pyfunction!(import_sources, m)?)?;
    m.add_function(wrap_pyfunction!(function_cfg, m)?)?;
    m.add_function(wrap_pyfunction!(function_fingerprints, m)?)?;
    m.add_function(wrap_pyfunction!(suppressions, m)?)?;
    m.add_function(wrap_pyfunction!(ignored_regions, m)?)?;
    m.add_function(wrap_pyfunction!(parse_tree, m)?)?;
    m.add_function(wrap_pyfunction!(query, m)?)?;
    m.add_function(wrap_pyfunction!(tags_query, m)?)?;
    Ok(())
}
