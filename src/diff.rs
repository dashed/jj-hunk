use anyhow::{bail, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use similar::{ChangeTag, TextDiff};
use std::collections::{HashMap, HashSet};
use std::fmt::Write;

pub const HUNK_ID_PREFIX: &str = "hunk-";

/// Hex digits in a full hunk id (a SHA-256 digest).
const HUNK_ID_HEX_LEN: usize = 64;

/// Shortest abbreviation we will ever *print*, in hex digits.
///
/// Matches jj's own floor for change and commit ids (`format_short_id(id)` is
/// `id.shortest(8)`), and for the same reasons: eight hex digits is short
/// enough to read at a glance and retype without error, while 32 bits keeps
/// accidental sharing of a prefix out of sight for any diff a person would
/// look at. Anything that does collide is widened rather than truncated, so
/// the floor is a readability choice, never a correctness one.
///
/// Shorter forms are still *accepted* on input -- an unambiguous prefix of any
/// length resolves -- exactly as jj accepts a two-character change id.
pub const MIN_SHORT_ID_HEX: usize = 8;

fn is_zero(v: &usize) -> bool {
    *v == 0
}
const CONTEXT_LINES: usize = 3;

#[derive(Debug, Clone, Serialize)]
pub struct LineRange {
    pub start: usize,
    #[serde(rename = "lines")]
    pub length: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct HunkContext {
    #[serde(rename = "pre")]
    pub before: String,
    #[serde(rename = "post")]
    pub after: String,
}

/// Semantic metadata extracted via tree-sitter (when the `semantic` feature is enabled).
#[derive(Debug, Clone, Default, Serialize)]
pub struct SemanticInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enclosing_function: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enclosing_scope: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_doc_comment: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_import: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_toplevel: bool,
    #[serde(skip_serializing_if = "is_zero")]
    pub nesting_depth: usize,
    /// Whether a parser actually ran for this hunk's file.
    ///
    /// Distinguishes "parsed, and top level" from "never parsed". Without it,
    /// an unsupported file type looks identical to genuinely top-level code:
    /// the default is depth 0 with is_toplevel false, so `depth(0)` matched it
    /// while `toplevel()` did not.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_analyzed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Hunk {
    pub index: usize,
    pub id: String,
    /// The shortest unambiguous form of `id`, for humans to read and retype.
    ///
    /// Machine-readable output keeps `id` as the full 64 hex digits -- that is
    /// the stable contract, and a program has no reason to care about length --
    /// and carries this alongside. Text and diff output show only this one.
    ///
    /// Assigned by [`assign_short_ids`] over a whole diff, so it is only as
    /// short as the set of hunks it was computed against allows.
    pub short_id: String,
    #[serde(rename = "type")]
    pub hunk_type: String,
    pub removed: String,
    pub added: String,
    #[serde(rename = "before")]
    pub before_range: LineRange,
    #[serde(rename = "after")]
    pub after_range: LineRange,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<HunkContext>,
    #[serde(flatten)]
    pub semantic: SemanticInfo,
}

#[derive(Debug, Clone, Default)]
pub struct HunkSelection {
    pub indices: HashSet<usize>,
    pub ids: HashSet<String>,
}

impl HunkSelection {
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty() && self.ids.is_empty()
    }

    /// Resolve this selection against one file's hunks.
    ///
    /// Indices match exactly; ids match as prefixes, so a selection may name a
    /// hunk by its full id or by any abbreviation of it. An id naming more
    /// than one hunk is an error rather than a multi-select: a selection is
    /// what destructive commands act on, and keeping two hunks because a short
    /// prefix was shared is the worst possible reading of the intent. jj
    /// rejects ambiguous change-id prefixes for the same reason.
    ///
    /// An id matching *nothing* is not an error here -- callers apply a
    /// selection file by file, so most ids do not belong to the file at hand.
    /// `validate_spec_resolves` catches ids that match nothing anywhere.
    pub fn resolve(&self, hunks: &[Hunk]) -> Result<HashSet<usize>> {
        let mut selected: HashSet<usize> = hunks
            .iter()
            .map(|hunk| hunk.index)
            .filter(|index| self.indices.contains(index))
            .collect();

        // Sorted, so that a selection naming two ambiguous ids reports the
        // same one every time rather than whichever the hash set yielded first.
        let mut ids: Vec<&String> = self.ids.iter().collect();
        ids.sort_unstable();

        for id in ids {
            let matched: Vec<&Hunk> = hunks.iter().filter(|hunk| id_matches(id, &hunk.id)).collect();
            if matched.len() > 1 {
                let candidates: Vec<&str> = matched.iter().map(|hunk| hunk.short_id.as_str()).collect();
                bail!(
                    "hunk id {id} is ambiguous: it names {} hunks ({}). \
                     Use a longer prefix.",
                    matched.len(),
                    candidates.join(", ")
                );
            }
            selected.extend(matched.iter().map(|hunk| hunk.index));
        }

        Ok(selected)
    }
}

