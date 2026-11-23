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
//! Three markers are also what tree-sitter-cpp makes of valid C:
//! `new_expression` from a pointer named `new` (`new->next = head`),
//! `delete_expression` from a parameter named `delete` (`return delete * 2;`),
//! and `reference_declarator` from the statement `a & b;`. A `new` or
//! `delete` expression counts only when it parsed without error, has no
//! `ERROR` or `MISSING` node beside it, and the file never uses `new` or
//! `delete` as a name outside a `#define` or `#ifdef` (MFC's
//! `#define new DEBUG_NEW` is C++). A deleted function
//! (`void f(double) = delete;`) always counts, though the grammar leaves a
//! `MISSING` operand in it. A reference declarator counts under the same
//! error rule, unless it is a bare `a & b;`: a reference declared with no
//! initializer and without `extern`, which C++ rejects.
//!
//! Two more things count for nothing, whatever the marker:
//!
//! - A marker whose subtree holds a `MISSING` token. Error recovery inserts
//!   tokens, and for `MA_API T f(void);` with `MA_API` defined elsewhere it
//!   can build the qualified name `T::f` around a `MISSING` `::`. Whether it
//!   does depends on the code around the declaration, so one such header
//!   reads as C++ and a reduction of it does not.
//! - A marker inside an arm only a C++ compiler reads: `#ifdef __cplusplus`,
//!   `#if defined(__cplusplus) [&& ...]`, and the `#else` of
//!   `#ifndef __cplusplus` / `#if !defined(__cplusplus)`. That is how a C
//!   header offers C++ callers operator overloads or a wrapper class. A
//!   header whose only C++ is in such arms is C when compiled as C.
//!
//! Still read as C++: text in another language that tree-sitter-cpp can
//! parse, such as JavaScript in an Emscripten `EM_ASM` argument (`new`,
//! `delete`, `catch`) or Objective-C message sends (`[obj sel:x]` reads as a
//! lambda).
//!
//! The error rule costs some real C++: in `API_EXPORT int& counter();` the
//! grammar cannot place the unknown macro and leaves an `ERROR` beside the
//! reference, so a header whose only marker is that shape reads as C.
//!
//! Parses `source` with the C++ grammar internally, rather than taking an
//! already-parsed `Tree` like [`crate::detect_min_c_standard`] does —
//! there's no way to know which grammar to parse a `.h` file with *before*
//! this check runs. tree-sitter-cpp accepts nearly all valid C as a subset,
//! so parsing as C++ first and then looking for C++-exclusive node kinds is
//! the only order that works: parsing as C first would reject genuine C++
//! syntax outright rather than surface it as a queryable node.

use tree_sitter::{Node, Parser, TreeCursor};

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

    let mut scan = Scan::new(tree.walk(), cpp_only_lines(source));
    crate::query::walk_preorder(tree.root_node(), |node| scan.visit(node, source));
    scan.verdict()
}

/// The 1-based inclusive line ranges of the arms only a C++ compiler reads:
/// `#ifdef __cplusplus`, `#if defined(__cplusplus) [&& ...]`, and the `#else`
/// of `#ifndef __cplusplus` / `#if !defined(__cplusplus)`. C++ there is how a
/// C header serves C++ callers, so it is no evidence about the header itself.
/// The scan is the line-based one in [`crate::dead_code`], which the
/// unbalanced brace of an `extern "C"` guard cannot mis-nest.
fn cpp_only_lines(source: &[u8]) -> Vec<(usize, usize)> {
    crate::dead_code::dead_code_ranges(&String::from_utf8_lossy(source))
        .into_iter()
        .filter(|region| region.reason == crate::dead_code::DeadCodeReason::CppOnly)
        .map(|region| (region.start_line, region.end_line))
        .collect()
}

