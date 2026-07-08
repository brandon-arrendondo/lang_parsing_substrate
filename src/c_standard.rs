//! Best-effort lower bound on which C standard a file's syntax requires.
//!
//! No existing tool (clang, cppcheck, SonarQube) infers a C standard from
//! source alone — every one of them treats it as build-config input
//! (`-std=`, `compile_commands.json`) and requires it as an argument rather
//! than producing it as an output. That's the right call when a build
//! system is available, but this substrate's consumers sometimes have none
//! (a single file with no project context), so a syntax-only signal is the
//! only one available at all, not a redundant one.
//!
//! Consistent with the rest of this crate (`language_for_file` returns
//! `None` rather than guess), [`detect_min_c_standard`] never fabricates a
//! standard: it only reports `Some` for syntax with **no earlier-standard
//! meaning at all**, including no long-standing GNU-extension precedent.
//! Many intuitive markers (`inline`, compound literals, `//` comments,
//! mixed declarations/code, anonymous struct members) are deliberately
//! excluded for exactly this reason — GCC accepted all of them as
//! extensions well before they were standardized, so their presence doesn't
//! prove the file requires the standard that later absorbed them. `_Bool`,
//! `_Complex`, `typeof`, and `_BitInt` are excluded because the pinned
//! `tree-sitter-c` grammar doesn't tokenize them distinctly from an
//! ordinary identifier — there is no node to key a query off. See
//! `docs/research-c-standard-detection.md` for the full per-marker survey.
//!
//! This is a syntax-only signal, not a preprocessor-aware one: a marker
//! guarded by `#if __STDC_VERSION__ >= 201112L ... #endif` and never
//! actually compiled at this standard is still counted, since tree-sitter
//! has no macro evaluation. Documented as a known limitation rather than
//! solved (see the research doc's false-positive/negative traps section).

use crate::query::node_text;
use tree_sitter::{Node, Tree};

/// A lower bound on the C standard a file's syntax requires. Ordered so
/// `C99 < C11 < C23` for [`detect_min_c_standard`]'s "never returns a lower
/// standard than any individual marker implies" guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CStandard {
    C99,
    /// Stands in for "C11 or C17" — C17 is a defect-fix release with no
    /// syntax of its own, so `tree-sitter-c` cannot distinguish the two.
    C11,
    C23,
}

