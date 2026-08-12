//! Stable, machine-readable failure codes.
//!
//! Every failure exits 1 with prose on stderr, and prose is the only thing a
//! caller has ever had to branch on. That is a poor contract: this project has
//! reworded the ambiguity error, the empty-selection error and the
//! unresolved-path error within the last few weeks, and a caller matching on
//! any of those texts would have broken three times. A code would have
//! survived all three.
//!
//! So each failure carries an [`ErrorCode`] and a bag of the facts that were
//! previously only recoverable by parsing the message -- the candidate ids
//! behind an ambiguous prefix, the offending path, the caret offset. The code
//! says what went wrong; `details` says what to do about it.
//!
//! # The default must not move
//!
//! `list --format json` writes its result to **stdout**, and callers read
//! non-empty stdout as "here are the hunks". Writing an error object there
//! would make a failed run look like a successful one carrying unexpected
//! fields. So the structured form is opt-in, on **stderr**, and the default
//! rendering is byte-for-byte what `fn main() -> Result<()>` printed before
//! this module existed: `Error: {err:?}` and nothing on stdout.

use clap::ValueEnum;
use serde::Serialize;
use serde_json::{Map, Value};
use std::fmt;
use std::io::Write;

/// How a failure is rendered on stderr.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum ErrorFormat {
    /// `Error: <prose>` -- what every release before this one printed.
    #[default]
    Human,
    /// One JSON object, one line: `{"error", "code", "message", "details"}`.
    Json,
}

/// A stable code and the coarse category it belongs to.
///
/// The two live in one constant so they cannot drift apart: a code's category
/// is a property of the code, not something each raising site restates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorCode {
    pub category: &'static str,
    pub code: &'static str,
}

impl ErrorCode {
    const fn new(category: &'static str, code: &'static str) -> Self {
        Self { category, code }
    }
}

// --- the contract -----------------------------------------------------------
//
// These strings are public API. A caller branches on them, so renaming one is
// a breaking change and adding a new one is not. Every variant of
// `HunksetError` has a code here, which is what keeps the mapping total: a new
// variant will not compile until it is given one.

/// The expression is not syntactically a hunkset.
pub const PARSE_ERROR: ErrorCode = ErrorCode::new("parse", "PARSE_ERROR");
/// A predicate name the language does not have.
pub const UNKNOWN_FUNCTION: ErrorCode = ErrorCode::new("selection", "UNKNOWN_FUNCTION");
/// A predicate was given a value or an argument shape it does not accept.
pub const INVALID_ARGUMENT: ErrorCode = ErrorCode::new("selection", "INVALID_ARGUMENT");
/// A glob pattern that does not compile.
pub const INVALID_GLOB: ErrorCode = ErrorCode::new("selection", "INVALID_GLOB");
/// A regex pattern that does not compile.
pub const INVALID_REGEX: ErrorCode = ErrorCode::new("selection", "INVALID_REGEX");
/// `id()` named a hunk this diff does not contain.
pub const UNKNOWN_ID: ErrorCode = ErrorCode::new("selection", "UNKNOWN_ID");
/// `id()` named an abbreviation that reaches more than one hunk.
pub const AMBIGUOUS_ID: ErrorCode = ErrorCode::new("selection", "AMBIGUOUS_ID");
/// A semantic predicate was used in a build without the `semantic` feature.
pub const SEMANTIC_FEATURE_REQUIRED: ErrorCode =
    ErrorCode::new("selection", "SEMANTIC_FEATURE_REQUIRED");
