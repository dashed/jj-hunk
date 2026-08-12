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
cargo install --git https://github.com/dashed/jj-hunk --locked
```

**Not `cargo install jj-hunk`.** That name on crates.io is the upstream project
(`laulauland/jj-hunk`, currently 0.3.0) — a different, older program with no hunkset
query language. Installing it gives you a binary that cannot run most of what is
documented here.

**Set `JJ_HUNK_ERROR_FORMAT=json` before you start.** Every failure then arrives as one JSON object
on stderr with a stable `code` to branch on, instead of a paragraph of prose you would have to
pattern-match. Exit code, stdout and the wording all stay as they are, so it costs nothing. See
[Errors](#errors).

```bash
export JJ_HUNK_ERROR_FORMAT=json
```

**No jj config is required.** Every `jj-hunk` verb pins `merge-tools.jj-hunk.program` to the
executable that is running and passes it to `jj` on the command line, so a `[merge-tools.jj-hunk]`
block in `~/.jjconfig.toml` is ignored by them — a stale or wrong one cannot break them either.

That config is load-bearing only on the raw `jj --tool=jj-hunk` path (see
[Direct jj --tool Usage](#direct-jj---tool-usage)), where `jj` resolves the tool itself:

```toml
[merge-tools.jj-hunk]
program = "jj-hunk"                          # on PATH, or an absolute path
edit-args = ["select", "$left", "$right"]
```

Registering a *different* tool that wraps jj-hunk therefore has to use a name other than
`jj-hunk`, and be driven through `jj` directly.

### Semantic predicates need the `semantic` build feature

`function()`, `scope()`, `annotation()`, `decorator()`, `doc()`, `import()`, `toplevel()`, and
`depth()` are backed by tree-sitter and only exist in a binary built with the `semantic` feature.
A binary built without it does not silently return nothing — every one of those predicates fails
with an explicit error:

```
Error: hunkset evaluation error: function() requires the 'semantic' feature (build with --features semantic)
```

If you see that, you are on a `--no-default-features` build — `semantic` is on by default
here, so reinstall without that flag. Every other predicate
(`file`, `glob`, `extension`, `status`, `type`, `lines`, `before_line`, `after_line`, `content`,
`added`, `removed`, `id`, `all`, `none`) works in any build.

You do not have to provoke the error to find out which build you are on:

```bash
jj-hunk schema | jq .build.semantic   # true or false, exit 0 either way
```

## Commands

Nine subcommands. `list` and `schema` are read-only, `select` is plumbing that `jj` invokes, and the
other six rewrite history.

| Command | What it does |
|---------|--------------|
| `list` | Show the hunks in a diff. Read-only — the only one safe to run speculatively |
| `schema` | Describe the hunkset language, the error codes and the verbs as JSON. Read-only, and works outside a repo |
| `split <spec> <msg>` | Move the selected hunks into a new commit |
| `commit <spec> <msg>` | Commit the selected hunks |
| `squash <spec>` | Move the selected hunks into the parent |
| `diffedit <spec>` | Rewrite a revision to contain **only** the selected hunks |
| `restore <spec>` | **Undo** the selected hunks, taking their content from another revision |
| `absorb [<spec>]` | Route each hunk into the mutable ancestor that last touched its lines |
| `select <left> <right>` | Called by `jj --tool=jj-hunk`; not for direct use |

### The trap: the verbs disagree about what a named hunk MEANS

Read this before writing a selector. The same expression means different things per verb:

| Verb | The hunks you name are... | The hunks you do NOT name... |
|------|---------------------------|------------------------------|
| `split` | the ones that **leave**, into the new commit | stay in the original revision |
| `commit` | the ones that **are committed** | stay in the working copy |
| `squash` | the ones that **move** to the destination | stay where they are |
| `restore` | the ones that are **UNDONE** | are left alone |
| `diffedit` | the ones that are **KEPT** | are **discarded** |

`diffedit` and `restore` are near-inverses. Against the same diff, `diffedit 'id(X)'` throws away
everything *except* X; `restore 'id(X)'` throws away *only* X. Confusing the two destroys the
wrong half of the diff.

### `restore` reads its ids from a REVERSED listing

`jj restore` hands its editor the destination on the left and the source on the right, so
`restore` builds its spec against `destination -> source` — the reverse of `jj diff -r`. A hunk id
copied from a plain `jj-hunk list` **does not resolve there**: the removed and added lines are
swapped, and the id is a hash over them.

```bash
$ jj-hunk list --format text                    # the forward diff
M f.txt
  hunk 0 replace hunk-8ece5680 (before 2+1 after 2+1)
    - AAA
    + AAA-changed

$ jj-hunk restore 'id(hunk-8ece5680)'
Error: hunkset evaluation error: hunk id 'hunk-8ece5680' matches no hunk in this diff -- ids change when the hunk's content or its file changes, so one copied from an earlier listing may be stale. Run 'list' again for the current ids.
```

List the diff the way `restore` sees it instead. For a default `restore` (which is
`--changes-in @`) that is `--from @ --to @-`:

```bash
$ jj-hunk list --from @ --to @- --format text
M f.txt
  hunk 0 replace hunk-2bbf2ed8 (before 2+1 after 2+1)
    - AAA-changed
    + AAA

