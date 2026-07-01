//! Syntactic import-source extraction.
//!
//! Walks a parsed tree and pulls out the raw text of every import / include /
//! use / require target, per language. This is deliberately shallow: no path
//! resolution, no distinguishing relative vs. absolute, no following `as`
//! aliases to their target — just "what did this file's syntax name as a
//! dependency." That's enough to build the Ce/Ca edge set the coupling
//! metrics need; resolving those names to actual files is a separate,
//! per-tool concern (this crate has no notion of a project's file layout).
//!
//! Dispatch is keyed on [`crate::registry::LanguageInfo::key`] rather than on
//! `tree_sitter::Language` identity, since the latter has no stable way to
//! ask "which grammar is this."

use std::collections::HashSet;
use tree_sitter::{Node, Tree};

/// Returns the raw text of every import-source found in `tree`, in
/// depth-first order, one entry per import site (a statement that imports
/// multiple names, e.g. `import a, b` in Python or `use a::{b, c}` in Rust,
/// contributes one entry per name or one entry for the whole group,
/// depending on what the grammar exposes as a distinct node — see
/// per-language notes inline below).
///
/// Entries are not deduplicated — callers building a Ce/Ca edge set should
/// dedupe per file (multiple `import os` statements are one dependency, not
/// two). Returns an empty vec for a `key` this module doesn't recognize.
pub fn import_sources(tree: &Tree, source: &[u8], key: &str) -> Vec<String> {
    let mut out = Vec::new();
    walk(tree.root_node(), source, key, &mut out);
    out
}

/// Convenience wrapper: the distinct import-source count for `tree`, i.e.
/// `import_sources(..).len()` after deduplication. This is the Ce
/// (efferent coupling) contribution of a single file.
pub fn distinct_import_count(tree: &Tree, source: &[u8], key: &str) -> usize {
    import_sources(tree, source, key)
        .into_iter()
        .collect::<HashSet<_>>()
        .len()
}

fn walk(node: Node, source: &[u8], key: &str, out: &mut Vec<String>) {
    extract(node, source, key, out);
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, source, key, out);
    }
}

fn node_text<'a>(node: Node, source: &'a [u8]) -> &'a str {
    node.utf8_text(source).unwrap_or("").trim()
}

/// Strips a single layer of quoting/bracketing from a source-literal token:
/// `"foo"`, `'foo'`, `<foo>` all become `foo`. Leaves plain identifiers
/// untouched.
fn unquote(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        let is_wrapped = matches!((first, last), (b'"', b'"') | (b'\'', b'\'') | (b'<', b'>'));
        if is_wrapped {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

fn extract(node: Node, source: &[u8], key: &str, out: &mut Vec<String>) {
    match key {
        "c" | "cpp" => extract_preproc_include(node, source, out),
        "rust" => extract_rust(node, source, out),
        "python" => extract_python(node, source, out),
        "javascript" | "typescript" => extract_js_like(node, source, out),
        "go" => extract_go(node, source, out),
        "java" => extract_java(node, source, out),
        "csharp" => extract_csharp(node, source, out),
        "kotlin" => extract_kotlin(node, source, out),
        "swift" => extract_swift(node, source, out),
        "php" => extract_php(node, source, out),
        "fortran" => extract_fortran(node, source, out),
        "scala" => extract_scala(node, source, out),
        "lua" => extract_lua(node, source, out),
        // Ada's `use_clause` re-references an already-`with`ed package rather
        // than naming a new dependency, so only `with_clause` is extracted.
        "ada" => extract_ada(node, source, out),
        _ => {}
    }
}

fn extract_preproc_include(node: Node, source: &[u8], out: &mut Vec<String>) {
    if node.kind() == "preproc_include" {
        if let Some(path) = node.child_by_field_name("path") {
            out.push(unquote(node_text(path, source)));
        }
    }
}

fn extract_rust(node: Node, source: &[u8], out: &mut Vec<String>) {
    if node.kind() == "use_declaration" {
        if let Some(arg) = node.child_by_field_name("argument") {
            out.push(node_text(arg, source).to_string());
        }
    }
}

fn extract_python(node: Node, source: &[u8], out: &mut Vec<String>) {
    match node.kind() {
        "import_statement" => {
            let mut cursor = node.walk();
            for child in node.children_by_field_name("name", &mut cursor) {
                out.push(python_import_name(child, source));
            }
        }
        "import_from_statement" => {
            if let Some(module) = node.child_by_field_name("module_name") {
                out.push(node_text(module, source).to_string());
            }
        }
        _ => {}
    }
}

fn python_import_name(node: Node, source: &[u8]) -> String {
    if node.kind() == "aliased_import" {
        if let Some(name) = node.child_by_field_name("name") {
            return node_text(name, source).to_string();
        }
    }
    node_text(node, source).to_string()
}

fn extract_js_like(node: Node, source: &[u8], out: &mut Vec<String>) {
    if matches!(node.kind(), "import_statement" | "import_require_clause") {
        if let Some(src) = node.child_by_field_name("source") {
            out.push(unquote(node_text(src, source)));
        }
    }
}

fn extract_go(node: Node, source: &[u8], out: &mut Vec<String>) {
    if node.kind() == "import_spec" {
        if let Some(path) = node.child_by_field_name("path") {
            out.push(unquote(node_text(path, source)));
        }
    }
}

fn extract_java(node: Node, source: &[u8], out: &mut Vec<String>) {
    if node.kind() == "import_declaration" {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if matches!(child.kind(), "identifier" | "scoped_identifier") {
                out.push(node_text(child, source).to_string());
            }
        }
    }
}

fn extract_csharp(node: Node, source: &[u8], out: &mut Vec<String>) {
    if node.kind() == "using_directive" {
        // `name` is only populated for the alias in `using Alias = X;`; the
        // plain `using System.Text;` form is a bare positional child.
        let target = node
            .child_by_field_name("name")
            .or_else(|| node.named_child(0));
        if let Some(target) = target {
            out.push(node_text(target, source).to_string());
        }
    }
}

fn extract_kotlin(node: Node, source: &[u8], out: &mut Vec<String>) {
    if node.kind() == "import" {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if matches!(child.kind(), "identifier" | "qualified_identifier") {
                out.push(node_text(child, source).to_string());
                break;
            }
        }
    }
}

