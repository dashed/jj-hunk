//! `--dry-run` for the five verbs that rewrite history: what would be true
//! afterwards.
//!
//! # Why this is not "which hunks matched"
//!
//! `jj-hunk list --spec '<expr>'` already answers that, and the docs teach the
//! list -> preview -> act loop around it. Reprinting the matched hunks under a
//! different flag would add nothing.
//!
//! What `list` cannot show is the *consequence*, and the verbs disagree about
//! consequence in exactly the way that costs people a revision:
//!
//! | verb       | the hunks you name              | the hunks you do not name |
//! |------------|---------------------------------|---------------------------|
//! | `split`    | go into the first commit        | go into the second        |
//! | `commit`   | are committed                   | stay in the working copy  |
//! | `squash`   | move into the parent            | stay where they are       |
//! | `diffedit` | are **kept**                    | are **discarded**         |
//! | `restore`  | are **undone**                  | are left alone            |
//!
//! The last two are near-inverses of each other, and `list` shows the same set
//! of hunks for both. So a preview that only names hunks cannot tell them
//! apart; one that names the *outcome* can, and that is what this module
//! prints. For those two, `selected.effect` alone is enough -- `keep` against
//! `undo` -- and so is asking which half has `loses_content`.
//!
//! Across all five it takes `effect` **and** `lands_in.kind` on both halves,
//! because `split` and `commit` do the same thing to the hunks and differ only
//! in where the remainder ends up (a second commit, or the working copy). That
//! is a fact about the two verbs, not a gap in the output, so the effect
//! vocabulary is not padded out to make them look different. The test
//! `each_verb_has_a_distinct_signature` pins the four fields that do separate
//! them.
//!
//! # Shape and spirit
//!
//! [`crate::absorb`] has had `--dry-run` since before this module existed, and
//! its plan is the model followed here: a headline sentence, then each
//! destination with what lands there, then the notes, then a statement that
//! nothing was changed. The difference is the rendering -- absorb prints prose
//! because that same text is what a real `absorb` prints too, so it is not a
//! dry-run-only contract and cannot be changed without changing absorb's
//! output. These five verbs print nothing on stdout today, so `--dry-run` is
//! free to be JSON from the start, and is:
//!
//! * There is no default to preserve. A `--format` flag would mean inventing a
//!   human rendering as well, and prose an agent has to regex is precisely
//!   what this project is retiring.
//! * `list --format json` and `schema` already put JSON on stdout, so a
//!   caller's stdout handling does not have to fork on the verb.
//! * Pretty-printed rather than compact, matching `schema`: this is read once
//!   before a decision, and the indentation buys a human reading the same bytes
//!   the agent did.
//!
//! Every fact in the prose fields (`summary`, `describe`) is also present as
//! structure. Nothing requires parsing them.
//!
//! # What a dry run does *not* catch
//!
//! Every check `jj-hunk` itself performs runs here, in the same order, with the
//! same codes -- because the dry run is the real code path with the final
//! `jj` invocation removed, not a second implementation of it. What it cannot
//! run is jj's own refusals: an immutable revision, a concurrent operation, a
//! rebase that conflicts. Those are still discovered by running for real.

use crate::commands::{self, DiffTarget, FileHunks, SpecDecision};
use crate::errors::{self, CodedError};
use crate::spec::Spec;
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::HashSet;
use std::process::Command;

/// The verb being previewed, held as its own arguments rather than as an
/// already-resolved plan.
///
/// Resolving a revision costs a `jj log`, and doing that up front would change
/// which failure a caller sees: `split --dry-run 'bad(' -r nosuchrev` must
/// still report the parse error, because that is what the real run reports.
/// So nothing here is resolved until every check has passed, which is after
/// the selection has been evaluated and validated.
#[derive(Debug, Clone, Copy)]
pub enum Verb<'a> {
    Split { message: &'a str },
    Commit { message: &'a str },
    Squash,
    Diffedit,
    Restore,
}

impl Verb<'_> {
    fn name(&self) -> &'static str {
        match self {
            Verb::Split { .. } => "split",
            Verb::Commit { .. } => "commit",
            Verb::Squash => "squash",
            Verb::Diffedit => "diffedit",
            Verb::Restore => "restore",
        }
    }
}

