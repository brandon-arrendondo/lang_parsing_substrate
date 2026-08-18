//! PyO3 bindings, feature-gated behind `pyo3`. Every public function here
//! owns its parse: Python cannot hand in a `tree_sitter::Node`/`Tree` (this
//! crate's `tree-sitter` version has no ABI relationship to tree-sitter's own
//! separate Python bindings), so each wrapper takes `(language_key, source)`,
//! parses internally, walks the tree, and returns only owned data.
//!
//! Consumers that need the tree itself for their own semantic walks (e.g.
//! clew's thread/lock harvesting) still parse with a language-specific
//! tree-sitter Python package directly — this module only exposes the
//! substrate's language-agnostic analysis primitives.
//!
//! `useless_conversion` is allowed crate-wide-in-this-module: pyo3 0.22's
//! `#[pyfunction]` expansion applies a `?`/`From<PyErr>` conversion clippy
//! flags as a no-op on functions that already return `PyResult` — a
//! macro-generated false positive, not something callable here can fix.
#![allow(clippy::useless_conversion)]

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use tree_sitter::Parser;

use crate::calls;
use crate::calls::CallEdge;
use crate::cfg;
use crate::cfg::{BasicBlock, CfgEdge, FunctionCfg};
use crate::fingerprint;
use crate::fingerprint::Fingerprint;
use crate::imports;
use crate::regions;
use crate::regions::IgnoredRegion;
use crate::registry;
use crate::registry::LanguageInfo;
use crate::suppressions as suppressions_mod;
use crate::suppressions::Suppression;

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
    registry::languages()
        .iter()
        .find(|l| l.key == language_key)
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

#[pymodule]
fn lang_parsing_substrate(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyLanguageInfo>()?;
    m.add_class::<PyCallEdge>()?;
    m.add_class::<PyBasicBlock>()?;
    m.add_class::<PyFunctionCfg>()?;
    m.add_class::<PyFingerprint>()?;
    m.add_class::<PySuppression>()?;
    m.add_class::<PyIgnoredRegion>()?;
    m.add_function(wrap_pyfunction!(languages, m)?)?;
    m.add_function(wrap_pyfunction!(supported_languages_report, m)?)?;
    m.add_function(wrap_pyfunction!(call_edges, m)?)?;
    m.add_function(wrap_pyfunction!(import_sources, m)?)?;
    m.add_function(wrap_pyfunction!(function_cfg, m)?)?;
    m.add_function(wrap_pyfunction!(function_fingerprints, m)?)?;
    m.add_function(wrap_pyfunction!(suppressions, m)?)?;
    m.add_function(wrap_pyfunction!(ignored_regions, m)?)?;
    Ok(())
}
