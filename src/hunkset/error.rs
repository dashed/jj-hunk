use crate::errors::{self, CodedError, ErrorCode};
use serde_json::{Map, Value};
use thiserror::Error;

/// One of the hunks an abbreviated id reached.
///
/// Structured rather than pre-joined so that [`HunksetError::details`] can
/// hand a caller the ids to retry with. Before this, the only way to recover
/// them was to split the rendered message on `", "`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdCandidate {
    /// The abbreviation `list` prints, which is unambiguous over this diff.
    pub short_id: String,
    /// The path the hunk's file has now.
    pub path: String,
}

impl std::fmt::Display for IdCandidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.short_id, self.path)
    }
}

fn render_candidates(candidates: &[IdCandidate]) -> String {
    candidates
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Error)]
pub enum HunksetError {
    #[error("{message}")]
    Parse {
        message: String,
        input: String,
        position: usize,
    },
    #[error("unknown function '{name}'")]
    UnknownFunction { name: String },
    #[error("invalid regex '{pattern}': {source}")]
    InvalidRegex {
        pattern: String,
        source: regex::Error,
    },
    /// `GlobError` already renders as `invalid glob '<pattern>': <reason>`,
    /// so it is passed through rather than re-worded here.
    #[error("{source}")]
    InvalidGlob { source: crate::glob::GlobError },
    #[error("hunk id '{id}' matches no hunk in this diff -- ids change when the hunk's content or its file changes, so one copied from an earlier listing may be stale. Run 'list' again for the current ids.")]
    UnknownId { id: String },
    #[error("hunk id '{prefix}' is ambiguous -- it matches {} hunks: {}. Use more characters, or exact:\"<full-id>\".", .candidates.len(), render_candidates(.candidates))]
    AmbiguousId {
        prefix: String,
        /// Sorted by their rendered form, so the message and the details agree
        /// on an order and neither depends on hash iteration.
        candidates: Vec<IdCandidate>,
    },
    #[error("{func}() does not accept '{value}' -- valid values are: {valid}")]
    InvalidArgument {
        func: String,
        value: String,
        valid: String,
    },
    #[error("{name}() requires the 'semantic' feature (build with --features semantic)")]
    #[allow(dead_code)] // only constructed when semantic feature is disabled
    SemanticFeatureRequired { name: String },
}

/// Longest run of input shown above the caret before it is windowed.
///
/// An expression can be far wider than a terminal -- a generated one, or the
/// wall of parentheses that trips the parser's nesting limit -- and echoing
/// all of it buries the message under thousands of characters of noise.
const MAX_CONTEXT_WIDTH: usize = 100;

const ELLIPSIS: &str = "...";

impl HunksetError {
    /// The stable code a caller branches on.
    ///
    /// Total by construction: adding a variant without giving it a code does
    /// not compile, so the taxonomy cannot quietly grow a hole.
    pub fn code(&self) -> ErrorCode {
        match self {
            HunksetError::Parse { .. } => errors::PARSE_ERROR,
            HunksetError::UnknownFunction { .. } => errors::UNKNOWN_FUNCTION,
            HunksetError::InvalidRegex { .. } => errors::INVALID_REGEX,
            HunksetError::InvalidGlob { .. } => errors::INVALID_GLOB,
            HunksetError::UnknownId { .. } => errors::UNKNOWN_ID,
            HunksetError::AmbiguousId { .. } => errors::AMBIGUOUS_ID,
            HunksetError::InvalidArgument { .. } => errors::INVALID_ARGUMENT,
            HunksetError::SemanticFeatureRequired { .. } => errors::SEMANTIC_FEATURE_REQUIRED,
        }
    }