/// Whether `node`'s subtree holds a token that error recovery inserted. For
/// `MA_API T f(void);`, where `MA_API` is a macro defined elsewhere,
/// tree-sitter-cpp can read `T f` as the qualified name `T::f` with a
/// `MISSING` `::`; the C++ syntax there is the parser's, not the file's.
/// Only subtrees that hold an error are entered.
fn has_missing_token(node: Node) -> bool {
    if !node.has_error() {
        return false;
    }
    let mut found = false;
    crate::query::walk_preorder(node, |n| {
        if n.is_missing() {
            found = true;
            crate::query::Walk::Stop
        } else if n.has_error() {
            crate::query::Walk::Continue
        } else {
            crate::query::Walk::SkipChildren
        }
    });
    found
}

/// State for the single walk in [`looks_like_cpp`].
///
/// `new_expression`, `delete_expression` and `reference_declarator` also come
/// out of valid C: a pointer named `new` (`new->next = head`), a parameter
/// named `delete`, or an expression statement `a & b;`. So these three are
/// judged from their parent, where the walk can see their siblings without a
/// `Node::parent` call per node (that walks down from the root each time).
struct Scan<'a> {
    /// An unambiguous marker, or a reference declarator in a C++-only position.
    found: bool,
    /// A `new` or `delete` expression parsed cleanly.
    new_or_delete: bool,
    /// `new` or `delete` is used as a name somewhere, which C++ never allows
    /// outside the preprocessor.
    keyword_as_name: bool,
    /// Start byte of the name in the last `#define` or `#ifdef` seen. A
    /// macro may be named `new` in C++ too (MFC's `#define new DEBUG_NEW`).
    macro_name_start: Option<usize>,
    /// End byte of the furthest `ERROR` node seen. In pre-order, a node that
    /// starts before it lies inside that `ERROR`.
    error_end: usize,
    /// Reused for every node's children, rather than a new cursor per node.
    cursor: TreeCursor<'a>,
    /// From [`cpp_only_lines`]; a marker starting in one counts for nothing.
    cpp_only: Vec<(usize, usize)>,
}

impl<'a> Scan<'a> {
    fn new(cursor: TreeCursor<'a>, cpp_only: Vec<(usize, usize)>) -> Self {
        Self {
            found: false,
            new_or_delete: false,
            keyword_as_name: false,
            macro_name_start: None,
            error_end: 0,
            cursor,
            cpp_only,
        }
    }

    /// Whether `node` starts in an arm only a C++ compiler reads.
    fn in_cpp_only_arm(&self, node: Node) -> bool {
        let line = node.start_position().row + 1;
        self.cpp_only
            .iter()
            .any(|&(start, end)| (start..=end).contains(&line))
    }

    fn visit(&mut self, node: Node<'a>, source: &[u8]) -> crate::query::Walk {
        match node.kind() {
            "ERROR" => self.error_end = self.error_end.max(node.end_byte()),
            "preproc_def" | "preproc_function_def" | "preproc_ifdef" => {
                self.macro_name_start = node.child_by_field_name("name").map(|n| n.start_byte());
            }
            "identifier" | "field_identifier"
                if matches!(&source[node.byte_range()], b"new" | b"delete")
                    && self.macro_name_start != Some(node.start_byte()) =>
            {
                self.keyword_as_name = true;
            }
            _ => {}
        }
        if is_cpp_only_marker(node) {
            self.found = !has_missing_token(node) && !self.in_cpp_only_arm(node);
        } else if node.child_count() > 0 && node.start_byte() >= self.error_end && !node.is_error()
        {
            self.judge_children(node);
        }
        if self.found {
            crate::query::Walk::Stop
        } else {
            crate::query::Walk::Continue
        }
    }

    /// Judges the ambiguous markers among `parent`'s children. A marker beside
    /// an `ERROR` or `MISSING` sibling, or with one inside it, is error
    /// recovery and counts for nothing, except the `delete` of a deleted
    /// function.
    fn judge_children(&mut self, parent: Node<'a>) {
        let mut candidate = false;
        for child in parent.children(&mut self.cursor) {
            if child.is_error() || child.is_missing() {
                return;
            }
            candidate |= matches!(
                child.kind(),
                "new_expression" | "delete_expression" | "reference_declarator"
            );
        }
        if !candidate {
            return;
        }
        let children: Vec<Node<'a>> = parent.children(&mut self.cursor).collect();
        for child in children {
            if self.in_cpp_only_arm(child) {
                continue;
            }
            if is_deleted_function(parent, child) {
                self.found = true;
                return;
            }
            if child.has_error() {
                continue;
            }
            match child.kind() {
                "new_expression" | "delete_expression" => self.new_or_delete = true,
                "reference_declarator" if !is_bitwise_and_statement(parent, child) => {
                    self.found = true;
                    return;
                }
                _ => {}
            }
        }
    }

