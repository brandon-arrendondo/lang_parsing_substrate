//! Interrupt-handler (ISR) detection for C/C++.
//!
//! Identifies function definitions in a translation unit that carry **real
//! syntactic evidence** of being an interrupt/ISR handler — a GNU
//! `__attribute__((interrupt))`/`__attribute__((interrupt("...")))`, a C23
//! `[[gnu::interrupt]]`-style attribute, or an AVR-libc-style
//! `ISR(vector) { ... }` macro invocation. See this repo's
//! `ISR_DETECTION.md` for the full design handoff and the tree-sitter-c
//! 0.24.2 probe output each evidence shape was verified against.
//!
//! Deliberately **not** a name-substring heuristic (`isr`/`irq`/`interrupt`
//! in the function name): a real-world firmware audit (see
//! `ISR_DETECTION.md`'s "Catapult" section) found that name-matching alone
//! produced ~76-80% false positives, including a plain main-loop function
//! named `DEV_APPROX_IRQProcess`. No name-based fallback is offered here —
//! callers wanting that heuristic as a separate, lower-confidence signal
//! should implement it themselves.
//!
//! Any macro-shaped function definition (`SOMENAME(args) { body }`, parsed
//! by this grammar identically to `ISR(vector) { body }` — see
//! [`InterruptEvidence::MacroInvocation`]) is reported with its macro name;
//! deciding *which* macro names actually mean "interrupt handler" is a
//! project-convention fact, not a language fact, so that filtering is left
//! to the caller rather than baked in here.
//!
//! Vendor keyword-style annotations (IAR's `#pragma vector=` + `__interrupt`,
//! Keil's `__irq`) are a documented gap, not covered: both produce an
//! `ERROR` node under `tree-sitter-c` 0.24.2, and IAR's pragma has no AST
//! link to the function it annotates (positional/textual only) — see
//! `ISR_DETECTION.md` for the verification.

use crate::calls::get_function_name;
use crate::query::{find_descendants_of_kind, find_first_descendant, node_text};
use tree_sitter::Node;

/// One function definition identified as an interrupt/ISR handler, along
/// with the syntactic evidence that justified it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptHandler {
    /// The handler's name, when resolvable via [`get_function_name`]. Always
    /// `None` for [`InterruptEvidence::MacroInvocation`] — a macro-shaped
    /// function definition has no `name` field for this grammar to expose
    /// (see that variant's docs).
    pub name: Option<String>,
    /// Byte range of the `function_definition` node.
    pub node_range: (usize, usize),
    /// The evidence that identified this function as a handler.
    pub evidence: InterruptEvidence,
}

/// Why a function definition was identified as an interrupt/ISR handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterruptEvidence {
    /// `__attribute__((interrupt))` or `__attribute__((interrupt("...")))`.
    GnuAttribute {
        /// Always `"interrupt"` — kept as a field (rather than a unit
        /// variant) for symmetry with [`InterruptEvidence::C23Attribute`]
        /// and in case a future vendor spelling needs distinguishing.
        attribute_name: String,
    },
    /// A C23 `[[...]]` attribute named `interrupt`, e.g. `[[gnu::interrupt]]`.
    C23Attribute {
        /// The attribute's namespace prefix, e.g. `Some("gnu")` for
        /// `[[gnu::interrupt]]`, or `None` for an unprefixed `[[interrupt]]`.
        prefix: Option<String>,
        /// Always `"interrupt"`.
        name: String,
    },
    /// A macro-invocation-shaped function definition, e.g. AVR-libc's
    /// `ISR(TIMER1_COMPA_vect) { ... }`. `tree-sitter-c` has no macro
    /// expansion, so this parses as an ordinary `function_definition` whose
    /// `type` field holds the macro name (misparsed as a return-type
    /// identifier) and whose `declarator` is a `parenthesized_declarator`
    /// wrapping the macro's argument. Structurally indistinguishable from
    /// any other macro of the same call-like shape — this variant reports
    /// *every* such function, not just ones named `ISR`; filtering by which
    /// macro names actually mean "interrupt handler" is the caller's job
    /// (see module docs).
    MacroInvocation {
        /// The macro name, e.g. `"ISR"`.
        macro_name: String,
    },
}

