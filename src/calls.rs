//! Call-graph edge extraction.
//!
//! Ported from knots' `external_calls` metric (function/macro-name collection
//! plus the call-node-kind vocabulary in `handle_call_node` /
//! `collect_external_calls_recursive`), generalized to record every call
//! target rather than only the ones that fall outside the file's own
//! function/macro names. Unlike [`crate::imports`], this dispatch is *not*
//! keyed by language: the node kinds involved (`call_expression`, `call`,
//! `method_invocation`, ...) are shared across enough grammars that a single
//! generic walk covers all 16 languages without a per-language table.
//!
//! No cross-file resolution happens here — `is_external` only means "not one
//! of the names this file itself defines." Turning that into an actual
//! cross-file call graph (deciding which external callee belongs to which
//! other file) is a per-tool concern, same as `import_sources`.

use std::collections::HashSet;
use tree_sitter::Node;

/// One call site: `caller` (a function/macro name defined in this file)
/// invoked `callee`. `is_external` is true when `callee` is not among the
/// function/macro names this file defines — i.e. it's presumably resolved
/// elsewhere (another file, a library, a language builtin).
///
/// Only named functions become callers; anonymous closures/lambdas are
/// skipped as call *sites* (matching knots' default, non-anonymous-counting
/// behavior), though calls made from inside them are still attributed to
/// their nearest enclosing named function, since the underlying traversal
/// does not stop at nested-function boundaries — the same behavior as the
/// existing `external_calls` metric it extends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallEdge {
    pub caller: String,
    pub callee: String,
    pub is_external: bool,
}

/// Builds call-graph edges for every named function/macro in `tree`.
///
/// Building a corpus-wide graph is left to the caller: concatenate
/// `call_edges` results across every file's tree. This function only sees
/// one file, so `is_external` can't distinguish "external to this file, but
/// defined in another file of the same project" from "genuinely external" —
/// that classification needs the whole corpus's local-name sets, which is
/// the caller's job.
pub fn call_edges(root: Node, source: &str) -> Vec<CallEdge> {
    let local_names = collect_local_names(root, source);
    let mut functions = Vec::new();
    collect_functions(root, source, &mut functions);

    let mut edges = Vec::new();
    for (node, caller) in functions {
        let mut callees = Vec::new();
        collect_call_names(node, source, &mut callees);
        for callee in callees {
            let is_external = !local_names.contains(&callee);
            edges.push(CallEdge {
                caller: caller.clone(),
                callee,
                is_external,
            });
        }
    }
    edges
}