/// What happens to one half of the diff.
///
/// The five values are the whole vocabulary, and they are what makes two verbs
/// distinguishable without knowing either in advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Effect {
    /// Written into a commit this run creates.
    Commit,
    /// Left where it is.
    Keep,
    /// Moved into a revision that already exists.
    Move,
    /// Dropped. The file goes back to what the other side of this diff has.
    Discard,
    /// Reverted: the content is replaced by another revision's.
    Undo,
}

impl Effect {
    fn as_str(self) -> &'static str {
        match self {
            Effect::Commit => "commit",
            Effect::Keep => "keep",
            Effect::Move => "move",
            Effect::Discard => "discard",
            Effect::Undo => "undo",
        }
    }

    /// Whether this half's content survives anywhere in the repository.
    ///
    /// The one field a caller has to read before deciding whether a preview is
    /// safe to turn into a real run.
    fn loses_content(self) -> bool {
        matches!(self, Effect::Discard | Effect::Undo)
    }
}

// ---------------------------------------------------------------------------
// Revisions
// ---------------------------------------------------------------------------

/// A revision, named three ways.
///
/// `change_id` rather than `commit_id` is what a caller should follow: every
/// verb here rewrites something, and a rewrite keeps the change id and mints a
/// new commit id. The commit id is reported anyway because it is what the
/// repository looks like *now*, which is what a preview is about.
#[derive(Debug, Clone, Serialize)]
pub struct RevisionRef {
    /// The revset that names it, as the caller would write it.
    revset: String,
    change_id: String,
    commit_id: String,
}

/// `commit_id`, then the change id. Same shape absorb reads, minus the fields
/// absorb needs and this does not.
const REV_TEMPLATE: &str = r#"commit_id ++ "\t" ++ change_id ++ "\n""#;

/// Resolve `revset` to exactly one revision, reporting it under `label`.
///
/// `label` exists because the parent of a revision is resolved by the commit
/// id `resolve_revisions` already pinned -- re-resolving `(<revset>)-` could
/// land somewhere else if the revset is not stable -- while the string a
/// caller would type for it is `<revset>-`.
///
/// The two failure codes are the ones `resolve_single_revision` raises for the
/// same two failures. A dry run that reported a revset problem under a
/// different code than the real run would be worse than no dry run.
fn revision_ref(revset: &str, label: &str) -> Result<RevisionRef> {
    let output = Command::new("jj")
        .args(["log", "--no-graph", "-r", revset, "-T", REV_TEMPLATE])
        .output()
        .context("Failed to run jj log")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CodedError::new(
            errors::REVSET_UNRESOLVED,
            format!("failed to resolve revset `{}`: {}", revset, stderr.trim()),
        )
        .with("revset", revset)
        .with("resolved", 0)
        .with("jj_stderr", stderr.trim())
        .into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let rows: Vec<&str> = stdout.lines().filter(|line| !line.trim().is_empty()).collect();
    match rows.len() {
        1 => {
            let (commit_id, change_id) = rows[0].split_once('\t').unwrap_or((rows[0], ""));
            Ok(RevisionRef {
                revset: label.to_string(),
                change_id: change_id.trim().to_string(),
                commit_id: commit_id.trim().to_string(),
            })
        }
        0 => Err(CodedError::new(
            errors::REVSET_UNRESOLVED,
            format!("revset `{}` did not resolve to any revision", revset),
        )
        .with("revset", revset)
        .with("resolved", 0)
        .into()),
        n => Err(CodedError::new(
            errors::REVSET_AMBIGUOUS,
            format!(
                "revset `{}` resolved to {} revisions, but jj-hunk needs exactly one.",
                revset, n
            ),
        )
        .with("revset", revset)
        .with("resolved", n)
        .into()),
    }
}

/// The two revisions the previewed diff is taken between.
///
/// Named `left`/`right` rather than before/after on purpose. For `restore` the
/// target is deliberately reversed -- the destination is on the left and the
/// source on the right -- so "before" and "after" would mean the opposite of
/// what they mean everywhere else. `left`/`right` always describe the same
/// thing: the two sides of the listing named in `listing`.
struct Endpoints {
    /// `None` only for a root commit, which has no parent to diff against.
    left: Option<RevisionRef>,
    right: RevisionRef,
}