/// Whether `selector` names `full`, either exactly or as an abbreviation.
///
/// Only a strictly shorter selector is treated as an abbreviation. One as long
/// as the id has to equal it: two ids of the same length cannot prefix each
/// other, so anything else would just be a wrong id spelled at full length.
pub fn id_matches(selector: &str, full: &str) -> bool {
    if selector.len() < full.len() {
        full.starts_with(selector)
    } else {
        selector == full
    }
}

/// Extract hunks from before/after content.
///
/// `path` is the file's path on the right-hand side of the diff. It is part of
/// every id, so the same edit made to two files does not produce one id shared
/// between them -- see [`compute_hunk_id`].
///
/// It must be **workspace-root-relative**. Callers reach this function holding
/// a path in whichever frame their caller spoke, and the frames differ: `list`
/// is handed cwd-relative paths by jj, `select` repo-relative ones by the
/// merge-tool protocol. Hashing whichever one arrived made a hunk's id a
/// function of the directory the command ran from -- the same hunk answered to
/// a different id from `sub/` than from the root, so a spec produced at the
/// root resolved against nothing one level down.
pub fn get_hunks(path: &str, before: &str, after: &str) -> Vec<Hunk> {
    let diff = TextDiff::from_lines(before, after);
    let before_lines = split_lines_with_endings(before);
    let mut ids = HunkIds::new(path);
    let mut hunks = Vec::new();
    let mut current_removed = String::new();
    let mut current_added = String::new();
    let mut in_hunk = false;
    let mut before_line = 1;
    let mut after_line = 1;
    let mut hunk_before_start = 0;
    let mut hunk_after_start = 0;
    let mut hunk_before_len = 0;
    let mut hunk_after_len = 0;

    for change in diff.iter_all_changes() {
        let line_count = count_lines(change.value());
        match change.tag() {
            ChangeTag::Equal => {
                if in_hunk {
                    finalize_hunk(
                        &mut hunks,
                        &mut ids,
                        &mut current_removed,
                        &mut current_added,
                        &before_lines,
                        LineRange { start: hunk_before_start, length: hunk_before_len },
                        LineRange { start: hunk_after_start, length: hunk_after_len },
                    );
                    hunk_before_len = 0;
                    hunk_after_len = 0;
                    in_hunk = false;
                }
                before_line += line_count;
                after_line += line_count;
            }
            ChangeTag::Delete => {
                if !in_hunk {
                    in_hunk = true;
                    hunk_before_start = before_line;
                    hunk_after_start = after_line;
                    hunk_before_len = 0;
                    hunk_after_len = 0;
                }
                current_removed.push_str(change.value());
                hunk_before_len += line_count;
                before_line += line_count;
            }
            ChangeTag::Insert => {
                if !in_hunk {
                    in_hunk = true;
                    hunk_before_start = before_line;
                    hunk_after_start = after_line;
                    hunk_before_len = 0;
                    hunk_after_len = 0;
                }
                current_added.push_str(change.value());
                hunk_after_len += line_count;
                after_line += line_count;
            }
        }
    }

    if in_hunk {
        finalize_hunk(
            &mut hunks,
            &mut ids,
            &mut current_removed,
            &mut current_added,
            &before_lines,
            LineRange { start: hunk_before_start, length: hunk_before_len },
            LineRange { start: hunk_after_start, length: hunk_after_len },
        );
    }

    // A file on its own is a valid scope to abbreviate against, and it means a
    // `Hunk` is never handed out with a `short_id` that does not identify it.
    // `list` widens these again over the whole diff once every file is loaded.
    assign_short_ids(hunks.iter_mut());

    hunks
}

