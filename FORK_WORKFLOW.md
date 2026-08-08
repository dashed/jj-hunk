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
└── alberto/my-jj-hunk (5-way merge)
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
│   │  - src/semantic.rs (1253 lines, 30 tests)
│   │  - 20 tree-sitter grammars behind the `semantic` feature
│   │  - enclosing function/scope, annotations, doc/import flags, depth
│
├── ○ alberto/hunkset-lang
│   │  - src/hunkset/{ast,error,eval,parser,pattern}.rs
│   │  - src/glob.rs
│   │  - Declares the `semantic` feature *contract*
│
└── ○ alberto/my-jj-hunk (merge of all above)
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
       alberto/fork-customizations alberto/new-feature \
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

## Roadmap

A systematic bug hunt across the diff core, the query language, the semantic
configs and the jj integration produced 38 findings; fixing them surfaced 6
more. All but three are now fixed and on the feature branches — see **Fixed**
above for the ones with lasting lessons, and the PR descriptions for the full
inventory.

### Still open

**`alberto/short-ids`** — hunk ids are full 64-hex sha256, unusable by hand.
`id()` already supports unambiguous prefixes (and now errors on ambiguous
ones), so the remaining work is emitting an abbreviated form in `list` output
with collision-aware widening, the way jj does for change ids.

While doing it, decide deliberately what an id should survive. Ids hash the
surrounding context, so they are stable under distant edits but change when
anything within the context window moves, and they **do not survive a rebase** —
the opposite of the durability an agent wants while editing around its own
pending changes. A context-free id is durable but collides between identical
edits in one file, so it needs a disambiguator. Pick one rather than inheriting
the current behaviour by default.

**`alberto/jj-verbs`** — port the commands upstream lacks: `diffedit`,
`restore`, and `absorb`. Absorb is the valuable one — blame-based routing of
hunks into the mutable ancestor that introduced the code, via
`jj file annotate`.

Watch the argument-semantics inversion: for `split` the named hunks become the
split-off commit and for `restore` they are the ones undone, but for `diffedit`
they are the ones **kept**.

**Absorb needs a second, context-free notion of hunk identity.** It executes as
a sequence of `jj squash` calls, and every squash rewrites history — shifting
the remaining hunks' context and therefore their ids underneath it.
`jj-hunk-tool` hit this and solved it with a fingerprint of path plus the
`+`/`-` lines only, deliberately excluding context, re-matched after each
squash. That is not an optimization; it is what makes absorb correct.

Three things in `jj-hunk-tool`'s absorb are worth **not** copying: routing lets
only `-` lines vote, and resolves *ambiguity* by refusing while letting
*absence* of evidence fall through to a much weaker recency heuristic; its
interactive retarget menu iterates a `HashSet`, so the numbered ancestor list is
ordered differently on every run; and it prints hunk-relative line numbers in
one place and absolute ones in another while `:1-3` selectors mean the former.

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
placement. See `alberto/strict-selectors` below.

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

---

*Last updated: 2026-08-09*
*Bug hunt complete: 314 tests, glob now verified identical to jj across 1650 differential comparisons*