fn endpoints(target: &DiffTarget) -> Result<Endpoints> {
    match target {
        DiffTarget::Rev(rev) => {
            let revset = rev.as_deref().unwrap_or("@");
            let right = revision_ref(revset, revset)?;
            let left = match commands::resolve_revisions(target)?.before {
                Some(parent) => Some(revision_ref(&parent, &format!("{revset}-"))?),
                None => None,
            };
            Ok(Endpoints { left, right })
        }
        DiffTarget::FromTo { from, to } => Ok(Endpoints {
            left: Some(revision_ref(from, from)?),
            right: revision_ref(to, to)?,
        }),
    }
}

// ---------------------------------------------------------------------------
// The plan
// ---------------------------------------------------------------------------

/// Where a half's outcome is realised.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum Landing {
    /// A revision that already exists and survives this run.
    Revision {
        #[serde(flatten)]
        revision: RevisionRef,
    },
    /// The working-copy commit. Deliberately carries no ids: `commit` replaces
    /// `@` with a fresh commit, so naming today's would name the wrong one.
    WorkingCopy,
    /// A commit this run would create, whose ids cannot be known in advance.
    NewCommit {
        /// How jj will refer to it in the output of the real run.
        label: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

#[derive(Debug, Serialize)]
struct Half {
    /// Hunks in this half. A change with no hunks is counted separately, in
    /// `whole_file_changes`, because it is not selectable hunk-wise at all.
    hunks: usize,
    whole_file_changes: usize,
    /// Files contributing to this half. A file whose hunks land on both sides
    /// is counted in both, so the two need not sum to the number of files.
    files: usize,
    effect: &'static str,
    /// Whether this half's content survives anywhere in the repository.
    loses_content: bool,
    lands_in: Landing,
    /// The revision whose content takes the place of what is lost. Present
    /// exactly when `loses_content` is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    replaced_by: Option<RevisionRef>,
    /// The same facts as one English clause. Nothing needs to parse this.
    describe: String,
}

#[derive(Debug, Serialize)]
struct WholeFile {
    /// Which half this change falls into. It has no hunks, so it is kept or
    /// reset as a unit.
    selected: bool,
    /// Why it has no hunks: `binary`, `mode-change`, `symlink`, `rename`,
    /// `empty-file`.
    reason: &'static str,
}

#[derive(Debug, Serialize)]
struct FileOutcome {
    path: String,
    status: String,
    /// The path this file had on the left of the diff, for a rename or copy.
    #[serde(skip_serializing_if = "Option::is_none")]
    from: Option<String>,
    /// Hunk ids, in diff order.
    ///
    /// Full ids, not the abbreviations `list --format text` shows: this output
    /// is machine-readable, `list --format json` carries the full form, and a
    /// caller cross-referencing the two must be able to compare them for
    /// equality rather than for prefix.
    selected_hunks: Vec<String>,
    unselected_hunks: Vec<String>,
    /// Present only for a change no hunk can express.
    #[serde(skip_serializing_if = "Option::is_none")]
    whole_file: Option<WholeFile>,
}

#[derive(Debug, Serialize)]
pub struct Plan {
    /// Always true. A caller can assert on it before trusting that its
    /// repository is untouched.
    dry_run: bool,
    command: &'static str,
    /// Always false: the machine-readable form of absorb's closing
    /// "--dry-run: nothing was changed".
    changed: bool,
    /// The selection as it was written, hunkset or spec.
    selector: String,
    /// The `jj-hunk list` invocation whose hunk ids these are. For `restore`
    /// this is a reversed diff, which is why it is stated rather than assumed.
    listing: String,
    /// The revision this run would rewrite.
    revision: RevisionRef,
    selected: Half,
    unselected: Half,
    files: Vec<FileOutcome>,
    notes: Vec<String>,
    /// One line, the two halves joined. Absorb's headline, in a field.
    summary: String,
}

// ---------------------------------------------------------------------------
// Building it
// ---------------------------------------------------------------------------

/// Build and print the plan.
///
/// Called after every validation the real run performs, so anything reported
/// here is a problem the real run would have hit at the same point with the
/// same code.
pub fn report(
    verb: Verb<'_>,
    target: &DiffTarget,
    spec: Option<&Spec>,
    file_hunks: Option<&[FileHunks]>,
    raw_spec: &str,
    allow_empty: bool,
) -> Result<()> {
    let plan = build(verb, target, spec, file_hunks, raw_spec, allow_empty)?;
    println!("{}", serde_json::to_string_pretty(&plan)?);
    Ok(())
}

fn build(
    verb: Verb<'_>,
    target: &DiffTarget,
    spec: Option<&Spec>,
    file_hunks: Option<&[FileHunks]>,
    raw_spec: &str,
    allow_empty: bool,
) -> Result<Plan> {
    // Both are `None` only when the spec did not parse as JSON or YAML. The
    // real run hands that to `select`, which fails on it -- so failing here is
    // the same answer, reached one process earlier.
    let (Some(spec), Some(file_hunks)) = (spec, file_hunks) else {
        anyhow::bail!(
            "--dry-run cannot preview this selection: it is neither a hunkset \
             expression nor a spec this build can parse.\n\
             Run without --dry-run to see what `select` makes of it."
        );
    };

    let ends = endpoints(target)?;
    let files = outcomes(spec, file_hunks)?;

    let listing = target.listing_command();
    let (selected, unselected, revision) = halves(verb, &ends, &files)?;
    let notes = notes(verb, spec, file_hunks, &files, &listing, allow_empty);

    let summary = format!(
        "{} --dry-run: {}; {}. Nothing was changed.",
        verb.name(),
        selected.describe,
        unselected.describe
    );

    Ok(Plan {
        dry_run: true,
        command: verb.name(),
        changed: false,
        selector: raw_spec.trim().to_string(),
        listing,
        revision,
        selected,
        unselected,
        files,
        notes,
        summary,
    })
}

/// Split every change in the diff into the half it lands in.
fn outcomes(spec: &Spec, file_hunks: &[FileHunks]) -> Result<Vec<FileOutcome>> {
    let mut out = Vec::with_capacity(file_hunks.len());

    for fh in file_hunks {
        let decision = commands::spec_decision(Some(spec), &fh.path);
        // The same resolution `filter_hunks` performs for `select`, so the
        // preview and the run cannot disagree about which hunk an id names.
        let kept: HashSet<usize> = match &decision {
            SpecDecision::KeepAll => fh.hunks.iter().map(|hunk| hunk.index).collect(),
            SpecDecision::Skip => HashSet::new(),
            SpecDecision::KeepSelection(selection) => selection
                .resolve(&fh.hunks)
                .with_context(|| format!("cannot resolve the selection for {}", fh.path))?,
        };

        let mut selected_hunks = Vec::new();
        let mut unselected_hunks = Vec::new();
        for hunk in &fh.hunks {
            if kept.contains(&hunk.index) {
                selected_hunks.push(hunk.id.clone());
            } else {
                unselected_hunks.push(hunk.id.clone());
            }
        }

        // A change with no hunks is atomic: `select` keeps it whole under
        // `{"action": "keep"}` and resets it whole otherwise. A hunk selection
        // over it matches nothing, so it resets -- which is the half it is
        // reported in, rather than the one the entry looks like it asked for.
        let whole_file = (fh.hunks.is_empty() && fh.changes_without_hunks()).then(|| WholeFile {
            selected: matches!(decision, SpecDecision::KeepAll),
            reason: hunkless_reason(fh),
        });

        out.push(FileOutcome {
            path: fh.path.clone(),
            status: fh.status.clone(),
            from: fh.rename_source().map(str::to_string),
            selected_hunks,
            unselected_hunks,
            whole_file,
        });
    }

    Ok(out)
}

/// Why a change produced no hunks, in the order `changes_without_hunks` tests
/// them -- so a file that is two of these at once is reported as the one that
/// decided it.
fn hunkless_reason(fh: &FileHunks) -> &'static str {
    if fh.is_binary {
        "binary"
    } else if fh.mode.is_some() {
        "mode-change"
    } else if fh.is_symlink {
        "symlink"
    } else if fh.rename_source().is_some() {
        "rename"
    } else {
        "empty-file"
    }
}