    /// The parts of the failure that were previously readable only by parsing
    /// the message.
    pub fn details(&self) -> Map<String, Value> {
        let mut details = Map::new();
        let mut put = |key: &str, value: Value| {
            details.insert(key.to_string(), value);
        };

        match self {
            HunksetError::Parse { input, position, .. } => {
                let (_, column) = locate(input, *position);
                put("input", Value::from(input.as_str()));
                // Character offset into the whole expression, which is what
                // the caret is placed from.
                put("position", Value::from(*position));
                put("line", Value::from(line_number(input, *position)));
                // Zero-based, within the offending line -- an expression may
                // span several, and `position` alone points past the end of
                // the first one.
                put("column", Value::from(column));
            }
            HunksetError::UnknownFunction { name } => put("function", Value::from(name.as_str())),
            HunksetError::InvalidRegex { pattern, source } => {
                put("pattern", Value::from(pattern.as_str()));
                put("reason", Value::from(source.to_string()));
            }
            HunksetError::InvalidGlob { source } => {
                put("pattern", Value::from(source.pattern()));
                put("reason", Value::from(source.reason()));
            }
            HunksetError::UnknownId { id } => put("id", Value::from(id.as_str())),
            HunksetError::AmbiguousId { prefix, candidates } => {
                put("prefix", Value::from(prefix.as_str()));
                put("count", Value::from(candidates.len()));
                put(
                    "candidates",
                    Value::Array(
                        candidates
                            .iter()
                            .map(|c| {
                                serde_json::json!({"short_id": c.short_id, "path": c.path})
                            })
                            .collect(),
                    ),
                );
            }
            HunksetError::InvalidArgument { func, value, valid } => {
                put("function", Value::from(func.as_str()));
                put("value", Value::from(value.as_str()));
                put("valid", Value::from(valid.as_str()));
            }
            HunksetError::SemanticFeatureRequired { name } => {
                put("predicate", Value::from(name.as_str()))
            }
        }

        details
    }

    /// Carry this error's code and details up under the message the CLI
    /// prints for it.
    ///
    /// The two call sites in `commands.rs` used to render the error into a
    /// string and drop the value, which left the top level nothing to read but
    /// the text. The message is unchanged; only the code now survives with it.
    pub fn coded(&self, message: String) -> CodedError {
        CodedError::new(self.code(), message).with_details(self.details())
    }

    /// Format the error with a caret pointing at the position in the input.
    pub fn display_with_context(&self) -> String {
        match self {
            HunksetError::Parse { message, input, position } => {
                let (line, column) = locate(input, *position);
                let (shown, caret_column) = window(line, column);
                format!("{}\n{}^\n{}", shown, " ".repeat(caret_column), message)
            }
            other => format!("{}", other),
        }
    }
}

/// Split `position` (a character offset into the whole input) into the line it
/// falls on and the column within that line.
///
/// An expression may span several lines -- whitespace, newlines included, is
/// skipped by the tokenizer -- and a caret indented by the offset into the
/// whole input would point past the end of the first line.
fn locate(input: &str, position: usize) -> (&str, usize) {
    let mut line_start = 0usize;
    let mut column = 0usize;
    for (index, (byte, ch)) in input.char_indices().enumerate() {
        if index == position {
            break;
        }
        if ch == '\n' {
            line_start = byte + ch.len_utf8();
            column = 0;
        } else {
            column += 1;
        }
    }
    let line_end = input[line_start..]
        .find('\n')
        .map_or(input.len(), |offset| line_start + offset);
    // A position at or past the end of the input lands one column past the
    // last character, which is what "expected ..., got end of input" wants.
    (input[line_start..line_end].trim_end_matches('\r'), column)
}

/// The 1-based line `position` falls on.
///
/// Separate from [`locate`], which returns the line's *text*: a caller reading
/// `details` gets the whole expression and needs a number to index it by, not
/// a copy of one line it would then have to find again.
fn line_number(input: &str, position: usize) -> usize {
    input
        .chars()
        .take(position)
        .filter(|ch| *ch == '\n')
        .count()
        + 1
}