$ jj-hunk restore 'id(hunk-2bbf2ed8)'           # undoes that hunk, leaves the rest
```

In general `restore -c REV` reads from `jj-hunk list --from REV --to REV-`, and
`restore --from A --into B` reads from `jj-hunk list --from B --to A`. Content selectors need the
same care, because `added()` and `removed()` are swapped along with everything else.

### `absorb`

Routes each hunk into the mutable ancestor that last touched its lines, using `jj file annotate` to
find it. With no spec it considers every hunk in the revision.

```bash
$ jj-hunk absorb --dry-run
absorb from ztlvxwwl (no description)
  2 hunks: 2 moving into 2 ancestors, 0 staying

move into zsyysozv c2: touch line 2
  f.txt:2  -1 +1  hunk-fba1c241

move into ykzysqmq c3: touch line 7
  f.txt:7  -1 +1  hunk-5157c417

line numbers are in the parent of ztlvxwwl; an insertion is listed at the line it goes before
--dry-run: nothing was changed
```

**Run `--dry-run` first.** The plan names every destination commit, and it is the only preview
there is.

- **Pure insertions stay put by default.** No line of an insertion blames to an ancestor, so
  `--insertions=skip` (the default) leaves it behind, printing `it only adds lines, so no line of
  it blames to an ancestor`. `--insertions=surrounding` opts into jj's own rule: route it only
  when the lines above and below agree on a destination.
- **Renamed and copied files are refused**, with the reason printed beside the hunk — a rename is
  a whole-file change that would ride into whichever ancestor took its first hunk. Commit the
  rename on its own first, then absorb.
- **The source revision is left empty, not abandoned.**
- **The undo is `jj op restore <id>`, not `jj undo`** — absorb performs several operations and
  `jj undo` reverses only the last. The id is printed on the final line.

An absorb that can route nothing is **not an error**. It prints `Nothing to absorb: every hunk
stays in <rev>.` and exits **0**, so an agent checking only the exit code will read a refused
rename as success. Check the summary line, not the status.

## Ask the binary: `jj-hunk schema`

The query language reference that follows is prose, and prose goes stale. `jj-hunk schema` is the
same information as JSON, generated from the code that implements it, and it costs one read:

```bash
jj-hunk schema                      # one JSON object on stdout, exit 0, no repo needed
```

Reach for it before writing a selector you cannot easily verify, and whenever you need to know
something about *this* binary rather than about jj-hunk in general. Three questions it answers that
`--help` cannot:

1. **Can this predicate reach a change with no hunks?** `reaches_hunkless_changes`. Getting this
   wrong is a wrong answer at exit 0 — see [Changes with no hunks](#changes-with-no-hunks-half-the-predicates-cannot-see-them).
2. **Is this a `semantic` build?** `build.semantic`, and `available` per predicate. The alternative
   is running a tree-sitter predicate and reading the failure.
3. **What can fail, and how do I branch on it?** `errors[]` is the complete code list, so retry
   logic can be written before anything has failed.

### Shape

```jsonc
{
  "schema_version": 1,               // bumped on a breaking change; new fields do not bump it
  "tool":   { "name": "jj-hunk", "version": "0.4.1-my-jj-hunk" },
  "build":  { "semantic": true, "features": ["semantic"] },
  "hunkset": {
    "classes":          [ { "name": "file", "reaches_hunkless_changes": true, "description": "..." } ],
    "pattern_prefixes": [ "exact", "substring", "glob", "regex" ],
    "operators":        [ { "symbol": "~", "position": "prefix", "name": "negation", "description": "..." } ],
    "constants":        [ { "name": "all", "reaches_hunkless_changes": true, "summary": "..." } ],
    "predicates":       [ /* 20 entries, see below */ ]
  },
  "errors":   [ { "code": "UNKNOWN_ID", "category": "selection" } ],
  "commands": [ { "name": "split", "summary": "...", "accepts_selection": true, "has_allow_empty": true } ]
}
```

A predicate entry, in full:

```json
{
  "name": "content",
  "class": "content",
  "reaches_hunkless_changes": false,
  "arity": "one_or_more",
  "argument": "pattern",
  "pattern_prefixes": ["exact", "substring", "glob", "regex"],
  "default_pattern_kind": "substring",
  "available": true,
  "summary": "Match a hunk whose added or removed text matches. Defaults to substring matching."
}
```

| Field | Values | Read it for |
|-------|--------|-------------|
| `class` | `file`, `content`, `semantic` | The coarse grouping. **`semantic` is a kind of `content`** — it never reaches a hunkless change either |
| `reaches_hunkless_changes` | `true`/`false` | The load-bearing fact. `false` means no argument makes this predicate select a binary, a pure rename, a mode-only flip, a retargeted symlink or an empty add |
| `arity` | `none`, `one_or_more`, `zero_or_more` | `zero_or_more` is `annotation()`/`decorator()`, where no argument means "has any" |
| `argument` | `pattern`, `number_or_range`, `none` | |
| `pattern_prefixes` | subset of the four | Empty for numeric arguments. `["exact"]` for `id()`, which resolves its argument rather than matching with it |
| `default_pattern_kind` | one of the four, absent when there is none | What an unprefixed argument means. `file(a.rs)` is exact, `added(TODO)` is a substring |
| `values` | present only for `status()` and `type()` | The closed set. A value outside it is an error, not an empty result |
| `available` | `true`/`false` | False in a build missing `requires_feature` |

### Recipes

```bash
# Which predicates can carry a rename or a binary along with a selection?
jj-hunk schema | jq -r '.hunkset.predicates[] | select(.reaches_hunkless_changes) | .name'
# file glob extension status type