fn collect_functions<'a>(node: Node<'a>, source: &str, out: &mut Vec<(Node<'a>, String)>) {
    if is_function_kind(node.kind()) && !is_macro_function_definition(node) {
        if let Some(name) = get_function_name(node, source) {
            out.push((node, name));
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_functions(child, source, out);
    }
}

/// Returns `true` if `kind` is a function-like node this module treats as a
/// potential call-graph caller.
pub fn is_function_kind(kind: &str) -> bool {
    matches!(
        kind,
        "function_definition"
            | "function_item"
            | "function_declaration"
            | "function_expression"
            | "arrow_function"
            | "method_definition"
            | "generator_function_declaration"
            | "generator_function"
            | "subprogram_body"
            | "expression_function_declaration"
            | "task_body"
            | "method_declaration"
            | "func_literal"
            | "constructor_declaration"
            | "local_function_statement"
            | "init_declaration"
            // Fortran: function subprogram, subroutine subprogram, module procedure, main program
            | "function"
            | "subroutine"
            | "module_procedure"
            | "program"
    )
}

fn is_macro_function_definition(node: Node) -> bool {
    node.kind() == "function_definition"
        && node
            .child_by_field_name("declarator")
            .map(|d| d.kind() == "parenthesized_declarator")
            .unwrap_or(false)
}

/// Resolves the name of a function-like node, or `None` for genuinely
/// anonymous usage (callbacks, IIFEs, return values, etc.) that can't be
/// traced back to a binding.
pub fn get_function_name(node: Node, source: &str) -> Option<String> {
    match node.kind() {
        "function_item"
        | "method_definition"
        | "generator_function_declaration"
        | "generator_function"
        | "method_declaration"
        | "constructor_declaration"
        | "local_function_statement" => name_field(node, source),

        "function_declaration" => name_field(node, source).or_else(|| {
            let mut cursor = node.walk();
            let found = node
                .children(&mut cursor)
                .find(|c| c.kind() == "simple_identifier");
            found
                .and_then(|c| c.utf8_text(source.as_bytes()).ok())
                .map(|s| s.to_string())
        }),

        "function_definition" => name_field(node, source)
            .or_else(|| get_c_name(node, source))
            .or_else(|| get_lua_assignment_name(node, source)),

        "function_expression" => {
            name_field(node, source).or_else(|| get_name_from_assignment_context(node, source))
        }

        "arrow_function" => get_name_from_assignment_context(node, source),

        "init_declaration" => Some("init".to_string()),

        "func_literal" => None,

        "subprogram_body" | "expression_function_declaration" => {
            let mut cursor = node.walk();
            let spec = node.children(&mut cursor).find(|c| {
                matches!(
                    c.kind(),
                    "function_specification" | "procedure_specification"
                )
            })?;
            name_field(spec, source)
        }

        "task_body" => {
            let mut cursor = node.walk();
            let found = node
                .children(&mut cursor)
                .find(|c| c.kind() == "identifier");
            found
                .and_then(|c| c.utf8_text(source.as_bytes()).ok())
                .map(|s| s.to_string())
        }

        "function" => name_in_child(node, "function_statement", source),
        "subroutine" => name_in_child(node, "subroutine_statement", source),
        "module_procedure" => name_in_child(node, "module_procedure_statement", source),

        "program" => {
            let mut cursor = node.walk();
            let stmt = node
                .children(&mut cursor)
                .find(|c| c.kind() == "program_statement")?;
            let mut inner = stmt.walk();
            let first_named = stmt.named_children(&mut inner).next();
            first_named
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                .map(|s| s.to_string())
                .or_else(|| Some("program".to_string()))
        }

        _ => get_c_name(node, source),
    }
}

fn name_field(node: Node, source: &str) -> Option<String> {
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())
        .map(|s| s.to_string())
}

fn name_in_child(node: Node, child_kind: &str, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let child = node
        .children(&mut cursor)
        .find(|c| c.kind() == child_kind)?;
    name_field(child, source)
}

fn get_c_name(node: Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "function_declarator" {
            return get_declarator_name(child, source);
        }
        if child.kind() == "pointer_declarator" {
            if let Some(name) = get_function_name_from_declarator(child, source) {
                return Some(name);
            }
        }
    }
    None
}

fn get_function_name_from_declarator(node: Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "function_declarator" {
            return get_declarator_name(child, source);
        } else if child.kind() == "pointer_declarator" {
            if let Some(name) = get_function_name_from_declarator(child, source) {
                return Some(name);
            }
        }
    }
    None
}

fn get_declarator_name(node: Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier"
            | "qualified_identifier"
            | "destructor_name"
            | "operator_name"
            | "field_identifier" => {
                return Some(child.utf8_text(source.as_bytes()).ok()?.to_string());
            }
            "pointer_declarator" | "function_declarator" => {
                if let Some(name) = get_declarator_name(child, source) {
                    return Some(name);
                }
            }
            _ => {}
        }
    }
    None
}

