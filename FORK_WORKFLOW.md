# jj-hunk Fork Workflow

This document describes the branch structure and workflow for maintaining a custom
`jj-hunk` fork with independent feature branches that can be selectively combined.

## Overview

This fork uses a **modular feature branch strategy** where:

- `main` tracks upstream [`laulauland/jj-hunk`](https://github.com/laulauland/jj-hunk)
- Each feature lives in its own independent branch based on `main`
- `alberto/my-jj-hunk` is a merge commit that combines all desired features
- `alberto/fork-customizations` holds fork-specific additions (version, docs)
- Jujutsu (jj) is used for version control alongside Git

This approach allows:
- Easy updates when upstream releases new versions
- Selective feature inclusion (enable/disable features by changing the merge)
- Clean separation of concerns
- Simple conflict resolution per-feature

## Why This Fork Exists

Three projects solve non-interactive hunk selection for jj, and none of them is
the tool this fork wants to be:

| Project | Strength | Why not just use it |
|---|---|---|
| [`laulauland/jj-hunk`](https://github.com/laulauland/jj-hunk) | Only tool on the [official jj community-tools list](https://docs.jj-vcs.dev/latest/community_tools/); ships via crates.io, Homebrew, Nix. Fine hunk granularity. | Thin command surface, 17 tests, **no test CI**, slow release cadence. |
| [`sigma/jj-hunk`](https://github.com/sigma/jj-hunk) | The **hunkset query language** and tree-sitter semantics — the best ideas in this space. | All of it sits unmerged on branches, unreleased and never PR'd upstream. |
| [`mvzink/jj-hunk-tool`](https://github.com/mvzink/jj-hunk-tool) | Best engineering: 125 tests, `absorb`/`diffedit`/`restore`, sub-hunk line ranges, short IDs. | Different architecture; coarser git-style hunks; **no CI at all**; rejects renamed files outright. Its ID algorithm and patch slicer live in the `git-surgeon` crate, floated as `"0.1.14"` (i.e. `^0.1.14`) — and its documented install path, `cargo install --git`, re-resolves dependencies unless `--locked` is passed, so an upstream patch release could silently change every hunk ID. Also shells out to `patch(1)` at runtime. |

This fork takes `laulauland/jj-hunk` as its base — it keeps the distribution
channels and the community listing — then lands sigma's unmerged work as
independent feature branches and ports the best of `jj-hunk-tool`'s ideas on top.

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

```
main (upstream v0.4.1, 3643ee8)
│
├── alberto/fork-customizations
│   └── Version suffix, README banner, this doc
│
├── alberto/tree-sitter-semantic
│   └── src/semantic.rs — tree-sitter analyzer for 20 languages
│
├── alberto/hunkset-lang
│   └── Algebraic hunk query language + semantic feature contract
│
├── alberto/test-ci
│   └── .github/workflows/ci.yml
│
└── alberto/my-jj-hunk (4-way merge)
    └── Integration branch combining all features + customizations
```

### Branch Descriptions

| Branch | Purpose | Base | Standalone build |
|--------|---------|------|:----------------:|
| `main` | Tracks upstream `laulauland/jj-hunk` | — | yes |
| `alberto/fork-customizations` | Version, README banner, this doc | `main` | yes |
| `alberto/tree-sitter-semantic` | Semantic analyzer (20 grammars) + 13 bug fixes | `main` | yes |
| `alberto/hunkset-lang` | Hunkset query language + 12 bug fixes + docs | `main` | yes |
| `alberto/test-ci` | Test workflow (Linux + macOS, both feature modes) | `main` | yes |
| `alberto/my-jj-hunk` | Combined features | merge | yes |

Every feature branch is based directly on `main` and **builds and tests green on
its own**. That is the invariant that keeps rebases cheap — if a branch only
compiles as part of the merge, it is not a feature branch, it is a fragment.

### Visual DAG

```
◆  main (upstream v0.4.1, 3643ee8)
│
├── ○ alberto/fork-customizations
│   │  - Version: 0.4.1-my-jj-hunk
│   │  - README fork banner
│   │  - FORK_WORKFLOW.md
│
├── ○ alberto/tree-sitter-semantic
│   │  - src/semantic.rs (2179 lines, 86 tests)
│   │  - 20 tree-sitter grammars behind the `semantic` feature
│   │  - enclosing function/scope, annotations, doc/import flags, depth
│
├── ○ alberto/hunkset-lang
│   │  - src/hunkset/{ast,error,eval,parser,pattern}.rs
│   │  - src/glob.rs, src/absorb.rs
│   │  - Declares the `semantic` feature *contract*
│   │  - The command surface: 8 verbs
│
├── ○ alberto/test-ci
│   │  - .github/workflows/ci.yml
│
└── ○ alberto/my-jj-hunk (merge of all four)
       - Includes all features + fork customizations
```

## The `semantic` Feature Contract

`hunkset-lang` and `tree-sitter-semantic` are deliberately **independent**, not
stacked. The seam between them is the `semantic` cargo feature:

- **`alberto/hunkset-lang`** declares `semantic = []` — an empty feature. Its
  semantic predicates (`function()`, `scope()`, `annotation()`, `doc()`,
  `import()`, `toplevel()`, `depth()`) compile against the flag and return
  `function() requires the 'semantic' feature` when it is off.
- **`alberto/tree-sitter-semantic`** *redefines* the same feature with the 20
  grammar dependencies and supplies `src/semantic.rs` to populate the data.

So either branch is useful alone, and the integration merge simply takes the
grammar-bearing definition of the feature. If you ever want the query language
without a 21-crate build, drop `tree-sitter-semantic` from the merge and
everything still compiles.

## Provenance

Most of what is on the feature branches is imported, reorganized, and verified
rather than original. Track it honestly:

| Branch | Imported from | Upstream commits |
|---|---|---|
| `alberto/tree-sitter-semantic` | `sigma/dev` | `59143b6`, `6dee312`, `558f184`, `56a3c97` |
| `alberto/hunkset-lang` | `sigma/dev` | `3fafa16`, `4e3b820`, `8decd9d`, `854c6dd`, `86cf49c` |

The fork's own contributions so far: the reorganization (sigma ships these as one
entangled DAG with two merge commits, semantic and hunkset interleaved — here they
are two independent branches that each build and test green alone), the README
hunkset documentation that the import initially left behind, and the
two bug-fix commits on `alberto/hunkset-lang` — `fix(diff-format)` and
`fix(hunkset)` — with 25 regression tests between them.

## Remotes

```bash
git remote -v
# upstream  git@github.com:laulauland/jj-hunk.git   ← main tracks this
# sigma     https://github.com/sigma/jj-hunk.git    ← source of imported features
# mvzink    https://github.com/mvzink/jj-hunk-tool.git ← reference for ported ideas
```

`mvzink` is an unrelated codebase fetched into the same object store purely so its
history is greppable when porting ideas. Nothing merges from it directly.

When you create your own GitHub fork, add it as `origin` and push there:

```bash
git remote add origin git@github.com:<you>/jj-hunk.git
jj git push --remote origin --bookmark 'glob:alberto/*'
```

## Jujutsu (jj) Setup

This repo uses jj in colocated mode, meaning both `jj` and `git` commands work.

### Why jj?

- **Automatic rebasing**: When you update a parent, descendants auto-rebase
- **First-class conflicts**: Conflicts are stored in commits, resolve when convenient
- **Operation log**: Every operation can be undone with `jj undo`
- **Change IDs**: Stable identifiers that survive rebases (unlike git commit hashes)
- **Multi-parent commits**: Native support for merge commits with 4+ parents

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

## Updating from Upstream

### Step 1: Fetch

```bash
jj git fetch --all-remotes
```

### Step 2: Update main

```bash
jj bookmark set main -r main@upstream
```

### Step 3: Rebase feature branches onto new main

Rebase in order of conflict risk (lowest first). `old_main` is the previous main
commit id.

```bash
# Lowest risk — touches only Cargo.toml version + docs
jj rebase -s 'roots(old_main..alberto/fork-customizations)' -d main

# Low risk — adds a self-contained module
jj rebase -s 'roots(old_main..alberto/tree-sitter-semantic)' -d main

# Highest risk — rewrites commands.rs, diff.rs, spec.rs
jj rebase -s 'roots(old_main..alberto/hunkset-lang)' -d main
```

### Step 4: Resolve conflicts

```bash
jj log -r 'conflicts()'

# For each conflicted commit:
jj new <conflicted-commit-id>
# edit files to resolve
jj squash -u                      # move resolution into parent, keep its message
```

**Tip:** resolve the earliest conflicted commit first — descendants often
auto-resolve.

### Step 5: Rebuild the integration branch

```bash
jj new alberto/tree-sitter-semantic alberto/hunkset-lang alberto/fork-customizations \
  alberto/test-ci -m "integration: combine all feature branches"
jj bookmark set alberto/my-jj-hunk --allow-backwards
```

### Step 6: Verify

Do not skip this — a merge that compiles is not a merge that works.

```bash
cargo build --all-features
cargo test --all-features          # expect all green
cargo build --no-default-features  # hunkset must still build without tree-sitter
```

## Known Upstream Conflict

Upstream has an unmerged fix worth taking when it lands:

| Upstream branch | Commit | What it does | Conflict |
|---|---|---|---|
| `codex/fix-merge-list` | `6d7d44e` | Handle merge commits in `list` | Conflicts with `hunkset-lang` in `src/commands.rs` |

Both rewrite large parts of `src/commands.rs`. Only that one file conflicts;
`src/main.rs` and `tests/integration.rs` merge cleanly. Take upstream's
restructuring of `read_diff_summary` and re-apply the hunkset filter call on top.

## Adding a New Feature

```bash
# 1. Branch from main
jj new main -m "feat: description of feature"
jj bookmark create alberto/new-feature

# 2. Develop. Changes are auto-tracked; start a new commit per logical unit:
jj new -m "next part of feature"

# 3. Verify it stands alone
cargo build && cargo test

# 4. Fold into the integration branch
jj new alberto/tree-sitter-semantic alberto/hunkset-lang \
       alberto/fork-customizations alberto/test-ci alberto/new-feature \
  -m "integration: combine all feature branches"
jj bookmark set alberto/my-jj-hunk --allow-backwards
```

**Do not create empty placeholder commits for planned branches.** A bookmark on an
empty commit looks like work that exists. Track intent in the roadmap below and
create the branch when you have code for it.

## Fixed

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
implemented one on `origin/diff-output-format` and dropped it in the refactor
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

Note the test-visibility trap this exposed: `alberto/hunkset-lang` declares
`semantic = []`, so it *cannot* observe semantic behaviour. The four
integration tests here are `#[cfg(feature = "semantic")]`-gated — inert on that
branch, live in the merge. A semantic change that is only tested on
`hunkset-lang` is not tested at all.

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
more. All but three are now fixed and on the feature branches — see **Fixed**
above for the ones with lasting lessons, and the PR descriptions for the full
inventory. A later independent review of the whole branch found 15 more, also
fixed; that round is the reason the feature roadmap below is empty and the
lesson section above has grown.

### Still open

Nothing on the feature roadmap. `alberto/short-ids` and `alberto/jj-verbs` were
the last two entries here and both shipped — see **Fixed** above. What is left
is hygiene.

**Clippy is still advisory.** `cargo clippy --locked --all-targets` reports 11
warnings on the integration merge: one `too many arguments`, four derivable
`impl`s, two collapsible `if`s, a manual `Iterator::find`, a no-op `as_ref`, a
field assignment outside a `Default::default()` initializer, and a `vec!` in a
test. None is a correctness claim, and the CI job's own comment promises to
promote it to a gate once the backlog is cleared. Clear it and promote it, or
drop the promise — a comment that has been about to happen for a while is worth
less than either.

**Agent worktrees accumulate.** At last count 18 directories under
`.claude/worktrees/` and 11 `worktree-agent-*` branches were still on disk, and
that accumulation filled the disk mid-session once already. Nothing removes them
on its own, and the branch outlives the worktree:

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

## Workflow Tips

### Moving a bookmark does not re-parent the integration merge

jj auto-rebases descendants when you *rewrite* a commit — edit
`fork-customizations` and the merge follows automatically. But adding a **new**
commit to a feature branch and moving the bookmark to it does not: the merge
still lists the old commit as its parent, and silently keeps building the old
code. The symptom is a test count that does not go up.

```bash
jj log -r 'alberto/my-jj-hunk-'   # what the merge is actually built from
jj log -r 'alberto/hunkset-lang'  # where the bookmark now points
```

If those disagree, rebuild the merge (see "Rebuild the integration branch") and
`jj abandon` the stale one.

### Keep every feature branch independently green

Before folding anything into `alberto/my-jj-hunk`, check out the branch alone and
run `cargo build && cargo test`. This is what makes upstream rebases cheap — you
can rebase and validate one branch at a time instead of debugging a four-way merge.

### Cargo.toml is the recurring conflict

`fork-customizations` owns `[package] version`, `hunkset-lang` owns the `[features]
contract` plus `regex`/`thiserror`, and `tree-sitter-semantic` owns the grammar
deps and the real `[features]` definition. On conflict, keep all three regions —
they touch different parts of the file. The `[features]` block should end up with
`tree-sitter-semantic`'s grammar-bearing definition, not `hunkset-lang`'s empty one.

### Cargo.lock conflicts

Take main's version, then let cargo regenerate:

```bash
jj restore --from main Cargo.lock
cargo build
```

### Version suffix after upstream bumps

`fork-customizations` sets `version = "X.Y.Z-my-jj-hunk"`. After rebasing onto a
new upstream release, update the `X.Y.Z` prefix to match.

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
| `FORK_WORKFLOW.md` | This documentation (on `fork-customizations`) |

## Version Scheme

- Upstream: `0.4.1`
- Integration: `0.4.1-my-jj-hunk`

The `-my-jj-hunk` suffix identifies which build is running. Note it sorts as a
semver *prerelease*, i.e. below plain `0.4.1` — fine for a local fork, but do not
publish it to crates.io under that scheme.

## Rebase History

| Date | Upstream Range | New Commits | Conflicts | Notes |
|------|---------------|:-----------:|-----------|-------|
| 2026-08-08 | — → 3643ee8 | — | — | Fork created at upstream v0.4.1; imported sigma's hunkset + semantic work as two independent branches |
| 2026-08-09 | (no upstream move) | 17 | — | Bug hunt across four areas: 38 findings, 6 more surfaced while fixing. All but three fixed. Test count 17 → 314 |
| 2026-08-10 | (no upstream move) | 10 | — | Short ids and the `diffedit`/`restore`/`absorb` verbs shipped, closing the roadmap; independent multi-agent review then found 15 correctness bugs, all fixed. Test count 314 → 478 |

---

*Last updated: 2026-08-11*
*478 tests green on the integration merge (`cargo test --all-features`, jj 0.44.0)*
*Command surface complete at 8 verbs; feature roadmap empty, hygiene backlog open*
