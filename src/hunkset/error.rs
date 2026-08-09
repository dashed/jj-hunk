use thiserror::Error;

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
    #[error("hunk id '{prefix}' is ambiguous -- it matches {count} hunks: {candidates}. Use more characters, or exact:\"<full-id>\".")]
    AmbiguousId {
        prefix: String,
        count: usize,
        candidates: String,
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
}
