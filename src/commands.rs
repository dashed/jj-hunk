use crate::diff::{self, apply_selected_hunks, get_hunks, Hunk, HunkSelection};
use crate::errors::{self, CodedError};
use crate::glob::glob_match;
use crate::hunkset::{self, EnrichedHunk};
#[cfg(feature = "semantic")]
use crate::semantic;
use crate::spec::{Action, DefaultAction, FileSpec, HunkSelector, Spec};
use anyhow::{Context, Result};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

const JJ_HUNK_TOOL_ARG: &str = "--tool=jj-hunk";
const JJ_HUNK_PROGRAM_KEY: &str = "merge-tools.jj-hunk.program";
const JJ_HUNK_EDIT_ARGS_KEY: &str = "merge-tools.jj-hunk.edit-args";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum ListFormat {
    #[default]
    Json,
    Yaml,
    Text,
    Diff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum ListGrouping {
    #[default]
    None,
    Directory,
    Extension,
    Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum BinaryMode {
    Skip,
    #[default]
    Mark,
    Include,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListMode {
    #[default]
    Full,
    Files,
    SpecTemplate,
}

/// Caps applied to a file's contents *before* it is diffed, so a very large
/// file can be listed without materialising its whole diff.
///
/// Deliberately not plumbed into the paths that build or consume a spec. A
/// hunk id is a hash of the text the hunk was computed from, so ids taken from
/// a truncated file do not exist in the real diff; a spec built from them would
/// select nothing. `list` is the only consumer, and `build_spec_template`
/// refuses outright for any file this actually cut.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Truncation {
    pub max_bytes: Option<usize>,
    pub max_lines: Option<usize>,
}

impl Truncation {
    /// Read the file whole.
    pub const NONE: Self = Self {
        max_bytes: None,
        max_lines: None,
    };
}

/// The two trees a diff is taken between.
///
/// `Rev` is the shape every revision-editing command wants: `jj diff -r REV`,
/// i.e. parent(REV) -> REV, or the working copy against its parent when
/// `None`.
///
/// `FromTo` is `jj diff --from A --to B`. It exists because `jj restore` hands
/// its diff editor the *destination* on the left and the *source* on the
/// right -- the reverse of `jj diff -r`. A hunk id is a hash of the text it was
/// computed from, so a spec built from the forward diff names nothing at all in
/// that view; building it from the reversed one is what makes ids resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffTarget {
    Rev(Option<String>),
    FromTo { from: String, to: String },
}

impl DiffTarget {
    pub fn rev(rev: Option<&str>) -> Self {
        DiffTarget::Rev(rev.map(str::to_string))
    }

    pub fn from_to(from: &str, to: &str) -> Self {
        DiffTarget::FromTo {
            from: from.to_string(),
            to: to.to_string(),
        }
    }

    /// The `jj-hunk list` invocation that shows exactly this diff, so an error
    /// about an id that did not resolve can name the listing it came from.
    pub(crate) fn listing_command(&self) -> String {
        match self {
            DiffTarget::Rev(None) => "jj-hunk list".to_string(),
            DiffTarget::Rev(Some(rev)) => format!("jj-hunk list -r {rev}"),
            DiffTarget::FromTo { from, to } => format!("jj-hunk list --from {from} --to {to}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ListOptions {
    pub rev: Option<String>,
    /// Left side of an explicit two-revision diff. Mutually exclusive with
    /// `rev`.
    pub from: Option<String>,
    /// Right side of an explicit two-revision diff. Mutually exclusive with
    /// `rev`.
    pub to: Option<String>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub group: ListGrouping,
    pub format: ListFormat,
    pub mode: ListMode,
    pub spec: Option<String>,
    pub spec_file: Option<String>,
    pub binary: BinaryMode,
    pub truncation: Truncation,
}

impl Default for ListOptions {
    fn default() -> Self {
        Self {
            rev: None,
            from: None,
            to: None,
            include: Vec::new(),
            exclude: Vec::new(),
            group: ListGrouping::default(),
            format: ListFormat::default(),
            mode: ListMode::default(),
            spec: None,
            spec_file: None,
            binary: BinaryMode::default(),
            truncation: Truncation::NONE,
        }
    }
}

impl From<Option<&str>> for ListOptions {
    fn from(rev: Option<&str>) -> Self {
        Self {
            rev: rev.map(str::to_string),
            ..Self::default()
        }
    }
}

#[derive(Debug, Serialize)]
struct ListOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    files: Option<Vec<FileEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    groups: Option<Vec<ListGroup>>,
}

#[derive(Debug, Serialize)]
struct ListGroup {
    name: String,
    files: Vec<FileEntry>,
}

#[derive(Debug, Serialize)]
struct FileEntry {
    path: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    rename: Option<RenameInfo>,
    hunks: Vec<Hunk>,
    #[serde(skip_serializing_if = "Option::is_none")]
    binary: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<ModeChange>,
    /// Set when either side of this entry is a symlink, so a reader can see
    /// why a change that jj calls modified carries no hunks.
    #[serde(skip_serializing_if = "Option::is_none")]
    symlink: Option<bool>,
    /// Set when `--max-bytes`/`--max-lines` actually cut this file, so the
    /// listed hunks describe only its opening slice.
    #[serde(skip_serializing_if = "Option::is_none")]
    truncated: Option<bool>,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct RenameInfo {
    pub(crate) from: String,
    pub(crate) to: String,
}

#[derive(Debug, Serialize)]
struct ListSummaryOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    files: Option<Vec<FileSummary>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    groups: Option<Vec<ListSummaryGroup>>,
}

#[derive(Debug, Serialize)]
struct ListSummaryGroup {
    name: String,
    files: Vec<FileSummary>,
}

#[derive(Debug, Serialize)]
struct FileSummary {
    path: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    rename: Option<RenameInfo>,
    hunk_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    binary: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<ModeChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    symlink: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    truncated: Option<bool>,
}

#[derive(Debug, Serialize)]
struct SpecTemplateOutput {
    files: BTreeMap<String, SpecTemplateEntry>,
    default: String,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum SpecTemplateEntry {
    Ids {
        ids: Vec<String>,
        /// Left-hand path of a rename or copy. `select` needs it to find the
        /// "before" content the ids were computed against.
        #[serde(skip_serializing_if = "Option::is_none")]
        from: Option<String>,
    },
    Action {
        action: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        from: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
struct DiffSummaryEntry {
    status: String,
    path: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    target: String,
    #[serde(default)]
    source_executable: bool,
    #[serde(default)]
    target_executable: bool,
    /// jj's `file_type()`: `"file"`, `"symlink"`, `"tree"`, `"git-submodule"`,
    /// `"conflict"`, or `""` when that side has no entry at all.
    #[serde(default)]
    source_file_type: String,
    #[serde(default)]
    target_file_type: String,
}

/// A change to a file's executable bit.
///
/// jj tracks exactly one mode bit, and it is not part of any hunk. A mode-only
/// change therefore produced zero hunks and vanished from the listing, so the
/// file looked unchanged. It is reported here so it is at least visible; it is
/// never *selectable*, and `select` always restores it from the left side (see
/// `restore_exec_bit`), which leaves it in the working copy.
#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ModeChange {
    from: &'static str,
    to: &'static str,
}

fn git_mode(executable: bool) -> &'static str {
    if executable {
        "100755"
    } else {
        "100644"
    }
}

/// Whether either side of this entry is a symlink.
///
/// A link's target is not text: `jj file show` refuses to print one, so both
/// sides of a retargeted link diff as the empty string and `get_hunks` yields
/// nothing. The change is real -- jj calls it modified -- but nothing in the
/// hunk machinery can see it, so it has to be recognised from the summary
/// instead. Either side counts, because a link that becomes an empty file, or
/// an empty file that becomes a link, diffs to nothing just as thoroughly.
fn involves_symlink(entry: &DiffSummaryEntry) -> bool {
    entry.source_file_type == "symlink" || entry.target_file_type == "symlink"
}

fn mode_change_for_entry(entry: &DiffSummaryEntry) -> Option<ModeChange> {
    // On an added or removed file the mode is part of the addition or the
    // removal, not a change on top of it.
    if matches!(entry.status.as_str(), "added" | "removed") {
        return None;
    }
    if entry.source_executable == entry.target_executable {
        return None;
    }
    Some(ModeChange {
        from: git_mode(entry.source_executable),
        to: git_mode(entry.target_executable),
    })
}

/// List hunks in current working copy or a specific revision
pub fn list<T>(options: T) -> Result<()>
where
    T: Into<ListOptions>,
{
    let options = options.into();
    let target = list_target(&options)?;
    let resolved_spec_input = resolve_optional_spec(options.spec.as_deref(), options.spec_file.as_deref())?;
    let spec = match &resolved_spec_input {
        Some(content) if hunkset::is_hunkset(content) => {
            // The same view being listed, so the selector filters exactly the
            // hunks this command is about to print.
            let json = evaluate_hunkset(content, &target, options.truncation)?;
            Some(Spec::from_str(&json)?)
        }
        Some(content) => Some(Spec::from_str(content)?),
        None => None,
    };
    // Before the revision is resolved, so a spec keyed on a path that could
    // never have come from a diff is answered the same way here as it is by
    // the verbs that would act on it. `list --spec` is how a spec gets checked
    // before it is run, so the two have to agree on which keys are even
    // sayable.
    if let Some(spec) = spec.as_ref() {
        validate_spec_paths(spec)?;
    }

    let include = normalize_patterns(&options.include);
    let exclude = normalize_patterns(&options.exclude);
    // Check the patterns before any file is looked at, so a typo is reported
    // the same way whether or not the revision happens to have changes in it.
    for pattern in include.iter().chain(exclude.iter()) {
        crate::glob::validate_glob(pattern)?;
    }

    let all_file_hunks = load_file_hunks(&target, options.binary, options.truncation)?;

    // `list --spec` is how a spec is checked before it is run, so it has to
    // agree with the writing verbs about which paths that spec names. Without
    // this it answered a root-generated spec from a subdirectory with an empty
    // listing and exit 0 -- the same spec `split` refuses outright.
    let spec = spec.map(|mut spec| {
        let frame = PathFrame::discover();
        adopt_spec_frame(&mut spec, &frame_pairs(&all_file_hunks, &frame));
        spec
    });

    let mut files = Vec::new();

    for fh in all_file_hunks {
        let paths_to_check = fh.all_paths();
        if !include.is_empty() && !matches_any(&include, &paths_to_check)? {
            continue;
        }
        if !exclude.is_empty() && matches_any(&exclude, &paths_to_check)? {
            continue;
        }

        let decision = spec_decision(spec.as_ref(), &fh.path);
        if matches!(decision, SpecDecision::Skip) {
            continue;
        }

        // Asked before `fh.hunks` is moved out, and asked of `fh` rather than
        // of the entry below, so that `list` and `evaluate_hunkset` read the
        // one definition of the question.
        let hunkless_change = fh.changes_without_hunks();

        let mut hunks = fh.hunks;

        if let SpecDecision::KeepSelection(selection) = &decision {
            hunks = filter_hunks(hunks, selection)
                .with_context(|| format!("cannot resolve the selection for {}", fh.path))?;
        }

        let entry = FileEntry {
            path: fh.path,
            status: fh.status,
            rename: fh.rename,
            hunks,
            binary: if fh.is_binary { Some(true) } else { None },
            mode: fh.mode,
            symlink: if fh.is_symlink { Some(true) } else { None },
            truncated: if fh.truncated { Some(true) } else { None },
        };

        // An empty hunk list is only nothing to show when a selection filtered
        // the hunks away. Everything else here came from `jj diff`, so it is a
        // change by definition, and dropping the ones no hunk can express made
        // them invisible to `list` and to `--spec-template` both.
        if entry.hunks.is_empty() && !hunkless_change {
            continue;
        }

        files.push(entry);
    }

    match options.mode {
        ListMode::Full => {
            let output = if options.group == ListGrouping::None {
                ListOutput {
                    files: Some(files),
                    groups: None,
                }
            } else {
                let groups = group_files(files, options.group);
                ListOutput {
                    files: None,
                    groups: Some(groups),
                }
            };

            match options.format {
                ListFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&output)?);
                }
                ListFormat::Yaml => {
                    println!("{}", serde_yaml::to_string(&output)?);
                }
                ListFormat::Text => {
                    print!("{}", render_text_output(&output));
                }
                ListFormat::Diff => {
                    print!("{}", render_diff_output(&output));
                }
            }
        }
        ListMode::Files => {
            let summary = build_summary_output(files, options.group);
            match options.format {
                ListFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&summary)?);
                }
                ListFormat::Yaml => {
                    println!("{}", serde_yaml::to_string(&summary)?);
                }
                ListFormat::Text | ListFormat::Diff => {
                    print!("{}", render_text_summary_output(&summary));
                }
            }
        }
        ListMode::SpecTemplate => {
            if matches!(options.format, ListFormat::Text | ListFormat::Diff) {
                anyhow::bail!("--spec-template does not support text output (use json or yaml)");
            }
            let template = build_spec_template(files)?;
            match options.format {
                ListFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&template)?);
                }
                ListFormat::Yaml => {
                    println!("{}", serde_yaml::to_string(&template)?);
                }
                ListFormat::Text | ListFormat::Diff => {}
            }
        }
    }

    Ok(())
}

/// Which diff `list` was asked for.
///
/// The clap layer already refuses `--rev` alongside `--from`/`--to`; this
/// repeats the check because `ListOptions` is public and can be built without
/// going through it.
fn list_target(options: &ListOptions) -> Result<DiffTarget> {
    if options.from.is_none() && options.to.is_none() {
        return Ok(DiffTarget::rev(options.rev.as_deref()));
    }
    if options.rev.is_some() {
        anyhow::bail!("--rev cannot be used with --from/--to");
    }
    // jj defaults whichever side was left out to the working copy.
    Ok(DiffTarget::from_to(
        options.from.as_deref().unwrap_or("@"),
        options.to.as_deref().unwrap_or("@"),
    ))
}

const SUMMARY_TEMPLATE: &str = r#""{\"status\":" ++ self.status().escape_json() ++ ",\"path\":" ++ self.path().display().escape_json() ++ ",\"source\":" ++ self.source().path().display().escape_json() ++ ",\"target\":" ++ self.target().path().display().escape_json() ++ ",\"source_executable\":" ++ if(self.source().executable(), "true", "false") ++ ",\"target_executable\":" ++ if(self.target().executable(), "true", "false") ++ ",\"source_file_type\":" ++ self.source().file_type().escape_json() ++ ",\"target_file_type\":" ++ self.target().file_type().escape_json() ++ "}\n""#;

struct FilePaths {
    before: Option<String>,
    after: Option<String>,
}

pub(crate) enum SpecDecision {
    Skip,
    KeepAll,
    KeepSelection(HunkSelection),
}

#[cfg(feature = "semantic")]
fn enrich_hunks_with_semantics(
    hunks: &mut [Hunk],
    path: &str,
    before_text: &str,
    after_text: &str,
) {
    if hunks.is_empty() {
        return;
    }

    let ext = semantic::extension_from_path(path);
    if ext.is_empty() {
        return;
    }

    // Parse both source texts lazily. For each hunk, we pick the appropriate
    // parsed tree and line number:
    // - Hunks with before_range.length > 0 reference lines in the original file.
    // - Pure insertions (before_range.length == 0) or added files reference
    //   lines in the after file.
    let before_parsed = if !before_text.is_empty() {
        semantic::ParsedFile::parse(ext, before_text)
    } else {
        None
    };
    let after_parsed = if !after_text.is_empty() {
        semantic::ParsedFile::parse(ext, after_text)
    } else {
        None
    };

    for hunk in hunks.iter_mut() {
        let ctx = if hunk.before_range.length > 0 {
            before_parsed
                .as_ref()
                .map(|p| p.context_at_line(hunk.before_range.start))
        } else {
            after_parsed
                .as_ref()
                .map(|p| p.context_at_line(hunk.after_range.start))
        };

        if let Some(ctx) = ctx {
            hunk.semantic = crate::diff::SemanticInfo {
                enclosing_function: ctx.enclosing_function,
                enclosing_scope: ctx.enclosing_scope,
                annotations: ctx.annotations,
                is_doc_comment: ctx.is_doc_comment,
                is_import: ctx.is_import,
                is_toplevel: ctx.is_toplevel,
                is_analyzed: ctx.is_analyzed,
                nesting_depth: ctx.nesting_depth,
            };
        }
    }
}

#[cfg(not(feature = "semantic"))]
fn enrich_hunks_with_semantics(
    _hunks: &mut [Hunk],
    _path: &str,
    _before_text: &str,
    _after_text: &str,
) {
}