# Is this a semantic build, without provoking an error?
jj-hunk schema | jq .build.semantic

# The valid status() values, rather than guessing "deleted" and selecting nothing
jj-hunk schema | jq -r '.hunkset.predicates[] | select(.name=="status") | .values[]'

# Every error code, for a retry table written up front
jj-hunk schema | jq -r '.errors[] | "\(.category)\t\(.code)"'
```

`schema` describes the language, not the flags — for those, `--help` is generated from the same
definitions and cannot drift either. What it deliberately does *not* carry is a per-flag command
schema: `commands[]` is an index (name, one-line summary, whether the verb takes a hunkset, whether
it has `--allow-empty`) and `jj-hunk <verb> --help` has the rest.

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

**A renamed or copied file matches either of its paths.** `file()`, `glob()` and `extension()`
reach it by where it is now *and* by where it came from, which is what `--include` / `--exclude`
have always done:

```bash
jj-hunk list --spec 'file("secret/keys.txt")'   # the path it was renamed from
jj-hunk list --spec 'file("exposed.txt")'       # the path it has now
```

The old path is the point: it is the name you have when you are looking for what used to be
somewhere. Three consequences:

- `~glob("secret/*")` **drops** a file renamed out of `secret/` — the diff still spells
  `secret/keys.txt` on its left side, so keeping it handed you what you excluded.
- `extension()` matches both sides of a rename that changed the extension: `mod.txt` → `mod.rs`
  answers to `extension("rs")` and to `extension("txt")`.
- Matching only. The spec is still keyed by the **new** path with the old one in `from` — that is
  the path `select` resolves a file by.

Only when jj reports a rename or copy. A move too large for jj's rename detection is an ordinary
add plus delete, and each path names its own change.

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
| `id("hunk-399b086c")` | Select by hunk ID — full, short, or any unambiguous prefix |
| `id(hunk-399b086c)` | The quotes are optional on an id |
| `id("hunk-399b086c", "hunk-3274da35")` | Multiple IDs in one call; the forms may be mixed |
| `all()` / `none()` | Everything / nothing |

**`id()` is the one predicate that errors instead of returning nothing.** Every other predicate
narrows a set, so matching nothing is a legitimate empty result; `id()` resolves a name, so a name
that resolves to nothing is a mistake:

```
Error: hunkset evaluation error: hunk id 'hunk-deadbeef' matches no hunk in this diff -- ids change when the hunk's content or its file changes, so one copied from an earlier listing may be stale. Run 'list' again for the current ids.
```

This fires on `list --spec` too, unlike the spec validator described under
[When an ID does not resolve](#when-an-id-does-not-resolve). A prefix that matches more than one
hunk — or the bare `hunk-` — is likewise rejected with the candidates named, never guessed at.

`id()` resolves ids, it does not pattern-match them: `substring:`, `glob:` and `regex:` on `id()`
are rejected. Only `exact:` is meaningful, and it means "do not treat this as an abbreviation".
See [Hunk IDs](#hunk-ids) for the two forms and their shelf life.

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

For a file in an unsupported language, semantic predicates contribute nothing. When *nothing* in
the diff could be parsed, that is said on stderr:

```
warning: function() found no semantic metadata -- no parser is available for: notes.txt, only.kt.
The empty result reflects missing language support, not an absence of matches.
```

**The warning is not per-file, and this is the trap.** It fires only when no file in the diff was
parsed at all. A diff holding one `.rs` and one `.txt` parses the `.rs`, so an empty
`function("nope")` result comes back **silently** — indistinguishable from a genuine no-match, with
the `.txt` unmentioned. Do not read "no warning" as "every file was understood". If it matters
whether a file was analysed, check `semantic.is_analyzed` in `--format json` rather than inferring
it from stderr.

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

The quotes may be dropped around a single bare word, which then follows the same table —
`content(let)` is the substring `let`. Anything containing a space or a dot needs the quotes, so
`content("let v")` and `file("wide.rs")` are the safe forms; `file(wide.rs)` is a parse error.

**A malformed glob is an error, everywhere.** `glob()`, `file(glob:)`, `--include` and `--exclude`
all reject one outright rather than matching nothing — which matters most under `~`, where
"matches nothing" would have inverted into "matches everything":

```
Error: hunkset evaluation error: invalid glob 'src/[': unclosed '[' -- a character class needs a matching ']'
```

### Changes with no hunks: half the predicates cannot see them

Five kinds of change have nothing to select *within* — a binary file, a symlink, a mode-only
flip, a rename or copy whose text did not move, and an add or remove of an empty file. Each is
selected **whole** or not at all. Which predicates can reach one is a hard split:

| Reach them (select them whole) | Never reach them |
|--------------------------------|------------------|
| `all()`, `file()`, `glob()`, `extension()`, `status()`, and any negation `~x` | `content()`, `added()`, `removed()`, `lines()`, `id()` |

The right column is not an oversight. These changes are given a stand-in hunk that carries a path
and a status and no text at all, and the stand-in is *marked* as one so that a content-level
predicate declines it outright. The marking is what does the work: empty text is not unmatchable
text, and both `content("")` and `lines(0..N)` are true of it. A question about content is refused
rather than answered about bytes that were never diffed — a link's target and a renamed file's body
are not in any hunk, so there was nothing there to match on.

**This is the trap: a `content()`-only selector silently leaves every one of them behind.** It is
not an error and nothing warns you — they just stay in the revision you were emptying:

```bash
$ jj-hunk list --format text
M blob.bin [binary]
A brand_new_empty.txt
M config [symlink, whole-file only]
R moved_elsewhere.txt (moved.txt -> moved_elsewhere.txt)
M script.sh [mode 100644 -> 100755, not selectable]
M text.txt
  hunk 0 replace hunk-ac0f7677 (before 2+1 after 2+1)
    - world
    + WORLD

