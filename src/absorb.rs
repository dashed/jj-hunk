//! `absorb`: move each hunk of a revision into the mutable ancestor that last
//! touched the lines it changes.
//!
//! The shape of the command follows `jj absorb`, at hunk granularity and with
//! the same selector language as the rest of `jj-hunk`:
//!
//! ```text
//! jj-hunk absorb [<spec>] [-r <rev>] [--dry-run] [--insertions <policy>]
//! ```
//!
//! # How a hunk is routed
//!
//! `jj file annotate` on the **parent** says which commit last touched each
//! line. A hunk's `-` lines are looked up there; if they all blame to one
//! mutable ancestor, the hunk goes to that ancestor, and otherwise it stays
//! where it is. "Stays" is the answer to every question absorb cannot answer
//! confidently -- see [`route_hunk`] for the full list of reasons, each of
//! which is printed next to the hunk it applies to.
//!
//! # Why hunks need a second identity here
//!
//! Absorb runs as a *sequence* of squashes, and every squash rewrites history.
//! A `Hunk::id` covers the three lines of context on either side of the hunk
//! (see `compute_hunk_id`), so the moment the first squash lands, the ids of
//! every hunk that has not moved yet may have changed -- their context shifted.
//! Routing by id and then squashing one target at a time would select the wrong
//! hunk, or nothing at all, from the second squash onwards.
//!
//! So absorb carries a second, deliberately coarser identity: the
//! [`fingerprint`], which is the path plus the `+`/`-` lines and nothing else.
//! That survives anything happening elsewhere in the file. Before each squash
//! the diff is re-derived from the rewritten source and the planned hunks are
//! re-matched by fingerprint, which is what turns the ids handed to `select`
//! back into current ones.
//!
//! Fingerprints are not unique: two identical one-line edits in one file share
//! one. That is handled head-on rather than assumed away -- see [`HunkGroup`].

use crate::commands::{self, BinaryMode, SpecDecision, Truncation};
use crate::diff::Hunk;
use crate::hunkset;
use crate::spec::Spec;
use anyhow::{Context, Result};
use clap::ValueEnum;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::process::Command;

/// What to do with a hunk that only inserts lines.
///
/// A pure insertion has no `-` lines, so nothing about it blames to anything:
/// the evidence absorb routes on is simply absent. The two policies differ in
/// what they are willing to do about that, and neither of them guesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum InsertionPolicy {
    /// Leave every pure insertion in the source revision.
    ///
    /// The default, because it makes absorb's refusals symmetric: a hunk whose
    /// evidence is *ambiguous* and a hunk with no evidence at all are both left
    /// alone, and the only hunks that move are the ones the annotation named.
    #[default]
    Skip,
    /// Route a pure insertion to the ancestor that owns the lines on both sides
    /// of it, when those agree.
    ///
    /// This is real evidence rather than a fallback heuristic: the insertion
    /// point sits strictly inside one commit's run of lines, so no other
    /// ancestor has a claim on it. Where the insertion lands exactly on the
    /// boundary between two commits' lines, both have an equal claim and the
    /// hunk stays. Matches the rule `jj absorb` itself uses.
    Surrounding,
}

pub struct AbsorbOptions {
    /// Hunkset expression or JSON/YAML spec naming the hunks to consider.
    /// `None` considers every hunk in the revision.
    pub spec: Option<String>,
    pub spec_file: Option<String>,
    /// Revision to absorb from (default `@`).
    pub rev: Option<String>,
    pub dry_run: bool,
    pub insertions: InsertionPolicy,
}

/// `commit_id`, then the change id, both in full and abbreviated, then the
/// first line of the description. Description last, so a tab inside it cannot
/// be mistaken for a field separator.
const COMMIT_TEMPLATE: &str = r#"commit_id ++ "\t" ++ change_id ++ "\t" ++ change_id.short(8) ++ "\t" ++ if(description, description.first_line(), "") ++ "\n""#;

/// One commit, in the terms absorb needs to talk about it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CommitRef {
    /// Full commit id. Annotation reports these, so it is the matching key.
    commit_id: String,
    /// Full change id.
    ///
    /// Every revset absorb passes to jj is spelled with this rather than a
    /// commit id. Each squash rewrites its destination *and* rebases every
    /// descendant, so by the second squash the commit ids collected up front
    /// name commits that no longer exist -- while the change ids still name the
    /// same commits they always did.
    change_id: String,
    /// Abbreviated change id, for display only.
    short: String,
    description: String,
}

impl CommitRef {
    fn label(&self) -> String {
        if self.description.is_empty() {
            format!("{} (no description)", self.short)
        } else {
            format!("{} {}", self.short, self.description)
        }
    }
}

/// What absorb knows about one hunk, kept after the `Hunk` it came from is
/// gone. Every line number here is on the **parent** side.
#[derive(Debug, Clone)]
struct HunkFacts {
    /// Abbreviated id, for display. Deliberately not used for matching: it is
    /// only valid against the diff it was read from.
    short_id: String,
    /// First line the hunk covers in the parent, 1-based. A pure insertion
    /// covers no lines, and this is the line it is inserted *before*.
    before_start: usize,
    removed: usize,
    added: usize,
}

/// The hunks in one file that share a [`fingerprint`], and the unit absorb
/// routes.
///
/// Two hunks in a file can be byte-identical once the context is dropped --
/// the same one-line edit made twice. Nothing downstream can then tell them
/// apart: the spec absorb writes names hunks by id, and re-deriving those ids
/// after a squash goes through the fingerprint, which matches both. So they are
/// routed together or not at all, and a group whose members disagree about
/// where they belong stays put with that stated as the reason. Assuming
/// uniqueness instead would silently move the wrong one of the pair.
#[derive(Debug, Clone)]
struct HunkGroup {
    path: String,
    fingerprint: String,
    /// One entry per member, in diff order. Almost always exactly one.
    members: Vec<HunkFacts>,
}