pub(crate) fn resolve_optional_spec(
    spec: Option<&str>,
    spec_file: Option<&str>,
) -> Result<Option<String>> {
    if spec.is_none() && spec_file.is_none() {
        return Ok(None);
    }

    Ok(Some(resolve_spec_input(spec, spec_file)?))
}

/// Evaluate a hunkset expression against the current diff state for a given
/// revision, returning a JSON-serialized Spec.
///
/// `truncation` must be the same view the result will be applied to. The
/// returned spec names concrete hunk ids, and an id only means anything
/// relative to the text it was computed from -- evaluating against the whole
/// file and then filtering a truncated listing with the result silently drops
/// any hunk the cut reshaped.
pub(crate) fn evaluate_hunkset(
    hunkset_expr: &str,
    target: &DiffTarget,
    truncation: Truncation,
) -> Result<String> {
    let ast = hunkset::parse(hunkset_expr)
        .map_err(|e| e.coded(format!("failed to parse hunkset:\n{}", e.display_with_context())))?;

    // `Mark`, not `Skip`. Skipping meant no expression could so much as name a
    // binary file, while the spec that came back still said `default: reset` --
    // so `all()` quietly meant "all except the binaries" and their changes were
    // dropped from whatever the verb went on to do. They are visible here
    // instead, each standing in for its whole-file change.
    let file_hunks = load_file_hunks(target, BinaryMode::Mark, truncation)?;

    // A binary was only the narrowest instance of that bug. Every shape
    // `changes_without_hunks` names has the same two halves -- no hunk to be
    // matched by, and a real change that `default: reset` will undo -- so
    // `all()` also meant "all except the symlinks, the renames, the mode flips
    // and the empty adds", and left every one of them behind at exit 0.
    //
    // Computed once, and used again below to rewrite what was selected. Two
    // separate conditions could disagree, and the way they would fail is a
    // spec naming stand-in ids that `select` cannot resolve.
    let stand_ins: Vec<(usize, Hunk)> = file_hunks
        .iter()
        .enumerate()
        .filter(|(_, fh)| fh.hunks.is_empty() && fh.changes_without_hunks())
        .map(|(index, fh)| (index, whole_file_stand_in(fh)))
        .collect();

    // The rename source rides along so that `file()`, `glob()` and
    // `extension()` can name a change by the path it moved *from*, which is
    // what `--include`/`--exclude` have always done through
    // `FileHunks::all_paths`. Same accessor on both sides, so the two cannot
    // answer "which files does this pattern name?" differently again.
    let mut enriched: Vec<EnrichedHunk> = file_hunks
        .iter()
        .flat_map(|fh| {
            fh.hunks
                .iter()
                .map(move |hunk| EnrichedHunk::real(&fh.path, fh.rename_source(), &fh.status, hunk))
        })
        .collect();
    // `stand_in`, not `real`, and that is the whole of the content-level
    // guarantee: the evaluator declines to show a stand-in's text to
    // `content()`, `added()`, `removed()`, `lines()` and `id()` because it is
    // told which entries are stand-ins, not because their text is empty.
    // A pure rename *is* a stand-in -- it produces no hunks at all -- so this
    // is the entry that has to carry both paths, or `file("<old name>")` still
    // could not reach the one change where the old name is all you have.
    enriched.extend(stand_ins.iter().map(|(index, hunk)| {
        let fh = &file_hunks[*index];
        EnrichedHunk::stand_in(&fh.path, fh.rename_source(), &fh.status, hunk)
    }));

    let selected = hunkset::evaluate(&ast, &enriched)
        .map_err(|e| e.coded(format!("hunkset evaluation error: {}", e.display_with_context())))?;

    let rename_sources: HashMap<&str, &str> = file_hunks
        .iter()
        .filter_map(|fh| fh.rename_source().map(|from| (fh.path.as_str(), from)))
        .collect();
    let mut spec = hunkset::to_spec(&selected, &enriched, &rename_sources);

    // Each of these is kept or reset whole, never hunk-wise -- a binary cannot
    // survive `select`'s text round trip, and a link target, an exec bit and a
    // path are each one atomic value with no half to pick. The stand-in exists
    // only so a predicate can name the file, and the id it carries names
    // nothing `select` could resolve, so a selected one is rewritten into the
    // same whole-file action `--spec-template` emits for it.
    //
    // This walks the set the stand-ins were built from, not a second guess at
    // it: a file given a stand-in and missed here would leave `whole-file:`
    // ids in the spec, which `select` rejects.
    for (index, _) in &stand_ins {
        let fh = &file_hunks[*index];
        if let Some(entry) = spec.files.get_mut(&fh.path) {
            *entry = FileSpec::Action {
                action: Action::Keep,
                from: rename_source(&fh.rename, &fh.path),
            };
        }
    }

    serde_json::to_string(&spec).context("failed to serialize hunkset result as spec")
}

/// The one hunk a file with no hunks gets to be named by.
///
/// Each such change is atomic -- git models a binary the same way, as "these
/// two files differ" and nothing finer, and a link target, an exec bit and a
/// path have no halves either -- so the stand-in carries exactly what a
/// *file-level* predicate reads: the path and status come from the enclosing
/// [`EnrichedHunk`], and `type()` reports what happened to the file as a whole.
///
/// What a *content-level* predicate reads is left empty here, but nothing rests
/// on that any more. It is the enclosing [`EnrichedHunk::stand_in`] that makes
/// this unmatchable by `content()`, `added()`, `removed()`, `lines()` and
/// `id()`, because emptiness turned out not to mean unmatchable: `content("")`
/// found the empty string inside the empty string, and `lines(0..N)` found line
/// 0 inside the range. The fields stay empty because there is honestly nothing
/// to put in them -- a stand-in made any more real would start answering
/// questions about text that was never diffed -- not because emptiness is a
/// defence.
///
/// The id is not in hunk-id form on purpose, and that property is still worth
/// keeping even now that `id()` declines a stand-in outright.
/// `normalize_hunk_id` only ever yields `hunk-<hex>`, so `id("whole-file:link")`
/// is refused as a malformed argument rather than accepted and silently matching
/// nothing -- which is the difference between a typo being reported and `~id(..)`
/// quietly meaning "everything".
fn whole_file_stand_in(file: &FileHunks) -> Hunk {
    let hunk_type = match file.status.as_str() {
        "added" | "copied" => "insert",
        "removed" => "delete",
        _ => "replace",
    };
    let name = format!("whole-file:{}", file.path);

    Hunk {
        index: 0,
        short_id: name.clone(),
        id: name,
        hunk_type: hunk_type.to_string(),
        removed: String::new(),
        added: String::new(),
        // Line 0 exists in no file. That is a true statement about a real
        // file and a false one about `lines()`: `hunk_touches_range` reads a
        // zero-length range as the single point `start`, and 0 is inside
        // `0..100000`. `EnrichedHunk::stand_in` is what keeps `lines()` away.
        before_range: crate::diff::LineRange { start: 0, length: 0 },
        after_range: crate::diff::LineRange { start: 0, length: 0 },
        context: None,
        semantic: crate::diff::SemanticInfo::default(),
    }
}

/// A file's hunks with metadata, loaded from a jj diff.
pub(crate) struct FileHunks {
    pub(crate) path: String,
    pub(crate) status: String,
    pub(crate) hunks: Vec<Hunk>,
    pub(crate) rename: Option<RenameInfo>,
    pub(crate) is_binary: bool,
    /// Either side is a symlink, so this entry can carry a change no hunk
    /// expresses (see `involves_symlink`).
    pub(crate) is_symlink: bool,
    pub(crate) mode: Option<ModeChange>,
    /// Whether either side of this file was cut short before diffing.
    pub(crate) truncated: bool,
}

impl FileHunks {
    /// Whether this file carries a change that no hunk can express, so an
    /// empty hunk list does not mean "nothing happened here".
    ///
    /// Five shapes reach this: binary contents (never split hunk-wise), a
    /// mode-only flip (jj's exec bit is not part of any hunk), a rename or copy
    /// of a file whose text did not move (both sides diff identical), an
    /// empty file added or removed (nothing on either side to diff), and a
    /// symlink, whose target `jj file show` will not print -- so a link
    /// retargeted from one path to another diffs empty against empty. All five
    /// have to stay visible in `list`, be named by `--spec-template`, and be
    /// reachable by a hunkset expression, because a file none of those reaches
    /// takes `default: reset` -- which for a rename restores the old path and
    /// deletes the new one, and for an added empty file deletes it outright.
    ///
    /// This lives on `FileHunks` rather than on the `FileEntry` built from it
    /// because the two callers need it at different points: `list` asks before
    /// it has an entry, and `evaluate_hunkset` never builds one. Asking the
    /// loader's own type is the only way the two cannot answer differently,
    /// and them disagreeing is exactly how a shape goes missing from one verb
    /// while looking fine in another.
    fn changes_without_hunks(&self) -> bool {
        self.is_binary
            || self.mode.is_some()
            || self.is_symlink
            || self.rename_source().is_some()
            || matches!(self.status.as_str(), "added" | "removed")
    }

    /// The path this file had on the left side of the diff, when a rename or a
    /// copy moved it. `None` when the two sides agree.
    ///
    /// The one definition of "the other name this file goes by", because three
    /// separate spellings of `rename.from != path` is three chances for them to
    /// drift: `--include`/`--exclude` filter through `all_paths` below, the
    /// spec carries it as `from:`, and the hunkset predicates match on it via
    /// `EnrichedHunk::all_paths`. Those had already drifted once -- the hunkset
    /// predicates simply did not have it -- which is the gap this closed.
    pub(crate) fn rename_source(&self) -> Option<&str> {
        self.rename
            .as_ref()
            .filter(|r| r.from != self.path)
            .map(|r| r.from.as_str())
    }

    /// All paths associated with this file entry (primary + rename source).
    pub(crate) fn all_paths(&self) -> Vec<&str> {
        let mut paths = vec![self.path.as_str()];
        paths.extend(self.rename_source());
        paths
    }
}

/// Load all file hunks for a revision, applying semantic enrichment.
/// This is the shared core used by both `list` and `evaluate_hunkset`.
pub(crate) fn load_file_hunks(
    target: &DiffTarget,
    binary: BinaryMode,
    truncation: Truncation,
) -> Result<Vec<FileHunks>> {
    // Validate the revision *before* doing any work: a merge or a
    // multi-revision revset makes every hunk below meaningless.
    let revisions = resolve_revisions(target)?;
    let summary_entries = read_diff_summary(target)?;
    let mut result = Vec::new();
    // Every path below is cwd-relative, because that is how jj prints them and
    // that is what `list` has to keep showing. Ids are hashed from the
    // root-relative spelling instead, so that the id for a hunk is the same one
    // `select` computes and the same one it had when the command was run a
    // directory up.
    let frame = PathFrame::discover();

    for entry in &summary_entries {
        let path = primary_path(entry);
        if path.is_empty() {
            continue;
        }
        let hashed_path = frame.to_root(&path).unwrap_or_else(|| path.clone());

        let file_paths = file_paths_for_entry(entry, &path);
        let before_bytes = match (revisions.before.as_deref(), file_paths.before.as_deref()) {
            (Some(rev), Some(p)) => read_jj_file(Some(rev), p)?,
            // No parent revision, or this side of the diff has no file.
            _ => Vec::new(),
        };
        let after_bytes = match file_paths.after.as_deref() {
            Some(p) => read_jj_file(revisions.after.as_deref(), p)?,
            None => Vec::new(),
        };

        let is_binary = is_binary_data(&before_bytes) || is_binary_data(&after_bytes);
        if is_binary && binary == BinaryMode::Skip {
            continue;
        }

        let should_diff = !is_binary || binary == BinaryMode::Include;
        let ((before_text, before_cut), (after_text, after_cut)) = if should_diff {
            (
                truncate_text(&String::from_utf8_lossy(&before_bytes), truncation),
                truncate_text(&String::from_utf8_lossy(&after_bytes), truncation),
            )
        } else {
            ((String::new(), false), (String::new(), false))
        };

        let mut hunks = if should_diff {
            get_hunks(&hashed_path, &before_text, &after_text)
        } else {
            Vec::new()
        };

        enrich_hunks_with_semantics(&mut hunks, &path, &before_text, &after_text);

        result.push(FileHunks {
            path,
            status: entry.status.clone(),
            hunks,
            rename: rename_info(entry),
            is_binary,
            is_symlink: involves_symlink(entry),
            mode: mode_change_for_entry(entry),
            truncated: before_cut || after_cut,
        });
    }

    // Abbreviate against the whole diff, not per file. A short id has to name
    // one hunk among everything the user is looking at, and it also has to
    // mean the same thing whether or not they passed --include, so this
    // happens before any filtering.
    diff::assign_short_ids(result.iter_mut().flat_map(|file| file.hunks.iter_mut()));

    Ok(result)
}

/// The two sides a diff is computed between.
pub(crate) struct DiffRevisions {
    /// Revision the "before" text is read from. `None` means the target has no
    /// parent (the root commit), so the before side is empty.
    pub(crate) before: Option<String>,
    /// Revision the "after" text is read from. `None` means the working copy
    /// on disk.
    pub(crate) after: Option<String>,
}

/// One resolved revision and the ids of its parents.
pub(crate) struct ResolvedRevision {
    pub(crate) id: String,
    pub(crate) parents: Vec<String>,
}

/// Emits `<commit id>\t<parent id> <parent id> ...` per revision, so one
/// `jj log` answers both "how many revisions?" and "how many parents?".
const REVISION_TEMPLATE: &str =
    r#"commit_id.short() ++ "\t" ++ parents.map(|c| c.commit_id().short()).join(" ") ++ "\n""#;

/// Revisions a revset resolves to, in `jj log` order.
pub(crate) fn resolve_revset(revset: &str) -> Result<Vec<ResolvedRevision>> {
    let output = Command::new("jj")
        .args(["log", "--no-graph", "-r", revset, "-T", REVISION_TEMPLATE])
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
        // jj's own wording, passed through rather than re-read: it is the only
        // thing that says *why* the revset did not resolve.
        .with("jj_stderr", stderr.trim())
        .into());
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let (id, parents) = line.split_once('\t').unwrap_or((line, ""));
            ResolvedRevision {
                id: id.trim().to_string(),
                parents: parents.split_whitespace().map(str::to_string).collect(),
            }
        })
        .collect())
}

/// The single revision `revset` names, or an error saying what it named
/// instead.
///
/// Every diff jj-hunk builds is between two specific trees, so a revset that
/// resolves to none or to several has no meaning here.
fn resolve_single_revision(revset: &str) -> Result<ResolvedRevision> {
    let mut resolved = resolve_revset(revset)?;
    match resolved.len() {
        // Same code as a revset jj rejected outright: from the caller's side
        // both mean "this names no revision to diff", and the distinction --
        // whether jj could parse it -- is in `jj_stderr`, present only when it
        // could not.
        0 => Err(CodedError::new(
            errors::REVSET_UNRESOLVED,
            format!("revset `{}` did not resolve to any revision", revset),
        )
        .with("revset", revset)
        .with("resolved", 0)
        .into()),
        1 => Ok(resolved.remove(0)),
        n => {
            let ids: Vec<&str> = resolved.iter().map(|r| r.id.as_str()).collect();
            Err(CodedError::new(
                errors::REVSET_AMBIGUOUS,
                format!(
                    "revset `{}` resolved to {} revisions, but jj-hunk needs exactly one.\n\
                     Hunks are only defined between a single revision and its parent.\n\
                     Resolved to: {}",
                    revset,
                    n,
                    preview_ids(ids.iter().copied())
                ),
            )
            .with("revset", revset)
            .with("resolved", n)
            // Capped at the same width the prose previews, so the two cannot
            // disagree and a revset matching a whole repo does not turn one
            // error line into megabytes. `resolved` is the true count.
            .with(
                "revisions",
                ids.iter().take(PREVIEW_IDS).copied().collect::<Vec<_>>(),
            )
            .into())
        }
    }
}

/// How many ids an error names before it stops counting.
const PREVIEW_IDS: usize = 5;

fn preview_ids<'a>(ids: impl ExactSizeIterator<Item = &'a str>) -> String {
    let total = ids.len();
    let mut shown: Vec<String> = ids.take(PREVIEW_IDS).map(str::to_string).collect();
    if total > PREVIEW_IDS {
        shown.push(format!("... and {} more", total - PREVIEW_IDS));
    }
    shown.join(", ")
}