$ jj-hunk split 'content("WORLD")' "text only"   # succeeds, exit 0
$ jj-hunk list --format text                     # ... but all of this is left over
M blob.bin [binary]
A brand_new_empty.txt
M config [symlink, whole-file only]
R moved_elsewhere.txt (moved.txt -> moved_elsewhere.txt)
M script.sh [mode 100644 -> 100755, not selectable]
```

The same split is machine-readable, so a query can be checked before it is run rather than after:

```bash
jj-hunk schema | jq -r '.hunkset.predicates[] | select(.reaches_hunkless_changes) | .name'
# file glob extension status type
```

When a selection is meant to cover everything, reach for a predicate from the left column —
`all()`, `all() ~ content("...")`, `glob("**")`, or `--spec-template`, which names every one of
them explicitly. To pin one down in a JSON spec use `{"action": "keep"}`; `{"ids": []}` resets it,
restoring the parent byte-for-byte, and works on symlinks and non-UTF-8 paths too.

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
  hunk 0 insert hunk-399b086c (before 2+0 after 2+1)
    + use std::io;
  hunk 1 insert hunk-e338f6bc (before 5+0 after 6+1) in UserService
    +     hits: u64,
  hunk 2 replace hunk-3274da35 (before 9+1 after 11+1) in UserService::handle_request
    -         let a = 1;
    +         let a = 111;
```

The trailing `in UserService` / `in UserService::handle_request` come from the semantic analyzer;
a binary built without the `semantic` feature prints the same lines without them. A file with no
text hunks — a binary, a symlink, a pure rename — is listed with its status and no hunk lines at all:

```
M blob.bin [binary]
M config [symlink, whole-file only]
A empty-add.txt
R moved.txt (tomove.txt -> moved.txt)
```

`--format diff` produces a unified patch whose `@@` headers carry the enclosing function name (with
the `semantic` feature; without it, the ID alone) and the hunk's short ID:

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

Combine it with `--spec` to export just the hunks a query selects. The filtered patch is
re-anchored to its own context, so it applies at the right line rather than at the unfiltered
offset:

```bash
$ jj-hunk list --spec 'id(hunk-25117ece)' --format diff
--- a/f.txt
+++ b/f.txt
@@ -14,7 +14,7 @@ [hunk-25117ece]
 L14
 L15
 L16
-L17
+L17-changed
 L18
 L19
 L20
```

### Trimming the listing: `--fields`

A JSON listing is mostly `removed`, `added` and `context`, and the loop you are running — list,
pick an id, act on it — reads none of them. You will read that text again anyway when you open the
file. Ask for the skeleton instead:

```bash
jj-hunk list --format json --fields 'path,hunks.id,hunks.type'
```

On a 138-line diff across three source files (69 hunks) that is 9,719 bytes instead of 66,248 —
**6.8x smaller**. `hunks.short_id` in place of `hunks.id` makes it 10.5x; both resolve in `id()`
and in a spec's `ids`. Add `hunks.before` when you need to know *where* a hunk is without reading
it.

