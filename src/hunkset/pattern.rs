use crate::glob::Glob;
use regex::Regex;
use super::ast::{PatternKind, StringPattern};
use super::error::HunksetError;

/// What a compiled pattern matches with.
///
/// The compiled form lives *inside* the variant that needs it, rather than
/// beside a `kind` tag in an `Option`. That makes "a glob pattern whose glob
/// failed to compile" unrepresentable: the only way to build `Glob`/`Regex` is
/// to have handled the compile error, so `matches` has no failure case left to
/// paper over with `false`. That paper-over is the bug this shape removes --
/// under `~`, a pattern that matches nothing selects the entire diff.
enum Matcher {
    Exact,
    Substring,
    Glob(Glob),
    Regex(Regex),
}

/// A pattern that has been checked and is ready to match.
pub(super) struct CompiledPattern {
    matcher: Matcher,
    value: String,
    /// Whether the user wrote `kind:"value"`, or the parser inferred the kind.
    ///
    /// Carried through from [`StringPattern`] rather than dropped here,
    /// because a predicate that resolves its argument specially -- `id()`,
    /// which prefix-matches -- has to tell `id(exact:"...")`, where the user
    /// asked for whole-id equality, from a bare `id(hunk-8a30)`, whose `Exact`
    /// kind is only the parser's default for an unquoted word. Losing the
    /// distinction made the abbreviation documented in `list` output match
    /// nothing at exit 0.
    explicit: bool,
}

impl CompiledPattern {
    pub(super) fn compile(pattern: &StringPattern) -> Result<Self, HunksetError> {
        let matcher = match pattern.kind {
            PatternKind::Exact => Matcher::Exact,
            PatternKind::Substring => Matcher::Substring,
            PatternKind::Glob => Matcher::Glob(
                Glob::compile(&pattern.value).map_err(|source| HunksetError::InvalidGlob { source })?,
            ),
            PatternKind::Regex => Matcher::Regex(Regex::new(&pattern.value).map_err(|e| {
                HunksetError::InvalidRegex {
                    pattern: pattern.value.clone(),
                    source: e,
                }
            })?),
        };
        Ok(Self {
            matcher,
            value: pattern.value.clone(),
            explicit: pattern.explicit,
        })
    }

    pub(super) fn matches(&self, haystack: &str) -> bool {
        match &self.matcher {
            Matcher::Exact => haystack == self.value,
            Matcher::Substring => haystack.contains(&self.value),
            Matcher::Glob(glob) => glob.is_match(haystack),
            Matcher::Regex(regex) => regex.is_match(haystack),
        }
    }

    /// True when the user asked for exact matching in so many words.
    ///
    /// Deliberately not "the kind happens to be `Exact`": that is also what an
    /// unquoted word gets by default, and treating the default as a request
    /// turns `id(hunk-8a30)` into a demand for an id that is literally eight
    /// characters long.
    pub(super) fn is_explicitly_exact(&self) -> bool {
        self.explicit && matches!(self.matcher, Matcher::Exact)
    }

    /// The kind the user wrote, or `None` when the parser inferred it.
    ///
    /// For predicates that interpret their argument themselves and so cannot
    /// honour an arbitrary kind -- they need to say which one they were given
    /// rather than quietly ignore it.
    pub(super) fn explicit_kind(&self) -> Option<&'static str> {
        if !self.explicit {
            return None;
        }
        Some(match self.matcher {
            Matcher::Exact => "exact",
            Matcher::Substring => "substring",
            Matcher::Glob(_) => "glob",
            Matcher::Regex(_) => "regex",
        })
    }

    pub(super) fn value(&self) -> &str {
        &self.value
    }
}

pub(super) fn compile_patterns(patterns: Vec<StringPattern>) -> Result<Vec<CompiledPattern>, HunksetError> {
    patterns.iter().map(CompiledPattern::compile).collect()
}