/// Work out which revisions to read the before/after text from.
///
/// A hunk only means something between exactly one revision and exactly one
/// parent. Both of the ways that can fail used to degrade into an empty
/// before-text -- which is indistinguishable from "the whole file is new" --
/// so the diff was wrong but the exit status was 0:
///
///   * a revset resolving to several revisions (`-r 'all()'`), and
///   * a merge commit, where `@-` is ambiguous.
///
/// Both are now rejected outright.
///
/// A `FromTo` target names both sides itself, so neither failure applies to it:
/// each side only has to resolve to exactly one revision.
pub(crate) fn resolve_revisions(target: &DiffTarget) -> Result<DiffRevisions> {
    let revset = match target {
        DiffTarget::Rev(revset) => revset.as_deref(),
        DiffTarget::FromTo { from, to } => {
            return Ok(DiffRevisions {
                before: Some(resolve_single_revision(from)?.id),
                after: Some(resolve_single_revision(to)?.id),
            })
        }
    };

    let target = revset.unwrap_or("@");

    let resolved = resolve_single_revision(target)?;
    if resolved.parents.len() > 1 {
        anyhow::bail!(
            "`{}` ({}) is a merge commit with {} parents, so it has no single \
             \"before\" state to diff against.\n\
             Reporting it anyway would describe every file as a whole-file insertion, \
             and a selection built from that diff would commit something other than \
             what was listed.\n\
             Diff against one parent explicitly (`-r {}`), or choose a non-merge revision.\n\
             Parents: {}",
            target,
            resolved.id,
            resolved.parents.len(),
            resolved.parents[0],
            preview_ids(resolved.parents.iter().map(String::as_str))
        );
    }

    Ok(DiffRevisions {
        // Resolved ids rather than `({rev})-`: they cannot be re-resolved into
        // something else between here and the reads below.
        before: resolved.parents.into_iter().next(),
        after: revset.map(|_| resolved.id),
    })
}

fn read_diff_summary(target: &DiffTarget) -> Result<Vec<DiffSummaryEntry>> {
    let mut diff_args = vec!["diff", "--template", SUMMARY_TEMPLATE];
    match target {
        DiffTarget::Rev(Some(rev)) => {
            diff_args.push("-r");
            diff_args.push(rev);
        }
        DiffTarget::Rev(None) => {}
        DiffTarget::FromTo { from, to } => {
            diff_args.push("--from");
            diff_args.push(from);
            diff_args.push("--to");
            diff_args.push(to);
        }
    }

    let output = Command::new("jj")
        .args(&diff_args)
        .output()
        .context("Failed to run jj diff")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("jj diff failed: {}", stderr.trim());
    }

    let summary = String::from_utf8_lossy(&output.stdout);
    let mut entries = Vec::new();
    for (index, line) in summary.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: DiffSummaryEntry = serde_json::from_str(line)
            .with_context(|| format!("Failed to parse diff summary line {}", index + 1))?;
        entries.push(entry);
    }

    Ok(entries)
}

fn primary_path(entry: &DiffSummaryEntry) -> String {
    if !entry.path.is_empty() {
        entry.path.clone()
    } else if !entry.target.is_empty() {
        entry.target.clone()
    } else {
        entry.source.clone()
    }
}

fn rename_info(entry: &DiffSummaryEntry) -> Option<RenameInfo> {
    match entry.status.as_str() {
        "renamed" | "copied" => {
            if entry.source.is_empty() {
                return None;
            }
            let to = if entry.target.is_empty() {
                entry.path.clone()
            } else {
                entry.target.clone()
            };
            Some(RenameInfo {
                from: entry.source.clone(),
                to,
            })
        }
        _ => None,
    }
}

fn file_paths_for_entry(entry: &DiffSummaryEntry, path: &str) -> FilePaths {
    match entry.status.as_str() {
        "added" => FilePaths {
            before: None,
            after: Some(path.to_string()),
        },
        "removed" => FilePaths {
            before: Some(path.to_string()),
            after: None,
        },
        "renamed" | "copied" => {
            let before = if entry.source.is_empty() {
                path.to_string()
            } else {
                entry.source.clone()
            };
            let after = if entry.target.is_empty() {
                path.to_string()
            } else {
                entry.target.clone()
            };
            FilePaths {
                before: Some(before),
                after: Some(after),
            }
        }
        _ => FilePaths {
            before: Some(path.to_string()),
            after: Some(path.to_string()),
        },
    }
}

/// Quote `value` as a jj fileset string literal, including the delimiters.
///
/// From jj's own grammar (`lib/src/fileset.pest`):
///
/// ```text
/// string_literal = "\"" ~ (string_content | string_escape)* ~ "\""
/// string_escape  = "\\" ~ ("t"|"r"|"n"|"0"|"e"|("x" ~ HEX{2})|"\""|"\\")
/// ```
///
/// Only `"` and `\` are excluded from `string_content`, so those are the two
/// that strictly must be escaped; control characters are escaped as well so the
/// argument stays a single readable token in any error message.
fn fileset_string_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            '\0' => out.push_str("\\0"),
            '\x1b' => out.push_str("\\e"),
            // The remaining C0 controls have no named escape. `\xHH` is only
            // used for these, so it never has to encode a multi-byte char.
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A fileset expression matching exactly one path.
///
/// Passing a path to jj bare makes jj parse it as fileset *syntax*: `(`, `)`,
/// `~`, `,` and `"` are operators, and the default pattern kind is a glob, so
/// `br[1].txt` silently matches `br1.txt`. `file:` is the cwd-relative exact
/// pattern, which is the frame `.display()` reports paths in (see
/// `SUMMARY_TEMPLATE`).
fn fileset_exact_path(path: &str) -> String {
    format!("file:{}", fileset_string_literal(path))
}

fn read_jj_file(rev: Option<&str>, path: &str) -> Result<Vec<u8>> {
    let pattern = fileset_exact_path(path);
    let mut args = vec!["file", "show"];
    if let Some(rev) = rev {
        args.push("-r");
        args.push(rev);
    }
    args.push(&pattern);

    let output = Command::new("jj")
        .args(&args)
        .output()
        .with_context(|| format!("Failed to run jj file show for {}", path))?;

    // A failed read used to become an empty file, which is indistinguishable
    // from "this file is new" -- the file then produced no hunks and was
    // dropped from the listing entirely.
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "failed to read `{}`{}: {}",
            path,
            rev.map(|r| format!(" at revision {}", r))
                .unwrap_or_default(),
            stderr.trim()
        );
    }

    Ok(output.stdout)
}

fn is_binary_data(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }

    bytes.contains(&0) || std::str::from_utf8(bytes).is_err()
}

/// Cut `content` down to the configured caps, reporting whether anything was
/// dropped. A limit the content already fits inside is not a truncation.
fn truncate_text(content: &str, truncation: Truncation) -> (String, bool) {
    let mut truncated = false;
    let mut result = content.to_string();

    if let Some(max_lines) = truncation.max_lines {
        // `split_inclusive` keeps each line's terminator, so the kept prefix is
        // byte-identical to the head of the original.
        let mut rest = result.split_inclusive('\n');
        let kept: String = rest.by_ref().take(max_lines).collect();
        // Only a line we did not keep counts as a truncation, so a limit the
        // file already fits under changes nothing.
        if rest.next().is_some() {
            truncated = true;
            result = kept;
        }
    }

    if let Some(max_bytes) = truncation.max_bytes {
        if result.len() > max_bytes {
            // Never split a multi-byte character: the tail would not be valid
            // UTF-8, and `String::truncate` panics on a non-boundary index.
            let mut end = max_bytes;
            while !result.is_char_boundary(end) {
                end -= 1;
            }
            result.truncate(end);
            truncated = true;
        }
    }

    (result, truncated)
}

pub(crate) fn spec_decision(spec: Option<&Spec>, path: &str) -> SpecDecision {
    let Some(spec) = spec else {
        return SpecDecision::KeepAll;
    };

    if let Some(file_spec) = spec.files.get(path) {
        match file_spec {
            FileSpec::Action {
                action: Action::Keep,
                ..
            } => SpecDecision::KeepAll,
            FileSpec::Action {
                action: Action::Reset,
                ..
            } => SpecDecision::Skip,
            FileSpec::Selection(selection) => {
                let selection = selection.to_selection();
                if selection.is_empty() {
                    SpecDecision::Skip
                } else {
                    SpecDecision::KeepSelection(selection)
                }
            }
        }
    } else if spec.default == DefaultAction::Reset {
        SpecDecision::Skip
    } else {
        SpecDecision::KeepAll
    }
}

pub(crate) fn filter_hunks(hunks: Vec<Hunk>, selection: &HunkSelection) -> Result<Vec<Hunk>> {
    let keep = selection.resolve(&hunks)?;
    Ok(hunks
        .into_iter()
        .filter(|hunk| keep.contains(&hunk.index))
        .collect())
}

/// One flag occurrence is one pattern.
///
/// These used to be split on `,` as well. A comma is an ordinary character in
/// a path and carries no meaning in a glob, so that made every comma-containing
/// path unreachable by a filter, with no way to escape it -- the last place
/// such paths could not be named. `--include`/`--exclude` are repeatable, which
/// is how the README documents them, so nothing is lost.
fn normalize_patterns(patterns: &[String]) -> Vec<String> {
    patterns
        .iter()
        .map(|pattern| pattern.trim())
        .filter(|pattern| !pattern.is_empty())
        .map(|pattern| pattern.to_string())
        .collect()
}

/// Whether any of `paths` matches any of `patterns`.
///
/// A malformed pattern is an error, not a silent non-match. `--exclude` exists
/// to keep something out of the listing, and a pattern that matches nothing
/// keeps nothing out -- so a typo in `--exclude 'vendor/[a-z*.txt'` used to
/// list the vendored tree it was written to hide, and `list --spec-template`
/// would then bake that listing into a spec that drives `split`.
fn matches_any(patterns: &[String], paths: &[&str]) -> Result<bool> {
    for pattern in patterns {
        for path in paths {
            if glob_match(pattern, path)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn group_files(files: Vec<FileEntry>, grouping: ListGrouping) -> Vec<ListGroup> {
    let mut groups: Vec<ListGroup> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();

    for file in files {
        let key = match grouping {
            ListGrouping::Directory => directory_group(&file.path),
            ListGrouping::Extension => extension_group(&file.path),
            ListGrouping::Status => file.status.clone(),
            ListGrouping::None => String::new(),
        };

        if let Some(position) = index.get(&key).copied() {
            groups[position].files.push(file);
        } else {
            index.insert(key.clone(), groups.len());
            groups.push(ListGroup {
                name: key,
                files: vec![file],
            });
        }
    }

    groups
}

fn build_summary_output(files: Vec<FileEntry>, grouping: ListGrouping) -> ListSummaryOutput {
    let summaries: Vec<FileSummary> = files
        .into_iter()
        .map(|file| FileSummary {
            path: file.path,
            status: file.status,
            rename: file.rename,
            hunk_count: file.hunks.len(),
            binary: file.binary,
            mode: file.mode,
            symlink: file.symlink,
            truncated: file.truncated,
        })
        .collect();

    if grouping == ListGrouping::None {
        ListSummaryOutput {
            files: Some(summaries),
            groups: None,
        }
    } else {
        let groups = group_summaries(summaries, grouping);
        ListSummaryOutput {
            files: None,
            groups: Some(groups),
        }
    }
}

fn group_summaries(files: Vec<FileSummary>, grouping: ListGrouping) -> Vec<ListSummaryGroup> {
    let mut groups: Vec<ListSummaryGroup> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();

    for file in files {
        let key = match grouping {
            ListGrouping::Directory => directory_group(&file.path),
            ListGrouping::Extension => extension_group(&file.path),
            ListGrouping::Status => file.status.clone(),
            ListGrouping::None => String::new(),
        };

        if let Some(position) = index.get(&key).copied() {
            groups[position].files.push(file);
        } else {
            index.insert(key.clone(), groups.len());
            groups.push(ListSummaryGroup {
                name: key,
                files: vec![file],
            });
        }
    }

    groups
}

/// The left-hand path of a rename or copy, when it differs from the current
/// path. A spec entry needs it so `select` can find the "before" content the
/// entry's hunk ids were computed against.
fn rename_source(rename: &Option<RenameInfo>, path: &str) -> Option<String> {
    rename
        .as_ref()
        .filter(|r| r.from != path)
        .map(|r| r.from.clone())
}

fn build_spec_template(files: Vec<FileEntry>) -> Result<SpecTemplateOutput> {
    // A hunk id is a hash of the text it was diffed from, so ids taken from a
    // file that was cut short do not exist in the real diff. Emitting them
    // would hand over a template that `split` is bound to reject, naming ids
    // this very command had just printed.
    let cut: Vec<&str> = files
        .iter()
        .filter(|file| file.truncated == Some(true))
        .map(|file| file.path.as_str())
        .collect();
    if !cut.is_empty() {
        return Err(CodedError::new(
            errors::TRUNCATED_SPEC_TEMPLATE,
            format!(
                "cannot build a spec template from truncated files:\n  {}\n\
                 Hunk ids are computed from the file contents, so ids taken from a \
                 truncated file will not match the real diff.\n\
                 Drop --max-bytes/--max-lines to template these files.",
                cut.join("\n  ")
            ),
        )
        .with("paths", cut)
        .into());
    }

    let mut output = BTreeMap::new();

    for file in files {
        let from = rename_source(&file.rename, &file.path);

        // Whole-file entries, for the two kinds of file that cannot be named
        // hunk by hand:
        //
        // - Binary. `select` rebuilds a file from text and a non-UTF-8 file
        //   cannot survive that round trip. Under `--binary include` it does
        //   carry hunks, but they are for reading, not for selecting, so the
        //   template names the action rather than ids it could not honour.
        // - A change with no hunks at all (see `changes_without_hunks`).
        //   Skipping these emitted a template that did not mention the file,
        //   and `select` resets every file the spec leaves unnamed -- so the
        //   documented `--spec-template` -> `split` round trip silently undid
        //   the very rename it had just been asked to carry.
        if file.binary == Some(true) || file.hunks.is_empty() {
            output.insert(
                file.path,
                SpecTemplateEntry::Action {
                    action: "keep".to_string(),
                    from,
                },
            );
            continue;
        }

        // Abbreviated, because a template exists to be edited by hand: the
        // documented workflow is to redirect it to a file and delete the ids
        // you do not want, and 64-character lines make that miserable. Short
        // ids are unambiguous over the diff this was built from, and
        // `validate_spec_resolves` turns any that stop being so into an error
        // rather than a silent mis-select.
        let ids = file.hunks.into_iter().map(|hunk| hunk.short_id).collect();
        output.insert(file.path, SpecTemplateEntry::Ids { ids, from });
    }

    Ok(SpecTemplateOutput {
        files: output,
        default: "reset".to_string(),
    })
}

fn directory_group(path: &str) -> String {
    let path = Path::new(path);
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_string_lossy().to_string(),
        _ => ".".to_string(),
    }
}

fn extension_group(path: &str) -> String {
    let path = Path::new(path);
    match path.extension() {
        Some(ext) => ext.to_string_lossy().to_string(),
        None => "<no-ext>".to_string(),
    }
}

fn render_text_output(output: &ListOutput) -> String {
    let mut lines = Vec::new();

    if let Some(groups) = &output.groups {
        for (index, group) in groups.iter().enumerate() {
            let name = if group.name == "." || group.name.is_empty() {
                "<root>"
            } else {
                group.name.as_str()
            };
            lines.push(format!("{}:", name));
            format_files_text(&mut lines, &group.files);
            if index + 1 < groups.len() {
                lines.push(String::new());
            }
        }
    } else if let Some(files) = &output.files {
        format_files_text(&mut lines, files);
    }

    if lines.is_empty() {
        return String::new();
    }

    let mut output = lines.join("\n");
    output.push('\n');
    output
}

fn render_diff_output(output: &ListOutput) -> String {
    let mut result = String::new();

    let files = if let Some(groups) = &output.groups {
        groups.iter().flat_map(|g| g.files.iter()).collect::<Vec<_>>()
    } else if let Some(files) = &output.files {
        files.iter().collect()
    } else {
        return result;
    };

    for file in files {
        let (a_path, b_path) = match file.status.as_str() {
            "added" => ("/dev/null".to_string(), format!("b/{}", file.path)),
            "removed" => (format!("a/{}", file.path), "/dev/null".to_string()),
            _ => (format!("a/{}", file.path), format!("b/{}", file.path)),
        };

        result.push_str(&format!("--- {}\n+++ {}\n", a_path, b_path));

        // Hunks whose context windows touch or overlap must be emitted as a
        // single @@ block, otherwise the ranges overlap and the patch is
        // invalid. Anything further apart than two context windows is
        // independent.
        //
        // `new_offset` is what turns the after-side line numbers of the *whole*
        // diff into the after-side line numbers of *this patch*. It is reset
        // per file because each file's coordinates are its own.
        let mut new_offset: i64 = 0;
        for block in group_adjacent_hunks(&file.hunks) {
            let (text, delta) = render_diff_block(&file.hunks[block.0..block.1], new_offset);
            result.push_str(&text);
            new_offset += delta;
        }
    }

    result
}

/// Split a file's hunks into runs that must share one `@@` block.
/// Returns half-open index ranges into `hunks`.
fn group_adjacent_hunks(hunks: &[Hunk]) -> Vec<(usize, usize)> {
    let mut blocks = Vec::new();
    let mut start = 0;
    for i in 1..hunks.len() {
        let prev = &hunks[i - 1];
        let prev_end = prev.before_range.start + prev.before_range.length;
        let gap = hunks[i].before_range.start.saturating_sub(prev_end);
        // Two windows of CONTEXT_LINES is the most we can reconstruct from the
        // stored per-hunk context, and also the point past which git would
        // split the hunks anyway.
        if gap > 2 * DIFF_CONTEXT_LINES {
            blocks.push((start, i));
            start = i;
        }
    }
    if !hunks.is_empty() {
        blocks.push((start, hunks.len()));
    }
    blocks
}

/// Context lines stored per hunk by `diff::build_context`.
const DIFF_CONTEXT_LINES: usize = 3;

/// Split into lines WITHOUT discarding the line terminator information.
///
/// `str::lines()` strips both `\n` and a preceding `\r`, and loses whether the
/// final line was newline-terminated at all. Re-emitting those lines with a
/// bare `\n` silently converted CRLF files to LF and dropped the
/// `\ No newline at end of file` marker, so `git apply` either rejected the
/// patch or -- when the change was only the trailing newline -- applied a
/// textually identical hunk and silently discarded the edit.
///
/// Returns (content_without_trailing_newline, was_newline_terminated). The
/// content keeps any `\r`, which unified diff treats as part of the line.
fn context_lines(text: &str) -> Vec<(&str, bool)> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        match rest.find('\n') {
            Some(i) => {
                out.push((&rest[..i], true));
                rest = &rest[i + 1..];
            }
            None => {
                out.push((rest, false));
                rest = "";
            }
        }
    }
    out
}

