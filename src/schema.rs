//! `jj-hunk schema` -- the machine-readable description of the hunkset
//! language, the error codes, and the verbs.
//!
//! # Why this exists when `--help` already does
//!
//! `--help` describes the *command line*. It cannot describe the hunkset
//! language, which is the part of this tool a caller actually has to get
//! right, and where getting it wrong is silent.
//!
//! The load-bearing fact is the class split. File-level predicates (`file`,
//! `glob`, `extension`, `status`, `type`) reach every change in the diff,
//! including the ones that produced no hunks at all -- a binary, a retargeted
//! symlink, a mode-only flip, a pure rename, an empty add. Content-level
//! predicates (`content`, `added`, `removed`, `lines`, `id`) can never reach
//! those, because there is no diffed text behind them to match; see
//! [`crate::hunkset::EnrichedHunk::content`] for the mechanism. So
//! `split 'content("x")'` in a diff containing a rename leaves the rename
//! behind, exits 0, and says nothing.
//!
//! An agent that reads `"class": "content"` knows that structurally, up front,
//! instead of discovering it as a wrong result. That is the field this feature
//! exists for; `reaches_hunkless_changes` states the same thing as a boolean so
//! nothing has to be inferred from a label.
//!
//! # Derived, not restated
//!
//! Documentation in this repo has drifted from the code repeatedly, and each
//! time it was caught by running the binary rather than by a test. So this
//! module reads the real definitions wherever it can:
//!
//! | field | source of truth |
//! |---|---|
//! | `arity`, `argument` | [`arg_shape`], the pass that validates arguments |
//! | `default_pattern_kind` | [`default_kind`], applied before compilation |
//! | `values` | [`VALID_TYPES`] / [`VALID_STATUSES`] |
//! | `available` | `cfg!(feature = "semantic")` |
//! | `errors` | [`crate::errors::ALL`] |
//! | `commands` | clap's own parsed `Command` |
//!
//! What is left is one table of facts no type carries: which predicates exist,
//! whether each reaches a hunkless change, whether each needs the `semantic`
//! feature, and a line of prose. Every one of those except the prose is pinned
//! by a test below that fails if the table and the evaluator disagree -- the
//! class by evaluating each predicate against a real hunk and a stand-in and
//! watching which come back, the name list by reading the dispatch table out of
//! `eval.rs`, the feature set by the error a `--no-default-features` build
//! raises.

use crate::errors;
use crate::hunkset::{arg_shape, default_kind, ArgShape, PatternKind, VALID_STATUSES, VALID_TYPES};
use anyhow::Result;
use serde::Serialize;

/// The version of the *shape* below, not of the tool.
///
/// Bumped when a field is removed, renamed, or changes meaning. Adding a field
/// or a new predicate/error/command entry does **not** bump it: a caller that
/// ignores unknown fields keeps working, and one that pinned a version would
/// otherwise have to be updated to learn about a predicate it does not use.
const SCHEMA_VERSION: u32 = 1;

/// Every pattern-kind prefix the tokenizer accepts, in the order the parser
/// matches them.
const PATTERN_PREFIXES: &[&str] = &["exact", "substring", "glob", "regex"];

// ---------------------------------------------------------------------------
// The table of facts no type carries
// ---------------------------------------------------------------------------

struct PredicateFacts {
    name: &'static str,
    /// Whether this predicate can select a change that produced no hunks.
    ///
    /// Pinned by `class_matches_what_each_predicate_does_to_a_hunkless_change`.
    reaches_hunkless: bool,
    /// Whether it is one of the tree-sitter predicates, present only when the
    /// `semantic` feature is on.
    ///
    /// Pinned in both directions by the feature-gated tests below.
    semantic: bool,
    summary: &'static str,
}

const fn p(
    name: &'static str,
    reaches_hunkless: bool,
    semantic: bool,
    summary: &'static str,
) -> PredicateFacts {
    PredicateFacts {
        name,
        reaches_hunkless,
        semantic,
        summary,
    }
}