fn extract_swift(node: Node, source: &[u8], out: &mut Vec<String>) {
    if node.kind() == "import_declaration" {
        let mut cursor = node.walk();
        let parts: Vec<&str> = node
            .named_children(&mut cursor)
            .filter(|c| c.kind() == "identifier")
            .map(|c| node_text(c, source))
            .collect();
        if !parts.is_empty() {
            out.push(parts.join("."));
        }
    }
}

fn extract_php(node: Node, source: &[u8], out: &mut Vec<String>) {
    match node.kind() {
        "namespace_use_declaration" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if matches!(child.kind(), "namespace_name" | "qualified_name" | "name") {
                    out.push(node_text(child, source).to_string());
                }
            }
        }
        "namespace_use_clause" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if matches!(child.kind(), "name" | "qualified_name") {
                    out.push(node_text(child, source).to_string());
                    break;
                }
            }
        }
        "include_expression"
        | "include_once_expression"
        | "require_expression"
        | "require_once_expression" => {
            if let Some(expr) = node.named_child(0) {
                out.push(unquote(node_text(expr, source)));
            }
        }
        _ => {}
    }
}

fn extract_fortran(node: Node, source: &[u8], out: &mut Vec<String>) {
    match node.kind() {
        "use_statement" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "module_name" {
                    out.push(node_text(child, source).to_string());
                    break;
                }
            }
        }
        "include_statement" => {
            if let Some(path) = node.child_by_field_name("path") {
                out.push(unquote(node_text(path, source)));
            }
        }
        _ => {}
    }
}

fn extract_scala(node: Node, source: &[u8], out: &mut Vec<String>) {
    if node.kind() == "import_declaration" {
        // Each dotted segment (`scala`, `collection`, ...) is a separate
        // `path`-field child rather than one node spanning the whole path.
        let mut cursor = node.walk();
        let segments: Vec<&str> = node
            .children_by_field_name("path", &mut cursor)
            .filter(|c| c.is_named())
            .map(|c| node_text(c, source))
            .collect();
        if !segments.is_empty() {
            out.push(segments.join("."));
        }
    }
}

fn extract_lua(node: Node, source: &[u8], out: &mut Vec<String>) {
    if node.kind() != "function_call" {
        return;
    }
    let Some(name) = node.child_by_field_name("name") else {
        return;
    };
    if node_text(name, source) != "require" {
        return;
    }
    let Some(args) = node.child_by_field_name("arguments") else {
        return;
    };
    if let Some(first) = args.named_child(0) {
        out.push(unquote(node_text(first, source)));
    }
}

