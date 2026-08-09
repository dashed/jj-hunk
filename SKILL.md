---
name: jj-hunk
description: Programmatic hunk selection for jj (Jujutsu). Use when splitting commits, making partial commits, selectively squashing changes, or coordinating concurrent agent work into clean history.
---

# jj-hunk: Programmatic Hunk Selection

Use `jj-hunk` for non-interactive hunk selection in jj. Essential for AI agents that need to create clean, logical commits from mixed changes — especially when multiple agents work concurrently.

## When to Use This Skill

- Splitting a commit into multiple logical commits
- Committing only specific hunks (partial commit)
- Squashing only certain changes into parent
- Coordinating concurrent agent work into clean, independent branches
- Any hunk selection that would normally require `jj split -i` or `jj squash -i`

## Setup

```bash
cargo install jj-hunk
```

Add to `~/.jjconfig.toml`:
```toml
[merge-tools.jj-hunk]
program = "jj-hunk"
edit-args = ["select", "$left", "$right"]
```

### Semantic predicates need the `semantic` build feature

`function()`, `scope()`, `annotation()`, `decorator()`, `doc()`, `import()`, `toplevel()`, and
`depth()` are backed by tree-sitter and only exist in a binary built with the `semantic` feature.
A binary built without it does not silently return nothing — every one of those predicates fails
with an explicit error:

```
Error: hunkset evaluation error: function() requires the 'semantic' feature (build with --features semantic)
```

If you see that, rebuild with `cargo install jj-hunk --features semantic`. Every other predicate
(`file`, `glob`, `extension`, `status`, `type`, `lines`, `content`, `added`, `removed`, `id`,
`all`, `none`) works in any build.

## Hunkset Query Language

jj-hunk supports an algebraic query language (hunkset) for selecting hunks, inspired by jj's filesets and revsets. This is the recommended way to select hunks — it's more readable, composable, and semantically aware than JSON specs.

### Operators

Lowest to highest precedence:

| Operator | Meaning | Example |
|----------|---------|---------|
| `x \| y` | Union | `type(insert) \| type(delete)` |
| `x & y` | Intersection | `type(insert) & glob("src/**")` |
| `x ~ y` | Difference | `all() ~ type(delete)` |
| `~x` | Negation | `~type(delete)` |
| `(x)` | Grouping | `(type(insert) \| type(replace)) & file("x")` |

Union and intersection chain freely. **Difference does not**: `a ~ b ~ c` is a parse error — write
`(a ~ b) ~ c`. This differs from jj's revsets, where `~` is left-associative. `!` is not an operator
here; `~` covers both negation and difference. `all` and `none` may be written bare when they are
the whole expression.

### Functions

**File predicates:**

| Function | Description |
|----------|-------------|
| `file("path")` | Exact file path match (whole path, not a suffix) |
| `glob("src/**/*.rs")` | Glob pattern on file path |
| `extension("rs")` | File extension, written without the dot |
| `status(modified)` | File status: `modified`, `added`, `removed`, `renamed`, `copied` |

`status()` takes a bare identifier, not a string. A deleted file is `removed` (there is no
`deleted`); an invalid value is rejected with the list of valid ones.

**Hunk type:**

| Function | Description |
|----------|-------------|
| `type(insert)` | Insertions only |
| `type(delete)` | Deletions only |
| `type(replace)` | Replacements only |

**Content matching:**

| Function | Description |
|----------|-------------|
| `content("text")` | Added or removed text contains "text" |
| `added("text")` | Added text contains "text" |
| `removed("text")` | Removed text contains "text" |

**Line ranges:**

| Function | Description |
|----------|-------------|
| `lines(10..20)` | Hunks touching lines 10-20, in either the before or after file |
| `before_line(10..20)` | Hunks in the "before" line range |
| `after_line(10..20)` | Hunks in the "after" line range |

**Ranges are inclusive of both endpoints, despite the Rust-looking `..`.** `lines(10..20)`
includes line 20, and `lines(7..7)` selects the hunk on line 7. The same applies to `depth()`.

**Identity (stable across concurrent changes):**

| Function | Description |
|----------|-------------|
| `id("hunk-b6548253")` | Select by hunk ID — full, short, or any unambiguous prefix |
| `id("hunk-b6548253", "hunk-397f491f")` | Multiple IDs in one call; the forms may be mixed |
| `all()` / `none()` | Everything / nothing |