/// The predicates, grouped by class so the split is visible in the source too.
const PREDICATES: &[PredicateFacts] = &[
    // --- file-level: reach every change, hunks or not ---
    p(
        "file",
        true,
        false,
        "Match a change by path, on either side of a rename. Defaults to exact matching.",
    ),
    p(
        "glob",
        true,
        false,
        "Match a change by path, on either side of a rename. Defaults to glob matching.",
    ),
    p(
        "extension",
        true,
        false,
        "Match a change by file extension, on either side of a rename. Write it without the dot.",
    ),
    p(
        "status",
        true,
        false,
        "Match a change by what happened to the file as a whole.",
    ),
    p(
        "type",
        true,
        false,
        "Match a change by the shape of its edit.",
    ),
    // --- content-level: never reach a change with no hunks ---
    p(
        "content",
        false,
        false,
        "Match a hunk whose added or removed text matches. Defaults to substring matching.",
    ),
    p(
        "added",
        false,
        false,
        "Match a hunk whose added text matches. Defaults to substring matching.",
    ),
    p(
        "removed",
        false,
        false,
        "Match a hunk whose removed text matches. Defaults to substring matching.",
    ),
    p(
        "lines",
        false,
        false,
        "Match a hunk touching the given line numbers on either side of the diff.",
    ),
    p(
        "before_line",
        false,
        false,
        "Match a hunk touching the given line numbers on the before side.",
    ),
    p(
        "after_line",
        false,
        false,
        "Match a hunk touching the given line numbers on the after side.",
    ),
    p(
        "id",
        false,
        false,
        "Match one hunk by its stable id, or by an unambiguous prefix of it. An id naming no hunk, \
         or more than one, is an error rather than an empty result.",
    ),
    // --- semantic: content-level, and only in a build with the feature ---
    p(
        "function",
        false,
        true,
        "Match a hunk inside a named function. Defaults to exact matching.",
    ),
    p(
        "scope",
        false,
        true,
        "Match a hunk inside a named class, module or other enclosing scope. Defaults to exact \
         matching.",
    ),
    p(
        "annotation",
        false,
        true,
        "Match a hunk carrying an attribute or decorator; with no argument, any at all.",
    ),
    p("decorator", false, true, "An alias for annotation()."),
    p(
        "doc",
        false,
        true,
        "Match a hunk inside a documentation comment.",
    ),
    p(
        "import",
        false,
        true,
        "Match a hunk inside an import or use declaration.",
    ),
    p(
        "toplevel",
        false,
        true,
        "Match a hunk at the top level of a file some bundled parser could read.",
    ),
    p(
        "depth",
        false,
        true,
        "Match a hunk by its nesting depth in a file some bundled parser could read.",
    ),
];

/// `all` and `none` are not predicates: the grammar accepts them without
/// parentheses, refuses them any argument, and never routes them through the
/// predicate dispatch. Reported separately so their arity is not misread as
/// "takes anything".
const CONSTANTS: &[(&str, bool, &str)] = &[
    (
        "all",
        true,
        "Every change in the diff, including changes that produced no hunks.",
    ),
    (
        "none",
        false,
        "Nothing at all. Useful as an explicit empty selection.",
    ),
];

/// `symbol` alone does not identify an operator -- `~` is both the infix
/// difference and the prefix negation -- so `name` and `position` are what a
/// caller keys on.
const OPERATORS: &[(&str, &str, &str, &str)] = &[
    ("|", "infix", "union", "Changes matched by either side."),
    ("&", "infix", "intersection", "Changes matched by both sides."),
    (
        "~",
        "infix",
        "difference",
        "Changes matched by the left side and not the right. Chaining needs parentheses.",
    ),
    (
        "~",
        "prefix",
        "negation",
        "Every change the operand does not match. Note that this reaches hunkless changes even \
         when the operand cannot: ~content(\"x\") selects every binary and every pure rename in \
         the diff.",
    ),
];

// ---------------------------------------------------------------------------
// Derivations
// ---------------------------------------------------------------------------

/// How many arguments a predicate takes, and of what kind -- read off the
/// validation pass rather than restated.
fn shape_of(name: &str) -> (&'static str, &'static str) {
    match arg_shape(name) {
        ArgShape::None => ("none", "none"),
        ArgShape::Patterns => ("one_or_more", "pattern"),
        ArgShape::Numeric => ("one_or_more", "number_or_range"),
        ArgShape::OptionalPatterns => ("zero_or_more", "pattern"),
    }
}

fn kind_name(kind: PatternKind) -> &'static str {
    match kind {
        PatternKind::Exact => "exact",
        PatternKind::Substring => "substring",
        PatternKind::Glob => "glob",
        PatternKind::Regex => "regex",
    }
}