/// Finds every function definition in `root` carrying real syntactic
/// evidence of being an interrupt/ISR handler. See module docs for exactly
/// what counts as evidence and what's deliberately excluded.
///
/// Per-file only, like the rest of this crate: no cross-file resolution, no
/// judgment about whether a handler is ever actually wired up to a vector
/// table. That's a caller concern.
pub fn interrupt_handlers<'a>(root: Node<'a>, source: &str) -> Vec<InterruptHandler> {
    let mut out = Vec::new();
    for func in find_descendants_of_kind(root, "function_definition") {
        let evidence = attribute_evidence(func, source).or_else(|| macro_evidence(func, source));
        if let Some(evidence) = evidence {
            out.push(InterruptHandler {
                name: get_function_name(func, source),
                node_range: (func.start_byte(), func.end_byte()),
                evidence,
            });
        }
    }
    out
}

/// Checks `func`'s direct children for a GNU `attribute_specifier` or a C23
/// `attribute_declaration` naming the `interrupt` attribute. Both syntaxes
/// attach as a direct child of `function_definition` (verified against
/// `tree-sitter-c` 0.24.2 — see `ISR_DETECTION.md`), not a deeper descendant,
/// so this deliberately does not recurse past `func`'s immediate children.
fn attribute_evidence(func: Node, source: &str) -> Option<InterruptEvidence> {
    let mut cursor = func.walk();
    for child in func.children(&mut cursor) {
        match child.kind() {
            "attribute_specifier" => {
                if let Some(evidence) = gnu_attribute_evidence(child, source) {
                    return Some(evidence);
                }
            }
            "attribute_declaration" => {
                if let Some(evidence) = c23_attribute_evidence(child, source) {
                    return Some(evidence);
                }
            }
            _ => {}
        }
    }
    None
}

/// `attribute_specifier (argument_list (identifier))` for the no-argument
/// form (`__attribute__((interrupt))`), or `attribute_specifier
/// (argument_list (call_expression function: (identifier) arguments:
/// (argument_list ...)))` for the with-argument form
/// (`__attribute__((interrupt("IRQ")))`).
fn gnu_attribute_evidence(attr_spec: Node, source: &str) -> Option<InterruptEvidence> {
    let arg_list = find_first_descendant(attr_spec, |n| n.kind() == "argument_list")?;
    let mut cursor = arg_list.walk();
    for child in arg_list.named_children(&mut cursor) {
        let name = match child.kind() {
            "identifier" => node_text(child, source.as_bytes()),
            "call_expression" => {
                let Some(func_field) = child.child_by_field_name("function") else {
                    continue;
                };
                node_text(func_field, source.as_bytes())
            }
            _ => continue,
        };
        if name == "interrupt" {
            return Some(InterruptEvidence::GnuAttribute {
                attribute_name: name.to_string(),
            });
        }
    }
    None
}

/// `attribute_declaration (attribute prefix: (identifier)? name:
/// (identifier))`, matching `node-types.json`'s documented schema (same
/// node this crate's `c_standard.rs` already keys off for an unrelated
/// purpose).
fn c23_attribute_evidence(attr_decl: Node, source: &str) -> Option<InterruptEvidence> {
    for attr in find_descendants_of_kind(attr_decl, "attribute") {
        let Some(name_node) = attr.child_by_field_name("name") else {
            continue;
        };
        let name = node_text(name_node, source.as_bytes());
        if name != "interrupt" {
            continue;
        }
        let prefix = attr
            .child_by_field_name("prefix")
            .map(|p| node_text(p, source.as_bytes()).to_string());
        return Some(InterruptEvidence::C23Attribute {
            prefix,
            name: name.to_string(),
        });
    }
    None
}

