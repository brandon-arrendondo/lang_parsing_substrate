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
        other => panic!("no smoke-test snippet registered for language key {other:?}"),
    }
}

#[test]
fn every_compiled_in_language_parses_a_valid_snippet() {
    for lang in lang_parsing_substrate::languages() {
        let ext = lang.extensions[0];
        let path = Path::new("smoke").with_extension(ext);
        let language = lang_parsing_substrate::language_for_file(&path)
            .unwrap_or_else(|| panic!("{} (.{ext}) did not resolve to a grammar", lang.name));

        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language)
            .unwrap_or_else(|e| panic!("{}: failed to load grammar: {e}", lang.name));

        let source = snippet_for(lang.key);
        let tree = parser
            .parse(source, None)
            .unwrap_or_else(|| panic!("{}: parser returned no tree", lang.name));

        assert!(
            !tree.root_node().has_error(),
            "{} snippet failed to parse cleanly:\n{source}\n{}",
            lang.name,
            tree.root_node().to_sexp()
        );
    }
}
