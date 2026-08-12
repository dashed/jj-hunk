use crate::diff::Hunk;
use crate::spec::{DefaultAction, FileSpec, HunkSpec, Spec};
use std::collections::{BTreeMap, HashMap, HashSet};

use super::ast::{Arg, Expr, PatternKind, StringPattern};
use super::error::{HunksetError, IdCandidate};
use super::pattern::{compile_patterns, CompiledPattern};

/// A hunk with its file-level context, used during evaluation.
///
/// Two kinds of entry reach the predicates. Most are hunks the differ really
/// produced. The rest are *stand-ins*: one per change that produced no hunks at
/// all -- a binary, a retargeted symlink, a mode-only flip, a pure rename, an
/// empty add or remove -- minted by `whole_file_stand_in` so that a file-level
/// predicate has something to name the file by. [`EnrichedHunk::content`] is
/// what keeps the two apart.
#[derive(Debug)]
pub struct EnrichedHunk<'a> {
    /// Where the file is *now* -- the right-hand side of the diff.
    ///
    /// This is the file's identity: it keys the spec, names the file in an
    /// error message, and is the only path `select` can resolve. A path
    /// *predicate* matches more broadly than this (see [`Self::all_paths`]),
    /// but nothing that has to name the file may read anything else.
    pub file_path: &'a str,
    /// Where the file was on the left-hand side, when a rename or a copy moved
    /// it. `None` when the two sides agree, which is every other change.
    ///
    /// Required rather than defaulted, because the bug this closed was two
    /// code paths answering "which files does this pattern name?" differently.
    /// A builder that could be left off would rebuild that gap one new
    /// construction site later, and it would be invisible: the predicate would
    /// simply stop matching the old path, at exit 0, and the file would fall
    /// through to `default: reset`, which for a rename restores the old path
    /// and deletes the new one.
    rename_source: Option<&'a str>,
    pub file_status: &'a str,
    /// Private, and reachable only through the accessors below.
    ///
    /// A public field is one new predicate away from the leak this type exists
    /// to prevent: whoever writes the next content predicate would reach for
    /// `h.hunk.added` because it is there, and a stand-in would answer.
    hunk: &'a Hunk,
    /// Whether `hunk` stands in for a whole-file change instead of being one.
    is_stand_in: bool,
}

impl<'a> EnrichedHunk<'a> {
    /// An entry for a hunk the differ really produced.
    pub fn real(
        file_path: &'a str,
        rename_source: Option<&'a str>,
        file_status: &'a str,
        hunk: &'a Hunk,
    ) -> Self {
        Self {
            file_path,
            rename_source,
            file_status,
            hunk,
            is_stand_in: false,
        }
    }

    /// An entry for a change with no hunks, standing in for the whole file.
    pub fn stand_in(
        file_path: &'a str,
        rename_source: Option<&'a str>,
        file_status: &'a str,
        hunk: &'a Hunk,
    ) -> Self {
        Self {
            file_path,
            rename_source,
            file_status,
            hunk,
            is_stand_in: true,
        }
    }