const NO_NEWLINE_MARKER: &str = "\\ No newline at end of file\n";

/// Emit one diff line, appending the no-newline marker when the source line was
/// not newline-terminated.
fn push_diff_line(body: &mut String, prefix: char, line: &str, newline_terminated: bool) {
    body.push(prefix);
    body.push_str(line);
    body.push('\n');
    if !newline_terminated {
        body.push_str(NO_NEWLINE_MARKER);
    }
}

/// Reconstruct the `gap` source lines sitting between two hunks.
///
/// `prev.context.post` holds the first up-to-3 lines after `prev`, and
/// `next.context.pre` holds the last up-to-3 lines before `next`. For a gap of
/// at most `2 * DIFF_CONTEXT_LINES` the two together cover it exactly.
fn gap_lines<'a>(prev: &'a Hunk, next: &'a Hunk, gap: usize) -> Vec<(&'a str, bool)> {
    let post: Vec<(&str, bool)> = prev
        .context
        .as_ref()
        .map(|c| context_lines(&c.after))
        .unwrap_or_default();
    let mut out: Vec<(&str, bool)> = post.into_iter().take(gap.min(DIFF_CONTEXT_LINES)).collect();
    if out.len() < gap {
        let pre: Vec<(&str, bool)> = next
            .context
            .as_ref()
            .map(|c| context_lines(&c.before))
            .unwrap_or_default();
        // `pre` covers the last min(3, gap) lines of the gap; take the tail we
        // are still missing.
        let need = gap - out.len();
        let skip = pre.len().saturating_sub(need);
        out.extend(pre.into_iter().skip(skip));
    }
    out
}

/// Render one `@@` block, and report how far it moves the after-side.
///
/// `new_offset` is the running difference between before-side and after-side
/// line numbers, accumulated over the blocks already emitted for this file.
/// The returned delta is this block's own contribution to it.
///
/// The after-side start cannot be read off `Hunk::after_range`: that coordinate
/// is in the working-copy file, which includes every hunk, and a `--spec` emits
/// only some of them. Using it left the `+` start counting hunks this patch
/// does not carry, and `git apply` anchors its search on exactly that number --
/// so in a file with repeated text it would find the *next* matching block,
/// report "succeeded ... (offset -13 lines)", and rewrite the wrong lines at
/// exit 0. Deriving the start from the before-side plus what this patch has
/// actually emitted so far keeps the two sides describing the same file.
fn render_diff_block(hunks: &[Hunk], new_offset: i64) -> (String, i64) {
    let mut result = String::new();
    let Some(first) = hunks.first() else {
        return (result, 0);
    };
    let last = &hunks[hunks.len() - 1];

    let leading: Vec<(&str, bool)> = first
        .context
        .as_ref()
        .map(|c| context_lines(&c.before))
        .unwrap_or_default();
    let trailing: Vec<(&str, bool)> = last
        .context
        .as_ref()
        .map(|c| context_lines(&c.after))
        .unwrap_or_default();

    // Body, and the change-line counts, are built together so the header can
    // be computed from what was actually emitted.
    let mut body = String::new();
    let mut old_len = leading.len();
    let mut new_len = leading.len();
    for (line, nl) in &leading {
        push_diff_line(&mut body, ' ', line, *nl);
    }

    for (i, hunk) in hunks.iter().enumerate() {
        if i > 0 {
            let prev = &hunks[i - 1];
            let prev_end = prev.before_range.start + prev.before_range.length;
            let gap = hunk.before_range.start.saturating_sub(prev_end);
            for (line, nl) in gap_lines(prev, hunk, gap) {
                push_diff_line(&mut body, ' ', line, nl);
                old_len += 1;
                new_len += 1;
            }
        }
        for (line, nl) in context_lines(&hunk.removed) {
            push_diff_line(&mut body, '-', line, nl);
            old_len += 1;
        }
        for (line, nl) in context_lines(&hunk.added) {
            push_diff_line(&mut body, '+', line, nl);
            new_len += 1;
        }
    }

    for (line, nl) in &trailing {
        push_diff_line(&mut body, ' ', line, *nl);
        old_len += 1;
        new_len += 1;
    }

    let block_start = first.before_range.start.saturating_sub(leading.len());
    let new_start = (block_start as i64 + new_offset).max(0) as usize;
    // A zero-length side is written as `-0,0` / `+0,0`; otherwise the start is
    // the first line the block covers.
    let old_start = if old_len == 0 { 0 } else { block_start.max(1) };
    let new_start = if new_len == 0 { 0 } else { new_start.max(1) };

    let mut scope = String::new();
    if let Some(s) = &first.semantic.enclosing_scope {
        if let Some(func) = &first.semantic.enclosing_function {
            scope = format!(" {}::{}", s, func);
        } else {
            scope = format!(" {}", s);
        }
    } else if let Some(func) = &first.semantic.enclosing_function {
        scope = format!(" {}", func);
    }

    // The abbreviated id, with no ellipsis: it is unambiguous over this diff,
    // so it can be copied straight out of the header into a selector.
    result.push_str(&format!(
        "@@ -{},{} +{},{} @@{} [{}]\n",
        old_start, old_len, new_start, new_len, scope, first.short_id
    ));
    result.push_str(&body);
    (result, new_len as i64 - old_len as i64)
}

fn render_text_summary_output(output: &ListSummaryOutput) -> String {
    let mut lines = Vec::new();

    if let Some(groups) = &output.groups {
        for (index, group) in groups.iter().enumerate() {
            let name = if group.name == "." || group.name.is_empty() {
                "<root>"
            } else {
                group.name.as_str()
            };
            lines.push(format!("{}:", name));
            format_summary_text(&mut lines, &group.files);
            if index + 1 < groups.len() {
                lines.push(String::new());
            }
        }
    } else if let Some(files) = &output.files {
        format_summary_text(&mut lines, files);
    }

    if lines.is_empty() {
        return String::new();
    }

    let mut output = lines.join("\n");
    output.push('\n');
    output
}

fn format_files_text(lines: &mut Vec<String>, files: &[FileEntry]) {
    for file in files {
        lines.push(format_file_header(file));
        for hunk in &file.hunks {
            let mut hunk_line = format!(
                "  hunk {} {} {} (before {}+{} after {}+{})",
                hunk.index,
                hunk.hunk_type,
                hunk.short_id,
                hunk.before_range.start,
                hunk.before_range.length,
                hunk.after_range.start,
                hunk.after_range.length,
            );
            if let Some(scope) = &hunk.semantic.enclosing_scope {
                if let Some(func) = &hunk.semantic.enclosing_function {
                    hunk_line.push_str(&format!(" in {}::{}", scope, func));
                } else {
                    hunk_line.push_str(&format!(" in {}", scope));
                }
            } else if let Some(func) = &hunk.semantic.enclosing_function {
                hunk_line.push_str(&format!(" in {}", func));
            }
            lines.push(hunk_line);
            if !hunk.removed.is_empty() {
                for line in hunk.removed.lines() {
                    lines.push(format!("    - {}", line));
                }
            }
            if !hunk.added.is_empty() {
                for line in hunk.added.lines() {
                    lines.push(format!("    + {}", line));
                }
            }
        }
    }
}

fn format_summary_text(lines: &mut Vec<String>, files: &[FileSummary]) {
    for file in files {
        let mut line = format!(
            "{} {} ({} hunks)",
            status_char(&file.status),
            file.path,
            file.hunk_count
        );
        if let Some(rename) = &file.rename {
            line.push_str(&format!(" ({} -> {})", rename.from, rename.to));
        }
        if file.binary == Some(true) {
            line.push_str(" [binary]");
        }
        if file.symlink == Some(true) {
            line.push_str(SYMLINK_MARKER);
        }
        if file.truncated == Some(true) {
            line.push_str(" [truncated]");
        }
        if let Some(mode) = &file.mode {
            line.push_str(&format_mode_change(mode));
        }
        lines.push(line);
    }
}

fn format_file_header(file: &FileEntry) -> String {
    let mut header = format!("{} {}", status_char(&file.status), file.path);
    if let Some(rename) = &file.rename {
        header.push_str(&format!(" ({} -> {})", rename.from, rename.to));
    }
    if file.binary == Some(true) {
        header.push_str(" [binary]");
    }
    if file.symlink == Some(true) {
        header.push_str(SYMLINK_MARKER);
    }
    if file.truncated == Some(true) {
        header.push_str(" [truncated]");
    }
    if let Some(mode) = &file.mode {
        header.push_str(&format_mode_change(mode));
    }
    header
}

/// A retargeted link shows up as a modified file with no hunks, which on its
/// own reads as a bug in this tool. Every other hunkless change here announces
/// itself -- `[binary]`, `[mode ...]`, `(src -> dst)` -- so this one does too,
/// and says in the same breath that there is no half of it to pick.
const SYMLINK_MARKER: &str = " [symlink, whole-file only]";

/// A mode change cannot be selected, so say so where it is reported.
fn format_mode_change(mode: &ModeChange) -> String {
    format!(" [mode {} -> {}, not selectable]", mode.from, mode.to)
}

fn status_char(status: &str) -> &'static str {
    match status {
        "modified" => "M",
        "added" => "A",
        "removed" => "D",
        "renamed" => "R",
        "copied" => "C",
        _ => "?",
    }
}

/// Select hunks (called by jj --tool)
pub fn select(left: &str, right: &str) -> Result<()> {
    let spec_path = std::env::var("JJ_HUNK_SELECTION").ok();

    let mut spec = if let Some(path) = spec_path {
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read spec from {}", path))?;
        Spec::from_str(&content)?
    } else {
        // No selection = keep everything
        return Ok(());
    };
    // `select` is reached both through a driving verb, which has checked this
    // already, and through a bare `jj --tool=jj-hunk`, which has not: on that
    // path the spec is whatever the user put in `JJ_HUNK_SELECTION` and this
    // is the only check it gets.
    validate_spec_paths(&spec)?;

    let left_path = Path::new(left);
    let right_path = Path::new(right);

    // Get all files in both directories
    let left_files = list_files(left_path);
    let right_files = list_files(right_path);
    let all_files: HashSet<_> = left_files.union(&right_files).cloned().collect();

    // Both spellings of every path, because the spec speaks one and the
    // directories jj materialised are laid out in the other.
    let frame = PathFrame::discover();
    let files: Vec<SelectPath> = all_files.iter().map(|path| frame.resolve(path)).collect();
    let by_display: HashMap<&str, &SelectPath> = files
        .iter()
        .map(|file| (file.display.as_str(), file))
        .collect();

    // `select` normally reads a spec the driving verb already re-keyed, but on
    // the raw `jj --tool=jj-hunk` path there is no driving verb and the spec is
    // whatever the user wrote. `fs` is exactly the root-relative spelling, so
    // the same rule applies here with no frame conversion needed.
    let frame_pairs: Vec<(String, String)> = files
        .iter()
        .map(|file| (file.display.clone(), file.fs.clone()))
        .collect();
    adopt_spec_frame(&mut spec, &frame_pairs);

    // jj hands a rename to the tool as two unrelated paths: the old one in
    // `left`, the new one in `right`. Only the new one is named in the spec,
    // so the old one has to be resolved through the entry that claims it.
    let mut rename_sources: HashMap<&str, &str> = HashMap::new();
    for (path, file_spec) in &spec.files {
        if let Some(from) = file_spec.source_path() {
            if from != path {
                rename_sources.insert(from, path.as_str());
            }
        }
    }

    // Whether each named file ended up keeping any of its change. A rename
    // source can only be resolved once its target has been decided.
    let mut kept: HashMap<&str, bool> = HashMap::new();

    for file in &files {
        let Some(file_spec) = spec.files.get(&file.display) else {
            continue;
        };
        // A `from` is written in the spec's frame like every other path, and
        // it has to name a file that is really there. Resolving it through the
        // file union rather than joining it onto `left` means a stale or
        // mis-framed one falls back to "same path" instead of pointing at
        // whatever happens to sit at that name.
        let source = file_spec
            .source_path()
            .filter(|from| *from != file.display.as_str())
            .and_then(|from| by_display.get(from))
            .map(|found| found.fs.as_str());

        let keeps_change = match file_spec {
            FileSpec::Action {
                action: Action::Keep,
                ..
            } => true,
            FileSpec::Action {
                action: Action::Reset,
                ..
            } => {
                reset_file(left_path, right_path, &file.fs)?;
                false
            }
            FileSpec::Selection(selection) => apply_hunk_selection(
                left_path,
                right_path,
                source,
                file,
                &selection.to_selection(),
            )?,
        };
        kept.insert(file.display.as_str(), keeps_change);
    }

    for file in &files {
        if spec.files.contains_key(&file.display) {
            continue;
        }

        // A path that some entry claims as its rename source, and that is
        // really gone from the right side, follows that entry: a kept rename
        // leaves it deleted, a reset one puts it back. (A copy leaves the
        // source in place on the right, so it takes the default action like
        // any other file.)
        if let Some(target) = rename_sources.get(file.display.as_str()) {
            if !right_files.contains(&file.fs) {
                if !kept.get(target).copied().unwrap_or(false) {
                    reset_file(left_path, right_path, &file.fs)?;
                }
                continue;
            }
        }

        if spec.default == DefaultAction::Reset {
            reset_file(left_path, right_path, &file.fs)?;
        }
    }

    Ok(())
}

/// One file's path in both the frames `select` has to straddle.
///
/// jj materialises the two diff directories with **repo-relative** paths, but
/// every path jj *prints* is relative to the **cwd**: `jj diff --summary`, and
/// `self.path().display()` in `SUMMARY_TEMPLATE`, which is where `list` --
/// and so every spec key, and every `file:` pattern -- gets its paths from.
///
/// Run from the repo root the two spellings are identical and the difference
/// is invisible. Run from `pkg/` they are not, and conflating them cost the
/// spec both of its footholds at once: the key `pkg/a.txt` is not what any
/// producer wrote, and the hunk ids hash the path, so even a key that matched
/// would have resolved against ids computed over a different string.
struct SelectPath {
    /// The path under `left`/`right`: repo-relative, for filesystem access --
    /// and the string the file's hunk ids are hashed with, because that is the
    /// one spelling every invocation agrees on wherever it was run from.
    fs: String,
    /// The path as jj prints it: cwd-relative. The spec key.
    display: String,
}

