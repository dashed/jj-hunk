use crate::diff::{apply_selected_hunks, get_hunks, Hunk, HunkSelection};
use crate::glob::glob_match;
use crate::hunkset::{self, EnrichedHunk};
#[cfg(feature = "semantic")]
use crate::semantic;
use crate::spec::{Action, DefaultAction, FileSpec, HunkSelector, Spec};
use anyhow::{Context, Result};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::Command;
use walkdir::WalkDir;

const JJ_HUNK_TOOL_ARG: &str = "--tool=jj-hunk";
const JJ_HUNK_PROGRAM_KEY: &str = "merge-tools.jj-hunk.program";
const JJ_HUNK_EDIT_ARGS_KEY: &str = "merge-tools.jj-hunk.edit-args";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ListFormat {
    Json,
    Yaml,
    Text,
    Diff,
}

impl Default for ListFormat {
    fn default() -> Self {
        Self::Json
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ListGrouping {
    None,
    Directory,
    Extension,
    Status,
}

impl Default for ListGrouping {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BinaryMode {
    Skip,
    Mark,
    Include,
}

impl Default for BinaryMode {
    fn default() -> Self {
        Self::Mark
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListMode {
    Full,
    Files,
    SpecTemplate,
}

impl Default for ListMode {
    fn default() -> Self {
        Self::Full
    }
}

#[derive(Debug, Clone)]
pub struct ListOptions {
    pub rev: Option<String>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub group: ListGrouping,
    pub format: ListFormat,
    pub mode: ListMode,
    pub spec: Option<String>,
    pub spec_file: Option<String>,
    pub binary: BinaryMode,
}

impl Default for ListOptions {
    fn default() -> Self {
        Self {
            rev: None,
            include: Vec::new(),
            exclude: Vec::new(),
            group: ListGrouping::default(),
            format: ListFormat::default(),
            mode: ListMode::default(),
            spec: None,
            spec_file: None,
            binary: BinaryMode::default(),
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
}

#[derive(Debug, Serialize, Clone)]
struct RenameInfo {
    from: String,
    to: String,
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
}

#[derive(Debug, Serialize)]
struct SpecTemplateOutput {
    files: HashMap<String, SpecTemplateEntry>,
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
}

/// A change to a file's executable bit.
///
/// jj tracks exactly one mode bit, and it is not part of any hunk. A mode-only
/// change therefore produced zero hunks and vanished from the listing, so the
/// file looked unchanged. It is reported here so it is at least visible; it is
/// never *selectable*, and `select` always restores it from the left side (see
/// `restore_exec_bit`), which leaves it in the working copy.
#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
struct ModeChange {
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
    let resolved_spec_input = resolve_optional_spec(options.spec.as_deref(), options.spec_file.as_deref())?;
    let spec = match &resolved_spec_input {
        Some(content) if hunkset::is_hunkset(content) => {
            let json = evaluate_hunkset(content, options.rev.as_deref())?;
            Some(Spec::from_str(&json)?)
        }
        Some(content) => Some(Spec::from_str(content)?),
        None => None,
    };

    let include = normalize_patterns(&options.include);
    let exclude = normalize_patterns(&options.exclude);

    let all_file_hunks = load_file_hunks(options.rev.as_deref(), options.binary)?;

    let mut files = Vec::new();

    for fh in all_file_hunks {
        let paths_to_check = fh.all_paths();
        if !include.is_empty() && !paths_to_check.iter().any(|p| matches_any(&include, p)) {
            continue;
        }
        if !exclude.is_empty() && paths_to_check.iter().any(|p| matches_any(&exclude, p)) {
            continue;
        }

        let decision = spec_decision(spec.as_ref(), &fh.path);
        if matches!(decision, SpecDecision::Skip) {
            continue;
        }

        let mut hunks = fh.hunks;

        if let SpecDecision::KeepSelection(selection) = &decision {
            hunks = filter_hunks(hunks, selection);
        }

        // A file with no hunks is still a real change if it is binary or if
        // only its mode moved; dropping those made them invisible.
        if hunks.is_empty() && !fh.is_binary && fh.mode.is_none() {
            continue;
        }

        files.push(FileEntry {
            path: fh.path,
            status: fh.status,
            rename: fh.rename,
            hunks,
            binary: if fh.is_binary { Some(true) } else { None },
            mode: fh.mode,
        });
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
            let template = build_spec_template(files);
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

const SUMMARY_TEMPLATE: &str = r#""{\"status\":" ++ self.status().escape_json() ++ ",\"path\":" ++ self.path().display().escape_json() ++ ",\"source\":" ++ self.source().path().display().escape_json() ++ ",\"target\":" ++ self.target().path().display().escape_json() ++ ",\"source_executable\":" ++ if(self.source().executable(), "true", "false") ++ ",\"target_executable\":" ++ if(self.target().executable(), "true", "false") ++ "}\n""#;

struct FilePaths {
    before: Option<String>,
    after: Option<String>,
}

enum SpecDecision {
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

fn resolve_optional_spec(spec: Option<&str>, spec_file: Option<&str>) -> Result<Option<String>> {
    if spec.is_none() && spec_file.is_none() {
        return Ok(None);
    }

    Ok(Some(resolve_spec_input(spec, spec_file)?))
}

/// Evaluate a hunkset expression against the current diff state for a given
/// revision, returning a JSON-serialized Spec.
fn evaluate_hunkset(hunkset_expr: &str, rev: Option<&str>) -> Result<String> {
    let ast = hunkset::parse(hunkset_expr)
        .map_err(|e| anyhow::anyhow!("failed to parse hunkset:\n{}", e.display_with_context()))?;

    let file_hunks = load_file_hunks(rev, BinaryMode::Skip)?;

    let enriched: Vec<EnrichedHunk> = file_hunks
        .iter()
        .flat_map(|fh| {
            fh.hunks.iter().map(move |hunk| EnrichedHunk {
                file_path: &fh.path,
                file_status: &fh.status,
                hunk,
            })
        })
        .collect();

    let selected = hunkset::evaluate(&ast, &enriched)
        .map_err(|e| anyhow::anyhow!("hunkset evaluation error: {}", e.display_with_context()))?;

    let rename_sources: HashMap<&str, &str> = file_hunks
        .iter()
        .filter_map(|fh| {
            fh.rename
                .as_ref()
                .filter(|r| r.from != fh.path)
                .map(|r| (fh.path.as_str(), r.from.as_str()))
        })
        .collect();
    let spec = hunkset::to_spec(&selected, &enriched, &rename_sources);

    serde_json::to_string(&spec).context("failed to serialize hunkset result as spec")
}

/// A file's hunks with metadata, loaded from a jj diff.
struct FileHunks {
    path: String,
    status: String,
    hunks: Vec<Hunk>,
    rename: Option<RenameInfo>,
    is_binary: bool,
    mode: Option<ModeChange>,
}

impl FileHunks {
    /// All paths associated with this file entry (primary + rename source).
    fn all_paths(&self) -> Vec<&str> {
        let mut paths = vec![self.path.as_str()];
        if let Some(rename) = &self.rename {
            if rename.from != self.path {
                paths.push(&rename.from);
            }
        }
        paths
    }
}

/// Load all file hunks for a revision, applying semantic enrichment.
/// This is the shared core used by both `list` and `evaluate_hunkset`.
fn load_file_hunks(rev: Option<&str>, binary: BinaryMode) -> Result<Vec<FileHunks>> {
    // Validate the revision *before* doing any work: a merge or a
    // multi-revision revset makes every hunk below meaningless.
    let revisions = resolve_revisions(rev)?;
    let summary_entries = read_diff_summary(rev)?;
    let mut result = Vec::new();

    for entry in &summary_entries {
        let path = primary_path(entry);
        if path.is_empty() {
            continue;
        }

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
        let (before_text, after_text) = if should_diff {
            (
                String::from_utf8_lossy(&before_bytes).into_owned(),
                String::from_utf8_lossy(&after_bytes).into_owned(),
            )
        } else {
            (String::new(), String::new())
        };

        let mut hunks = if should_diff {
            get_hunks(&before_text, &after_text)
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
            mode: mode_change_for_entry(entry),
        });
    }

    Ok(result)
}

/// The two sides a diff is computed between.
struct DiffRevisions {
    /// Revision the "before" text is read from. `None` means the target has no
    /// parent (the root commit), so the before side is empty.
    before: Option<String>,
    /// Revision the "after" text is read from. `None` means the working copy
    /// on disk.
    after: Option<String>,
}

/// One resolved revision and the ids of its parents.
struct ResolvedRevision {
    id: String,
    parents: Vec<String>,
}

/// Emits `<commit id>\t<parent id> <parent id> ...` per revision, so one
/// `jj log` answers both "how many revisions?" and "how many parents?".
const REVISION_TEMPLATE: &str =
    r#"commit_id.short() ++ "\t" ++ parents.map(|c| c.commit_id().short()).join(" ") ++ "\n""#;

/// Revisions a revset resolves to, in `jj log` order.
fn resolve_revset(revset: &str) -> Result<Vec<ResolvedRevision>> {
    let output = Command::new("jj")
        .args(["log", "--no-graph", "-r", revset, "-T", REVISION_TEMPLATE])
        .output()
        .context("Failed to run jj log")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("failed to resolve revset `{}`: {}", revset, stderr.trim());
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

fn preview_ids<'a>(ids: impl ExactSizeIterator<Item = &'a str>) -> String {
    const MAX: usize = 5;
    let total = ids.len();
    let mut shown: Vec<String> = ids.take(MAX).map(str::to_string).collect();
    if total > MAX {
        shown.push(format!("... and {} more", total - MAX));
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
fn resolve_revisions(revset: Option<&str>) -> Result<DiffRevisions> {
    let target = revset.unwrap_or("@");

    let mut targets = resolve_revset(target)?;
    match targets.len() {
        0 => anyhow::bail!("revset `{}` did not resolve to any revision", target),
        1 => {}
        n => anyhow::bail!(
            "revset `{}` resolved to {} revisions, but jj-hunk needs exactly one.\n\
             Hunks are only defined between a single revision and its parent.\n\
             Resolved to: {}",
            target,
            n,
            preview_ids(targets.iter().map(|r| r.id.as_str()))
        ),
    }

    let resolved = targets.remove(0);
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

fn read_diff_summary(revset: Option<&str>) -> Result<Vec<DiffSummaryEntry>> {
    let mut diff_args = vec!["diff", "--template", SUMMARY_TEMPLATE];
    if let Some(rev) = revset {
        diff_args.push("-r");
        diff_args.push(rev);
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

fn spec_decision(spec: Option<&Spec>, path: &str) -> SpecDecision {
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

fn filter_hunks(hunks: Vec<Hunk>, selection: &HunkSelection) -> Vec<Hunk> {
    hunks
        .into_iter()
        .filter(|hunk| selection.matches(hunk.index, &hunk.id))
        .collect()
}

fn normalize_patterns(patterns: &[String]) -> Vec<String> {
    patterns
        .iter()
        .flat_map(|pattern| pattern.split(','))
        .map(|pattern| pattern.trim())
        .filter(|pattern| !pattern.is_empty())
        .map(|pattern| pattern.to_string())
        .collect()
}

fn matches_any(patterns: &[String], path: &str) -> bool {
    patterns.iter().any(|pattern| glob_match(pattern, path))
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

fn build_spec_template(files: Vec<FileEntry>) -> SpecTemplateOutput {
    let mut output = HashMap::new();

    for file in files {
        let from = rename_source(&file.rename, &file.path);

        if file.hunks.is_empty() {
            if file.binary == Some(true) {
                output.insert(
                    file.path,
                    SpecTemplateEntry::Action {
                        action: "keep".to_string(),
                        from,
                    },
                );
            }
            continue;
        }

        let ids = file.hunks.into_iter().map(|hunk| hunk.id).collect();
        output.insert(file.path, SpecTemplateEntry::Ids { ids, from });
    }

    SpecTemplateOutput {
        files: output,
        default: "reset".to_string(),
    }
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
        for block in group_adjacent_hunks(&file.hunks) {
            result.push_str(&render_diff_block(&file.hunks[block.0..block.1]));
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

fn render_diff_block(hunks: &[Hunk]) -> String {
    let mut result = String::new();
    let Some(first) = hunks.first() else {
        return result;
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

    let old_start = first.before_range.start.saturating_sub(leading.len());
    let new_start = first.after_range.start.saturating_sub(leading.len());
    // A zero-length side is written as `-0,0` / `+0,0`; otherwise the start is
    // the first line the block covers.
    let old_start = if old_len == 0 { 0 } else { old_start.max(1) };
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

    // Truncate ID for readability (first 12 hex chars after "hunk-")
    let short_id = if first.id.len() > 17 {
        format!("{}...", &first.id[..17])
    } else {
        first.id.clone()
    };

    result.push_str(&format!(
        "@@ -{},{} +{},{} @@{} [{}]\n",
        old_start, old_len, new_start, new_len, scope, short_id
    ));
    result.push_str(&body);
    result
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
                hunk.id,
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
    if let Some(mode) = &file.mode {
        header.push_str(&format_mode_change(mode));
    }
    header
}

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

    let spec = if let Some(path) = spec_path {
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read spec from {}", path))?;
        Spec::from_str(&content)?
    } else {
        // No selection = keep everything
        return Ok(());
    };

    let left_path = Path::new(left);
    let right_path = Path::new(right);

    // Get all files in both directories
    let left_files = list_files(left_path);
    let right_files = list_files(right_path);
    let all_files: HashSet<_> = left_files.union(&right_files).cloned().collect();

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

    for filepath in &all_files {
        let Some(file_spec) = spec.files.get(filepath) else {
            continue;
        };
        let source = file_spec
            .source_path()
            .filter(|from| *from != filepath.as_str());

        let keeps_change = match file_spec {
            FileSpec::Action {
                action: Action::Keep,
                ..
            } => true,
            FileSpec::Action {
                action: Action::Reset,
                ..
            } => {
                reset_file(left_path, right_path, filepath)?;
                false
            }
            FileSpec::Selection(selection) => apply_hunk_selection(
                left_path,
                right_path,
                source,
                filepath,
                &selection.to_selection(),
            )?,
        };
        kept.insert(filepath.as_str(), keeps_change);
    }

    for filepath in &all_files {
        if spec.files.contains_key(filepath) {
            continue;
        }

        // A path that some entry claims as its rename source, and that is
        // really gone from the right side, follows that entry: a kept rename
        // leaves it deleted, a reset one puts it back. (A copy leaves the
        // source in place on the right, so it takes the default action like
        // any other file.)
        if let Some(target) = rename_sources.get(filepath.as_str()) {
            if !right_files.contains(filepath) {
                if !kept.get(target).copied().unwrap_or(false) {
                    reset_file(left_path, right_path, filepath)?;
                }
                continue;
            }
        }

        if spec.default == DefaultAction::Reset {
            reset_file(left_path, right_path, filepath)?;
        }
    }

    Ok(())
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
    let left_file = left.join(filepath);
    let right_file = right.join(filepath);

    // `symlink_metadata` deliberately does NOT traverse the link. `exists()`
    // does, so a dangling symlink in `left` (jj materialises only the changed
    // files, so a link's target is usually absent) reported "not there" and we
    // deleted a file that was never selected. `fs::copy` traverses too, and
    // wrote the *target's* bytes into the link's path.
    let left_meta = fs::symlink_metadata(&left_file).ok();
    let right_exists = fs::symlink_metadata(&right_file).is_ok();

    match left_meta {
        Some(meta) => {
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
/// `source` is the file's path on the left side when it differs from
/// `filepath` (renames and copies). The spec's hunk ids were computed by
/// diffing `left/<source>` against `right/<filepath>`; joining `filepath` onto
/// both sides instead recomputes the change as one whole-file insertion under
/// a different id, so nothing matches and the file is written out empty.
fn apply_hunk_selection(
    left: &Path,
    right: &Path,
    source: Option<&str>,
    filepath: &str,
    selection: &HunkSelection,
) -> Result<bool> {
    // A `from` naming something that is not on the left is unusable. Fall back
    // to the spec key rather than reading an empty "before" and concluding the
    // file is new -- that path ends in deleting it.
    let source = source.filter(|from| fs::symlink_metadata(left.join(from)).is_ok());

    let left_file = left.join(source.unwrap_or(filepath));
    let right_file = right.join(filepath);

    // Not `exists()`: that traverses symlinks, so a dangling link (jj
    // materialises only the changed files, so a link's target is usually
    // absent) reads as "not there".
    let right_exists = fs::symlink_metadata(&right_file).is_ok();

    // Deleted on the right: the whole file is a single delete hunk. Keep the
    // deletion only if that hunk is selected -- otherwise the file is reset,
    // which for a deletion means putting it back. Returning early here left
    // every deletion in the commit no matter what the spec said.
    if !right_exists {
        // A deletion is always same-path, so read the left side at `filepath`
        // rather than at a `source` a malformed spec might have supplied.
        // Unreadable (a symlink, say) reads as empty, which yields no hunks
        // and so restores the file -- the safe direction.
        let before = fs::read_to_string(left.join(filepath)).unwrap_or_default();
        let keeps_deletion = get_hunks(&before, "")
            .iter()
            .any(|hunk| selection.matches(hunk.index, &hunk.id));
        if !keeps_deletion {
            reset_file(left, right, filepath)?;
        }
        return Ok(keeps_deletion);
    }

    // A file that exists on the right but not at `filepath` on the left is
    // either newly added or the target of a rename. Either way its "reset"
    // state is: not present.
    let existed_at_this_path = source.is_none() && fs::symlink_metadata(&left_file).is_ok();

    let before = fs::read_to_string(&left_file).unwrap_or_default();
    let after = fs::read_to_string(&right_file)?;

    let result = apply_selected_hunks(&before, &after, selection);
    let keeps_change = result != before;

    if !existed_at_this_path && !keeps_change {
        // Writing `result` here would leave a 0-byte file in the commit for an
        // added file, or an unwanted copy of the old content for a rename.
        fs::remove_file(&right_file)?;
        return Ok(false);
    }

    fs::write(&right_file, &result)?;
    // `fs::write` keeps whatever mode the destination already had, so a
    // `chmod +x` in the working copy survived even when none of the file's
    // hunks were selected. A mode change is not a hunk and cannot be selected,
    // so it is restored from the left side exactly like unselected content.
    restore_exec_bit(&left_file, &right_file)?;
    Ok(keeps_change)
}

/// Give `right` the executable bits `left` has.
#[cfg(unix)]
fn restore_exec_bit(left: &Path, right: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    // A file that is absent on the left is newly added; there is no earlier
    // mode to restore, and its own mode is part of the addition.
    let Ok(left_meta) = fs::metadata(left) else {
        return Ok(());
    };
    let right_meta =
        fs::metadata(right).with_context(|| format!("Failed to stat {}", right.display()))?;

    let exec = left_meta.permissions().mode() & 0o111;
    let mode = (right_meta.permissions().mode() & !0o111) | exec;
    fs::set_permissions(right, fs::Permissions::from_mode(mode))
        .with_context(|| format!("Failed to restore mode on {}", right.display()))
}

#[cfg(not(unix))]
fn restore_exec_bit(_left: &Path, _right: &Path) -> Result<()> {
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

/// Check that a spec actually resolves against the diff it will be applied to.
///
/// `Spec::selects_nothing` is structural: a non-empty id list satisfies it even
/// when no such hunk exists. So a typo'd path or a stale id passed the guard,
/// selected nothing, and produced an empty commit at exit 0 -- the exact
/// failure that guard was added to prevent.
///
/// An entry that deliberately keeps nothing (`"ids": []`) is not an error:
/// blanking out the ids you do not want is the documented workflow, and the
/// structural check above already catches a spec where *every* entry is empty.
fn validate_spec_resolves(spec: &Spec, file_hunks: &[FileHunks]) -> Result<()> {
    let mut known: HashMap<&str, &FileHunks> = HashMap::new();
    for fh in file_hunks {
        for path in fh.all_paths() {
            known.insert(path, fh);
        }
    }

    let mut problems: Vec<String> = Vec::new();

    for (path, file_spec) in &spec.files {
        // An entry that keeps nothing by construction cannot produce the empty
        // commit this guards against, so an unknown path there is harmless.
        let intends_to_keep = match file_spec {
            FileSpec::Action { action, .. } => *action == Action::Keep,
            FileSpec::Selection(selection) => {
                !selection.hunks.is_empty() || !selection.ids.is_empty()
            }
        };

        let Some(fh) = known.get(path.as_str()) else {
            if intends_to_keep {
                problems.push(format!("{path}: no such path in the diff"));
            }
            continue;
        };

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
            if !fh.hunks.iter().any(|hunk| hunk.id == id) {
                problems.push(format!("{path}: no hunk with id {id}"));
            }
        }

        for selector in &selection.hunks {
            let HunkSelector::Index(index) = selector else {
                continue;
            };
            if !fh.hunks.iter().any(|hunk| hunk.index == *index) {
                problems.push(format!(
                    "{path}: no hunk with index {index} (file has {})",
                    fh.hunks.len()
                ));
            }
        }
    }

    if !problems.is_empty() {
        problems.sort();
        anyhow::bail!(
            "spec does not resolve against the diff:\n  {}\n\
             Those entries would keep nothing. Check them against \
             `jj-hunk list --spec-template`, or pass --allow-empty if that is \
             intended.",
            problems.join("\n  ")
        );
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

fn run_jj_with_selection(
    args: &[&str],
    spec: Option<&str>,
    spec_file: Option<&str>,
    rev: Option<&str>,
    allow_empty: bool,
) -> Result<()> {
    let raw_spec = resolve_spec_input(spec, spec_file)?;
    let is_hunkset = hunkset::is_hunkset(&raw_spec);
    let mut spec_content = if is_hunkset {
        evaluate_hunkset(&raw_spec, rev)?
    } else {
        raw_spec.clone()
    };

    if let Ok(mut parsed) = Spec::from_str(&spec_content) {
        // Refuse to mutate history with a selection that keeps nothing. jj
        // would happily create an empty commit and exit 0, which hides a
        // typo'd selector from any script driving this.
        if !allow_empty && parsed.selects_nothing() {
            anyhow::bail!(
                "selection matched no hunks: {}\n\
                 Nothing would be kept, so this would create an empty commit.\n\
                 Check the selector with `jj-hunk list --spec ...`, or pass \
                 --allow-empty if that is intended.",
                raw_spec.trim()
            );
        }

        // A hunkset spec is built from the diff it was evaluated against, so
        // its paths and ids resolve by construction and it already carries
        // every rename source. A hand-written one needs both passes.
        if !is_hunkset {
            // `Mark`, not `Skip`: a binary file has no hunks but is still a
            // legitimate spec target via `{"action": "keep"}`.
            let file_hunks = load_file_hunks(rev, BinaryMode::Mark)?;
            if !allow_empty {
                validate_spec_resolves(&parsed, &file_hunks)?;
            }
            if fill_rename_sources(&mut parsed, &file_hunks) {
                spec_content = serde_json::to_string(&parsed)
                    .context("failed to re-serialize spec with rename sources")?;
            }
        }
    }

    let temp_file = std::env::temp_dir().join(format!("jj-hunk-{}.spec", std::process::id()));
    fs::write(&temp_file, spec_content)?;

    let config_args = jj_hunk_tool_config_args()?;

    let status = Command::new("jj")
        .args(&config_args)
        .args(args)
        .env("JJ_HUNK_SELECTION", &temp_file)
        .status()
        .context("Failed to run jj")?;

    fs::remove_file(&temp_file).ok();

    if !status.success() {
        anyhow::bail!("jj command failed");
    }
    Ok(())
}

fn jj_hunk_tool_config_args() -> Result<Vec<String>> {
    let mut args = Vec::new();

    if !jj_config_key_exists(JJ_HUNK_PROGRAM_KEY) {
        let program = std::env::current_exe()
            .context("Failed to determine current jj-hunk executable path")?;
        let program = program
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("jj-hunk executable path is not valid UTF-8"))?;
        args.push("--config".to_string());
        args.push(format!("{JJ_HUNK_PROGRAM_KEY}={}", toml_string(program)));
    }

    if !jj_config_key_exists(JJ_HUNK_EDIT_ARGS_KEY) {
        args.push("--config".to_string());
        args.push(format!(
            r#"{JJ_HUNK_EDIT_ARGS_KEY}=["select", "$left", "$right"]"#
        ));
    }

    Ok(args)
}

fn jj_config_key_exists(key: &str) -> bool {
    Command::new("jj")
        .args(["config", "get", key])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).expect("string serialization should not fail")
}

pub fn split(
    spec: Option<&str>,
    spec_file: Option<&str>,
    message: &str,
    rev: Option<&str>,
    allow_empty: bool,
) -> Result<()> {
    let mut args = vec!["split", JJ_HUNK_TOOL_ARG, "-m", message];
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
    run_jj_with_selection(
        &["commit", "-i", JJ_HUNK_TOOL_ARG, "-m", message],
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
}