fn finalize_hunk(
    hunks: &mut Vec<Hunk>,
    ids: &mut HunkIds,
    current_removed: &mut String,
    current_added: &mut String,
    before_lines: &[&str],
    before_range: LineRange,
    after_range: LineRange,
) {
    let removed = std::mem::take(current_removed);
    let added = std::mem::take(current_added);
    let hunk_type = determine_hunk_type(&removed, &added);
    let context = build_context(before_lines, &before_range);
    let id = ids.next_id(hunk_type, &removed, &added, context.as_ref());

    hunks.push(Hunk {
        index: hunks.len(),
        short_id: id.clone(),
        id,
        hunk_type: hunk_type.to_string(),
        removed,
        added,
        before_range,
        after_range,
        context,
        semantic: SemanticInfo::default(),
    });
}

/// Hands out ids for one file's hunks, in diff order.
///
/// Exists so a hunk's identity is defined in exactly one place. `list` and
/// `select` must arrive at the same id for the same hunk or a spec stops
/// resolving, and the disambiguation below only works if both walk the file's
/// hunks in the same order with the same counter.
struct HunkIds {
    /// The path as it is hashed: this platform's separator rewritten to `/`.
    ///
    /// `list` reads its paths from jj, which prints `/` on every platform;
    /// `select` reads its from a `WalkDir` over the directories jj
    /// materialised, which yields the platform separator. On Windows the two
    /// therefore hand the *same* file two different strings, and an id agreed
    /// on by neither side is an id no spec can carry between them.
    ///
    /// Only `MAIN_SEPARATOR` is rewritten, never `\` unconditionally. On Unix
    /// a backslash is an ordinary character in a filename, so a file really
    /// named `back\slash.txt` would otherwise hash as `back/slash.txt` and
    /// every id in the repo root that anyone has written down would move.
    path: String,
    /// How many hunks with each content digest have already been handed an id.
    ///
    /// Two hunks in one file can be byte-identical *and* sit in identical
    /// context -- two copies of the same block, each surrounded by the same
    /// lines. Their content digests are equal, so this ordinal is the only
    /// thing that tells them apart.
    seen: HashMap<[u8; 32], u32>,
}

impl HunkIds {
    fn new(path: &str) -> Self {
        Self {
            path: path.replace(std::path::MAIN_SEPARATOR, "/"),
            seen: HashMap::new(),
        }
    }

    fn next_id(
        &mut self,
        hunk_type: &str,
        removed: &str,
        added: &str,
        context: Option<&HunkContext>,
    ) -> String {
        let digest = content_digest(&self.path, hunk_type, removed, added, context);
        let occurrence = self.seen.entry(digest).or_insert(0);
        let id = compute_hunk_id(&digest, *occurrence);
        *occurrence += 1;
        id
    }
}

/// Rebuild `after` keeping only the selected hunks, resetting the rest to
/// `before`.
///
/// The selection is resolved through [`get_hunks`] rather than against ids
/// recomputed inline. The two used to be separate copies of the same
/// expression, which is exactly how they would drift, and only the copy here
/// decides what a `split` actually writes.
pub fn apply_selected_hunks(
    path: &str,
    before: &str,
    after: &str,
    selected: &HunkSelection,
) -> Result<String> {
    let hunks = get_hunks(path, before, after);
    let keep = selected.resolve(&hunks)?;

    let diff = TextDiff::from_lines(before, after);
    let mut result = String::new();
    let mut hunk_idx = 0;
    let mut in_hunk = false;
    let mut hunk_before = String::new();
    let mut hunk_after = String::new();

    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                if in_hunk {
                    push_hunk(&mut result, &mut hunk_before, &mut hunk_after, keep.contains(&hunk_idx));
                    hunk_idx += 1;
                    in_hunk = false;
                }
                result.push_str(change.value());
            }
            ChangeTag::Delete => {
                in_hunk = true;
                hunk_before.push_str(change.value());
            }
            ChangeTag::Insert => {
                in_hunk = true;
                hunk_after.push_str(change.value());
            }
        }
    }

    if in_hunk {
        push_hunk(&mut result, &mut hunk_before, &mut hunk_after, keep.contains(&hunk_idx));
    }

    Ok(result)
}

fn push_hunk(result: &mut String, hunk_before: &mut String, hunk_after: &mut String, keep: bool) {
    let removed = std::mem::take(hunk_before);
    let added = std::mem::take(hunk_after);
    result.push_str(if keep { &added } else { &removed });
}

fn determine_hunk_type(removed: &str, added: &str) -> &'static str {
    match (removed.is_empty(), added.is_empty()) {
        (true, false) => "insert",
        (false, true) => "delete",
        _ => "replace",
    }
}

