# jj-hunk

Programmatic hunk selection for [jj (Jujutsu)](https://github.com/martinvonz/jj).

Select specific diff hunks when splitting, committing, or squashing—without interactive UI. Designed for AI agents and automation.

## Installation

### 1. Install the binary

```bash
cargo install jj-hunk
```

### 2. Verify

```bash
jj-hunk --help
```

### Semantic build feature

The tree-sitter-backed hunkset predicates — `function()`, `scope()`, `annotation()`, `decorator()`,
`doc()`, `import()`, `toplevel()`, and `depth()` — are compiled behind a `semantic` cargo feature.
A binary built without it does not quietly return nothing; each of those predicates fails with an
explicit error:

```
Error: hunkset evaluation error: function() requires the 'semantic' feature (build with --features semantic)
```

If you hit that, reinstall with the feature enabled:

```bash
cargo install jj-hunk --features semantic
```

Everything else — `file`, `glob`, `extension`, `status`, `type`, `lines`, `content`, `added`,
`removed`, `id`, `all`, `none`, and the whole JSON/YAML spec format — works in any build.

## Quick Start

```bash
# See what hunks exist in your changes
jj-hunk list

# See hunks for a specific revision (diff vs parent)
# Note: revset must resolve to a single revision
jj-hunk list --rev @

# Emit YAML instead of JSON
jj-hunk list --format yaml

# List files only (hunk counts)
jj-hunk list --files

# Emit a spec template using short hunk ids
jj-hunk list --spec-template --format yaml

# Split changes: hunks 0,1 of foo.rs → first commit, rest → second
jj-hunk split '{"files": {"src/foo.rs": {"hunks": [0, 1]}}, "default": "reset"}' "first commit"

# Split a specific revision (not just working copy)
jj-hunk split -r @- '{"files": {"src/foo.rs": {"action": "keep"}}, "default": "reset"}' "first commit"

# Commit specific files, leave rest in working copy
jj-hunk commit '{"files": {"src/fix.rs": {"action": "keep"}}, "default": "reset"}' "bug fix"

# Squash specific changes into parent
jj-hunk squash '{"files": {"src/cleanup.rs": {"action": "keep"}}, "default": "reset"}'

# Squash a specific revision into its parent
jj-hunk squash -r @- '{"files": {"src/cleanup.rs": {"action": "keep"}}, "default": "reset"}'

# Keep only the named hunks in a revision, dropping the rest of its diff
jj-hunk diffedit 'file("src/foo.rs")' -r @-

# Undo the named hunks (ids come from the reversed view — see Restore)
jj-hunk restore 'id("hunk-5b300823")'

# Send each hunk back to the ancestor that last touched its lines
jj-hunk absorb --dry-run

# Read spec from a file (JSON or YAML)
jj-hunk split --spec-file spec.yaml "first commit"

# Read spec from stdin
cat spec.json | jj-hunk commit - "bug fix"
```

## Commands

| Command | Description |
|---------|-------------|
| `jj-hunk list [options]` | List hunks, files, or spec templates |
| `jj-hunk split [-r rev] <spec> <message>` | Split changes into two commits |
| `jj-hunk commit <spec> <message>` | Commit selected hunks |
| `jj-hunk squash [-r rev] <spec>` | Squash selected hunks into parent |
| `jj-hunk diffedit [-r rev] <spec>` | Keep only the selected hunks in a revision |
| `jj-hunk restore [-c rev] <spec>` | Undo the selected hunks |
| `jj-hunk absorb [-r rev] [<spec>]` | Move hunks into the ancestors that last touched their lines |

### What a named hunk means

The mutating verbs do **not** agree about this, and it is the easiest thing in the
tool to get backwards. The spec is the same in every case; what changes is what
happens to the hunks it names:

| Command | The hunks you name are... | The hunks you don't name... |
|---------|---------------------------|-----------------------------|
| `split` | put into the **split-off** commit | stay in the original revision |
| `commit` | **committed** | stay in the working copy |
| `squash` | **moved** into the parent | stay in the source revision |
| `diffedit` | **kept** | are discarded from the revision |
| `restore` | **undone** | are left alone |

`diffedit` and `restore` are near-inverses: `diffedit 'id(X)'` keeps only X,
`restore 'id(X)'` throws away only X. If a selection does the opposite of what you
expected, this table is why.

`absorb` is the odd one out — it does not keep or drop anything, it *routes*. See
[Absorb](#absorb).

### Choosing a revision

| Command | Revision flags |
|---------|----------------|
| `commit` | none — always the working copy |
| `split`, `squash`, `absorb` | `-r <rev>` (default `@`) |
| `diffedit` | `-r <rev>`, **or** `--from <rev>` / `-t`,`--to <rev>` |
| `restore` | `-c`,`--changes-in <rev>` (default `@`), **or** `--from <rev>` / `-t`,`--into <rev>` |

The two styles are mutually exclusive: `-r` cannot be combined with `--from`/`--to` on
`diffedit`, and `-c` cannot be combined with `--from`/`--into` on `restore`. Whichever
side of a `--from`/`--to` pair you leave out defaults to `@`.

### `--allow-empty`

`split`, `commit`, `squash`, `diffedit`, and `restore` all accept `--allow-empty`.
It means **an empty result is acceptable** — it does *not* turn off checking.

A spec entry that does not name anything real is still an error with the flag set: a
typo'd path, a stale id, an out-of-range index, and a path filed under a rename's old
name are all reported either way. `--allow-empty` only says that a selection which
legitimately keeps nothing should produce an empty commit instead of failing. Without
it, keeping nothing is an error — the main guard against a typo'd selector producing a
silent no-op.

### List options

- `--rev <revset>` — diff the revision against its parent (revset must resolve to a single revision)
- `--from <rev>` / `--to <rev>` — diff two revisions directly instead of a revision against its parent (each defaults to `@`). This is how you see the view `restore` names — see [Restore](#restore)
- `--format json|yaml|text|diff` — output format (default: json)
- `--include <glob>` / `--exclude <glob>` — filter paths (repeatable; one pattern per occurrence)
- `--group none|directory|extension|status` — group output
- `--binary skip|mark|include` — binary handling (default: mark)
- `--max-bytes <n>` / `--max-lines <n>` — truncate file contents before diffing
- `--spec <hunkset|json|yaml>` / `--spec-file <path>` — filter output with a hunkset expression or JSON/YAML spec
- `--files` — list files with hunk counts only
- `--spec-template` — emit a spec template (JSON/YAML only)

`<spec>` may be a hunkset expression (e.g. `'type(insert) & file("src/*.rs")'`), an inline JSON/YAML string, or `-` to read from stdin. The format is auto-detected. Use `--spec-file <path>` to read a JSON/YAML file (omit `<spec>` when using `--spec-file`).

### Truncation

`--max-lines <n>` keeps the first `n` lines of a file, terminators included; `--max-bytes <n>` keeps
the first `n` bytes, backing up only far enough to land on a UTF-8 character boundary. Given both,
lines apply first and bytes apply to what remains. Truncation happens on **both** sides before
diffing, so hunks past the cut do not exist at all — the file is not skipped, and it is not treated
as binary. A file already under the limit is untouched.

`--max-bytes` cuts at a byte offset, which can land mid-line and produce a final hunk that is an
artifact of the cut rather than a real change. `--max-lines` always cuts on a line boundary and
never does this — prefer it unless you specifically need a byte budget.

Everything a single `list` prints describes the same view of the diff: the hunk listing, the
`--files` count, and a `--spec`-filtered subset all agree, truncated or not.

Truncated files are marked: `truncated: true` on the file entry in JSON/YAML (omitted when false),
and ` [truncated]` in `--format text` headers alongside the existing `[binary]` marker. Truncation
is a no-op for binary files under the default `--binary mark`, since those are never diffed.

Both flags are on `list` only, deliberately. A hunk id is a hash of the text it was diffed from, so
ids computed from truncated content would not exist in the real diff; `split`/`commit`/`squash`
always read files whole. For the same reason `--spec-template` fails, naming the offending files,
if any file was actually truncated — rather than sort out which of the template's ids survive the
cut, it refuses to emit a template that `split` might reject.

### Diffedit

`diffedit` rewrites a revision to contain only the hunks you name, dropping the rest
of its diff. Its ids come from the ordinary forward view, `jj-hunk list -r <rev>`.

```bash
$ jj-hunk list --format text
M f.txt
  hunk 0 replace hunk-cbb1d936 (before 1+1 after 1+1)
    - one
    + ONE
  hunk 1 replace hunk-05ec3d94 (before 5+1 after 5+1)
    - five
    + FIVE

$ jj-hunk diffedit 'id("hunk-cbb1d936")'
$ jj-hunk list --format text
M f.txt
  hunk 0 replace hunk-cbb1d936 (before 1+1 after 1+1)
    - one
    + ONE
```

The `five` → `FIVE` change is gone from the revision. Use `--from`/`--to` in place of
`-r` to edit the diff between two revisions.

### Restore

`restore` is the inverse: the hunks you name are the ones **undone**, taken back from
another revision.

Starting from the same two-hunk diff as above, undoing the first one leaves the second
behind:

```bash
$ jj-hunk restore 'id("hunk-5b300823")'   # note: not the id list printed above
$ jj-hunk list --format text
M f.txt
  hunk 0 replace hunk-05ec3d94 (before 5+1 after 5+1)
    - five
    + FIVE
```

**Its ids come from the reversed diff, not from plain `list`.** This trips people up,
so it is worth being explicit. `jj restore` shows its diff editor the destination on
the left and the source on the right, and jj-hunk builds the spec against that same
`destination -> source` view — the opposite direction from `jj diff -r`. A hunk that
reads `- one / + ONE` in the forward view reads `- ONE / + one` in restore's view, and
being different text it has a **different id**:

```bash
$ jj-hunk list -r @ --format text            # forward view
  hunk 0 replace hunk-cbb1d936 (before 1+1 after 1+1)
    - one
    + ONE

$ jj-hunk list --from @ --to @- --format text  # the view restore names
  hunk 0 replace hunk-5b300823 (before 1+1 after 1+1)
    - ONE
    + one
```

So list the ids for a `restore` with `jj-hunk list --from <destination> --to <source>`
— for the default `-c @`, that is `jj-hunk list --from @ --to @-`. Feeding it an id
from the forward view fails cleanly rather than undoing the wrong thing:

```text
Error: hunkset evaluation error: hunk id 'hunk-cbb1d936' matches no hunk in this diff -- ids change when the hunk's content or its file changes, so one copied from an earlier listing may be stale. Run 'list' again for the current ids.
```

Path-level specs (`{"action": "keep"}`, `file()`, `glob()`) are unaffected — only ids
and content matching depend on the direction.

### Absorb

`absorb` routes each hunk into the mutable ancestor that last touched the lines it
changes, using `jj file annotate` to decide. Nothing is selected or discarded; hunks
just move to where they belong.

```bash
$ jj-hunk absorb --dry-run
absorb from rulnrwzm (no description)
  2 hunks: 2 moving into 2 ancestors, 0 staying

move into ylrxpsut c2: owns line 2
  f.txt:2  -1 +1  hunk-6c9ce678

move into xtlwxmny c3: owns line 8
  f.txt:8  -1 +1  hunk-a2f5649f

line numbers are in the parent of rulnrwzm; an insertion is listed at the line it goes before
--dry-run: nothing was changed
```

Drop `--dry-run` to apply it. The closing line names the undo, and it is an operation
restore rather than `jj undo`, because absorb is several rewrites:

```text
Absorbed 2 hunks into 2 revisions:
  ylrxpsut c2: owns line 2
  xtlwxmny c3: owns line 8
Undo all of it with: jj op restore 6ae777943f9e
```

The source revision is left **empty rather than abandoned**, so `@` stays where it is.

A hunk that cannot be routed unambiguously stays put, and the plan says why. Each of
these is a *stay*, not an error — absorb exits 0 and changes nothing for that hunk:

| Reason printed | Meaning |
|----------------|---------|
| `the N lines it changes were last touched by N commits` | The hunk spans lines owned by different ancestors |
| `it only adds lines, so no line of it blames to an ancestor` | A pure insertion — see below |
| `<path> is new in this revision, so none of its lines has any history` | Nothing to blame against |
| `<path> was renamed from <old>` | See below |

**Pure insertions stay by default.** An added line blames to nothing, so there is no
honest answer. `--insertions=surrounding` opts into jj's own rule instead — route the
insertion to the ancestor that owns the lines on both sides of it, when those agree:

```text
$ jj-hunk absorb --dry-run --insertions surrounding
  2 hunks: 1 moving into 1 ancestor, 1 staying

move into mlmsxszv c1: alpha and beta
  f.txt:5  -0 +1  hunk-0a508a3a
...
--insertions=surrounding: insertions are routed by the lines around them
```

**Renamed and copied files are refused.** A rename is a whole-file change that would
ride into whichever ancestor received the file's first moving hunk, quietly moving the
rename too. Absorb declines and tells you the fix:

```text
stay in kszqssku (no description)
  renamed.txt:20  -1 +1  hunk-5c133735
    renamed.txt was renamed from f.txt, and a rename is a whole-file change that would ride into the ancestor with the first hunk that moved -- commit the rename on its own first, then absorb
```

`absorb` takes an optional spec to absorb only some hunks; with no spec it considers
every hunk in the revision.

## Spec Format

Specs can be **JSON or YAML**. Inline JSON is convenient for short specs; use `--spec-file` or stdin for larger ones. You can select hunks by index (`hunks`) or by `ids` emitted by `jj-hunk list`. An id entry may be a full `hunk-<sha256>`, the abbreviated `short_id`, or any unambiguous prefix of one — see [Hunk IDs](#hunk-ids). `hunks` entries may also be id strings.

```json
{
  "files": {
    "path/to/file": {"hunks": [0, "hunk-1b26091b", 2]},
    "path/to/other": {"ids": ["hunk-8a30a9af"]},
    "path/to/another": {"action": "keep"},
    "path/to/skip": {"action": "reset"}
  },
  "default": "reset"
}
```

- `{"hunks": [indices|ids]}` — select by index (0-based) or id string
- `{"ids": ["hunk-8a30a9af"]}` — select hunks by id from `jj-hunk list`
- `{"action": "keep"}` — keep all changes in file
- `{"action": "reset"}` — discard all changes in file
- `{"from": "old/path"}` — source path of a renamed or copied file (see below)
- `"default"` — action for unlisted files (`"keep"` or `"reset"`)

`ids` and `hunks` are merged if both are provided. Use `jj-hunk list --spec-template` to generate an id-based starting spec.

File keys are matched in two spellings: the one `list` prints from your current
directory, and the **workspace-root-relative** one. So a spec generated at the repo
root applies from anywhere in the repo. The reverse does not hold — a spec generated
in a subdirectory can key a file as `../top.txt`, which names nothing from any other
directory, so prefer generating specs at the root when they need to travel.

Running from a subdirectory works:

```bash
$ cd src && jj-hunk list --format text
M foo.rs
  ...
$ jj-hunk split '{"files": {"foo.rs": {"action": "keep"}}, "default": "reset"}' "fix"
```

A spec **may name paths that are not in the diff**, so a checked-in allowlist stays
usable as files come and go. It only has to keep at least one path that is really
there; a spec whose every entry is absent selects nothing and is rejected:

```text
Error: spec does not resolve against the diff:
  not/here.rs: no such path in the diff
Those entries do not name exactly what they meant to. Check them against `jj-hunk list --spec-template`.
```

That forgiveness covers bare `{"action": "keep"}` entries. An entry carrying `ids` or
`hunks` under an absent path is **always** rejected, whatever else resolved — those ids
were read off a real diff, so the path is a typo rather than an idle allowlist entry.

### Changes with no hunks

Some changes have nothing to select *within*: binary files, symlinks, mode-only
changes, pure renames and copies, and adds or removes of an empty file. `list` reports
all of them, and `--spec-template` gives each one `{"action": "keep"}` so the template
covers the whole diff. They are all-or-nothing — address them with `action` (or let
`default` decide), never with `ids` or `hunks`:

```text
$ jj-hunk list --format text
M blob.bin [binary]
A brand_new_empty.txt
M config [symlink, whole-file only]
R moved_elsewhere.txt (moved.txt -> moved_elsewhere.txt)
M script.sh [mode 100644 -> 100755, not selectable]
M text.txt
  hunk 0 replace hunk-1a94e681 (before 2+1 after 2+1)
    - world
    + WORLD
```

`--spec-template` covers that whole diff: `text.txt` gets its `ids`, and each of the
other five gets `action: keep` — plus `from: moved.txt` on the rename. (`list` sorts its
files by path; `--spec-template` does not order its keys, so do not diff two templates
expecting a stable order.)

Four details worth knowing:

- `{"ids": []}` — keep nothing from this file — works on binaries, on symlinks, and on
  files whose parent is not valid UTF-8. The parent is restored byte for byte.
- A **mode change rides along with the file**: keep any of a file's hunks and its
  chmod comes too; reset the file and the chmod resets with it. It is never selectable
  on its own, hence `not selectable` in the listing above.
- A **symlink** is written as a link, never through one. `jj file show` prints nothing
  for a link, so a link pointed at a new target diffs empty against empty and carries no
  hunks at all — `list` marks it `[symlink, whole-file only]`, and `action`/`default` is
  the only way to select it.
- **A hunkset expression cannot name most of these.** `all()`, `file()` and `glob()`
  match hunks, and a binary is the only hunkless shape given a stand-in hunk to be
  matched by. A symlink, a rename, a mode-only flip and an empty add or remove are
  reachable from a JSON spec but not from a hunkset, so a hunkset-driven verb leaves
  them behind. Use `--spec-template`, which names every one of them, when a selection is
  meant to cover the whole diff.

### Renamed files: the `from` field

A file entry for a renamed or copied file carries an extra `from` field naming its source path. It
is keyed under the *new* path, and `--spec-template` emits it for you:

```yaml
files:
  new_name.txt:
    ids:
    - hunk-0a2e31bc
    from: old_name.txt
default: reset
```

Keying such an entry under the *old* path is an error, and the message names the path
to use instead:

```text
Error: spec does not resolve against the diff:
  old_name.txt: renamed to new_name.txt in this diff -- file the entry under new_name.txt instead
```

It exists because of how jj drives the tool. `jj-hunk select` is handed two directories — a "before"
and an "after" — and nothing else, so it cannot tell that `after/new_name.txt` is the same file as
`before/old_name.txt`. Without the link it recomputes the change as one whole-file insertion, which
matches none of the spec's hunk ids, and writes an empty file. `from` supplies the missing link.

`split`, `commit`, and `squash` fill `from` in automatically from jj's rename detection before
handing the spec off, so a hand-written spec that omits it still works. It is load-bearing on the
raw `jj --tool=jj-hunk` path (see [How It Works](#how-it-works)), where nothing pre-processes the
spec: omitting it there drops the renamed file from the commit entirely.

Note that `from` only appears when jj itself reports the file as renamed or copied. A move that
changes the file too much for jj's rename detection is reported as an ordinary add plus delete, and
the spec names both paths separately.

## Hunk IDs

A hunk id is `hunk-` followed by a SHA-256 over the hunk's **path**, its type, its removed and added
lines, and up to three lines of surrounding context from the *parent* side of the diff. (Two
byte-identical hunks in one file are told apart by a tiebreak ordinal, so an id always names exactly
one hunk.)

Every id has two written forms:

| Form | Length | Where it appears |
|------|--------|------------------|
| Full | `hunk-` + 64 hex | The `id` field in `--format json` and `--format yaml` |
| Short | `hunk-` + 8 hex (widened on collision) | The `short_id` field, `--format text`, `--format diff` headers, and `--spec-template` |

The short form is the shortest prefix that is still unique across the whole diff, and never fewer
than eight hex digits — the same floor jj uses for change and commit ids. If two hunks in one diff
would otherwise share a prefix, every short id in that diff widens together, so they stay in
columns.

The same hunk, seen through each format:

```text
$ jj-hunk list --format text
M src/lib.rs
  hunk 0 replace hunk-8a30a9af (before 2+1 after 2+1)
    - old_fn()
    + new_fn()

$ jj-hunk list --format json     # excerpt
    "id": "hunk-8a30a9af59936de30a9a364b3bce467052dfc7a0a12c52f106410b04723417ef",
    "short_id": "hunk-8a30a9af",
```

Anywhere an id is *accepted* — `ids` and `hunks` entries in a spec, and the `id()` predicate — the
full form, the short form, and any unambiguous prefix all work, so `hunk-8a3` is a fine way to name
that hunk in a small diff. A trailing `...` is tolerated too: nothing emits it any more, but older
diff headers did, and ids copied out of them still resolve.

`id(exact:"...")` turns off **prefix** resolution, and nothing else. Both written forms
still work under it — `id(exact:"hunk-8a30a9af")` and `id(exact:"hunk-8a30a9af5993…")`
each select their hunk — but a proper prefix of either no longer resolves. Reach for it
when a short id might grow ambiguous and you would rather be told than guess.

A prefix that names more than one hunk is an error listing the candidates, never a guess:

```text
$ jj-hunk list --spec 'id("hunk-8")'
Error: hunkset evaluation error: hunk id 'hunk-8' is ambiguous -- it matches 2 hunks: hunk-8a28fbf7 (wide.rs), hunk-8acca845 (wide.rs). Use more characters, or exact:"<full-id>".
```

A prefix that names **no** hunk is an error too, rather than an empty selection:

```text
$ jj-hunk list --spec 'id("hunk-deadbeef")'
Error: hunkset evaluation error: hunk id 'hunk-deadbeef' matches no hunk in this diff -- ids change when the hunk's content or its file changes, so one copied from an earlier listing may be stale. Run 'list' again for the current ids.
```

### How long an id stays valid

Short answer: for as long as it takes to copy it out of one command and into the next. That is the
workflow these ids are for — `list`, choose, then `split`/`commit`/`squash` against the same
working-copy state — and within it they are solid.

An id **survives**:

- other hunks appearing or disappearing elsewhere in the same file, including work by a concurrent
  agent — and including **inside its own context window**. The three context lines are read from the
  parent side, so editing one of them in your working copy adds a separate hunk and leaves this id
  alone. Only an *adjacent* edit disturbs it, and then because the two hunks merge (see below);
- edits to any other file;
- line numbers shifting. Positions are not hashed, so prepending six lines to a file leaves the ids
  of the hunks below it untouched.

An id **does not survive**:

- **renaming or moving the file.** The path is hashed, so the same edit under a new name is a new
  id. (This is also why one edit applied to several files yields a distinct id per file rather than
  one shared id, which is the point — `id()` is the identity predicate.)
- **an edit that changes the hunk itself.** Touching a line immediately adjacent to a hunk merges
  the two into one larger hunk with different text, and therefore a different id. A line or more of
  untouched code in between keeps them separate, and keeps the id.
- **a change to the parent's content around the hunk** — a rebase onto a parent that rewrote those
  lines, or squashing something into the parent. Context is read from the parent side, so it moves
  when the parent does. A rebase that leaves those lines alone does keep the id, but do not rely on
  it: treat any rebase as invalidating.

Folding the context in is a deliberate trade. It costs durability across rebases and buys
distinctness: two identical one-line edits in one file are told apart by where they sit rather than
by an ordinal, which is what you want from a selector.

**In practice: re-run `list` after you edit, and use the ids from that run.** Do not cache ids
across an editing session, a rebase, or a rename — regenerating them costs one command.

### When an id does not resolve

Every writing command — `split`, `commit`, `squash`, `diffedit`, `restore` — checks each spec entry
against the diff before doing anything, and refuses the whole operation if one does not name exactly
what it meant to:

```text
$ jj-hunk split '{"files": {"wide.rs": {"ids": ["hunk-deadbeef"]}}, "default": "reset"}' "nope"
Error: spec does not resolve against the diff:
  wide.rs: no hunk with id hunk-deadbeef
Those entries do not name exactly what they meant to. Check them against `jj-hunk list --spec-template`.
```

The middle line names the specific problem, one per bad entry:

| Line | Cause |
|------|-------|
| `no hunk with id hunk-deadbeef` | The id matches nothing in this diff — usually stale, see above |
| `id hunk-3 is ambiguous, it names 3 hunks -- use a longer prefix` | The prefix is too short *within that file* |
| `no hunk with index 99 (file has 6)` | An out-of-range `hunks` index |
| `no such path in the diff` | The path is absent. Always reported for an entry naming `ids`/`hunks`; for a bare `{"action": "keep"}`, only when no other entry named a path that is present |
| `renamed to new_name.txt in this diff -- file the entry under new_name.txt instead` | The entry is keyed by a rename's old path |

**`--allow-empty` does not skip this check.** It permits an empty *result*; every message above is
still reported with the flag set. See [`--allow-empty`](#--allow-empty).

`jj-hunk list --spec` applies the same resolution rules, so a stale id is reported there too rather
than silently selecting nothing — which is what makes `list --spec` a safe way to check a selector
before handing it to a command that writes.

## Hunkset Query Language

As an alternative to JSON/YAML specs, jj-hunk supports a **hunkset** query language inspired by jj's [filesets](https://docs.jj-vcs.dev/latest/filesets/) and [revsets](https://docs.jj-vcs.dev/latest/revsets/). Hunkset expressions are auto-detected (anything that doesn't start with `{` or `[`).

```bash
# Split: all insertions in Rust files
jj-hunk split 'type(insert) & glob("src/**/*.rs")' "add new code"

# List only hunks inside a specific function
jj-hunk list --spec 'function("parse_spec")'

# Verify a selection covers all changes in a scope
jj-hunk list --spec 'function("apply") ~ id("hunk-8a30a9af")'
# Empty output = the id covers everything in that function
```

### Operators

Hunkset expressions compose using set algebra, from lowest to highest precedence:

| Operator | Meaning | Example |
|----------|---------|---------|
| `x \| y` | Union | `type(insert) \| type(delete)` |
| `x & y` | Intersection | `type(insert) & glob("src/**")` |
| `x ~ y` | Difference (x but not y) | `all() ~ type(delete)` |
| `~x` | Negation (complement) | `~type(delete)` |
| `(x)` | Grouping | `(type(insert) \| type(replace)) & file("x")` |

Union and intersection chain freely. **Difference does not**: `a ~ b ~ c` is a parse error, and you
have to write `(a ~ b) ~ c`. This is a deliberate difference from jj's revsets, where `~` is an
ordinary left-associative infix operator.

`!` is not an operator here — `~` serves as both negation and difference. `all` and `none` may be
written bare, without parentheses, when they are the whole expression.

### Functions

#### File predicates

| Function | Description |
|----------|-------------|
| `file("path")` | Hunks in file matching path (exact match on the whole repo-relative path) |
| `glob("pattern")` | Hunks in files matching glob pattern |
| `extension("rs")` | Hunks in files with given extension, written without the leading dot |
| `status(modified)` | Hunks in files with given status (modified, added, removed, renamed, copied) |

`status()` takes a bare identifier rather than a string. A deleted file is `removed`; there is no
`deleted`. An unrecognised value is rejected with the list of valid ones.

##### Glob patterns

`glob()` follows jj's [fileset](https://docs.jj-vcs.dev/latest/filesets/) rules exactly, so a
pattern means the same thing here as in `jj diff -- 'glob:"..."'`. Patterns are anchored to the
whole repo-relative path:

| Pattern | Matches |
|---------|---------|
| `*.rs` | Top-level `.rs` files **only** — `*` never crosses `/` |
| `**/*.rs` | `.rs` files at any depth, including top level |
| `src/*.rs` | `.rs` files directly in `src/` |
| `src/**` | Everything under `src/`, recursively |
| `*.{c,h}` | Brace alternation |
| `*.[ch]` | Character classes, including `[a-z]`, `[!a-z]`, `[^a-z]` |

Details worth knowing:

- `*` and `?` never cross `/`. `?` matches exactly one **byte**, so `h?llo.rs` does not match
  `héllo.rs` (but `h??llo.rs` does).
- `**` is only recursive as a whole path component. Elsewhere it degrades to `*`, so `**.rs` is
  equivalent to `*.rs` and matches top-level files only.
- A trailing `/` is not a directory wildcard: `src/` is the literal path `src`. Use `src/**`.
- `\` escapes the next character on Unix (`a\*b` matches a file literally named `a*b`); a character
  class is the portable alternative (`[*]`, `[[]`).
- A malformed pattern is an **error**, not an empty match:

  ```text
  $ jj-hunk list --spec 'glob("[abc")'
  Error: hunkset evaluation error: invalid glob '[abc': unclosed '[' -- a character class needs a matching ']'
  ```

  This matters most under negation, where a silently-empty pattern would invert into matching
  *everything*. `~glob("[abc")` raises the same error rather than selecting the whole diff.

The same matcher backs `file(glob:"...")` and the `--include` / `--exclude` flags, and rejects a
malformed pattern the same way in all four places.

#### Hunk type

| Function | Description |
|----------|-------------|
| `type(insert)` | Insertions only |
| `type(delete)` | Deletions only |
| `type(replace)` | Replacements only |

#### Line ranges

| Function | Description |
|----------|-------------|
| `lines(10..20)` | Hunks touching lines 10-20 (before or after) |
| `before_line(10..20)` | Hunks in the "before" line range |
| `after_line(10..20)` | Hunks in the "after" line range |

**Ranges include both endpoints**, despite borrowing Rust's exclusive `..` spelling. `lines(10..20)`
includes line 20, and `lines(7..7)` selects the hunk on line 7. `depth()` ranges work the same way.

#### Content matching

| Function | Description |
|----------|-------------|
| `content("text")` | Hunks where added or removed text contains "text" |
| `added("text")` | Hunks where added text contains "text" |
| `removed("text")` | Hunks where removed text contains "text" |

The haystack is the hunk's own added and removed lines — not its context, and not the
rest of the file.

#### Binary files

A binary change has no hunks to look inside, so the predicates split in two. Half of
them can still name a binary file, and select it whole; the other half can never reach
one:

| Reach binary files (selected whole) | Never reach them |
|-------------------------------------|------------------|
| `all()`, `file()`, `glob()`, `extension()`, `status()`, and any negation | `content()`, `added()`, `removed()`, `lines()`, `id()` |

The consequence is worth stating plainly: **a selector built only from `content()` and
friends leaves every binary change behind.** If a selection is meant to cover the whole
diff, union in a file-level term — `content("TODO") | extension("png")` — or start from
`all()` and subtract.

```bash
$ jj-hunk list --spec 'all()' --format text
M blob.bin [binary]
M text.txt
  hunk 0 replace hunk-1a94e681 (before 2+1 after 2+1)
    - world
    + WORLD

$ jj-hunk list --spec 'content("WORLD")' --format text   # blob.bin is gone
M text.txt
  hunk 0 replace hunk-1a94e681 (before 2+1 after 2+1)
    - world
    + WORLD
```

#### Identity

| Function | Description |
|----------|-------------|
| `id("hunk-8a30a9af")` | Select by hunk ID (from `jj-hunk list`); full, short, or any unambiguous prefix |
| `id("hunk-8a30a9af", "hunk-1b26091b")` | Several IDs in one call; the forms may be mixed |
| `id(hunk-8a30a9af)` | The quotes are optional — an id is not a pattern |

A prefix matching more than one hunk — or the bare `hunk-` — is rejected rather than guessed at, and
so is a prefix matching **none**. See [Hunk IDs](#hunk-ids) for the two written forms and for how
long an id stays valid.

`id()` resolves ids; it does not pattern-match them. `exact:` is the only prefix it accepts, and it
narrows rather than matches — see [Hunk IDs](#hunk-ids). The others are rejected outright:

```text
$ jj-hunk list --spec 'id(regex:"8a28")'
Error: hunkset evaluation error: id() does not accept 'regex:"8a28"' -- valid values are: a plain id, or exact:"<id>" to rule out abbreviation -- id() resolves ids, it does not pattern-match them
```

#### Semantic (tree-sitter powered)

These predicates use tree-sitter to parse source files and extract structural information. Supported languages: Rust, Python, JavaScript, TypeScript, Go, C, C++, Java, Ruby, C#, Scala, Swift, PHP, Bash, Elixir, Erlang, Haskell, OCaml, Zig, Lua.

| Function | Description |
|----------|-------------|
| `function("name")` | Hunks inside a function/method named exactly `name` |
| `scope("name")` | Hunks inside a scope (class, struct, impl, module, etc.) named exactly `name` |
| `annotation("test")` | Hunks inside functions/scopes whose annotation/decorator text contains `test` |
| `decorator("route")` | Alias for `annotation()` |
| `doc()` | Hunks that are doc comments |
| `import()` | Hunks that are import/use/require statements |
| `toplevel()` | Hunks not inside any function or scope |
| `depth(0)` | Hunks at nesting depth 0 (top-level); accepts ranges like `depth(0..1)` |
| `all()` | All hunks |
| `none()` | No hunks |

`all()` and `none()` are ordinary predicates and work in any build. The rest of this table needs a
binary built with the `semantic` feature — see [Semantic build feature](#semantic-build-feature).

For a file in an unsupported language these predicates contribute nothing and say so on stderr:

```
warning: function() found no semantic metadata -- no parser is available for: notes.txt, only.kt.
The empty result reflects missing language support, not an absence of matches.
```

`toplevel()` and `depth()` exclude unparsed files as well, so a file nobody could parse is never
mistaken for genuinely top-level code.

#### Decorator-only changes attribute differently per language

When a hunk touches **only** a decorator/attribute line and not the function body, whether that line
counts as part of the function is decided by the language grammar, and the grammars disagree:

| Attributes to the FUNCTION | Attributes to the enclosing SCOPE only |
|---------------------------|----------------------------------------|
| Java, C#, Swift, Scala, PHP, JavaScript | Rust, Python, TypeScript (`.ts`, `.tsx`) |

So for a one-line edit to the `@Test(timeout = 1)` line above a method `my_test` inside a class
`Holder`, `function("my_test")` selects the hunk in Java but not in Rust, Python, or TypeScript;
`scope("Holder")` selects it in all of them. JavaScript and TypeScript disagree with each other on
byte-identical source. This is a real inconsistency, not a documented design: when a selection has
to cover decorators portably, prefer `scope()`, a line range, or explicit hunk ids.

### Pattern syntax

String arguments support pattern prefixes, following jj conventions. **The prefix goes outside the
quotes** — `regex:"..."`, not `"regex:..."`. Writing it inside the quotes is a parse error that
tells you to move it.

| Prefix | Meaning | Example |
|--------|---------|---------|
| (none) | Depends on the predicate — see below | `added("TODO")` |
| `exact:"text"` | Exact match | `file(exact:"src/lib.rs")` |
| `substring:"text"` | Substring match | `function(substring:"test")` |
| `glob:"pattern"` | Glob pattern | `file(glob:"src/**/*.rs")` |
| `regex:"pattern"` | Regular expression | `added(regex:"fn\\s+\\w+")` |

A bare quoted string is not uniformly "substring". The default depends on what is being matched:

| Predicate | Bare-string default | Consequence |
|-----------|--------------------|-------------|
| `function()`, `scope()` | **exact** | `function("alpha")` does not match `alpha_beta` |
| `file()`, `extension()` | **exact** | `file("lib.rs")` does not match `src/lib.rs` |
| `content()`, `added()`, `removed()` | substring | `added("TODO")` matches any line containing TODO |
| `annotation()`, `decorator()` | substring | `annotation("test")` matches `#[test]` |

To match a family of identifiers, opt in explicitly with `substring:`, `glob:`, or `regex:`.

Dropping the quotes changes nothing about matching. A bare word follows the same row of the table as
the same word quoted, so `added(200)` and `added("200")` are one predicate written two ways, both
matching any added line containing `200`. Quotes are only needed when the text contains characters
the parser would otherwise read as syntax.

One caveat on `glob:` with the content predicates: their haystack is hunk text, not a path, but the
glob matcher is the path matcher, so `*` still refuses to cross `/`. `added(glob:"*TODO*")` will not
match a line reading `// TODO`. Use `substring:` or `regex:` for content.

### Examples

```bash
# Separate test changes from implementation
jj-hunk split 'annotation("test")' "test: add unit tests"
jj-hunk split 'all() ~ annotation("test")' "feat: implementation"

# Commit only import changes
jj-hunk split 'import()' "chore: update imports"

# Commit doc changes separately
jj-hunk split 'doc()' "docs: update documentation"

# All changes in a specific class
jj-hunk split 'scope("UserService")' "refactor: update UserService"

# Top-level changes only (constants, types, not inside functions)
jj-hunk split 'toplevel() ~ import()' "refactor: update type definitions"

# Hunks adding TODO/FIXME comments
jj-hunk list --spec 'added(regex:"(TODO|FIXME)")'
```

## Example Output

```bash
$ jj-hunk list --format json
{
  "files": [
    {
      "path": "src/lib.rs",
      "status": "modified",
      "hunks": [
        {
          "index": 0,
          "id": "hunk-8a30a9af59936de30a9a364b3bce467052dfc7a0a12c52f106410b04723417ef",
          "short_id": "hunk-8a30a9af",
          "type": "replace",
          "removed": "old_fn()\n",
          "added": "new_fn()\n",
          "before": {"start": 2, "lines": 1},
          "after": {"start": 2, "lines": 1},
          "context": {"pre": "// prev\n", "post": "// next\n"}
        }
      ]
    },
    {
      "path": "src/main.rs",
      "status": "removed",
      "hunks": [
        {
          "index": 0,
          "id": "hunk-1b26091bd874d9fe00c61ef49b52052985205b17abc77b2c18f52184e276e388",
          "short_id": "hunk-1b26091b",
          "type": "delete",
          "removed": "dead_code()\n",
          "added": "",
          "before": {"start": 1, "lines": 1},
          "after": {"start": 1, "lines": 0}
        }
      ]
    }
  ]
}
```

- `files` is a list of file entries. Each entry includes `status`, optional `rename`, and `hunks`.
- Each hunk includes `index`, the full `id` (sha256), its abbreviated `short_id`, line ranges (`before`/`after`), and optional `context`. `--format text`, `--format diff`, and `--spec-template` print the short form; see [Hunk IDs](#hunk-ids).
- When grouped (`--group`), output uses `groups: [{name, files}]` instead of `files`.
- With `--files`, entries carry `hunk_count` instead of a `hunks` array.
- In a `semantic`-enabled build, hunks also carry `enclosing_function`, `enclosing_scope`, `annotations`, `nesting_depth`, and `is_analyzed` where the language could be parsed.

### List Modes

```bash
# Files-only summary
jj-hunk list --files --format text

# Spec template (ids, default reset)
jj-hunk list --spec-template --format yaml
```

`--spec-template` supports JSON and YAML only; `--format text` and `--format diff` are rejected.

### Diff Output

`--format diff` emits a unified patch whose `@@` headers carry the enclosing function name (when
semantic analysis is available) and the hunk's short id:

```diff
--- a/wide.rs
+++ b/wide.rs
@@ -4,7 +4,7 @@ func_2 [hunk-f5696093]
 
 
 fn func_2() {
-    let v = 2;
+    let v = 200;
 }
 
 
```

The id in the header is the short form and is written plain, with no trailing `...`. It can be
pasted straight into `id()` or a spec's `ids`. The function name needs a `semantic` build; without
one the header is the same minus that word — `@@ -4,7 +4,7 @@ [hunk-f5696093]`.

Combine it with `--spec` to export exactly the hunks a query selects. Adjacent hunks whose context
overlaps are emitted as a single `@@` block labelled with the first hunk's id, so the patch is not a
one-to-one rendering of the hunk list.

**Apply it with `git apply`, not `git am`.** The output is a bare unified diff with no mail headers,
so `git am` rejects it outright (`Patch format detection failed`). `git apply` handles it, including
added files, deleted files, CRLF line endings, and missing trailing newlines, all byte-for-byte.
Renames are the exception: the patch records a rename as an ordinary modification of the new path,
so it will not apply to a tree that still has the old path.

### Filtering and Grouping

```bash
jj-hunk list --include 'src/**' --exclude '**/*.test.rs' --group directory
```

`--include` and `--exclude` take one pattern per occurrence and use the same glob rules as
`glob()` above.

## How It Works

jj-hunk integrates with jj's `--tool` mechanism:

1. You run a jj-hunk verb (`split`, `commit`, `squash`, `diffedit`, `restore`, `absorb`) with a JSON/YAML spec
2. jj-hunk writes the spec to a temp file and sets `JJ_HUNK_SELECTION` env var
3. jj-hunk passes temporary `merge-tools.jj-hunk` config to jj
4. jj invokes `jj-hunk select $left $right` as the diff tool
5. jj-hunk reads the spec and modifies `$right` to include only selected hunks
6. jj snapshots the result

Step 3 is **pinned to the running binary**: jj-hunk supplies `program` and `edit-args`
itself, pointing at its own executable, so the tool it drives is always the one you
invoked. A `[merge-tools.jj-hunk]` block in your jj config is therefore ignored by these
verbs — harmless to leave in place, but it is not what makes them work, and editing it
will not change their behaviour. To drive a *different* tool, register it under another
name and call `jj` directly with `--tool=<that name>`.

For direct control with `jj` itself, pass the tool config explicitly:

```bash
echo '{"files": {"src/foo.rs": {"hunks": [0]}}}' > /tmp/spec.json
JJ_HUNK_SELECTION=/tmp/spec.json jj \
  --config 'merge-tools.jj-hunk.program="jj-hunk"' \
  --config 'merge-tools.jj-hunk.edit-args=["select", "$left", "$right"]' \
  split -i --tool=jj-hunk -m "message"
```

On this path the spec goes to `select` untouched — step 1 above is what fills in rename sources and
rejects a selection that matches nothing. So a spec written for direct use must name `from` itself
for every renamed file (see [Renamed files](#renamed-files-the-from-field)); omitting it drops the
file from the resulting commit.

## Use Cases

### AI Agents

The primary use case. AI agents can create clean, logical commits without interactive prompts. Instead of dumping all changes into one commit, an agent can:

1. Analyze changes with `jj-hunk list`
2. Group files by logical concern (schema, services, tests, etc.)
3. Split iteratively to create a narrative commit history

The JSON/YAML spec format is easy for LLMs to construct programmatically.

### Clean History Workflow

Reorganize messy development history into reviewer-friendly commits. Squash everything, then split by concern:

```bash
jj squash --from 'all:trunk()..@-' --into @
jj edit @
jj-hunk split '{"files": {"src/db/schema.ts": {"action": "keep"}}, "default": "reset"}' "feat: add schema"
jj-hunk split '{"files": {"src/api/routes.ts": {"action": "keep"}}, "default": "reset"}' "feat: add routes"
jj describe -m "feat: add UI"
```

See `.claude/commands/clean-history.md` for a complete workflow.

### CI/CD Automation

Script commit splitting in pipelines. Enforce commit hygiene rules, auto-split by file patterns, or validate that commits are properly scoped.

### Partial Commits

Keep experimental code in working copy while committing only the finished parts:

```bash
jj-hunk commit '{"files": {"src/fix.rs": {"action": "keep"}}, "default": "reset"}' "fix: handle edge case"
# Experimental changes remain uncommitted
```

## License

MIT