    fn verdict(&self) -> bool {
        self.found || (self.new_or_delete && !self.keyword_as_name)
    }
}

/// Whether `child` of `parent` is the `= delete` of a deleted function
/// (`void f(double) = delete;`). tree-sitter-cpp reads it as a
/// `delete_expression` whose operand is `MISSING`, under an `init_declarator`
/// that declares a function. C cannot initialize a function at all, so the
/// operand is not checked.
fn is_deleted_function(parent: Node, child: Node) -> bool {
    parent.kind() == "init_declarator"
        && child.kind() == "delete_expression"
        && parent
            .child_by_field_name("declarator")
            .is_some_and(|d| d.kind() == "function_declarator")
}

/// Whether `declarator`, a `reference_declarator` directly under the
/// declaration `parent`, is how tree-sitter-cpp reads the C statement
/// `a & b;`. As C++ that declares a reference with no initializer, which is
/// only legal when `extern`; a reference to a function (`const T& g();`) has
/// a function declarator inside instead of a bare name.
fn is_bitwise_and_statement(parent: Node, declarator: Node) -> bool {
    if parent.kind() != "declaration" {
        return false;
    }
    // tree-sitter-cpp gives the inner declarator no field name.
    let names_a_variable = declarator
        .named_child(0)
        .is_some_and(|inner| inner.kind() == "identifier");
    let mut cursor = parent.walk();
    let is_extern = parent.children(&mut cursor).any(|c| {
        c.kind() == "storage_class_specifier" && c.child(0).is_some_and(|k| k.kind() == "extern")
    });
    names_a_variable && !is_extern
}

