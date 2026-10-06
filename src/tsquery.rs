//! Run a tree-sitter query (S-expression pattern language) and return owned
//! captures, plus each grammar's bundled tags query.
//!
//! Distinct from [`crate::query`], which holds predicate-based traversal
//! helpers and deliberately has no pattern language. This module is the thin
//! pass-through for consumers that do want one, mainly the bundled
//! `TAGS_QUERY`s, which name every definition and reference in a file.
//!
//! The standard predicates (`#eq?`, `#match?`, `#any-of?` and their `not-`
//! forms) are applied. tree-sitter-tags' own directives (`#strip!`,
//! `#select-adjacent!`, used by the JavaScript tags query on its `@doc`
//! capture) are NOT applied, so a `@doc` capture returns every adjacent
//! comment, unstripped.

use tree_sitter::{Language, Node, Query, QueryCursor, QueryError, StreamingIterator};

/// One capture: a node a query pattern matched and named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    /// Index of the pattern within the query source that produced the match.
    pub pattern_index: usize,
    /// Capture name, without the `@`.
    pub name: String,
    /// Kind of the captured node.
    pub kind: &'static str,
    /// Start offset in bytes.
    pub start_byte: usize,
    /// End offset in bytes (exclusive).
    pub end_byte: usize,
    /// Start (row, column), 0-based, column in bytes.
    pub start_point: (usize, usize),
    /// End (row, column), 0-based, column in bytes.
    pub end_point: (usize, usize),
}

/// Every capture of `query_source` over `root`, in document order.
///
/// Returns the compile error when the query does not parse against
/// `language`, for example a node kind the grammar does not have.
pub fn run_query(
    language: &Language,
    root: Node,
    source: &[u8],
    query_source: &str,
) -> Result<Vec<Capture>, QueryError> {
    let query = Query::new(language, query_source)?;
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut captures = cursor.captures(&query, root, source);
    let mut out = Vec::new();
    while let Some((m, index)) = captures.next() {
        let capture = m.captures[*index];
        let node = capture.node;
        let (start, end) = (node.start_position(), node.end_position());
        out.push(Capture {
            pattern_index: m.pattern_index,
            name: names[capture.index as usize].to_string(),
            kind: node.kind(),
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            start_point: (start.row, start.column),
            end_point: (end.row, end.column),
        });
    }
    Ok(out)
}

/// The tags query bundled with `language_key`'s grammar crate, or `None` when
/// the grammar ships none or the language is not compiled in.
///
/// TypeScript's own tags query only adds the TypeScript-specific constructs
/// (signatures, interfaces, modules), so for `typescript` and `tsx` it is
/// returned after JavaScript's, which supplies functions, classes and calls.
pub fn tags_query(language_key: &str) -> Option<String> {
    match language_key {
        #[cfg(feature = "lang-c")]
        "c" => Some(tree_sitter_c::TAGS_QUERY.to_string()),
        #[cfg(feature = "lang-cpp")]
        "cpp" => Some(tree_sitter_cpp::TAGS_QUERY.to_string()),
        #[cfg(feature = "lang-python")]
        "python" => Some(tree_sitter_python::TAGS_QUERY.to_string()),
        #[cfg(feature = "lang-rust")]
        "rust" => Some(tree_sitter_rust::TAGS_QUERY.to_string()),
        #[cfg(feature = "lang-javascript")]
        "javascript" => Some(tree_sitter_javascript::TAGS_QUERY.to_string()),
        #[cfg(all(feature = "lang-javascript", feature = "lang-typescript"))]
        "typescript" | "tsx" => Some(format!(
            "{}\n{}",
            tree_sitter_javascript::TAGS_QUERY,
            tree_sitter_typescript::TAGS_QUERY
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::language_for_key;
    use tree_sitter::Parser;

    fn captures(key: &str, src: &str, query: &str) -> Vec<Capture> {
        let language = language_for_key(key).unwrap();
        let mut parser = Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(src, None).unwrap();
        run_query(&language, tree.root_node(), src.as_bytes(), query).unwrap()
    }

    #[test]
    #[cfg(feature = "lang-python")]
    fn captures_carry_names_and_positions() {
        let src = "def f():\n    pass\n\ndef g():\n    f()\n";
        let caps = captures(
            "python",
            src,
            "(function_definition name: (identifier) @name) @def",
        );
        let names: Vec<_> = caps
            .iter()
            .filter(|c| c.name == "name")
            .map(|c| &src[c.start_byte..c.end_byte])
            .collect();
        assert_eq!(names, ["f", "g"]);
        let defs: Vec<_> = caps.iter().filter(|c| c.name == "def").collect();
        assert_eq!(defs[1].start_point, (3, 0));
        assert_eq!(defs[1].kind, "function_definition");
    }

    #[test]
    #[cfg(feature = "lang-python")]
    fn predicates_are_applied() {
        let src = "a = 1\nb = 2\n";
        let caps = captures("python", src, "((identifier) @id (#eq? @id \"b\"))");
        assert_eq!(caps.len(), 1);
        assert_eq!(&src[caps[0].start_byte..caps[0].end_byte], "b");
    }

    #[test]
    #[cfg(feature = "lang-python")]
    fn an_invalid_query_is_an_error() {
        let language = language_for_key("python").unwrap();
        let mut parser = Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse("x = 1", None).unwrap();
        assert!(run_query(&language, tree.root_node(), b"x = 1", "(no_such_kind) @x").is_err());
    }

    #[test]
    fn every_bundled_tags_query_compiles() {
        for key in [
            "c",
            "cpp",
            "python",
            "rust",
            "javascript",
            "typescript",
            "tsx",
        ] {
            if let (Some(language), Some(q)) = (language_for_key(key), tags_query(key)) {
                assert!(
                    Query::new(&language, &q).is_ok(),
                    "tags query for {key} does not compile"
                );
            }
        }
    }

    #[test]
    #[cfg(all(feature = "lang-javascript", feature = "lang-typescript"))]
    fn typescript_tags_find_functions_and_interfaces() {
        let src = "interface I { m(): void }\nfunction f() { g(); }\n";
        let caps = captures("typescript", src, &tags_query("typescript").unwrap());
        let names: Vec<_> = caps.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"definition.function"));
        assert!(names.contains(&"definition.interface"));
        assert!(names.contains(&"reference.call"));
    }
}