/// Hash of everything about a hunk except which occurrence of itself it is.
///
/// Two hunks share a digest exactly when they are the same change, to the same
/// file, sitting between the same lines. That is a real possibility -- two
/// copies of one block in a file, each edited identically -- so a digest is
/// not yet an identity. [`HunkIds`] turns it into one.
fn content_digest(
    path: &str,
    hunk_type: &str,
    removed: &str,
    added: &str,
    context: Option<&HunkContext>,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"path\0");
    hasher.update(path.as_bytes());
    hasher.update(b"\0type\0");
    hasher.update(hunk_type.as_bytes());
    hasher.update(b"\0removed\0");
    hasher.update(removed.as_bytes());
    hasher.update(b"\0added\0");
    hasher.update(added.as_bytes());
    match context {
        Some(ctx) => {
            hasher.update(b"\0context\0");
            hasher.update(ctx.before.as_bytes());
            hasher.update(b"\0");
            hasher.update(ctx.after.as_bytes());
        }
        None => {
            hasher.update(b"\0context\0");
        }
    }
    hasher.finalize().into()
}

/// The stable identity of one hunk: its content digest, plus which occurrence
/// of that digest it is within its file.
///
/// # What the id covers
///
/// The path, the hunk type, the removed and added lines, and the three lines
/// of context on either side ([`CONTEXT_LINES`]). Plus `occurrence`, which is
/// 0 for all but repeated-identical hunks and exists only to break their tie.
///
/// The path is in there so that the same edit applied to five files yields
/// five ids rather than one; `id()` is the identity predicate, so an id must
/// name exactly one hunk in the diff, not a set of them.
///
/// It is the **workspace-root-relative** path, not the one the command
/// printed. Which file a hunk is in is a property of the hunk; which directory
/// you were standing in when you asked is not, and hashing the latter made
/// `hunk-bff4817d` at the root and `hunk-228ac8df` from `sub/` two names for
/// one hunk. `list` and `select` convert into this frame before hashing, and
/// they must keep agreeing about it -- see [`HunkIds`].
///
/// # Durability, and what it costs
///
/// Including the context is a deliberate trade, not an oversight:
///
/// - An id survives most edits elsewhere in the file, including edits *inside*
///   its three-line context window. The context is read from the **parent**
///   side, so changing the working copy does not move it. Verified: with a
///   hunk at line 10, editing line 8 leaves the id untouched.
/// - What does change an id is an edit **adjacent** to the hunk, because the
///   two then merge into a single larger hunk with different text -- a
///   different hunk, correctly given a different id. The rule is about hunk
///   merging, not about a window.
/// - An id does **not** reliably survive a rebase. Rebasing onto a parent that
///   rewrites the surrounding lines changes the context and so the id;
///   rebasing onto one that does not touch the file leaves it identical. Treat
///   any rebase as invalidating rather than reasoning case by case.
///
/// Both are fine for the workflow these ids exist for: `list`, pick, `split`,
/// all against one working-copy state, where an id only has to stay valid for
/// as long as it takes to copy it out of one command and into the next. Within
/// that window, folding the context in makes ids markedly *more* distinct,
/// which is precisely what you want from a selector -- two identical one-line
/// edits in one file are told apart by where they sit, rather than falling
/// back on the occurrence ordinal.
///
/// # Do not "fix" this by dropping the context
///
/// An `absorb`-style command -- one that squashes each hunk into the commit
/// that last touched its lines -- will need a *second*, context-free identity:
/// path plus `+`/`-` lines only. It has to, because every squash it performs
/// shifts the context of the hunks it has not got to yet, so a context-bearing
/// id is invalidated by the command's own progress.
///
/// That is an argument for adding a coarser identity alongside this one, not
/// for weakening this one. Removing context here would make ordinary selection
/// coarser (every repeated one-line edit in a file collapses onto one digest,
/// leaving the occurrence ordinal to carry the whole load) to serve a command
/// that wants a different notion of sameness anyway.
fn compute_hunk_id(digest: &[u8; 32], occurrence: u32) -> String {
    // Occurrence 0 is the overwhelmingly common case, and hashing it in
    // unconditionally keeps one code path -- an id is always
    // `H(content_digest, occurrence)`, with nothing to special-case when
    // reading a diff of this function later.
    let mut hasher = Sha256::new();
    hasher.update(b"hunk\0");
    hasher.update(digest);
    hasher.update(b"\0occurrence\0");
    hasher.update(occurrence.to_le_bytes());

    format!("{HUNK_ID_PREFIX}{}", hex_encode(&hasher.finalize()))
}

