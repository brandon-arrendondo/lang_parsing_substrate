//! Best-effort content-based disambiguation for `.h` files between C and
//! C++.
//!
//! `.h` is inherently ambiguous by extension alone — `language_for_file`
//! resolves it to the C grammar unconditionally (see this crate's
//! `CLAUDE.md`), which is correct for the overwhelming majority of `.h`
//! files but wrong for a C++ header that happens to use the `.h` extension
//! instead of `.hpp`/`.hxx`. [`looks_like_cpp`] gives callers that already
//! have the file's bytes (e.g. after reading it to parse) a way to catch
//! the common case: a header using unambiguous C++ syntax.
//!
//! Consistent with the rest of this crate's "never fabricate" stance
//! ([`crate::detect_min_c_standard`] is the other example), this only
//! returns `true` for syntax that has **no meaning in C at all** — a
//! header written entirely in the C-compatible subset of C++ (no classes,
//! templates, namespaces, `::`, references, `try`/`catch`, …) is
//! indistinguishable from C by syntax alone and is intentionally left
//! classified as C rather than guessed at. `false` means "consistent with
//! C", not "confirmed C".
//!
//! Deliberately excluded from the marker set: `static_assert_declaration`
//! (tree-sitter-cpp's dedicated node for the `static_assert` keyword) —
//! a C11 header that uses the `static_assert` macro from `<assert.h>`
//! (which expands to `_Static_assert`, but tree-sitter has no preprocessor
//! and sees the literal, unexpanded `static_assert` spelling) would parse
//! identically under the C++ grammar and produce a false positive. This
//! mirrors `c_standard`'s exclusion of the same identifier for the same
//! reason.
//!
//! Parses `source` with the C++ grammar internally, rather than taking an
//! already-parsed `Tree` like [`crate::detect_min_c_standard`] does —
//! there's no way to know which grammar to parse a `.h` file with *before*
//! this check runs. tree-sitter-cpp accepts nearly all valid C as a subset,
//! so parsing as C++ first and then looking for C++-exclusive node kinds is
//! the only order that works: parsing as C first would reject genuine C++
//! syntax outright rather than surface it as a queryable node.

use tree_sitter::{Node, Parser};

/// Returns `true` if `source`'s syntax contains a construct with no meaning
/// in C — i.e. the file can only be C++. See the module documentation for
/// what is and isn't treated as a marker, and why `false` doesn't mean
/// "confirmed C".
pub fn looks_like_cpp(source: &[u8]) -> bool {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_cpp::LANGUAGE.into())
        .is_err()
    {
        return false;
    }
    let Some(tree) = parser.parse(source, None) else {
        return false;
    };

    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if is_cpp_only_marker(node) {
            return true;
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    false
}

/// Node kinds that tree-sitter-cpp 0.23 only produces for constructs with no
/// C meaning whatsoever. Verified against that version's `node-types.json`/
/// `grammar.json` — not assumed from general knowledge of C++ syntax, since
/// what matters is whether *this pinned grammar* actually emits a distinct,
/// queryable node for it.
fn is_cpp_only_marker(node: Node) -> bool {
    matches!(
        node.kind(),
        "class_specifier"
            | "namespace_definition"
            | "namespace_alias_definition"
            | "template_declaration"
            | "template_instantiation"
            | "using_declaration"
            | "access_specifier"
            | "qualified_identifier"
            | "reference_declarator"
            | "new_expression"
            | "delete_expression"
            | "try_statement"
            | "catch_clause"
            | "throw_statement"
            | "friend_declaration"
            | "operator_cast"
            | "operator_name"
            | "lambda_expression"
            | "destructor_name"
            | "field_initializer_list"
            | "base_class_clause"
            | "noexcept"
            | "concept_definition"
            | "requires_clause"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_c_header_is_not_cpp() {
        let src = br#"
#ifndef FOO_H
#define FOO_H

typedef struct Point {
    int x;
    int y;
} Point;

int point_distance(const Point *a, const Point *b);

#endif
"#;
        assert!(!looks_like_cpp(src));
    }

    #[test]
    fn c11_static_assert_macro_is_not_cpp() {
        let src = br#"
#include <assert.h>
static_assert(sizeof(int) == 4, "unexpected int size");
"#;
        assert!(!looks_like_cpp(src));
    }

    #[test]
    fn class_is_cpp() {
        let src = br#"
class Widget {
public:
    Widget();
    int value() const;
private:
    int value_;
};
"#;
        assert!(looks_like_cpp(src));
    }

    #[test]
    fn namespace_is_cpp() {
        let src = b"namespace app { int helper(); }";
        assert!(looks_like_cpp(src));
    }

    #[test]
    fn template_is_cpp() {
        let src = b"template<typename T> T max(T a, T b) { return a > b ? a : b; }";
        assert!(looks_like_cpp(src));
    }

    #[test]
    fn scope_resolution_is_cpp() {
        let src = b"int Widget::value() const { return 0; }";
        assert!(looks_like_cpp(src));
    }

    #[test]
    fn reference_parameter_is_cpp() {
        let src = b"void increment(int &x);";
        assert!(looks_like_cpp(src));
    }

    #[test]
    fn try_catch_is_cpp() {
        let src = b"void f() { try { g(); } catch (...) { } }";
        assert!(looks_like_cpp(src));
    }

    #[test]
    fn using_namespace_is_cpp() {
        let src = b"using namespace std;";
        assert!(looks_like_cpp(src));
    }

    #[test]
    fn empty_source_is_not_cpp() {
        assert!(!looks_like_cpp(b""));
    }
}
