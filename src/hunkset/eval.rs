use crate::diff::Hunk;
use crate::spec::{DefaultAction, FileSpec, HunkSpec, Spec};
use std::collections::{HashMap, HashSet};

use super::ast::{Arg, Expr, PatternKind, StringPattern};
use super::error::HunksetError;
use super::pattern::{compile_patterns, CompiledPattern};

/// A hunk with its file-level context, used during evaluation.
#[derive(Debug)]
pub struct EnrichedHunk<'a> {
    pub file_path: &'a str,
    pub file_status: &'a str,
    pub hunk: &'a Hunk,
}

/// Evaluate a hunkset expression against a list of enriched hunks.
/// Returns a set of indices into the input slice that match.
pub fn evaluate(expr: &Expr, hunks: &[EnrichedHunk]) -> Result<HashSet<usize>, HunksetError> {
    match expr {
        Expr::All => Ok((0..hunks.len()).collect()),
        Expr::None => Ok(HashSet::new()),
        Expr::Negation(inner) => {
            let inner_set = evaluate(inner, hunks)?;
            Ok((0..hunks.len())
                .filter(|i| !inner_set.contains(i))
                .collect())
        }
        Expr::Union(..) | Expr::Intersection(..) | Expr::Difference(..) => {
            evaluate_chain(expr, hunks)
        }
        Expr::Function(name, args) => evaluate_function(name, args, hunks),
    }
}

/// Evaluate a run of binary operators without recursing down its left spine.
///
/// `a | b | c | ...` parses into a left-leaning tree, so its spine is as long
/// as the chain -- and the parser's nesting guard cannot see that, because it
/// builds the chain in a loop rather than by recursing. Walking that spine
/// recursively overflowed the stack and aborted the process (`fatal runtime
/// error: stack overflow`) at around 40_000 terms, reachable through
/// `--spec-file` or stdin where no argv limit applies.
///
/// Descending iteratively leaves only real nesting on the stack -- parentheses
/// and stacked `~`, both of which the parser *does* bound -- so evaluation
/// depth is capped by `MAX_DEPTH` no matter how long the chain is.
fn evaluate_chain(expr: &Expr, hunks: &[EnrichedHunk]) -> Result<HashSet<usize>, HunksetError> {
    enum Op {
        Union,
        Intersection,
        Difference,
    }

    // Unwind the spine to its leftmost leaf, remembering each operator and the
    // right operand it applies to.
    let mut spine: Vec<(Op, &Expr)> = Vec::new();
    let mut node = expr;
    loop {
        match node {
            Expr::Union(left, right) => {
                spine.push((Op::Union, right));
                node = left;
            }
            Expr::Intersection(left, right) => {
                spine.push((Op::Intersection, right));
                node = left;
            }
            Expr::Difference(left, right) => {
                spine.push((Op::Difference, right));
                node = left;
            }
            _ => break,
        }
    }

    // Then fold back up, innermost first, which is the order the recursive
    // version applied them in.
    let mut acc = evaluate(node, hunks)?;
    for (op, right) in spine.into_iter().rev() {
        let rhs = evaluate(right, hunks)?;
        acc = match op {
            Op::Union => {
                acc.extend(rhs);
                acc
            }
            Op::Intersection => acc.intersection(&rhs).copied().collect(),
            Op::Difference => acc.difference(&rhs).copied().collect(),
        };
    }
    Ok(acc)
}

/// Check that the `semantic` feature is enabled. Returns an error if not.
#[cfg(feature = "semantic")]
fn require_semantic(_name: &str) -> Result<(), HunksetError> {
    Ok(())
}

#[cfg(not(feature = "semantic"))]
fn require_semantic(name: &str) -> Result<(), HunksetError> {
    Err(HunksetError::SemanticFeatureRequired { name: name.to_string() })
}

/// The kind a predicate assumes for an argument whose kind the parser only
/// inferred.
///
/// The parser cannot know: it gives an unquoted word `Exact` and a quoted
/// string `Substring`, which is right for some predicates and wrong for others.
/// `extension(rs)` wants whole-value equality; `added(TODO)` wants a substring,
/// and comparing a whole hunk's added text to the word `TODO` for equality
/// matched nothing at exit 0 -- and, under `~`, everything.
///
/// An explicit `kind:"value"` is never overridden. Silently replacing a kind
/// the user asked for is how a misunderstood query becomes an empty result.
fn default_kind(name: &str) -> Option<PatternKind> {
    match name {
        "glob" => Some(PatternKind::Glob),
        // Paths and enum-like values name one thing exactly. Substring
        // matching here quietly pulled in `CacheManager` for `scope("Cache")`,
        // contradicting the README.
        "file" | "extension" | "status" | "type" | "function" | "scope" => Some(PatternKind::Exact),
        // Text predicates look *inside* a blob, so a bare word is a substring.
        "content" | "added" | "removed" | "annotation" | "decorator" => {
            Some(PatternKind::Substring)
        }
        // `id()` resolves its own argument; see `eval_id`.
        _ => None,
    }
}

/// Compile a predicate's arguments with its default kind already applied.
///
/// The defaulting happens *before* compilation, so every pattern -- whatever
/// kind it ends up with -- is checked once, up front, where `evaluate_function`
/// can return the error. Predicates used to re-derive their kind and re-compile
/// behind an `unwrap()`, which meant the up-front pass validated a `Substring`
/// that was never used while the `Glob` that was actually matched with had
/// never been checked at all.
fn compile_args(name: &str, args: &[Arg]) -> Result<Vec<CompiledPattern>, HunksetError> {
    let default = default_kind(name);
    let patterns: Vec<StringPattern> = extract_patterns(args)
        .into_iter()
        .map(|p| match default {
            Some(kind) if !p.explicit => StringPattern::inferred(kind, p.value),
            _ => p,
        })
        .collect();
    compile_patterns(patterns)
}

/// Values accepted by the enum-like predicates. A misspelling here would
/// otherwise select nothing and exit 0.
const VALID_TYPES: &[&str] = &["insert", "delete", "replace"];
const VALID_STATUSES: &[&str] = &["modified", "added", "removed", "renamed", "copied"];

fn validate_enum_args(func: &str, args: &[Arg], valid: &[&str]) -> Result<(), HunksetError> {
    for p in extract_patterns(args) {
        if !valid.contains(&p.value.as_str()) {
            return Err(HunksetError::InvalidArgument {
                func: func.to_string(),
                value: p.value,
                valid: valid.join(", "),
            });
        }
    }
    Ok(())
}

/// Warn when a semantic predicate came back empty only because nothing could
/// be parsed. Without this, "no language support for .txt" is indistinguishable
/// from "no hunk matched your query".
///
/// Deliberately silent when *some* file was analyzed: an empty result is then a
/// real answer about real metadata.
fn warn_if_nothing_analyzed(func: &str, result: &HashSet<usize>, hunks: &[EnrichedHunk]) {
    if !result.is_empty() || hunks.is_empty() {
        return;
    }
    if hunks.iter().any(|h| h.hunk.semantic.is_analyzed) {
        return;
    }
    let mut files: Vec<&str> = hunks.iter().map(|h| h.file_path).collect();
    files.sort_unstable();
    files.dedup();
    let shown = files.iter().take(3).copied().collect::<Vec<_>>().join(", ");
    let more = if files.len() > 3 {
        format!(" (+{} more)", files.len() - 3)
    } else {
        String::new()
    };
    eprintln!(
        "warning: {func}() found no semantic metadata -- no parser is available for: {shown}{more}. \
         The empty result reflects missing language support, not an absence of matches."
    );
}