/// Which `kind:"value"` prefixes are meaningful on this predicate's argument.
///
/// Empty for the numeric predicates, whose argument is a number rather than a
/// pattern. `id()` resolves its argument as an id instead of matching with it,
/// so it takes `exact:` -- which only rules out prefix matching -- and refuses
/// the other three outright rather than ignoring them.
fn prefixes_for(name: &str, argument: &str) -> &'static [&'static str] {
    if argument != "pattern" {
        &[]
    } else if name == "id" {
        &["exact"]
    } else {
        PATTERN_PREFIXES
    }
}

/// The closed value sets, straight from the constants the evaluator checks
/// against. A misspelling here is the classic silent empty selection.
fn values_for(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "status" => Some(VALID_STATUSES),
        "type" => Some(VALID_TYPES),
        _ => None,
    }
}

const SEMANTIC_ENABLED: bool = cfg!(feature = "semantic");

// ---------------------------------------------------------------------------
// The emitted shape
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct Schema {
    schema_version: u32,
    tool: Tool,
    build: Build,
    hunkset: Hunkset,
    errors: Vec<ErrorEntry>,
    commands: Vec<CommandEntry>,
}

#[derive(Serialize)]
struct Tool {
    name: &'static str,
    version: &'static str,
}

#[derive(Serialize)]
struct Build {
    /// Whether the tree-sitter predicates work in *this* binary.
    semantic: bool,
    features: Vec<&'static str>,
}

#[derive(Serialize)]
struct Hunkset {
    classes: Vec<ClassEntry>,
    pattern_prefixes: &'static [&'static str],
    operators: Vec<OperatorEntry>,
    constants: Vec<ConstantEntry>,
    predicates: Vec<PredicateEntry>,
}

#[derive(Serialize)]
struct ClassEntry {
    name: &'static str,
    reaches_hunkless_changes: bool,
    description: &'static str,
}

#[derive(Serialize)]
struct OperatorEntry {
    symbol: &'static str,
    /// `infix` or `prefix`. Without it the two `~` entries are indistinguishable.
    position: &'static str,
    name: &'static str,
    description: &'static str,
}

/// No `class` field. `all` and `none` are not predicates, and inventing a
/// fourth class name for them would hand a caller a value that `classes` does
/// not define.
#[derive(Serialize)]
struct ConstantEntry {
    name: &'static str,
    reaches_hunkless_changes: bool,
    summary: &'static str,
}

#[derive(Serialize)]
struct PredicateEntry {
    name: &'static str,
    class: &'static str,
    /// The whole point: false means this predicate cannot select a binary, a
    /// pure rename, a mode-only flip, a retargeted symlink or an empty add,
    /// whatever argument it is given.
    reaches_hunkless_changes: bool,
    arity: &'static str,
    argument: &'static str,
    pattern_prefixes: &'static [&'static str],
    #[serde(skip_serializing_if = "Option::is_none")]
    default_pattern_kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    values: Option<&'static [&'static str]>,
    /// False in a build without the feature this predicate needs. Using it
    /// then fails with `SEMANTIC_FEATURE_REQUIRED`.
    available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    requires_feature: Option<&'static str>,
    summary: &'static str,
}

#[derive(Serialize)]
struct ErrorEntry {
    code: &'static str,
    category: &'static str,
}

#[derive(Serialize)]
struct CommandEntry {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    /// Whether this verb takes a hunkset expression (or a JSON/YAML spec).
    accepts_selection: bool,
    /// Whether it has `--allow-empty`, i.e. whether it can fail with
    /// `EMPTY_SELECTION`.
    has_allow_empty: bool,
    /// Whether it has `--dry-run`, i.e. whether its outcome can be read before
    /// it writes.
    ///
    /// Here for the same reason `has_allow_empty` is: an agent choosing a verb
    /// wants "can I look before I leap?" answered in one read, not by running
    /// `--help` eight times or by discovering the flag's absence as a usage
    /// error on a real revision. The shape of what `--dry-run` prints differs
    /// between `absorb` (prose, because it is what a real absorb prints too)
    /// and the five rewriting verbs (JSON), which is documented rather than
    /// published: this field answers whether the flag exists, not what it emits.
    has_dry_run: bool,
}

/// A predicate's class, computed from the two facts that define it rather than
/// written down a third time.
fn class_of(facts: &PredicateFacts) -> &'static str {
    if facts.semantic {
        "semantic"
    } else if facts.reaches_hunkless {
        "file"
    } else {
        "content"
    }
}