impl HunkGroup {
    fn len(&self) -> usize {
        self.members.len()
    }
}

/// Where one group ended up.
enum Routing {
    Move(CommitRef),
    /// Stays in the source revision, and why.
    Stay(String),
}

struct Plan {
    source: CommitRef,
    /// Destinations with the groups routed to them, oldest ancestor first.
    moves: Vec<(CommitRef, Vec<HunkGroup>)>,
    stays: Vec<(HunkGroup, String)>,
    /// Whole-file changes that no hunk selection can express.
    notes: Vec<String>,
    insertions: InsertionPolicy,
    dry_run: bool,
}

impl Plan {
    fn moving_hunks(&self) -> usize {
        self.moves.iter().flat_map(|(_, gs)| gs).map(HunkGroup::len).sum()
    }

    fn staying_hunks(&self) -> usize {
        self.stays.iter().map(|(group, _)| group.len()).sum()
    }
}

pub fn absorb(options: AbsorbOptions) -> Result<()> {
    let plan = build_plan(&options)?;
    print!("{}", render_plan(&plan));

    if options.dry_run {
        return Ok(());
    }
    execute(&plan)
}

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// A hunk's identity with the context deliberately left out: the path, the
/// removed lines and the added lines, and nothing else.
///
/// This is the one thing about a hunk that a squash performed elsewhere in the
/// same file cannot change. `Hunk::id` folds in three lines of context on
/// either side, which makes it a sharper selector but ties it to one diff; the
/// module docs explain why absorb needs both.
///
/// Not unique, and not treated as though it were: see [`HunkGroup`].
fn fingerprint(path: &str, hunk: &Hunk) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"absorb-fingerprint\0path\0");
    hasher.update(path.as_bytes());
    hasher.update(b"\0removed\0");
    hasher.update(hunk.removed.as_bytes());
    hasher.update(b"\0added\0");
    hasher.update(hunk.added.as_bytes());

    let mut out = String::with_capacity(64);
    for byte in hasher.finalize() {
        let _ = write!(&mut out, "{:02x}", byte);
    }
    out
}

// ---------------------------------------------------------------------------
// Planning
// ---------------------------------------------------------------------------

fn build_plan(options: &AbsorbOptions) -> Result<Plan> {
    let rev = options.rev.as_deref();

    // Rejects a revset naming several revisions, and a merge commit, with the
    // same reasoning `list` and `squash` use: a hunk only means something
    // between one revision and one parent.
    let revisions = commands::resolve_revisions(&commands::DiffTarget::rev(rev))?;
    let source = commit_ref(rev.unwrap_or("@"))?;

    let Some(parent) = revisions.before.as_deref() else {
        anyhow::bail!(
            "{} has no parent, so there is no ancestor to absorb into.",
            source.label()
        );
    };

    let ancestors = commit_refs(&format!("::{parent} & mutable()"))?;
    // Keyed by commit id, because that is what annotation reports.
    let mutable: BTreeMap<&str, (usize, &CommitRef)> = ancestors
        .iter()
        .enumerate()
        .map(|(rank, commit)| (commit.commit_id.as_str(), (rank, commit)))
        .collect();

    let spec = load_spec(options, rev)?;
    // `Mark`, not `Skip`: a binary file has no hunks to route, but the user
    // still needs telling that its change is staying behind.
    let files = commands::load_file_hunks(&commands::DiffTarget::rev(rev), BinaryMode::Mark, Truncation::NONE)?;

    let mut notes: Vec<String> = Vec::new();
    if ancestors.is_empty() {
        notes.push(format!(
            "{} has no mutable ancestors, so nothing can move",
            source.short
        ));
    }

    // Rank -> groups, so destinations come out in one fixed order however the
    // files happened to be walked.
    let mut per_target: BTreeMap<usize, Vec<HunkGroup>> = BTreeMap::new();
    let mut stays: Vec<(HunkGroup, String)> = Vec::new();
    let mut considered = 0usize;

    for file in files {
        let decision = commands::spec_decision(spec.as_ref(), &file.path);
        if matches!(decision, SpecDecision::Skip) {
            continue;
        }

        // Every file in the diff is accounted for, whether or not absorb can
        // do anything with it. A change that silently fails to appear in the
        // plan is exactly the kind a person only notices much later.
        if file.hunks.is_empty() {
            notes.push(unroutable_note(&file));
            continue;
        }
        if file.mode.is_some() {
            // `select` always restores the executable bit from the left side,
            // so a mode change cannot ride along with any hunk that does move.
            notes.push(format!(
                "{} also changes its executable bit, which is not part of any \
                 hunk and stays",
                file.path
            ));
        }

        let path = file.path.clone();
        let renamed = renamed_reason(&file);
        let is_added = file.status == "added";
        let hunks = match decision {
            SpecDecision::KeepSelection(selection) => commands::filter_hunks(file.hunks, &selection)
                .with_context(|| format!("cannot resolve the selection for {path}"))?,
            _ => file.hunks,
        };
        if hunks.is_empty() {
            continue;
        }
        considered += hunks.len();

        let history = if let Some(reason) = renamed {
            History::Absent(reason)
        } else if is_added {
            History::Absent(format!(
                "{path} is new in this revision, so none of its lines has any history"
            ))
        } else {
            annotate(parent, &path)?
        };

        for group in group_by_fingerprint(&path, &hunks) {
            match route_group(&group, &history, &mutable, options.insertions) {
                Routing::Move(target) => {
                    let rank = mutable
                        .get(target.commit_id.as_str())
                        .map(|(rank, _)| *rank)
                        .expect("a routed target is always one of the mutable ancestors");
                    per_target.entry(rank).or_default().push(group);
                }
                Routing::Stay(reason) => stays.push((group, reason)),
            }
        }
    }

    if considered == 0 {
        // Anything the revision does change but absorb cannot route belongs in
        // the refusal, or "nothing to absorb" reads as "nothing is there".
        let unroutable = if notes.is_empty() {
            String::new()
        } else {
            format!("\n  {}", notes.join("\n  "))
        };
        if spec.is_some() {
            anyhow::bail!(
                "the selection matched no hunks in {}, so there is nothing to \
                 absorb.{unroutable}\n\
                 Check it with `jj-hunk list --spec ...`.",
                source.short
            );
        }
        anyhow::bail!(
            "{} changes no lines that absorb can route, so there is nothing to \
             absorb.{unroutable}",
            source.label()
        );
    }

    // Oldest ancestor first. The order has to be fixed for the plan to be
    // reproducible at all, and this is the direction history flows: by the time
    // a destination is written its own ancestors are already final, so a
    // conflict surfaces at the oldest commit that could have caused it.
    let moves = per_target
        .into_iter()
        .rev()
        .map(|(rank, groups)| (ancestors[rank].clone(), groups))
        .collect();

    Ok(Plan {
        source,
        moves,
        stays,
        notes,
        insertions: options.insertions,
        dry_run: options.dry_run,
    })
}