fn extract_ada(node: Node, source: &[u8], out: &mut Vec<String>) {
    if node.kind() == "with_clause" {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if matches!(child.kind(), "identifier" | "selected_component") {
                out.push(node_text(child, source).to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::language_for_key;
    use tree_sitter::Parser;

    fn sources(key: &str, code: &str) -> Vec<String> {
        let language = language_for_key(key).unwrap_or_else(|| panic!("no grammar for {key}"));
        let mut parser = Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(code, None).unwrap();
        import_sources(&tree, code.as_bytes(), key)
    }

    #[cfg(feature = "lang-c")]
    #[test]
    fn c_include() {
        let code = "#include <stdio.h>\n#include \"local.h\"\nint main() {}\n";
        assert_eq!(sources("c", code), vec!["stdio.h", "local.h"]);
    }

    #[cfg(feature = "lang-cpp")]
    #[test]
    fn cpp_include() {
        let code = "#include <vector>\n#include \"foo.hpp\"\n";
        assert_eq!(sources("cpp", code), vec!["vector", "foo.hpp"]);
    }

    #[cfg(feature = "lang-rust")]
    #[test]
    fn rust_use() {
        let code = "use std::collections::HashMap;\nuse crate::foo::{bar, baz};\n";
        assert_eq!(
            sources("rust", code),
            vec!["std::collections::HashMap", "crate::foo::{bar, baz}"]
        );
    }

    #[cfg(feature = "lang-python")]
    #[test]
    fn python_import_forms() {
        let code = "import os, sys\nfrom collections import OrderedDict\nimport numpy as np\n";
        assert_eq!(
            sources("python", code),
            vec!["os", "sys", "collections", "numpy"]
        );
    }

    #[cfg(feature = "lang-javascript")]
    #[test]
    fn javascript_import() {
        let code = "import foo from \"foo\";\nimport { a, b } from './bar.js';\n";
        assert_eq!(sources("javascript", code), vec!["foo", "./bar.js"]);
    }

    #[cfg(feature = "lang-typescript")]
    #[test]
    fn typescript_import_and_require() {
        let code = "import { A } from \"a\";\nimport x = require(\"b\");\n";
        assert_eq!(sources("typescript", code), vec!["a", "b"]);
    }

    #[cfg(feature = "lang-go")]
    #[test]
    fn go_import_spec() {
        let code = "package main\nimport (\n\t\"fmt\"\n\t_ \"os\"\n)\n";
        assert_eq!(sources("go", code), vec!["fmt", "os"]);
    }

    #[cfg(feature = "lang-java")]
    #[test]
    fn java_import_declaration() {
        let code = "import java.util.List;\nimport static java.lang.Math.*;\n";
        assert_eq!(
            sources("java", code),
            vec!["java.util.List", "java.lang.Math"]
        );
    }

    #[cfg(feature = "lang-csharp")]
    #[test]
    fn csharp_using_directive() {
        let code = "using System;\nusing System.Collections.Generic;\n";
        assert_eq!(
            sources("csharp", code),
            vec!["System", "System.Collections.Generic"]
        );
    }

    #[cfg(feature = "lang-kotlin")]
    #[test]
    fn kotlin_import() {
        let code = "import kotlin.collections.List\n";
        assert_eq!(sources("kotlin", code), vec!["kotlin.collections.List"]);
    }

    #[cfg(feature = "lang-swift")]
    #[test]
    fn swift_import() {
        let code = "import Foundation\n";
        assert_eq!(sources("swift", code), vec!["Foundation"]);
    }

    #[cfg(feature = "lang-php")]
    #[test]
    fn php_use_and_require() {
        let code = "<?php\nuse Foo\\Bar;\nrequire_once 'vendor/autoload.php';\n";
        assert_eq!(
            sources("php", code),
            vec!["Foo\\Bar", "vendor/autoload.php"]
        );
    }

    #[cfg(feature = "lang-fortran")]
    #[test]
    fn fortran_use_statement() {
        let code = "program p\n  use iso_fortran_env\nend program p\n";
        assert_eq!(sources("fortran", code), vec!["iso_fortran_env"]);
    }

    #[cfg(feature = "lang-scala")]
    #[test]
    fn scala_import_declaration() {
        let code = "import scala.collection.mutable.ListBuffer\n";
        assert_eq!(
            sources("scala", code),
            vec!["scala.collection.mutable.ListBuffer"]
        );
    }

    #[cfg(feature = "lang-lua")]
    #[test]
    fn lua_require_call() {
        let code = "local m = require(\"module.name\")\n";
        assert_eq!(sources("lua", code), vec!["module.name"]);
    }

    #[cfg(feature = "lang-ada")]
    #[test]
    fn ada_with_clause() {
        let code =
            "with Ada.Text_IO, Ada.Integer_Text_IO;\nprocedure P is\nbegin\n  null;\nend P;\n";
        assert_eq!(
            sources("ada", code),
            vec!["Ada.Text_IO", "Ada.Integer_Text_IO"]
        );
    }

    #[cfg(feature = "lang-rust")]
    #[test]
    fn unrecognized_key_yields_nothing() {
        let code = "use std::fmt;\n";
        let language = language_for_key("rust").unwrap();
        let mut parser = Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(code, None).unwrap();
        assert!(import_sources(&tree, code.as_bytes(), "not-a-real-language-key").is_empty());
    }

    #[cfg(feature = "lang-python")]
    #[test]
    fn distinct_count_dedupes() {
        let code = "import os\nimport os\nimport sys\n";
        let language = language_for_key("python").unwrap();
        let mut parser = Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(code, None).unwrap();
        assert_eq!(distinct_import_count(&tree, code.as_bytes(), "python"), 2);
    }
}