/// Counts for one half.
struct Counts {
    hunks: usize,
    whole_file_changes: usize,
    files: usize,
}

fn count(files: &[FileOutcome], selected: bool) -> Counts {
    let mut counts = Counts {
        hunks: 0,
        whole_file_changes: 0,
        files: 0,
    };
    for file in files {
        let hunks = if selected {
            file.selected_hunks.len()
        } else {
            file.unselected_hunks.len()
        };
        let whole = file
            .whole_file
            .as_ref()
            .is_some_and(|w| w.selected == selected);
        if hunks > 0 || whole {
            counts.files += 1;
        }
        counts.hunks += hunks;
        counts.whole_file_changes += usize::from(whole);
    }
    counts
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

/// A subject phrase that remembers whether it is singular.
///
/// The sentences below are assembled, so the verb has to be chosen rather than
/// written: without this, one hunk read "1 hunk in 1 file are DISCARDED",
/// which is the line a human skims to decide whether to run for real.
struct Phrase {
    text: String,
    singular: bool,
}

impl Phrase {
    fn verb(&self, singular: &'static str, plural: &'static str) -> &'static str {
        if self.singular {
            singular
        } else {
            plural
        }
    }
}

impl std::fmt::Display for Phrase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// "2 hunks in 1 file", "1 hunk and 2 whole-file changes in 2 files",
/// "nothing".
fn phrase(counts: &Counts) -> Phrase {
    let mut parts = Vec::new();
    if counts.hunks > 0 {
        parts.push(plural(counts.hunks, "hunk", "hunks"));
    }
    if counts.whole_file_changes > 0 {
        parts.push(plural(
            counts.whole_file_changes,
            "whole-file change",
            "whole-file changes",
        ));
    }
    if parts.is_empty() {
        // "nothing is kept", not "nothing are kept".
        return Phrase {
            text: "nothing".to_string(),
            singular: true,
        };
    }
    Phrase {
        text: format!(
            "{} in {}",
            parts.join(" and "),
            plural(counts.files, "file", "files")
        ),
        singular: counts.hunks + counts.whole_file_changes == 1,
    }
}

