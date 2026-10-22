//! Parses one minimal, syntactically valid snippet per compiled-in language
//! through `language_for_file` + a real `tree_sitter::Parser`. Unit tests in
//! `registry.rs` only check extension-to-grammar dispatch; this catches
//! grammar/API mismatches (e.g. a tree-sitter-* upgrade changing its node
//! kinds or `LANGUAGE` constant) that dispatch tests can't see.

use std::path::Path;

fn snippet_for(key: &str) -> &'static str {
    match key {
        "c" => "int main(void) { return 0; }\n",
        "cpp" => "int main() { return 0; }\n",
        "rust" => "fn main() {}\n",
        "python" => "def main():\n    pass\n",
        "javascript" => "function main() {}\n",
        "typescript" => "function main(): void {}\n",
        "ada" => "procedure Main is\nbegin\n   null;\nend Main;\n",
        "go" => "package main\n\nfunc main() {}\n",
        "java" => "class Main { void main() {} }\n",
        "csharp" => "class Program { static void Main() {} }\n",
        "kotlin" => "fun main() {}\n",
        "swift" => "func main() {}\n",
        "php" => "<?php\nfunction main() {}\n",
        "fortran" => "program main\nend program main\n",
        "scala" => "object Main { def main(args: Array[String]): Unit = {} }\n",
        "lua" => "function main() end\n",
        "yaml" => "name: main\nitems: [a, b]\n",
        "json" => "{\"name\": \"main\", \"items\": [1, 2]}\n",
        "markdown" => "# Main\n\nSome *text* and a [link](other.md).\n",
        "bash" => "#!/bin/sh\nmain() { echo \"$1\"; }\nmain \"$@\"\n",
        "cmake" => "cmake_minimum_required(VERSION 3.20)\nadd_library(main main.c)\n",
        "rst" => "Main\n====\n\nSome *text* and a paragraph.\n",
        other => panic!("no smoke-test snippet registered for language key {other:?}"),
    }
}

#[test]
fn every_compiled_in_language_parses_a_valid_snippet() {
    for lang in lang_parsing_substrate::languages() {
        let ext = lang.extensions[0];
        let path = Path::new("smoke").with_extension(ext);
        assert_parses_cleanly(lang.name, &path, snippet_for(lang.key));
    }
}

/// `markdown_inline` and `CMakeLists.txt` are reached through `language_for_key`
/// and a file name, not through a registered extension, so the loop above
/// doesn't see them.
#[cfg(feature = "lang-markdown")]
#[test]
fn markdown_inline_grammar_parses_inline_text() {
    let language = lang_parsing_substrate::language_for_key("markdown_inline").unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).unwrap();
    let tree = parser
        .parse("Some *text* and a [link](other.md).", None)
        .unwrap();
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    assert!(root.to_sexp().contains("inline_link"), "{}", root.to_sexp());
}

#[cfg(feature = "lang-cmake")]
#[test]
fn cmakelists_parses_by_file_name() {
    assert_parses_cleanly(
        "CMake",
        Path::new("sub/CMakeLists.txt"),
        snippet_for("cmake"),
    );
}

fn assert_parses_cleanly(name: &str, path: &Path, source: &str) {
    let language = lang_parsing_substrate::language_for_file(path)
        .unwrap_or_else(|| panic!("{name} ({}) did not resolve to a grammar", path.display()));

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&language)
        .unwrap_or_else(|e| panic!("{name}: failed to load grammar: {e}"));

    let tree = parser
        .parse(source, None)
        .unwrap_or_else(|| panic!("{name}: parser returned no tree"));

    assert!(
        !tree.root_node().has_error(),
        "{name} snippet failed to parse cleanly:\n{source}\n{}",
        tree.root_node().to_sexp()
    );
}
