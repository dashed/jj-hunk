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

List options:
- `--rev <revset>` — diff the revision against its parent (revset must resolve to a single revision)
- `--format json|yaml|text` — output format (default: json)
- `--include <glob>` / `--exclude <glob>` — filter paths (repeatable, supports `**`, `*`, `?`)
- `--group none|directory|extension|status` — group output
- `--binary skip|mark|include` — binary handling (default: mark)
- `--max-bytes <n>` / `--max-lines <n>` — truncate before diffing
- `--spec <hunkset|json|yaml>` / `--spec-file <path>` — filter output with a hunkset expression or JSON/YAML spec
- `--files` — list files with hunk counts only
- `--spec-template` — emit a spec template (JSON/YAML only)

`<spec>` may be a hunkset expression (e.g. `'type(insert) & file("src/*.rs")'`), an inline JSON/YAML string, or `-` to read from stdin. The format is auto-detected. Use `--spec-file <path>` to read a JSON/YAML file (omit `<spec>` when using `--spec-file`).

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
- `"default"` — action for unlisted files (`"keep"` or `"reset"`)

`ids` and `hunks` are merged if both are provided. Use `jj-hunk list --spec-template` to generate an id-based starting spec.

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

### Functions

#### File predicates

| Function | Description |
|----------|-------------|
| `file("path")` | Hunks in file matching path (exact match) |
| `glob("pattern")` | Hunks in files matching glob pattern |
| `extension("rs")` | Hunks in files with given extension |
| `status(modified)` | Hunks in files with given status (modified, added, removed, renamed, copied) |

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

#### Content matching

| Function | Description |
|----------|-------------|
| `content("text")` | Hunks where added or removed text contains "text" |
| `added("text")` | Hunks where added text contains "text" |
| `removed("text")` | Hunks where removed text contains "text" |

#### Identity

| Function | Description |
|----------|-------------|
| `id("hunk-...")` | Select by stable hunk ID (from `jj-hunk list`) |

#### Semantic (tree-sitter powered)

These predicates use tree-sitter to parse source files and extract structural information. Supported languages: Rust, Python, JavaScript, TypeScript, Go, C, C++, Java, Ruby, C#, Scala, Swift, PHP, Bash, Elixir, Erlang, Haskell, OCaml, Zig, Lua.

| Function | Description |
|----------|-------------|
| `function("name")` | Hunks inside a function/method with given name |
| `scope("name")` | Hunks inside a scope (class, struct, impl, module, etc.) |
| `annotation("test")` | Hunks inside functions/scopes with matching annotation/decorator |
| `decorator("route")` | Alias for `annotation()` |
| `doc()` | Hunks that are doc comments |
| `import()` | Hunks that are import/use/require statements |
| `toplevel()` | Hunks not inside any function or scope |
| `depth(0)` | Hunks at nesting depth 0 (top-level); accepts ranges like `depth(0..1)` |
| `all()` | All hunks |
| `none()` | No hunks |

For unsupported languages, `function()`, `scope()`, and other semantic predicates return empty sets with a warning.

### Pattern syntax

String arguments support pattern prefixes, following jj conventions:

| Prefix | Meaning | Example |
|--------|---------|---------|
| (none) | Substring for content, exact for identifiers | `added("TODO")` |
| `exact:"text"` | Exact match | `file(exact:"src/lib.rs")` |
| `substring:"text"` | Substring match | `function(substring:"test")` |
| `glob:"pattern"` | Glob pattern | `file(glob:"src/**/*.rs")` |
| `regex:"pattern"` | Regular expression | `added(regex:"fn\\s+\\w+")` |

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
      "status": "deleted",
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

### List Modes

```bash
# Files-only summary
jj-hunk list --files --format text

# Spec template (ids, default reset)
jj-hunk list --spec-template --format yaml
```

### Filtering and Grouping

```bash
jj-hunk list --include 'src/**' --exclude '**/*.test.rs' --group directory
```

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
