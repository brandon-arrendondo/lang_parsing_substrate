//! A shared language-parsing substrate for static-analysis tools:
//! tree-sitter grammar dispatch, language detection ([`registry`]), and a
//! growing set of language-agnostic analysis primitives — import/call
//! graphs, control-flow graphs, structural fingerprinting, suppression
//! comments — built on top of a unified [`registry::LanguageInfo`] table
//! across 16 languages, compiled in at build time via Cargo feature flags.
//! Consumers include knots, moldy, and tools_sqc; see this crate's README
//! for the full module-to-purpose table.

#![warn(missing_docs)]

pub mod c_standard;
pub mod calls;
pub mod cfg;
#[cfg(all(feature = "lang-c", feature = "lang-cpp"))]
pub mod cpp_header;
#[cfg(any(feature = "lang-c", feature = "lang-cpp", feature = "lang-csharp"))]
pub mod dead_code;
#[cfg(feature = "lang-csharp")]
pub mod dead_code_csharp;
#[cfg(feature = "lang-swift")]
pub mod dead_code_swift;
pub mod fingerprint;
pub mod imports;
#[cfg(any(feature = "lang-c", feature = "lang-cpp"))]
pub mod isr;
pub mod path_ignore;
#[cfg(feature = "pyo3")]
mod py;
pub mod query;
pub mod regions;
pub mod registry;
pub mod suppressions;

pub use c_standard::{detect_min_c_standard, CStandard};
pub use calls::{call_edges, collect_local_names, get_function_name, is_function_kind, CallEdge};
pub use cfg::{build_function_cfg, BasicBlock, BlockId, CfgEdge, FunctionCfg};
#[cfg(all(feature = "lang-c", feature = "lang-cpp"))]
pub use cpp_header::looks_like_cpp;
#[cfg(any(feature = "lang-c", feature = "lang-cpp", feature = "lang-csharp"))]
pub use dead_code::{dead_code_ranges, DeadCodeReason, DeadCodeRegion};
#[cfg(feature = "lang-csharp")]
pub use dead_code_csharp::{csharp_dead_code_regions, CSharpDeadCodeRegion};
#[cfg(feature = "lang-swift")]
pub use dead_code_swift::{swift_dead_code_regions, SwiftDeadCodeRegion};
pub use fingerprint::{
    duplicate_groups, function_fingerprints, structural_hash, CorpusFingerprint, Fingerprint,
};
pub use imports::{distinct_import_count, import_sources};
#[cfg(any(feature = "lang-c", feature = "lang-cpp"))]
pub use isr::{interrupt_handlers, InterruptEvidence, InterruptHandler};
pub use path_ignore::PathIgnore;
pub use query::{
    find_ancestor, find_descendants, find_descendants_of_kind, find_descendants_of_kinds,
    find_first_descendant, nearest_ancestor_of_kind, nearest_ancestor_of_kinds, node_text,
};
pub use regions::{ignored_regions, IgnoredRegion};
#[cfg(all(feature = "lang-c", feature = "lang-cpp"))]
pub use registry::language_for_header_content;
pub use registry::{
    is_extension_for_language, is_parseable_extension, is_source_extension, language_for_file,
    language_for_key, language_info_for_file, languages, sloc_mode_for_file,
    supported_languages_report, LanguageInfo, SlocMode,
};
pub use suppressions::{suppressions, Suppression};

// Grammar re-exports — gated by feature so consumers reach grammars without
// adding their own direct tree-sitter-* dependencies.
#[cfg(feature = "lang-ada")]
pub use tree_sitter_ada;
#[cfg(feature = "lang-c")]
pub use tree_sitter_c;
#[cfg(feature = "lang-csharp")]
pub use tree_sitter_c_sharp;
#[cfg(feature = "lang-cpp")]
pub use tree_sitter_cpp;
#[cfg(feature = "lang-fortran")]
pub use tree_sitter_fortran;
#[cfg(feature = "lang-go")]
pub use tree_sitter_go;
#[cfg(feature = "lang-java")]
pub use tree_sitter_java;
#[cfg(feature = "lang-javascript")]
pub use tree_sitter_javascript;
#[cfg(feature = "lang-kotlin")]
pub use tree_sitter_kotlin_ng;
#[cfg(feature = "lang-lua")]
pub use tree_sitter_lua;
#[cfg(feature = "lang-php")]
pub use tree_sitter_php;
#[cfg(feature = "lang-python")]
pub use tree_sitter_python;
#[cfg(feature = "lang-rust")]
pub use tree_sitter_rust;
#[cfg(feature = "lang-scala")]
pub use tree_sitter_scala;
#[cfg(feature = "lang-swift")]
pub use tree_sitter_swift;
#[cfg(feature = "lang-typescript")]
pub use tree_sitter_typescript;