/// Give every hunk the shortest `short_id` that still tells it from the others.
///
/// One length is chosen for the whole set, rather than jj's per-id lengths.
/// jj varies them because it abbreviates against a whole repository, where a
/// uniform length would be dictated by the single worst pair; a diff holds few
/// enough hunks that a uniform width costs a character or two at most, and it
/// keeps `list` output in columns.
///
/// Call this over every hunk a person could be looking at in one go -- the
/// whole diff, not one file -- or two files' hunks can be shown wearing the
/// same abbreviation.
pub fn assign_short_ids<'a>(hunks: impl IntoIterator<Item = &'a mut Hunk>) {
    let mut hunks: Vec<&mut Hunk> = hunks.into_iter().collect();
    let len = shortest_unique_prefix_len(hunks.iter().map(|hunk| hunk.id.as_str()));
    for hunk in &mut hunks {
        hunk.short_id = abbreviate(&hunk.id, len);
    }
}

/// Hex digits needed for every id in `ids` to be distinct, never fewer than
/// [`MIN_SHORT_ID_HEX`] and never more than a full id.
fn shortest_unique_prefix_len<'a>(ids: impl IntoIterator<Item = &'a str>) -> usize {
    let mut hex: Vec<&str> = ids.into_iter().map(id_hex).collect();
    hex.sort_unstable();

    // Sorted, the only ids that can share a long prefix are neighbours.
    let mut needed = MIN_SHORT_ID_HEX;
    for pair in hex.windows(2) {
        needed = needed.max(common_prefix_len(pair[0], pair[1]) + 1);
    }
    needed.min(HUNK_ID_HEX_LEN)
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count()
}

fn id_hex(id: &str) -> &str {
    id.strip_prefix(HUNK_ID_PREFIX).unwrap_or(id)
}

fn abbreviate(id: &str, hex_len: usize) -> String {
    let hex = id_hex(id);
    format!("{HUNK_ID_PREFIX}{}", &hex[..hex_len.min(hex.len())])
}

/// Normalize a hunk id string to `hunk-<hex>`.
///
/// Accepts the prefixes `hunk-`, `id:`, `sha:` and `sha256:`, or none at all,
/// and a trailing `...` -- which nothing emits any more, but which used to be
/// printed in diff headers, so ids copied out of an older listing still work.
///
/// The result may be shorter than 64 hex digits: an abbreviation is a valid
/// way to name a hunk, and callers resolve it against the diff themselves
/// ([`HunkSelection::resolve`], `id()`), where being ambiguous is an error.
pub fn normalize_hunk_id(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_end_matches("...");
    if trimmed.is_empty() {
        return None;
    }

    let hex = trimmed
        .strip_prefix(HUNK_ID_PREFIX)
        .or_else(|| trimmed.strip_prefix("id:"))
        .or_else(|| trimmed.strip_prefix("sha:"))
        .or_else(|| trimmed.strip_prefix("sha256:"))
        .unwrap_or(trimmed);

    if hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }

    Some(format!("{HUNK_ID_PREFIX}{}", hex.to_lowercase()))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(&mut out, "{:02x}", byte);
    }
    out
}

fn build_context(before_lines: &[&str], before_range: &LineRange) -> Option<HunkContext> {
    if before_lines.is_empty() {
        return None;
    }

    let start_idx = before_range
        .start
        .saturating_sub(1)
        .min(before_lines.len());
    let before_start = start_idx.saturating_sub(CONTEXT_LINES);
    let before_slice = before_lines.get(before_start..start_idx).unwrap_or(&[]);
    let after_start = (start_idx + before_range.length).min(before_lines.len());
    let after_end = (after_start + CONTEXT_LINES).min(before_lines.len());
    let after_slice = before_lines.get(after_start..after_end).unwrap_or(&[]);

    if before_slice.is_empty() && after_slice.is_empty() {
        return None;
    }

    Some(HunkContext {
        before: before_slice.concat(),
        after: after_slice.concat(),
    })
}

fn split_lines_with_endings(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut lines = Vec::new();
    let mut start = 0;
    for (idx, ch) in text.char_indices() {
        if ch == '\n' {
            lines.push(&text[start..=idx]);
            start = idx + 1;
        }
    }

    if start < text.len() {
        lines.push(&text[start..]);
    }

    lines
}

