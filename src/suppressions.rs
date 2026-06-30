//! Inline `tools:suppress TOOL:RULE` single-line suppression comments.
//!
//! Builds on the comment recognition in [`crate::regions`]. See
//! `docs/unified-config-spec.md` for the full syntax. Block-region
//! suppression (`tools:off` / `tools:on`) lives in [`crate::regions`] —
//! this module covers only the single-line form.
//!
//! Resolving the suppressed *statement* (e.g. "the enclosing function" for
//! knots metrics) is tool-specific and out of scope here: this module
//! anchors each suppression to the comment line and the next non-blank
//! line, and each tool maps that anchor to its own AST notion of "the
//! following statement."

use crate::regions::marker_text;
use crate::registry::SlocMode;

/// A single-line `tools:suppress` comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suppression {
    /// 1-indexed line number of the `tools:suppress` comment itself.
    pub comment_line: usize,
    /// 1-indexed line number of the next non-blank line after the comment —
    /// the statement being suppressed. `None` if the comment is the last
    /// non-blank content in the file.
    pub target_line: Option<usize>,
    /// Tool name, e.g. `"knots"`, `"sqc"`, `"funky"`.
    pub tool: String,
    /// Rule or metric ID within that tool, e.g. `"cognitive"`, `"INT30-C"`.
    pub rule: String,
    /// Truncated SHA-256 tamper-detection hash — required by sqc, unused by
    /// other tools.
    pub hash: Option<String>,
    /// Free-text justification, if present.
    pub justification: Option<String>,
}

impl Suppression {
    /// Whether this suppression covers `tool`'s `rule`. Tool names match
    /// case-insensitively (consistent with
    /// [`crate::regions::IgnoredRegion::applies_to`]); rule IDs are
    /// case-sensitive since IDs like `INT30-C` are case-significant.
    pub fn applies_to(&self, tool: &str, rule: &str) -> bool {
        self.tool.eq_ignore_ascii_case(tool) && self.rule == rule
    }
}

struct ParsedSuppress {
    tool: String,
    rule: String,
    hash: Option<String>,
    justification: Option<String>,
}

/// Splits `s` on whitespace, treating double-quoted spans (e.g.
/// `JUSTIFICATION:"legacy, JIRA-123"`) as a single token.
fn tokenize(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for c in s.chars() {
        if c == '"' {
            in_quotes = !in_quotes;
            current.push(c);
        } else if c.is_whitespace() && !in_quotes {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Parses comment text as a `tools:suppress TOOL:RULE [HASH:...]
/// [JUSTIFICATION:"..."]` marker. Returns `None` if the text isn't a
/// recognized marker.
fn parse_suppress(text: &str) -> Option<ParsedSuppress> {
    let rest = text.strip_prefix("tools:suppress ")?;
    let mut tokens = tokenize(rest.trim()).into_iter();

    let head = tokens.next()?;
    let (tool, rule) = head.split_once(':')?;
    if tool.is_empty() || rule.is_empty() {
        return None;
    }

    let mut hash = None;
    let mut justification = None;
    for tok in tokens {
        if let Some(v) = tok.strip_prefix("HASH:") {
            hash = Some(v.to_string());
        } else if let Some(v) = tok.strip_prefix("JUSTIFICATION:") {
            justification = Some(v.trim_matches('"').to_string());
        }
    }

    Some(ParsedSuppress {
        tool: tool.to_string(),
        rule: rule.to_string(),
        hash,
        justification,
    })
}

/// Scans `source` for `tools:suppress TOOL:RULE` comments, using the comment
/// syntax implied by `sloc_mode`.
pub fn suppressions(source: &str, sloc_mode: SlocMode) -> Vec<Suppression> {
    let lines: Vec<&str> = source.lines().collect();
    let mut out = Vec::new();

    for (idx, line) in lines.iter().enumerate() {
        let comment_line = idx + 1;
        let Some(text) = marker_text(line, sloc_mode) else {
            continue;
        };
        let Some(parsed) = parse_suppress(text) else {
            continue;
        };

        let target_line = lines[comment_line..]
            .iter()
            .position(|l| !l.trim().is_empty())
            .map(|offset| comment_line + offset + 1);

        out.push(Suppression {
            comment_line,
            target_line,
            tool: parsed.tool,
            rule: parsed.rule,
            hash: parsed.hash,
            justification: parsed.justification,
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqc_suppression_with_hash_and_justification() {
        let src = "// tools:suppress sqc:INT30-C HASH:abc123def456789a JUSTIFICATION:\"validated\"\nuint32_t x = y + z;\n";
        let s = suppressions(src, SlocMode::Default);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].comment_line, 1);
        assert_eq!(s[0].target_line, Some(2));
        assert_eq!(s[0].tool, "sqc");
        assert_eq!(s[0].rule, "INT30-C");
        assert_eq!(s[0].hash.as_deref(), Some("abc123def456789a"));
        assert_eq!(s[0].justification.as_deref(), Some("validated"));
        assert!(s[0].applies_to("sqc", "INT30-C"));
        assert!(s[0].applies_to("SQC", "INT30-C"));
        assert!(!s[0].applies_to("sqc", "INT31-C"));
    }

    #[test]
    fn knots_suppression_without_hash() {
        let src = "// tools:suppress knots:cognitive JUSTIFICATION:\"legacy, JIRA-123\"\nvoid big_function() {}\n";
        let s = suppressions(src, SlocMode::Default);
        assert_eq!(s.len(), 1);
        assert!(s[0].hash.is_none());
        assert_eq!(s[0].justification.as_deref(), Some("legacy, JIRA-123"));
    }

    #[test]
    fn skips_blank_lines_to_find_target() {
        let src = "// tools:suppress knots:cognitive\n\n\nfn big() {}\n";
        let s = suppressions(src, SlocMode::Default);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].target_line, Some(4));
    }

    #[test]
    fn no_target_when_comment_is_last_content() {
        let src = "fn a() {}\n// tools:suppress knots:cognitive\n";
        let s = suppressions(src, SlocMode::Default);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].target_line, None);
    }

    #[test]
    fn python_comment_syntax() {
        let src = "# tools:suppress knots:cognitive JUSTIFICATION:\"legacy\"\ndef big_function():\n    pass\n";
        let s = suppressions(src, SlocMode::Python);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].tool, "knots");
        assert_eq!(s[0].target_line, Some(2));
    }

    #[test]
    fn malformed_tool_rule_pair_is_ignored() {
        let src = "// tools:suppress knots\nfn a() {}\n";
        assert!(suppressions(src, SlocMode::Default).is_empty());
    }

    #[test]
    fn ordinary_comment_is_not_a_suppression() {
        let src = "// suppress everything please\nfn a() {}\n";
        assert!(suppressions(src, SlocMode::Default).is_empty());
    }

    #[test]
    fn no_markers_yields_no_suppressions() {
        let src = "fn a() {}\nfn b() {}\n";
        assert!(suppressions(src, SlocMode::Default).is_empty());
    }
}