/// True when `func` is shaped like a macro invocation
/// (`SOMENAME(args) { body }` — its `declarator` is a
/// `parenthesized_declarator`, not a `function_declarator`). Mirrors
/// `calls.rs`'s private `is_macro_function_definition`, which uses this same
/// shape to *exclude* such definitions from the call graph — see this
/// module's docs and `ISR_DETECTION.md`'s "Cross-cutting note" for why that
/// exclusion means an `ISR(vector) { ... }` handler's own calls are
/// currently invisible to `call_edges`.
fn macro_evidence(func: Node, source: &str) -> Option<InterruptEvidence> {
    let declarator = func.child_by_field_name("declarator")?;
    if declarator.kind() != "parenthesized_declarator" {
        return None;
    }
    let type_node = func.child_by_field_name("type")?;
    Some(InterruptEvidence::MacroInvocation {
        macro_name: node_text(type_node, source.as_bytes()).to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    #[cfg(feature = "lang-c")]
    fn parse_c(source: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_c::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn avr_libc_macro_invocation_is_detected() {
        let source = "ISR(TIMER1_COMPA_vect) {\n    counter++;\n}\n";
        let tree = parse_c(source);
        let handlers = interrupt_handlers(tree.root_node(), source);
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0].name, None);
        assert_eq!(
            handlers[0].evidence,
            InterruptEvidence::MacroInvocation {
                macro_name: "ISR".to_string()
            }
        );
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn gnu_attribute_no_arg_is_detected() {
        let source = "__attribute__((interrupt)) void real_isr(void) {\n    counter++;\n}\n";
        let tree = parse_c(source);
        let handlers = interrupt_handlers(tree.root_node(), source);
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0].name.as_deref(), Some("real_isr"));
        assert_eq!(
            handlers[0].evidence,
            InterruptEvidence::GnuAttribute {
                attribute_name: "interrupt".to_string()
            }
        );
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn gnu_attribute_with_arg_is_detected() {
        let source =
            "void __attribute__((interrupt(\"IRQ\"))) arm_isr(void) {\n    counter++;\n}\n";
        let tree = parse_c(source);
        let handlers = interrupt_handlers(tree.root_node(), source);
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0].name.as_deref(), Some("arm_isr"));
        assert_eq!(
            handlers[0].evidence,
            InterruptEvidence::GnuAttribute {
                attribute_name: "interrupt".to_string()
            }
        );
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn c23_attribute_is_detected() {
        let source = "[[gnu::interrupt]] void c23_isr(void *frame) {\n    counter++;\n}\n";
        let tree = parse_c(source);
        let handlers = interrupt_handlers(tree.root_node(), source);
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0].name.as_deref(), Some("c23_isr"));
        assert_eq!(
            handlers[0].evidence,
            InterruptEvidence::C23Attribute {
                prefix: Some("gnu".to_string()),
                name: "interrupt".to_string()
            }
        );
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn bare_isr_ish_name_with_no_marking_is_not_detected() {
        // Regression for the Catapult DEV_APPROX_IRQProcess failure mode:
        // an ISR-ish *name* with zero attribute/macro evidence must not be
        // reported. This is the whole reason this module exists instead of
        // a name-substring regex.
        let source = "void DEV_APPROX_IRQProcess(void) {\n    poll();\n}\n";
        let tree = parse_c(source);
        assert!(interrupt_handlers(tree.root_node(), source).is_empty());
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn unrelated_attribute_is_not_detected() {
        let source = "__attribute__((noreturn)) void die(void) {\n    for(;;);\n}\n";
        let tree = parse_c(source);
        assert!(interrupt_handlers(tree.root_node(), source).is_empty());
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn unrelated_macro_shaped_function_is_still_reported_as_macro_invocation() {
        // Any macro-shaped function_definition is structurally
        // indistinguishable from ISR(vector) { ... } — see module docs on
        // why filtering by macro name is left to the caller.
        let source = "TEST_CASE(my_test) {\n    assert(1);\n}\n";
        let tree = parse_c(source);
        let handlers = interrupt_handlers(tree.root_node(), source);
        assert_eq!(handlers.len(), 1);
        assert_eq!(
            handlers[0].evidence,
            InterruptEvidence::MacroInvocation {
                macro_name: "TEST_CASE".to_string()
            }
        );
    }

    #[test]
    #[cfg(feature = "lang-c")]
    fn multiple_handlers_in_one_file_are_all_detected() {
        let source = "ISR(TIMER1_COMPA_vect) {\n    counter++;\n}\n\n__attribute__((interrupt)) void real_isr(void) {\n    counter++;\n}\n";
        let tree = parse_c(source);
        let handlers = interrupt_handlers(tree.root_node(), source);
        assert_eq!(handlers.len(), 2);
    }

    #[cfg(feature = "lang-cpp")]
    #[test]
    fn cpp_gnu_attribute_is_detected() {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .unwrap();
        let source = "__attribute__((interrupt)) void real_isr(void) {\n    counter++;\n}\n";
        let tree = parser.parse(source, None).unwrap();
        let handlers = interrupt_handlers(tree.root_node(), source);
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0].name.as_deref(), Some("real_isr"));
    }

    #[cfg(feature = "lang-cpp")]
    #[test]
    fn cpp_c23_attribute_is_detected() {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .unwrap();
        let source = "[[gnu::interrupt]] void c23_isr(void *frame) {\n    counter++;\n}\n";
        let tree = parser.parse(source, None).unwrap();
        let handlers = interrupt_handlers(tree.root_node(), source);
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0].name.as_deref(), Some("c23_isr"));
    }
}