Names: file fields bare — `path`, `status`, `rename`, `binary`, `mode`, `symlink`, `truncated` —
and hunk fields as `hunks.<name>` — `index`, `id`, `short_id`, `type`, `removed`, `added`,
`before`, `after`, `context`, `enclosing_function`, `enclosing_scope`, `annotations`,
`is_doc_comment`, `is_import`, `is_toplevel`, `nesting_depth`, `is_analyzed`. `hunks` alone means
every hunk field and `hunks.semantic` the eight semantic ones. The `hunks.` prefix is optional, so
`--fields 'path,id,type'` works. Repeatable and comma-separated.

Three things to know:

- **Five file flags come back whether you ask or not**: `rename`, `binary`, `mode`, `symlink`,
  `truncated`. Each appears only when it is true, so a mask that could hide one would let you read
  "not truncated" off a truncated listing. `truncated` means the hunks you are looking at are a
  prefix of the real diff; `rename.from` is what the raw `jj --tool=jj-hunk` path needs to not lose
  the file.
- **A misspelled name is an error, not an omission.** `--fields 'paht,id'` exits 1 with
  `INVALID_FIELDS` and `details.valid_fields`; it does not hand you entries with no `path`.
- **It is json/yaml only.** With `--format text`, `--format diff`, `--files` or `--spec-template`
  it exits 1 with `INCOMPATIBLE_OPTIONS` rather than quietly ignoring you.

**Applying a `--format diff` patch: use `git apply`, not `git am`.** The output is a bare unified
diff with no mail headers, so `git am` rejects it outright (`Patch format detection failed`).
`git apply` handles it, including added files, deleted files, CRLF line endings, and missing
trailing newlines. Renames are the exception — the patch records a rename as a plain modification
of the new path, so it will not apply against a tree that still has the old path.

## Errors

Every failure exits **1**, writes prose to **stderr**, and leaves **stdout empty**. That is the
default and it is not changing: `list --format json` writes its result to stdout, so a caller reads
non-empty stdout as "here are the hunks", and an error object arriving there would make a failed run
look like a successful one carrying unexpected fields.

For a caller that needs to branch on *what* went wrong, opt into the structured form:

```bash
# per call
jj-hunk --error-format json list --spec 'id(hunk-deadbeef)'

# or once, for a whole session -- this also reaches the nested `jj-hunk select`
# that the mutating verbs run through `jj --tool=jj-hunk`, which no flag can
export JJ_HUNK_ERROR_FORMAT=json
```

Still exit 1, still nothing on stdout, still stderr — but one JSON object on one line:

```json
{
  "error": "selection",
  "code": "UNKNOWN_ID",
  "message": "hunkset evaluation error: hunk id 'hunk-deadbeef' matches no hunk in this diff -- ids change when the hunk's content or its file changes, so one copied from an earlier listing may be stale. Run 'list' again for the current ids.",
  "details": { "id": "hunk-deadbeef" }
}
```

| Field | |
|-------|--|
| `error` | Coarse category: `parse`, `selection`, `revset`, `usage`, `internal` |
| `code` | The stable identifier to branch on |
| `message` | Exactly the text human mode prints, `Caused by:` chain included — nothing is lost by opting in |
| `details` | The machine-actionable facts, keyed per code |

The flag beats the environment variable, so one call can opt back out of a session-wide setting.
Both accept `human` and `json`, case-insensitively. An unrecognised value is a usage error rather
than a silent fallback to prose — a caller that believes it opted in and did not would parse prose
as JSON forever.

**Not covered:** argument errors caught by the argument parser itself — an unknown flag, a missing
subcommand, `--rev` together with `--from` — exit **2** with prose on stderr, whatever
`--error-format` says. They are raised before the setting is known, and they already exited 2 before
this feature existed.

### Codes

**The codes are a public contract.** Renaming one is a breaking change; adding one is not. Branch on
`code`, never on `message`: the ambiguity, empty-selection and unresolved-path wordings have each
been rewritten within the last few releases, and a caller matching their text would have broken
three times.

The table below is prose. `jj-hunk schema` carries the same list as data, generated from the
declarations themselves, so a retry table can be built before anything has failed:

```bash
jj-hunk schema | jq -r '.errors[] | "\(.category)\t\(.code)"'
```