/// What shape of arguments a predicate accepts.
#[derive(PartialEq)]
enum ArgShape {
    /// At least one string/pattern argument, no ranges.
    Patterns,
    /// At least one number or range. `lines(2)` is accepted as `2..2`.
    Numeric,
    /// No arguments at all.
    None,
    /// Zero or more patterns (zero has its own meaning).
    OptionalPatterns,
}

fn arg_shape(name: &str) -> ArgShape {
    match name {
        "file" | "glob" | "extension" | "status" | "type" | "content" | "added" | "removed"
        | "id" | "function" | "scope" => ArgShape::Patterns,
        "lines" | "before_line" | "after_line" | "depth" => ArgShape::Numeric,
        "doc" | "import" | "toplevel" => ArgShape::None,
        _ => ArgShape::OptionalPatterns, // annotation/decorator: zero args = "has any"
    }
}

/// Reject argument lists that would otherwise degenerate into a silent
/// `none()` -- and, under `~`, into a silent `all()`.
///
/// `patterns.iter().any(..)` over an empty vec is `false`, so a forgotten
/// argument used to mean "match nothing", which negation turned into "match
/// the entire diff". Wrong-typed arguments were dropped just as quietly by
/// extract_patterns/extract_ranges.
fn validate_args(name: &str, args: &[Arg]) -> Result<(), HunksetError> {
    let shape = arg_shape(name);
    let invalid = |value: String, valid: &str| HunksetError::InvalidArgument {
        func: name.to_string(),
        value,
        valid: valid.to_string(),
    };

    match shape {
        ArgShape::None => {
            if !args.is_empty() {
                return Err(invalid(
                    "an argument".to_string(),
                    "no arguments -- this predicate takes none",
                ));
            }
        }
        ArgShape::Patterns => {
            if args.is_empty() {
                return Err(invalid(
                    "no argument".to_string(),
                    "at least one string, e.g. file(\"src/a.rs\")",
                ));
            }
            if let Some((i, _)) = args.iter().enumerate().find(|(_, a)| matches!(a, Arg::Range(..))) {
                return Err(invalid(
                    format!("a range (argument {})", i + 1),
                    "strings, not ranges",
                ));
            }
        }
        ArgShape::Numeric => {
            if args.is_empty() {
                return Err(invalid(
                    "no argument".to_string(),
                    "a number or range, e.g. lines(10) or lines(10..20)",
                ));
            }
            for a in args {
                if let Arg::Pattern(p) = a {
                    if p.value.parse::<usize>().is_err() {
                        return Err(invalid(
                            p.value.clone(),
                            "a number or range, e.g. lines(10) or lines(10..20)",
                        ));
                    }
                }
                if let Arg::Range(lo, hi) = a {
                    if lo > hi {
                        return Err(invalid(
                            format!("{lo}..{hi}"),
                            "a range whose start is not greater than its end",
                        ));
                    }
                }
            }
        }
        ArgShape::OptionalPatterns => {}
    }
    Ok(())
}

fn evaluate_function(name: &str, args: &[Arg], hunks: &[EnrichedHunk]) -> Result<HashSet<usize>, HunksetError> {
    validate_args(name, args)?;
    // Compile once, up front, with this predicate's default kind applied, so
    // every malformed regex *and* every malformed glob is reported here rather
    // than turning into a silent non-match inside a predicate.
    let compiled = compile_args(name, args)?;

    match name.as_ref() {
        // `file()` and `glob()` filter the same field and differ only in the
        // kind they assume for an argument without a prefix.
        "file" | "glob" => Ok(eval_path(&compiled, hunks)),
        "extension" => Ok(eval_extension(&compiled, hunks)),
        "status" => {
            validate_enum_args("status", args, VALID_STATUSES)?;
            Ok(eval_status(&compiled, hunks))
        }
        "type" => {
            validate_enum_args("type", args, VALID_TYPES)?;
            Ok(eval_type(&compiled, hunks))
        }
        "lines" => Ok(eval_lines(args, hunks, LineRangeMode::Either)),
        "before_line" => Ok(eval_lines(args, hunks, LineRangeMode::Before)),
        "after_line" => Ok(eval_lines(args, hunks, LineRangeMode::After)),
        "content" => Ok(eval_content(&compiled, hunks, ContentMode::Either)),
        "added" => Ok(eval_content(&compiled, hunks, ContentMode::Added)),
        "removed" => Ok(eval_content(&compiled, hunks, ContentMode::Removed)),
        "id" => eval_id(&compiled, hunks),
        "function" => {
            require_semantic(name)?;
            let r = eval_semantic(&compiled, hunks, SemanticField::Function);
            warn_if_nothing_analyzed(name, &r, hunks);
            Ok(r)
        }
        "scope" => {
            require_semantic(name)?;
            let r = eval_semantic(&compiled, hunks, SemanticField::Scope);
            warn_if_nothing_analyzed(name, &r, hunks);
            Ok(r)
        }
        "annotation" | "decorator" => {
            require_semantic(name)?;
            let r = eval_annotation(&compiled, hunks);
            warn_if_nothing_analyzed(name, &r, hunks);
            Ok(r)
        }
        "doc" => {
            require_semantic(name)?;
            let r = eval_doc(hunks);
            warn_if_nothing_analyzed(name, &r, hunks);
            Ok(r)
        }
        "import" => {
            require_semantic(name)?;
            let r = eval_import(hunks);
            warn_if_nothing_analyzed(name, &r, hunks);
            Ok(r)
        }
        "toplevel" => {
            require_semantic(name)?;
            let r = eval_toplevel(hunks);
            warn_if_nothing_analyzed(name, &r, hunks);
            Ok(r)
        }
        "depth" => {
            // Argument shape is validated up front by validate_args.
            require_semantic(name)?;
            let r = eval_depth(args, hunks);
            warn_if_nothing_analyzed("depth", &r, hunks);
            Ok(r)
        }
        _ => Err(HunksetError::UnknownFunction { name: name.to_string() }),
    }
}

// --- helpers ---