fn classes() -> Vec<ClassEntry> {
    vec![
        ClassEntry {
            name: "file",
            reaches_hunkless_changes: true,
            description: "Reads only file-level facts -- path, status, the shape of the change. \
                          Reaches every change in the diff, including the ones that produced no \
                          hunks at all: binaries, pure renames, mode-only flips, retargeted \
                          symlinks, empty adds and removes.",
        },
        ClassEntry {
            name: "content",
            reaches_hunkless_changes: false,
            description: "Reads the diffed text or its line numbers. A change with no hunks has \
                          no diffed text, so no argument makes one of these match it. A selection \
                          built only from content predicates silently leaves such changes behind, \
                          at exit 0.",
        },
        ClassEntry {
            name: "semantic",
            reaches_hunkless_changes: false,
            description: "Content-level, plus tree-sitter metadata: everything said about content \
                          applies. Additionally needs the 'semantic' feature (see build.semantic) \
                          and returns nothing for files no bundled parser can read.",
        },
    ]
}

fn predicates() -> Vec<PredicateEntry> {
    PREDICATES
        .iter()
        .map(|facts| {
            let (arity, argument) = shape_of(facts.name);
            PredicateEntry {
                name: facts.name,
                class: class_of(facts),
                reaches_hunkless_changes: facts.reaches_hunkless,
                arity,
                argument,
                pattern_prefixes: prefixes_for(facts.name, argument),
                default_pattern_kind: default_kind(facts.name).map(kind_name),
                values: values_for(facts.name),
                available: !facts.semantic || SEMANTIC_ENABLED,
                requires_feature: facts.semantic.then_some("semantic"),
                summary: facts.summary,
            }
        })
        .collect()
}

/// Describe the verbs from clap's own view of them, so a renamed flag or a new
/// subcommand cannot leave a stale entry here.
///
/// Deliberately *not* a full command schema. Flags, arity and defaults are all
/// in `--help`, which clap generates from these same definitions and which
/// therefore cannot drift either -- restating them here would double this
/// output to say what a caller can already read. What `--help` does not say,
/// and what this answers in one read instead of eight, is which verbs take a
/// hunkset at all and which can refuse an empty one.
fn commands(cli: &clap::Command) -> Vec<CommandEntry> {
    cli.get_subcommands()
        .map(|sub| {
            let has = |id: &str| sub.get_arguments().any(|arg| arg.get_id() == id);
            CommandEntry {
                name: sub.get_name().to_string(),
                summary: sub.get_about().map(ToString::to_string),
                accepts_selection: has("spec") || has("spec_file"),
                has_allow_empty: has("allow_empty"),
                has_dry_run: has("dry_run"),
            }
        })
        .collect()
}

fn build(cli: &clap::Command) -> Schema {
    Schema {
        schema_version: SCHEMA_VERSION,
        tool: Tool {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        },
        build: Build {
            semantic: SEMANTIC_ENABLED,
            features: if SEMANTIC_ENABLED {
                vec!["semantic"]
            } else {
                Vec::new()
            },
        },
        hunkset: Hunkset {
            classes: classes(),
            pattern_prefixes: PATTERN_PREFIXES,
            operators: OPERATORS
                .iter()
                .map(|&(symbol, position, name, description)| OperatorEntry {
                    symbol,
                    position,
                    name,
                    description,
                })
                .collect(),
            constants: CONSTANTS
                .iter()
                .map(|&(name, reaches, summary)| ConstantEntry {
                    name,
                    reaches_hunkless_changes: reaches,
                    summary,
                })
                .collect(),
            predicates: predicates(),
        },
        errors: errors::ALL
            .iter()
            .map(|code| ErrorEntry {
                code: code.code,
                category: code.category,
            })
            .collect(),
        commands: commands(cli),
    }
}

/// Print the schema to stdout as pretty JSON.
///
/// Pretty rather than compact, matching `list --format json`: this is read once
/// per session, and the indentation buys a human being able to read the same
/// bytes the agent did.
pub fn schema(cli: &clap::Command) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&build(cli))?);
    Ok(())
}

