//! Markdown structure over the `markdown` block grammar: container-prefix
//! stripping ([`ContentMap`]), heading sections ([`sections`]) and front
//! matter ([`front_matter`]).
//!
//! The block grammar leaves container syntax inside leaf-block ranges: a
//! paragraph in a block quote spans the `> ` of its second line, and one in a
//! list item spans the item's indentation. Those prefixes are
//! `block_continuation`, `block_quote_marker` and `list_marker_*` nodes. On a
//! row that opens a list item or quote there is no `block_continuation`, only
//! the marker, so a paragraph or fence that starts on the `- ` line needs the
//! marker's end too. [`ContentMap`] records, per row, the byte where content
//! starts after all of them. Use it before re-parsing an `inline` node with
//! the `markdown_inline` grammar, or before reading a code fence's content.
//!
//! [`sections`] splits a document at its headings, ATX (`# Title`) and setext
//! (`Title` over `===` / `---`). A `#` line inside a code fence is never a
//! heading, because the grammar parses the fence first. Only document-level
//! headings start a section: a heading inside a block quote or list item is
//! part of the section around it. Sections are flat — each runs to the next
//! heading of any level — and carry their level, so a caller that wants a
//! hierarchy builds it from the levels.
//!
//! Known gap: the result is only as good as the grammar's parse. The grammar
//! sometimes closes a code fence early, on a delimiter-like line inside it
//! that CommonMark would not accept as a closing fence (indented four
//! spaces, or after a ` * ` comment prefix). The rest of the document is then
//! out of step, and a heading after it can be missed. Across about 6,000
//! real-world Markdown files this affected a handful of files, about 0.1% of
//! headings.

use crate::query::{walk_preorder, Walk};
use std::ops::Range;
use tree_sitter::Node;

/// Per-row content start for a Markdown tree, after container prefixes.
#[derive(Debug, Clone)]
pub struct ContentMap {
    /// Byte offset of the start of each row.
    line_starts: Vec<usize>,
    /// Byte where content begins on each row; the row's start when the row
    /// has no prefix.
    content_starts: Vec<usize>,
}

impl ContentMap {
    /// Builds the map in one walk of the tree rooted at `root`, parsed from
    /// `source` with the `markdown` grammar.
    pub fn new(root: Node, source: &str) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(source.match_indices('\n').map(|(i, _)| i + 1));
        let mut content_starts = line_starts.clone();
        walk_preorder(root, |node| {
            let kind = node.kind();
            if kind == "block_continuation"
                || kind == "block_quote_marker"
                || kind.starts_with("list_marker_")
            {
                if let Some(start) = content_starts.get_mut(node.start_position().row) {
                    *start = (*start).max(node.end_byte());
                }
            }
            Walk::Continue
        });
        ContentMap {
            line_starts,
            content_starts,
        }
    }

    /// Byte where content starts on `row` (0-indexed), after any container
    /// prefix.
    pub fn content_start(&self, row: usize) -> usize {
        self.content_starts[row]
    }

    /// End of `row`'s text, before its `\n` or `\r\n`.
    fn line_end(&self, row: usize, source: &str) -> usize {
        let end = self
            .line_starts
            .get(row + 1)
            .map_or(source.len(), |next| next - 1);
        if end > self.line_starts[row] && source.as_bytes()[end - 1] == b'\r' {
            end - 1
        } else {
            end
        }
    }

    /// `node`'s text, one entry per source row, without container prefixes
    /// or line endings. The first row starts at the node itself; a final row
    /// that the node reaches only through a prefix is dropped.
    pub fn content_lines<'s>(&self, node: Node, source: &'s str) -> Vec<&'s str> {
        let first = node.start_position().row;
        let last = node.end_position().row.min(self.line_starts.len() - 1);
        let mut lines = Vec::new();
        for row in first..=last {
            let start = if row == first {
                node.start_byte()
            } else {
                self.content_start(row)
            };
            if row > first && row == last && node.end_byte() <= start {
                break;
            }
            let end = self.line_end(row, source).min(node.end_byte()).max(start);
            lines.push(&source[start..end]);
        }
        lines
    }
}