A prefix that matches more than one hunk — or the bare `hunk-` — is rejected rather than guessed at,
with the candidates named. See [Hunk IDs](#hunk-ids) for the two forms and their shelf life.

**Semantic (tree-sitter powered, requires the `semantic` feature):**

| Function | Description |
|----------|-------------|
| `function("name")` | Hunks inside a function/method with exactly that name |
| `scope("ClassName")` | Hunks inside a class/struct/impl/module with exactly that name |
| `annotation("test")` | Hunks in functions/scopes whose annotation text contains "test" |
| `decorator("route")` | Alias for `annotation()` |
| `doc()` | Hunks that are doc comments |
| `import()` | Hunks that are import/use/require statements |
| `toplevel()` | Hunks not inside any function or scope |
| `depth(0..1)` | Hunks at nesting depth 0 or 1 |

Supported languages: Rust, Python, JavaScript, TypeScript, Go, C, C++, Java, Ruby, C#, Scala,
Swift, PHP, Bash, Elixir, Erlang, Haskell, OCaml, Zig, Lua.

For a file in an unsupported language, semantic predicates contribute nothing and say so on stderr:

```
warning: function() found no semantic metadata -- no parser is available for: notes.txt, only.kt.
The empty result reflects missing language support, not an absence of matches.
```

`toplevel()` and `depth()` also exclude unparsed files, so an unsupported file is never mistaken
for genuinely top-level code.

### Pattern syntax

String arguments accept a pattern prefix. **The prefix goes outside the quotes** — `regex:"..."`,
not `"regex:..."`. Writing it inside the quotes is a parse error that tells you to move it.

| Form | Meaning |
|------|---------|
| `exact:"text"` | Exact match |
| `substring:"text"` | Substring match |
| `glob:"pattern"` | Glob pattern |
| `regex:"pattern"` | Regular expression |

**Without a prefix, the default depends on the predicate**, so a bare quoted string is not
uniformly "substring":

| Predicate | Bare-string default | Meaning |
|-----------|--------------------|---------|
| `function()`, `scope()` | **exact** | `function("alpha")` does NOT match `alpha_beta` |
| `file()`, `extension()` | **exact** | `file("svc.rs")` does NOT match `src/svc.rs` |
| `content()`, `added()`, `removed()` | substring | `added("TODO")` matches any line containing TODO |
| `annotation()`, `decorator()` | substring | `annotation("test")` matches `#[test]` |

To match a family of identifiers, opt in explicitly:

```bash
jj-hunk list --spec 'function(substring:"test")'   # test_a, my_test, test_b ...
jj-hunk list --spec 'function(glob:"test_*")'
jj-hunk list --spec 'function(regex:"^handle_")'
```

### Decorator-only changes attribute differently per language

If a hunk touches **only** a decorator/attribute line and not the function body, the language
grammar decides whether that line counts as part of the function:

| Attributes to the FUNCTION | Attributes to the enclosing SCOPE only |
|---------------------------|----------------------------------------|
| Java, C#, Swift, Scala, PHP, JavaScript | Rust, Python, TypeScript (`.ts`, `.tsx`) |

So for a one-line change to `@Test(timeout = 1)` above `my_test`:

```bash
# Java  -> selects it
# Rust / Python / TypeScript -> selects nothing
jj-hunk list --spec 'function("my_test")'

# works in every language above
jj-hunk list --spec 'scope("Holder")'
```

Note that JavaScript and TypeScript disagree with each other on identical source text. When a
selection must cover decorators portably, reach for `scope()`, a line range, or explicit hunk IDs
instead of `function()`.

### Examples

```bash
# All insertions in Rust files
jj-hunk split 'type(insert) & glob("src/**/*.rs")' "add new code"

# Everything inside UserService class
jj-hunk split 'scope("UserService")' "refactor: update UserService"

# All test functions
jj-hunk split 'annotation("test")' "test: add unit tests"

# Imports only
jj-hunk split 'import()' "chore: update imports"

# Everything except docs
jj-hunk split 'all() ~ doc()' "feat: implementation"
```

## Output Formats

```bash
jj-hunk list --format json    # Structured data (default)
jj-hunk list --format yaml    # YAML variant
jj-hunk list --format text    # Human-readable summary with semantic context
jj-hunk list --format diff    # Unified diff with hunk IDs and semantic context
```

`--format text` names the enclosing scope and function inline, which is the fastest way to see
which hunks belong to which logical change:

```
M src/svc.rs
  hunk 0 insert hunk-b6548253 (before 2+0 after 2+1)
    + use std::io;
  hunk 1 insert hunk-f2c7f434 (before 5+0 after 6+1) in UserService
    +     hits: u64,
  hunk 2 replace hunk-397f491f (before 9+1 after 11+1) in UserService::handle_request
    -         let a = 1;
    +         let a = 111;
```

`--format diff` produces a unified patch whose `@@` headers carry the enclosing function name and
the hunk's short ID:

```diff
--- a/wide.rs
+++ b/wide.rs
@@ -4,7 +4,7 @@ func_2 [hunk-f5696093]
 
 
 fn func_2() {
-    let v = 2;
+    let v = 200;
 }
 
 
```

The ID in the header is written plain, with no trailing `...`, and can be pasted straight into
`id()` or a spec.

Combine it with `--spec` to export just the hunks a query selects.

**Applying a `--format diff` patch: use `git apply`, not `git am`.** The output is a bare unified
diff with no mail headers, so `git am` rejects it outright (`Patch format detection failed`).
`git apply` handles it, including added files, deleted files, CRLF line endings, and missing
trailing newlines. Renames are the exception — the patch records a rename as a plain modification
of the new path, so it will not apply against a tree that still has the old path.

## Hunk IDs

A hunk ID is `hunk-` plus a SHA-256 over the hunk's **path**, its type, its removed and added lines,
and up to three lines of surrounding context from the *parent* side of the diff.

Two written forms, both naming the same hunk:

| Form | Length | Where it appears |
|------|--------|------------------|
| Full | `hunk-` + 64 hex | The `id` field in `--format json` / `--format yaml` |
| Short | `hunk-` + 8 hex | The `short_id` field, `--format text`, `--format diff` headers, `--spec-template` |

The short form is the shortest prefix that is unique across the diff, never under eight hex digits.
Everywhere an ID is *accepted* — `ids` and `hunks` in a spec, and `id()` — the full form, the short
form, and any unambiguous prefix all work, and the forms may be mixed in one call. A trailing `...`
is tolerated for IDs copied out of older diff headers. An ambiguous prefix is an error naming the
candidates, never a guess. (`id(exact:"...")` is the one form that demands the full 64 hex.)

### How long an ID stays valid

Long enough to carry it from one command to the next against the same working-copy state. That is
the workflow — `list`, choose, `split` — and within it IDs are solid.

**Survives:** other hunks appearing or disappearing elsewhere in the same file (concurrent agent
work), edits to other files, and line numbers shifting — positions are not hashed.

**Does not survive:**

- **renaming or moving the file** — the path is hashed;
- **an edit touching a line immediately adjacent to the hunk**, which merges the two into one larger
  hunk with different text. A line of untouched code in between keeps them separate;
- **a rebase, or a squash into the parent**, when it rewrites the lines around the hunk. Context is
  read from the parent side. Treat any rebase as invalidating.

**So: re-run `list` after editing, and use the IDs from that run.** Do not cache IDs across an
editing session, a rebase, or a rename.

### When an ID does not resolve

`split`, `commit`, and `squash` validate every spec entry before touching anything and refuse the
whole operation if one does not name exactly what it meant to:

```
Error: spec does not resolve against the diff:
  wide.rs: no hunk with id hunk-deadbeef
Those entries do not name exactly what they meant to. Check them against `jj-hunk list --spec-template`, or pass --allow-empty if that is intended.
```

The middle line names the specific problem — `no hunk with id ...` (usually a stale ID), `id hunk-3
is ambiguous, it names 3 hunks -- use a longer prefix`, or `no hunk with index 99 (file has 10)`.

`jj-hunk list --spec` does not run this check: an ID matching nothing simply selects nothing, which
is what makes `list --spec` safe to iterate with.

## Core Workflow

### 1. Explore hunks with hunkset queries

```bash
# See all hunks
jj-hunk list --format text

# Preview what a query would select
jj-hunk list --spec 'scope("UserService")' --format text

# Verify a selection covers everything you expect
jj-hunk list --spec 'scope("UserService") ~ id("hunk-f2c7f434", "hunk-397f491f")' --format text
# Empty output = those ids cover everything in that scope
```

Other `list` options:

- `--rev <revset>` — diff the revision against its parent (must resolve to a single revision)
- `--include <glob>` / `--exclude <glob>` — filter paths; one pattern per flag, repeatable
- `--group none|directory|extension|status` — group output as `groups: [{name, files}]`
- `--binary skip|mark|include` — binary handling (default: `mark`, which lists the file with 0 hunks)
- `--files` — list files with hunk counts only
- `--spec-template` — emit an ID-based starting spec (JSON/YAML only; `text` and `diff` are rejected)

### 2. Select and split

Use hunkset expressions directly with split/commit/squash — the format is auto-detected, so
anything that isn't JSON/YAML is parsed as a hunkset:

```bash
# Split by semantic scope
jj-hunk split 'scope("UserService") & ~import()' "refactor: update UserService"

# Split by file pattern
jj-hunk split 'glob("src/api/**")' "feat: add API endpoints"

# Split by content
jj-hunk split 'added("TODO") | added("FIXME")' "chore: add TODOs"
```

A selection that matches nothing is a hard error, not an empty commit:

```
Error: selection matched no hunks: function("a")
Nothing would be kept, so this would create an empty commit.
Check the selector with `jj-hunk list --spec ...`, or pass --allow-empty if that is intended.
```

This is the main guard against a typo'd selector silently producing a no-op commit. Pass
`--allow-empty` only when an empty commit is genuinely what you want.

### 3. Use stable IDs for safety

When other changes may arrive concurrently, **always resolve your hunkset query to hunk IDs before executing the split**. This protects against new hunks appearing between the query and the split:

```bash
# Step 1: Query to find the hunks you want
jj-hunk list --spec 'function("handle_request") & glob("src/api/**")' --format text

# Step 2: Note the hunk IDs from the output, then split using IDs
jj-hunk split 'id("hunk-b6548253", "hunk-397f491f")' "feat: handle_request implementation"
```

`--format text` and `--format diff` print the short ID directly, so you can copy it out of either
one. `--spec-template` writes short IDs too, which makes it the quickest way to get a full,
correctly-shaped starting spec.

An ID survives another agent adding, removing, or shifting hunks elsewhere in the same file — that
is exactly what it is for. It does **not** survive a rename or a rebase. See
[Hunk IDs](#hunk-ids).

### 4. JSON specs (alternative)

For complex selections or when building specs programmatically:

```json
{
  "files": {
    "src/foo.rs": {"ids": ["hunk-b6548253", "hunk-397f491f"]},
    "src/bar.rs": {"action": "keep"},
    "src/qux.rs": {"action": "reset"}
  },
  "default": "reset"
}
```

| Spec | Effect |
|------|--------|
| `{"hunks": [0, 2]}` | Include only hunks 0 and 2 (indices or ID strings) |
| `{"ids": ["hunk-b6548253"]}` | Include hunks by ID (full, short, or unambiguous prefix) |
| `{"action": "keep"}` | Include all changes in the file |
| `{"action": "reset"}` | Discard all changes in the file |
| `"default": "reset"` | Unlisted files are discarded |
| `"default": "keep"` | Unlisted files are kept |

`ids` and `hunks` are merged if both are provided. Specs may be inline, read from stdin with `-`,
or loaded with `--spec-file`. `jj-hunk list --spec-template` generates an ID-based starting spec.

**Renamed files carry a `from` field.** `select` is handed two directories and nothing else, so it
cannot tell that `right/new_name` used to be `left/old_name`; `from` supplies that link:

```json
{"files": {"new_name.txt": {"ids": ["hunk-ca38eba7"], "from": "old_name.txt"}}, "default": "reset"}
```

`split`/`commit`/`squash` fill this in for you from jj's rename detection, so a hand-written spec
that omits it still works. It is load-bearing only on the raw `jj --tool=jj-hunk` path below, where
omitting it drops the file from the commit entirely.

## Direct jj --tool Usage

The commands above are wrappers. For direct control:

```bash
echo '{"files": {"src/foo.rs": {"hunks": [0]}}, "default": "reset"}' > /tmp/spec.json
JJ_HUNK_SELECTION=/tmp/spec.json jj split -i --tool=jj-hunk -m "message"
```

On this path nothing pre-processes the spec, so **you must write `from` yourself for any renamed
file**. Without it the selection matches no hunk in the recomputed diff and the file is lost.

## Multi-Agent Concurrent Workflow

When multiple agents work in parallel on different tasks, their changes tend to intermingle in the working copy. jj-hunk enables each agent to retrospectively extract its own changes into clean, independent branches.

### Setup: Create the merge structure

Before agents start working, establish the topology:

```bash
# Create the merge point that will combine all agent work
jj new main -m "merge: combine agent work"
# Save its change ID
MERGE=$(jj log -r @ --template 'change_id' --no-graph)
```

### Each agent's workflow

```bash
# 1. Make changes normally (code, tests, etc.)
#    Changes land in the working copy alongside other agents' work.

# 2. Query to identify YOUR changes using semantic context
jj-hunk list --spec 'scope("UserService") & glob("src/api/**")' --format text

# 3. Verify the query captures exactly your work — refine if needed
jj-hunk list --spec 'function("handle_request") & file("src/api/handler.rs")' --format text

# 4. Resolve to hunk IDs (protects against concurrent changes)
#    Collect the short IDs from the output above.

# 5. Split your changes into a clean commit
#    Do this from the same working-copy state you listed — do not reuse IDs
#    collected before an edit or a rebase.
jj-hunk split 'id("hunk-b6548253", "hunk-397f491f")' "feat: add request handler"

# 6. Rebase the clean commit to its proper place in the graph
jj rebase -r <new_change> -d main

# 7. Update the merge to include your branch
jj rebase -r $MERGE -d <new_change> -d <other_branches...>
```

### Verification

```bash
jj diff -r $MERGE
# Should be empty (or contain only conflict resolutions)
```

If the merge has unexpected content, an agent's split was incomplete — use `jj-hunk list -r $MERGE` to see what's left and dispatch it.

### Example: Three agents working concurrently

```
main
├── agent-1: feat: add database schema
│   (created by: jj-hunk split 'glob("src/db/**")' ...)
├── agent-2: feat: add API endpoints
│   (created by: jj-hunk split 'scope("Router") & glob("src/api/**")' ...)
├── agent-3: refactor: update shared utils
│   (created by: jj-hunk split 'function("parse_config") | function("validate")' ...)
└── merge: combine agent work (should be empty)
```

### Key principles

- **Query first, split by ID**: Use hunkset queries to explore, but resolve to hunk IDs before executing the split. IDs are content-addressed (SHA256), so another agent's hunks arriving elsewhere in the same file do not disturb yours. Re-list if you rebase or rename in between — see [Hunk IDs](#hunk-ids).
- **Combine predicates for precision**: Use `&` to intersect scope and file queries — `scope("MyClass") & file("src/models.rs")` is safer than either alone, because it won't accidentally capture unrelated changes that happen to be in the same file or an identically-named scope in a different file.
- **Verify the merge**: After splitting, the merge change should be empty. If it's not, something was missed — use `jj-hunk list -r $MERGE` to find and dispatch remaining hunks.
- **Rebase, don't move**: After `jj-hunk split` creates a new change, use `jj rebase` to position it in the graph. The split creates the change as a child of the current revision; rebasing moves it to its logical location.

## Hunk Types

| Type | Meaning |
|------|---------|
| `insert` | New lines added |
| `delete` | Lines removed |
| `replace` | Lines changed (removed + added) |

## Tips

- **Prefer hunkset over JSON**: Hunkset expressions are more readable and composable. Reserve JSON specs for programmatic generation.
- **Use `--format text` for exploration**: Shows semantic context (enclosing function/scope) inline.
- **Use `--format diff` for export**: A unified patch with hunk IDs, appliable with `git apply` (not `git am`) or archivable for review.
- **Prefer IDs for stability**: Hunk IDs (SHA256) are unaffected by concurrent changes elsewhere in the file. Always resolve queries to IDs before executing splits in concurrent workflows — and re-run `list` if you edited or rebased since collecting them.
- **Use `--spec` on `list` to verify**: Preview what a split would select before executing it. An empty result means your query matches nothing; refine it.
- **`"default": "reset"` is safer**: Explicitly include what you want rather than excluding what you don't.
- **Watch the matching mode**: `function("x")` is exact. If a query unexpectedly returns nothing, try `substring:"x"` before assuming the hunk isn't there.