// ---------------------------------------------------------------------------
// Drift guards
// ---------------------------------------------------------------------------
//
// Everything below exists because a hand-written description of a language is
// worth nothing once it stops describing the language. Each test pins one of
// the facts in PREDICATES against the code that actually implements it.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{Hunk, LineRange, SemanticInfo};
    use crate::hunkset::{evaluate, parse, EnrichedHunk, HunksetError};
    use std::collections::HashSet;

    /// The dispatch table in `eval.rs`, read out of the source itself.
    ///
    /// Scraping is ugly and will need a nudge if that `match` is reformatted --
    /// which is the trade: a test that fails loudly on a reformat, against a
    /// schema that goes quietly wrong on a new predicate. There is no type to
    /// reflect over here; the set of predicate names exists only as match arms.
    fn dispatch_table_names() -> HashSet<String> {
        let source = include_str!("hunkset/eval.rs");
        let body = source
            .split_once("fn evaluate_function(")
            .expect("evaluate_function must exist -- did it get renamed?")
            .1
            .split_once("_ => Err(HunksetError::UnknownFunction")
            .expect("the catch-all arm must exist -- did it get rewritten?")
            .0;

        let mut names = HashSet::new();
        for line in body.lines() {
            let trimmed = line.trim();
            // An arm is `"a" | "b" => ...`; a line inside an arm's body never
            // both starts with a quote and carries a fat arrow.
            if !trimmed.starts_with('"') || !trimmed.contains("=>") {
                continue;
            }
            let head = trimmed.split("=>").next().unwrap_or_default();
            for part in head.split('|') {
                let name = part.trim().trim_matches('"').trim();
                if !name.is_empty() {
                    names.insert(name.to_string());
                }
            }
        }
        assert!(
            names.len() > 5,
            "the scrape found only {names:?} -- the match was probably reformatted"
        );
        names
    }

    fn schema_names() -> HashSet<String> {
        PREDICATES.iter().map(|p| p.name.to_string()).collect()
    }

    /// The schema's predicate list and the evaluator's dispatch table must name
    /// the same set.
    ///
    /// This is the drift this whole feature would otherwise create: a predicate
    /// added to `eval.rs` and forgotten here is invisible to every agent that
    /// trusts the schema, and one deleted from `eval.rs` but left here is
    /// advertised and then errors.
    #[test]
    fn the_predicate_list_matches_the_evaluators_dispatch_table() {
        let (dispatch, schema) = (dispatch_table_names(), schema_names());
        let missing: Vec<_> = dispatch.difference(&schema).collect();
        let extra: Vec<_> = schema.difference(&dispatch).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "schema and eval.rs disagree: eval.rs has {missing:?} that the schema omits, \
             and the schema advertises {extra:?} that eval.rs does not implement"
        );
    }

    /// Every advertised name is one the evaluator actually resolves, and names
    /// it does not advertise are not.
    ///
    /// The scrape above compares two lists of strings; this runs them. Calling
    /// a predicate with no arguments is enough to tell the two apart: a real
    /// one either succeeds or complains about its arguments, while an unknown
    /// one falls through to UNKNOWN_FUNCTION.
    #[test]
    fn advertised_names_resolve_and_unadvertised_ones_do_not() {
        for facts in PREDICATES {
            let expr = parse(&format!("{}()", facts.name)).expect("a call must parse");
            if let Err(HunksetError::UnknownFunction { name }) = evaluate(&expr, &[]) {
                panic!("the schema advertises {name}(), which the evaluator does not implement");
            }
        }
        for name in ["nope", "path", "text", "files", "hunk", "annotations"] {
            let expr = parse(&format!("{name}()")).expect("a call must parse");
            assert!(
                matches!(
                    evaluate(&expr, &[]),
                    Err(HunksetError::UnknownFunction { .. })
                ),
                "{name}() is not in the schema but the evaluator resolved it"
            );
        }
    }

    /// A hunk with something for every predicate to find.
    fn real_hunk() -> Hunk {
        Hunk {
            index: 0,
            id: format!("hunk-{}", "ab".repeat(32)),
            short_id: "hunk-abab".to_string(),
            hunk_type: "insert".to_string(),
            removed: "gone\n".to_string(),
            added: "TODO\n".to_string(),
            before_range: LineRange {
                start: 1,
                length: 1,
            },
            after_range: LineRange {
                start: 1,
                length: 1,
            },
            context: None,
            semantic: SemanticInfo {
                enclosing_function: Some("f".to_string()),
                enclosing_scope: Some("S".to_string()),
                annotations: vec!["test".to_string()],
                is_doc_comment: true,
                is_import: true,
                is_toplevel: true,
                nesting_depth: 0,
                is_analyzed: true,
            },
        }
    }

    /// What a pure rename leaves behind: a change with no hunks, standing in
    /// for the whole file. This is the thing the class split is about.
    fn stand_in_hunk() -> Hunk {
        Hunk {
            index: 0,
            id: "whole-file:b.rs".to_string(),
            short_id: "whole-file:b.rs".to_string(),
            hunk_type: "replace".to_string(),
            removed: String::new(),
            added: String::new(),
            before_range: LineRange {
                start: 0,
                length: 0,
            },
            after_range: LineRange {
                start: 0,
                length: 0,
            },
            context: None,
            semantic: SemanticInfo::default(),
        }
    }

    /// An expression per predicate that matches [`real_hunk`] as widely as the
    /// predicate allows -- so that whether it also reaches the stand-in is
    /// decided by the predicate's class and by nothing else.
    ///
    /// The degenerate arguments are deliberate. `content("")` and
    /// `lines(0..1000)` are the two that used to reach a stand-in, back when a
    /// stand-in was kept unmatchable by having empty text and a zero line
    /// range: the empty substring is inside every string, and line 0 is inside
    /// `0..N`.
    fn probe_for(name: &str) -> &'static str {
        match name {
            "file" => r#"file(glob:"*.rs")"#,
            "glob" => r#"glob("*.rs")"#,
            "extension" => r#"extension("rs")"#,
            "status" => r#"status("modified", "renamed")"#,
            "type" => r#"type("insert", "replace")"#,
            "content" => r#"content("")"#,
            "added" => r#"added("")"#,
            "removed" => r#"removed("")"#,
            "lines" => "lines(0..1000)",
            "before_line" => "before_line(0..1000)",
            "after_line" => "after_line(0..1000)",
            "id" => r#"id("hunk-abab")"#,
            "function" => r#"function("f")"#,
            "scope" => r#"scope("S")"#,
            "annotation" => "annotation()",
            "decorator" => "decorator()",
            "doc" => "doc()",
            "import" => "import()",
            "toplevel" => "toplevel()",
            "depth" => "depth(0..100)",
            other => panic!(
                "no probe expression for {other}() -- add one, or the class it claims goes unchecked"
            ),
        }
    }

    /// Run `expr` over one real hunk (index 0) and one stand-in (index 1).
    ///
    /// An error counts as "selected nothing", which is what a semantic
    /// predicate does in a `--no-default-features` build.
    fn probe(expr: &str) -> HashSet<usize> {
        let (real, stand_in) = (real_hunk(), stand_in_hunk());
        let hunks = vec![
            EnrichedHunk::real("a.rs", None, "modified", &real),
            EnrichedHunk::stand_in("b.rs", Some("was-b.rs"), "renamed", &stand_in),
        ];
        let parsed = parse(expr).unwrap_or_else(|e| panic!("probe {expr:?} does not parse: {e}"));
        evaluate(&parsed, &hunks).unwrap_or_default()
    }

    /// **The one that matters.** Every predicate's advertised class is checked
    /// by running it against a real hunk and a hunkless change and seeing which
    /// come back.
    ///
    /// Without this, `class` is a comment: someone adds a predicate that reads
    /// `EnrichedHunk::content()`, calls it file-level out of habit, and an
    /// agent then believes `split '<that>()'` will carry a rename along. It
    /// will not, it will exit 0, and the rename will be reset.
    ///
    /// Each probe must also match the real hunk. An expression that matched
    /// nothing at all would otherwise "prove" every predicate content-level.
    #[test]
    fn class_matches_what_each_predicate_does_to_a_hunkless_change() {
        for facts in PREDICATES {
            let expression = probe_for(facts.name);
            let selected = probe(expression);

            if facts.semantic && !SEMANTIC_ENABLED {
                assert!(
                    selected.is_empty(),
                    "{expression} matched something in a build without the semantic feature"
                );
                continue;
            }

            assert!(
                selected.contains(&0),
                "the probe {expression} does not even match an ordinary hunk, so it proves \
                 nothing about {}()'s class",
                facts.name
            );
            assert_eq!(
                selected.contains(&1),
                facts.reaches_hunkless,
                "{}() is advertised as class {:?} (reaches_hunkless_changes = {}), but \
                 {expression} {} the hunkless change",
                facts.name,
                class_of(facts),
                facts.reaches_hunkless,
                if selected.contains(&1) {
                    "selected"
                } else {
                    "did not select"
                }
            );
        }
    }

    /// The constants get the same treatment, since `all` is the only spelling
    /// that reaches everything and callers lean on that.
    #[test]
    fn the_constants_reach_what_they_claim_to() {
        for &(name, reaches, _) in CONSTANTS {
            let selected = probe(name);
            assert_eq!(
                selected.contains(&1),
                reaches,
                "{name} claims reaches_hunkless_changes = {reaches}"
            );
        }
        assert_eq!(probe("all").len(), 2, "all must reach every change");
        assert!(probe("none").is_empty(), "none must reach nothing");
    }

    /// `all` and `none` are reported as constants rather than predicates
    /// because the grammar refuses to give them arguments. If that ever
    /// changed, their `arity: "none"` would be a lie.
    #[test]
    fn the_constants_take_no_arguments() {
        for name in ["all", "none"] {
            assert!(
                parse(&format!(r#"{name}("x")"#)).is_err(),
                "{name} accepted an argument, so it is a predicate now"
            );
        }
    }

    /// `id()` names a hunk by a digest of its text. A stand-in has no text, so
    /// it has no id -- and the name it does carry internally must not be a way
    /// in through the front door.
    #[test]
    fn a_stand_ins_internal_name_is_not_an_id() {
        let expr = parse(r#"id("whole-file:b.rs")"#).expect("parses");
        let (real, stand_in) = (real_hunk(), stand_in_hunk());
        let hunks = vec![
            EnrichedHunk::real("a.rs", None, "modified", &real),
            EnrichedHunk::stand_in("b.rs", Some("was-b.rs"), "renamed", &stand_in),
        ];
        assert!(
            evaluate(&expr, &hunks).is_err(),
            "a stand-in's internal name was accepted as a hunk id"
        );
    }

    /// The prefixes the schema advertises per predicate are the ones the
    /// evaluator accepts.
    ///
    /// `id()` is the reason this is per-predicate rather than one global list:
    /// it resolves its argument instead of matching with it, so
    /// `id(substring:"e2d4")` is refused rather than quietly reinterpreted.
    ///
    /// The argument has to be one the predicate would accept unprefixed, or
    /// this measures the wrong thing -- `status(glob:"x")` is refused for "x"
    /// not being a status, which says nothing about `glob:`. That is also the
    /// reading of the enum predicates worth writing down: a prefix is accepted
    /// there but cannot widen anything, because the value is still checked
    /// against the literal list. `values` is what a caller should read for
    /// those; their prefixes are honest, not useful.
    #[test]
    fn advertised_pattern_prefixes_are_the_ones_the_evaluator_accepts() {
        for facts in PREDICATES {
            let (_, argument) = shape_of(facts.name);
            if argument != "pattern" {
                continue;
            }
            let value = match values_for(facts.name) {
                Some(values) => values[0],
                // Hex, so that `id()` gets as far as looking the id up rather
                // than rejecting the argument for not being id-shaped.
                None if facts.name == "id" => "abcd",
                None => "x",
            };
            let advertised = prefixes_for(facts.name, argument);
            for prefix in PATTERN_PREFIXES {
                let expr = format!(r#"{}({prefix}:"{value}")"#, facts.name);
                let parsed = parse(&expr).expect("a prefixed call must parse");
                let refused = matches!(
                    evaluate(&parsed, &[]),
                    Err(HunksetError::InvalidArgument { .. })
                );
                assert_eq!(
                    !refused,
                    advertised.contains(prefix),
                    "{expr} is {} by the evaluator, but the schema advertises {advertised:?}",
                    if refused { "refused" } else { "accepted" }
                );
            }
        }
    }

    /// A closed value set is published for exactly the predicates the evaluator
    /// checks against one. Publishing one for a predicate that does not have
    /// one would be a lie; omitting one that does costs an agent the single
    /// most common silent empty selection -- a misspelled `status`.
    #[test]
    fn published_value_sets_match_the_predicates_that_are_validated_against_one() {
        let source = include_str!("hunkset/eval.rs");
        let validated: HashSet<&str> = source
            .match_indices("validate_enum_args(\"")
            .filter_map(|(at, _)| {
                let rest = &source[at + "validate_enum_args(\"".len()..];
                rest.split_once('"').map(|(name, _)| name)
            })
            .collect();
        let published: HashSet<&str> = PREDICATES
            .iter()
            .filter(|p| values_for(p.name).is_some())
            .map(|p| p.name)
            .collect();
        assert_eq!(
            validated, published,
            "eval.rs validates {validated:?} against a value list; the schema publishes one for \
             {published:?}"
        );
    }

    /// Every code declared in `errors.rs` reaches `errors::ALL`, and so reaches
    /// the schema.
    ///
    /// The list this replaces had already fallen behind by one code. An agent
    /// that pre-wrote its retry table from an incomplete list would treat the
    /// missing code as an unknown failure -- which is the prose-matching
    /// fallback the codes exist to retire.
    #[test]
    fn every_declared_error_code_is_published() {
        let source = include_str!("errors.rs");
        // Anchored at the start of a line, so that prose in a doc comment
        // that happens to spell out a declaration is not read as one. It was:
        // this test's first run found a sixteenth code named "...", quoted out
        // of the comment on `ALL`.
        let declared: HashSet<&str> = source
            .lines()
            .filter_map(|line| line.strip_prefix("pub const "))
            .filter_map(|rest| {
                let (name, tail) = rest.split_once(':')?;
                tail.trim_start()
                    .starts_with("ErrorCode")
                    .then_some(name.trim())
            })
            .collect();
        let published: HashSet<&str> = errors::ALL.iter().map(|c| c.code).collect();
        assert!(
            !declared.is_empty(),
            "the scrape found no codes -- errors.rs was probably restructured"
        );
        assert_eq!(
            declared, published,
            "errors.rs declares {declared:?} but errors::ALL publishes {published:?}"
        );
    }

    /// In a build without the feature, the predicates the schema marks
    /// unavailable are exactly the ones that refuse to run.
    ///
    /// This is the direction that matters for honesty: CI builds
    /// `--no-default-features`, and a schema that claimed a tree-sitter
    /// predicate was available there would send an agent down a path that
    /// always fails.
    #[cfg(not(feature = "semantic"))]
    #[test]
    fn unavailable_predicates_are_exactly_the_ones_this_build_refuses() {
        for facts in PREDICATES {
            // A shape error would be raised before the feature check, so give
            // each predicate arguments it actually accepts.
            let expr = parse(probe_for(facts.name)).expect("probe parses");
            let refused = matches!(
                evaluate(&expr, &[]),
                Err(HunksetError::SemanticFeatureRequired { .. })
            );
            assert_eq!(
                refused, facts.semantic,
                "{}() is advertised available = {}, but this build {} it",
                facts.name,
                !facts.semantic,
                if refused { "refuses" } else { "accepts" }
            );
        }
        assert!(
            predicates().iter().any(|p| !p.available),
            "no predicate reported unavailable in a build without the semantic feature"
        );
    }

    /// ...and in a full build nothing is refused, so `available: true` across
    /// the board is the truth rather than a default nobody checked.
    #[cfg(feature = "semantic")]
    #[test]
    fn a_semantic_build_refuses_no_predicate() {
        for facts in PREDICATES {
            let expr = parse(probe_for(facts.name)).expect("probe parses");
            assert!(
                !matches!(
                    evaluate(&expr, &[]),
                    Err(HunksetError::SemanticFeatureRequired { .. })
                ),
                "{}() was refused for the semantic feature in a build that has it",
                facts.name
            );
            assert!(
                predicates()
                    .iter()
                    .any(|p| p.name == facts.name && p.available),
                "{}() is reported unavailable in a build with the semantic feature",
                facts.name
            );
        }
    }

    /// Serialising must not panic, and the fields callers are told to branch on
    /// must be present under the names they are told to use.
    #[test]
    fn the_published_shape_has_the_documented_fields() {
        let cli = clap::Command::new("jj-hunk").subcommand(
            clap::Command::new("list").about("List hunks").arg(
                clap::Arg::new("spec")
                    .long("spec")
                    .action(clap::ArgAction::Set),
            ),
        );
        let json: serde_json::Value =
            serde_json::to_value(build(&cli)).expect("the schema must serialise");

        assert_eq!(json["schema_version"], SCHEMA_VERSION);
        assert_eq!(json["build"]["semantic"], SEMANTIC_ENABLED);

        let content = json["hunkset"]["predicates"]
            .as_array()
            .expect("predicates is an array")
            .iter()
            .find(|p| p["name"] == "content")
            .expect("content() must be published");
        assert_eq!(content["class"], "content");
        assert_eq!(content["reaches_hunkless_changes"], false);
        assert_eq!(content["default_pattern_kind"], "substring");

        let commands = json["commands"].as_array().expect("commands is an array");
        assert_eq!(commands[0]["name"], "list");
        assert_eq!(commands[0]["accepts_selection"], true);
        assert_eq!(commands[0]["has_allow_empty"], false);
        assert_eq!(commands[0]["has_dry_run"], false);
    }
}