/// A document-level heading and the text up to the next one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// 1–6 for a heading; 0 for the preamble, the text before the first
    /// heading (after any front matter), reported only when it is not blank.
    pub level: u8,
    /// The heading's text as written, inline markup included, without the
    /// `#` markers, a closing `#` sequence, or a setext underline. A
    /// multi-line setext heading's lines are joined with spaces. Empty for
    /// the preamble and for a bare `#`.
    pub heading_text: String,
    /// Bytes of the heading line(s), line ending included. Empty, at the
    /// section's start, for the preamble.
    pub heading_range: Range<usize>,
    /// Bytes from the end of the heading to the next document-level heading
    /// or the end of the document.
    pub body_range: Range<usize>,
    /// 1-indexed first line of the whole section.
    pub start_line: usize,
    /// 1-indexed last line of the whole section, inclusive.
    pub end_line: usize,
}

impl Section {
    /// Bytes of the whole section, heading and body.
    pub fn byte_range(&self) -> Range<usize> {
        self.heading_range.start..self.body_range.end
    }
}

/// The document's sections in order. `root` is a tree parsed from `source`
/// with the `markdown` grammar. See the module docs for what starts one.
pub fn sections(root: Node, source: &str) -> Vec<Section> {
    let map = ContentMap::new(root, source);
    let headings = document_headings(root);
    let doc_end = root.end_byte();
    let line_of = |byte: usize| map.line_starts.partition_point(|&s| s <= byte);

    let mut out = Vec::new();
    let body_start = front_matter(root, source).map_or(root.start_byte(), |f| f.byte_range.end);
    let preamble_end = headings.first().map_or(doc_end, |h| h.start_byte());
    if !source[body_start..preamble_end].trim().is_empty() {
        out.push(Section {
            level: 0,
            heading_text: String::new(),
            heading_range: body_start..body_start,
            body_range: body_start..preamble_end,
            start_line: line_of(body_start),
            end_line: line_of(preamble_end - 1),
        });
    }
    for (i, heading) in headings.iter().enumerate() {
        let end = headings
            .get(i + 1)
            .map_or(doc_end, |next| next.start_byte());
        let (level, heading_text) = heading_parts(*heading, &map, source);
        out.push(Section {
            level,
            heading_text,
            heading_range: heading.start_byte()..heading.end_byte(),
            body_range: heading.end_byte()..end,
            start_line: line_of(heading.start_byte()),
            end_line: line_of(end.max(heading.start_byte() + 1) - 1),
        });
    }
    out
}

/// ATX and setext headings that are children of the document or of its
/// `section` nodes, in document order — not those inside a quote or list.
fn document_headings(root: Node) -> Vec<Node> {
    let mut out = Vec::new();
    walk_preorder(root, |node| match node.kind() {
        "document" | "section" => Walk::Continue,
        "atx_heading" | "setext_heading" => {
            out.push(node);
            Walk::SkipChildren
        }
        _ => Walk::SkipChildren,
    });
    out
}