| `code` | Raised when | `details` carries | What to do about it |
|--------|-------------|-------------------|---------------------|
| `PARSE_ERROR` | The expression is not syntactically a hunkset | `input`, `position` (character offset into the whole expression), `line` (1-based), `column` (0-based, within that line) | Fix the syntax at `position`; the same offset the `^` is drawn from. Not retryable as sent |
| `UNKNOWN_FUNCTION` | No predicate by that name | `function` | A typo or an invented predicate. Check it against the predicate list; do not retry unchanged |
| `INVALID_ARGUMENT` | A predicate rejected the value or the argument shape | `function`, `value`, `valid` | Re-issue with one of the values named in `valid` |
| `INVALID_GLOB` | A glob pattern does not compile | `pattern`, `reason` | Fix the pattern. Do not treat it as "matched nothing": under `~` that inverts to "keep everything" |
| `INVALID_REGEX` | A regex pattern does not compile | `pattern`, `reason` | Same |
| `UNKNOWN_ID` | `id()` named a hunk this diff does not contain | `id` | Almost always a stale id — they are hashes of the hunk's content and context. Re-run `list` and use an id from that run |
| `AMBIGUOUS_ID` | An abbreviation reaches more than one hunk | `prefix`, `count`, `candidates[]` of `{short_id, path}` | Retry with one of `candidates[].short_id`. Each is already unambiguous over this diff, so no lengthening loop is needed |
| `SEMANTIC_FEATURE_REQUIRED` | A semantic predicate in a build without the `semantic` feature | `predicate` | Rewrite without semantic predicates, or install a default build. Cannot occur in a default build, where `semantic` is on |
| `EMPTY_SELECTION` | The selection resolved and keeps nothing | `selector` (`null` when it came from `--spec-file`), `listing_command` | Widen the selector, or pass `--allow-empty` if empty really was the intent. `listing_command --spec <selector>` shows what it matched |
| `PATH_NOT_IN_DIFF` | A JSON/YAML spec names a path, id or index the diff does not contain | `problems[]` (see below), `listing_command` | Fix the named entries. Every problem carries the `path` to edit |
| `REVSET_UNRESOLVED` | jj could not resolve the revset, or it named no revision | `revset`, `resolved` (always `0`), `jj_stderr` (only when jj itself rejected it) | Fix the revset. `jj_stderr` is jj's own explanation of why |
| `REVSET_AMBIGUOUS` | The revset resolved to more than one revision | `revset`, `resolved` (the true count), `revisions[]` (the first few) | Narrow it. A hunk is only defined between one revision and its parent |
| `TRUNCATED_SPEC_TEMPLATE` | `--spec-template` over files `--max-bytes`/`--max-lines` cut short | `paths[]` | Drop the limits for those files. Ids from a truncated file are hashes of text the real diff does not have |
| `INVALID_FIELDS` | `--fields` named something that is not an output field, or named nothing at all | `fields[]` (the names refused), `valid_fields[]` (every name accepted, hunk fields in their dotted form), `always_included[]` (the file flags no mask can drop) | Re-issue with names from `valid_fields`. Nothing was printed: stdout is empty, so a missing key means a refused run, not a missing value |
| `INCOMPATIBLE_OPTIONS` | Two options that cannot both be honoured: `--fields` with `--files`, `--spec-template`, `--format text` or `--format diff`; or `--spec-template` with a text format | `option`, `incompatible_with` | Drop one of the two it names |
| `UNKNOWN` | Anything not yet classified — I/O errors, failed `jj` invocations | `{}` | Read `message`. Do not build behaviour on `UNKNOWN`: a failure may be given a real code in a later release |

`PATH_NOT_IN_DIFF` reports the spec as a whole, so `details.problems` has one entry per failed
reference. Every entry has a `path` and a `kind`:

| `kind` | Also carries | Means |
|--------|--------------|-------|
| `no-such-path` | — | The diff contains no such file |
| `renamed` | `renamed_to` | The entry is keyed by a rename's old path; file it under `renamed_to` |
| `no-such-id` | `id` | That file has no hunk with that id — usually stale |
| `ambiguous-id` | `id`, `count` | The prefix names `count` hunks in that file; use a longer one |
| `no-such-index` | `index`, `hunk_count` | `index` is out of range; the file has `hunk_count` hunks |

`AMBIGUOUS_ID` and `ambiguous-id` are the same fault at two layers, and both appear because a
hunkset fails per predicate while a spec fails as a whole document: `id("hunk-2")` in a hunkset
raises `AMBIGUOUS_ID`, and `{"ids": ["hunk-2"]}` in a spec raises `PATH_NOT_IN_DIFF` with an
`ambiguous-id` problem naming the file.

A worked retry, which is the shape most of these take:

```bash
$ jj-hunk --error-format json list --spec 'id("hunk-2")'
{"error":"selection","code":"AMBIGUOUS_ID","message":"...","details":{
  "prefix":"hunk-2","count":2,
  "candidates":[{"short_id":"hunk-21748da5","path":"many.txt"},
                {"short_id":"hunk-253c1b1c","path":"many.txt"}]}}

$ jj-hunk list --spec 'id("hunk-21748da5")'   # one hunk, no guessing
```

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
candidates, never a guess. `exact:` does **not** demand the full 64 hex —
`id(exact:"hunk-f5696093")` resolves the short form fine. All `exact:` does is switch off
abbreviation: it accepts the full id or the exact short id, and rejects every other prefix,
`hunk-f569` and `hunk-f5696093a5ac` alike.

### How long an ID stays valid

Long enough to carry it from one command to the next against the same working-copy state. That is
the workflow — `list`, choose, `split` — and within it IDs are solid.

**Survives:** other hunks appearing or disappearing elsewhere in the same file (concurrent agent
work), edits to other files, and line numbers shifting — positions are not hashed. It survives an
edit *inside* its own three-line context window too, because the context is read from the parent
side and the parent side did not change.