/// Why every hunk in a renamed (or copied) file stays where it is.
///
/// Absorb moves lines; a rename is a whole-file change and no hunk selection
/// can express it. It does not simply sit still either: `select` is handed the
/// old path alongside the new one for any file the spec names, so the rename is
/// performed by the *first* squash that carries any hunk out of the file. That
/// puts the new name in an ancestor while every commit between it and the
/// source still refers to the old one, and the rebase that follows conflicts
/// them -- which is how a run that reported "2 moving, 0 staying" ended with a
/// conflicted ancestor and nothing moved. Across two destinations it cannot be
/// made to work at all: a file is renamed once, and the second squash would
/// find the old path gone.
///
/// So the file stays whole, and says so. Committing the rename on its own first
/// leaves a plain modification behind, which absorb routes normally.
fn renamed_reason(file: &commands::FileHunks) -> Option<String> {
    let rename = file.rename.as_ref().filter(|r| r.from != file.path)?;
    Some(format!(
        "{} was renamed from {}, and a rename is a whole-file change that would \
         ride into the ancestor with the first hunk that moved -- commit the \
         rename on its own first, then absorb",
        file.path, rename.from
    ))
}

/// Why a file that is part of the change contributes no hunks to route.
fn unroutable_note(file: &commands::FileHunks) -> String {
    if file.is_binary {
        format!(
            "{} is binary, so its change cannot be split into hunks and stays",
            file.path
        )
    } else if file.mode.is_some() {
        format!(
            "{} only changes its executable bit, which is not part of any hunk \
             and stays",
            file.path
        )
    } else if let Some(rename) = file.rename.as_ref().filter(|r| r.from != file.path) {
        // A rename with no edits alongside it. Named for what it is rather than
        // left to the catch-all below, which would report the one change this
        // file does have as "no hunks".
        format!(
            "{} was only renamed from {}, which is not part of any hunk and stays",
            file.path, rename.from
        )
    } else {
        // A symlink, most likely: `jj file show` yields no content for one, so
        // both sides of the diff come out empty and no hunk is produced.
        format!(
            "{} changed in a way that produces no hunks, so it stays whole",
            file.path
        )
    }
}

/// Group one file's hunks by fingerprint, in first-appearance order.
fn group_by_fingerprint(path: &str, hunks: &[Hunk]) -> Vec<HunkGroup> {
    let mut order: Vec<String> = Vec::new();
    let mut members: BTreeMap<String, Vec<HunkFacts>> = BTreeMap::new();

    for hunk in hunks {
        let print = fingerprint(path, hunk);
        let entry = members.entry(print.clone()).or_insert_with(|| {
            order.push(print.clone());
            Vec::new()
        });
        entry.push(HunkFacts {
            short_id: hunk.short_id.clone(),
            before_start: hunk.before_range.start,
            removed: hunk.before_range.length,
            added: hunk.after_range.length,
        });
    }

    order
        .into_iter()
        .map(|print| HunkGroup {
            path: path.to_string(),
            members: members.remove(&print).unwrap_or_default(),
            fingerprint: print,
        })
        .collect()
}

fn route_group(
    group: &HunkGroup,
    history: &History,
    mutable: &BTreeMap<&str, (usize, &CommitRef)>,
    policy: InsertionPolicy,
) -> Routing {
    let mut decided: Option<CommitRef> = None;

    for facts in &group.members {
        match route_hunk(facts, history, mutable, policy) {
            Routing::Stay(reason) => return Routing::Stay(reason),
            Routing::Move(target) => match &decided {
                None => decided = Some(target),
                Some(first) if *first == target => {}
                Some(first) => {
                    return Routing::Stay(format!(
                        "identical to another hunk in {}, and the two belong to \
                         different ancestors ({} and {}), which no selection can \
                         tell apart",
                        group.path, first.short, target.short
                    ))
                }
            },
        }
    }

    match decided {
        Some(target) => Routing::Move(target),
        // A group is always built from at least one hunk, so this is
        // unreachable; refusing is still the right answer if it ever is not.
        None => Routing::Stay("it contains no hunks".to_string()),
    }
}