/// Extract the name of a Lua anonymous function_definition from its
/// assignment context.
fn get_lua_assignment_name(node: Node, source: &str) -> Option<String> {
    let parent = node.parent()?;
    match parent.kind() {
        "field" => {
            let mut cur = parent.walk();
            let found = parent
                .named_children(&mut cur)
                .find(|c| c.kind() == "identifier");
            found
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                .map(|s| s.to_string())
        }
        "expression_list" => {
            let idx = {
                let mut cur = parent.walk();
                let pos = parent
                    .named_children(&mut cur)
                    .position(|c| c.id() == node.id())
                    .unwrap_or(0);
                pos
            };
            let assign = parent.parent()?;
            if assign.kind() != "assignment_statement" {
                return None;
            }
            let mut cur = assign.walk();
            let found = assign
                .children(&mut cur)
                .find(|c| c.kind() == "variable_list");
            let var_list = found?;
            let mut cur2 = var_list.walk();
            let var = var_list.named_children(&mut cur2).nth(idx)?;
            if var.kind() != "identifier" {
                return None;
            }
            var.utf8_text(source.as_bytes()).ok().map(|s| s.to_string())
        }
        _ => None,
    }
}

/// Extract the name of an arrow_function or anonymous function_expression
/// from the surrounding assignment context. Returns None for truly
/// anonymous usage (callbacks, IIFEs, return values, etc.).
fn get_name_from_assignment_context(node: Node, source: &str) -> Option<String> {
    let parent = node.parent()?;
    match parent.kind() {
        // const foo = () => {}  or  const foo = function() {}
        "variable_declarator" => parent
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .map(|s| s.to_string()),
        // { foo: () => {} }
        "pair" => parent
            .child_by_field_name("key")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .map(|s| s.to_string()),
        // class { foo = () => {} }
        "public_field_definition" => parent
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .map(|s| s.to_string()),
        _ => None,
    }
}

/// Collects all function and macro names defined in this translation unit.
/// Used to classify call sites as local vs. external.
pub fn collect_local_names(root: Node, source: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    collect_local_names_recursive(root, source, &mut names);
    names
}