It also survives **being listed from a different working directory**. The path folded into the
hash is the one relative to the workspace root, so the same hunk is `hunk-64640aa9` whether it is
listed from the repo root as `sub/deep.txt` or from `sub/` as `deep.txt`. Note that what `list`
*prints* is still relative to your current directory — only the identity is frame-independent.

**Does not survive:**

- **renaming or moving the file** — the path is hashed;
- **an edit touching a line immediately adjacent to the hunk**, which merges the two into one larger
  hunk with different text. A line of untouched code in between keeps them separate;
- **a rebase, or a squash into the parent**, when it rewrites the lines around the hunk. Context is
  read from the parent side. Treat any rebase as invalidating;
- **reversing the diff.** `--from A --to B` and `--from B --to A` swap the removed and added lines,
  so every id differs. This is what makes `restore` ids distinct — see
  [`restore` reads its ids from a REVERSED listing](#restore-reads-its-ids-from-a-reversed-listing).

**So: re-run `list` after editing, and use the IDs from that run.** Do not cache IDs across an
editing session, a rebase, or a rename.

### When an ID does not resolve

Every mutating verb — `split`, `commit`, `squash`, `diffedit`, `restore` — validates each entry of
a **JSON/YAML spec** before touching anything, and refuses the whole operation if one does not name
exactly what it meant to:

```
Error: spec does not resolve against the diff:
  wide.rs: no hunk with id hunk-deadbeef
Those entries do not name exactly what they meant to. Check them against `jj-hunk list --spec-template`.
```

The middle line names the specific problem — `no hunk with id ...` (usually a stale ID), `id hunk-3
is ambiguous, it names 3 hunks -- use a longer prefix`, `no hunk with index 99 (file has 10)`,
`no such path in the diff`, or, for a spec keyed by a rename's old name,
`old.txt: renamed to new.txt in this diff -- file the entry under new.txt instead`.

**`--allow-empty` does not silence this.** That flag says an empty *result* is acceptable; it never
meant "do not check whether what I wrote refers to anything". A typo'd path or a stale ID is still
reported with the flag set. The listing named in the message is the one matching the verb's own
diff, so `restore` points at its reversed listing with the revisions already resolved — `jj-hunk
list --from 395f21b77162 --to 8f3be992928c --spec-template` — rather than sending you back to the
plain listing that produced the bad id.

A spec **may** name paths that are absent from the diff — a reusable allowlist stays usable as
files come and go — as long as it still keeps at least one path that *is* present. Only a spec that
keeps nothing real is rejected. That applies to bare `{"action": "keep"}` entries. An entry that
names `ids` or `hunks` under an absent path is **always** rejected, however much else resolved:
those ids were read off a real diff, so the path is a typo. Tolerating it committed a subset of
what the spec asked for, at exit 0, with nothing on stderr.

`jj-hunk list --spec` does not run this check, so a JSON spec naming a stale ID or a missing path
simply selects nothing there, which is what makes `list --spec` safe to iterate with. A **hunkset**
is different: `id()` resolves ids as it evaluates, so `list --spec 'id(hunk-deadbeef)'` errors even
on `list`. So do a malformed glob and a semantic predicate in a non-`semantic` build.

## Core Workflow

### 1. Explore hunks with hunkset queries

```bash
# See all hunks
jj-hunk list --format text

# Preview what a query would select
jj-hunk list --spec 'scope("UserService")' --format text

# Verify a selection covers everything you expect
jj-hunk list --spec 'scope("UserService") ~ id("hunk-e338f6bc", "hunk-3274da35")' --format text
# Empty output = those ids cover everything in that scope
```

Other `list` options:

- `--rev <revset>` — diff the revision against its parent (must resolve to a single revision)
- `--from <rev> --to <rev>` — diff two revisions explicitly; mutually exclusive with `--rev`. This
  is how you list the diff `restore` works against
- `--include <glob>` / `--exclude <glob>` — filter paths; one pattern per flag, repeatable. A
  malformed glob is an error, not a pattern that matches nothing
- `--group none|directory|extension|status` — group output as `groups: [{name, files}]`
- `--binary skip|mark|include` — binary handling (default: `mark`, which lists the file with 0 hunks)
- `--max-bytes <n>` / `--max-lines <n>` — truncate file contents before diffing
- `--files` — list files with hunk counts only
- `--spec-template` — emit an ID-based starting spec (JSON/YAML only; `text` and `diff` are rejected)
- `--fields <names>` — emit only these fields of a JSON/YAML listing. **Use it.** See
  [Trimming the listing](#trimming-the-listing---fields)

`list` shows changes that have no hunks at all — binaries, symlinks, pure renames and copies,
mode-only changes, and empty-file adds and removes. `--spec-template` emits `{"action": "keep"}`
for them (plus `"from"` for a rename or copy), so the template always covers the whole diff.

A **hunkset expression reaches all of those, but only through a file-level predicate** — each is
given a stand-in hunk carrying its path and status for `all()`, `file()`, `glob()`, `extension()`,
`status()` and `~x` to match, and a selected one becomes `{"action": "keep"}`. A *content-level*
predicate still cannot reach one, so `jj-hunk split 'content("...")'` leaves every symlink, rename,
mode flip, empty add and binary behind, at exit 0. See
[Changes with no hunks](#changes-with-no-hunks-half-the-predicates-cannot-see-them).

Paths — in output, in spec keys, and in `file()`/`glob()` — are **relative to the current
directory**, not the repo root. Running from a subdirectory works, and the paths differ from a run
at the root (`src/svc.rs` there, `svc.rs` from inside `src/`). **Hunk IDs do not.** The path folded
into the hash is workspace-root-relative, so an ID is the same string whichever directory you ask
from, and a spec generated at the root resolves from a subdirectory.

### 2. Select and split

Use hunkset expressions directly with any of the mutating verbs — the format is auto-detected, so
anything that isn't JSON/YAML is parsed as a hunkset. Remember that the same expression means
different things per verb; see [the trap](#the-trap-the-verbs-disagree-about-what-a-named-hunk-means):

```bash
# Split by semantic scope
jj-hunk split 'scope("UserService") & ~import()' "refactor: update UserService"

# Split by file pattern
jj-hunk split 'glob("src/api/**")' "feat: add API endpoints"

# Split by content
jj-hunk split 'added("TODO") | added("FIXME")' "chore: add TODOs"
```

A selection that matches nothing is a hard error, from every mutating verb:

```
Error: selection matched no hunks: content("zzzznomatch")
An empty selection is nearly always a mistyped selector rather than an intent, so it is refused.
Check it against `jj-hunk list --spec ...`, or pass --allow-empty if that is what you meant.
```

This is the main guard against a typo'd selector silently producing a no-op. What it would
otherwise have carried out differs per verb — an empty commit for `split`, a **discarded diff** for
`diffedit`, nothing at all for `restore`. Pass `--allow-empty` only when that outcome is genuinely
what you want; it does not excuse a spec that names things which do not exist (see
[When an ID does not resolve](#when-an-id-does-not-resolve)).

### 3. Use stable IDs for safety

When other changes may arrive concurrently, **always resolve your hunkset query to hunk IDs before executing the split**. This protects against new hunks appearing between the query and the split:

```bash
# Step 1: Query to find the hunks you want
jj-hunk list --spec 'function("handle_request") & glob("src/api/**")' --format text

# Step 2: Note the hunk IDs from the output, then split using IDs
jj-hunk split 'id("hunk-399b086c", "hunk-3274da35")' "feat: handle_request implementation"
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
    "src/svc.rs": {"ids": ["hunk-399b086c", "hunk-3274da35"]},
    "src/bar.rs": {"action": "keep"},
    "src/qux.rs": {"action": "reset"}
  },
  "default": "reset"
}
```

Keys are paths **relative to the current directory**, and an ID belongs to the path it was listed
under — the path is part of the hash, so these two IDs are only valid under `src/svc.rs`, listed
from the repo root.

| Spec | Effect |
|------|--------|
| `{"hunks": [0, 2]}` | Include only hunks 0 and 2 (indices or ID strings) |
| `{"ids": ["hunk-399b086c"]}` | Include hunks by ID (full, short, or unambiguous prefix) |
| `{"action": "keep"}` | Include all changes in the file |
| `{"action": "reset"}` | Discard all changes in the file |
| `"default": "reset"` | Unlisted files are discarded |
| `"default": "keep"` | Unlisted files are kept |

`ids` and `hunks` are merged if both are provided. Specs may be inline, read from stdin with `-`,
or loaded with `--spec-file`. `jj-hunk list --spec-template` generates an ID-based starting spec.

**Renamed files carry a `from` field.** `select` is handed two directories and nothing else, so it
cannot tell that `right/new_name` used to be `left/old_name`; `from` supplies that link:

```json
{"files": {"new_name.txt": {"ids": ["hunk-111b8dea"], "from": "old_name.txt"}}, "default": "reset"}
```

The mutating verbs fill this in for you from jj's rename detection, so a hand-written spec that
omits it still works. It is load-bearing only on the raw `jj --tool=jj-hunk` path below, where
omitting it drops the file from the commit entirely.

Key the entry under the **new** name. Using the old one is an error that names the replacement:

```
Error: spec does not resolve against the diff:
  old.txt: renamed to new.txt in this diff -- file the entry under new.txt instead
```

Two names for matching, one for keying: a hunkset expression may *select* the file by its old path,
but the spec it produces is always keyed by the new one.

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
jj-hunk split 'id("hunk-399b086c", "hunk-3274da35")' "feat: add request handler"

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
- **Check which verb you are holding**: `diffedit` keeps what you name, `restore` undoes what you name. Re-read [the trap](#the-trap-the-verbs-disagree-about-what-a-named-hunk-means) before either.
- **`--dry-run` before every `absorb`**, and read the summary line rather than the exit code — a refused rename exits 0.
- **Do not trust exit 0 to mean "everything moved"**: a `content()`-only selector leaves every hunkless change behind — binaries, symlinks, renames, mode flips, empty adds — silently. Run `list` again afterwards to see what is left.