fn route_hunk(
    facts: &HunkFacts,
    history: &History,
    mutable: &BTreeMap<&str, (usize, &CommitRef)>,
    policy: InsertionPolicy,
) -> Routing {
    let lines = match history {
        History::Lines(lines) => lines.as_slice(),
        History::Absent(reason) => return Routing::Stay(reason.clone()),
    };

    let owner = if facts.removed > 0 {
        // 1-based lines [start, start + removed) -> 0-based slice. The whole
        // range has to be there: routing on the prefix that happened to exist
        // would be deciding on part of the evidence.
        let from = facts.before_start.saturating_sub(1);
        let Some(covered) = lines
            .get(from..from + facts.removed)
            .filter(|slice| !slice.is_empty())
        else {
            return Routing::Stay(format!(
                "it covers parent lines {}-{}, which is past the {} the parent \
                 has -- refusing to guess",
                facts.before_start,
                facts.before_start + facts.removed - 1,
                lines.len()
            ));
        };

        // Sorted before deduplicating, so the set is the same however the
        // lines were ordered, and so the names in the message below are too.
        let mut owners: Vec<&str> = covered.iter().map(String::as_str).collect();
        owners.sort_unstable();
        owners.dedup();

        if owners.len() > 1 {
            let named: Vec<String> = owners.iter().map(|id| label(id, mutable)).collect();
            return Routing::Stay(format!(
                "the {} lines it changes were last touched by {} commits ({})",
                facts.removed,
                owners.len(),
                named.join(", ")
            ));
        }
        owners[0].to_string()
    } else {
        match insertion_owner(facts, lines, policy, mutable) {
            Ok(owner) => owner,
            Err(reason) => return Routing::Stay(reason),
        }
    };

    match mutable.get(owner.as_str()) {
        Some((_, target)) => Routing::Move((*target).clone()),
        // Annotating the parent can only name the parent's ancestors, so a
        // commit missing from the mutable set is one absorb is not allowed to
        // rewrite.
        None => Routing::Stay(format!(
            "its lines were last changed by {}, which is immutable",
            label(&owner, mutable)
        )),
    }
}

/// Which commit owns the point a pure insertion goes at, under `policy`.
fn insertion_owner(
    facts: &HunkFacts,
    lines: &[String],
    policy: InsertionPolicy,
    mutable: &BTreeMap<&str, (usize, &CommitRef)>,
) -> Result<String, String> {
    if policy == InsertionPolicy::Skip {
        return Err(
            "it only adds lines, so no line of it blames to an ancestor \
             (--insertions=surrounding routes these by their neighbours)"
                .to_string(),
        );
    }

    // The insertion goes before 1-based line `before_start`, so the line above
    // it is `before_start - 1` and the line below it is `before_start`.
    let above = facts
        .before_start
        .checked_sub(2)
        .and_then(|index| lines.get(index));
    let below = lines.get(facts.before_start.saturating_sub(1));

    match (above, below) {
        (Some(a), Some(b)) if a != b => Err(format!(
            "it is inserted exactly on the boundary between lines owned by {} and {}",
            label(a, mutable),
            label(b, mutable)
        )),
        (Some(owner), _) | (None, Some(owner)) => Ok(owner.clone()),
        (None, None) => {
            Err("it is inserted into a file that is empty in the parent".to_string())
        }
    }
}

/// Name a commit for a person to read.
///
/// Change ids are what a jj user navigates by, and absorb has one for every
/// mutable ancestor because that is the set it queried. It has no change id for
/// anything outside that set -- annotation reports commit ids, and resolving
/// the rest would mean walking the whole ancestry for the sake of a message.
/// So those are labelled as commit ids *and said to be*, rather than printed
/// bare next to change ids and left looking like the same kind of thing.
fn label(commit_id: &str, mutable: &BTreeMap<&str, (usize, &CommitRef)>) -> String {
    match mutable.get(commit_id) {
        Some((_, commit)) => commit.short.clone(),
        None => format!("commit {}", commit_id.chars().take(8).collect::<String>()),
    }
}

fn load_spec(options: &AbsorbOptions, rev: Option<&str>) -> Result<Option<Spec>> {
    let Some(raw) = commands::resolve_optional_spec(options.spec.as_deref(), options.spec_file.as_deref())?
    else {
        return Ok(None);
    };

    if hunkset::is_hunkset(&raw) {
        // Evaluated against the same untruncated view absorb routes over, so
        // the ids it names are the ids the hunks actually have.
        let json = commands::evaluate_hunkset(&raw, &commands::DiffTarget::rev(rev), Truncation::NONE)?;
        return Ok(Some(Spec::from_str(&json)?));
    }
    Ok(Some(Spec::from_str(&raw)?))
}

// ---------------------------------------------------------------------------
// jj queries
// ---------------------------------------------------------------------------

fn commit_refs(revset: &str) -> Result<Vec<CommitRef>> {
    let output = Command::new("jj")
        .args(["log", "--no-graph", "-r", revset, "-T", COMMIT_TEMPLATE])
        .output()
        .context("Failed to run jj log")?;

    if !output.status.success() {
        anyhow::bail!(
            "failed to resolve revset `{}`: {}",
            revset,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let mut fields = line.splitn(4, '\t');
            Some(CommitRef {
                commit_id: fields.next()?.to_string(),
                change_id: fields.next()?.to_string(),
                short: fields.next()?.to_string(),
                description: fields.next().unwrap_or("").trim().to_string(),
            })
        })
        .collect())
}