/// The characters that separate path components here.
///
/// `\` is a separator on Windows and an ordinary filename character everywhere
/// else, so it cannot be one of these unconditionally: a Unix file really named
/// `back\slash.txt` would be read as two components, and both its spec key and
/// its hunk ids would come out naming a file that does not exist.
fn path_separators() -> &'static [char] {
    if cfg!(windows) {
        &['/', '\\']
    } else {
        &['/']
    }
}

/// Translates jj's materialised paths into the frame every producer speaks.
struct PathFrame {
    /// The cwd relative to the workspace root, one component per element.
    /// Empty when `select` is running at the root, which makes every
    /// translation the identity.
    prefix: Vec<String>,
}

impl PathFrame {
    /// jj runs a merge tool with its own cwd, so the frame is discovered from
    /// the process rather than passed in. A cwd that is not inside a workspace
    /// leaves the prefix empty, which is what `select` did before it knew
    /// about frames at all.
    fn discover() -> Self {
        Self {
            prefix: workspace_prefix().unwrap_or_default(),
        }
    }

    fn resolve(&self, fs_path: &str) -> SelectPath {
        SelectPath {
            display: self.to_display(fs_path),
            fs: fs_path.to_string(),
        }
    }

    /// The same relative path jj would print for `fs_path`: the part below the
    /// cwd, prefixed with one `..` per directory the cwd has to climb out of.
    fn to_display(&self, fs_path: &str) -> String {
        if self.prefix.is_empty() {
            return fs_path.to_string();
        }

        let parts: Vec<_> = Path::new(fs_path).components().collect();
        let shared = self
            .prefix
            .iter()
            .zip(&parts)
            .take_while(|(dir, part)| part.as_os_str() == dir.as_str())
            .count();

        let mut display = PathBuf::new();
        for _ in shared..self.prefix.len() {
            display.push("..");
        }
        for part in &parts[shared..] {
            display.push(part.as_os_str());
        }
        display.to_string_lossy().into_owned()
    }

    /// The inverse of [`PathFrame::to_display`]: the workspace-root-relative
    /// path a cwd-relative one names, always `/`-separated.
    ///
    /// jj prints paths relative to the cwd, so from `sub/deep/` a diff reads
    /// `low.txt`, `../mid.txt` and `../../top.txt` -- three spellings of paths
    /// that are `sub/deep/low.txt`, `sub/mid.txt` and `top.txt` in the only
    /// frame two invocations can both agree on. Resolving the `..` components
    /// is the whole job, and it is why this is not simply `prefix + path`.
    ///
    /// `None` when the path climbs above the workspace root. Nothing jj emits
    /// can -- every file it names lives under the root -- so this answers a
    /// hand-written spec, and the honest answer is that such a path names no
    /// file in the workspace. Clamping at the root instead would silently make
    /// `../../top.txt` from `sub/` mean `top.txt`, quietly resolving a
    /// malformed entry onto a real file.
    fn to_root(&self, display_path: &str) -> Option<String> {
        if self.prefix.is_empty() {
            return Some(display_path.replace(std::path::MAIN_SEPARATOR, "/"));
        }

        let mut parts: Vec<&str> = self.prefix.iter().map(String::as_str).collect();
        for component in display_path.split(path_separators()) {
            match component {
                "" | "." => {}
                ".." => {
                    parts.pop()?;
                }
                name => parts.push(name),
            }
        }
        Some(parts.join("/"))
    }

    /// Why `display_path` cannot name a file in this workspace, if it cannot.
    ///
    /// Only the shape of the path is judged here. Whether a well-formed one is
    /// really in the diff is a different question, asked later and against the
    /// diff itself.
    fn path_problem(&self, display_path: &str) -> Option<String> {
        if display_path.is_empty() {
            return Some("is empty".to_string());
        }
        // No path jj can print holds one, so a key that does was not read off
        // any diff. Refused rather than passed along, because this string is
        // echoed into error messages and listings and a terminal acts on some
        // of them.
        if let Some(c) = display_path.chars().find(|c| (*c as u32) < 0x20) {
            return Some(format!("contains the control character U+{:04X}", c as u32));
        }
        // jj prints every path relative to the cwd, so no producer emits an
        // absolute one and no diff holds one.
        if Path::new(display_path).has_root() {
            return Some("is an absolute path".to_string());
        }

        let mut climb = 0usize;
        let mut seen_a_name = false;
        for component in display_path.split(path_separators()) {
            match component {
                "" | "." => {}
                ".." => {
                    // jj prints normalised paths, so an interior `..` spells a
                    // path that already has a spelling. Two keys for one file
                    // is how one entry starts shadowing another.
                    if seen_a_name {
                        return Some(
                            "has a `..` after a named directory, which is not how \
                             jj spells any path"
                                .to_string(),
                        );
                    }
                    // A leading `..` is ordinary -- from `sub/` jj really does
                    // print `../top.txt` -- so what is refused is where the
                    // climb lands, not that it happened at all.
                    climb += 1;
                    if climb > self.prefix.len() {
                        return Some("climbs above the workspace root".to_string());
                    }
                }
                _ => seen_a_name = true,
            }
        }

        None
    }
}

/// The current directory relative to the workspace root, or `None` when it is
/// not inside one.
///
/// The root is found the way jj finds it: the nearest ancestor holding a `.jj`.
/// Asking `jj` itself would be authoritative, but `select` runs *inside* a jj
/// invocation, and a subprocess is a poor thing to owe an answer to there.
fn workspace_prefix() -> Option<Vec<String>> {
    let cwd = std::env::current_dir().ok()?;
    let mut dir = cwd.as_path();
    let mut prefix: Vec<String> = Vec::new();

    loop {
        if dir.join(".jj").exists() {
            prefix.reverse();
            return Some(prefix);
        }
        // No `.jj` anywhere up to the filesystem root.
        prefix.push(dir.file_name()?.to_string_lossy().into_owned());
        dir = dir.parent()?;
    }
}

fn list_files(dir: &Path) -> HashSet<String> {
    let mut files = HashSet::new();
    if !dir.exists() {
        return files;
    }

    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        // Symlinks must be included. WalkDir does not follow them, so their
        // file_type is `symlink`, not `file` -- filtering on is_file() alone
        // left every symlink out of the union, so no spec could ever select or
        // reset one and its change silently rode along in every commit.
        if entry.file_type().is_file() || entry.file_type().is_symlink() {
            if let Ok(rel) = entry.path().strip_prefix(dir) {
                let name = rel.to_string_lossy().to_string();
                if name != "JJ-INSTRUCTIONS" {
                    files.insert(name);
                }
            }
        }
    }
    files
}

fn reset_file(left: &Path, right: &Path, filepath: &str) -> Result<()> {
    // No entry to restore *to*, so there is nothing to do here -- and every
    // write would land somewhere else entirely.
    let Some(right_file) = contained_join(right, filepath) else {
        return Ok(());
    };
    let right_exists = fs::symlink_metadata(&right_file).is_ok();

    // `symlink_metadata` deliberately does NOT traverse the link. `exists()`
    // does, so a dangling symlink in `left` (jj materialises only the changed
    // files, so a link's target is usually absent) reported "not there" and we
    // deleted a file that was never selected. `fs::copy` traverses too, and
    // wrote the *target's* bytes into the link's path.
    match locate(left, filepath) {
        Some((left_file, meta)) => {
            if let Some(parent) = right_file.parent() {
                fs::create_dir_all(parent)?;
            }
            if right_exists {
                fs::remove_file(&right_file)?;
            }
            if meta.file_type().is_symlink() {
                let target = fs::read_link(&left_file)?;
                symlink_file(&target, &right_file)?;
            } else {
                fs::copy(&left_file, &right_file)?;
            }
        }
        None => {
            if right_exists {
                fs::remove_file(&right_file)?;
            }
        }
    }
    Ok(())
}

/// `dir.join(relative)`, unless the path would lead *through* a symlink.
///
/// Only the last component may be a link; that is an entry in its own right.
/// A link at any earlier component is a way out of `dir` altogether, and jj
/// materialises exactly that shape whenever a commit replaces a directory with
/// a symlink: one side gets `conf` as a link, the other `conf/x.txt` as a file,
/// and every read, unlink and write under `conf/` lands wherever the link
/// points -- outside the repo, at exit 0. A tree cannot hold both entries, so
/// the honest answer is that this path is not in this one.
///
/// `..` and absolute components are refused for the same reason. Nothing here
/// should produce them -- these paths come from `strip_prefix`-ing a walk of
/// the directory itself -- but joining one on is the whole escape in miniature.
fn contained_join(dir: &Path, relative: &str) -> Option<PathBuf> {
    let mut path = dir.to_path_buf();
    let mut components = Path::new(relative).components().peekable();

    while let Some(component) = components.next() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return None;
        }
        path.push(component.as_os_str());
        if components.peek().is_none() {
            break; // the leaf itself may be a link
        }
        if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return None;
        }
    }

    Some(path)
}

/// The entry `relative` names inside `dir`, if there is one.
///
/// `None` covers both "not there" and "not reachable without going through a
/// symlink", which for a tree jj materialised mean the same thing.
fn locate(dir: &Path, relative: &str) -> Option<(PathBuf, fs::Metadata)> {
    let path = contained_join(dir, relative)?;
    let meta = fs::symlink_metadata(&path).ok()?;
    Some((path, meta))
}

#[cfg(unix)]
fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

/// Apply a hunk selection to one file, reporting whether anything was kept.
///
/// `source` is the file's path on the left side when it differs from the spec's
/// own (renames and copies), and like every path under `left`/`right` it is in
/// `SelectPath::fs` form. The spec's hunk ids were computed by diffing
/// `left/<source>` against the right-hand file; joining the target name onto
/// both sides instead recomputes the change as one whole-file insertion under a
/// different id, so nothing matches and the file is written out empty.
///
/// Every id here is recomputed from `file.fs` -- the workspace-root-relative
/// spelling, which is also the frame `list` hashes in. The id hashes the path,
/// so the two sides have to agree on which string that is; recomputing from
/// `file.display` instead tied the id to the cwd, and since jj runs this tool
/// with the cwd it was invoked from, a spec produced at the root stopped
/// resolving the moment it was applied one directory down.
fn apply_hunk_selection(
    left: &Path,
    right: &Path,
    source: Option<&str>,
    file: &SelectPath,
    selection: &HunkSelection,
) -> Result<bool> {
    // `locate`, not `join` plus `exists()`: the latter traverses symlinks, so a
    // dangling link (jj materialises only the changed files, so a link's target
    // is usually absent) reads as "not there", and a link standing where a
    // directory component should be reads as a path in a different tree.
    //
    // A `from` naming something that is not on the left is unusable, so it
    // falls back to the spec's own path rather than reading an empty "before"
    // and concluding the file is new -- that path ends in deleting it.
    let left_entry = source
        .and_then(|from| locate(left, from))
        .or_else(|| locate(left, &file.fs));
    let right_entry = locate(right, &file.fs);

    // A selection that names nothing keeps nothing, and that is decidable
    // without reading a single byte of either side. Deciding it here is what
    // makes `{"ids": []}` -- the documented "keep nothing from this file"
    // spelling -- work on files no hunk selection could ever describe: a
    // binary, a symlink, one whose parent is not valid UTF-8. Reading them
    // first is how an unreadable parent came to be committed as an empty file.
    if selection.is_empty() {
        reset_file(left, right, &file.fs)?;
        return Ok(false);
    }

    // Nothing on the right: the whole file is a single delete hunk. Keep the
    // deletion only if that hunk is selected -- otherwise the file is reset,
    // which for a deletion means putting it back. Returning early here left
    // every deletion in the commit no matter what the spec said.
    let Some((right_file, right_meta)) = right_entry else {
        // A deletion is always same-path, so read the left side at the spec's
        // own path rather than at a `source` a malformed spec might have
        // supplied. An unreadable left side yields no delete hunk, so no id
        // can name one and nothing is kept: the file goes back, which is both
        // the safe direction and the only one available.
        let deleted = locate(left, &file.fs);
        let before = side_text(as_side(&deleted), &file.display).unwrap_or_default();
        let hunks = get_hunks(&file.fs, &before, "");
        let keeps_deletion = !selection.resolve(&hunks)?.is_empty();
        if !keeps_deletion {
            reset_file(left, right, &file.fs)?;
        }
        return Ok(keeps_deletion);
    };

    let before = side_text(as_side(&left_entry), &file.display)?;
    let after = side_text(Some((right_file.as_path(), &right_meta)), &file.display)?;

    let result = apply_selected_hunks(&file.fs, &before, &after, selection)?;

    if result == before {
        // Nothing of this file's change survived the selection. Restoring it
        // from the left side is a byte copy, so it puts back content no text
        // round-trip could reproduce -- and the mode along with it. Writing
        // `result` instead committed whatever `before` had decoded as, and for
        // an added file or a rename target that was a zero-byte file.
        reset_file(left, right, &file.fs)?;
        return Ok(false);
    }

    // A link's change was kept, and the right side already *is* it. There is
    // nothing to write, and writing is the one thing that must not happen:
    // `fs::write` follows the link and puts the text on its target, a path in
    // neither directory jj handed us. (A link is also all-or-nothing in
    // practice -- one side of the diff is empty, so its whole change is a
    // single hunk.)
    if right_meta.file_type().is_symlink() {
        return Ok(true);
    }

    // Something was kept, so the file's change rides along whole -- including a
    // `chmod +x`, which is not a hunk, cannot be selected apart from the
    // content it came with, and has no half to leave behind. Stripping it here
    // discarded it outright under `diffedit`, which keeps no remainder commit.
    write_regular_file(&right_file, &result, &right_meta)?;
    Ok(true)
}

/// One side of the diff as the text its hunk ids were computed over.
///
/// `list` reads both sides with `jj file show`, which prints *nothing* for a
/// symlink: an entry that is a link on one side is described as that side being
/// empty, and the ids the user selects from are hashed over that emptiness.
/// `select` has to read it the same way, or the ids it recomputes name nothing
/// and a selection that looked perfectly good resets the file instead.
///
/// An absent side is empty too -- that is what a newly added file, or the
/// target of a rename, looks like. A side that is *there* but unreadable is
/// neither, and reading it as `""` said it was: `select` then saw a whole-file
/// insertion, matched none of it, and wrote the empty string over the file.
fn side_text(entry: Option<(&Path, &fs::Metadata)>, display: &str) -> Result<String> {
    match entry {
        None => Ok(String::new()),
        Some((_, meta)) if meta.file_type().is_symlink() => Ok(String::new()),
        Some((path, _)) => Ok(read_selectable(path, display)?.unwrap_or_default()),
    }
}

/// Borrow what `locate` returned, for `side_text`.
fn as_side(entry: &Option<(PathBuf, fs::Metadata)>) -> Option<(&Path, &fs::Metadata)> {
    entry.as_ref().map(|(path, meta)| (path.as_path(), meta))
}

/// Read a file as text, telling "not there" apart from "there but unreadable".
///
/// `fs::read_to_string(..).unwrap_or_default()` collapses the two, and `""` is
/// exactly what a newly added file reads as. It also traverses symlinks, so a
/// link in the tree could feed `select` the bytes of a file outside the two
/// directories jj handed it; anything that is not a regular file is refused
/// here instead.
fn read_text(path: &Path) -> Result<Option<String>> {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return Ok(None);
    };
    if !meta.file_type().is_file() {
        anyhow::bail!("{} is not a regular file", path.display());
    }
    fs::read_to_string(path)
        .map(Some)
        .with_context(|| format!("Failed to read {}", path.display()))
}

/// `read_text`, with the advice that goes with failing to read one side of a
/// hunk selection.
///
/// Reading lossily instead is not an option: `select` writes this text back
/// out, so the replacement characters would become the committed file.
fn read_selectable(path: &Path, display: &str) -> Result<Option<String>> {
    read_text(path).with_context(|| {
        format!(
            "Failed to read {} as text. A file that is not valid UTF-8 cannot \
             be selected hunk-wise; keep or reset it whole instead \
             (`{{\"action\": \"keep\"}}`, `{{\"action\": \"reset\"}}`, or an \
             empty selection `{{\"ids\": []}}`).",
            display
        )
    })
}

