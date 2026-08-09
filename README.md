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

# Emit a spec template using stable ids
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

Split and squash accept `-r <rev>` to target any revision (default: `@`). Commit always operates on the working copy.

Split, commit, and squash all accept `--allow-empty` to permit a selection that keeps nothing. Without it, a selection that matches no hunks is an error rather than a silent empty commit — the main guard against a typo'd selector producing a no-op.

List options:
- `--rev <revset>` — diff the revision against its parent (revset must resolve to a single revision)
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
if any file was actually truncated — the template would contain ids that `split` is guaranteed to
reject.

## Spec Format

Specs can be **JSON or YAML**. Inline JSON is convenient for short specs; use `--spec-file` or stdin for larger ones. You can select hunks by index (`hunks`) or by stable `ids` (sha256) emitted by `jj-hunk list`. IDs are emitted as `hunk-<sha256>`. `hunks` entries may also be id strings.

```json
{
  "files": {
    "path/to/file": {"hunks": [0, "hunk-7c3d...", 2]},
    "path/to/other": {"ids": ["hunk-9a2b..."]},
    "path/to/another": {"action": "keep"},
    "path/to/skip": {"action": "reset"}
  },
  "default": "reset"
}
```

- `{"hunks": [indices|ids]}` — select by index (0-based) or id string
- `{"ids": ["hunk-..."]}` — select hunks by id from `jj-hunk list`
- `{"action": "keep"}` — keep all changes in file
- `{"action": "reset"}` — discard all changes in file
- `{"from": "old/path"}` — source path of a renamed or copied file (see below)
- `"default"` — action for unlisted files (`"keep"` or `"reset"`)

`ids` and `hunks` are merged if both are provided. Use `jj-hunk list --spec-template` to generate an id-based starting spec.

### Renamed files: the `from` field

A file entry for a renamed or copied file carries an extra `from` field naming its source path. It
is keyed under the *new* path, and `--spec-template` emits it for you:

```yaml
files:
  new_name.txt:
    ids:
    - hunk-b55c2cba516e...
    from: old_name.txt
default: reset
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

## Hunkset Query Language

As an alternative to JSON/YAML specs, jj-hunk supports a **hunkset** query language inspired by jj's [filesets](https://docs.jj-vcs.dev/latest/filesets/) and [revsets](https://docs.jj-vcs.dev/latest/revsets/). Hunkset expressions are auto-detected (anything that doesn't start with `{` or `[`).

```bash
# Split: all insertions in Rust files
jj-hunk split 'type(insert) & glob("src/**/*.rs")' "add new code"

# List only hunks inside a specific function
jj-hunk list --spec 'function("parse_spec")'

# Verify a selection covers all changes in a scope
jj-hunk list --spec 'function("apply") ~ id("hunk-7c3d...")'
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
- A malformed pattern (`[abc`, `x{a`) matches nothing rather than raising an error.

The same matcher backs `file(glob:"...")` and the `--include` / `--exclude` flags.

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

#### Identity

| Function | Description |
|----------|-------------|
| `id("hunk-...")` | Select by stable hunk ID (from `jj-hunk list`); an unambiguous prefix is enough |
| `id("hunk-a...", "hunk-b...")` | Several IDs in one call |

A prefix matching more than one hunk — or the bare `hunk-` — is rejected rather than guessed at.

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
          "id": "hunk-4c1b1b3...",
          "index": 0,
          "type": "replace",
          "removed": "old_fn()\n",
          "added": "new_fn()\n",
          "before": {"start": 10, "lines": 1},
          "after": {"start": 10, "lines": 1},
          "context": {"pre": "// prev\n", "post": "// next\n"}
        }
      ]
    },
    {
      "path": "src/main.rs",
      "status": "removed",
      "hunks": [
        {
          "id": "hunk-771ad9f...",
          "index": 0,
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
- Each hunk includes a stable `id` (sha256), `index`, line ranges (`before`/`after`), and optional `context`.
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
semantic analysis is available) and the hunk id:

```diff
--- a/wide.rs
+++ b/wide.rs
@@ -17,7 +17,7 @@ func_2 [hunk-8e7229073a43...]
 
 
 fn func_2() {
-    let v = 2;
+    let v = 200;
 }
```

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

1. You run `jj-hunk split/commit/squash` with a JSON/YAML spec
2. jj-hunk writes the spec to a temp file and sets `JJ_HUNK_SELECTION` env var
3. jj-hunk passes temporary `merge-tools.jj-hunk` config to jj when needed
4. jj invokes `jj-hunk select $left $right` as the diff tool
5. jj-hunk reads the spec and modifies `$right` to include only selected hunks
6. jj snapshots the result

For direct control with `jj` itself, provide the same tool config explicitly or define it in your jj config:

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