fn commit_ref(revset: &str) -> Result<CommitRef> {
    let mut refs = commit_refs(revset)?;
    match refs.len() {
        1 => Ok(refs.remove(0)),
        n => anyhow::bail!("revset `{revset}` resolved to {n} revisions, but absorb needs one"),
    }
}

/// What the parent knows about where a file's lines came from.
enum History {
    /// The commit that last touched each line, 1-based by position.
    Lines(Vec<String>),
    /// There is no such history, and why. Every hunk in the file stays, with
    /// this printed as its reason.
    Absent(String),
}

/// Annotate `path` at `rev`.
///
/// A refusal from jj -- the path is a symlink, or a conflict, or otherwise not
/// a regular file -- is one file's problem, not the run's: those hunks stay and
/// the rest of the revision is still absorbed. Only a failure to *run* jj at
/// all is fatal, because then nothing below can be trusted either.
fn annotate(rev: &str, path: &str) -> Result<History> {
    let output = Command::new("jj")
        .args([
            "file",
            "annotate",
            "-r",
            rev,
            "-T",
            r#"commit.commit_id() ++ "\n""#,
            "--",
            path,
        ])
        .output()
        .with_context(|| format!("Failed to run jj file annotate for {path}"))?;

    if !output.status.success() {
        return Ok(History::Absent(format!(
            "{path} could not be annotated, so its lines have no traceable \
             history ({})",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    Ok(History::Lines(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_string)
            .collect(),
    ))
}

fn current_operation_id() -> Result<String> {
    let output = Command::new("jj")
        .args(["op", "log", "--no-graph", "--limit", "1", "-T", r#"id.short() ++ "\n""#])
        .output()
        .context("Failed to run jj op log")?;

    if !output.status.success() {
        anyhow::bail!(
            "failed to read the operation log: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Commits in `revset` that came out of a rewrite with conflicts.
fn conflicted(revset: &str) -> Result<Vec<String>> {
    let output = Command::new("jj")
        .args([
            "log",
            "--no-graph",
            "-r",
            revset,
            "-T",
            r#"if(conflict, change_id.short(8) ++ "\n", "")"#,
        ])
        .output()
        .context("Failed to run jj log")?;

    if !output.status.success() {
        anyhow::bail!(
            "failed to check for conflicts: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct SquashSpec {
    files: BTreeMap<String, SquashFile>,
    default: &'static str,
}

#[derive(Serialize)]
struct SquashFile {
    ids: Vec<String>,
}

fn execute(plan: &Plan) -> Result<()> {
    if plan.moves.is_empty() {
        println!("Nothing to absorb: every hunk stays in {}.", plan.source.short);
        return Ok(());
    }

    // Captured before the first squash, and printed whatever happens: the
    // command performs one jj operation per destination, so `jj undo` alone
    // would only take back the last of them.
    let undo_point = current_operation_id()?;

    let mut moved: Vec<&CommitRef> = Vec::new();
    for (index, (target, groups)) in plan.moves.iter().enumerate() {
        let outcome = squash_into(&plan.source, target, groups).and_then(|()| {
            // `<target>::` is exactly what that squash rewrote: the destination
            // itself, the source, and every descendant jj rebased on the way.
            // Bounded, unlike walking the whole ancestry for each squash.
            let conflicts = conflicted(&format!("{}::", target.change_id))?;
            if conflicts.is_empty() {
                return Ok(());
            }
            anyhow::bail!(
                "moving into {} left {} conflicted ({}), and absorb will not \
                 squash further hunks into a conflicted history",
                target.short,
                if conflicts.len() == 1 { "a commit" } else { "commits" },
                conflicts.join(", ")
            )
        });

        if let Err(err) = outcome {
            return Err(err.context(interrupted_report(plan, &moved, index, &undo_point)));
        }
        moved.push(target);
    }

    println!();
    println!(
        "Absorbed {} hunks into {} revisions:",
        plan.moving_hunks(),
        moved.len()
    );
    for target in &moved {
        println!("  {}", target.label());
    }
    if plan.staying_hunks() > 0 {
        println!(
            "{} hunks stayed in {}.",
            plan.staying_hunks(),
            plan.source.short
        );
    }
    println!("Undo all of it with: jj op restore {undo_point}");
    Ok(())
}

/// What moved and what did not, for a run that stopped part way.
fn interrupted_report(
    plan: &Plan,
    moved: &[&CommitRef],
    failed_at: usize,
    undo_point: &str,
) -> String {
    let mut report = String::new();
    let _ = writeln!(report, "absorb stopped part way, and history is half-moved.");

    if moved.is_empty() {
        let _ = writeln!(report, "  moved:     nothing");
    } else {
        for (position, target) in moved.iter().enumerate() {
            let field = if position == 0 { "  moved:    " } else { "            " };
            let _ = writeln!(report, "{field} {}", target.label());
        }
    }

    for (position, (target, groups)) in plan.moves.iter().enumerate().skip(failed_at) {
        let field = if position == failed_at { "  not moved:" } else { "            " };
        let hunks: usize = groups.iter().map(HunkGroup::len).sum();
        let _ = writeln!(report, "{field} {} ({hunks} hunks)", target.label());
    }

    let _ = writeln!(
        report,
        "Everything not listed as moved is still in {}.\n\
         Undo all of it with: jj op restore {undo_point}",
        plan.source.short
    );
    report
}

/// Move one destination's hunks, re-deriving their ids from the current diff.
fn squash_into(source: &CommitRef, target: &CommitRef, groups: &[HunkGroup]) -> Result<()> {
    let files = commands::load_file_hunks(
        &commands::DiffTarget::rev(Some(&source.change_id)),
        BinaryMode::Mark,
        Truncation::NONE,
    )?;

    let mut spec_files: BTreeMap<String, SquashFile> = BTreeMap::new();
    for group in groups {
        let file = files
            .iter()
            .find(|file| file.path == group.path)
            .with_context(|| {
                format!(
                    "{} is no longer part of {}, so the planned hunks cannot be found",
                    group.path, source.short
                )
            })?;

        let matched: Vec<&Hunk> = file
            .hunks
            .iter()
            .filter(|hunk| fingerprint(&file.path, hunk) == group.fingerprint)
            .collect();

        // The count is the check that the re-match found the same hunks the
        // plan was built from. Squashing a different number of hunks than
        // planned would move something nobody asked to move.
        if matched.len() != group.members.len() {
            anyhow::bail!(
                "{}: expected {} hunk(s) matching the planned change but found {}. \
                 The revision changed under absorb; re-run it.",
                group.path,
                group.members.len(),
                matched.len()
            );
        }

        spec_files
            .entry(group.path.clone())
            .or_insert_with(|| SquashFile { ids: Vec::new() })
            .ids
            .extend(matched.into_iter().map(|hunk| hunk.id.clone()));
    }

    let spec = serde_json::to_string(&SquashSpec {
        files: spec_files,
        default: "reset",
    })
    .context("failed to serialize the absorb selection")?;

    let args = [
        "squash",
        "-i",
        "--tool=jj-hunk",
        // The source keeps its identity even when every hunk leaves it. An
        // abandoned working-copy commit is replaced by a *new* change, and the
        // remaining squashes all name the source by change id.
        "--keep-emptied",
        "--from",
        &source.change_id,
        "--into",
        &target.change_id,
    ];

    commands::run_jj_with_selection(&args, Some(&spec), None, Some(&source.change_id), false)
        .with_context(|| format!("failed to squash into {}", target.label()))
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// One hunk's line in the plan: where it is, and how big it is.
///
/// Line numbers are absolute and always on the parent side -- the same frame
/// the routing decision was made in. A pure insertion is shown at the line it
/// goes before, which is why the counts are printed rather than a range.
fn hunk_location(group: &HunkGroup, facts: &HunkFacts) -> String {
    format!(
        "{}:{}  -{} +{}  {}",
        group.path, facts.before_start, facts.removed, facts.added, facts.short_id
    )
}

fn render_plan(plan: &Plan) -> String {
    let mut out = String::new();
    let moving = plan.moving_hunks();
    let staying = plan.staying_hunks();

    let _ = writeln!(out, "absorb from {}", plan.source.label());
    let _ = writeln!(
        out,
        "  {} hunks: {moving} moving into {} {}, {staying} staying",
        moving + staying,
        plan.moves.len(),
        if plan.moves.len() == 1 { "ancestor" } else { "ancestors" },
    );

    for (target, groups) in &plan.moves {
        let _ = writeln!(out);
        let _ = writeln!(out, "move into {}", target.label());
        for group in groups {
            for facts in &group.members {
                let _ = writeln!(out, "  {}", hunk_location(group, facts));
            }
        }
    }

    if !plan.stays.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "stay in {}", plan.source.label());
        for (group, reason) in &plan.stays {
            for facts in &group.members {
                let _ = writeln!(out, "  {}", hunk_location(group, facts));
                let _ = writeln!(out, "    {reason}");
            }
        }
    }

    for note in &plan.notes {
        let _ = writeln!(out, "\nnote: {note}");
    }

    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "line numbers are in the parent of {}; an insertion is listed at the line it goes before",
        plan.source.short
    );
    if plan.insertions == InsertionPolicy::Surrounding {
        let _ = writeln!(
            out,
            "--insertions=surrounding: insertions are routed by the lines around them"
        );
    }
    if plan.dry_run {
        let _ = writeln!(out, "--dry-run: nothing was changed");
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::get_hunks;

    fn commit(commit_id: &str, short: &str) -> CommitRef {
        CommitRef {
            commit_id: commit_id.to_string(),
            change_id: format!("{commit_id}-change"),
            short: short.to_string(),
            description: format!("commit {short}"),
        }
    }

    /// Owners as annotation reports them: one entry per line of the parent.
    fn owners(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    fn mutable_map(commits: &[CommitRef]) -> BTreeMap<&str, (usize, &CommitRef)> {
        commits
            .iter()
            .enumerate()
            .map(|(rank, c)| (c.commit_id.as_str(), (rank, c)))
            .collect()
    }

    fn route(
        path: &str,
        before: &str,
        after: &str,
        annotation: &[String],
        commits: &[CommitRef],
        policy: InsertionPolicy,
    ) -> Vec<Routing> {
        let hunks = get_hunks(path, before, after);
        let map = mutable_map(commits);
        group_by_fingerprint(path, &hunks)
            .into_iter()
            .map(|group| route_group(&group, &History::Lines(annotation.to_vec()), &map, policy))
            .collect()
    }

    fn moved_to(routing: &Routing) -> Option<&str> {
        match routing {
            Routing::Move(target) => Some(target.short.as_str()),
            Routing::Stay(_) => None,
        }
    }

    fn stay_reason(routing: &Routing) -> &str {
        match routing {
            Routing::Stay(reason) => reason,
            Routing::Move(target) => panic!("expected a stay, got a move to {}", target.short),
        }
    }

    /// The fingerprint must not move when only the surrounding lines do --
    /// that is the entire reason it exists alongside `Hunk::id`.
    #[test]
    fn a_fingerprint_ignores_context_where_an_id_does_not() {
        let near = get_hunks("f.txt", "a\nb\nc\nTARGET\ne\n", "a\nb\nc\nCHANGED\ne\n");
        let far = get_hunks("f.txt", "z\ny\nx\nTARGET\ne\n", "z\ny\nx\nCHANGED\ne\n");

        assert_eq!(near.len(), 1);
        assert_eq!(far.len(), 1);
        assert_ne!(near[0].id, far[0].id, "ids fold in the context");
        assert_eq!(
            fingerprint("f.txt", &near[0]),
            fingerprint("f.txt", &far[0]),
            "fingerprints must not"
        );
    }

    #[test]
    fn the_same_change_to_two_files_gets_two_fingerprints() {
        let hunks = get_hunks("one.rs", "a\nb\n", "a\nB\n");
        let other = get_hunks("two.rs", "a\nb\n", "a\nB\n");
        assert_ne!(
            fingerprint("one.rs", &hunks[0]),
            fingerprint("two.rs", &other[0])
        );
    }

    #[test]
    fn a_hunk_goes_to_the_commit_that_owns_its_lines() {
        let commits = [commit("aaa", "older"), commit("bbb", "newer")];
        let annotation = owners(&["aaa", "bbb", "aaa"]);

        let routings = route(
            "f.txt",
            "one\ntwo\nthree\n",
            "one\nTWO\nthree\n",
            &annotation,
            &commits,
            InsertionPolicy::Skip,
        );
        assert_eq!(routings.len(), 1);
        assert_eq!(moved_to(&routings[0]), Some("newer"));
    }

    #[test]
    fn a_hunk_spanning_two_owners_stays() {
        let commits = [commit("aaa", "older"), commit("bbb", "newer")];
        let annotation = owners(&["aaa", "aaa", "bbb", "aaa"]);

        let routings = route(
            "f.txt",
            "one\ntwo\nthree\nfour\n",
            "one\nTWO\nTHREE\nfour\n",
            &annotation,
            &commits,
            InsertionPolicy::Skip,
        );
        assert_eq!(routings.len(), 1);
        let reason = stay_reason(&routings[0]);
        assert!(reason.contains("2 commits"), "{reason}");
        assert!(reason.contains("older") && reason.contains("newer"), "{reason}");
    }

    #[test]
    fn a_hunk_whose_lines_are_immutable_stays() {
        // `aaa` is not in the mutable map at all.
        let commits = [commit("bbb", "newer")];
        let annotation = owners(&["aaa", "aaa"]);

        let routings = route(
            "f.txt",
            "one\ntwo\n",
            "one\nTWO\n",
            &annotation,
            &commits,
            InsertionPolicy::Skip,
        );
        assert!(stay_reason(&routings[0]).contains("immutable"));
    }

    #[test]
    fn a_pure_insertion_stays_by_default() {
        let commits = [commit("aaa", "older")];
        let annotation = owners(&["aaa", "aaa"]);

        let routings = route(
            "f.txt",
            "one\ntwo\n",
            "one\ninserted\ntwo\n",
            &annotation,
            &commits,
            InsertionPolicy::Skip,
        );
        let reason = stay_reason(&routings[0]);
        assert!(reason.contains("only adds lines"), "{reason}");
        assert!(reason.contains("--insertions=surrounding"), "{reason}");
    }

    #[test]
    fn surrounding_routes_an_insertion_inside_one_owners_lines() {
        let commits = [commit("aaa", "older"), commit("bbb", "newer")];
        let annotation = owners(&["bbb", "aaa", "aaa", "bbb"]);

        // Inserted between lines 2 and 3, both owned by `aaa`.
        let routings = route(
            "f.txt",
            "one\ntwo\nthree\nfour\n",
            "one\ntwo\nNEW\nthree\nfour\n",
            &annotation,
            &commits,
            InsertionPolicy::Surrounding,
        );
        assert_eq!(moved_to(&routings[0]), Some("older"));
    }

    #[test]
    fn surrounding_refuses_an_insertion_on_a_boundary() {
        let commits = [commit("aaa", "older"), commit("bbb", "newer")];
        let annotation = owners(&["aaa", "bbb"]);

        // Inserted between line 1 (`aaa`) and line 2 (`bbb`).
        let routings = route(
            "f.txt",
            "one\ntwo\n",
            "one\nNEW\ntwo\n",
            &annotation,
            &commits,
            InsertionPolicy::Surrounding,
        );
        let reason = stay_reason(&routings[0]);
        assert!(reason.contains("boundary"), "{reason}");
    }

    #[test]
    fn surrounding_routes_an_insertion_at_either_end_of_the_file() {
        let commits = [commit("aaa", "older")];
        let annotation = owners(&["aaa", "aaa"]);

        let first = route(
            "f.txt",
            "one\ntwo\n",
            "NEW\none\ntwo\n",
            &annotation,
            &commits,
            InsertionPolicy::Surrounding,
        );
        assert_eq!(moved_to(&first[0]), Some("older"));

        let last = route(
            "f.txt",
            "one\ntwo\n",
            "one\ntwo\nNEW\n",
            &annotation,
            &commits,
            InsertionPolicy::Surrounding,
        );
        assert_eq!(moved_to(&last[0]), Some("older"));
    }

    /// Two byte-identical edits in one file share a fingerprint, so nothing
    /// downstream can select one without the other.
    #[test]
    fn identical_hunks_bound_for_one_ancestor_travel_together() {
        let commits = [commit("aaa", "older"), commit("bbb", "newer")];
        // Both `DUP` lines (2 and 5) belong to `aaa`.
        let annotation = owners(&["bbb", "aaa", "bbb", "bbb", "aaa", "bbb"]);

        let routings = route(
            "f.txt",
            "p\nDUP\nq\nr\nDUP\ns\n",
            "p\nEDIT\nq\nr\nEDIT\ns\n",
            &annotation,
            &commits,
            InsertionPolicy::Skip,
        );

        assert_eq!(routings.len(), 1, "the two identical hunks form one group");
        assert_eq!(moved_to(&routings[0]), Some("older"));
    }

    #[test]
    fn identical_hunks_bound_for_different_ancestors_stay() {
        let commits = [commit("aaa", "older"), commit("bbb", "newer")];
        let annotation = owners(&["aaa", "aaa", "aaa", "bbb", "bbb", "bbb"]);

        let routings = route(
            "f.txt",
            "p\nDUP\nq\nr\nDUP\ns\n",
            "p\nEDIT\nq\nr\nEDIT\ns\n",
            &annotation,
            &commits,
            InsertionPolicy::Skip,
        );

        assert_eq!(routings.len(), 1);
        let reason = stay_reason(&routings[0]);
        assert!(reason.contains("identical to another hunk"), "{reason}");
        assert!(reason.contains("older") && reason.contains("newer"), "{reason}");
    }

    #[test]
    fn a_hunk_in_a_file_with_no_history_stays() {
        let hunks = get_hunks("new.txt", "", "one\ntwo\n");
        let commits = [commit("aaa", "older")];
        let map = mutable_map(&commits);

        let groups = group_by_fingerprint("new.txt", &hunks);
        let absent = History::Absent("new.txt is new in this revision".to_string());
        let reason = match route_group(&groups[0], &absent, &map, InsertionPolicy::Surrounding) {
            Routing::Stay(reason) => reason,
            Routing::Move(_) => panic!("a file with no parent version must not route"),
        };
        assert!(reason.contains("new in this revision"), "{reason}");
    }

    fn sample_group(path: &str, before_start: usize, removed: usize, added: usize) -> HunkGroup {
        HunkGroup {
            path: path.to_string(),
            fingerprint: format!("{path}-{before_start}"),
            members: vec![HunkFacts {
                short_id: "hunk-0badcafe".to_string(),
                before_start,
                removed,
                added,
            }],
        }
    }

    fn sample_plan() -> Plan {
        Plan {
            source: CommitRef {
                commit_id: "sss".to_string(),
                change_id: "sss-change".to_string(),
                short: "srcsrcsr".to_string(),
                description: "work in progress".to_string(),
            },
            moves: vec![
                (commit("aaa", "olderold"), vec![sample_group("f.txt", 3, 1, 1)]),
                (commit("bbb", "newernew"), vec![sample_group("f.txt", 8, 2, 3)]),
            ],
            stays: vec![(sample_group("g.txt", 1, 0, 4), "because".to_string())],
            notes: vec!["img.png is binary".to_string()],
            insertions: InsertionPolicy::Skip,
            dry_run: true,
        }
    }

    /// The plan is the product of `--dry-run`, so its shape is worth pinning:
    /// counts, destination order, and one line per hunk with its parent-side
    /// position.
    #[test]
    fn a_plan_reads_as_counts_then_destinations_then_leftovers() {
        let rendered = render_plan(&sample_plan());

        assert_eq!(
            rendered,
            "absorb from srcsrcsr work in progress\n\
             \x20 3 hunks: 2 moving into 2 ancestors, 1 staying\n\
             \n\
             move into olderold commit olderold\n\
             \x20 f.txt:3  -1 +1  hunk-0badcafe\n\
             \n\
             move into newernew commit newernew\n\
             \x20 f.txt:8  -2 +3  hunk-0badcafe\n\
             \n\
             stay in srcsrcsr work in progress\n\
             \x20 g.txt:1  -0 +4  hunk-0badcafe\n\
             \x20   because\n\
             \n\
             note: img.png is binary\n\
             \n\
             line numbers are in the parent of srcsrcsr; an insertion is listed \
             at the line it goes before\n\
             --dry-run: nothing was changed\n"
        );
    }

    /// Rendering has to be a pure function of the plan, or two identical runs
    /// can print two different things.
    #[test]
    fn rendering_the_same_plan_twice_gives_the_same_text() {
        assert_eq!(render_plan(&sample_plan()), render_plan(&sample_plan()));
    }

    /// A run that stops half way has to say which side of the line each
    /// destination fell on. "Some of it worked" with no detail is the one
    /// outcome a person cannot recover from by reading.
    #[test]
    fn an_interrupted_run_names_what_moved_and_what_did_not() {
        let plan = sample_plan();
        let moved: Vec<&CommitRef> = vec![&plan.moves[0].0];
        let report = interrupted_report(&plan, &moved, 1, "0123abcd");

        assert!(report.contains("stopped part way"), "{report}");
        assert!(report.contains("  moved:     olderold"), "{report}");
        assert!(report.contains("  not moved: newernew"), "{report}");
        assert!(report.contains("(1 hunks)"), "{report}");
        assert!(report.contains("jj op restore 0123abcd"), "{report}");
    }

    #[test]
    fn an_interrupted_run_that_moved_nothing_says_so() {
        let plan = sample_plan();
        let report = interrupted_report(&plan, &[], 0, "0123abcd");

        assert!(report.contains("moved:     nothing"), "{report}");
        assert!(report.contains("not moved: olderold"), "{report}");
        assert!(report.contains("newernew"), "{report}");
    }
}