/// Replace `path`'s contents, never through a symlink, keeping the mode
/// `was` describes.
///
/// `fs::write` opens with `O_TRUNC` and follows links, so a link committed in
/// the repo -- `link.txt -> ../../victim` -- sent the new contents to its
/// target: a file in neither directory jj handed the tool, rewritten at exit 0.
/// Unlinking first means the write lands on a fresh regular file at exactly
/// this path whatever was sitting there, so no future caller can reintroduce
/// that by reaching this function with a link.
fn write_regular_file(path: &Path, contents: &str, was: &fs::Metadata) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            let context = format!("Failed to replace {}", path.display());
            return Err(anyhow::Error::new(e).context(context));
        }
    }
    fs::write(path, contents).with_context(|| format!("Failed to write {}", path.display()))?;

    // The unlink took the file's mode with it and `fs::write` created the
    // replacement from the umask, so it has to be put back explicitly.
    restore_mode(path, was)
}

/// Give `path` the permissions `was` recorded.
#[cfg(unix)]
fn restore_mode(path: &Path, was: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = was.permissions().mode() & 0o7777;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("Failed to restore mode on {}", path.display()))
}

#[cfg(not(unix))]
fn restore_mode(_path: &Path, _was: &fs::Metadata) -> Result<()> {
    Ok(())
}

fn resolve_spec_input(spec: Option<&str>, spec_file: Option<&str>) -> Result<String> {
    if let Some(path) = spec_file {
        if path.is_empty() {
            anyhow::bail!("Spec file path is empty");
        }
        return fs::read_to_string(path)
            .with_context(|| format!("Failed to read spec file {}", path));
    }

    let spec = spec.ok_or_else(|| anyhow::anyhow!("Spec is required (or use --spec-file)"))?;
    if spec == "-" {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .context("Failed to read spec from stdin")?;
        if buffer.trim().is_empty() {
            anyhow::bail!("Spec from stdin is empty");
        }
        return Ok(buffer);
    }

    Ok(spec.to_string())
}

/// Refuse spec keys that cannot name a file in this workspace.
///
/// A spec key is a path an agent wrote, and every consumer of one looks it up
/// in a set of paths that came off the diff -- `spec_decision`, `select` and
/// `validate_spec_resolves` all reach for `spec.files.get(...)`. That is what
/// makes a key like `/etc/passwd`, or one climbing out of the repo into a
/// private key, harmless today: no such key is in the set, so nothing happens.
///
/// Harmless is not the same as refused, and the distance between them is one
/// future lookup that joins a key onto a directory instead of comparing it
/// against the set. `select` already refuses such a join from the other side,
/// in `contained_join`; this refuses it before one is ever built, for every
/// command that reads a spec rather than only for the one that writes files.
///
/// Refusing it here also makes the answer the same shape whatever the entry
/// says. `validate_spec_resolves` reports an unknown path only when the entry
/// means to keep something and nothing else in the spec keeps anything real --
/// deliberately, because a stale allowlist entry is not an error. So a
/// traversal key sitting under `{"action": "reset"}`, or beside one entry that
/// does resolve, was accepted in silence at exit 0. That is the right answer
/// for a path that is merely absent from this diff, and the wrong one for a
/// path that could not have come from any diff.
///
/// Nothing here reads the diff: this judges the shape of the key, and can
/// answer before a revision is even resolved.
pub(crate) fn validate_spec_paths(spec: &Spec) -> Result<()> {
    let frame = PathFrame::discover();
    let mut problems: Vec<String> = Vec::new();

    // Reported through `escape_debug`, which leaves an ordinary path exactly as
    // written. A path only reaches this list by being malformed, and one of the
    // ways it can be malformed is by carrying the control characters that would
    // let it rewrite the very line reporting it.
    let show = |path: &str| path.escape_debug().to_string();

    for (key, file_spec) in &spec.files {
        if let Some(reason) = frame.path_problem(key) {
            problems.push(format!("{}: {reason}", show(key)));
        }
        // A rename's `from` is a spec key in all but position: written by the
        // same hand, in the same frame, and resolved by `select` the same way.
        // `validate_spec_resolves` never looks at it, so without this it is the
        // one path in a spec that nothing checks at all.
        if let Some(from) = file_spec.source_path() {
            if let Some(reason) = frame.path_problem(from) {
                problems.push(format!(
                    "{}: its `from` {} {reason}",
                    show(key),
                    show(from)
                ));
            }
        }
    }

    if problems.is_empty() {
        return Ok(());
    }

    problems.sort();
    anyhow::bail!(
        "spec names paths that cannot be in this workspace:\n  {}\n\
         A spec key is a path the way jj prints one: relative to the directory \
         the command runs in, and inside the workspace.",
        problems.join("\n  ")
    );
}

/// One way a spec entry failed to name something the diff contains.
///
/// Held as a value rather than a formatted line so the same finding can be
/// both read aloud and handed over structured. Rendering it to a string first
/// and recovering the path afterwards would mean parsing our own prose, which
/// is what the error codes exist to spare a caller.
enum SpecProblem {
    RenamedTo { path: String, new_path: String },
    NoSuchPath { path: String },
    NoSuchId { path: String, id: String },
    AmbiguousId { path: String, id: String, count: usize },
    NoSuchIndex { path: String, index: usize, hunk_count: usize },
}

impl SpecProblem {
    /// The line this problem contributes to the human message. Unchanged from
    /// when these were built as strings in place.
    fn message(&self) -> String {
        match self {
            SpecProblem::RenamedTo { path, new_path } => format!(
                "{path}: renamed to {new_path} in this diff -- \
                 file the entry under {new_path} instead"
            ),
            SpecProblem::NoSuchPath { path } => format!("{path}: no such path in the diff"),
            SpecProblem::NoSuchId { path, id } => format!("{path}: no hunk with id {id}"),
            SpecProblem::AmbiguousId { path, id, count } => format!(
                "{path}: id {id} is ambiguous, it names {count} hunks -- use a longer prefix"
            ),
            SpecProblem::NoSuchIndex { path, index, hunk_count } => {
                format!("{path}: no hunk with index {index} (file has {hunk_count})")
            }
        }
    }

    fn to_json(&self) -> serde_json::Value {
        match self {
            SpecProblem::RenamedTo { path, new_path } => {
                serde_json::json!({"kind": "renamed", "path": path, "renamed_to": new_path})
            }
            SpecProblem::NoSuchPath { path } => {
                serde_json::json!({"kind": "no-such-path", "path": path})
            }
            SpecProblem::NoSuchId { path, id } => {
                serde_json::json!({"kind": "no-such-id", "path": path, "id": id})
            }
            SpecProblem::AmbiguousId { path, id, count } => {
                serde_json::json!({"kind": "ambiguous-id", "path": path, "id": id, "count": count})
            }
            SpecProblem::NoSuchIndex { path, index, hunk_count } => serde_json::json!({
                "kind": "no-such-index", "path": path, "index": index, "hunk_count": hunk_count
            }),
        }
    }
}

/// Check that a spec actually refers to the diff it will be applied to.
///
/// `Spec::selects_nothing` is structural: a non-empty id list satisfies it even
/// when no such hunk exists. So a typo'd path or a stale id passed the guard,
/// selected nothing, and produced an empty commit at exit 0 -- the exact
/// failure that guard was added to prevent.
///
/// This is about *reference*, not about emptiness, which is why `--allow-empty`
/// does not gate it. An entry that deliberately keeps nothing (`"ids": []`) on
/// a path that is in the diff refers to something real and is fine; a name that
/// matches nothing in the diff is a different thing entirely.
///
/// Three kinds of mismatch, treated differently:
///
/// - A bad id, a bad index, or a key that is only the left-hand side of a
///   rename: the file is right there, so the entry meant a specific thing and
///   got it wrong. Always reported.
/// - An absent path under an entry that names hunks: those ids came off a real
///   diff, so this is not an allowlist entry that is merely idle -- it asked
///   for hunks that are not here. Always reported.
/// - An absent path under a bare `{"action": "keep"}`: reported only when the
///   spec names nothing that IS in the diff. A checked-in or script-generated
///   spec lists a stable allowlist, and most of it is legitimately absent from
///   any one diff; but a spec none of whose paths appear was written against
///   some other diff, and would quietly select nothing.
fn validate_spec_resolves(
    spec: &Spec,
    file_hunks: &[FileHunks],
    target: &DiffTarget,
) -> Result<()> {
    // Keyed by primary path only. Aliasing a rename's left-hand path to the
    // same entry made a spec keyed by the old path validate and then do the
    // opposite of what it said: `fill_rename_sources` and `select` both look
    // the entry up under the NEW path, so nothing was filled in, the deletion
    // branch ran, and the rename was reverted. For a copy the old path is a
    // diff entry in its own right, so the alias could also shadow a real file
    // -- whichever jj happened to list second won.
    let known: HashMap<&str, &FileHunks> = file_hunks
        .iter()
        .map(|fh| (fh.path.as_str(), fh))
        .collect();

    // Paths that exist on the left of this diff only as the source of a rename
    // or copy, mapped to the path the spec has to name instead.
    let mut renamed_from: HashMap<&str, &str> = HashMap::new();
    for fh in file_hunks {
        if let Some(rename) = fh.rename.as_ref().filter(|r| r.from != fh.path) {
            renamed_from
                .entry(rename.from.as_str())
                .or_insert(fh.path.as_str());
        }
    }

    let mut problems: Vec<SpecProblem> = Vec::new();
    // Held back until the whole spec has been walked: whether an absent path
    // matters depends on what the rest of the spec turned out to name.
    let mut unresolved: Vec<SpecProblem> = Vec::new();
    // Whether any entry that means to keep something names a path this diff
    // actually contains.
    let mut keeps_a_real_path = false;

    for (path, file_spec) in &spec.files {
        // An entry that keeps nothing by construction -- `{"action": "reset"}`,
        // or a blanked-out `"ids": []` -- cannot produce the empty commit this
        // guards against, so an unknown path there is harmless either way.
        let intends_to_keep = match file_spec {
            FileSpec::Action { action, .. } => *action == Action::Keep,
            FileSpec::Selection(selection) => {
                !selection.hunks.is_empty() || !selection.ids.is_empty()
            }
        };

        let Some(fh) = known.get(path.as_str()) else {
            match renamed_from.get(path.as_str()) {
                // Whatever this entry meant, `select` cannot carry it out under
                // this key: it matches nothing on the right, and resetting it
                // would resurrect the file the rename moved away. The hunk ids
                // are printed under the new path, so that is the key to use.
                Some(new_path) => problems.push(SpecProblem::RenamedTo {
                    path: path.clone(),
                    new_path: new_path.to_string(),
                }),
                None if intends_to_keep => match file_spec {
                    // Ids and indices are read off a real diff, so an entry
                    // carrying them is not a stable allowlist that has gone
                    // stale: it names hunks that were somewhere once and are
                    // not here now. Reported no matter what else resolved --
                    // pooling it with the allowlist case let one entry that
                    // still matches mask every entry that no longer does, and
                    // the commit then held a subset of what the spec asked for.
                    FileSpec::Selection(_) => {
                        problems.push(SpecProblem::NoSuchPath { path: path.clone() })
                    }
                    FileSpec::Action { .. } => {
                        unresolved.push(SpecProblem::NoSuchPath { path: path.clone() })
                    }
                },
                None => {}
            }
            continue;
        };
        keeps_a_real_path |= intends_to_keep;

        let FileSpec::Selection(selection) = file_spec else {
            continue;
        };

        let ids = selection.ids.iter().map(String::as_str).chain(
            selection
                .hunks
                .iter()
                .filter_map(|selector| match selector {
                    HunkSelector::Id(id) => Some(id.as_str()),
                    HunkSelector::Index(_) => None,
                }),
        );
        for id in ids {
            // Prefix matching, so a spec may name hunks by the abbreviated ids
            // `list` prints. Ambiguity is caught here rather than left to
            // `select`, which sees one file at a time and would just keep both.
            let matched = fh.hunks.iter().filter(|hunk| diff::id_matches(id, &hunk.id)).count();
            match matched {
                0 => problems.push(SpecProblem::NoSuchId {
                    path: path.clone(),
                    id: id.to_string(),
                }),
                1 => {}
                n => problems.push(SpecProblem::AmbiguousId {
                    path: path.clone(),
                    id: id.to_string(),
                    count: n,
                }),
            }
        }

        for selector in &selection.hunks {
            let HunkSelector::Index(index) = selector else {
                continue;
            };
            if !fh.hunks.iter().any(|hunk| hunk.index == *index) {
                problems.push(SpecProblem::NoSuchIndex {
                    path: path.clone(),
                    index: *index,
                    hunk_count: fh.hunks.len(),
                });
            }
        }
    }

    // An allowlist that names a stable set of paths is a legitimate reusable
    // spec, and most of it is legitimately absent from any one diff -- so an
    // absent allowlist path is only worth reporting when nothing else in the
    // spec keeps anything real. A spec in that state selects nothing at all,
    // which is the silent empty commit this whole check exists to catch; a spec
    // that keeps something has simply outlived a few of its entries.
    if !keeps_a_real_path && spec.default != DefaultAction::Keep {
        problems.append(&mut unresolved);
    }

    if !problems.is_empty() {
        // By the rendered line, which is what this sorted before the findings
        // became values -- so the message reads in exactly the order it did.
        problems.sort_by_key(SpecProblem::message);
        // The listing is named after the target rather than hardcoded, because
        // `restore` edits the diff the other way round: an id copied from
        // `list -r REV` cannot resolve there, and a fixed hint would send the
        // reader straight back to the listing that produced the bad id.
        //
        // `--allow-empty` is deliberately not offered as a way out: it says an
        // empty result is acceptable, not that the names in the spec need not
        // exist, and suggesting it here is what taught people to silence this
        // check instead of fixing the spec.
        let lines: Vec<String> = problems.iter().map(SpecProblem::message).collect();
        return Err(CodedError::new(
            errors::PATH_NOT_IN_DIFF,
            format!(
                "spec does not resolve against the diff:\n  {}\n\
                 Those entries do not name exactly what they meant to. Check them \
                 against `{} --spec-template`.",
                lines.join("\n  "),
                target.listing_command()
            ),
        )
        .with(
            "problems",
            problems.iter().map(SpecProblem::to_json).collect::<Vec<_>>(),
        )
        .with("listing_command", target.listing_command())
        .into());
    }

    Ok(())
}

/// Attach the left-hand path of every rename the spec names but does not
/// describe. Returns whether anything was filled in.
///
/// `select` is handed two directories and nothing else, so it cannot tell that
/// `right/dst` used to be `left/src`. Without the source path it recomputes the
/// change as one whole-file insertion, matches none of the spec's hunk ids, and
/// writes an empty file. Filling this in here means a hand-written spec -- the
/// usual "copy an id out of `list`" shape -- does not have to know about it.
fn fill_rename_sources(spec: &mut Spec, file_hunks: &[FileHunks]) -> bool {
    let mut filled = false;

    for fh in file_hunks {
        let Some(source) = rename_source(&fh.rename, &fh.path) else {
            continue;
        };
        let Some(file_spec) = spec.files.get_mut(&fh.path) else {
            continue;
        };
        if file_spec.source_path().is_some() {
            continue; // already stated; do not second-guess it
        }
        file_spec.set_source_path(source);
        filled = true;
    }

    filled
}

/// Every path a spec could name in this diff, paired with the
/// workspace-root-relative spelling of the same file.
fn frame_pairs(file_hunks: &[FileHunks], frame: &PathFrame) -> Vec<(String, String)> {
    file_hunks
        .iter()
        .flat_map(FileHunks::all_paths)
        .filter_map(|display| {
            frame
                .to_root(display)
                .map(|root| (display.to_string(), root))
        })
        .collect()
}

