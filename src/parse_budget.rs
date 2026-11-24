//! Parsing with a work budget, so a malformed input costs a bounded amount.
//!
//! tree-sitter's error recovery can be quadratic in the input. A header made
//! of `/* x...` followed by N lines of ` */` takes about 0.45 s at N = 1,000
//! and about 7.6 s at N = 4,000 (the C and C++ grammars alike), and a
//! 100 KB file of that shape runs for minutes. [`parse_within_budget`]
//! stops such a parse and says so, rather than letting the caller hang or
//! quietly guess.
//!
//! The budget counts **steps**: one step is one call of tree-sitter's
//! progress callback, which the parser makes every 100 internal operations.
//! A step count is a property of the input and the grammar, not of the
//! machine, so a parse that exceeds its budget exceeds it everywhere, and a
//! refusal can be reproduced. It is not stable across tree-sitter or grammar
//! upgrades, which can change how many operations a parse takes; re-measure
//! the default after one.
//!
//! [`ParseBudget::default`] allows 2,000 steps plus a quarter of a step per
//! input byte. Measured with tree-sitter 0.25.10, tree-sitter-c 0.24.2 and
//! tree-sitter-cpp 0.23.4 over the 5,837 `.c` and `.h` files of twelve real
//! C codebases, no file needed more than 0.023 steps per byte, and no file
//! under 2 KB more than 22 steps: the default leaves a margin of over ten
//! times. The quadratic shape above needs 2.5 steps per byte at N = 1,000
//! and 10 at N = 4,000.

use std::fmt;

use tree_sitter::{ParseOptions, ParseState, Parser, Tree};

/// How much work a parse may do: [`ParseBudget::steps_for`] steps for an
/// input of a given length. See the [module docs](self) for what a step is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseBudget {
    /// Steps allowed whatever the input's length.
    pub base_steps: u64,
    /// Further steps allowed per 1,024 input bytes.
    pub steps_per_kib: u64,
}

impl ParseBudget {
    /// The default: 2,000 steps plus 256 per KiB (a quarter of a step per
    /// byte).
    pub const DEFAULT: Self = Self {
        base_steps: 2_000,
        steps_per_kib: 256,
    };

    /// No bound: the parse runs as long as tree-sitter takes.
    pub const UNLIMITED: Self = Self {
        base_steps: u64::MAX,
        steps_per_kib: 0,
    };

    /// The steps allowed for an input of `len` bytes.
    pub fn steps_for(&self, len: usize) -> u64 {
        let per_byte = self.steps_per_kib.saturating_mul(len as u64) / 1024;
        self.base_steps.saturating_add(per_byte)
    }
}

impl Default for ParseBudget {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A parse that [`parse_within_budget`] stopped because it reached its
/// budget. Carries what a caller needs to report the refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetExceeded {
    /// The steps the budget allowed for this input.
    pub budget_steps: u64,
    /// The input's length in bytes.
    pub source_len: usize,
    /// How far into the input the parser had read when it was stopped.
    pub byte_offset: usize,
}

impl fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "parse stopped at its budget of {} steps for {} bytes, at byte {}",
            self.budget_steps, self.source_len, self.byte_offset
        )
    }
}

impl std::error::Error for BudgetExceeded {}

/// Parses `source` with `parser`'s language, stopping once the parse has
/// taken more than `budget` allows for an input of this length.
///
/// `Ok(None)` is [`Parser::parse`]'s own `None`: the parser has no language
/// set. A parse stopped by the budget is `Err`, never a partial tree.
pub fn parse_within_budget(
    parser: &mut Parser,
    source: &[u8],
    budget: ParseBudget,
) -> Result<Option<Tree>, BudgetExceeded> {
    let budget_steps = budget.steps_for(source.len());
    let mut steps = 0u64;
    let mut stopped_at = None;
    let mut progress = |state: &ParseState| {
        steps += 1;
        let stop = steps > budget_steps;
        if stop {
            stopped_at = Some(state.current_byte_offset());
        }
        stop
    };
    let options = ParseOptions::new().progress_callback(&mut progress);
    let tree = parser.parse_with_options(
        &mut |offset, _| source.get(offset..).unwrap_or_default(),
        None,
        Some(options),
    );
    match stopped_at {
        Some(byte_offset) => Err(BudgetExceeded {
            budget_steps,
            source_len: source.len(),
            byte_offset,
        }),
        None => Ok(tree),
    }
}

#[cfg(all(test, feature = "lang-c"))]
mod tests {
    use super::*;

    fn c_parser() -> Parser {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_c::LANGUAGE.into())
            .unwrap();
        parser
    }

    /// The malformed-comment shape whose error recovery is quadratic.
    fn unclosed_comment_header(lines: usize) -> Vec<u8> {
        let mut src = format!("/* {}\n", "x".repeat(40)).into_bytes();
        for _ in 0..lines {
            src.extend_from_slice(b" */\n");
        }
        src
    }

    #[test]
    fn ordinary_source_parses_within_the_default() {
        let src = b"#include <stdio.h>\nint main(void) { printf(\"hi\\n\"); return 0; }\n";
        let tree = parse_within_budget(&mut c_parser(), src, ParseBudget::default())
            .unwrap()
            .unwrap();
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn quadratic_recovery_is_stopped_and_reported() {
        let src = unclosed_comment_header(4_000);
        let start = std::time::Instant::now();
        let err = parse_within_budget(&mut c_parser(), &src, ParseBudget::default()).unwrap_err();
        assert_eq!(err.source_len, src.len());
        assert_eq!(err.budget_steps, ParseBudget::DEFAULT.steps_for(src.len()));
        assert!(err.byte_offset <= src.len());
        // Unbounded, this parse takes about 8 s; the default stops it after
        // about 6,000 steps, well under a second. 5 s leaves room for a debug
        // build on a loaded machine without letting the quadratic parse pass.
        assert!(start.elapsed().as_secs() < 5, "{:?}", start.elapsed());
    }

    #[test]
    fn the_refusal_is_deterministic() {
        let src = unclosed_comment_header(1_000);
        let first = parse_within_budget(&mut c_parser(), &src, ParseBudget::default());
        let second = parse_within_budget(&mut c_parser(), &src, ParseBudget::default());
        let (Err(first), Err(second)) = (first, second) else {
            panic!("expected both parses to reach the budget");
        };
        assert_eq!(first, second);
    }

    #[test]
    fn unlimited_budget_never_stops() {
        let src = unclosed_comment_header(300);
        assert!(parse_within_budget(&mut c_parser(), &src, ParseBudget::UNLIMITED).is_ok());
    }

    #[test]
    fn a_parser_without_a_language_is_ok_none() {
        assert!(matches!(
            parse_within_budget(&mut Parser::new(), b"int x;", ParseBudget::default()),
            Ok(None)
        ));
    }

    #[test]
    fn steps_for_saturates() {
        let huge = ParseBudget {
            base_steps: u64::MAX - 1,
            steps_per_kib: u64::MAX,
        };
        assert_eq!(huge.steps_for(usize::MAX), u64::MAX);
        assert_eq!(ParseBudget::DEFAULT.steps_for(0), 2_000);
        assert_eq!(ParseBudget::DEFAULT.steps_for(4096), 3_024);
    }
}