/// Node kinds that tree-sitter-cpp 0.23 only produces for constructs with no
/// C meaning whatsoever. Verified against that version's `node-types.json`/
/// `grammar.json` — not assumed from general knowledge of C++ syntax, since
/// what matters is whether *this pinned grammar* actually emits a distinct,
/// queryable node for it. The ambiguous `new_expression`,
/// `delete_expression` and `reference_declarator` are judged in
/// [`Scan::judge_children`] instead.
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

    /// Wraps `body` in the usual `extern "C"` guard, which tree-sitter-cpp
    /// can only half-parse: the brace opens and closes in different
    /// `#ifdef` branches.
    fn guarded(body: &str) -> Vec<u8> {
        format!(
            "#ifdef __cplusplus\nextern \"C\" {{\n#endif\n\n{body}\n\n#ifdef __cplusplus\n}}\n#endif\n"
        )
        .into_bytes()
    }

    const POINTER_NAMED_NEW: &str = "static inline void list_add(struct list_head *new, struct list_head *head) { new->next = head->next; head->next = new; }";
    const PARAMETER_NAMED_DELETE: &str = "int f(int delete) { return delete * 2; }";
    const BITWISE_AND_STATEMENT: &str = "void g(int a, int b) { a & b; }";
    const KR_DEFINITION: &str =
        "int old_style(a, b)\nint a;\nchar *b;\n{\n    return a & 1;\n}\n\nstatic void h(struct node *new) { struct node *p = new; new = p; free(new); }";

    #[test]
    fn pointer_named_new_is_not_cpp() {
        assert!(!looks_like_cpp(POINTER_NAMED_NEW.as_bytes()));
        assert!(!looks_like_cpp(&guarded(POINTER_NAMED_NEW)));
    }

    #[test]
    fn parameter_named_delete_is_not_cpp() {
        assert!(!looks_like_cpp(PARAMETER_NAMED_DELETE.as_bytes()));
        assert!(!looks_like_cpp(&guarded(PARAMETER_NAMED_DELETE)));
        let initialized = b"int f(int delete) { int x = delete * 2; return x; }";
        assert!(!looks_like_cpp(initialized));
    }

    #[test]
    fn bitwise_and_statement_is_not_cpp() {
        assert!(!looks_like_cpp(BITWISE_AND_STATEMENT.as_bytes()));
        assert!(!looks_like_cpp(&guarded(BITWISE_AND_STATEMENT)));
    }

    #[test]
    fn kr_definition_after_extern_c_guard_is_not_cpp() {
        assert!(!looks_like_cpp(KR_DEFINITION.as_bytes()));
        assert!(!looks_like_cpp(&guarded(KR_DEFINITION)));
    }

    #[test]
    fn new_declared_elsewhere_is_not_cpp() {
        // `new` is declared in another header, so only error recovery is left.
        for body in [
            "void f(void) { new->next = 0; }",
            "void f(void) { list = new; }",
        ] {
            assert!(!looks_like_cpp(body.as_bytes()), "{body}");
        }
    }

    #[test]
    fn new_beside_a_parse_error_is_not_cpp() {
        assert!(!looks_like_cpp(b"void f(void) { new T $ }"));
    }

    #[test]
    fn delete_inside_a_parse_error_is_not_cpp() {
        assert!(!looks_like_cpp(b"void f( { delete p; }"));
    }

    #[test]
    fn new_beside_a_missing_node_is_not_cpp() {
        assert!(!looks_like_cpp(b"void f(void) { return new T }"));
    }

    #[test]
    fn new_defined_as_a_macro_is_still_cpp() {
        let src = b"#define new DEBUG_NEW\nvoid f(void) { T *p = new T; }";
        assert!(looks_like_cpp(src));
        assert!(looks_like_cpp(
            b"#ifdef new\n#endif\nvoid f(void) { delete p; }"
        ));
    }

    #[test]
    fn deleted_function_is_cpp() {
        assert!(looks_like_cpp(b"void f(double) = delete;"));
        assert!(looks_like_cpp(&guarded("void f(double) = delete;")));
    }

    #[test]
    fn reference_beside_an_unknown_macro_is_not_detected() {
        // Known gap: the grammar leaves an ERROR beside the reference.
        assert!(!looks_like_cpp(b"API_EXPORT int& counter();"));
    }

    #[test]
    fn new_and_delete_expressions_are_cpp() {
        for body in [
            "void f() { T *p = new T; }",
            "void f(int n) { int *a = new int[n]; }",
            "void f(T *p) { delete p; }",
            "void f(T *p) { delete[] p; }",
        ] {
            assert!(looks_like_cpp(body.as_bytes()), "{body}");
            assert!(looks_like_cpp(&guarded(body)), "guarded: {body}");
        }
    }

    #[test]
    fn reference_declarators_are_cpp() {
        for body in [
            "void f() { T& r = x; }",
            "void f(T& x);",
            "const T& g();",
            "extern T& r;",
        ] {
            assert!(looks_like_cpp(body.as_bytes()), "{body}");
            assert!(looks_like_cpp(&guarded(body)), "guarded: {body}");
        }
    }

    #[test]
    fn class_inside_extern_c_guard_is_cpp() {
        assert!(looks_like_cpp(&guarded("class Widget { int v; };")));
    }

    /// An include guard, the `extern "C"` guard, a typedef'd struct and one
    /// declaration behind an export macro defined elsewhere. tree-sitter-cpp
    /// reads `ma_log_callback ma_log_callback_init` as a qualified name with a
    /// `MISSING` `::` here. The recovery is that sensitive: shorter names or
    /// the declaration alone parse without it.
    const EXPORT_MACRO_HEADER: &[u8] = b"#ifndef X_H
#define X_H
#ifdef __cplusplus
extern \"C\" {
#endif
typedef unsigned int ma_uint32;
typedef struct
{
    void* pUserData;
} ma_log_callback;