fn collect_local_names_recursive(node: Node, source: &str, names: &mut HashSet<String>) {
    if is_function_kind(node.kind()) {
        if let Some(name) = get_function_name(node, source) {
            names.insert(name);
        }
    } else if matches!(node.kind(), "preproc_def" | "preproc_function_def") {
        if let Some(name_node) = node.child_by_field_name("name") {
            if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                names.insert(name.to_string());
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_local_names_recursive(child, source, names);
    }
}

fn handle_call_node(node: Node, source: &str, out: &mut Vec<String>) {
    let Some(func_node) = node.child_by_field_name("function") else {
        return;
    };
    match func_node.kind() {
        "identifier" => {
            if let Ok(name) = func_node.utf8_text(source.as_bytes()) {
                out.push(name.to_string());
            }
        }
        "scoped_identifier"
        | "attribute"
        | "member_expression"
        | "selector_expression"
        | "member_access_expression" => {
            if let Ok(name) = func_node.utf8_text(source.as_bytes()) {
                out.push(name.to_string());
            }
        }
        "field_expression" => {
            if let Some(field) = func_node.child_by_field_name("field") {
                if let Ok(method_name) = field.utf8_text(source.as_bytes()) {
                    out.push(method_name.to_string());
                }
            }
        }
        _ => {}
    }
}

/// Collects every call target name reachable from `node`, recursing through
/// nested function bodies (a nested closure's calls are attributed to every
/// enclosing named function, not just its own) — matching the traversal the
/// `external_calls` metric already uses.
fn collect_call_names(node: Node, source: &str, out: &mut Vec<String>) {
    if node.kind() == "call_expression"
        || node.kind() == "call"
        || node.kind() == "invocation_expression"
    {
        handle_call_node(node, source, out);
    }
    if node.kind() == "procedure_call_statement" || node.kind() == "function_call" {
        if let Some(name_node) = node.child_by_field_name("name") {
            if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                out.push(name.to_string());
            }
        }
    }
    if node.kind() == "method_invocation" {
        if let Some(name_node) = node.child_by_field_name("name") {
            if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                out.push(name.to_string());
            }
        }
    }
    if node.kind() == "call_expression" && node.child_by_field_name("function").is_none() {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if !child.is_named() {
                continue;
            }
            if matches!(
                child.kind(),
                "value_arguments" | "type_arguments" | "annotated_lambda" | "call_suffix"
            ) {
                continue;
            }
            let text = child.utf8_text(source.as_bytes()).unwrap_or("").to_string();
            out.push(text);
            break;
        }
    }
    if node.kind() == "object_creation_expression" {
        if let Some(type_node) = node.child_by_field_name("type") {
            if let Ok(name) = type_node.utf8_text(source.as_bytes()) {
                out.push(name.to_string());
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_call_names(child, source, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::language_for_key;
    use tree_sitter::Parser;

    fn edges(key: &str, code: &str) -> Vec<CallEdge> {
        let language = language_for_key(key).unwrap_or_else(|| panic!("no grammar for {key}"));
        let mut parser = Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(code, None).unwrap();
        call_edges(tree.root_node(), code)
    }

    #[cfg(feature = "lang-rust")]
    #[test]
    fn rust_local_and_external_calls() {
        let code =
            "fn helper() {}\nfn main() { helper(); println!(\"hi\"); std::fs::read(\"x\"); }\n";
        let e = edges("rust", code);
        assert!(e.contains(&CallEdge {
            caller: "main".into(),
            callee: "helper".into(),
            is_external: false,
        }));
        assert!(e
            .iter()
            .any(|c| c.caller == "main" && c.callee == "std::fs::read" && c.is_external));
    }

    #[cfg(feature = "lang-python")]
    #[test]
    fn python_calls() {
        let code = "def helper():\n    pass\n\ndef main():\n    helper()\n    os.path.join('a')\n";
        let e = edges("python", code);
        assert!(e.contains(&CallEdge {
            caller: "main".into(),
            callee: "helper".into(),
            is_external: false,
        }));
        assert!(e
            .iter()
            .any(|c| c.caller == "main" && c.callee == "os.path.join" && c.is_external));
    }

    #[cfg(feature = "lang-go")]
    #[test]
    fn go_calls() {
        let code =
            "package main\nfunc helper() {}\nfunc main() {\n\thelper()\n\tfmt.Println(\"hi\")\n}\n";
        let e = edges("go", code);
        assert!(e.contains(&CallEdge {
            caller: "main".into(),
            callee: "helper".into(),
            is_external: false,
        }));
        assert!(e
            .iter()
            .any(|c| c.caller == "main" && c.callee == "fmt.Println" && c.is_external));
    }

    #[cfg(feature = "lang-java")]
    #[test]
    fn java_method_invocation() {
        let code = "class A {\n  void helper() {}\n  void main() {\n    helper();\n    System.out.println(\"hi\");\n  }\n}\n";
        let e = edges("java", code);
        assert!(e.contains(&CallEdge {
            caller: "main".into(),
            callee: "helper".into(),
            is_external: false,
        }));
        assert!(e
            .iter()
            .any(|c| c.caller == "main" && c.callee == "println" && c.is_external));
    }

    #[cfg(feature = "lang-c")]
    #[test]
    fn c_calls() {
        let code =
            "int helper() { return 1; }\nint main() { helper(); printf(\"hi\"); return 0; }\n";
        let e = edges("c", code);
        assert!(e.contains(&CallEdge {
            caller: "main".into(),
            callee: "helper".into(),
            is_external: false,
        }));
        assert!(e
            .iter()
            .any(|c| c.caller == "main" && c.callee == "printf" && c.is_external));
    }

    #[cfg(feature = "lang-rust")]
    #[test]
    fn no_functions_yields_no_edges() {
        assert!(edges("rust", "const X: i32 = 1;\n").is_empty());
    }
}