/// The selection resolved, and keeps nothing.
pub const EMPTY_SELECTION: ErrorCode = ErrorCode::new("selection", "EMPTY_SELECTION");
/// A JSON/YAML spec names a path, id or index the diff does not contain.
pub const PATH_NOT_IN_DIFF: ErrorCode = ErrorCode::new("selection", "PATH_NOT_IN_DIFF");
/// A revset jj could not resolve, or that resolved to nothing.
pub const REVSET_UNRESOLVED: ErrorCode = ErrorCode::new("revset", "REVSET_UNRESOLVED");
/// A revset that resolved to more than the one revision a diff needs.
pub const REVSET_AMBIGUOUS: ErrorCode = ErrorCode::new("revset", "REVSET_AMBIGUOUS");
/// `--spec-template` was asked for over files that `--max-bytes`/`--max-lines`
/// cut short, whose ids would name nothing in the real diff.
pub const TRUNCATED_SPEC_TEMPLATE: ErrorCode =
    ErrorCode::new("usage", "TRUNCATED_SPEC_TEMPLATE");
/// A spec key (or a rename `from`) that names a path this workspace cannot
/// contain: absolute, control-character-bearing, or climbing past the root.
pub const PATH_OUTSIDE_WORKSPACE: ErrorCode =
    ErrorCode::new("usage", "PATH_OUTSIDE_WORKSPACE");
/// Anything not yet given a code. Its `message` is still the full prose, so a
/// caller loses nothing it had before -- but it should not branch on this.
pub const UNKNOWN: ErrorCode = ErrorCode::new("internal", "UNKNOWN");

/// A failure that knows its own code.
///
/// The alternative was to recognise failures at the top level by their
/// message text, which is the very thing this module exists to stop callers
/// doing. Anywhere a code cannot be attached at the raising site is a signal
/// to push the type further down, not to start matching strings here.
#[derive(Debug)]
pub struct CodedError {
    code: ErrorCode,
    message: String,
    details: Map<String, Value>,
}

impl CodedError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: Map::new(),
        }
    }

    /// Attach one machine-actionable fact.
    #[must_use]
    pub fn with(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.details.insert(key.to_string(), value.into());
        self
    }

    /// Attach a whole prepared bag, for a typed error that builds its own.
    #[must_use]
    pub fn with_details(mut self, details: Map<String, Value>) -> Self {
        self.details = details;
        self
    }
}

impl fmt::Display for CodedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

// No `source()`: an anyhow error wrapping this must render exactly as
// `anyhow!("<the same message>")` did, and a source would add a "Caused by:"
// section that the human output never had.
impl std::error::Error for CodedError {}

/// Field order here is the documented shape, and serde preserves it.
#[derive(Serialize)]
struct Payload<'a> {
    error: &'a str,
    code: &'a str,
    message: String,
    details: Map<String, Value>,
}

/// Print `err` to stderr in the requested form. Never writes to stdout.
pub fn report(err: &anyhow::Error, format: ErrorFormat) {
    // `std::process::exit` is what runs next, and it is not obliged to flush
    // anything a command already wrote.
    let _ = std::io::stdout().flush();

    match format {
        // Byte-for-byte what `impl Termination for Result` printed when `main`
        // returned a `Result`. Changing this breaks every existing caller.
        ErrorFormat::Human => eprintln!("Error: {err:?}"),
        ErrorFormat::Json => eprintln!("{}", render_json(err)),
    }
}

