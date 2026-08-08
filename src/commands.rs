use crate::diff::{apply_selected_hunks, get_hunks, Hunk, HunkSelection};
use crate::glob::glob_match;
use crate::hunkset::{self, EnrichedHunk};
#[cfg(feature = "semantic")]
use crate::semantic;
use crate::spec::{Action, DefaultAction, FileSpec, Spec};
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
}

#[derive(Debug, Serialize)]
struct SpecTemplateOutput {
    files: HashMap<String, SpecTemplateEntry>,
    default: String,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum SpecTemplateEntry {
    Ids { ids: Vec<String> },
    Action { action: String },
}

#[derive(Debug, Deserialize)]
struct DiffSummaryEntry {
    status: String,
    path: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    target: String,
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

        if hunks.is_empty() && !fh.is_binary {
            continue;
        }

        files.push(FileEntry {
            path: fh.path,
            status: fh.status,
            rename: fh.rename,
            hunks,
            binary: if fh.is_binary { Some(true) } else { None },
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

const SUMMARY_TEMPLATE: &str = r#""{\"status\":" ++ self.status().escape_json() ++ ",\"path\":" ++ self.path().display().escape_json() ++ ",\"source\":" ++ self.source().path().display().escape_json() ++ ",\"target\":" ++ self.target().path().display().escape_json() ++ "}\n""#;

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
    let spec = hunkset::to_spec(&selected, &enriched);

    serde_json::to_string(&spec).context("failed to serialize hunkset result as spec")
}

/// A file's hunks with metadata, loaded from a jj diff.
struct FileHunks {
    path: String,
    status: String,
    hunks: Vec<Hunk>,
    rename: Option<RenameInfo>,
    is_binary: bool,
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
    let summary_entries = read_diff_summary(rev)?;
    let (before_rev, after_rev) = resolve_revisions(rev);
    let mut result = Vec::new();

    for entry in &summary_entries {
        let path = primary_path(entry);
        if path.is_empty() {
            continue;
        }

        let file_paths = file_paths_for_entry(entry, &path);
        let before_bytes = file_paths
            .before
            .as_deref()
            .map(|p| read_jj_file(before_rev.as_deref(), p))
            .unwrap_or_default();
        let after_bytes = file_paths
            .after
            .as_deref()
            .map(|p| read_jj_file(after_rev.as_deref(), p))
            .unwrap_or_default();

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
        });
    }

