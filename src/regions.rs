//! Inline `tools:off` / `tools:on` region scanning.
//!
//! See `docs/unified-config-spec.md` for the full comment syntax. This module
//! covers only the block-region form (`tools:off [TOOL[,TOOL,...]]` /
//! `tools:on`) — the precursor primitive that knots, moldy, and tools_sqc all
//! need identically. The richer `tools:suppress TOOL:RULE` single-line syntax
//! builds on top of this and is out of scope here.

use crate::registry::SlocMode;
use std::ops::Range;

/// A source region marked off by a `tools:off` / `tools:on` comment pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoredRegion {
    /// Byte offsets into the source, spanning from the start of the
    /// `tools:off` line to the end of the matching `tools:on` line.
    pub byte_range: Range<usize>,
    /// 1-indexed, inclusive line numbers for the same span.
    pub line_range: Range<usize>,
    /// Tools this region applies to. `None` means all tools (no qualifier on
    /// `tools:off`).
    pub tools: Option<Vec<String>>,
}

impl IgnoredRegion {
    /// Whether this region scopes the given tool (e.g. `"knots"`, `"funky"`,
    /// `"sqc"`). An unqualified `tools:off` (no tool list) scopes every tool.
    pub fn applies_to(&self, tool: &str) -> bool {
        match &self.tools {
            None => true,
            Some(tools) => tools.iter().any(|t| t.eq_ignore_ascii_case(tool)),
        }
    }
}

/// Returns the line-comment prefixes and, if any, the single-line block
/// comment delimiters for a given [`SlocMode`].
fn comment_syntax(
    mode: SlocMode,
) -> (
    &'static [&'static str],
    Option<(&'static str, &'static str)>,
) {
    match mode {
        SlocMode::Default => (&["//"], Some(("/*", "*/"))),
        SlocMode::Python => (&["#"], None),
        SlocMode::Ada => (&["--"], None),
        SlocMode::Lua => (&["--"], Some(("--[[", "]]"))),
        SlocMode::Fortran => (&["!"], None),
    }
}

/// If `line` is entirely a comment under `mode`'s syntax, returns the
/// trimmed text inside the comment. Otherwise `None`.
///
/// Shared with [`crate::suppressions`], which recognizes a different marker
/// vocabulary (`tools:suppress TOOL:RULE`) inside the same comment forms.
pub(crate) fn marker_text(line: &str, mode: SlocMode) -> Option<&str> {
    let trimmed = line.trim();
    let (line_prefixes, block) = comment_syntax(mode);

    for prefix in line_prefixes {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return Some(rest.trim());
        }
    }

    if let Some((open, close)) = block {
        if let Some(rest) = trimmed.strip_prefix(open) {
            if let Some(body) = rest.strip_suffix(close) {
                return Some(body.trim());
            }
        }
    }

    None
}

enum Marker {
    Off(Option<Vec<String>>),
    On,
}

/// Parses comment text as a `tools:off`/`tools:on` marker. Returns `None` if
/// the text isn't a recognized marker (i.e. it's an ordinary comment).
fn parse_marker(text: &str) -> Option<Marker> {
    if text == "tools:on" {
        return Some(Marker::On);
    }
    if text == "tools:off" {
        return Some(Marker::Off(None));
    }
    if let Some(rest) = text.strip_prefix("tools:off ") {
        let tools: Vec<String> = rest
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !tools.is_empty() {
            return Some(Marker::Off(Some(tools)));
        }
    }
    None
}

