//! Hunkset: an algebraic query language for selecting diff hunks.
//!
//! Inspired by jj's fileset and revset languages, hunkset expressions compose
//! using set algebra (`|`, `&`, `~`) with predicate functions that filter hunks
//! by file path, hunk type, content, line ranges, and semantic context.
//!
//! # Example
//!
//! ```text
//! type(insert) & glob("src/**/*.rs") ~ annotation("test")
//! ```
//!
//! Selects all insertions in Rust source files that are not inside `#[test]`
//! functions.

mod ast;
mod error;
mod eval;
mod parser;
mod pattern;

#[allow(unused_imports)]
pub use error::HunksetError;
pub use eval::{evaluate, to_spec, EnrichedHunk};
pub use parser::parse;

/// Returns true if the input *looks like* a hunkset expression rather than
/// JSON or YAML.
///
/// This deliberately does NOT trial-parse. Deciding by `parse(..).is_ok()`
/// means a malformed hunkset is not recognised as one, so it falls through to
/// the JSON/YAML parser and the user is told their hunkset is bad JSON:
///
/// ```text
/// $ jj-hunk list --spec 'type(insert'
/// Error: Failed to parse spec as JSON (expected ident at line 1 column 2) ...
/// ```
///
/// Sniffing structurally instead lets a syntax error be reported as a syntax
/// error, with the caret that `HunksetError::display_with_context` already
/// builds.
pub fn is_hunkset(input: &str) -> bool {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Quick reject: JSON/YAML object/array literals
    let first = trimmed.as_bytes()[0];
    if first == b'{' || first == b'[' {
        return false;
    }
    // `all` and `none` are the only atoms the grammar accepts without
    // parentheses, and the parser already reaches that branch from nested
    // positions (`(all)`, `type(insert) | none`). Recognising them here is
    // what makes the paren-less form work on its own too, instead of being
    // reported as malformed JSON.
    //
    // The match is on the whole trimmed input, so it cannot widen into YAML:
    // `all: true` and `all-of-it` are still not hunksets, and neither bare
    // word deserialises into a `Spec` (which must be a mapping), so nothing
    // that used to parse changes meaning.
    if trimmed == "all" || trimmed == "none" {
        return true;
    }
    // Require at least one `(` to avoid false positives on bare YAML values
    // like `reset`, `keep`, etc.
    if !trimmed.contains('(') {
        return false;
    }
    // A hunkset starts with a predicate name, a negation, or a group.
    //
    // `!` is claimed too even though it is not an operator: someone who typed
    // it meant a hunkset, and claiming it lets the parser say so. Left to the
    // YAML fallback, `!import()` parsed as a tag, produced an empty spec, and
    // selected nothing while exiting 0.
    match first {
        b'~' | b'(' | b'!' => true,
        c if c.is_ascii_alphabetic() || c == b'_' => {
            // ...and the leading identifier must be followed by `(`, allowing
            // whitespace. This keeps YAML scalars like `keep: true` out.
            let ident_len = trimmed
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
                .count();
            trimmed[ident_len..].trim_start().starts_with('(')
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_hunkset_vs_json() {
        assert!(is_hunkset("type(insert)"));
        assert!(is_hunkset("all()"));
        assert!(is_hunkset("~type(delete)"));
        assert!(is_hunkset("(type(insert) | type(delete))"));
        assert!(!is_hunkset(r#"{"files": {}}"#));
        assert!(!is_hunkset("[1, 2, 3]"));
    }

    /// The parser accepts `all` and `none` without parentheses, and reaches
    /// that branch for `(all)` and `a | all`. The sniffer has to agree, or the
    /// paren-less form works everywhere except on its own.
    #[test]
    fn bare_all_and_none_are_hunksets() {
        assert!(is_hunkset("all"));
        assert!(is_hunkset("none"));
        assert!(is_hunkset("  all  "));
    }

    /// `!` is not an operator, but claiming it here is what lets the parser
    /// say so. Left to the YAML fallback, `!import()` parsed as a tag and
    /// selected nothing at all.
    #[test]
    fn bang_prefixed_input_is_claimed_as_a_hunkset() {
        assert!(is_hunkset("!import()"));
        assert!(is_hunkset("!type(insert)"));
    }

    /// The two keywords above are the only bare words allowed through. Any
    /// other scalar, and anything that merely starts with them, still belongs
    /// to the JSON/YAML parser.
    #[test]
    fn bare_yaml_scalars_are_not_hunksets() {
        for input in [
            "keep",
            "reset",
            "all: true",
            "none: false",
            "allow",
            "all-of-it",
            "none_at_all",
            "files:\n  a.txt:\n    action: keep",
            "",
            "   ",
        ] {
            assert!(!is_hunkset(input), "{input:?} should not be a hunkset");
        }
    }
}