MA_API ma_log_callback ma_log_callback_init(void* pUserData);
#ifdef __cplusplus
}
#endif
#endif
";

    #[test]
    fn inserted_scope_operator_is_not_cpp() {
        let tree = {
            let mut parser = Parser::new();
            parser
                .set_language(&tree_sitter_cpp::LANGUAGE.into())
                .unwrap();
            parser.parse(EXPORT_MACRO_HEADER, None).unwrap()
        };
        // Guards the fixture: the grammar must still build the false marker.
        let mut inserted = false;
        crate::query::walk_preorder(tree.root_node(), |n| {
            inserted |= n.kind() == "qualified_identifier" && has_missing_token(n);
            crate::query::Walk::Continue
        });
        assert!(inserted, "fixture no longer yields a MISSING `::`");
        assert!(!looks_like_cpp(EXPORT_MACRO_HEADER));
    }

    #[test]
    fn written_scope_operator_beside_an_export_macro_is_cpp() {
        let mut src = EXPORT_MACRO_HEADER.to_vec();
        src.extend_from_slice(b"int Widget::value() const { return 0; }\n");
        assert!(looks_like_cpp(&src));
    }

    #[test]
    fn cpp_only_arms_of_a_c_header_are_not_cpp() {
        for src in [
            // operator overloads for C++ callers
            "typedef struct { float x, y; } Vec2;\n#if defined(__cplusplus) && !defined(NO_CPP_OPERATORS)\ninline Vec2 operator+(Vec2 a, Vec2 b) { return a; }\ninline Vec2& operator+=(Vec2& a, Vec2 b) { return a; }\n#endif\n",
            "#ifdef __cplusplus\n# include <cstring>\nusing namespace std;\n#else\n# include <string.h>\n#endif\n",
            "#ifndef __cplusplus\nint f(void);\n#else\nnamespace m { class Model { public: Model(); ~Model(); }; }\n#endif\n",
            "#if !defined(__cplusplus)\nint f(void);\n#else\ntemplate<typename T> T id(T t) { return t; }\n#endif\n",
            "#ifdef __cplusplus\nvoid f(T& x);\nvoid g(T *p) { delete p; }\n#endif\n",
        ] {
            assert!(!looks_like_cpp(src.as_bytes()), "{src}");
        }
    }

    #[test]
    fn cpp_outside_a_cpp_only_arm_is_cpp() {
        for src in [
            "#ifndef __cplusplus\nnamespace m { int f(); }\n#endif\n",
            "#ifdef __cplusplus\nextern \"C\" {\n#endif\nint f(void);\n#ifdef __cplusplus\n}\n#endif\nclass Widget { int v; };\n",
            "#ifdef __cplusplus\nusing namespace std;\n#endif\nvoid f(T& x);\n",
            "#if defined(__cplusplus) || defined(WANT_CPP)\nusing namespace std;\n#endif\n",
        ] {
            assert!(looks_like_cpp(src.as_bytes()), "{src}");
        }
    }

    #[test]
    fn cpp_header_with_an_unknown_class_macro_is_cpp() {
        // A Qt header: `Q_OBJECT` is a macro tree-sitter-cpp cannot place.
        let src = b"#ifndef SIGNALBAR_H
#define SIGNALBAR_H
#include <QObject>
class SignalBar : public QStyledItemDelegate
{
\tQ_OBJECT

public:
\tSignalBar(QObject *parent = 0);
\t~SignalBar();
\tvirtual void paint(QPainter *painter, const QStyleOptionViewItem &option, const QModelIndex &index) const;
};
#endif
";
        assert!(looks_like_cpp(src));
    }

    #[test]
    fn foreign_text_still_reads_as_cpp() {
        // Known gap: Emscripten JavaScript parses as a C++ new-expression.
        assert!(looks_like_cpp(
            b"void f(void) { EM_ASM({ var a = new Uint8Array(data); }); }"
        ));
    }

    #[test]
    fn empty_source_is_not_cpp() {
        assert!(!looks_like_cpp(b""));
    }
}