/// Trim `line` to a readable width around `column`, returning the text to show
/// and the column the caret sits at within it.
fn window(line: &str, column: usize) -> (String, usize) {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= MAX_CONTEXT_WIDTH {
        return (line.to_string(), column);
    }

    // Centre the window on the caret, then pull it back inside the line so a
    // caret near either end still gets a full width of context.
    let start = column.saturating_sub(MAX_CONTEXT_WIDTH / 2);
    let end = (start + MAX_CONTEXT_WIDTH).min(chars.len());
    let start = end.saturating_sub(MAX_CONTEXT_WIDTH);

    let mut shown = String::new();
    let mut caret = column - start;
    if start > 0 {
        shown.push_str(ELLIPSIS);
        caret += ELLIPSIS.chars().count();
    }
    shown.extend(&chars[start..end]);
    if end < chars.len() {
        shown.push_str(ELLIPSIS);
    }
    (shown, caret)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pattern that is malformed as both a regex and a glob.
    const UNCLOSED_CLASS: &str = "[";

    fn parse_error(input: &str, position: usize) -> HunksetError {
        HunksetError::Parse {
            message: "boom".to_string(),
            input: input.to_string(),
            position,
        }
    }

    /// `(text above the caret, caret column, message)`.
    fn rendered(input: &str, position: usize) -> (String, usize, String) {
        let display = parse_error(input, position).display_with_context();
        let mut lines = display.lines();
        let text = lines.next().expect("input line").to_string();
        let caret = lines.next().expect("caret line");
        let message = lines.next().expect("message line").to_string();
        (text, caret.find('^').expect("caret"), message)
    }

    #[test]
    fn short_input_is_echoed_whole() {
        let (text, caret, message) = rendered("type(insert", 11);
        assert_eq!(text, "type(insert");
        assert_eq!(caret, 11);
        assert_eq!(message, "boom");
    }

    #[test]
    fn caret_lands_on_the_offending_character() {
        let input = "all() ~ none() ~ all()";
        let (_, caret, _) = rendered(input, 15);
        assert_eq!(input.chars().nth(caret), Some('~'));
    }

    #[test]
    fn long_input_is_windowed_around_the_caret() {
        // A wall of parentheses with a marker exactly where the error is.
        let mut input = "(".repeat(5_000);
        input.push('@');
        input.push_str(&")".repeat(5_000));

        let (text, caret, message) = rendered(&input, 5_000);
        assert!(
            text.chars().count() <= MAX_CONTEXT_WIDTH + 2 * ELLIPSIS.chars().count(),
            "context was not windowed: {} chars",
            text.chars().count()
        );
        assert_eq!(text.chars().nth(caret), Some('@'), "caret moved off target");
        assert!(text.starts_with(ELLIPSIS) && text.ends_with(ELLIPSIS));
        assert_eq!(message, "boom");
    }

    #[test]
    fn window_keeps_context_when_the_caret_sits_at_either_end() {
        let mut at_start = String::from("@");
        at_start.push_str(&"x".repeat(500));
        let (text, caret, _) = rendered(&at_start, 0);
        assert_eq!(text.chars().nth(caret), Some('@'));
        assert!(!text.starts_with(ELLIPSIS), "no elision needed at the start");

        let at_end = format!("{}@", "x".repeat(500));
        let (text, caret, _) = rendered(&at_end, 500);
        assert_eq!(text.chars().nth(caret), Some('@'));
        assert!(text.starts_with(ELLIPSIS));
    }

    #[test]
    fn caret_is_placed_within_the_offending_line_not_the_whole_input() {
        let input = "type(insert)\n  | file(\"a\"\n  | all()";
        // The `f` of `file` is the 4th character of the second line.
        let position = input.chars().take_while(|c| *c != 'f').count();
        let (text, caret, _) = rendered(input, position);
        assert_eq!(text, "  | file(\"a\"");
        assert_eq!(caret, 4);
    }

    #[test]
    fn non_ascii_input_does_not_push_the_caret_off_target() {
        let input = "file(\"héllo\") @";
        let position = input.chars().take_while(|c| *c != '@').count();
        let (text, caret, _) = rendered(input, position);
        assert_eq!(text.chars().nth(caret), Some('@'));
    }

    #[test]
    fn non_parse_errors_are_shown_plainly() {
        let err = HunksetError::UnknownFunction { name: "nope".into() };
        assert_eq!(err.display_with_context(), "unknown function 'nope'");
    }

    /// Every variant with the code it maps to, so a variant given the wrong
    /// code is caught here rather than by an agent branching on it in the
    /// field. The list is exhaustive on purpose: a new variant that reuses an
    /// existing code by accident would still compile.
    // The malformed pattern below is the point of the case; clippy reads the
    // argument to `Regex::new` and fails the build over exactly the
    // malformedness being tested.
    #[allow(clippy::invalid_regex)]
    #[test]
    fn every_variant_maps_to_its_own_code() {
        let cases: Vec<(HunksetError, &str)> = vec![
            (parse_error("type(insert", 11), "PARSE_ERROR"),
            (
                HunksetError::UnknownFunction { name: "nope".into() },
                "UNKNOWN_FUNCTION",
            ),
            (
                HunksetError::InvalidRegex {
                    pattern: UNCLOSED_CLASS.to_string(),
                    source: regex::Regex::new(UNCLOSED_CLASS).unwrap_err(),
                },
                "INVALID_REGEX",
            ),
            (
                HunksetError::InvalidGlob {
                    source: crate::glob::Glob::compile(UNCLOSED_CLASS).unwrap_err(),
                },
                "INVALID_GLOB",
            ),
            (HunksetError::UnknownId { id: "hunk-dead".into() }, "UNKNOWN_ID"),
            (
                HunksetError::AmbiguousId {
                    prefix: "hunk-a".into(),
                    candidates: vec![IdCandidate {
                        short_id: "hunk-abcd1234".into(),
                        path: "a.txt".into(),
                    }],
                },
                "AMBIGUOUS_ID",
            ),
            (
                HunksetError::InvalidArgument {
                    func: "type".into(),
                    value: "nope".into(),
                    valid: "insert".into(),
                },
                "INVALID_ARGUMENT",
            ),
            // Only ever constructed in a `--no-default-features` build, so no
            // integration test can reach it: `semantic` is on by default and
            // the suite runs against that binary. The mapping is still part of
            // the contract, so it is asserted where the variant exists.
            (
                HunksetError::SemanticFeatureRequired { name: "function".into() },
                "SEMANTIC_FEATURE_REQUIRED",
            ),
        ];

        let mut seen = std::collections::HashSet::new();
        for (error, expected) in &cases {
            assert_eq!(error.code().code, *expected, "{error}");
            assert!(seen.insert(*expected), "{expected} used twice");
        }
    }

    /// The facts a caller acts on, which used to be recoverable only by
    /// picking the rendered message apart. Each assertion is a piece of prose
    /// nobody has to parse any more.
    #[test]
    fn details_carry_the_facts_the_message_used_to_bury() {
        let ambiguous = HunksetError::AmbiguousId {
            prefix: "hunk-a".into(),
            candidates: vec![
                IdCandidate { short_id: "hunk-abcd1234".into(), path: "a.txt".into() },
                IdCandidate { short_id: "hunk-af005ba1".into(), path: "b.txt".into() },
            ],
        };
        let details = ambiguous.details();
        assert_eq!(details["count"], 2);
        assert_eq!(details["candidates"][1]["short_id"], "hunk-af005ba1");
        assert_eq!(details["candidates"][1]["path"], "b.txt");
        // The message still names them all, so human mode lost nothing.
        assert!(ambiguous.to_string().contains("hunk-af005ba1 (b.txt)"));

        let glob = HunksetError::InvalidGlob {
            source: crate::glob::Glob::compile("[").unwrap_err(),
        };
        assert_eq!(glob.details()["pattern"], "[");

        let semantic = HunksetError::SemanticFeatureRequired { name: "function".into() };
        assert_eq!(semantic.details()["predicate"], "function");
    }

    /// A caret on line three of an expression is at a small column and a large
    /// offset. Reporting one as the other would send a caller to the wrong
    /// character every time an expression wrapped.
    #[test]
    fn a_parse_error_reports_the_line_and_the_column_separately() {
        let input = "type(insert)\n  | file(\"a\"\n  | all()";
        let position = input.chars().take_while(|c| *c != 'f').count();
        let details = parse_error(input, position).details();

        assert_eq!(details["position"], position);
        assert_eq!(details["line"], 2);
        assert_eq!(details["column"], 4);
        assert_eq!(details["input"], input);
    }
}