fn count_lines(value: &str) -> usize {
    if value.is_empty() {
        return 0;
    }

    let mut count = value.matches('\n').count();
    if !value.ends_with('\n') {
        count += 1;
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunk_id_is_sha256_hex_and_stable() {
        let before = "one\nTwo\nthree\n";
        let after = "one\nTWO\nthree\n";

        let hunks_first = get_hunks("f.txt", before, after);
        let hunks_second = get_hunks("f.txt", before, after);

        assert_eq!(hunks_first.len(), 1);
        assert_eq!(hunks_second.len(), 1);

        let id_first = &hunks_first[0].id;
        let id_second = &hunks_second[0].id;

        assert_eq!(id_first, id_second);
        assert!(id_first.starts_with(HUNK_ID_PREFIX));

        let hex = id_first.strip_prefix(HUNK_ID_PREFIX).unwrap();
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn hunk_id_changes_with_content() {
        let before = "alpha\nbravo\n";
        let after_one = "alpha\nbravo!\n";
        let after_two = "alpha\nbravo?\n";

        let id_one = get_hunks("f.txt", before, after_one)[0].id.clone();
        let id_two = get_hunks("f.txt", before, after_two)[0].id.clone();

        assert_ne!(id_one, id_two);
    }

    #[test]
    fn apply_selected_hunks_matches_by_id() {
        let before = "a\nb\nc\n";
        let after = "a\nb2\nc\n";

        let hunks = get_hunks("f.txt", before, after);
        let mut selection = HunkSelection::default();
        selection.ids.insert(hunks[0].id.clone());

        let selected_result = apply_selected_hunks("f.txt", before, after, &selection).unwrap();
        assert_eq!(selected_result, after);

        let empty_result =
            apply_selected_hunks("f.txt", before, after, &HunkSelection::default()).unwrap();
        assert_eq!(empty_result, before);
    }

    /// Two byte-identical blocks in one file, each surrounded by identical
    /// lines. Everything the id is hashed from is equal between them, so
    /// nothing but the occurrence ordinal can tell them apart.
    const DUPLICATE_BLOCKS_BEFORE: &str = "A\nB\nC\nDEL\nE\nF\nG\nA\nB\nC\nDEL\nE\nF\nG\n";
    const DUPLICATE_BLOCKS_AFTER: &str = "A\nB\nC\nE\nF\nG\nA\nB\nC\nE\nF\nG\n";

    #[test]
    fn identical_hunks_in_one_file_get_distinct_ids() {
        let hunks = get_hunks("f.txt", DUPLICATE_BLOCKS_BEFORE, DUPLICATE_BLOCKS_AFTER);
        assert_eq!(hunks.len(), 2, "fixture should produce two hunks");

        // Same change, same context: only the ordinal separates them.
        assert_eq!(hunks[0].removed, hunks[1].removed);
        assert_eq!(
            hunks[0].context.as_ref().map(|c| (&c.before, &c.after)),
            hunks[1].context.as_ref().map(|c| (&c.before, &c.after)),
        );

        let distinct: HashSet<&String> = hunks.iter().map(|hunk| &hunk.id).collect();
        assert_eq!(distinct.len(), 2, "duplicate hunks must not share an id");
    }

    #[test]
    fn an_id_selects_exactly_one_of_two_identical_hunks() {
        let hunks = get_hunks("f.txt", DUPLICATE_BLOCKS_BEFORE, DUPLICATE_BLOCKS_AFTER);

        for (index, hunk) in hunks.iter().enumerate() {
            let mut selection = HunkSelection::default();
            selection.ids.insert(hunk.id.clone());
            assert_eq!(
                selection.resolve(&hunks).unwrap(),
                HashSet::from([index]),
                "id of hunk {index} must resolve to that hunk alone"
            );
        }

        // ... and only that hunk's deletion is kept.
        let mut selection = HunkSelection::default();
        selection.ids.insert(hunks[0].id.clone());
        let applied =
            apply_selected_hunks("f.txt", DUPLICATE_BLOCKS_BEFORE, DUPLICATE_BLOCKS_AFTER, &selection)
                .unwrap();
        assert_eq!(applied, "A\nB\nC\nE\nF\nG\nA\nB\nC\nDEL\nE\nF\nG\n");
    }

    #[test]
    fn duplicate_hunk_ids_are_stable_across_runs() {
        let first = get_hunks("f.txt", DUPLICATE_BLOCKS_BEFORE, DUPLICATE_BLOCKS_AFTER);
        let second = get_hunks("f.txt", DUPLICATE_BLOCKS_BEFORE, DUPLICATE_BLOCKS_AFTER);

        let ids = |hunks: &[Hunk]| hunks.iter().map(|h| h.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&first), ids(&second));
    }

    #[test]
    fn the_same_edit_in_two_files_gets_two_ids() {
        let before = "a\nb\nc\n";
        let after = "a\nB\nc\n";

        let one = get_hunks("src/one.rs", before, after);
        let two = get_hunks("src/two.rs", before, after);

        assert_ne!(one[0].id, two[0].id);
    }

    #[test]
    fn short_ids_are_unique_and_at_least_the_floor() {
        let mut hunks = get_hunks("f.txt", DUPLICATE_BLOCKS_BEFORE, DUPLICATE_BLOCKS_AFTER);
        let mut others = get_hunks("g.txt", "a\nb\nc\n", "a\nB\nc\n");
        hunks.append(&mut others);

        assign_short_ids(hunks.iter_mut());

        let short: HashSet<&String> = hunks.iter().map(|hunk| &hunk.short_id).collect();
        assert_eq!(short.len(), hunks.len(), "short ids must be unique");

        for hunk in &hunks {
            let hex = hunk.short_id.strip_prefix(HUNK_ID_PREFIX).unwrap();
            assert!(hex.len() >= MIN_SHORT_ID_HEX, "{} is under the floor", hunk.short_id);
            assert!(hunk.id.starts_with(&hunk.short_id), "short id must prefix the full id");
        }
    }

    #[test]
    fn abbreviation_widens_past_the_floor_to_stay_unique() {
        // Ids sharing MIN_SHORT_ID_HEX + 2 hex digits need one more than that.
        let shared = "abcdef0123";
        let ids: Vec<String> = ["a", "b"]
            .iter()
            .map(|tail| format!("{HUNK_ID_PREFIX}{shared}{}", tail.repeat(54)))
            .collect();

        let len = shortest_unique_prefix_len(ids.iter().map(String::as_str));
        assert_eq!(len, shared.len() + 1);
    }

    #[test]
    fn a_short_id_resolves_and_an_ambiguous_prefix_is_an_error() {
        let hunks = get_hunks("f.txt", DUPLICATE_BLOCKS_BEFORE, DUPLICATE_BLOCKS_AFTER);

        let mut selection = HunkSelection::default();
        selection.ids.insert(hunks[1].short_id.clone());
        assert_eq!(selection.resolve(&hunks).unwrap(), HashSet::from([1]));

        // `hunk-` alone prefixes everything.
        let mut ambiguous = HunkSelection::default();
        ambiguous.ids.insert(HUNK_ID_PREFIX.to_string());
        let err = ambiguous.resolve(&hunks).unwrap_err().to_string();
        assert!(err.contains("ambiguous"), "{err}");
    }

    #[test]
    fn an_id_naming_nothing_in_this_file_is_not_an_error() {
        // Selections are applied file by file, so most ids belong elsewhere.
        let hunks = get_hunks("f.txt", "a\nb\n", "a\nB\n");
        let mut selection = HunkSelection::default();
        selection.ids.insert(format!("{HUNK_ID_PREFIX}{}", "0".repeat(64)));

        assert!(selection.resolve(&hunks).unwrap().is_empty());
    }

    #[test]
    fn normalize_hunk_id_accepts_prefixes() {
        let before = "foo\nbar\n";
        let after = "foo\nBAR\n";
        let id = get_hunks("f.txt", before, after)[0].id.clone();
        let hex = id.strip_prefix(HUNK_ID_PREFIX).unwrap();
        let expected = format!("{HUNK_ID_PREFIX}{hex}");

        assert_eq!(normalize_hunk_id(&format!("id:{hex}")).as_deref(), Some(expected.as_str()));
        assert_eq!(normalize_hunk_id(&format!("sha:{hex}")).as_deref(), Some(expected.as_str()));
        assert_eq!(normalize_hunk_id(&format!("sha256:{hex}")).as_deref(), Some(expected.as_str()));
        assert_eq!(normalize_hunk_id(hex).as_deref(), Some(expected.as_str()));
    }
}