    /// Every path this change may be *matched by*: where the file is now, plus
    /// where it came from if it was renamed or copied.
    ///
    /// The mirror of `FileHunks::all_paths`, which is what `--include` and
    /// `--exclude` have always filtered through. They and the hunkset path
    /// predicates are two ways of asking one question -- "which files does this
    /// pattern name?" -- and they used to answer it differently: the flags read
    /// both paths, `file()`/`glob()`/`extension()` read only `file_path`, so
    /// `--include 'secret/*'` found a file renamed out of `secret/` and
    /// `glob("secret/*")` did not.
    ///
    /// The old path is the whole point. It is the name you type when you are
    /// looking for what *used to be* somewhere, and a rename is exactly the
    /// case where you cannot know the new name to type instead.
    ///
    /// Answered for a stand-in too, and deliberately: a pure rename produces no
    /// hunks at all, so its stand-in is the only entry there is, and being
    /// reachable by `file("<old name>")` is precisely what it is for. Paths are
    /// file-level -- a stand-in carries them legitimately -- which is why this
    /// is not [`Self::content`], where the answer is withheld.
    pub fn all_paths(&self) -> impl Iterator<Item = &'a str> {
        std::iter::once(self.file_path).chain(self.rename_source)
    }

    /// The diffed text this entry may be matched *by* -- `None` for a stand-in.
    ///
    /// A stand-in exists so that a *file-level* predicate can name a change no
    /// hunk expresses. Nothing was ever diffed behind it, so a *content-level*
    /// predicate has no text it could have looked at and must never match it.
    /// A binary or a rename riding along with `content("...")` is a change the
    /// selector never asked for, and everything a hunkset does not pick up is
    /// handed back by `default: reset` -- so the mistake is destructive in both
    /// directions.
    ///
    /// That used to be enforced by the stand-in's *data*: empty `added` and
    /// `removed`, and both line ranges parked at `(start 0, length 0)`. Both
    /// halves of that argument are false. The empty substring is inside every
    /// string, so `content("")` matched every stand-in there was; and line 0 is
    /// inside `0..N`, so `lines(0..100000)` did too. `lines(1..100000)` did
    /// not, which is exactly why the first round of guards read as clean.
    /// Emptiness is not unmatchability -- a degenerate argument matches the
    /// empty value -- and the degenerate arguments cannot be enumerated ahead
    /// of the predicates nobody has written yet.
    ///
    /// So the answer is withheld rather than made empty. Rejecting `content("")`
    /// and `lines(0..N)` at argument validation was the other option and is
    /// worse: it fixes the two arguments that happen to have been found, and it
    /// changes what those arguments mean for ordinary hunks, where `content("")`
    /// selecting everything is a defensible reading of "content I did not
    /// constrain".
    pub fn content(&self) -> Option<&'a Hunk> {
        (!self.is_stand_in).then_some(self.hunk)
    }

    /// What happened to the file as a whole, which `type()` reads.
    ///
    /// File-level, not content-level, and so answered for a stand-in too: it
    /// carries the shape of its change (`insert`/`delete`/`replace`, derived
    /// from the file status and from no text at all). Withholding it here
    /// would stop `type(replace)` reaching a rename, which is the bug the
    /// stand-ins were introduced to fix.
    pub fn change_type(&self) -> &'a str {
        &self.hunk.hunk_type
    }

    /// The id this entry contributes to a spec once it has been selected.
    ///
    /// Answered for a stand-in on purpose. A selected one puts
    /// `whole-file:<path>` into the spec, which `evaluate_hunkset` then
    /// rewrites into a whole-file action -- reading an id out is not matching
    /// on one, and `id()` still cannot reach a stand-in.
    pub fn id(&self) -> &'a str {
        &self.hunk.id
    }

    /// The abbreviated id, for naming a hunk in an error message.
    pub fn short_id(&self) -> &'a str {
        &self.hunk.short_id
    }
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
pub(crate) fn default_kind(name: &str) -> Option<PatternKind> {
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
pub(crate) const VALID_TYPES: &[&str] = &["insert", "delete", "replace"];
pub(crate) const VALID_STATUSES: &[&str] =
    &["modified", "added", "removed", "renamed", "copied"];

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
    if hunks
        .iter()
        .any(|h| h.content().is_some_and(|hunk| hunk.semantic.is_analyzed))
    {
        return;
    }
    // `file_path`, not `all_paths()`: this names the files whose *content* no
    // parser could read, and only the right-hand side of a rename has content
    // to have been parsed. Listing the old path here would tell the user to go
    // look for a parser for a file that no longer exists.
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
pub(crate) enum ArgShape {
    /// At least one string/pattern argument, no ranges.
    Patterns,
    /// At least one number or range. `lines(2)` is accepted as `2..2`.
    Numeric,
    /// No arguments at all.
    None,
    /// Zero or more patterns (zero has its own meaning).
    OptionalPatterns,
}

pub(crate) fn arg_shape(name: &str) -> ArgShape {
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

    match name {
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
///
/// Either of the change's paths satisfies it -- see [`EnrichedHunk::all_paths`]
/// for why, and for the `--include`/`--exclude` behaviour it is matching.
fn eval_path(patterns: &[CompiledPattern], hunks: &[EnrichedHunk]) -> HashSet<usize> {
    filter_by_bool(hunks, |h| {
        h.all_paths()
            .any(|path| patterns.iter().any(|p| p.matches(path)))
    })
}

/// Match a change's file extension, on either side of a rename.
///
/// `a.txt` renamed to `b.rs` answers to `extension("rs")` *and* to
/// `extension("txt")`, which is worth stating because it reads oddly at first:
/// the file is not a .txt file any more. It is right anyway, for two reasons.
/// The change genuinely removed a .txt file and created a .rs one, and both
/// halves are in the diff this predicate is selecting from. And `extension(x)`
/// is `glob("*.x")` with the globbing spelled out for you -- if `glob` reads
/// both paths and `extension` reads one, the language starts contradicting
/// itself, which is a worse failure than the surprise. `--include '*.txt'`
/// already matched such a file, so this is where the flags were all along.
fn eval_extension(patterns: &[CompiledPattern], hunks: &[EnrichedHunk]) -> HashSet<usize> {
    filter_by_bool(hunks, |h| {
        h.all_paths().any(|path| {
            let ext = std::path::Path::new(path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            patterns.iter().any(|p| p.matches(ext))
        })
    })
}

fn eval_status(patterns: &[CompiledPattern], hunks: &[EnrichedHunk]) -> HashSet<usize> {
    filter_by_field(patterns, hunks, |h| h.file_status)
}

fn eval_type(patterns: &[CompiledPattern], hunks: &[EnrichedHunk]) -> HashSet<usize> {
    filter_by_field(patterns, hunks, |h| h.change_type())
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
            // A stand-in occupies no line, and saying so as `(0, 0)` was not
            // enough: line 0 is inside `lines(0..N)`.
            let Some(hunk) = h.content() else { return false };
            ranges.iter().any(|&(start, end)| {
                let before = hunk_touches_range(hunk.before_range.start, hunk.before_range.length, start, end);
                let after = hunk_touches_range(hunk.after_range.start, hunk.after_range.length, start, end);
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
            // A stand-in has no added or removed text, and saying so as `""`
            // was not enough: the empty substring is inside every string.
            let Some(hunk) = h.content() else { return false };
            patterns.iter().any(|p| match mode {
                ContentMode::Added => p.matches(&hunk.added),
                ContentMode::Removed => p.matches(&hunk.removed),
                ContentMode::Either => p.matches(&hunk.added) || p.matches(&hunk.removed),
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
                // A hunk id is a digest of the text, so identity is
                // content-level: a stand-in has no text and therefore no id to
                // be named by. Its `whole-file:<path>` name is already refused
                // above by `normalize_hunk_id`; this states the same rule where
                // the matching happens, so that a stand-in renamed into some
                // other shape later stays unnameable.
                let Some(hunk) = h.content() else { return false };
                if exact_only {
                    // `exact:` disables *prefix* matching -- it does not mean
                    // "the 64-hex form only". The short id is an equally
                    // canonical name for the same hunk, and the one `list`
                    // prints, so rejecting it made `id(exact:"hunk-3c6ce1bf")`
                    // silently select nothing at exit 0.
                    hunk.id == *id || hunk.short_id == *id
                } else {
                    crate::diff::id_matches(&id, &hunk.id)
                }
            })
            .map(|(i, _)| i)
            .collect();

        if matched.len() > 1 {
            // Named by their abbreviations: those are long enough to be
            // unambiguous over this diff, so the list is directly usable.
            //
            // `file_path`, not `all_paths()`: this tells the user where to look,
            // and the only path they can look at is the one the file has now.
            let mut which: Vec<IdCandidate> = matched
                .iter()
                .map(|&i| IdCandidate {
                    short_id: hunks[i].short_id().to_string(),
                    path: hunks[i].file_path.to_string(),
                })
                .collect();
            // By the rendered form, so the message reads in the same order it
            // always has now that the candidates are carried structured.
            which.sort_by_key(ToString::to_string);
            return Err(HunksetError::AmbiguousId {
                prefix: p.value().to_string(),
                candidates: which,
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
            let Some(hunk) = h.content() else { return false };
            let value = match field {
                SemanticField::Function => hunk.semantic.enclosing_function.as_deref(),
                SemanticField::Scope => hunk.semantic.enclosing_scope.as_deref(),
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
            let Some(hunk) = h.content() else { return false };
            if patterns.is_empty() {
                !hunk.semantic.annotations.is_empty()
            } else {
                hunk.semantic.annotations.iter().any(|ann| {
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
    filter_by_bool(hunks, |h| {
        h.content().is_some_and(|hunk| hunk.semantic.is_doc_comment)
    })
}

fn eval_import(hunks: &[EnrichedHunk]) -> HashSet<usize> {
    filter_by_bool(hunks, |h| {
        h.content().is_some_and(|hunk| hunk.semantic.is_import)
    })
}

fn eval_toplevel(hunks: &[EnrichedHunk]) -> HashSet<usize> {
    // A hunk no parser looked at is not "top level" -- it is unknown, and a
    // stand-in is the extreme of that: there was never any text to look at.
    filter_by_bool(hunks, |h| {
        h.content()
            .is_some_and(|hunk| hunk.semantic.is_analyzed && hunk.semantic.is_toplevel)
    })
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
            let Some(hunk) = h.content() else { return false };
            if !hunk.semantic.is_analyzed {
                return false;
            }
            let d = hunk.semantic.nesting_depth;
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
///
/// The old path goes in `from:` and nowhere else. A path predicate may *match*
/// a rename by either of its names, but the spec key stays the new path,
/// because that is the one `select` resolves a file by: keyed by the old path,
/// `select` would find no such file on the right, and the rename the user had
/// just selected would be undone by `default: reset` instead of committed.
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
            .push(h.id().to_string());
    }

    let spec_files: BTreeMap<String, FileSpec> = files
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
        let hunks_data = [
            make_hunk(0, "insert", "", "new line\n"),
            make_hunk(1, "delete", "old line\n", ""),
            make_hunk(2, "replace", "before\n", "after\n"),
        ];
        let enriched: Vec<EnrichedHunk> = hunks_data
            .iter()
            .map(|h| EnrichedHunk::real("src/lib.rs", None, "modified", h))
            .collect();

        assert_eq!(evaluate(&parse("type(insert)").unwrap(), &enriched).unwrap(), HashSet::from([0]));
        assert_eq!(evaluate(&parse("type(insert) | type(delete)").unwrap(), &enriched).unwrap(), HashSet::from([0, 1]));
    }

    #[test]
    fn eval_file_filter() {
        let h1 = make_hunk(0, "insert", "", "x\n");
        let h2 = make_hunk(1, "insert", "", "y\n");
        let enriched = vec![
            EnrichedHunk::real("src/lib.rs", None, "modified", &h1),
            EnrichedHunk::real("tests/test.rs", None, "added", &h2),
        ];
        assert_eq!(evaluate(&parse(r#"file("src/lib.rs")"#).unwrap(), &enriched).unwrap(), HashSet::from([0]));
    }

    #[test]
    fn eval_glob_filter() {
        let h1 = make_hunk(0, "insert", "", "x\n");
        let h2 = make_hunk(1, "insert", "", "y\n");
        let enriched = vec![
            EnrichedHunk::real("src/lib.rs", None, "modified", &h1),
            EnrichedHunk::real("tests/test.rs", None, "added", &h2),
        ];
        assert_eq!(evaluate(&parse(r#"glob("src/**/*.rs")"#).unwrap(), &enriched).unwrap(), HashSet::from([0]));
    }

    #[test]
    fn eval_content_filter() {
        let h1 = make_hunk(0, "insert", "", "TODO: fix this\n");
        let h2 = make_hunk(1, "replace", "old code\n", "new code\n");
        let enriched = vec![
            EnrichedHunk::real("a.rs", None, "modified", &h1),
            EnrichedHunk::real("b.rs", None, "modified", &h2),
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
            EnrichedHunk::real("src/a.rs", None, "modified", &h1),
            EnrichedHunk::real("src/b.rs", None, "modified", &h2),
            EnrichedHunk::real("tests/c.rs", None, "modified", &h3),
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
            EnrichedHunk::real("a.rs", None, "modified", &h1),
            EnrichedHunk::real("a.rs", None, "modified", &h2),
        ];
        assert_eq!(evaluate(&parse("lines(1..10)").unwrap(), &enriched).unwrap(), HashSet::from([0]));
        assert_eq!(evaluate(&parse("lines(20..30)").unwrap(), &enriched).unwrap(), HashSet::from([1]));
    }

    #[test]
    fn eval_to_spec() {
        let h1 = make_hunk(0, "insert", "", "x\n");
        let h2 = make_hunk(1, "delete", "y\n", "");
        let enriched = vec![
            EnrichedHunk::real("src/a.rs", None, "modified", &h1),
            EnrichedHunk::real("src/b.rs", None, "modified", &h2),
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
        let enriched = vec![EnrichedHunk::real("dst.rs", None, "renamed", &h)];
        let renames = HashMap::from([("dst.rs", "src.rs")]);
        let spec = to_spec(&HashSet::from([0]), &enriched, &renames);
        assert_eq!(spec.files["dst.rs"].source_path(), Some("src.rs"));
    }

    /// A renamed file answers to the path it came from, which is what
    /// `--include`/`--exclude` had always done through `FileHunks::all_paths`
    /// and the hunkset predicates did not. Without this, `glob("secret/*")`
    /// silently found nothing on a diff where `--include 'secret/*'` found the
    /// file -- and a hunk the expression cannot name takes `default: reset`.
    #[test]
    fn a_path_predicate_matches_a_rename_by_the_path_it_came_from() {
        let h1 = make_hunk(0, "replace", "old\n", "new\n");
        let h2 = make_hunk(1, "replace", "p\n", "q\n");
        let enriched = vec![
            EnrichedHunk::real("exposed.txt", Some("secret/keys.txt"), "renamed", &h1),
            EnrichedHunk::real("public.txt", None, "modified", &h2),
        ];

        for expr in [r#"glob("secret/*")"#, r#"file("secret/keys.txt")"#] {
            assert_eq!(
                evaluate(&parse(expr).unwrap(), &enriched).unwrap(),
                HashSet::from([0]),
                "{expr} did not reach the path the file was renamed from"
            );
        }
        // The new path keeps working: "either" must not have become "instead".
        assert_eq!(
            evaluate(&parse(r#"file("exposed.txt")"#).unwrap(), &enriched).unwrap(),
            HashSet::from([0]),
        );
    }

    /// The consequence of matching either path, stated where it can be seen:
    /// negation now *drops* a file renamed out of the excluded directory. That
    /// is the safe direction -- the diff still spells `secret/keys.txt` on its
    /// left side, so keeping it handed the user the thing they excluded -- and
    /// it is the reading `--exclude` has always had.
    #[test]
    fn negating_a_path_predicate_drops_a_rename_out_of_that_path() {
        let h1 = make_hunk(0, "replace", "old\n", "new\n");
        let h2 = make_hunk(1, "replace", "p\n", "q\n");
        let enriched = vec![
            EnrichedHunk::real("exposed.txt", Some("secret/keys.txt"), "renamed", &h1),
            EnrichedHunk::real("public.txt", None, "modified", &h2),
        ];
        assert_eq!(
            evaluate(&parse(r#"~glob("secret/*")"#).unwrap(), &enriched).unwrap(),
            HashSet::from([1]),
        );
    }

    /// A rename that changes the extension answers to both extensions. See
    /// `eval_extension` for why that is deliberate: `extension(x)` is
    /// `glob("*.x")` spelled out, so it has to read the paths `glob` reads or
    /// the two predicates start contradicting each other.
    #[test]
    fn extension_matches_either_side_of_a_rename_that_changed_it() {
        let h = make_hunk(0, "replace", "old\n", "new\n");
        let enriched = vec![EnrichedHunk::real("mod.rs", Some("mod.txt"), "renamed", &h)];
        assert_eq!(
            evaluate(&parse(r#"extension("rs")"#).unwrap(), &enriched).unwrap(),
            HashSet::from([0]),
        );
        assert_eq!(
            evaluate(&parse(r#"extension("txt")"#).unwrap(), &enriched).unwrap(),
            HashSet::from([0]),
        );
    }

    /// A pure rename produces no hunks, so its stand-in is the only entry there
    /// is -- and the old name is the only name a user could reasonably type for
    /// it. Paths are file-level, so a stand-in carries them; this is the case
    /// that would break if the "either path" lookup were ever put behind
    /// `content()`, which withholds a stand-in's answer on purpose.
    #[test]
    fn a_stand_in_is_reachable_by_the_path_its_rename_came_from() {
        let h = make_hunk(0, "replace", "", "");
        let enriched = vec![EnrichedHunk::stand_in(
            "dst.txt",
            Some("src.txt"),
            "renamed",
            &h,
        )];
        assert_eq!(
            evaluate(&parse(r#"file("src.txt")"#).unwrap(), &enriched).unwrap(),
            HashSet::from([0]),
        );
        // ...and still unmatchable by content, which this must not have loosened.
        assert!(evaluate(&parse(r#"content("")"#).unwrap(), &enriched)
            .unwrap()
            .is_empty());
    }

    /// Matched by the old path, keyed by the new one. The spec key is what
    /// `select` resolves a file by: keyed by `src.rs`, `select` finds no such
    /// file on the right-hand side and the rename the user just selected is
    /// undone by `default: reset` instead of committed. The old path belongs in
    /// `from:` and nowhere else.
    #[test]
    fn to_spec_keys_a_rename_by_its_new_path_when_matched_by_the_old_one() {
        let h = make_hunk(0, "replace", "old\n", "new\n");
        let enriched = vec![EnrichedHunk::real("dst.rs", Some("src.rs"), "renamed", &h)];
        let selected = evaluate(&parse(r#"file("src.rs")"#).unwrap(), &enriched).unwrap();
        let spec = to_spec(&selected, &enriched, &HashMap::from([("dst.rs", "src.rs")]));

        assert_eq!(
            spec.files.keys().collect::<Vec<_>>(),
            vec!["dst.rs"],
            "the old path leaked into the spec key"
        );
        assert_eq!(spec.files["dst.rs"].source_path(), Some("src.rs"));
    }

    #[test]
    fn unknown_function_is_error() {
        let h = make_hunk(0, "insert", "", "x\n");
        let enriched = vec![EnrichedHunk::real("a.rs", None, "modified", &h)];
        let expr = parse("functon(\"foo\")").unwrap();
        assert!(matches!(evaluate(&expr, &enriched).unwrap_err(), HunksetError::UnknownFunction { .. }));
    }

    #[test]
    fn invalid_regex_is_error() {
        let h = make_hunk(0, "insert", "", "x\n");
        let enriched = vec![EnrichedHunk::real("a.rs", None, "modified", &h)];
        let expr = parse(r#"added(regex:"(unclosed")"#).unwrap();
        assert!(matches!(evaluate(&expr, &enriched).unwrap_err(), HunksetError::InvalidRegex { .. }));
    }

    #[test]
    #[cfg(not(feature = "semantic"))]
    fn semantic_functions_require_feature() {
        let h = make_hunk(0, "insert", "", "x\n");
        let enriched = vec![EnrichedHunk::real("a.rs", None, "modified", &h)];
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
            EnrichedHunk::real("src/lib.rs", None, "modified", &h1),
            EnrichedHunk::real("src/lib.py", None, "modified", &h2),
        ];
        assert_eq!(evaluate(&parse("extension(rs)").unwrap(), &enriched).unwrap(), HashSet::from([0]));
    }

    #[test]
    fn eval_status_filter() {
        let h1 = make_hunk(0, "insert", "", "x\n");
        let h2 = make_hunk(1, "insert", "", "y\n");
        let enriched = vec![
            EnrichedHunk::real("a.rs", None, "modified", &h1),
            EnrichedHunk::real("b.rs", None, "added", &h2),
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
            EnrichedHunk::real("a.rs", None, "modified", &h1),
            EnrichedHunk::real("a.rs", None, "modified", &h2),
            EnrichedHunk::real("a.rs", None, "modified", &h3),
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
        let enriched = vec![EnrichedHunk::real("notes.txt", None, "modified", &h)];

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
            EnrichedHunk::real("a.rs", None, "modified", &h1),
            EnrichedHunk::real("a.rs", None, "modified", &h2),
            EnrichedHunk::real("a.rs", None, "modified", &h3),
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
            EnrichedHunk::real("a.rs", None, "modified", &h1),
            EnrichedHunk::real("a.rs", None, "modified", &h2),
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
            EnrichedHunk::real("a.rs", None, "modified", &h1),
            EnrichedHunk::real("a.rs", None, "modified", &h2),
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
            EnrichedHunk::real("src.txt", None, "modified", src),
            EnrichedHunk::real("vendor/lib.txt", None, "modified", vendor),
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
        let enriched = vec![EnrichedHunk::real("a.rs", None, "modified", &h)];
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

    /// A stand-in exactly as `whole_file_stand_in` mints one: no text, both
    /// ranges on line 0, and a name that is not in hunk-id form.
    fn make_stand_in(path: &str) -> Hunk {
        // No text, so `make_hunk` already gives both ranges length 0; only the
        // start moves to the line no file has.
        let mut hunk = make_hunk(0, "replace", "", "");
        hunk.id = format!("whole-file:{path}");
        hunk.short_id = hunk.id.clone();
        hunk.before_range.start = 0;
        hunk.after_range.start = 0;
        // Nothing was ever parsed, unlike the .rs fixtures `make_hunk` models.
        hunk.semantic = crate::diff::SemanticInfo::default();
        hunk
    }

    /// Every content-level predicate, asked with the argument that makes it
    /// degenerate. `EnrichedHunk::stand_in` withholds the text, so each one
    /// selects the real hunk and nothing else.
    ///
    /// Data alone did not do this. `content("")` finds the empty substring
    /// inside the stand-in's empty `added`, and `lines(0..9)` finds line 0
    /// inside the range that `(start 0, length 0)` reports -- both matched
    /// before the withholding, while `lines(1..9)` did not, which is how the
    /// leak stayed hidden.
    #[test]
    fn no_content_predicate_reaches_a_stand_in_however_degenerate_its_argument() {
        let real = make_hunk(0, "replace", "old\n", "new\n");
        let stand_in = make_stand_in("blob.bin");
        let enriched = vec![
            EnrichedHunk::real("a.rs", None, "modified", &real),
            EnrichedHunk::stand_in("blob.bin", None, "modified", &stand_in),
        ];

        for spec in [
            r#"content("")"#,
            r#"added("")"#,
            r#"removed("")"#,
            "lines(0..100000)",
            "before_line(0..100000)",
            "after_line(0..100000)",
        ] {
            assert_eq!(
                evaluate(&parse(spec).unwrap(), &enriched).unwrap(),
                HashSet::from([0]),
                "{spec} reached the stand-in"
            );
        }

        // `lines(0)` names the one line no real hunk can occupy, so it is the
        // sharpest form of the question: the answer is nothing at all, where
        // before the change it was the stand-in alone.
        assert!(
            evaluate(&parse("lines(0)").unwrap(), &enriched)
                .unwrap()
                .is_empty(),
            "line 0 belongs to no hunk, and a stand-in is not an exception"
        );
    }

    /// And the same predicates still answer for the real hunk they were asked
    /// about, so the rule above is "a stand-in has no content", not "content
    /// predicates match nothing".
    ///
    /// Preservation guard: with no stand-in in the list this passed before the
    /// change too, and cannot have failed beforehand.
    #[test]
    fn a_degenerate_content_argument_still_selects_an_ordinary_hunk() {
        let insert = make_hunk(0, "insert", "", "added only\n");
        let delete = make_hunk(1, "delete", "removed only\n", "");
        let enriched = vec![
            EnrichedHunk::real("a.rs", None, "modified", &insert),
            EnrichedHunk::real("b.rs", None, "modified", &delete),
        ];

        for spec in [
            r#"content("")"#,
            r#"added("")"#,
            r#"removed("")"#,
            "lines(0..100000)",
        ] {
            assert_eq!(
                evaluate(&parse(spec).unwrap(), &enriched).unwrap(),
                HashSet::from([0, 1]),
                "{spec} stopped selecting ordinary hunks"
            );
        }
    }

    /// File-level predicates must go on reaching the stand-in, which is the
    /// entire reason it is in the list. A fix that made `content()` miss it by
    /// dropping it from evaluation would pass the test above and reintroduce
    /// the bug the stand-ins were added for.
    #[test]
    fn file_level_predicates_still_reach_a_stand_in() {
        let real = make_hunk(0, "replace", "old\n", "new\n");
        let stand_in = make_stand_in("blob.bin");
        let enriched = vec![
            EnrichedHunk::real("a.rs", None, "modified", &real),
            EnrichedHunk::stand_in("blob.bin", None, "modified", &stand_in),
        ];

        for spec in [
            "all()",
            r#"file("blob.bin")"#,
            "type(replace)",
            "status(modified)",
        ] {
            assert!(
                evaluate(&parse(spec).unwrap(), &enriched)
                    .unwrap()
                    .contains(&1),
                "{spec} lost the stand-in"
            );
        }
        assert_eq!(
            evaluate(&parse(r#"~file("a.rs")"#).unwrap(), &enriched).unwrap(),
            HashSet::from([1]),
            "negation lost the stand-in"
        );
    }

    /// `id()` declines a stand-in on its own, not merely because
    /// `whole-file:<path>` is refused as an argument.
    ///
    /// This is the one case no end-to-end test can reach: from the CLI there is
    /// no way to hand `id()` a stand-in's name that survives
    /// `normalize_hunk_id`, so the argument check answers first and the match
    /// is never attempted. Handing the stand-in a well-formed `hunk-<hex>` id
    /// here gets past that check and asks the question directly. Before the
    /// change this selected it; now it is an unknown id, which is the same
    /// answer `id()` gives for any name that fits no hunk.
    #[test]
    fn id_cannot_match_a_stand_in_wearing_a_hunk_id() {
        let real = make_hunk_with_distinct_id(0, 'a');
        let mut stand_in = make_stand_in("blob.bin");
        stand_in.id = format!("hunk-{}", "b".repeat(64));
        stand_in.short_id = format!("hunk-{}", "b".repeat(8));
        let enriched = vec![
            EnrichedHunk::real("a.rs", None, "modified", &real),
            EnrichedHunk::stand_in("blob.bin", None, "modified", &stand_in),
        ];

        for spec in [
            "id(hunk-bbbb)".to_string(),
            format!(r#"id(exact:"{}")"#, stand_in.id),
            format!(r#"id(exact:"{}")"#, stand_in.short_id),
        ] {
            assert!(
                matches!(eval_err(&spec, &enriched), HunksetError::UnknownId { .. }),
                "{spec} named a stand-in"
            );
        }

        // The real hunk is still nameable, so this is not "id() matches
        // nothing".
        assert_eq!(
            evaluate(&parse("id(hunk-aaaa)").unwrap(), &enriched).unwrap(),
            HashSet::from([0])
        );
    }

    /// The negated form is where dropping the flag did damage: an id that
    /// resolved to nothing left `~` selecting the entire diff.
    #[test]
    fn negating_a_bare_abbreviated_id_excludes_exactly_that_hunk() {
        let h1 = make_hunk_with_distinct_id(0, 'a');
        let h2 = make_hunk_with_distinct_id(1, 'b');
        let enriched = vec![
            EnrichedHunk::real("a.rs", None, "modified", &h1),
            EnrichedHunk::real("b.rs", None, "modified", &h2),
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
            EnrichedHunk::real("a.rs", None, "modified", &h1),
            EnrichedHunk::real("b.rs", None, "modified", &h2),
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
        let enriched = vec![EnrichedHunk::real("a.rs", None, "modified", &h)];

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
        let enriched = vec![EnrichedHunk::real("a.rs", None, "modified", &h)];
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
        let enriched = vec![EnrichedHunk::real("a.rs", None, "modified", &h)];
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
            EnrichedHunk::real("a.rs", None, "modified", &h1),
            EnrichedHunk::real("b.rs", None, "modified", &h2),
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
            EnrichedHunk::real("a.rs", None, "modified", &h1),
            EnrichedHunk::real("b.rss", None, "modified", &h2),
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
                    vec![EnrichedHunk::real("a.rs", None, "modified", &h)];
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
            EnrichedHunk::real("a.rs", None, "modified", &h1),
            EnrichedHunk::real("b.rs", None, "modified", &h2),
            EnrichedHunk::real("c.rs", None, "modified", &h3),
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
            EnrichedHunk::real("src/a.rs", None, "modified", &h1),
            EnrichedHunk::real("src/a.rs", None, "modified", &h2),
            EnrichedHunk::real("src/b.rs", None, "modified", &h3),
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