fn filter_by_field<'a, F>(
    patterns: &[CompiledPattern],
    hunks: &[EnrichedHunk<'a>],
    field: F,
) -> HashSet<usize>
where
    F: Fn(&EnrichedHunk<'a>) -> &'a str,
{
    hunks
        .iter()
        .enumerate()
        .filter(|(_, h)| patterns.iter().any(|p| p.matches(field(h))))
        .map(|(i, _)| i)
        .collect()
}

fn extract_patterns(args: &[Arg]) -> Vec<StringPattern> {
    args.iter()
        .filter_map(|a| match a {
            Arg::Pattern(p) => Some(p.clone()),
            _ => None,
        })
        .collect()
}

fn extract_ranges(args: &[Arg]) -> Vec<(usize, usize)> {
    args.iter()
        .filter_map(|a| match a {
            Arg::Range(start, end) => Some((*start, *end)),
            // `lines(2)` reads naturally as "hunks touching line 2"; treat a
            // bare number as the single-line range 2..2 rather than dropping
            // it. validate_args has already rejected non-numeric patterns.
            Arg::Pattern(p) => p.value.parse::<usize>().ok().map(|n| (n, n)),
        })
        .collect()
}

// --- file predicates ---

/// Match a hunk's file path. `file()` and `glob()` are this same filter under
/// two different defaults, chosen by [`default_kind`].
fn eval_path(patterns: &[CompiledPattern], hunks: &[EnrichedHunk]) -> HashSet<usize> {
    filter_by_field(patterns, hunks, |h| h.file_path)
}

fn eval_extension(patterns: &[CompiledPattern], hunks: &[EnrichedHunk]) -> HashSet<usize> {
    hunks
        .iter()
        .enumerate()
        .filter(|(_, h)| {
            let ext = std::path::Path::new(h.file_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            patterns.iter().any(|p| p.matches(ext))
        })
        .map(|(i, _)| i)
        .collect()
}

fn eval_status(patterns: &[CompiledPattern], hunks: &[EnrichedHunk]) -> HashSet<usize> {
    filter_by_field(patterns, hunks, |h| h.file_status)
}

fn eval_type(patterns: &[CompiledPattern], hunks: &[EnrichedHunk]) -> HashSet<usize> {
    filter_by_field(patterns, hunks, |h| &h.hunk.hunk_type)
}

// --- line ranges ---

#[derive(Clone, Copy)]
enum LineRangeMode { Before, After, Either }

fn eval_lines(args: &[Arg], hunks: &[EnrichedHunk], mode: LineRangeMode) -> HashSet<usize> {
    let ranges = extract_ranges(args);
    if ranges.is_empty() {
        return HashSet::new();
    }
    hunks
        .iter()
        .enumerate()
        .filter(|(_, h)| {
            ranges.iter().any(|&(start, end)| {
                let before = hunk_touches_range(h.hunk.before_range.start, h.hunk.before_range.length, start, end);
                let after = hunk_touches_range(h.hunk.after_range.start, h.hunk.after_range.length, start, end);
                match mode {
                    LineRangeMode::Before => before,
                    LineRangeMode::After => after,
                    LineRangeMode::Either => before || after,
                }
            })
        })
        .map(|(i, _)| i)
        .collect()
}

fn hunk_touches_range(hunk_start: usize, hunk_len: usize, range_start: usize, range_end: usize) -> bool {
    if hunk_len == 0 {
        return hunk_start >= range_start && hunk_start <= range_end;
    }
    let hunk_end = hunk_start + hunk_len - 1;
    hunk_start <= range_end && hunk_end >= range_start
}

// --- content matching ---

#[derive(Clone, Copy)]
enum ContentMode { Added, Removed, Either }

fn eval_content(patterns: &[CompiledPattern], hunks: &[EnrichedHunk], mode: ContentMode) -> HashSet<usize> {
    hunks
        .iter()
        .enumerate()
        .filter(|(_, h)| {
            patterns.iter().any(|p| match mode {
                ContentMode::Added => p.matches(&h.hunk.added),
                ContentMode::Removed => p.matches(&h.hunk.removed),
                ContentMode::Either => p.matches(&h.hunk.added) || p.matches(&h.hunk.removed),
            })
        })
        .map(|(i, _)| i)
        .collect()
}

// --- stable ID ---

/// Select by stable hunk id, with jj-style abbreviation.
///
/// Takes either form: the full id, or any prefix of it -- including the
/// abbreviation `list` prints, which is chosen to be unambiguous over the
/// whole diff.
///
/// An abbreviated id that matches more than one hunk is an error, not a
/// multi-select: `id()` is the identity predicate, the one destructive commands
/// act on, and silently selecting two hunks because a 3-character prefix
/// collided is the worst possible reading of the user's intent. jj errors on
/// ambiguous change-id prefixes for the same reason.
///
/// `exact:"..."` disables prefix matching. Every full id is the same length,
/// so one can never prefix another and the escape hatch is not needed for
/// that; it is there for anyone who wants to rule abbreviation out.
///
/// An id naming *no* hunk is an error too, for the same reason ambiguity is:
/// `id()` names a specific hunk, so a name that fits none of them is a mistake
/// -- a stale id from an earlier listing, or a typo -- not a query that
/// legitimately came back empty the way `extension(rs)` can in a diff with no
/// Rust in it. Silently, `~id("hunk-typo")` was the whole diff.
fn eval_id(
    patterns: &[CompiledPattern],
    hunks: &[EnrichedHunk],
) -> Result<HashSet<usize>, HunksetError> {
    let mut selected = HashSet::new();
    for p in patterns {
        // `id()` reads its argument as an id, not as text to match with, so
        // any kind but `exact:` would be quietly ignored. `~id(substring:"e2d4")`
        // asked to drop every hunk whose id *contains* e2d4 and got prefix
        // matching instead -- keeping hunks it was told to drop.
        if let Some(kind) = p.explicit_kind() {
            if kind != "exact" {
                return Err(HunksetError::InvalidArgument {
                    func: "id".to_string(),
                    value: format!("{kind}:\"{}\"", p.value()),
                    valid: "a plain id, or exact:\"<id>\" to rule out abbreviation -- \
                            id() resolves ids, it does not pattern-match them"
                        .to_string(),
                });
            }
        }
        let Some(id) = crate::diff::normalize_hunk_id(p.value()) else {
            return Err(HunksetError::InvalidArgument {
                func: "id".to_string(),
                value: p.value().to_string(),
                valid: "a hunk id such as hunk-4c1b1b3... (or an unambiguous prefix)".to_string(),
            });
        };
        let exact_only = p.is_explicitly_exact();
        let matched: Vec<usize> = hunks
            .iter()
            .enumerate()
            .filter(|(_, h)| {
                if exact_only {
                    // `exact:` disables *prefix* matching -- it does not mean
                    // "the 64-hex form only". The short id is an equally
                    // canonical name for the same hunk, and the one `list`
                    // prints, so rejecting it made `id(exact:"hunk-3c6ce1bf")`
                    // silently select nothing at exit 0.
                    h.hunk.id == *id || h.hunk.short_id == *id
                } else {
                    crate::diff::id_matches(&id, &h.hunk.id)
                }
            })
            .map(|(i, _)| i)
            .collect();

        if matched.len() > 1 {
            // Named by their abbreviations: those are long enough to be
            // unambiguous over this diff, so the list is directly usable.
            let mut which: Vec<String> = matched
                .iter()
                .map(|&i| format!("{} ({})", hunks[i].hunk.short_id, hunks[i].file_path))
                .collect();
            which.sort();
            return Err(HunksetError::AmbiguousId {
                prefix: p.value().to_string(),
                count: matched.len(),
                candidates: which.join(", "),
            });
        }
        if matched.is_empty() {
            return Err(HunksetError::UnknownId {
                id: p.value().to_string(),
            });
        }
        selected.extend(matched);
    }
    Ok(selected)
}

// --- semantic ---

#[derive(Clone, Copy)]
enum SemanticField { Function, Scope }

fn eval_semantic(patterns: &[CompiledPattern], hunks: &[EnrichedHunk], field: SemanticField) -> HashSet<usize> {
    hunks
        .iter()
        .enumerate()
        .filter(|(_, h)| {
            let value = match field {
                SemanticField::Function => h.hunk.semantic.enclosing_function.as_deref(),
                SemanticField::Scope => h.hunk.semantic.enclosing_scope.as_deref(),
            };
            match value {
                Some(v) => patterns.iter().any(|p| p.matches(v)),
                None => false,
            }
        })
        .map(|(i, _)| i)
        .collect()
}

fn eval_annotation(patterns: &[CompiledPattern], hunks: &[EnrichedHunk]) -> HashSet<usize> {
    hunks
        .iter()
        .enumerate()
        .filter(|(_, h)| {
            if patterns.is_empty() {
                !h.hunk.semantic.annotations.is_empty()
            } else {
                h.hunk.semantic.annotations.iter().any(|ann| {
                    patterns.iter().any(|p| p.matches(ann))
                })
            }
        })
        .map(|(i, _)| i)
        .collect()
}

fn filter_by_bool<F>(hunks: &[EnrichedHunk], pred: F) -> HashSet<usize>
where
    F: Fn(&EnrichedHunk) -> bool,
{
    hunks.iter().enumerate()
        .filter(|(_, h)| pred(h))
        .map(|(i, _)| i)
        .collect()
}

fn eval_doc(hunks: &[EnrichedHunk]) -> HashSet<usize> {
    filter_by_bool(hunks, |h| h.hunk.semantic.is_doc_comment)
}

fn eval_import(hunks: &[EnrichedHunk]) -> HashSet<usize> {
    filter_by_bool(hunks, |h| h.hunk.semantic.is_import)
}

fn eval_toplevel(hunks: &[EnrichedHunk]) -> HashSet<usize> {
    // A hunk no parser looked at is not "top level" -- it is unknown.
    filter_by_bool(hunks, |h| h.hunk.semantic.is_analyzed && h.hunk.semantic.is_toplevel)
}

fn eval_depth(args: &[Arg], hunks: &[EnrichedHunk]) -> HashSet<usize> {
    let mut exact: Vec<usize> = Vec::new();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for arg in args {
        match arg {
            Arg::Pattern(p) => {
                if let Ok(n) = p.value.parse::<usize>() {
                    exact.push(n);
                }
            }
            Arg::Range(start, end) => {
                ranges.push((*start, *end));
            }
        }
    }
    hunks
        .iter()
        .enumerate()
        .filter(|(_, h)| {
            // Same reasoning as eval_toplevel: an unanalyzed hunk defaults to
            // depth 0, which would otherwise make depth(0) match every file
            // the parser could not read.
            if !h.hunk.semantic.is_analyzed {
                return false;
            }
            let d = h.hunk.semantic.nesting_depth;
            exact.contains(&d) || ranges.iter().any(|&(lo, hi)| d >= lo && d <= hi)
        })
        .map(|(i, _)| i)
        .collect()
}

// ---------------------------------------------------------------------------
// Hunkset → Spec conversion
// ---------------------------------------------------------------------------

/// Convert a set of matched enriched hunks into a Spec suitable for
/// split/commit/squash operations.
///
/// `rename_sources` maps a file's current path to the path it had on the left
/// side of the diff. Hunk ids for a renamed file are computed by diffing the
/// old path against the new one, so the spec has to carry the old path or
/// `select` cannot reproduce them.
pub fn to_spec(
    selected: &HashSet<usize>,
    hunks: &[EnrichedHunk],
    rename_sources: &HashMap<&str, &str>,
) -> Spec {
    let mut files: HashMap<String, Vec<String>> = HashMap::new();

    for &idx in selected {
        let Some(h) = hunks.get(idx) else { continue };
        files
            .entry(h.file_path.to_string())
            .or_default()
            .push(h.hunk.id.clone());
    }

    let spec_files: HashMap<String, FileSpec> = files
        .into_iter()
        .map(|(path, ids)| {
            let hunk_spec = HunkSpec {
                hunks: Vec::new(),
                ids,
                from: rename_sources
                    .get(path.as_str())
                    .map(|source| source.to_string()),
            };
            (path, FileSpec::Selection(hunk_spec))
        })
        .collect();

    Spec {
        files: spec_files,
        default: DefaultAction::Reset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::LineRange;
    use crate::hunkset::parse;

    fn make_hunk(index: usize, hunk_type: &str, removed: &str, added: &str) -> Hunk {
        Hunk {
            index,
            id: format!("hunk-{:064x}", index),
            short_id: format!("hunk-{:08x}", index),
            hunk_type: hunk_type.to_string(),
            removed: removed.to_string(),
            added: added.to_string(),
            before_range: LineRange {
                start: index * 10 + 1,
                length: if removed.is_empty() { 0 } else { removed.lines().count() },
            },
            after_range: LineRange {
                start: index * 10 + 1,
                length: if added.is_empty() { 0 } else { added.lines().count() },
            },
            context: None,
            // These fixtures model hunks in a .rs file, i.e. ones a parser did
            // run on. Tests for the unanalyzed case set this back to false.
            semantic: crate::diff::SemanticInfo { is_analyzed: true, ..Default::default() },
        }
    }

    #[test]
    fn eval_type_filter() {
        let hunks_data = vec![
            make_hunk(0, "insert", "", "new line\n"),
            make_hunk(1, "delete", "old line\n", ""),
            make_hunk(2, "replace", "before\n", "after\n"),
        ];
        let enriched: Vec<EnrichedHunk> = hunks_data
            .iter()
            .map(|h| EnrichedHunk { file_path: "src/lib.rs", file_status: "modified", hunk: h })
            .collect();

        assert_eq!(evaluate(&parse("type(insert)").unwrap(), &enriched).unwrap(), HashSet::from([0]));
        assert_eq!(evaluate(&parse("type(insert) | type(delete)").unwrap(), &enriched).unwrap(), HashSet::from([0, 1]));
    }

    #[test]
    fn eval_file_filter() {
        let h1 = make_hunk(0, "insert", "", "x\n");
        let h2 = make_hunk(1, "insert", "", "y\n");
        let enriched = vec![
            EnrichedHunk { file_path: "src/lib.rs", file_status: "modified", hunk: &h1 },
            EnrichedHunk { file_path: "tests/test.rs", file_status: "added", hunk: &h2 },
        ];
        assert_eq!(evaluate(&parse(r#"file("src/lib.rs")"#).unwrap(), &enriched).unwrap(), HashSet::from([0]));
    }

    #[test]
    fn eval_glob_filter() {
        let h1 = make_hunk(0, "insert", "", "x\n");
        let h2 = make_hunk(1, "insert", "", "y\n");
        let enriched = vec![
            EnrichedHunk { file_path: "src/lib.rs", file_status: "modified", hunk: &h1 },
            EnrichedHunk { file_path: "tests/test.rs", file_status: "added", hunk: &h2 },
        ];
        assert_eq!(evaluate(&parse(r#"glob("src/**/*.rs")"#).unwrap(), &enriched).unwrap(), HashSet::from([0]));
    }

    #[test]
    fn eval_content_filter() {
        let h1 = make_hunk(0, "insert", "", "TODO: fix this\n");
        let h2 = make_hunk(1, "replace", "old code\n", "new code\n");
        let enriched = vec![
            EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h1 },
            EnrichedHunk { file_path: "b.rs", file_status: "modified", hunk: &h2 },
        ];
        assert_eq!(evaluate(&parse(r#"added("TODO")"#).unwrap(), &enriched).unwrap(), HashSet::from([0]));
        assert_eq!(evaluate(&parse(r#"removed("old")"#).unwrap(), &enriched).unwrap(), HashSet::from([1]));
    }

    #[test]
    fn eval_intersection_and_difference() {
        let h1 = make_hunk(0, "insert", "", "new\n");
        let h2 = make_hunk(1, "insert", "", "also new\n");
        let h3 = make_hunk(2, "delete", "gone\n", "");
        let enriched = vec![
            EnrichedHunk { file_path: "src/a.rs", file_status: "modified", hunk: &h1 },
            EnrichedHunk { file_path: "src/b.rs", file_status: "modified", hunk: &h2 },
            EnrichedHunk { file_path: "tests/c.rs", file_status: "modified", hunk: &h3 },
        ];
        assert_eq!(evaluate(&parse(r#"type(insert) & glob("src/**")"#).unwrap(), &enriched).unwrap(), HashSet::from([0, 1]));
        assert_eq!(evaluate(&parse("all() ~ type(delete)").unwrap(), &enriched).unwrap(), HashSet::from([0, 1]));
    }

    #[test]
    fn eval_lines_filter() {
        let mut h1 = make_hunk(0, "replace", "old\n", "new\n");
        h1.before_range.start = 5;
        h1.before_range.length = 1;
        let mut h2 = make_hunk(1, "insert", "", "added\n");
        h2.before_range.start = 25;
        h2.before_range.length = 0;
        let enriched = vec![
            EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h1 },
            EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h2 },
        ];
        assert_eq!(evaluate(&parse("lines(1..10)").unwrap(), &enriched).unwrap(), HashSet::from([0]));
        assert_eq!(evaluate(&parse("lines(20..30)").unwrap(), &enriched).unwrap(), HashSet::from([1]));
    }

    #[test]
    fn eval_to_spec() {
        let h1 = make_hunk(0, "insert", "", "x\n");
        let h2 = make_hunk(1, "delete", "y\n", "");
        let enriched = vec![
            EnrichedHunk { file_path: "src/a.rs", file_status: "modified", hunk: &h1 },
            EnrichedHunk { file_path: "src/b.rs", file_status: "modified", hunk: &h2 },
        ];
        let selected = HashSet::from([0]);
        let spec = to_spec(&selected, &enriched, &HashMap::new());
        assert_eq!(spec.default, DefaultAction::Reset);
        assert!(spec.files.contains_key("src/a.rs"));
        assert!(!spec.files.contains_key("src/b.rs"));
        assert_eq!(spec.files["src/a.rs"].source_path(), None);
    }

    #[test]
    fn to_spec_carries_the_rename_source() {
        let h = make_hunk(0, "replace", "old\n", "new\n");
        let enriched = vec![EnrichedHunk {
            file_path: "dst.rs",
            file_status: "renamed",
            hunk: &h,
        }];
        let renames = HashMap::from([("dst.rs", "src.rs")]);
        let spec = to_spec(&HashSet::from([0]), &enriched, &renames);
        assert_eq!(spec.files["dst.rs"].source_path(), Some("src.rs"));
    }

    #[test]
    fn unknown_function_is_error() {
        let h = make_hunk(0, "insert", "", "x\n");
        let enriched = vec![EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h }];
        let expr = parse("functon(\"foo\")").unwrap();
        assert!(matches!(evaluate(&expr, &enriched).unwrap_err(), HunksetError::UnknownFunction { .. }));
    }

    #[test]
    fn invalid_regex_is_error() {
        let h = make_hunk(0, "insert", "", "x\n");
        let enriched = vec![EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h }];
        let expr = parse(r#"added(regex:"(unclosed")"#).unwrap();
        assert!(matches!(evaluate(&expr, &enriched).unwrap_err(), HunksetError::InvalidRegex { .. }));
    }

    #[test]
    #[cfg(not(feature = "semantic"))]
    fn semantic_functions_require_feature() {
        let h = make_hunk(0, "insert", "", "x\n");
        let enriched = vec![EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h }];
        for func in &["scope(\"Foo\")", "function(\"bar\")", "doc()", "import()", "toplevel()", "depth(0)", "annotation(\"test\")"] {
            let expr = parse(func).unwrap();
            let err = evaluate(&expr, &enriched).unwrap_err();
            assert!(matches!(err, HunksetError::SemanticFeatureRequired { .. }), "expected SemanticFeatureRequired for {}", func);
        }
    }

    #[test]
    fn eval_on_empty_hunks() {
        let empty: Vec<EnrichedHunk> = vec![];
        assert!(evaluate(&parse("all()").unwrap(), &empty).unwrap().is_empty());
        assert!(evaluate(&parse("none()").unwrap(), &empty).unwrap().is_empty());
        assert!(evaluate(&parse("type(insert)").unwrap(), &empty).unwrap().is_empty());
    }

    #[test]
    fn eval_extension_filter() {
        let h1 = make_hunk(0, "insert", "", "x\n");
        let h2 = make_hunk(1, "insert", "", "y\n");
        let enriched = vec![
            EnrichedHunk { file_path: "src/lib.rs", file_status: "modified", hunk: &h1 },
            EnrichedHunk { file_path: "src/lib.py", file_status: "modified", hunk: &h2 },
        ];
        assert_eq!(evaluate(&parse("extension(rs)").unwrap(), &enriched).unwrap(), HashSet::from([0]));
    }

    #[test]
    fn eval_status_filter() {
        let h1 = make_hunk(0, "insert", "", "x\n");
        let h2 = make_hunk(1, "insert", "", "y\n");
        let enriched = vec![
            EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h1 },
            EnrichedHunk { file_path: "b.rs", file_status: "added", hunk: &h2 },
        ];
        assert_eq!(evaluate(&parse("status(added)").unwrap(), &enriched).unwrap(), HashSet::from([1]));
    }

    #[test]
    #[cfg(feature = "semantic")]
    fn eval_doc_import_toplevel() {
        let mut h1 = make_hunk(0, "insert", "", "/// doc\n");
        h1.semantic.is_doc_comment = true;
        let mut h2 = make_hunk(1, "insert", "", "use foo;\n");
        h2.semantic.is_import = true;
        let mut h3 = make_hunk(2, "insert", "", "const X: i32 = 1;\n");
        h3.semantic.is_toplevel = true;
        let enriched = vec![
            EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h1 },
            EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h2 },
            EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h3 },
        ];
        assert_eq!(evaluate(&parse("doc()").unwrap(), &enriched).unwrap(), HashSet::from([0]));
        assert_eq!(evaluate(&parse("import()").unwrap(), &enriched).unwrap(), HashSet::from([1]));
        assert_eq!(evaluate(&parse("toplevel()").unwrap(), &enriched).unwrap(), HashSet::from([2]));
    }

    #[test]
    #[cfg(feature = "semantic")]
    fn unanalyzed_hunks_match_neither_toplevel_nor_depth_zero() {
        // A file no parser supports defaults to depth 0 / is_toplevel false.
        // Both predicates must exclude it, rather than depth(0) matching it
        // while toplevel() does not.
        let mut h = make_hunk(0, "insert", "", "hello\n");
        h.semantic.is_analyzed = false;
        let enriched = vec![EnrichedHunk { file_path: "notes.txt", file_status: "modified", hunk: &h }];

        assert!(
            evaluate(&parse("toplevel()").unwrap(), &enriched).unwrap().is_empty(),
            "unanalyzed hunk leaked into toplevel()"
        );
        assert!(
            evaluate(&parse("depth(0)").unwrap(), &enriched).unwrap().is_empty(),
            "unanalyzed hunk leaked into depth(0)"
        );
    }

    #[test]
    #[cfg(feature = "semantic")]
    fn eval_depth_filter() {
        let mut h1 = make_hunk(0, "insert", "", "x\n");
        h1.semantic.nesting_depth = 0;
        let mut h2 = make_hunk(1, "insert", "", "y\n");
        h2.semantic.nesting_depth = 1;
        let mut h3 = make_hunk(2, "insert", "", "z\n");
        h3.semantic.nesting_depth = 2;
        let enriched = vec![
            EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h1 },
            EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h2 },
            EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h3 },
        ];
        assert_eq!(evaluate(&parse("depth(0)").unwrap(), &enriched).unwrap(), HashSet::from([0]));
        assert_eq!(evaluate(&parse("depth(0..1)").unwrap(), &enriched).unwrap(), HashSet::from([0, 1]));
    }

    #[test]
    #[cfg(feature = "semantic")]
    fn eval_annotation_filter() {
        let mut h1 = make_hunk(0, "insert", "", "x\n");
        h1.semantic.annotations = vec!["#[test]".to_string()];
        let h2 = make_hunk(1, "insert", "", "y\n");
        let enriched = vec![
            EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h1 },
            EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h2 },
        ];
        assert_eq!(evaluate(&parse(r#"annotation("test")"#).unwrap(), &enriched).unwrap(), HashSet::from([0]));
        assert_eq!(evaluate(&parse("annotation()").unwrap(), &enriched).unwrap(), HashSet::from([0]));
        assert_eq!(evaluate(&parse(r#"decorator("test")"#).unwrap(), &enriched).unwrap(), HashSet::from([0]));
    }

    #[test]
    fn eval_regex_content() {
        let h1 = make_hunk(0, "insert", "", "fn hello_world() {\n");
        let h2 = make_hunk(1, "insert", "", "let x = 1;\n");
        let enriched = vec![
            EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h1 },
            EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h2 },
        ];
        assert_eq!(evaluate(&parse(r#"added(regex:"fn\s+\w+")"#).unwrap(), &enriched).unwrap(), HashSet::from([0]));
    }

    // -- bug: a malformed glob matched nothing, and `~` made that everything --

    /// Two hunks in two files, the second of which a selector would typically
    /// be written to exclude.
    fn two_files() -> (Hunk, Hunk) {
        (
            make_hunk(0, "insert", "", "src change\n"),
            make_hunk(1, "insert", "", "vendor change\n"),
        )
    }

    fn enrich<'a>(src: &'a Hunk, vendor: &'a Hunk) -> Vec<EnrichedHunk<'a>> {
        vec![
            EnrichedHunk { file_path: "src.txt", file_status: "modified", hunk: src },
            EnrichedHunk { file_path: "vendor/lib.txt", file_status: "modified", hunk: vendor },
        ]
    }

    fn eval_err(spec: &str, hunks: &[EnrichedHunk]) -> HunksetError {
        let expr = parse(spec).unwrap_or_else(|e| panic!("{spec} should parse: {e}"));
        evaluate(&expr, hunks)
            .err()
            .unwrap_or_else(|| panic!("{spec} should not have been accepted"))
    }

    /// The reported shape: an unterminated character class compiled to a
    /// pattern that matched nothing, which `~` inverted into the whole diff --
    /// so `split` committed the very hunk the selector existed to hold back.
    #[test]
    fn a_malformed_glob_is_an_error_not_a_selector_that_matches_nothing() {
        let (src, vendor) = two_files();
        let enriched = enrich(&src, &vendor);

        let err = eval_err(r#"~glob("vendor/[a-z*.txt")"#, &enriched);
        assert!(
            matches!(err, HunksetError::InvalidGlob { .. }),
            "expected a glob error, got: {err}"
        );
        assert!(
            err.to_string().contains("vendor/[a-z*.txt"),
            "the message should quote the pattern: {err}"
        );
    }

    /// Not only under `~`: a malformed glob anywhere is a typo, and silently
    /// missing is how a typo goes unnoticed.
    #[test]
    fn a_malformed_glob_is_an_error_without_a_negation_too() {
        let (src, vendor) = two_files();
        let enriched = enrich(&src, &vendor);
        for spec in [
            r#"glob("vendor/[a-z*.txt")"#,
            r#"glob("src/**/[abc")"#,
            r#"glob("a{b,c")"#,
            r#"glob("*.{c,h}}")"#,
            r#"glob("[z-a]")"#,
            r#"glob("x") | glob("{unclosed")"#,
            r#"all() ~ glob("vendor/[a-")"#,
        ] {
            assert!(
                matches!(eval_err(spec, &enriched), HunksetError::InvalidGlob { .. }),
                "{spec} should be a glob error"
            );
        }
    }

    /// An explicit `glob:` prefix reaches the same check, whichever predicate
    /// it is written on -- those used to compile behind an `unwrap()` on a
    /// pattern the up-front pass had never looked at.
    #[test]
    fn an_explicit_glob_prefix_is_validated_on_every_predicate() {
        let (src, vendor) = two_files();
        let enriched = enrich(&src, &vendor);
        for spec in [
            r#"file(glob:"vendor/[a-")"#,
            r#"~file(glob:"vendor/[a-")"#,
            r#"content(glob:"[a-")"#,
            r#"extension(glob:"[a-")"#,
        ] {
            assert!(
                matches!(eval_err(spec, &enriched), HunksetError::InvalidGlob { .. }),
                "{spec} should be a glob error"
            );
        }
    }

    /// The fix must not cost well-formed globs anything.
    #[test]
    fn well_formed_globs_still_select() {
        let (src, vendor) = two_files();
        let enriched = enrich(&src, &vendor);
        let selects = |spec: &str| evaluate(&parse(spec).unwrap(), &enriched).unwrap();
        assert_eq!(selects(r#"glob("vendor/**")"#), HashSet::from([1]));
        assert_eq!(selects(r#"~glob("vendor/**")"#), HashSet::from([0]));
        assert_eq!(selects(r#"glob("vendor/[a-z]*.txt")"#), HashSet::from([1]));
        assert_eq!(selects(r#"glob("*.{txt,md}")"#), HashSet::from([0]));
        assert_eq!(selects(r#"file(glob:"vendor/**")"#), HashSet::from([1]));
    }

    // -- bug: the `explicit` flag was dropped, so a bare id matched nothing --

    /// A hunk id is a name, and `list` prints an abbreviation of it. Whether
    /// the user quotes that abbreviation cannot change what it means.
    #[test]
    fn an_abbreviated_id_resolves_whether_or_not_it_is_quoted() {
        let h = make_hunk(0, "insert", "", "x\n");
        let enriched = vec![EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h }];
        // `hunk-00000000...`: any prefix of it names this hunk.
        let prefix = &h.id[..12];
        let quoted = format!(r#"id("{prefix}")"#);
        let bare = format!("id({prefix})");

        let expected = HashSet::from([0]);
        assert_eq!(evaluate(&parse(&quoted).unwrap(), &enriched).unwrap(), expected);
        assert_eq!(
            evaluate(&parse(&bare).unwrap(), &enriched).unwrap(),
            expected,
            "an unquoted abbreviation must resolve like a quoted one"
        );
        // The full id and the printed short id both work, quoted or not.
        for spec in [
            format!(r#"id("{}")"#, h.id),
            format!("id({})", h.id),
            format!(r#"id("{}")"#, h.short_id),
            format!("id({})", h.short_id),
        ] {
            assert_eq!(evaluate(&parse(&spec).unwrap(), &enriched).unwrap(), expected, "{spec}");
        }
    }

    /// `make_hunk` numbers its ids, so two of them differ only in the last
    /// digit and no abbreviation tells them apart. Give each one a distinct
    /// leading digit instead, the way real digests differ.
    fn make_hunk_with_distinct_id(index: usize, hex: char) -> Hunk {
        let mut hunk = make_hunk(index, "insert", "", "x\n");
        hunk.id = format!("hunk-{}", String::from(hex).repeat(64));
        hunk.short_id = format!("hunk-{}", String::from(hex).repeat(8));
        hunk
    }

    /// The negated form is where dropping the flag did damage: an id that
    /// resolved to nothing left `~` selecting the entire diff.
    #[test]
    fn negating_a_bare_abbreviated_id_excludes_exactly_that_hunk() {
        let h1 = make_hunk_with_distinct_id(0, 'a');
        let h2 = make_hunk_with_distinct_id(1, 'b');
        let enriched = vec![
            EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h1 },
            EnrichedHunk { file_path: "b.rs", file_status: "modified", hunk: &h2 },
        ];
        assert_eq!(
            evaluate(&parse("~id(hunk-aaaa)").unwrap(), &enriched).unwrap(),
            HashSet::from([1])
        );
        assert_eq!(
            evaluate(&parse(r#"~id("hunk-aaaa")"#).unwrap(), &enriched).unwrap(),
            HashSet::from([1]),
            "quoting must not change which hunks are left"
        );
    }

    /// An abbreviation naming two hunks is still an error, not a multi-select.
    #[test]
    fn an_ambiguous_bare_abbreviation_is_still_an_error() {
        let h1 = make_hunk_with_distinct_id(0, 'a');
        let mut h2 = make_hunk_with_distinct_id(1, 'a');
        h2.id.replace_range(60.., "bbbb");
        let enriched = vec![
            EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h1 },
            EnrichedHunk { file_path: "b.rs", file_status: "modified", hunk: &h2 },
        ];
        assert!(matches!(
            eval_err("id(hunk-aaaa)", &enriched),
            HunksetError::AmbiguousId { .. }
        ));
    }

    /// `exact:` still means what it says: the full id or the printed short id,
    /// and no other abbreviation.
    #[test]
    fn explicit_exact_still_rules_out_abbreviation() {
        let h = make_hunk(0, "insert", "", "x\n");
        let enriched = vec![EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h }];

        for spec in [
            format!(r#"id(exact:"{}")"#, h.id),
            format!(r#"id(exact:"{}")"#, h.short_id),
        ] {
            assert_eq!(
                evaluate(&parse(&spec).unwrap(), &enriched).unwrap(),
                HashSet::from([0]),
                "{spec}"
            );
        }

        // A shorter prefix is not a name `exact:` accepts.
        let spec = format!(r#"id(exact:"{}")"#, &h.id[..12]);
        assert!(matches!(
            eval_err(&spec, &enriched),
            HunksetError::UnknownId { .. }
        ));
    }

    /// An id that names nothing is a mistake -- a typo, or an id copied from a
    /// listing that has since moved on -- not an empty answer. Under `~` the
    /// empty answer was the whole diff.
    #[test]
    fn an_id_that_names_no_hunk_is_an_error() {
        let h = make_hunk(0, "insert", "", "x\n");
        let enriched = vec![EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h }];
        for spec in [
            r#"id("hunk-ffffffff")"#,
            r#"~id("hunk-ffffffff")"#,
            "id(hunk-ffffffff)",
        ] {
            let err = eval_err(spec, &enriched);
            assert!(
                matches!(err, HunksetError::UnknownId { .. }),
                "{spec} should be an unknown-id error, got: {err}"
            );
            assert!(err.to_string().contains("hunk-ffffffff"), "{err}");
        }
    }

    /// `id()` resolves ids; it does not match text. Any prefix but `exact:`
    /// would be quietly ignored, and `~id(substring:"...")` then kept hunks it
    /// had been told to drop.
    #[test]
    fn id_rejects_a_pattern_kind_it_cannot_honour() {
        let mut h = make_hunk(0, "insert", "", "x\n");
        h.id = format!("hunk-{}", "abcdef01".repeat(8));
        let enriched = vec![EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h }];
        // A real substring of the id, but not a prefix: substring matching
        // would have found it and prefix matching cannot, so honouring the
        // prefix the user wrote is the difference between a hit and a miss.
        let middle = "ef01abcd";
        for kind in ["substring", "glob", "regex"] {
            let spec = format!(r#"id({kind}:"{middle}")"#);
            let err = eval_err(&spec, &enriched);
            assert!(
                matches!(err, HunksetError::InvalidArgument { .. }),
                "{spec} should be rejected, got: {err}"
            );
            assert!(err.to_string().contains(kind), "{err}");
        }
    }

    // -- bug: an inferred kind made bare words unmatchable ------------------

    /// A bare word in a text predicate is a substring, not a demand that the
    /// hunk's whole added text equal it -- which nothing ever does.
    #[test]
    fn a_bare_word_in_a_text_predicate_is_a_substring() {
        let h1 = make_hunk(0, "insert", "", "// TODO: fix this\n");
        let h2 = make_hunk(1, "replace", "old code\n", "new code\n");
        let enriched = vec![
            EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h1 },
            EnrichedHunk { file_path: "b.rs", file_status: "modified", hunk: &h2 },
        ];
        let selects = |spec: &str| evaluate(&parse(spec).unwrap(), &enriched).unwrap();
        assert_eq!(selects("added(TODO)"), HashSet::from([0]));
        assert_eq!(selects("content(TODO)"), HashSet::from([0]));
        assert_eq!(selects("removed(old)"), HashSet::from([1]));
        // Quoting changes nothing, and `~` still means the complement of a
        // set that was actually found.
        assert_eq!(selects(r#"added("TODO")"#), HashSet::from([0]));
        assert_eq!(selects("~added(TODO)"), HashSet::from([1]));
    }

    /// Path and enum predicates keep matching whole values: `extension(rs)`
    /// must not start matching `.rss`.
    #[test]
    fn a_bare_word_in_a_path_or_enum_predicate_still_matches_the_whole_value() {
        let h1 = make_hunk(0, "insert", "", "x\n");
        let h2 = make_hunk(1, "insert", "", "y\n");
        let enriched = vec![
            EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h1 },
            EnrichedHunk { file_path: "b.rss", file_status: "modified", hunk: &h2 },
        ];
        let selects = |spec: &str| evaluate(&parse(spec).unwrap(), &enriched).unwrap();
        assert_eq!(selects("extension(rs)"), HashSet::from([0]));
        assert_eq!(selects(r#"file("a.rs")"#), HashSet::from([0]));
        // An explicit prefix is still honoured over the default.
        assert_eq!(selects(r#"extension(substring:"rs")"#), HashSet::from([0, 1]));
    }

    // -- bug: an operator chain overflowed the stack -------------------------

    /// How long a chain the guards must survive. The reported crash was at
    /// 100_000 terms; 40_000 already aborted a debug build.
    const LONG_CHAIN: usize = 100_000;

    /// Build, evaluate and free `spec` on a 2 MiB stack -- what a spawned
    /// thread gets by default, well under the main thread's 8 MiB.
    ///
    /// A stack overflow aborts the process rather than failing the test, which
    /// is the point: it is exactly the crash under test, and it cannot be
    /// mistaken for a passing run.
    fn evaluate_on_a_small_stack(spec: String) -> usize {
        std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(move || {
                let h = make_hunk(0, "insert", "", "x\n");
                let enriched =
                    vec![EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h }];
                let expr = parse(&spec).expect("chain should parse");
                let selected = evaluate(&expr, &enriched).expect("chain should evaluate");
                drop(expr);
                selected.len()
            })
            .expect("spawn")
            .join()
            .expect("a long chain overflowed a 2 MiB stack")
    }

    /// `a | a | a | ...` is a left-leaning tree as long as the chain. The
    /// parser builds it in a loop, so its nesting guard never fires, and both
    /// evaluating it and freeing it used to walk that spine recursively.
    #[test]
    fn a_long_union_chain_does_not_overflow_the_stack() {
        let chain = vec!["all()"; LONG_CHAIN].join(" | ");
        assert_eq!(evaluate_on_a_small_stack(chain), 1);
    }

    #[test]
    fn a_long_intersection_chain_does_not_overflow_the_stack() {
        let chain = vec!["all()"; LONG_CHAIN].join(" & ");
        assert_eq!(evaluate_on_a_small_stack(chain), 1);
    }

    /// Mixed operators build the same left spine through two different loops.
    #[test]
    fn a_long_mixed_chain_does_not_overflow_the_stack() {
        let mut chain = String::from("all()");
        for i in 0..LONG_CHAIN {
            chain.push_str(if i % 2 == 0 { " | all()" } else { " & all()" });
        }
        assert_eq!(evaluate_on_a_small_stack(chain), 1);
    }

    /// A difference chain needs its parentheses, so it is deep *and* long:
    /// the parentheses are capped by the parser, and what is left is spine.
    #[test]
    fn a_long_chain_under_the_nesting_limit_does_not_overflow_the_stack() {
        // 100 levels of parentheses -- inside the parser's limit -- wrapped
        // around a long chain, so both guards are exercised at once.
        let chain = vec!["all()"; LONG_CHAIN].join(" | ");
        let nested = format!("{}{chain}{}", "(".repeat(100), ")".repeat(100));
        assert_eq!(evaluate_on_a_small_stack(nested), 1);
    }

    /// Freeing the tree is its own recursion: a chain that evaluates fine
    /// still has to be dismantled, and the derived drop glue walked the spine.
    #[test]
    fn freeing_a_long_chain_does_not_overflow_the_stack() {
        let chain = vec!["none()"; LONG_CHAIN].join(" | ");
        std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(move || {
                let expr = parse(&chain).expect("chain should parse");
                drop(expr);
            })
            .expect("spawn")
            .join()
            .expect("freeing a long chain overflowed a 2 MiB stack");
    }

    /// The chain still means what it says once it no longer crashes.
    #[test]
    fn a_chain_evaluates_left_to_right() {
        let h1 = make_hunk(0, "insert", "", "x\n");
        let h2 = make_hunk(1, "delete", "y\n", "");
        let h3 = make_hunk(2, "replace", "a\n", "b\n");
        let enriched = vec![
            EnrichedHunk { file_path: "a.rs", file_status: "modified", hunk: &h1 },
            EnrichedHunk { file_path: "b.rs", file_status: "modified", hunk: &h2 },
            EnrichedHunk { file_path: "c.rs", file_status: "modified", hunk: &h3 },
        ];
        let selects = |spec: &str| evaluate(&parse(spec).unwrap(), &enriched).unwrap();
        assert_eq!(
            selects("type(insert) | type(delete) | type(replace)"),
            HashSet::from([0, 1, 2])
        );
        // `&` binds tighter than `|`, so this is a | (b & c) -- unchanged by
        // folding the spine iteratively.
        assert_eq!(
            selects("type(insert) | type(delete) & type(replace)"),
            HashSet::from([0])
        );
        assert_eq!(selects("all() & ~type(delete) & ~type(replace)"), HashSet::from([0]));
        assert_eq!(selects("(all() ~ type(delete)) ~ type(replace)"), HashSet::from([0]));
    }

    #[test]
    fn to_spec_multi_file() {
        let h1 = make_hunk(0, "insert", "", "x\n");
        let h2 = make_hunk(1, "insert", "", "y\n");
        let h3 = make_hunk(2, "delete", "z\n", "");
        let enriched = vec![
            EnrichedHunk { file_path: "src/a.rs", file_status: "modified", hunk: &h1 },
            EnrichedHunk { file_path: "src/a.rs", file_status: "modified", hunk: &h2 },
            EnrichedHunk { file_path: "src/b.rs", file_status: "modified", hunk: &h3 },
        ];
        let selected = HashSet::from([0, 2]);
        let spec = to_spec(&selected, &enriched, &HashMap::new());
        assert!(spec.files.contains_key("src/a.rs"));
        assert!(spec.files.contains_key("src/b.rs"));
        match spec.files.get("src/a.rs").unwrap() {
            FileSpec::Selection(hs) => assert_eq!(hs.ids.len(), 1),
            _ => panic!("expected Selection"),
        }
    }
}
