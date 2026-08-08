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

/// Returns true if the input looks like a hunkset expression rather than
/// JSON or YAML. Uses a trial parse: if it parses as a hunkset, it is one.
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
    // Require at least one `(` to avoid false positives on bare YAML values
    // like `all`, `none`, `reset`, etc.
    if !trimmed.contains('(') {
        return false;
    }
    parse(trimmed).is_ok()
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
}