    Ok(result)
}

fn resolve_revisions(revset: Option<&str>) -> (Option<String>, Option<String>) {
    if let Some(rev) = revset {
        (Some(format!("({})-", rev)), Some(rev.to_string()))
    } else {
        (Some("@-".to_string()), None)
    }
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

fn read_jj_file(rev: Option<&str>, path: &str) -> Vec<u8> {
    let mut args = vec!["file", "show"];
    if let Some(rev) = rev {
        args.push("-r");
        args.push(rev);
    }
    args.push(path);

    Command::new("jj")
        .args(&args)
        .output()
        .map(|o| o.stdout)
        .unwrap_or_default()
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
            } => SpecDecision::KeepAll,
            FileSpec::Action {
                action: Action::Reset,
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

fn build_spec_template(files: Vec<FileEntry>) -> SpecTemplateOutput {
    let mut output = HashMap::new();

    for file in files {
        if file.hunks.is_empty() {
            if file.binary == Some(true) {
                output.insert(
                    file.path,
                    SpecTemplateEntry::Action {
                        action: "keep".to_string(),
                    },
                );
            }
            continue;
        }

        let ids = file.hunks.into_iter().map(|hunk| hunk.id).collect();
        output.insert(file.path, SpecTemplateEntry::Ids { ids });
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

fn context_lines(text: &str) -> Vec<&str> {
    text.lines().collect()
}

/// Reconstruct the `gap` source lines sitting between two hunks.
///
/// `prev.context.post` holds the first up-to-3 lines after `prev`, and
/// `next.context.pre` holds the last up-to-3 lines before `next`. For a gap of
/// at most `2 * DIFF_CONTEXT_LINES` the two together cover it exactly.
fn gap_lines<'a>(prev: &'a Hunk, next: &'a Hunk, gap: usize) -> Vec<&'a str> {
    let post: Vec<&str> = prev
        .context
        .as_ref()
        .map(|c| context_lines(&c.after))
        .unwrap_or_default();
    let mut out: Vec<&str> = post.into_iter().take(gap.min(DIFF_CONTEXT_LINES)).collect();
    if out.len() < gap {
        let pre: Vec<&str> = next
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

    let leading: Vec<&str> = first
        .context
        .as_ref()
        .map(|c| context_lines(&c.before))
        .unwrap_or_default();
    let trailing: Vec<&str> = last
        .context
        .as_ref()
        .map(|c| context_lines(&c.after))
        .unwrap_or_default();

    // Body, and the change-line counts, are built together so the header can
    // be computed from what was actually emitted.
    let mut body = String::new();
    let mut old_len = leading.len();
    let mut new_len = leading.len();
    for line in &leading {
        body.push_str(&format!(" {}\n", line));
    }

    for (i, hunk) in hunks.iter().enumerate() {
        if i > 0 {
            let prev = &hunks[i - 1];
            let prev_end = prev.before_range.start + prev.before_range.length;
            let gap = hunk.before_range.start.saturating_sub(prev_end);
            for line in gap_lines(prev, hunk, gap) {
                body.push_str(&format!(" {}\n", line));
                old_len += 1;
                new_len += 1;
            }
        }
        for line in hunk.removed.lines() {
            body.push_str(&format!("-{}\n", line));
            old_len += 1;
        }
        for line in hunk.added.lines() {
            body.push_str(&format!("+{}\n", line));
            new_len += 1;
        }
    }

    for line in &trailing {
        body.push_str(&format!(" {}\n", line));
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
    header
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

    for filepath in all_files {
        let file_spec = spec.files.get(&filepath);

        match file_spec {
            Some(FileSpec::Action {
                action: Action::Keep,
            }) => {
                // Keep as-is
            }
            Some(FileSpec::Action {
                action: Action::Reset,
            }) => {
                reset_file(left_path, right_path, &filepath)?;
            }
            Some(FileSpec::Selection(selection)) => {
                let selection = selection.to_selection();
                apply_hunk_selection(left_path, right_path, &filepath, &selection)?;
            }
            None => {
                // Use default
                if spec.default == DefaultAction::Reset {
                    reset_file(left_path, right_path, &filepath)?;
                }
            }
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

fn apply_hunk_selection(
    left: &Path,
    right: &Path,
    filepath: &str,
    selection: &HunkSelection,
) -> Result<()> {
    let left_file = left.join(filepath);
    let right_file = right.join(filepath);

    let before = if left_file.exists() {
        fs::read_to_string(&left_file)?
    } else {
        String::new()
    };

    let after = if right_file.exists() {
        fs::read_to_string(&right_file)?
    } else {
        return Ok(());
    };

    let result = apply_selected_hunks(&before, &after, selection);

    fs::write(&right_file, result)?;
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

fn run_jj_with_selection(
    args: &[&str],
    spec: Option<&str>,
    spec_file: Option<&str>,
    rev: Option<&str>,
    allow_empty: bool,
) -> Result<()> {
    let raw_spec = resolve_spec_input(spec, spec_file)?;
    let spec_content = if hunkset::is_hunkset(&raw_spec) {
        evaluate_hunkset(&raw_spec, rev)?
    } else {
        raw_spec.clone()
    };

    // Refuse to mutate history with a selection that keeps nothing. jj would
    // happily create an empty commit and exit 0, which hides a typo'd
    // selector from any script driving this.
    if !allow_empty {
        if let Ok(parsed) = Spec::from_str(&spec_content) {
            if parsed.selects_nothing() {
                anyhow::bail!(
                    "selection matched no hunks: {}\n\
                     Nothing would be kept, so this would create an empty commit.\n\
                     Check the selector with `jj-hunk list --spec ...`, or pass \
                     --allow-empty if that is intended.",
                    raw_spec.trim()
                );
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