fn heading_parts(heading: Node, map: &ContentMap, source: &str) -> (u8, String) {
    let mut cursor = heading.walk();
    let kids: Vec<Node> = heading.children(&mut cursor).collect();
    if heading.kind() == "setext_heading" {
        let level = if kids.iter().any(|c| c.kind() == "setext_h2_underline") {
            2
        } else {
            1
        };
        let text = kids
            .iter()
            .find(|c| c.kind() == "paragraph")
            .and_then(|p| p.child(0).filter(|c| c.kind() == "inline"))
            .map(|inline| {
                map.content_lines(inline, source)
                    .iter()
                    .map(|l| l.trim())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        return (level, text);
    }
    let level = kids
        .iter()
        .find_map(|c| {
            c.kind()
                .strip_prefix("atx_h")?
                .strip_suffix("_marker")?
                .parse()
                .ok()
        })
        .unwrap_or(1);
    let text = kids
        .iter()
        .find(|c| c.kind() == "inline")
        .map_or("", |inline| &source[inline.byte_range()]);
    (level, strip_closing_sequence(text).to_string())
}

/// Drops an ATX heading's optional closing `#` run: CommonMark counts it only
/// when it is the whole text or follows a space or tab (`# C#` keeps its `#`).
fn strip_closing_sequence(text: &str) -> &str {
    let text = text.trim();
    let open = text.trim_end_matches('#');
    if open.is_empty() || open.ends_with([' ', '\t']) {
        open.trim_end()
    } else {
        text
    }
}

/// Front-matter syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontMatterKind {
    /// `---` delimiters.
    Yaml,
    /// `+++` delimiters.
    Toml,
}

/// Metadata block at the very start of a Markdown document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontMatter {
    /// `---` (YAML) or `+++` (TOML).
    pub kind: FrontMatterKind,
    /// Bytes of the whole block, delimiters and final line ending included.
    pub byte_range: Range<usize>,
    /// Bytes between the delimiter lines: the YAML or TOML text itself.
    pub body_range: Range<usize>,
    /// 1-indexed line of the opening delimiter.
    pub start_line: usize,
    /// 1-indexed line of the closing delimiter.
    pub end_line: usize,
}