fn render_json(err: &anyhow::Error) -> String {
    // Through the whole chain, so a `.context(..)` wrapper added above a coded
    // failure does not hide its code.
    let coded = err.chain().find_map(|link| link.downcast_ref::<CodedError>());

    let (code, details) = match coded {
        Some(coded) => (coded.code, coded.details.clone()),
        None => (UNKNOWN, Map::new()),
    };

    let payload = Payload {
        error: code.category,
        code: code.code,
        // `{:?}`, not `{}`: this is the text human mode would have printed,
        // "Caused by:" chain included, so opting in never loses information.
        message: format!("{err:?}"),
        details,
    };

    serde_json::to_string(&payload).unwrap_or_else(|_| {
        // Unreachable in practice -- every value here is a string, a number or
        // an array of those -- but a panic inside the error reporter would
        // replace a useful failure with a useless one.
        format!(
            r#"{{"error":"internal","code":"UNKNOWN","message":{},"details":{{}}}}"#,
            Value::String(format!("{err:?}"))
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_of(err: anyhow::Error) -> Value {
        serde_json::from_str(&render_json(&err)).expect("the reporter must emit valid JSON")
    }

    /// The documented shape, in the documented order. A caller reading
    /// `error`/`code`/`message`/`details` is reading the contract.
    #[test]
    fn a_coded_error_renders_the_documented_shape() {
        let err = anyhow::Error::from(
            CodedError::new(UNKNOWN_ID, "hunk id 'hunk-dead' matches no hunk")
                .with("id", "hunk-dead"),
        );
        let rendered = render_json(&err);
        assert!(
            rendered.starts_with(r#"{"error":"selection","code":"UNKNOWN_ID","message":"#),
            "field order is part of the documented shape: {rendered}"
        );

        let json = json_of(err);
        assert_eq!(json["error"], "selection");
        assert_eq!(json["code"], "UNKNOWN_ID");
        assert_eq!(json["details"]["id"], "hunk-dead");
    }

    /// Nothing is lost by opting in: the `message` field is the same text
    /// human mode prints, so a caller never has to run twice to read it.
    #[test]
    fn message_matches_what_human_mode_prints() {
        let err = anyhow::Error::from(CodedError::new(EMPTY_SELECTION, "line one\nline two"));
        assert_eq!(json_of(err)["message"], "line one\nline two");
    }

    /// An uncoded failure still has to produce parseable JSON, or a caller
    /// that opted in has to keep a prose fallback for the cases nobody has
    /// coded yet -- which is the fallback this feature exists to retire.
    #[test]
    fn an_uncoded_error_is_still_valid_json() {
        let json = json_of(anyhow::anyhow!("something nobody has coded"));
        assert_eq!(json["code"], "UNKNOWN");
        assert_eq!(json["error"], "internal");
        assert_eq!(json["message"], "something nobody has coded");
        assert_eq!(json["details"], serde_json::json!({}));
    }

    /// Commands wrap failures in `.context(..)` freely. If the code were read
    /// off the outermost link only, adding a context line anywhere above a
    /// raising site would silently downgrade it to UNKNOWN.
    #[test]
    fn a_code_survives_being_wrapped_in_context() {
        let err = anyhow::Error::from(CodedError::new(INVALID_GLOB, "invalid glob '['"))
            .context("while filtering the diff");

        let json = json_of(err);
        assert_eq!(json["code"], "INVALID_GLOB");
        // The chain is what human mode shows, so it is what `message` carries.
        assert!(
            json["message"].as_str().unwrap().contains("Caused by"),
            "{json}"
        );
    }

    /// Details are the point of the exercise: the code says what went wrong,
    /// these say what to do about it. Structured values must survive as
    /// structure rather than being flattened into more prose.
    #[test]
    fn details_keep_their_json_types() {
        let err = anyhow::Error::from(
            CodedError::new(REVSET_AMBIGUOUS, "too many")
                .with("revset", "all()")
                .with("resolved", 3)
                .with("revisions", vec!["aaa", "bbb"]),
        );
        let json = json_of(err);
        assert_eq!(json["details"]["resolved"], 3);
        assert_eq!(json["details"]["revisions"][1], "bbb");
    }

    /// Two codes that differ only by category would be indistinguishable to a
    /// caller keying on `code` alone, which is what callers are told to do.
    #[test]
    fn every_code_string_is_distinct() {
        let all = [
            PARSE_ERROR,
            UNKNOWN_FUNCTION,
            INVALID_ARGUMENT,
            INVALID_GLOB,
            INVALID_REGEX,
            UNKNOWN_ID,
            AMBIGUOUS_ID,
            SEMANTIC_FEATURE_REQUIRED,
            EMPTY_SELECTION,
            PATH_NOT_IN_DIFF,
            REVSET_UNRESOLVED,
            REVSET_AMBIGUOUS,
            TRUNCATED_SPEC_TEMPLATE,
            UNKNOWN,
        ];
        let unique: std::collections::HashSet<&str> = all.iter().map(|c| c.code).collect();
        assert_eq!(unique.len(), all.len());
    }
}
