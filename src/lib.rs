pub mod imports;
pub mod path_ignore;
pub mod regions;
pub mod registry;
pub mod suppressions;

pub use imports::{distinct_import_count, import_sources};
pub use path_ignore::PathIgnore;
pub use regions::{ignored_regions, IgnoredRegion};
pub use registry::{
    is_parseable_extension, is_source_extension, language_for_file, language_for_key,
    language_info_for_file, languages, sloc_mode_for_file, supported_languages_report,
    LanguageInfo, SlocMode,
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