/// Re-key a spec written in some other directory's frame into this one.
///
/// A spec key is whatever its producer printed, and every producer here prints
/// cwd-relative paths. So a spec generated at the repo root keys a file
/// `sub/mid.txt`, and one directory down that same file is called `mid.txt`:
/// the keys match nothing, and the whole spec fails *before* a single hunk id
/// is consulted -- `sub/mid.txt: no such path in the diff`, said of a path
/// plainly in the diff. Fixing the ids alone would not have made a
/// root-generated spec usable from a subdirectory, because resolution never
/// got as far as an id.
///
/// The workspace-root-relative spelling is the one that means the same file
/// from every directory, so a key that names no local path but does name a
/// real file in that frame is rewritten to the local spelling. Everything
/// downstream -- `spec_decision`, `validate_spec_resolves`, `select` -- goes on
/// looking paths up exactly as it did.
///
/// Two orderings are deliberate:
///
/// - A key that already names a diff path is never rewritten. It means today
///   what it meant before this function existed, and only that precedence
///   guarantees no spec that resolves now starts resolving to something else.
/// - A rewrite that would land on a key the spec already holds is dropped.
///   Merging two entries into one would silently discard whichever lost.
///
/// At the repo root a path *is* its own root-relative spelling, so this is the
/// identity there and no root-generated spec changes meaning. The reverse
/// direction stays broken on purpose: a spec written in `sub/` can key a file
/// `../top.txt`, which names nothing from anywhere else, so such a spec is
/// simply not portable and says so by failing to resolve.
fn adopt_spec_frame(spec: &mut Spec, paths: &[(String, String)]) -> bool {
    let local: HashSet<&str> = paths.iter().map(|(display, _)| display.as_str()).collect();
    let by_root: HashMap<&str, &str> = paths
        .iter()
        .map(|(display, root)| (root.as_str(), display.as_str()))
        .collect();

    let translate = |path: &str| -> Option<String> {
        if local.contains(path) {
            return None;
        }
        by_root.get(path).map(|display| (*display).to_string())
    };

    let mut changed = false;

    // A rename's `from` is a path in the spec's frame like any other, so it
    // travels with the key. Left behind, it would name nothing on the left and
    // `select` would recompute the rename as a whole-file insertion.
    for file_spec in spec.files.values_mut() {
        let Some(from) = file_spec.source_path().map(str::to_string) else {
            continue;
        };
        let Some(local_from) = translate(&from) else {
            continue;
        };
        file_spec.set_source_path(local_from);
        changed = true;
    }

    let rewrites: Vec<(String, String)> = spec
        .files
        .keys()
        .filter_map(|key| translate(key).map(|local| (key.clone(), local)))
        .filter(|(_, local)| !spec.files.contains_key(local))
        .collect();
    for (foreign, local) in rewrites {
        if let Some(file_spec) = spec.files.remove(&foreign) {
            spec.files.insert(local, file_spec);
            changed = true;
        }
    }

    changed
}

pub(crate) fn run_jj_with_selection(
    args: &[&str],
    spec: Option<&str>,
    spec_file: Option<&str>,
    rev: Option<&str>,
    allow_empty: bool,
) -> Result<()> {
    run_jj_with_selection_on(args, spec, spec_file, &DiffTarget::rev(rev), allow_empty)
}

/// As `run_jj_with_selection`, but for a command whose diff editor is shown
/// something other than `jj diff -r REV`.
///
/// `target` must be the view jj will hand to `select`, because that is the text
/// the spec's hunk ids are hashes of. Naming the wrong one does not misfire
/// loudly; it just matches nothing.
fn run_jj_with_selection_on(
    args: &[&str],
    spec: Option<&str>,
    spec_file: Option<&str>,
    target: &DiffTarget,
    allow_empty: bool,
) -> Result<()> {
    let raw_spec = resolve_spec_input(spec, spec_file)?;
    let is_hunkset = hunkset::is_hunkset(&raw_spec);
    let mut spec_content = if is_hunkset {
        // Never truncated: this spec is about to mutate history, and `select`
        // works from the files whole.
        evaluate_hunkset(&raw_spec, target, Truncation::NONE)?
    } else {
        raw_spec.clone()
    };

    if let Ok(mut parsed) = Spec::from_str(&spec_content) {
        // First, and before `adopt_spec_frame` rewrites anything, so the
        // refusal names the key as it was written.
        validate_spec_paths(&parsed)?;
        // Refuse to mutate history with a selection that keeps nothing: jj
        // would carry it out and exit 0, which hides a typo'd selector from any
        // script driving this. What it would carry out differs per verb -- an
        // empty commit for `split`, a discarded diff for `diffedit`, nothing at
        // all for `restore` -- so the message below stops at the one thing that
        // holds everywhere.
        if !allow_empty && parsed.selects_nothing() {
            return Err(CodedError::new(
                errors::EMPTY_SELECTION,
                format!(
                    "selection matched no hunks: {}\n\
                     An empty selection is nearly always a mistyped selector \
                     rather than an intent, so it is refused.\n\
                     Check it against `{} --spec ...`, or pass --allow-empty if \
                     that is what you meant.",
                    raw_spec.trim(),
                    target.listing_command()
                ),
            )
            .with("selector", raw_spec.trim())
            .with("listing_command", target.listing_command())
            .into());
        }

        // A hunkset spec is built from the diff it was evaluated against, so
        // its paths and ids resolve by construction and it already carries
        // every rename source. A hand-written one needs both passes.
        if !is_hunkset {
            // `Mark`, not `Skip`: a binary file has no hunks but is still a
            // legitimate spec target via `{"action": "keep"}`.
            let file_hunks = load_file_hunks(target, BinaryMode::Mark, Truncation::NONE)?;
            // Before validating, not after: a spec generated one directory up
            // is not wrong, it is written in another frame, and reporting every
            // one of its paths as absent would be the wrong complaint.
            let frame = PathFrame::discover();
            let mut rewritten = adopt_spec_frame(&mut parsed, &frame_pairs(&file_hunks, &frame));
            // Not gated on `allow_empty`. That flag says an empty *result* is
            // acceptable; it never meant "do not check whether what I wrote
            // refers to anything". Gating this too meant that passing it once
            // -- because one entry was legitimately blanked out -- switched off
            // typo detection for every other entry in the same spec, and a
            // mistyped path or stale id then produced an empty commit at exit
            // 0, which is precisely what this check exists to make loud.
            validate_spec_resolves(&parsed, &file_hunks, target)?;
            rewritten |= fill_rename_sources(&mut parsed, &file_hunks);
            if rewritten {
                spec_content = serde_json::to_string(&parsed)
                    .context("failed to re-serialize spec for the tool")?;
            }
        }
    }

    let temp_spec = TempSpec::create(&spec_content)?;

    let config_args = jj_hunk_tool_config_args()?;

    let status = Command::new("jj")
        .args(&config_args)
        .args(args)
        .env("JJ_HUNK_SELECTION", temp_spec.path())
        .status()
        .context("Failed to run jj")?;

    // `temp_spec` is removed by its `Drop`, so it goes whichever way we leave
    // here -- including the `?` above, which used to return past the cleanup
    // and leave the file behind whenever jj could not be spawned.
    if !status.success() {
        anyhow::bail!("jj command failed");
    }
    Ok(())
}

/// Attempts to find a free name before giving up. A collision needs the random
/// suffix to repeat, so more than a couple means something else is wrong.
const TEMP_SPEC_ATTEMPTS: usize = 16;

/// The spec file handed to the `select` child process through the environment.
///
/// The path used to be `<tmpdir>/jj-hunk-<pid>.spec`. A pid is a small and
/// guessable number, so anyone able to write to the temp directory could
/// pre-create that path as a symlink: the write then landed on the link's
/// target, and the cleanup unlinked only the link, leaving the victim file
/// overwritten (CWE-377). The name is now unpredictable and the file is created
/// with `O_CREAT|O_EXCL`, which refuses to open anything already at the path
/// rather than following it.
#[derive(Debug)]
struct TempSpec {
    path: PathBuf,
}

impl TempSpec {
    fn create(contents: &str) -> Result<Self> {
        let dir = std::env::temp_dir();

        for _ in 0..TEMP_SPEC_ATTEMPTS {
            let path = dir.join(format!("jj-hunk-{:016x}.spec", random_suffix()));

            return match create_new_exclusive(&path) {
                Ok(mut file) => {
                    // Own the path before the first write, so a failure part
                    // way through still cleans up after itself.
                    let temp = Self { path };
                    file.write_all(contents.as_bytes()).with_context(|| {
                        format!("Failed to write spec to {}", temp.path.display())
                    })?;
                    Ok(temp)
                }
                // Someone else holds that name. Pick another.
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => Err(anyhow::Error::new(e)
                    .context(format!("Failed to create a spec file in {}", dir.display()))),
            };
        }

        anyhow::bail!(
            "Failed to create a spec file in {} after {} attempts",
            dir.display(),
            TEMP_SPEC_ATTEMPTS
        )
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempSpec {
    fn drop(&mut self) {
        // Best effort: reporting a cleanup failure here would displace whatever
        // error we are already on our way out with.
        let _ = fs::remove_file(&self.path);
    }
}

/// Create `path` for writing, failing if anything is already there.
///
/// `create_new` is `O_CREAT|O_EXCL`, which is the part that matters: it does
/// not follow a symlink sitting at `path`, it refuses. On unix the file is also
/// created 0600 from the start, rather than being widened and narrowed again.
fn create_new_exclusive(path: &Path) -> std::io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // The spec names paths in a repo; it is nobody else's business.
        options.mode(0o600);
    }
    options.open(path)
}

/// An unguessable suffix for the temp file name.
///
/// `RandomState` is seeded from the OS, which is what makes the result
/// unpredictable; the pid, the clock and the counter only keep successive calls
/// in one process apart.
fn random_suffix() -> u64 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u32(std::process::id());
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    hasher.write_u128(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    hasher.finish()
}

/// The `--config` overrides that make `jj ... --tool=jj-hunk` call **this**
/// process back as the diff editor.
///
/// Both keys are pinned unconditionally, deliberately overriding whatever the
/// user's `merge-tools.jj-hunk` says. That is not defensiveness about a
/// mis-set key; it is the only way the command can be correct.
///
/// The spec handed to `select` names hunks by id, and an id is a hash this
/// build computed over this diff -- the fork folds the path and an occurrence
/// ordinal into it, so ids from here mean nothing to an upstream binary. The
/// docs tell people to persist `program = "jj-hunk"` in `~/.jjconfig.toml`;
/// resolving that through PATH, or through a `cargo install`ed copy, hands the
/// application step to a *different* program than the one that computed the
/// ids. It then resolves none of them, jj sees an unchanged tree, and the
/// commit comes out empty at exit 0 with the edits still in the working copy.
/// `edit-args` is pinned for the same reason and not merely for tidiness: a
/// stale one that swapped `$left` and `$right` would invert the whole
/// selection just as silently.
///
/// So: the process that computes the ids is always the process that applies
/// them. Anyone wanting a different tool can register it under a different name
/// and drive `jj` directly.
fn jj_hunk_tool_config_args() -> Result<Vec<String>> {
    let program = std::env::current_exe()
        .context("Failed to determine current jj-hunk executable path")?;
    let program = program
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("jj-hunk executable path is not valid UTF-8"))?;

    Ok(vec![
        "--config".to_string(),
        format!("{JJ_HUNK_PROGRAM_KEY}={}", toml_string(program)),
        "--config".to_string(),
        format!(r#"{JJ_HUNK_EDIT_ARGS_KEY}=["select", "$left", "$right"]"#),
    ])
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).expect("string serialization should not fail")
}

/// The `--message` argument to hand to jj, as a single token.
///
/// Passing the message as a separate `-m <msg>` argument made jj's own parser
/// read a message like `--wip` as a flag, so a description could never begin
/// with a dash -- not even via `--`, which got the value past *our* parser only
/// to have jj reject it. `--message=<msg>` is one argument, so everything after
/// the first `=` is the value whatever it looks like.
fn message_arg(message: &str) -> String {
    format!("--message={}", message)
}

pub fn split(
    spec: Option<&str>,
    spec_file: Option<&str>,
    message: &str,
    rev: Option<&str>,
    allow_empty: bool,
) -> Result<()> {
    let message = message_arg(message);
    let mut args = vec!["split", JJ_HUNK_TOOL_ARG, message.as_str()];
    if let Some(rev) = rev {
        args.push("-r");
        args.push(rev);
    }
    run_jj_with_selection(&args, spec, spec_file, rev, allow_empty)
}

pub fn commit(
    spec: Option<&str>,
    spec_file: Option<&str>,
    message: &str,
    allow_empty: bool,
) -> Result<()> {
    let message = message_arg(message);
    run_jj_with_selection(
        &["commit", "-i", JJ_HUNK_TOOL_ARG, message.as_str()],
        spec,
        spec_file,
        None, // commit always operates on @
        allow_empty,
    )
}

pub fn squash(
    spec: Option<&str>,
    spec_file: Option<&str>,
    rev: Option<&str>,
    allow_empty: bool,
) -> Result<()> {
    let mut args = vec!["squash", "-i", JJ_HUNK_TOOL_ARG];
    if let Some(rev) = rev {
        args.push("-r");
        args.push(rev);
    }
    run_jj_with_selection(&args, spec, spec_file, rev, allow_empty)
}

/// Rewrite a revision so it contains only the selected hunks of its diff.
///
/// The named hunks are the ones **kept**; everything else in the diff is
/// discarded. This is the plain reading of `jj diffedit`, whose editor is shown
/// the same diff as `jj diff -r REV` and takes the right-hand side as the
/// revision's new content.
pub fn diffedit(
    spec: Option<&str>,
    spec_file: Option<&str>,
    rev: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    allow_empty: bool,
) -> Result<()> {
    let mut args = vec!["diffedit", JJ_HUNK_TOOL_ARG];

    let target = if from.is_none() && to.is_none() {
        if let Some(rev) = rev {
            args.push("-r");
            args.push(rev);
        }
        DiffTarget::rev(rev)
    } else {
        if rev.is_some() {
            anyhow::bail!("-r/--rev cannot be used with --from/--to");
        }
        if let Some(from) = from {
            args.push("--from");
            args.push(from);
        }
        if let Some(to) = to {
            args.push("--to");
            args.push(to);
        }
        // jj defaults whichever side was left out to the working copy.
        DiffTarget::from_to(from.unwrap_or("@"), to.unwrap_or("@"))
    };

    run_jj_with_selection_on(&args, spec, spec_file, &target, allow_empty)
}

