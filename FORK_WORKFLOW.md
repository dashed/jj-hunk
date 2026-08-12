# jj-hunk Fork Workflow

This document describes what this fork of `jj-hunk` is, where its code came from,
what was fixed along the way, and how to work on it.

## Overview

`jj-hunk` selects diff hunks programmatically — for `split`, `commit`, `squash`,
`diffedit`, `restore` and `absorb` — with no interactive UI, so a script or an
agent can drive it. This fork is a **permanent divergence** from
[`laulauland/jj-hunk`](https://github.com/laulauland/jj-hunk). It adds the
hunkset query language, tree-sitter semantic predicates, three verbs upstream
lacks, test CI, and 528 tests where upstream has 17.

There is one branch, `main`. It is the default branch and the only place work
lands. Jujutsu (jj) drives it in colocated mode, so `git` works too.

### The four-branch era, and why it ended

Until 2026-08-11 the fork was four feature branches — `alberto/hunkset-lang`,
`alberto/tree-sitter-semantic`, `alberto/fork-customizations`, `alberto/test-ci`
— each based directly on a `main` pinned to the pristine upstream base
(`3643ee8`, v0.4.1), and combined by a four-parent integration merge on
`alberto/my-jj-hunk`. Every branch built and tested green on its own.

That structure existed to make **upstream rebases cheap**: rebase and validate
one branch at a time instead of debugging a four-way merge, and drop a feature
from the merge to turn it off. It was the right shape for a fork that expected to
track upstream.

It stopped being the right shape once the fork stopped expecting that. Upstream
has not moved since `3643ee8`; this fork is 45 commits past it, has rewritten
`src/commands.rs` in 24 of them, and has a verb set and a query language upstream
does not have. There is no rebase left to be cheap, and no PR that lands this
back. What remained was only the cost: every change had to be filed onto the
branch that owned its files, the merge rebuilt by hand afterwards, and a change
filed onto the wrong branch — or onto the right one without rebuilding the merge
— silently did nothing, with a test count that failed to go up as the only
symptom.

So `main` was fast-forwarded to the integration merge and is now the mainline;
the four pull requests closed as merged, and the branches are retired. The merge
commit `0a796e2` is still in the history and still names its four parents, which
is what keeps **Provenance** below attributable.

## Why This Fork Exists

Three projects solve non-interactive hunk selection for jj, and none of them is
the tool this fork wants to be:

| Project | Strength | Why not just use it |
|---|---|---|
| [`laulauland/jj-hunk`](https://github.com/laulauland/jj-hunk) | Only tool on the [official jj community-tools list](https://docs.jj-vcs.dev/latest/community_tools/); ships via crates.io, Homebrew, Nix. Fine hunk granularity. | Thin command surface, 17 tests, **no test CI**, slow release cadence. |
| [`sigma/jj-hunk`](https://github.com/sigma/jj-hunk) | The **hunkset query language** and tree-sitter semantics — the best ideas in this space. | All of it sits unmerged on branches, unreleased and never PR'd upstream. |
| [`mvzink/jj-hunk-tool`](https://github.com/mvzink/jj-hunk-tool) | Best engineering: 125 tests, `absorb`/`diffedit`/`restore`, sub-hunk line ranges, short IDs. | Different architecture; coarser git-style hunks; **no CI at all**; rejects renamed files outright. Its ID algorithm and patch slicer live in the `git-surgeon` crate, floated as `"0.1.14"` (i.e. `^0.1.14`) — and its documented install path, `cargo install --git`, re-resolves dependencies unless `--locked` is passed, so an upstream patch release could silently change every hunk ID. Also shells out to `patch(1)` at runtime. |

This fork takes `laulauland/jj-hunk` as its base — it keeps the distribution
channels and the community listing — then lands sigma's unmerged work, ports the
best of `jj-hunk-tool`'s ideas on top, and puts the result under test. What that
actually produced, and who is owed for which part of it, is **Provenance** below.

### Why this base, concretely

Both upstream tools drive jj through the **same `--tool` diff-editor callback
protocol**, so ideas port between them. The tiebreakers were hunk granularity and
dependency surface. On identical input:

```
             import sys  +  rewrite of load()      →  jj-hunk:      2 separable hunks
             (adjacent lines)                         jj-hunk-tool: 1 welded hunk
```

`jj-hunk` diffs with `similar` change-groups, so adjacent-but-unrelated edits stay
separable. `jj-hunk-tool` uses git-style 3-line context, which welds them together
and then needs sub-hunk line ranges to undo the damage. For splitting commits by
*concern*, finer default granularity is the more valuable property.

## Branch Structure

One branch: `main`.

```
◆  main — the whole fork: 8 verbs, 528 tests, CI
│
◇  3643ee8 — laulauland/jj-hunk v0.4.1, the base
```

`main` is the fast-forwarded integration merge, so the whole history is here:
the four retired branches' commits, the merge that combined them, and the
imports at the root. What that structure was and why it ended is under
**Overview** above.

Their names still appear in **Provenance** and **Fixed** below, where they say
*which commits* a piece of work shipped in. Those commits are all on `main`; the
branches are not — do not go looking for them.

## The `semantic` Feature

The tree-sitter analyzer is an ordinary **optional cargo feature, on by
default**:

```toml
[features]
default = ["semantic"]
semantic = ["dep:tree-sitter", "dep:tree-sitter-rust", …]   # 21 entries
```

Building with `--no-default-features` drops it. What that costs:

| | default | `--no-default-features` |
|---|---|---|
| crates in the dependency graph | 74 | 51 |
| tests | 528 | 435 |
| `function()` `scope()` `annotation()` `doc()` `import()` `toplevel()` `depth()` | evaluate | error |

The 23-crate difference is the `tree-sitter` runtime, its `tree-sitter-language`
shim, 20 grammar crates, and `streaming-iterator`. Everything else — the whole
query language, all eight verbs, and every file, line, content and identity
predicate — is unaffected.

The seven semantic predicates do **not** quietly return nothing when the feature
is off. They fail:

```
$ jj-hunk list --spec 'function("x")'      # binary built --no-default-features
Error: hunkset evaluation error: function() requires the 'semantic' feature (build with --features semantic)
```

That is the entire point of the `require_semantic` gate in
`src/hunkset/eval.rs`, and it is why CI runs both modes as first-class builds
rather than treating the second as a smoke test. Silently returning nothing is
the failure mode this fork has spent the most effort removing; a build flag is
not an excuse to reintroduce it.

**Historical note.** This feature used to be a *contract between branches*:
`alberto/hunkset-lang` declared `semantic = []`, an empty feature, while
`alberto/tree-sitter-semantic` redefined the same name with the grammar
dependencies. That let each branch build alone, and the integration merge simply
took the grammar-bearing definition. The split definition had no other purpose,
and with one branch it is gone — `Cargo.toml` now carries one definition, the
real one. What survives is the `#[cfg]` seam, which earns its place on the
merits above rather than as a packaging trick.

## Provenance

Most of what this fork carries is imported, reorganized, and verified rather
than original. The base for everything is `laulauland/jj-hunk` at `3643ee8`
(v0.4.1) — which is also `upstream/main` and `sigma/main`.

The fork branch names below are **retired**; they say which commits carried
which work, and every one of those commits is on `main` now. The commit ids
that live outside this repo resolve only while the remotes described under
**Remotes** below stay configured.

**The import copied trees, not commits.** There are exactly two import commits,
both rooted directly at `3643ee8`, and their files are byte-identical to the
source trees — same blob SHAs, not merely the same content. So a per-*commit*
attribution was never going to be exact; what follows is per-tree, which is
checkable.

| Fork branch | Import commit | Source tree | Content taken |
|---|---|---|---|
| `alberto/tree-sitter-semantic` | `49264d0` | `sigma/tree-sitter-semantic` @ `59143b6` | `src/semantic.rs` verbatim (blob `21b78b9b`), plus the tree-sitter deps and the `semantic` feature list |
| `alberto/hunkset-lang` | `6c56150` | `sigma/dev` @ `6844166` (the roll-up merge) | everything else, byte-identical to that tree: `commands.rs`, `diff.rs`, `glob.rs`, `spec.rs`, all of `hunkset/`, and the README hunkset section |

Note the two sources differ. `semantic.rs` matches the **topic branch** tip
`59143b6`, not `sigma/dev` — the two differ by one `#[allow(dead_code)]` line.
Everything else matches only `sigma/dev`'s tip, because just that roll-up merge
has both the `semantic-features` line and `diff-output-format` in one tree.

`sigma/dev` is 13 commits over the base, two of them merges, carrying **three**
feature lines: `hunkset-language` (`3fafa16`), `tree-sitter-semantic`
(`59143b6`), and `diff-output-format` (`8decd9d`). After the first merge, single
commits edit the query language and the analyzer together, so the concerns
cannot be separated by picking commits — the fork had to split them at the file
level. That is why this is a 3→2 reorganization rather than an unpicking.

**A caution if you re-derive this table.** sigma's commits are squashes whose
subjects often describe work that is not in their diff. `558f184` is titled
"Python docstring detection" and touches no `semantic.rs` at all (that logic
already shipped in `59143b6`); `4e3b820` is titled "semantic predicates" and is
entirely hunkset evaluator code. An earlier version of this table filed both by
subject, and got `558f184` wrong. Read the diffs.

Deliberately **not** imported:

- `6844166`'s `SKILL.md` revision. The import shipped upstream's `SKILL.md`
  untouched (same blob, `d9075de0`), and the fork later wrote its own.
- `upstream/codex/fix-merge-list` (`6d7d44e`), which handles merge commits by
  materializing trees through a hidden subcommand and
  `JJ_HUNK_LIST_REQUEST`/`JJ_HUNK_LIST_OUTPUT`. None of those identifiers exist
  here; `063d1ee` rejects merge revisions instead, naming the parents. A reader
  should not assume that work was taken.

**`mvzink/jj-hunk-tool` shares no history with this repo** — there is no merge
base — and none of its code is here: none of its dependencies, and none of its
identifiers. Its influence is real but conceptual and disclosed: it has the same
three verbs this fork adds, and a hunk fingerprint over "path plus non-context
lines" that `absorb` uses the same device for. The implementations are separate
and deliberately diverge; `c65ab6d` names jj-hunk-tool and argues against its
routing fallback.

### What the fork actually contributed

**The reorganization** described above: two branches that each build and test
green standalone in their own default configuration — `hunkset-lang` at 435
tests, `tree-sitter-semantic` at 103. Two caveats worth stating plainly, because
"builds green alone" overstates without them: `hunkset-lang` does **not** compile
under `--all-features`, by design, since its `semantic = []` is a contract-only
flag whose analyzer lives on the other branch; and that flag is inert there —
no tested configuration turns it on. The feature seam itself (`require_semantic`,
the `#[cfg]` pair) is sigma's and is byte-identical here. Using it as a *branch
boundary* is the fork's decision, and it is a packaging one, not new code.

**The verification effort, which is the bulk of it.** Upstream and `sigma/dev`
share a byte-identical `tests/integration.rs` — 521 lines, 11 tests. sigma added
a 3,737-line production codebase and **no integration tests at all**. This fork's
is 7,084 lines and 260 tests. Alongside them, **26 fix commits** (23 on
`hunkset-lang`, 3 on `tree-sitter-semantic`), and the target was mostly not the
imports: `src/commands.rs`, which is upstream's code, is touched by 24 of the 34
commits on `hunkset-lang`, and what turned up there was the serious kind —
writing outside the working tree, silently losing files and mode changes,
symlink corruption, CRLF loss.

**`src/absorb.rs`** (1,371 lines), and the `diffedit` and `restore` verbs, taking
the binary from five commands to eight. The verb *set* is mvzink's, as above;
the implementations are ours. Plus flags no ancestor has: `--allow-empty`,
`list --from/--to`, absorb's `--dry-run` and `--insertions`.

**`.github/workflows/ci.yml`** — the only test CI in any of the three projects.

**The `SKILL.md` rewrite.** The import left a 345-line document describing a
binary that no longer existed; it is now 840 lines describing the eight-command
one that does. (The README hunkset section, by contrast, arrived *with* the
import and was never missing — an earlier version of this file claimed credit
for it in error.)

### Numbers, and what each counts

| | base `3643ee8` | `sigma/dev` | this fork |
|---|---|---|---|
| tests, default build | 17 | 83 | **528** |
| tests, `--no-default-features` | — | — | 435 |
| `tests/integration.rs` | 521 lines, 11 tests | *byte-identical to base* | 7,084 lines, 260 tests |
| `src/` production lines | 1,896 | 3,737 | 8,745 |
| test CI | none | none | yes |

On `main` `default = ["semantic"]`, so `--all-features` and the default build
are the same thing; 528 is simply the default. 435 is the semantic-off build.

Commits over base: 45, as merged — `hunkset-lang` 34 (23 fix, 5 docs, 3 feat,
2 test, 1 chore), `tree-sitter-semantic` 5, `fork-customizations` 4, `test-ci` 2,
plus the integration merge itself.

**"Bugs fixed" is deliberately not reported.** 14 of those subjects bundle
several independent repairs — `fix: symlink corruption, argument validation, id
ambiguity, identifier matching` is four — so any bug count would be a guess.
Commit counts are what can be checked, so commit counts are what appear here.

## Remotes

```bash
git remote -v
# origin    https://github.com/dashed/jj-hunk.git      ← this fork
# upstream  git@github.com:laulauland/jj-hunk.git      ← the base, v0.4.1
# sigma     https://github.com/sigma/jj-hunk.git       ← source of the imported features
# mvzink    https://github.com/mvzink/jj-hunk-tool.git ← reference for ported ideas
```

`upstream`, `sigma` and `mvzink` are kept deliberately, as **read-only
reference**. Nothing tracks them, nothing merges from them, and no bookmark here
follows one. Before anyone tidies them away, the reason they are still
configured:

**Provenance above is only checkable while they exist.** Every commit it cites —
`59143b6`, `6844166`, `3fafa16`, `8decd9d`, `558f184`, `4e3b820` on sigma,
`6d7d44e` on upstream — is reachable from **no branch in this repo**. The only
refs carrying them are these remotes' remote-tracking refs, plus a handful of
`refs/jj/keep/*` pins jj created when it imported them, which are an
implementation detail rather than a record to rely on. That section's claim
that `mvzink/jj-hunk-tool` shares no history with this repo is likewise checked
by `git merge-base main mvzink/main` exiting 1, which needs the remote to mean
anything at all.

Remove the remotes and that section degrades into a list of hashes nobody can
look up. Re-adding and re-fetching would restore it — but only for as long as
those repositories exist and still carry those refs, which is not a guarantee
anyone here controls.

`mvzink` in particular is an unrelated codebase fetched into the same object
store purely so its history is greppable when porting ideas.

Pushing:

```bash
jj git push --remote origin --bookmark main
```

## Jujutsu (jj) Setup

This repo uses jj in colocated mode, meaning both `jj` and `git` commands work.

### Why jj?

- **Automatic rebasing**: When you update a parent, descendants auto-rebase
- **First-class conflicts**: Conflicts are stored in commits, resolve when convenient
- **Operation log**: Every operation can be undone with `jj undo`
- **Change IDs**: Stable identifiers that survive rebases (unlike git commit hashes)

### Key jj Concepts

```bash
# Bookmarks = Git branches
jj bookmark list                    # List all bookmarks

# Working copy IS a commit
jj status                           # See current state
jj diff                             # See changes in working copy

# Change IDs vs Commit IDs
# - Change ID (e.g., lzwquzuo): stable across rewrites
# - Commit ID (e.g., 1a739d9b): changes when commit is modified
```

## Adding a New Feature

Work on `main`. There is no integration merge to fold anything into, and no
branch that owns a particular file.

```bash
# jj auto-tracks the working copy; start a new commit per logical unit
# rather than growing one large one.
jj new main -m "feat: description of feature"
jj new -m "next part of feature"

# Verify. Both modes, because they are genuinely different builds.
cargo test
cargo test --no-default-features
cargo clippy --locked --all-targets      # currently clean; keep it that way

jj bookmark set main -r @-                # @ is the empty working copy on top
jj git push --remote origin --bookmark main
```

A short-lived branch off `main` is fine when the work wants reviewing in
isolation — rebase it onto `main` and move `main` forward when it lands. What is
not fine is re-growing a long-lived parallel line by habit: that is the structure
this fork just retired, and it costs the same thing it cost before.

**Do not create empty placeholder commits for planned work.** A bookmark on an
empty commit looks like work that exists. Track intent in the roadmap below and
create the commit when you have code for it.

## Fixed

Each entry names the branch and commit subject the work shipped under. Those
branches are retired and the commits are all on `main`; the names are kept
because they are how you find the commit, not because there is anywhere left to
check out.

### `--format diff` produced patches that could not be applied

Shipped on `alberto/hunkset-lang` as `fix(diff-format)`.

`--format diff` emitted **zero context lines**. `git apply` refuses zero-context
patches unless given `--unidiff-zero`, and `git am` has no such flag — so the
output could not be used the way the README and SKILL.md claimed. Without that
flag the failure mode differed by hunk type: replacements and deletions were
loudly rejected, but an insertion was silently *relocated*:

```
expected: L1 L2 INS L3 L4 L5 L6
actual:   L1 L2 L3 L4 L5 L6 INS
```

Worth recording how this was mis-diagnosed the first time. The obvious suspect
was the hunk header, since jj-hunk wrote `@@ -3,0 +3,1 @@` where git writes
`@@ -2,0 +3 @@` for the same change. That difference is real but **harmless** —
tested in isolation under `--unidiff-zero`, both forms apply identically and
produce the correct file. Missing context was the entire defect. Reading a
difference and assuming it is the cause is exactly the error the fix's tests are
designed to prevent.

The fix emits the up-to-3 context lines already recorded on every hunk, and
merges hunks whose context windows touch into a single `@@` block — separate
blocks with overlapping ranges are an invalid patch. Header lengths are counted
from the lines actually emitted rather than derived from the hunk ranges, so the
header and body cannot drift apart.

Verified end-to-end: a real `git am` of the output now applies cleanly.

### Selectors that matched nothing failed silently

Shipped on `alberto/hunkset-lang` as `fix(hunkset)`.

A misspelled, malformed, or misused hunkset expression selected nothing and
exited **0**. Driving `split` in a loop, that produced empty junk commits and
reported success — the worst failure mode for a tool built for agents.

Five silent failures, each now an error:

| Input | Was | Now |
|---|---|---|
| `type(insertt)` | 0 hunks, rc 0 | `type() does not accept 'insertt' -- valid values are: insert, delete, replace` |
| `status(bogus)` | 0 hunks, rc 0 | same shape, listing valid statuses |
| `depth(abc)` | 0 hunks, rc 0 | `depth() does not accept 'abc'` |
| `content("regex:import")` | matched literal text | `pattern prefix 'regex:' must go outside the quotes` |
| `file(substring:"a.p")` | prefix discarded | honoured |

The prefix fix is the structural one. `compile_exact` and `eval_glob` used to
rewrite a pattern's kind unconditionally, unable to tell a user-written
`substring:` from the parser's default for a bare string. `StringPattern` now
records whether the kind was **requested or inferred**, and only inferred kinds
may be overridden — so `file("a.py")` still matches exactly while
`file(substring:"a.p")` does what it says.

`depth()` is validated *before* the semantic feature gate: a malformed query is
malformed regardless of build flags. Watch for this when adding validation —
the first attempt rejected the documented `depth(0)` form, because `0` arrives
as a `Pattern` holding `"0"` rather than a `Range`. An existing unit test caught
it.

Syntax errors previously fell through `is_hunkset()`'s trial parse into the
JSON parser, so a stray paren reported `Failed to parse spec as JSON`.
`is_hunkset()` now sniffs structurally, which makes the caret that
`HunksetError::display_with_context` already built reachable for the first time:

```
$ jj-hunk list --spec 'type(insert'
Error: failed to parse hunkset:
type(insert
           ^
expected RParen, got end of input
```

Finally, `split`/`commit`/`squash` refuse a selection that keeps nothing, with
`--allow-empty` to opt out. The two are deliberately distinct: `--allow-empty`
permits a *valid* selector that matches nothing, and never excuses a typo.
`list` is unchanged — it is read-only, so an empty result is legitimate output
rather than a failure.

### `toplevel()` and `depth()` disagreed with each other

Shipped as `fix(semantic)` on `alberto/tree-sitter-semantic` and
`fix(hunkset)` on `alberto/hunkset-lang` — one defect on each side of the
feature seam.

Two bugs pushed the same pair of predicates in opposite directions.

**Python under-reported.** tree-sitter-python names its ROOT node `module`,
and `module` was listed in Python's `scope_kinds`. The root contains every
line, so every Python hunk came back non-top-level at depth >= 1 and both
predicates excluded all Python. Ruby lists `"module"` too, but that is a real
Ruby construct — its root is `program` — so Ruby was already correct.

**Unparsed files over-reported.** A file with no language config fell back to
`SemanticInfo::default()`: `nesting_depth` 0 but `is_toplevel` false. So a hunk
in `notes.txt` matched `depth(0)` while failing `toplevel()`.

```
before                              after
  a.py       toplevel=false depth=1   a.py       toplevel=true  depth=0 analyzed=true
  a.rs       toplevel=true  depth=0   a.rs       toplevel=true  depth=0 analyzed=true
  notes.txt  toplevel=false depth=0   notes.txt  toplevel=false depth=0 analyzed=false
             ^ matched depth(0)                  ^ matched by neither
```

`SemanticContext`/`SemanticInfo` now carry `is_analyzed`, and both predicates
require it, so "never parsed" is distinguishable from "parsed, and top level".

A regression test walks seven languages asserting a lone top-level statement
really is top level — that guards every config against the root-node mistake
rather than just fixing the one instance.

This also restored the **warning the README always promised**. sigma
implemented one on `sigma/diff-output-format` and dropped it in the refactor
into `src/hunkset/`; this version keys off `is_analyzed` instead of guessing
from absent metadata, and covers every semantic predicate rather than just
`function()` and `scope()`:

```
warning: function() found no semantic metadata -- no parser is available for:
notes.txt. The empty result reflects missing language support, not an absence
of matches.
```

It stays quiet when any file was analyzed, since an empty result is then a real
answer about real metadata.

Note the test-visibility trap this exposed. The four integration tests here are
`#[cfg(feature = "semantic")]`-gated, so they compile away entirely under
`--no-default-features`. When the query language and the analyzer lived on
separate branches this was a cross-branch trap — the branch declaring
`semantic = []` could not observe semantic behaviour at all, so a semantic
change tested only there was not tested. One branch retires that particular
shape of it, but not the underlying one: a green `--no-default-features` run
says nothing about any of these tests, which is why CI runs both modes rather
than picking one.

### Nothing ran the tests

Shipped as `alberto/test-ci`.

Upstream runs CI **only on tags**, to build release bottles — tests never ran
on a push or a pull request. By the time the bug hunt finished there were 314
of them and nothing executed any of them.

`.github/workflows/ci.yml` runs the suite on Linux and macOS in both feature
modes, because the two are genuinely different builds: the default one compiles
21 tree-sitter grammars and enables the semantic predicates, while
`--no-default-features` exercises the feature-contract path where those
predicates must error rather than silently return nothing.

The integration tests drive a **real jj binary** — `jj git init`, the `--tool`
diff-editor protocol, real repos — so jj is installed from its release tarball
and **pinned to 0.44.0**, the version the tests were written against. Pinning is
deliberate: these tests assert on jj's output format and revset behaviour, so an
unpinned jj would turn an upstream jj release into a mystery failure here.

`RUSTFLAGS=-Dwarnings` gates the build; the crate is rustc-warning-free in every
mode and that is worth keeping. Clippy runs **advisory only** — it still reports
lints inherited from upstream, and gating on those would make someone else's
debt the first red build. Formatting is not checked at all: no file in the repo
is rustfmt-clean, so a gate would be pure noise. Turn clippy into a gate once
the backlog is cleared.

One implementation note worth keeping: the release tarball stores its members
with a leading `./`, and GNU and BSD tar disagree about whether `jj` matches
`./jj`. The workflow extracts the whole archive and moves the binary out rather
than naming a member.

### Two identical blocks shared one hunk id

Shipped on `alberto/hunkset-lang` as `fix(diff)`; the `alberto/short-ids` branch
the roadmap called for was never needed as a separate branch.

A hunk id hashed the change and its context, so a file with two identical
blocks — easy to produce in JSON or in repetitive code — had two hunks and one
id between them. That id was the only handle anything downstream had.
`--spec-template` printed it twice, so one of the two hunks could not be named
at all; and because `hunkset::to_spec` turns matched hunks into ids, *every*
hunkset expression over such a file over-selected — `before_line(1..8)` matched
the first block and quietly took its twin fifty lines later.

An id is now `H(path, type, -lines, +lines, context, occurrence)`. `path` keeps
the same edit in five files from collapsing onto one id, since `id()` is the
identity predicate and has to name exactly one hunk. `occurrence` counts earlier
hunks in the file with the same digest; it is 0 for everything but the
repeated-identical case, where it is the only thing left to tell the copies
apart. `get_hunks` and `apply_selected_hunks` had held separate copies of the id
expression — which is how they would have drifted — so application now resolves
through `get_hunks` and applies by index.

Full ids stay 64 hex digits in JSON and YAML as the stable contract. Text, diff
and `--spec-template` output show a `short_id` beside them, abbreviated the way
jj abbreviates change ids: the shortest prefix at which every id in the *whole
diff* is distinct, floored at 8 hex digits to match jj's `format_short_id`, and
widened rather than truncated when that is not enough. Any unambiguous prefix of
either form is accepted on input, and an ambiguous one is an error rather than a
multi-select on all four paths that can see it.

The lasting part is the durability decision, because the roadmap entry that
asked for this work posed it as an either/or — context-bearing *or*
context-free, "pick one" — and the answer is both, for different jobs. It also
had the mechanics wrong, which `fix(id)` corrected in the code comment: the
context is read from the **parent** side, so editing inside a hunk's context
window does not move its id. What moves an id is an edit *adjacent* to the
hunk, because the two then merge into one larger hunk — a different hunk, and
correctly a different id. Nor does a rebase reliably invalidate an id: onto a
parent that rewrites the surrounding lines it does, onto one that leaves the
file alone it does not. Treat any rebase as invalidating rather than reasoning
case by case.

So hashing the context still *looks* like the defect, and it is not. It is why
two identical one-line edits in one file are told apart by where they sit
instead of falling back on the occurrence ordinal. These ids exist for
list-pick-split against a single working-copy state, where an id only has to
stay valid long enough to be copied out of one command and into the next, and
within that window folding the context in makes them markedly *more* distinct.
**The context stays.** What absorb needs is a second, context-free identity
beside this one — not a weakening of it.

### The verbs upstream lacks, and the second identity absorb needed

Shipped on `alberto/hunkset-lang` as `feat: add the diffedit and restore verbs`
and `feat(absorb)`, closing the `alberto/jj-verbs` roadmap entry. The command
surface is now eight verbs: `list`, `select`, `split`, `commit`, `squash`,
`diffedit`, `restore`, `absorb`.

**A named hunk does not mean the same thing in every verb.**

```
split     the named hunks go into the split-off commit
squash    the named hunks move to the destination
diffedit  the named hunks are KEPT
restore   the named hunks are UNDONE
```

That inversion is not a local choice; it is what jj shows the diff editor. `jj
restore` presents *destination → source*, the reverse of `jj diff -r`, and its
right side starts out fully restored — so keeping a hunk is exactly what lets
the restoration stand. The trap underneath it is quieter: a hunk id is a hash of
the text it was computed from, so a spec built against the forward diff names
nothing at all in restore's view. It would not misfire loudly, it would select
nothing. Each command therefore states the diff it builds its spec from as an
explicit `DiffTarget`, and `list` grew the matching `--from`/`--to` so that
restore's ids are visible from some command at all.

**Absorb proved the prediction above.** It routes each hunk into the mutable
ancestor whose lines it changes — `jj file annotate` on the parent gives
per-line ownership, and a hunk whose `-` lines blame to exactly one mutable
ancestor moves there — and it executes as a sequence of `jj squash` calls. Every
squash rewrites history, so after the first one the remaining hunks' context has
moved and their context-bearing ids no longer match. Routing therefore uses the
context-free fingerprint the id entry predicted, path plus the `+`/`-` lines and
nothing else, and re-derives the diff before every squash. Hunks in one file
that share a fingerprint are indivisible: moved together when they agree on a
destination, left where they are when they do not, because no id-based selection
can name one without the other.

Renamed and copied files stay put, with the reason printed. A rename is a
whole-file change that no hunk selection can express, and carrying it along with
the first squash conflicts every ancestor still using the old name — commit the
rename on its own first, then absorb.

Three things in `jj-hunk-tool`'s absorb were deliberately **not** copied, and
each is a rule worth carrying into the next command:

- It falls back to "the most recent mutable ancestor that touched this file",
  refusing on *ambiguity* while guessing on *absence* of evidence. Here pure
  insertions stay put by default, and `--insertions=surrounding` is an opt-in
  that routes only when the lines above and below agree — jj's own rule, and
  evidence rather than recency.
- Its interactive retarget menu iterates a `HashSet`, so the numbered ancestor
  list is ordered differently on every run. Determinism here is enforced rather
  than hoped for — ordered collections throughout, destinations oldest-first,
  owners sorted before dedup — and a test runs the plan five times and compares.
- It prints hunk-relative line numbers in one place and absolute ones in
  another, while its `:1-3` selectors mean the former. Pick one frame and print
  it everywhere.

### A fix verified only against its own bug

Shipped as three commits on `alberto/hunkset-lang` — `fix(spec)`,
`fix(select)`, and `fix: rendering, binaries, tool pinning, absorb renames, and
fail-open selectors` — plus `fix(semantic)` on `alberto/tree-sitter-semantic`.

An independent multi-agent code review over the whole branch produced **15
confirmed correctness findings**. All are fixed and verified. What earns them a
section rather than a changelog line is where several of them came from: they
were opened *by* the fixes recorded above.

- The symlink fix (`fix: symlink corruption`) routed every existence check
  through `symlink_metadata` and left the read and the write on
  `fs::read_to_string` and `fs::write`, which traverse links. A committed
  `link.txt -> ../outside/victim.txt` then sent `select`'s write to a path in
  neither directory jj materialised. Exit 0.
- The mode fix (`fix: stop silently losing files, merges, and mode changes`)
  restored the file mode from the left side unconditionally — including for a
  file whose hunks were *kept*, so a chmod that should have ridden along with
  them was stripped instead. Under `diffedit`, which keeps no remainder commit,
  that discarded it from history outright.
- The spec validator (`fix(select): carry rename sources, reset empty
  selections, validate specs`) had three separate holes. A spec keyed by a
  rename's **old** path validated and then reverted the rename, because `known`
  was indexed by every entry of `all_paths()` while `select` looks the entry up
  under the new path. `--allow-empty` gated `validate_spec_resolves`, so one
  blanked entry switched off typo detection for every other entry in the same
  spec — it says an empty *result* is acceptable, not that the names need not
  exist. And a change that produces no hunks — a pure rename, a pure copy, an
  empty file added or removed — was invisible to both `list` and
  `--spec-template`, so feeding `diffedit` its own template deleted the rename.

The lesson is the one this document keeps arriving at from new directions. **A
fix verified only against its own reproduction closes that case and can open the
one beside it.** The evidence here is blunt: a regression sweep of the 16 earlier
fixes on these branches passed 16/16 while these 15 bugs sat next to them,
because the sweep re-ran the scenarios those fixes were written for. Coverage of
a fix is not coverage of the code the fix touched. Review the neighbourhood
rather than the reproduction, and put a reader on it who did not write the fix.

Two findings also turned out to be larger than their reports.
`~glob("vendor/[a-z*.txt")` committed the vendored change it was written to
exclude — a malformed glob matched nothing and `~` inverted that into
everything — but the root cause was not glob syntax: `evaluate_function`
validated the parsed pattern while `eval_glob`/`eval_file` re-derived its kind
and recompiled behind `unwrap()`, so the validated pattern was never the one
matched with. And a stack overflow on long `|`/`&` chains had a twin in
`src/semantic.rs`, where `find_enclosing` recursed once per syntax node. Both
walks are now iterative.

## Roadmap

A systematic bug hunt across the diff core, the query language, the semantic
configs and the jj integration produced 38 findings; fixing them surfaced 6
more. All but three are now fixed and on `main` — see **Fixed** above for the
ones with lasting lessons, and the PR descriptions for the full inventory. A
later independent review of the whole tree found 15 more, also fixed; that round
is the reason the feature roadmap below is empty and the lesson section above
has grown.

### Still open

Nothing on the feature roadmap. `alberto/short-ids` and `alberto/jj-verbs` were
the last two entries here and both shipped — see **Fixed** above. What is left
is hygiene.

**Clippy is still advisory, and no longer has a reason to be.**
`cargo clippy --locked --all-targets` now reports **zero** warnings on `main` in
all three feature modes — default, `--no-default-features`, `--all-features`.
The 11 that used to sit here were cleared by two `chore(clippy)` commits.

The CI job stayed advisory because gating it would have failed the *other*
branches, which carried upstream's `src/` unchanged and its nine warnings with
it; `ci.yml`'s comment says so, and names main-being-clean as the real trigger.
That blocker died with the branches. `main` is now the only branch CI runs on
and it is clean, so the gate can be turned on by deleting `continue-on-error`
and restoring `RUSTFLAGS` on that step. Do it, or drop the comment — the promise
has been about to come due for long enough.

`ci.yml` also still triggers on `push: branches: [main, "alberto/**"]`. The
second pattern matches nothing now.

**Agent worktrees accumulate.** At last count 9 `worktree-agent-*` branches
existed against 2 live worktrees — so 8 branches outliving the directory they
were made for — and this accumulation filled the disk mid-session once already.
Nothing removes them on its own, and the branch outlives the worktree:

```bash
git worktree list                  # what actually exists
git worktree remove <path>         # while the directory is still there
git worktree prune                 # after directories were deleted by hand
git branch -D worktree-agent-<id>  # the branch is a separate cleanup
```

Sweep them between sessions rather than when the disk fills, which is the point
at which everything else in flight fails at once and for confusing reasons.

### Deliberately not fixed

- **Ruby `singleton_class`** (`class << self`) is not a configured scope. It was
  evidence for the depth-inflation bug, not a request; the inflation is fixed.
- **Zig `@import`** is not treated as an import — it is indistinguishable from
  `@embedFile`/`@cImport`.
- **Kotlin** remains absent: the grammar crate still uses the pre-0.25
  tree-sitter API.
- **jj's "copied" status** is untested rather than broken — jj 0.44 with the git
  backend reports a copied-and-edited file as "added", so those branches never
  execute.
- `content(glob:"...")` matches against hunk *text*, where `*` correctly stops
  at `/`, so `glob:"*TODO*"` will not match `// TODO`. Correct path-glob
  behaviour in the wrong place; `substring:`/`regex:` are the right tools. The
  cleaner fix is to stop accepting `glob:` on content predicates at all.
- **Left-associative `~`.** jj accepts `a ~ b ~ c`; this fork rejects it. The
  rejection is unambiguous under the existing precedence and costs users only a
  rewrite, so it stands — but the code comment claiming jj also rejects it was
  false and has been corrected.

## Building and Installing

```bash
# Full build (all 20 grammars)
cargo build --release

# Without tree-sitter — much faster, query language still works
cargo build --release --no-default-features

# Install
cargo install --path . --locked
```

Register the diff tool in `~/.config/jj/config.toml` (jj-hunk self-configures a
temporary tool definition when invoked through its own subcommands, but an
explicit entry lets you use `jj split --tool=jj-hunk` directly):

```toml
[merge-tools.jj-hunk]
program = "jj-hunk"
edit-args = ["select", "$left", "$right"]
```

Verify:

```bash
jj-hunk --help
jj-hunk list --spec 'type(insert) & glob("src/**/*.rs")' --format text
```

## Hunkset Quick Reference

```bash
# Set algebra: | union, & intersection, ~ difference, ~x negation
jj-hunk list --spec 'type(insert) | type(delete)'
jj-hunk list --spec '(type(insert) | type(replace)) & function("parse")'
jj-hunk list --spec 'all() ~ import()'

# Precedence: | is lowest, & binds tighter. Parenthesize when unsure.
```

| Category | Functions |
|---|---|
| File | `file()`, `glob()`, `extension()`, `status()` |
| Type | `type(insert\|delete\|replace)` |
| Lines | `lines()`, `before_line()`, `after_line()` |
| Content | `content()`, `added()`, `removed()` |
| Identity | `id()` (prefix matching supported) |
| Semantic | `function()`, `scope()`, `annotation()`/`decorator()`, `doc()`, `import()`, `toplevel()`, `depth()` |
| Constants | `all()`, `none()` |

Patterns accept `exact:`, `substring:`, `glob:`, and `regex:` prefixes. **The
prefix goes outside the quotes**, and getting that wrong fails silently:

```bash
jj-hunk list --spec 'added(regex:"(TODO|FIXME)")'   # correct
jj-hunk list --spec 'added("regex:TODO")'           # WRONG — matches the literal
                                                    # text `regex:TODO`, so it
                                                    # returns nothing, rc=0
```

Inside the quotes the prefix is just characters to search for. Regex compilation
itself is properly validated — `added(regex:"[")` exits 1 with
`invalid regex '[': unclosed character class` — so the only trap is prefix
placement. See **Selectors that matched nothing failed silently** above.

## Common jj Commands

### Navigation

```bash
jj log                              # View commit graph
jj log -r 'main..@'                 # Commits between main and working copy
jj status                           # Current state
jj diff                             # Changes in working copy
```

### Branching

```bash
jj bookmark list                    # List bookmarks
jj bookmark create <name>           # Create at current commit
jj bookmark set <name> -r <rev>     # Move bookmark
jj bookmark set <name> --allow-backwards
```

### Editing history

```bash
jj edit <change-id>                 # Edit existing commit
jj new                              # Create new commit
jj squash -u                        # Move changes to parent, keep parent's message
jj describe -m "message"            # Change commit message
jj abandon <change-id>              # Remove commit
```

### Rebasing

```bash
jj rebase -d main                   # Rebase current onto main
jj rebase -s <rev> -d <dest>        # Rebase rev and descendants
jj rebase -r <rev> -d <dest>        # Rebase only rev
```

### Syncing with Git

```bash
jj git fetch --all-remotes
jj git push --remote origin --bookmark <name>
jj git export
```

### Undo

```bash
jj undo                             # Undo last operation
jj op log                           # Operation history
jj op revert <op-id>                # Revert a specific operation
```

Prefer `jj op revert` over `jj op restore`: restore rewinds the whole repo state
and will discard work done in other workspaces since that operation.

## File Locations

| File | Purpose |
|------|---------|
| `Cargo.toml` | Manifest, fork version, `semantic` feature |
| `src/diff.rs` | Hunk extraction (`similar`), stable sha256 ids, `SemanticInfo` |
| `src/spec.rs` | JSON/YAML spec parsing |
| `src/commands.rs` | jj invocation, `list`/`split`/`commit`/`squash`, `--tool` callback |
| `src/hunkset/` | Query language: ast, parser, eval, pattern, error |
| `src/absorb.rs` | Blame-based routing, context-free fingerprints, squash plan |
| `src/semantic.rs` | tree-sitter analyzer, 20 language configs |
| `src/glob.rs` | Glob matching for path predicates |
| `FORK_WORKFLOW.md` | This documentation |

## Version Scheme

- Upstream: `0.4.1`
- This fork: `0.4.1-my-jj-hunk`

The `-my-jj-hunk` suffix identifies which build is running. Note it sorts as a
semver *prerelease*, i.e. below plain `0.4.1` — fine for a local fork, but do not
publish it to crates.io under that scheme.

---

*Last updated: 2026-08-11*
*528 tests green on `main` (`cargo test`, jj 0.44.0); 435 with `--no-default-features`*
*Command surface complete at 8 verbs; feature roadmap empty, hygiene backlog open*