/// The document's front matter, if it opens with a `---` or `+++` block.
/// `root` is a tree parsed from `source` with the `markdown` grammar.
pub fn front_matter(root: Node, source: &str) -> Option<FrontMatter> {
    let node = root.named_child(0)?;
    let kind = match node.kind() {
        "minus_metadata" => FrontMatterKind::Yaml,
        "plus_metadata" => FrontMatterKind::Toml,
        _ => return None,
    };
    let text = &source[node.byte_range()];
    let open_len = text.find('\n').map_or(text.len(), |i| i + 1);
    let close_start = text
        .trim_end_matches(['\n', '\r'])
        .rfind('\n')
        .map_or(open_len, |i| i + 1);
    Some(FrontMatter {
        kind,
        byte_range: node.byte_range(),
        body_range: node.start_byte() + open_len..node.start_byte() + close_start.max(open_len),
        start_line: node.start_position().row + 1,
        end_line: node.start_position().row
            + 1
            + text.trim_end_matches(['\n', '\r']).matches('\n').count(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_md::LANGUAGE.into())
            .unwrap();
        parser.parse(src, None).unwrap()
    }

    fn outline(src: &str) -> Vec<(u8, String, usize, usize)> {
        let tree = parse(src);
        sections(tree.root_node(), src)
            .into_iter()
            .map(|s| (s.level, s.heading_text, s.start_line, s.end_line))
            .collect()
    }

    const DOC: &str = "---\ntitle: x\n---\n\nIntro text.\n\n# Top #\n\nBody.\n\n```sh\n# not a heading\n```\n\nSetext\nTwo\n---\n\n> # quoted\n\n- item\n  ## in list\n\n### Third\n";

    #[test]
    fn sections_cover_atx_setext_and_preamble_but_not_fences_or_containers() {
        assert_eq!(
            outline(DOC),
            vec![
                (0, String::new(), 4, 6),
                (1, "Top".to_string(), 7, 14),
                (2, "Setext Two".to_string(), 15, 23),
                (3, "Third".to_string(), 24, 24),
            ]
        );
    }

    #[test]
    fn section_ranges_tile_the_body() {
        let tree = parse(DOC);
        let secs = sections(tree.root_node(), DOC);
        let fm = front_matter(tree.root_node(), DOC).unwrap();
        assert_eq!(secs[0].byte_range().start, fm.byte_range.end);
        for pair in secs.windows(2) {
            assert_eq!(pair[0].byte_range().end, pair[1].byte_range().start);
        }
        assert_eq!(secs.last().unwrap().byte_range().end, DOC.len());
        assert_eq!(&DOC[secs[1].heading_range.clone()], "# Top #\n");
        assert!(DOC[secs[1].body_range.clone()].contains("# not a heading"));
    }

    #[test]
    fn atx_closing_sequence_follows_commonmark() {
        let src = "# Empty\n#\n## Close ##\n# C#\n# esc \\#\n";
        let texts: Vec<String> = outline(src).into_iter().map(|s| s.1).collect();
        assert_eq!(texts, ["Empty", "", "Close", "C#", "esc \\#"]);
    }

    #[test]
    fn no_preamble_when_blank_and_no_sections_in_empty_doc() {
        assert_eq!(outline("\n\n# A\n").len(), 1);
        assert!(outline("").is_empty());
        assert_eq!(outline("just text\n"), vec![(0, String::new(), 1, 1)]);
    }

    #[test]
    fn crlf_and_bom() {
        let src = "\u{feff}# A\r\nbody\r\n\r\nB\r\n=\r\n";
        assert_eq!(
            outline(src),
            vec![(1, "A".to_string(), 1, 3), (1, "B".to_string(), 4, 5)]
        );
    }

    #[test]
    fn front_matter_yaml_and_toml() {
        let tree = parse(DOC);
        let fm = front_matter(tree.root_node(), DOC).unwrap();
        assert_eq!(fm.kind, FrontMatterKind::Yaml);
        assert_eq!(&DOC[fm.body_range.clone()], "title: x\n");
        assert_eq!((fm.start_line, fm.end_line), (1, 3));

        let src = "+++\ntitle = \"x\"\n+++\n\n# A\n";
        let tree = parse(src);
        let fm = front_matter(tree.root_node(), src).unwrap();
        assert_eq!(fm.kind, FrontMatterKind::Toml);
        assert_eq!(&src[fm.body_range.clone()], "title = \"x\"\n");

        let src = "# A\n\n---\nx: 1\n---\n";
        assert!(front_matter(parse(src).root_node(), src).is_none());
    }

    fn first_of_kind<'t>(root: Node<'t>, kind: &str) -> Node<'t> {
        crate::query::find_descendants_of_kind(root, kind)[0]
    }

    #[test]
    fn content_lines_strip_quote_and_list_prefixes() {
        let src = "> one\n> two\n";
        let tree = parse(src);
        let map = ContentMap::new(tree.root_node(), src);
        let inline = first_of_kind(tree.root_node(), "inline");
        assert_eq!(map.content_lines(inline, src), ["one", "two"]);

        let src = "- item\n  more\n";
        let tree = parse(src);
        let map = ContentMap::new(tree.root_node(), src);
        let inline = first_of_kind(tree.root_node(), "inline");
        assert_eq!(map.content_lines(inline, src), ["item", "more"]);
    }

    /// A fence that opens on the list-marker row: there is no
    /// `block_continuation` on that row, so the marker's end bounds it.
    #[test]
    fn content_lines_of_a_fence_opened_on_a_marker_row() {
        let src = "- ```\n  code\n  ```\n";
        let tree = parse(src);
        let map = ContentMap::new(tree.root_node(), src);
        assert_eq!(map.content_start(0), 2);
        let content = first_of_kind(tree.root_node(), "code_fence_content");
        assert_eq!(map.content_lines(content, src), ["code"]);
    }

    #[test]
    fn content_lines_of_nested_quote_in_list() {
        let src = "- > a\n  > b\n";
        let tree = parse(src);
        let map = ContentMap::new(tree.root_node(), src);
        let inline = first_of_kind(tree.root_node(), "inline");
        assert_eq!(map.content_lines(inline, src), ["a", "b"]);
    }
}