/// Best-effort lower bound: the earliest C standard `tree`'s syntax
/// requires. Returns `None` when no diagnostic marker is present — this
/// means "consistent with C89", not "confirmed C89". See the module
/// documentation for what is and isn't treated as a marker, and why.
pub fn detect_min_c_standard(tree: &Tree, source: &[u8]) -> Option<CStandard> {
    let mut best: Option<CStandard> = None;
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if let Some(found) = marker_standard(node, source) {
            if best.is_none_or(|b| found > b) {
                best = Some(found);
            }
            if best == Some(CStandard::C23) {
                break;
            }
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    best
}

/// The standard `node` implies on its own, or `None` if it isn't one of the
/// markers this module recognizes. Each arm below corresponds to a "keep"
/// row in `docs/research-c-standard-detection.md`'s marker tables.
fn marker_standard(node: Node, source: &[u8]) -> Option<CStandard> {
    match node.kind() {
        // C11: no pre-C11 meaning whatsoever.
        "generic_expression" => Some(CStandard::C11),
        // C11: covers both the `alignas` and `_Alignas` spellings, which are
        // both C11+ (the pre-C11 GNU equivalent is `__attribute__((aligned))`,
        // a structurally distinct node this rule never matches).
        "alignas_qualifier" => Some(CStandard::C11),
        // C11, but only the exact `_Alignof`/`alignof` spellings — the same
        // grammar rule also matches the GNU pre-C11 spellings `__alignof__`,
        // `__alignof`, and `_alignof`, which must not count.
        "alignof_expression" => {
            let text = node_text(node, source);
            (text.starts_with("_Alignof") || text.starts_with("alignof")).then_some(CStandard::C11)
        }
        // `type_qualifier` is a leaf node (no children) whose text is one of
        // several keywords, only some of which are standard markers. `const`,
        // `volatile`, `__restrict__`, `__extension__`, and the Clang-only
        // `_Nonnull` are deliberately not matched here.
        "type_qualifier" => match node_text(node, source) {
            "restrict" => Some(CStandard::C99),
            "_Atomic" | "_Noreturn" | "noreturn" => Some(CStandard::C11),
            "constexpr" => Some(CStandard::C23),
            _ => None,
        },
        // C99 designated initializers (`.field = x` / `[i] = x`). Plain
        // `field_identifier`/`subscript_range_designator` designators are a
        // pre-C99 GNU extension and are not matched.
        "initializer_pair" => {
            let mut cursor = node.walk();
            let has_designator = node
                .children_by_field_name("designator", &mut cursor)
                .any(|d| matches!(d.kind(), "field_designator" | "subscript_designator"));
            has_designator.then_some(CStandard::C99)
        }
        // C99: a bare `*` as an array size (`int f(int a[*])`) has no
        // pre-C99 meaning. A non-constant *expression* here would also be a
        // VLA, but distinguishing a VLA's non-constant size from an ordinary
        // constant/macro size needs constant-folding this crate doesn't do,
        // so only the unambiguous `[*]` form is matched.
        "array_declarator" | "abstract_array_declarator" => {
            let size = node.child_by_field_name("size")?;
            (!size.is_named() && node_text(size, source) == "*").then_some(CStandard::C99)
        }
        // C23: `nullptr` shares this grammar node with `NULL`, which is not
        // a marker (it's an ordinary pre-C89 macro).
        "null" => (node_text(node, source) == "nullptr").then_some(CStandard::C23),
        // C23: `[[...]]` attributes are structurally distinct from the GNU
        // `__attribute__((...))` extension (a different node entirely).
        "attribute_declaration" => Some(CStandard::C23),
        // C23: `#embed` has no dedicated grammar node (it falls through the
        // generic preprocessor-directive regex), so this is a text match on
        // the directive's spelling rather than a node-kind check. Known gap:
        // this only matches `#embed` at a position the grammar accepts as a
        // top-level preprocessor call; `#embed` used inside an aggregate
        // initializer's braces (its most common real-world position) instead
        // produces an `ERROR` node wrapping the `preproc_directive`, which
        // this function does not attempt to recover — false negative, not a
        // fabricated result, so it's consistent with this module's contract.
        "preproc_call" => {
            let directive = node.child_by_field_name("directive")?;
            (node_text(directive, source) == "#embed").then_some(CStandard::C23)
        }
        // C11: `_Static_assert`/`static_assert` have no dedicated grammar
        // node — they parse as an ordinary identifier used as a call/declarator
        // name. Both names are reserved identifiers, so a real-world
        // collision with an unrelated symbol of the same name is negligible.
        "identifier" => matches!(node_text(node, source), "_Static_assert" | "static_assert")
            .then_some(CStandard::C11),
        _ => None,
    }
}

#[cfg(test)]
#[cfg(feature = "lang-c")]
mod tests {
    use super::*;

    fn detect(source: &str) -> Option<CStandard> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_c::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        detect_min_c_standard(&tree, source.as_bytes())
    }

    #[test]
    fn plain_c89_compatible_file_returns_none() {
        let source = "int add(int a, int b) { int c; c = a + b; return c; }";
        assert_eq!(detect(source), None);
    }

    #[test]
    fn gnu_pre_standard_spellings_do_not_leak_into_the_signal() {
        let source = r#"
            __inline int f(__restrict__ int *p) { return *p; }
            int g(void) { int x = (int) { 1 }; return __typeof__(x) { 0 }.dummy; }
        "#;
        assert_eq!(detect(source), None);
    }

    #[test]
    fn stdbool_style_usage_alone_returns_none() {
        let source = "bool flag(void) { bool b = true; return b || false; }";
        assert_eq!(detect(source), None);
    }

    #[test]
    fn bare_restrict_is_c99() {
        let source = "void f(int *restrict p) { *p = 1; }";
        assert_eq!(detect(source), Some(CStandard::C99));
    }

    #[test]
    fn field_designated_initializer_is_c99() {
        let source = "struct s { int a; int b; }; struct s x = { .a = 1, .b = 2 };";
        assert_eq!(detect(source), Some(CStandard::C99));
    }

    #[test]
    fn subscript_designated_initializer_is_c99() {
        let source = "int a[3] = { [0] = 1, [2] = 3 };";
        assert_eq!(detect(source), Some(CStandard::C99));
    }

    #[test]
    fn unspecified_vla_size_in_prototype_is_c99() {
        let source = "void f(int n, int a[*]);";
        assert_eq!(detect(source), Some(CStandard::C99));
    }

    #[test]
    fn generic_expression_is_c11() {
        let source = "int f(int x) { return _Generic(x, int: 1, default: 0); }";
        assert_eq!(detect(source), Some(CStandard::C11));
    }

    #[test]
    fn alignas_qualifier_is_c11() {
        let source = "_Alignas(16) int x;";
        assert_eq!(detect(source), Some(CStandard::C11));
    }

    #[test]
    fn alignof_expression_is_c11() {
        let source = "int x = _Alignof(int);";
        assert_eq!(detect(source), Some(CStandard::C11));
    }

    #[test]
    fn gnu_alignof_spellings_do_not_count_as_c11() {
        let source = "int x = __alignof__(int); int y = _alignof(int);";
        assert_eq!(detect(source), None);
    }

    #[test]
    fn atomic_qualifier_is_c11() {
        let source = "_Atomic int counter;";
        assert_eq!(detect(source), Some(CStandard::C11));
    }

    #[test]
    fn noreturn_qualifier_is_c11() {
        let source = "_Noreturn void die(void);";
        assert_eq!(detect(source), Some(CStandard::C11));
    }

    #[test]
    fn static_assert_identifier_is_c11() {
        let source = "_Static_assert(sizeof(int) == 4, \"bad int size\");";
        assert_eq!(detect(source), Some(CStandard::C11));
    }

    #[test]
    fn nullptr_is_c23() {
        let source = "int *p = nullptr;";
        assert_eq!(detect(source), Some(CStandard::C23));
    }

    #[test]
    fn constexpr_qualifier_is_c23() {
        let source = "constexpr int limit = 100;";
        assert_eq!(detect(source), Some(CStandard::C23));
    }

    #[test]
    fn standard_attribute_is_c23() {
        let source = "[[nodiscard]] int f(void);";
        assert_eq!(detect(source), Some(CStandard::C23));
    }

    #[test]
    fn embed_directive_is_c23() {
        let source = "#embed \"data.bin\"\n";
        assert_eq!(detect(source), Some(CStandard::C23));
    }

    #[test]
    fn embed_directive_inside_initializer_braces_is_a_known_false_negative() {
        // Documented gap, not a bug: this position produces an `ERROR` node
        // rather than `preproc_call`, so it's not detected. See the
        // `preproc_call` match arm's comment.
        let source = "unsigned char data[] = {\n#embed \"data.bin\"\n};";
        assert_eq!(detect(source), None);
    }

    #[test]
    fn known_limitation_macro_guarded_marker_is_still_counted() {
        // Documented limitation, not a bug: tree-sitter has no preprocessor,
        // so a marker inside an untaken `#if __STDC_VERSION__` branch is
        // still visited and counted. If a future preprocessor-aware
        // refinement changes this, update this assertion deliberately.
        let source = r#"
            #if __STDC_VERSION__ >= 201112L
            int f(int x) { return _Generic(x, int: 1, default: 0); }
            #endif
        "#;
        assert_eq!(detect(source), Some(CStandard::C11));
    }

    #[test]
    fn combining_markers_never_reports_a_lower_standard_than_either_alone() {
        let c99_only = "void f(int *restrict p) { *p = 1; }";
        let c11_only = "_Atomic int counter;";
        let combined = format!("{c99_only}\n{c11_only}");
        assert_eq!(detect(c99_only), Some(CStandard::C99));
        assert_eq!(detect(c11_only), Some(CStandard::C11));
        assert_eq!(detect(&combined), Some(CStandard::C11));
    }
}