fn short(id: &str) -> &str {
    let end = id.char_indices().nth(8).map_or(id.len(), |(i, _)| i);
    &id[..end]
}

fn label_of(revision: &RevisionRef) -> String {
    format!("{} ({})", revision.revset, short(&revision.change_id))
}

/// The two halves, each with its effect, its landing and its sentence, plus
/// the revision this run would rewrite.
///
/// This function is the whole of what distinguishes the five verbs. Everything
/// above it is the same code for all of them. `restore` rewrites the *left* of
/// its deliberately reversed target and every other verb rewrites the right,
/// which is the one fact that keeps the two apart in every field below.
fn halves(
    verb: Verb<'_>,
    ends: &Endpoints,
    files: &[FileOutcome],
) -> Result<(Half, Half, RevisionRef)> {
    let selected = count(files, true);
    let unselected = count(files, false);
    let kept = phrase(&selected);
    let dropped = phrase(&unselected);

    // The left of the target diff, and where a discarded or undone half's
    // content comes back from. Absent only when the right has no parent at all
    // -- the virtual root commit, which jj refuses to rewrite anyway -- so this
    // is a guard rather than a case. `split` and `commit` never ask for it.
    let left = || -> Result<RevisionRef> {
        ends.left.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "`{}` has no parent, so there is no revision for its content to \
                 come from",
                ends.right.revset
            )
        })
    };

    let rewrites = match verb {
        Verb::Restore => left()?,
        _ => ends.right.clone(),
    };

    let (selected, unselected) = match verb {
        // The selected half becomes jj's "Selected changes" and takes the
        // message; the rest becomes "Remaining changes". Both are commits this
        // run creates, so neither can be named by an id yet.
        Verb::Split { message } => (
            Half::new(
                selected,
                Effect::Commit,
                Landing::NewCommit {
                    label: "Selected changes",
                    message: Some(message.to_string()),
                },
                None,
                format!(
                    "{kept} {} into the first commit, described {message:?}",
                    kept.verb("goes", "go")
                ),
            ),
            Half::new(
                unselected,
                Effect::Keep,
                Landing::NewCommit {
                    label: "Remaining changes",
                    message: None,
                },
                None,
                format!(
                    "{dropped} {} in the second commit",
                    dropped.verb("stays", "stay")
                ),
            ),
        ),
        Verb::Commit { message } => (
            Half::new(
                selected,
                Effect::Commit,
                Landing::NewCommit {
                    label: "the new commit",
                    message: Some(message.to_string()),
                },
                None,
                format!(
                    "{kept} {} committed as {message:?}",
                    kept.verb("is", "are")
                ),
            ),
            Half::new(
                unselected,
                Effect::Keep,
                Landing::WorkingCopy,
                None,
                format!(
                    "{dropped} {} in the working copy",
                    dropped.verb("stays", "stay")
                ),
            ),
        ),
        Verb::Squash => {
            let parent = left()?;
            (
                Half::new(
                    selected,
                    Effect::Move,
                    Landing::Revision {
                        revision: parent.clone(),
                    },
                    None,
                    format!(
                        "{kept} {} into {}",
                        kept.verb("moves", "move"),
                        label_of(&parent)
                    ),
                ),
                Half::new(
                    unselected,
                    Effect::Keep,
                    Landing::Revision {
                        revision: ends.right.clone(),
                    },
                    None,
                    format!(
                        "{dropped} {} in {}",
                        dropped.verb("stays", "stay"),
                        label_of(&ends.right)
                    ),
                ),
            )
        }
        // Keeps what you name. The rest is dropped, and the file goes back to
        // what the left of the diff has.
        Verb::Diffedit => {
            let source = left()?;
            (
                Half::new(
                    selected,
                    Effect::Keep,
                    Landing::Revision {
                        revision: ends.right.clone(),
                    },
                    None,
                    format!(
                        "{kept} {} kept in {}",
                        kept.verb("is", "are"),
                        label_of(&ends.right)
                    ),
                ),
                Half::new(
                    unselected,
                    Effect::Discard,
                    Landing::Revision {
                        revision: ends.right.clone(),
                    },
                    Some(source.clone()),
                    format!(
                        "{dropped} {} DISCARDED from {}, whose content there goes back to {}",
                        dropped.verb("is", "are"),
                        label_of(&ends.right),
                        label_of(&source)
                    ),
                ),
            )
        }
        // Undoes what you name -- the exact opposite of diffedit, and the
        // reason the target diff is built the other way round. `right` is the
        // revision content is restored *from*; `left` is the destination it is
        // written into.
        Verb::Restore => {
            let destination = left()?;
            (
                Half::new(
                    selected,
                    Effect::Undo,
                    Landing::Revision {
                        revision: destination.clone(),
                    },
                    Some(ends.right.clone()),
                    format!(
                        "{kept} {} UNDONE in {}, whose content there is restored from {}",
                        kept.verb("is", "are"),
                        label_of(&destination),
                        label_of(&ends.right)
                    ),
                ),
                Half::new(
                    unselected,
                    Effect::Keep,
                    Landing::Revision {
                        revision: destination.clone(),
                    },
                    None,
                    format!(
                        "{dropped} {} left alone in {}",
                        dropped.verb("is", "are"),
                        label_of(&destination)
                    ),
                ),
            )
        }
    };

    Ok((selected, unselected, rewrites))
}