/// Scans `source` for `tools:off` / `tools:on` region markers, using the
/// comment syntax implied by `sloc_mode`.
///
/// Nested `tools:off` markers are ignored until the next `tools:on` closes
/// the outermost region. An unclosed `tools:off` extends to end of file.
pub fn ignored_regions(source: &str, sloc_mode: SlocMode) -> Vec<IgnoredRegion> {
    let mut regions = Vec::new();
    let mut pending: Option<(usize, usize, Option<Vec<String>>)> = None;
    let mut byte_offset = 0usize;
    let mut last_line_no = 0usize;
    let mut source_len = 0usize;

    for (idx, line) in source.split_inclusive('\n').enumerate() {
        let line_no = idx + 1;
        last_line_no = line_no;
        let line_start_byte = byte_offset;
        let content = line.strip_suffix('\n').unwrap_or(line);

        if let Some(text) = marker_text(content, sloc_mode) {
            match parse_marker(text) {
                Some(Marker::Off(tools)) if pending.is_none() => {
                    pending = Some((line_start_byte, line_no, tools));
                }
                Some(Marker::Off(_)) => {}
                Some(Marker::On) => {
                    if let Some((start_byte, start_line, tools)) = pending.take() {
                        regions.push(IgnoredRegion {
                            byte_range: start_byte..(line_start_byte + content.len()),
                            line_range: start_line..line_no,
                            tools,
                        });
                    }
                }
                None => {}
            }
        }

        byte_offset += line.len();
        source_len = byte_offset;
    }

    if let Some((start_byte, start_line, tools)) = pending {
        regions.push(IgnoredRegion {
            byte_range: start_byte..source_len,
            line_range: start_line..last_line_no,
            tools,
        });
    }

    regions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unqualified_block_applies_to_every_tool() {
        let src = "a();\n/* tools:off */\nb();\nc();\n/* tools:on */\nd();\n";
        let regions = ignored_regions(src, SlocMode::Default);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].line_range, 2..5);
        assert!(regions[0].applies_to("knots"));
        assert!(regions[0].applies_to("funky"));
        let region_text = &src[regions[0].byte_range.clone()];
        assert!(region_text.contains("b();"));
        assert!(region_text.contains("c();"));
        assert!(!region_text.contains("a();"));
        assert!(!region_text.contains("d();"));
    }

    #[test]
    fn qualified_block_scopes_named_tools_only() {
        let src = "/* tools:off funky */\nint m[] = {1,0};\n/* tools:on */\n";
        let regions = ignored_regions(src, SlocMode::Default);
        assert_eq!(regions.len(), 1);
        assert!(regions[0].applies_to("funky"));
        assert!(!regions[0].applies_to("knots"));
    }

    #[test]
    fn multi_tool_qualifier_splits_on_comma() {
        let src = "// tools:off knots,sqc\nx();\n// tools:on\n";
        let regions = ignored_regions(src, SlocMode::Default);
        assert_eq!(regions.len(), 1);
        assert!(regions[0].applies_to("knots"));
        assert!(regions[0].applies_to("sqc"));
        assert!(!regions[0].applies_to("funky"));
    }

    #[test]
    fn python_line_comment_syntax() {
        let src = "a()\n# tools:off\nb()\n# tools:on\nc()\n";
        let regions = ignored_regions(src, SlocMode::Python);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].line_range, 2..4);
    }

    #[test]
    fn ada_and_fortran_dash_and_bang_comments() {
        let src = "-- tools:off\nx;\n-- tools:on\n";
        let regions = ignored_regions(src, SlocMode::Ada);
        assert_eq!(regions.len(), 1);

        let src = "! tools:off\nx\n! tools:on\n";
        let regions = ignored_regions(src, SlocMode::Fortran);
        assert_eq!(regions.len(), 1);
    }

    #[test]
    fn nested_off_markers_are_ignored_until_outer_on() {
        let src = "// tools:off\na();\n// tools:off\nb();\n// tools:on\nc();\n";
        let regions = ignored_regions(src, SlocMode::Default);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].line_range, 1..5);
    }

    #[test]
    fn unclosed_off_extends_to_end_of_file() {
        let src = "a();\n// tools:off\nb();\nc();\n";
        let regions = ignored_regions(src, SlocMode::Default);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].line_range, 2..4);
        assert_eq!(regions[0].byte_range.end, src.len());
    }

    #[test]
    fn no_markers_yields_no_regions() {
        let src = "a();\nb();\n// just a normal comment\n";
        assert!(ignored_regions(src, SlocMode::Default).is_empty());
    }

    #[test]
    fn ordinary_comment_is_not_a_marker() {
        let src = "// tools:offline mode flag\nx();\n";
        assert!(ignored_regions(src, SlocMode::Default).is_empty());
    }

    #[test]
    fn empty_source_yields_no_regions() {
        assert!(ignored_regions("", SlocMode::Default).is_empty());
    }
}
