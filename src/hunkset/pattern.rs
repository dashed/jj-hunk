use crate::glob::glob_match;
use regex::Regex;
use super::ast::{PatternKind, StringPattern};
use super::error::HunksetError;

/// A pattern with a pre-compiled regex (if applicable).
pub(super) struct CompiledPattern {
    kind: PatternKind,
    value: String,
    compiled_regex: Option<Regex>,
}

impl CompiledPattern {
    pub(super) fn compile(pattern: &StringPattern) -> Result<Self, HunksetError> {
        let compiled_regex = if pattern.kind == PatternKind::Regex {
            Some(Regex::new(&pattern.value).map_err(|e| HunksetError::InvalidRegex {
                pattern: pattern.value.clone(),
                source: e,
            })?)
        } else {
            None
        };
        Ok(Self {
            kind: pattern.kind,
            value: pattern.value.clone(),
            compiled_regex,
        })
    }

    pub(super) fn matches(&self, haystack: &str) -> bool {
        match self.kind {
            PatternKind::Exact => haystack == self.value,
            PatternKind::Substring => haystack.contains(&self.value),
            PatternKind::Glob => glob_match(&self.value, haystack),
            PatternKind::Regex => {
                self.compiled_regex.as_ref().map_or(false, |re| re.is_match(haystack))
            }
        }
    }

    /// True when the user explicitly asked for exact matching.
    pub(super) fn kind_is_exact(&self) -> bool {
        self.kind == PatternKind::Exact
    }

    pub(super) fn value(&self) -> &str {
        &self.value
    }
}

pub(super) fn compile_patterns(patterns: Vec<StringPattern>) -> Result<Vec<CompiledPattern>, HunksetError> {
    patterns.iter().map(CompiledPattern::compile).collect()
}