impl Half {
    fn new(
        counts: Counts,
        effect: Effect,
        lands_in: Landing,
        replaced_by: Option<RevisionRef>,
        describe: String,
    ) -> Self {
        debug_assert_eq!(
            effect.loses_content(),
            replaced_by.is_some(),
            "a half that loses content must say what replaces it, and one that \
             does not must not claim a replacement"
        );
        Self {
            hunks: counts.hunks,
            whole_file_changes: counts.whole_file_changes,
            files: counts.files,
            effect: effect.as_str(),
            loses_content: effect.loses_content(),
            lands_in,
            replaced_by,
            describe,
        }
    }
}

/// Facts the counts cannot carry, and that change what a caller should do.
fn notes(
    verb: Verb<'_>,
    spec: &Spec,
    file_hunks: &[FileHunks],
    files: &[FileOutcome],
    listing: &str,
    allow_empty: bool,
) -> Vec<String> {
    let mut notes = Vec::new();

    if matches!(verb, Verb::Restore) {
        notes.push(format!(
            "restore reads a reversed diff: these hunk ids come from \
             `{listing}`, not from `jj-hunk list -r <rev>`. An id copied from \
             the forward listing names nothing here."
        ));
    }

    let hunkless = files.iter().filter(|f| f.whole_file.is_some()).count();
    if hunkless > 0 {
        notes.push(format!(
            "{} of these changes produced no hunks at all, so no content-level \
             selector can reach them and each is kept or reset as a whole. See \
             `whole_file.reason` on each.",
            hunkless
        ));
    }

    // The other half of the same problem, and the one that has no field to
    // hang off: a file can have hunks *and* carry a change none of them
    // expresses -- a rename, an exec-bit flip, a retargeted symlink. `select`
    // lets that part ride along with the file's hunks, so it survives if any
    // hunk of that file survives and goes back if none does. Reading the two
    // hunk lists alone would suggest it can be split, which it cannot.
    let rides_along = file_hunks
        .iter()
        .filter(|fh| !fh.hunks.is_empty() && fh.changes_without_hunks())
        .count();
    if rides_along > 0 {
        notes.push(format!(
            "{} of these files carry a change no hunk expresses -- a rename, an \
             exec-bit flip, a retargeted symlink. That part cannot be selected \
             apart from the file's hunks: it is kept if any hunk of that file \
             is kept, and goes back if none is.",
            rides_along
        ));
    }

    // A spec key that names nothing in this diff is not always an error --
    // `validate_spec_resolves` lets a reusable allowlist keep its idle entries
    // -- but it does mean the run does less than the spec appears to ask for.
    let present: HashSet<&str> = file_hunks.iter().map(|fh| fh.path.as_str()).collect();
    let mut absent: Vec<&str> = spec
        .files
        .keys()
        .map(String::as_str)
        .filter(|path| !present.contains(path))
        .collect();
    if !absent.is_empty() {
        absent.sort_unstable();
        notes.push(format!(
            "the spec names {} that this diff does not contain, so they select \
             nothing here: {}",
            plural(absent.len(), "path", "paths"),
            absent.join(", ")
        ));
    }

    if allow_empty && spec.selects_nothing() {
        notes.push(
            "--allow-empty: this selection keeps nothing, and that was permitted."
                .to_string(),
        );
    }

    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rev(revset: &str, change: &str, commit: &str) -> RevisionRef {
        RevisionRef {
            revset: revset.to_string(),
            change_id: change.to_string(),
            commit_id: commit.to_string(),
        }
    }

    fn ends() -> Endpoints {
        Endpoints {
            left: Some(rev("@-", "parentchange", "parentcommit")),
            right: rev("@", "childchange", "childcommit"),
        }
    }

    /// One file, one hunk on each side, so every verb below is asked the same
    /// question and only the answer differs.
    fn two_hunks() -> Vec<FileOutcome> {
        vec![FileOutcome {
            path: "f.txt".to_string(),
            status: "modified".to_string(),
            from: None,
            selected_hunks: vec!["hunk-aaaa".to_string()],
            unselected_hunks: vec!["hunk-bbbb".to_string()],
            whole_file: None,
        }]
    }

    /// The property the whole module exists for: `diffedit` and `restore` show
    /// the same hunks, and a caller must be able to tell which way round they
    /// act without knowing either verb in advance.
    ///
    /// Reads `effect` and `loses_content` only -- the two fields a caller is
    /// told to branch on. If these ever agreed, a preview of `restore` would
    /// read exactly like a preview of `diffedit`, which is the mistake that
    /// costs a revision.
    #[test]
    fn diffedit_and_restore_are_mirror_images() {
        let files = two_hunks();
        let (edit_selected, edit_unselected, _) =
            halves(Verb::Diffedit, &ends(), &files).expect("diffedit has a parent");
        let (restore_selected, restore_unselected, _) =
            halves(Verb::Restore, &ends(), &files).expect("restore has a parent");

        assert_eq!(edit_selected.effect, "keep");
        assert_eq!(edit_unselected.effect, "discard");
        assert_eq!(restore_selected.effect, "undo");
        assert_eq!(restore_unselected.effect, "keep");

        // The half that loses content is the other one in each case.
        assert!(!edit_selected.loses_content && edit_unselected.loses_content);
        assert!(restore_selected.loses_content && !restore_unselected.loses_content);
    }

    /// No two verbs may produce the same preview. A caller that cannot tell
    /// `restore` from `diffedit`, or `commit` from `split`, has a preview that
    /// is worse than none: it looks like a confirmation.
    ///
    /// The signature is four fields, not one. `split` and `commit` share both
    /// effects because they do the same thing to the hunks; what separates them
    /// is where the *unselected* half lands -- a second commit against the
    /// working copy. Asserting on `selected.effect` alone would either fail
    /// here or force two names for one operation.
    #[test]
    fn each_verb_has_a_distinct_signature() {
        let files = two_hunks();
        let verbs = [
            Verb::Split { message: "m" },
            Verb::Commit { message: "m" },
            Verb::Squash,
            Verb::Diffedit,
            Verb::Restore,
        ];

        // Read back out of the serialised form, so the test cannot pass on a
        // discriminant a caller never sees.
        let kind = |landing: &Landing| {
            serde_json::to_value(landing).expect("a landing must serialise")["kind"]
                .as_str()
                .expect("every landing is tagged with its kind")
                .to_string()
        };

        let signatures: HashSet<String> = verbs
            .iter()
            .map(|verb| {
                let (selected, unselected, _) = halves(*verb, &ends(), &files)
                    .expect("every verb resolves against a revision with a parent");
                format!(
                    "{}/{} {}/{}",
                    selected.effect,
                    kind(&selected.lands_in),
                    unselected.effect,
                    kind(&unselected.lands_in)
                )
            })
            .collect();
        assert_eq!(signatures.len(), verbs.len(), "{signatures:?}");
    }

    /// `restore` writes into the destination, which is the *left* of its
    /// reversed target, and restores from the right. Getting this backwards
    /// would name the wrong revision as the one being rewritten -- the single
    /// most misleading thing this output could say.
    #[test]
    fn restore_rewrites_the_left_of_its_reversed_target() {
        let files = two_hunks();
        let (selected, _, rewrites) = halves(Verb::Restore, &ends(), &files).unwrap();
        assert_eq!(
            rewrites.revset, "@-",
            "the revision restore rewrites is the destination, not the source"
        );

        match &selected.lands_in {
            Landing::Revision { revision } => assert_eq!(revision.revset, "@-"),
            other => panic!("restore must land in a revision, got {other:?}"),
        }
        assert_eq!(
            selected.replaced_by.as_ref().map(|r| r.revset.as_str()),
            Some("@"),
            "the content comes from the right of the reversed diff"
        );
    }

    /// `squash` is the one verb whose selected half moves into a revision that
    /// already exists, and that revision is the parent.
    #[test]
    fn squash_moves_into_the_parent_and_keeps_the_rest_in_place() {
        let files = two_hunks();
        let (selected, unselected, rewrites) = halves(Verb::Squash, &ends(), &files).unwrap();
        assert_eq!(rewrites.revset, "@");

        match (&selected.lands_in, &unselected.lands_in) {
            (Landing::Revision { revision: to }, Landing::Revision { revision: stay }) => {
                assert_eq!(to.revset, "@-");
                assert_eq!(stay.revset, "@");
            }
            other => panic!("squash lands in two revisions, got {other:?}"),
        }
        assert!(!selected.loses_content && !unselected.loses_content);
    }

    /// A hunkless change is not a hunk. Counting it as one would report a
    /// binary as "1 hunk", and a caller comparing that against the hunk ids in
    /// `files` would find none.
    #[test]
    fn a_hunkless_change_is_counted_apart_from_hunks() {
        let files = vec![FileOutcome {
            path: "logo.png".to_string(),
            status: "modified".to_string(),
            from: None,
            selected_hunks: Vec::new(),
            unselected_hunks: Vec::new(),
            whole_file: Some(WholeFile {
                selected: true,
                reason: "binary",
            }),
        }];
        let counts = count(&files, true);
        assert_eq!(counts.hunks, 0);
        assert_eq!(counts.whole_file_changes, 1);
        assert_eq!(counts.files, 1);
        assert_eq!(phrase(&counts).text, "1 whole-file change in 1 file");

        let other = count(&files, false);
        assert_eq!(other.files, 0);
        assert_eq!(phrase(&other).text, "nothing");
    }

    /// A file with hunks on both sides is a file in both halves. Reporting it
    /// once would make the two `files` counts sum to the number of files, and
    /// a caller checking that sum would conclude a mixed file went one way.
    #[test]
    fn a_file_split_down_the_middle_is_counted_in_both_halves() {
        let files = two_hunks();
        assert_eq!(count(&files, true).files, 1);
        assert_eq!(count(&files, false).files, 1);
    }
}