/// Undo the selected hunks, taking their content from another revision.
///
/// The named hunks are the ones **undone** -- the exact opposite of
/// `diffedit`, and the one place in jj-hunk where that is true.
///
/// It falls out of what `jj restore` shows its diff editor: the destination on
/// the left and the source on the right, so the editor's right-hand side starts
/// out fully restored and keeping a hunk there means letting that restoration
/// stand. The spec is therefore built against `destination -> source`, the
/// reverse of `jj diff -r`, which is also the view whose hunk ids it must name
/// (`jj-hunk list --from <destination> --to <source>`).
pub fn restore(
    spec: Option<&str>,
    spec_file: Option<&str>,
    changes_in: Option<&str>,
    from: Option<&str>,
    into: Option<&str>,
    allow_empty: bool,
) -> Result<()> {
    if changes_in.is_some() && (from.is_some() || into.is_some()) {
        anyhow::bail!("-c/--changes-in cannot be used with --from/--into");
    }

    let mut args = vec!["restore", "-i", JJ_HUNK_TOOL_ARG];

    let target = if from.is_none() && into.is_none() {
        // `jj restore` with no target is `jj restore --changes-in @`.
        let revision = changes_in.unwrap_or("@");
        if let Some(changes_in) = changes_in {
            args.push("-c");
            args.push(changes_in);
        }

        let revisions = resolve_revisions(&DiffTarget::rev(Some(revision)))?;
        let destination = revisions
            .after
            .expect("an explicit revset always resolves to an id");
        let source = revisions.before.ok_or_else(|| {
            anyhow::anyhow!("`{revision}` has no parent, so there is nothing to restore from")
        })?;
        DiffTarget::FromTo {
            from: destination,
            to: source,
        }
    } else {
        if let Some(from) = from {
            args.push("--from");
            args.push(from);
        }
        if let Some(into) = into {
            args.push("--into");
            args.push(into);
        }
        // jj defaults whichever side was left out to the working copy. Note the
        // swap: the editor's *left* is the destination.
        DiffTarget::from_to(into.unwrap_or("@"), from.unwrap_or("@"))
    };

    run_jj_with_selection_on(&args, spec, spec_file, &target, allow_empty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_paths_are_wrapped_in_quotes_unchanged() {
        assert_eq!(fileset_string_literal("src/main.rs"), r#""src/main.rs""#);
        assert_eq!(fileset_exact_path("src/main.rs"), r#"file:"src/main.rs""#);
    }

    /// `"` and `\` are the only two characters excluded from `string_content`,
    /// so they are the two that must be escaped for the literal to parse.
    #[test]
    fn quotes_and_backslashes_are_escaped() {
        assert_eq!(fileset_string_literal(r#"quote".txt"#), r#""quote\".txt""#);
        assert_eq!(fileset_string_literal(r"back\slash"), r#""back\\slash""#);
        // A trailing backslash must not escape the closing delimiter.
        assert_eq!(fileset_string_literal(r"dir\"), r#""dir\\""#);
    }

    /// These are fileset *operators*; unquoted they made jj parse the path as
    /// an expression, so the file silently disappeared from the listing.
    #[test]
    fn fileset_operators_are_neutralised_by_quoting() {
        for path in [
            "paren(1).txt",
            "tilde~x.txt",
            "has,comma.txt",
            "single'.txt",
            "a|b.txt",
            "a&b.txt",
            "-leading-dash.txt",
        ] {
            let quoted = fileset_exact_path(path);
            assert!(quoted.starts_with(r#"file:""#), "{quoted}");
            assert!(quoted.ends_with('"'), "{quoted}");
            // The path survives verbatim: none of these need escaping.
            assert!(quoted.contains(path), "{path} was mangled into {quoted}");
        }
    }

    /// A bare path is parsed as a glob, so `br[1].txt` matched `br1.txt`.
    /// `file:` is an exact pattern, so the glob characters are inert.
    #[test]
    fn glob_characters_are_kept_but_matched_exactly() {
        assert_eq!(fileset_exact_path("br[1].txt"), r#"file:"br[1].txt""#);
        assert_eq!(fileset_exact_path("star*.txt"), r#"file:"star*.txt""#);
    }

    #[test]
    fn control_characters_use_the_escapes_the_grammar_defines() {
        assert_eq!(fileset_string_literal("a\tb"), r#""a\tb""#);
        assert_eq!(fileset_string_literal("a\nb"), r#""a\nb""#);
        assert_eq!(fileset_string_literal("a\rb"), r#""a\rb""#);
        assert_eq!(fileset_string_literal("a\x1bb"), r#""a\eb""#);
        assert_eq!(fileset_string_literal("a\x01b"), r#""a\x01b""#);
    }

    /// Non-ASCII is ordinary `string_content`; escaping it as `\xHH` would
    /// split a multi-byte character.
    #[test]
    fn non_ascii_paths_are_passed_through() {
        assert_eq!(fileset_string_literal("café-☃.txt"), r#""café-☃.txt""#);
    }

    #[test]
    fn revision_previews_are_truncated() {
        let ids: Vec<String> = (0..8).map(|i| format!("id{i}")).collect();
        let preview = preview_ids(ids.iter().map(String::as_str));
        assert!(preview.contains("id0"));
        assert!(preview.contains("... and 3 more"), "{preview}");
        assert!(!preview.contains("id5"), "{preview}");
    }

    // --- truncation ---------------------------------------------------------

    fn lines(max: usize) -> Truncation {
        Truncation {
            max_lines: Some(max),
            ..Truncation::NONE
        }
    }

    fn bytes(max: usize) -> Truncation {
        Truncation {
            max_bytes: Some(max),
            ..Truncation::NONE
        }
    }

    #[test]
    fn no_limits_leave_the_text_alone() {
        let (out, cut) = truncate_text("a\nb\nc\n", Truncation::NONE);
        assert_eq!(out, "a\nb\nc\n");
        assert!(!cut);
    }

    #[test]
    fn a_limit_the_text_already_fits_is_not_a_truncation() {
        assert_eq!(truncate_text("a\nb\n", lines(9)), ("a\nb\n".to_string(), false));
        assert_eq!(truncate_text("abc", bytes(3)), ("abc".to_string(), false));
    }

    #[test]
    fn max_lines_keeps_whole_lines_with_their_terminators() {
        let (out, cut) = truncate_text("a\nb\nc\nd\n", lines(2));
        assert_eq!(out, "a\nb\n");
        assert!(cut);
    }

    #[test]
    fn max_lines_zero_empties_the_text() {
        assert_eq!(truncate_text("a\n", lines(0)), (String::new(), true));
        // Nothing to drop, so nothing was truncated.
        assert_eq!(truncate_text("", lines(0)), (String::new(), false));
    }

    #[test]
    fn max_bytes_cuts_mid_line() {
        let (out, cut) = truncate_text("abcdef", bytes(3));
        assert_eq!(out, "abc");
        assert!(cut);
    }

    /// Cutting inside a multi-byte character would panic in `String::truncate`
    /// and leave invalid UTF-8 behind; the cut backs up to a boundary instead.
    #[test]
    fn max_bytes_never_splits_a_character() {
        // "é" is two bytes, so a limit of 2 lands inside the second character.
        let (out, cut) = truncate_text("aé", bytes(2));
        assert_eq!(out, "a");
        assert!(cut);
        assert_eq!(truncate_text("☃", bytes(1)), (String::new(), true));
    }

    #[test]
    fn both_limits_apply_together() {
        // Lines first (a\nb\n = 4 bytes), then the byte cap on what is left.
        let (out, cut) = truncate_text("a\nb\nc\n", Truncation {
            max_lines: Some(2),
            max_bytes: Some(3),
        });
        assert_eq!(out, "a\nb");
        assert!(cut);
    }

    // --- temp spec file -----------------------------------------------------

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jj-hunk-unit-{}-{}-{:x}",
            name,
            std::process::id(),
            random_suffix()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The whole point of `O_EXCL`: a symlink planted at the target path is
    /// refused, not followed. Following it wrote the spec over the link's
    /// target and then unlinked only the link (CWE-377).
    #[cfg(unix)]
    #[test]
    fn creating_a_spec_file_refuses_to_follow_a_planted_symlink() {
        let dir = scratch_dir("symlink");
        let victim = dir.join("victim");
        fs::write(&victim, "PRECIOUS").unwrap();

        let planted = dir.join("jj-hunk-planted.spec");
        std::os::unix::fs::symlink(&victim, &planted).unwrap();

        let err = create_new_exclusive(&planted).expect_err("should refuse an existing path");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&victim).unwrap(), "PRECIOUS");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn creating_a_spec_file_refuses_an_existing_regular_file() {
        let dir = scratch_dir("exists");
        let taken = dir.join("taken.spec");
        fs::write(&taken, "mine").unwrap();

        let err = create_new_exclusive(&taken).expect_err("should refuse an existing path");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&taken).unwrap(), "mine");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn temp_spec_names_do_not_repeat() {
        let names: HashSet<String> = (0..64)
            .map(|_| format!("{:016x}", random_suffix()))
            .collect();
        assert_eq!(names.len(), 64, "suffixes collided");
    }

    /// The pid alone is a guessable name. Whatever the suffix is, it must not
    /// be that.
    #[test]
    fn temp_spec_names_are_not_derived_from_the_pid_alone() {
        let temp = TempSpec::create("{}").unwrap();
        let name = temp.path().file_name().unwrap().to_string_lossy().to_string();
        assert_ne!(name, format!("jj-hunk-{}.spec", std::process::id()));
        assert!(name.starts_with("jj-hunk-"), "{name}");
        assert!(name.ends_with(".spec"), "{name}");
    }

    #[test]
    fn temp_spec_is_written_then_removed_on_drop() {
        let path = {
            let temp = TempSpec::create("hello spec").unwrap();
            assert_eq!(fs::read_to_string(temp.path()).unwrap(), "hello spec");
            temp.path().to_path_buf()
        };
        assert!(!path.exists(), "temp spec should be gone after drop");
    }

    #[cfg(unix)]
    #[test]
    fn temp_spec_is_private_to_its_owner() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempSpec::create("{}").unwrap();
        let mode = fs::metadata(temp.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
    }

    // A bad temp dir is covered by the `temp_spec_file_error_names_the
    // _directory` integration test: setting TMPDIR here would mutate
    // process-global state that every other test in this binary shares.

    // --- pattern normalization ---------------------------------------------

    /// A comma is an ordinary path character and means nothing in a glob.
    /// Splitting on it made comma-containing paths unreachable by a filter.
    #[test]
    fn patterns_are_not_split_on_commas() {
        assert_eq!(
            normalize_patterns(&["has,comma.txt".to_string()]),
            vec!["has,comma.txt".to_string()]
        );
    }

    #[test]
    fn patterns_are_trimmed_and_blanks_dropped() {
        assert_eq!(
            normalize_patterns(&["  src/**  ".to_string(), "   ".to_string()]),
            vec!["src/**".to_string()]
        );
    }

    #[test]
    fn a_message_is_passed_to_jj_as_one_argument() {
        // `-m <msg>` let jj read a dash-leading message as a flag.
        assert_eq!(message_arg("--wip"), "--message=--wip");
        assert_eq!(message_arg("plain"), "--message=plain");
        // An `=` in the message belongs to the value, not the split.
        assert_eq!(message_arg("a=b"), "--message=a=b");
    }

    fn frame_at(prefix: &[&str]) -> PathFrame {
        PathFrame {
            prefix: prefix.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// The four spellings jj really prints from `sub/deep/`, taken from a diff
    /// of `top.txt`, `sub/mid.txt`, `sub/deep/low.txt` and `other/side.txt`
    /// under jj 0.44. Every one of them has to name the same file the root
    /// invocation named, or ids and spec keys diverge by directory.
    #[test]
    fn a_cwd_relative_path_maps_back_to_the_workspace_root() {
        let frame = frame_at(&["sub", "deep"]);
        for (printed, expected) in [
            ("low.txt", "sub/deep/low.txt"),
            ("../mid.txt", "sub/mid.txt"),
            ("../../top.txt", "top.txt"),
            ("../../other/side.txt", "other/side.txt"),
        ] {
            assert_eq!(
                frame.to_root(printed).as_deref(),
                Some(expected),
                "{printed}"
            );
        }
    }

    /// `to_root` is the inverse of `to_display`, so composing them anywhere in
    /// the tree has to land back on the path that went in. This is the property
    /// `list` and `select` each rely on from their own side.
    #[test]
    fn to_display_and_to_root_are_inverses() {
        let frame = frame_at(&["sub", "deep"]);
        for root_path in [
            "sub/deep/low.txt",
            "sub/mid.txt",
            "top.txt",
            "other/side.txt",
            "sub/deep/nested/deeper.txt",
        ] {
            let displayed = frame.to_display(root_path);
            assert_eq!(
                frame.to_root(&displayed).as_deref(),
                Some(root_path),
                "{root_path} printed as {displayed}"
            );
        }
    }

    /// Climbing exactly to the root is ordinary -- `../top.txt` from `sub/` is
    /// how jj spells a root-level file -- and must not be mistaken for the
    /// over-climb below.
    #[test]
    fn climbing_exactly_to_the_workspace_root_is_allowed() {
        assert_eq!(
            frame_at(&["sub"]).to_root("../top.txt").as_deref(),
            Some("top.txt")
        );
    }

    /// A path that climbs *past* the root names no file in the workspace, so it
    /// resolves to nothing. Clamping instead would quietly turn a malformed
    /// `../../top.txt` written from `sub/` into the real file `top.txt`, and a
    /// spec entry nobody wrote would start selecting hunks.
    #[test]
    fn climbing_above_the_workspace_root_resolves_to_nothing() {
        assert_eq!(frame_at(&["sub"]).to_root("../../top.txt"), None);
        assert_eq!(frame_at(&["sub", "deep"]).to_root("../../../x.txt"), None);
    }

    /// At the root every path already is its own root-relative spelling. This
    /// is the property that keeps every id and every spec key ever generated
    /// there byte-identical across this change.
    #[test]
    fn at_the_repo_root_the_mapping_is_the_identity() {
        let frame = frame_at(&[]);
        for path in ["top.txt", "sub/deep/low.txt", "a/b/c/d.txt"] {
            assert_eq!(frame.to_root(path).as_deref(), Some(path));
            assert_eq!(frame.to_display(path), path);
        }
    }

    // --- spec key shape ---
    //
    // Two kinds of test, and they are not the same kind. The refusals below are
    // ordinary regression tests: the shapes they name were accepted before
    // `path_problem` existed. `every_path_jj_prints_is_an_acceptable_spec_key`
    // is the preservation guard -- every path in it was accepted before this
    // check and has to stay accepted after, so it is the one that cannot fail
    // against the code it was written for, and the one that fails first if the
    // refusal is ever widened into a ban on `..`.

    /// Everything jj actually prints has to survive the shape check, or the
    /// check is not a filter but a wall. The `..` spellings matter most: they
    /// are indistinguishable in form from the traversal below and are told
    /// apart only by where the climb lands.
    #[test]
    fn every_path_jj_prints_is_an_acceptable_spec_key() {
        for (prefix, path) in [
            (&[][..], "top.txt"),
            (&[][..], "sub/deep/low.txt"),
            (&["sub"][..], "mid.txt"),
            (&["sub"][..], "../top.txt"),
            (&["sub"][..], "deep/low.txt"),
            (&["sub", "deep"][..], "low.txt"),
            (&["sub", "deep"][..], "../mid.txt"),
            (&["sub", "deep"][..], "../../top.txt"),
            (&["sub", "deep"][..], "../../other/side.txt"),
        ] {
            assert_eq!(
                frame_at(prefix).path_problem(path),
                None,
                "{path} was refused from {prefix:?}"
            );
        }
    }

    /// A key that climbs past the root names no file in the workspace, from any
    /// directory. Refusing it is the whole point of measuring the climb against
    /// the cwd depth rather than banning `..`, which the case above needs.
    #[test]
    fn a_spec_key_that_climbs_above_the_workspace_root_is_refused() {
        for (prefix, path) in [
            (&[][..], "../top.txt"),
            (&[][..], "../../.ssh/id_rsa"),
            (&["sub"][..], "../../top.txt"),
            (&["sub"][..], "../../../../etc/passwd"),
            (&["sub", "deep"][..], "../../../x.txt"),
        ] {
            assert_eq!(
                frame_at(prefix).path_problem(path).as_deref(),
                Some("climbs above the workspace root"),
                "{path} was accepted from {prefix:?}"
            );
        }
    }

    /// jj prints relative paths only, so an absolute key came from somewhere
    /// else. It is refused whatever the cwd is, because no depth makes one
    /// legitimate.
    #[test]
    fn an_absolute_spec_key_is_refused_from_every_directory() {
        for prefix in [&[][..], &["sub"][..], &["sub", "deep"][..]] {
            assert_eq!(
                frame_at(prefix).path_problem("/etc/passwd").as_deref(),
                Some("is an absolute path"),
                "accepted from {prefix:?}"
            );
        }
    }

    /// `sub/../top.txt` names a real file and still is not a path jj would
    /// print. Two spellings of one file is how one spec entry starts shadowing
    /// another, so the un-normalised one is refused rather than normalised.
    #[test]
    fn a_dotdot_after_a_named_directory_is_refused() {
        let frame = frame_at(&["sub"]);
        for path in ["sub/../top.txt", "a/b/../../c.txt", "deep/../../top.txt"] {
            assert!(
                frame
                    .path_problem(path)
                    .is_some_and(|reason| reason.starts_with("has a `..` after")),
                "{path} was accepted"
            );
        }
    }

    /// No path jj can print holds a C0 control character, so a key that does was
    /// not read off any diff. The refusal also keeps such a key from reaching a
    /// terminal through the error message that reports it.
    #[test]
    fn a_control_character_in_a_spec_key_is_refused() {
        let frame = frame_at(&[]);
        for (path, code) in [
            ("top\u{0}.txt", "U+0000"),
            ("top\u{1b}[31m.txt", "U+001B"),
            ("top\n.txt", "U+000A"),
            ("top.txt\u{7}", "U+0007"),
        ] {
            assert_eq!(
                frame.path_problem(path).as_deref(),
                Some(format!("contains the control character {code}").as_str()),
                "{path:?} was accepted"
            );
        }
        // DEL and the C1 block are not covered, and that is a choice, not an
        // oversight: they are ordinary bytes in a filename on every filesystem
        // jj runs on, so refusing them would refuse a real path.
        assert_eq!(frame.path_problem("top\u{7f}.txt"), None);
    }

    /// An empty key names no file, and is the one malformed key that would grow
    /// teeth rather than fizzle if it ever reached a join: `dir.join("")` is
    /// `dir` itself.
    #[test]
    fn an_empty_spec_key_is_refused() {
        assert_eq!(
            frame_at(&[]).path_problem("").as_deref(),
            Some("is empty")
        );
    }

    /// Off Windows a backslash is an ordinary character in a filename, and
    /// splitting on it turns one file into two components. Normalising `\` to
    /// `/` unconditionally -- which is tempting, because on Windows it really
    /// is the separator -- rewrites `back\slash.txt` into a path naming no
    /// file, and moves the id of every such file at the repo root.
    #[cfg(not(windows))]
    #[test]
    fn a_backslash_is_a_filename_character_not_a_separator() {
        let frame = frame_at(&["sub"]);
        assert_eq!(
            frame.to_root(r"back\slash.txt").as_deref(),
            Some(r"sub/back\slash.txt")
        );
        assert_eq!(
            frame_at(&[]).to_root(r"sub/back\slash.txt").as_deref(),
            Some(r"sub/back\slash.txt")
        );
    }
}
