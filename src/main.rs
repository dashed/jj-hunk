use anyhow::Result;
use clap::{Args, Parser, Subcommand};

mod absorb;
mod diff;
#[cfg(feature = "semantic")]
mod semantic;
mod glob;
mod hunkset;
mod spec;
mod commands;

use absorb::{AbsorbOptions, InsertionPolicy};
use commands::{BinaryMode, ListFormat, ListGrouping, ListMode, ListOptions, Truncation};

#[derive(Parser)]
#[command(name = "jj-hunk")]
#[command(about = "Programmatic hunk selection for jj")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// List hunks in current changes
    List(ListArgs),

    /// Select hunks (called by jj --tool)
    Select {
        /// Path to "before" directory
        left: String,
        /// Path to "after" directory
        right: String,
    },

    /// Split changes with hunk selection
    Split {
        /// Hunk selection: hunkset expression, JSON/YAML spec, or '-' for stdin
        spec: Option<String>,
        /// Commit message
        message: Option<String>,
        /// Read spec from a file (JSON or YAML)
        #[arg(long = "spec-file", short = 'f')]
        spec_file: Option<String>,
        /// Revision to split (default: @)
        #[arg(short, long)]
        rev: Option<String>,
        /// Allow a selection that keeps nothing (creates an empty commit)
        #[arg(long = "allow-empty")]
        allow_empty: bool,
    },

    /// Commit selected hunks
    Commit {
        /// Hunk selection: hunkset expression, JSON/YAML spec, or '-' for stdin
        spec: Option<String>,
        /// Commit message
        message: Option<String>,
        /// Read spec from a file (JSON or YAML)
        #[arg(long = "spec-file", short = 'f')]
        spec_file: Option<String>,
        /// Allow a selection that keeps nothing (creates an empty commit)
        #[arg(long = "allow-empty")]
        allow_empty: bool,
    },

    /// Squash selected hunks into parent
    Squash {
        /// Hunk selection: hunkset expression, JSON/YAML spec, or '-' for stdin
        spec: Option<String>,
        /// Read spec from a file (JSON or YAML)
        #[arg(long = "spec-file", short = 'f')]
        spec_file: Option<String>,
        /// Revision to squash (default: @)
        #[arg(short, long)]
        rev: Option<String>,
        /// Allow a selection that keeps nothing (creates an empty commit)
        #[arg(long = "allow-empty")]
        allow_empty: bool,
    },

    /// Edit a revision's diff, keeping only the selected hunks
    Diffedit {
        /// Hunk selection: hunkset expression, JSON/YAML spec, or '-' for stdin.
        /// The hunks it names are the ones KEPT
        spec: Option<String>,
        /// Read spec from a file (JSON or YAML)
        #[arg(long = "spec-file", short = 'f')]
        spec_file: Option<String>,
        /// Revision to edit (default: @)
        #[arg(short, long, conflicts_with_all = ["from", "to"])]
        rev: Option<String>,
        /// Show changes from this revision (default: @)
        #[arg(long)]
        from: Option<String>,
        /// Edit changes in this revision (default: @)
        #[arg(short = 't', long)]
        to: Option<String>,
        /// Allow a selection that keeps nothing (discards the whole diff)
        #[arg(long = "allow-empty")]
        allow_empty: bool,
    },

    /// Undo the selected hunks, restoring their content from another revision
    Restore {
        /// Hunk selection: hunkset expression, JSON/YAML spec, or '-' for stdin.
        /// The hunks it names are the ones UNDONE
        spec: Option<String>,
        /// Read spec from a file (JSON or YAML)
        #[arg(long = "spec-file", short = 'f')]
        spec_file: Option<String>,
        /// Undo the changes in this revision (default: @)
        #[arg(short = 'c', long = "changes-in", conflicts_with_all = ["from", "into"])]
        changes_in: Option<String>,
        /// Revision to restore from, the source (default: @)
        #[arg(long)]
        from: Option<String>,
        /// Revision to restore into, the destination (default: @)
        #[arg(short = 't', long, alias = "to")]
        into: Option<String>,
        /// Allow a selection that undoes nothing
        #[arg(long = "allow-empty")]
        allow_empty: bool,
    },

    /// Move hunks into the mutable ancestors that last touched their lines
    Absorb {
        /// Hunk selection: hunkset expression, JSON/YAML spec, or '-' for stdin
        /// (default: every hunk in the revision)
        spec: Option<String>,
        /// Read spec from a file (JSON or YAML)
        #[arg(long = "spec-file", short = 'f')]
        spec_file: Option<String>,
        /// Revision to absorb from (default: @)
        #[arg(short, long)]
        rev: Option<String>,
        /// Print the routing plan without changing anything
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// What to do with hunks that only add lines
        #[arg(long, value_enum, default_value_t = InsertionPolicy::Skip)]
        insertions: InsertionPolicy,
    },
}

#[derive(Args)]
struct ListArgs {
    /// Revset to diff (e.g. @, @-, or a change id)
    #[arg(short, long, conflicts_with_all = ["from", "to"])]
    rev: Option<String>,
    /// Diff from this revision instead of a revision's parent (default: @)
    #[arg(long)]
    from: Option<String>,
    /// Diff to this revision (default: @)
    #[arg(long)]
    to: Option<String>,
    /// Include glob patterns (repeatable)
    #[arg(short = 'i', long)]
    include: Vec<String>,
    /// Exclude glob patterns (repeatable)
    #[arg(short = 'x', long)]
    exclude: Vec<String>,
    /// Group output by directory, extension, or status
    #[arg(long, value_enum, default_value_t = ListGrouping::None)]
    group: ListGrouping,
    /// Output format
    #[arg(long, value_enum, default_value_t = ListFormat::Json)]
    format: ListFormat,
    /// Binary handling
    #[arg(long, value_enum, default_value_t = BinaryMode::Mark)]
    binary: BinaryMode,
    /// Truncate file contents to N bytes before diffing
    #[arg(long)]
    max_bytes: Option<usize>,
    /// Truncate file contents to N lines before diffing
    #[arg(long)]
    max_lines: Option<usize>,
    /// Filter output with a hunkset expression or JSON/YAML spec
    #[arg(long)]
    spec: Option<String>,
    /// Read spec from a file (JSON or YAML)
    #[arg(long = "spec-file", short = 'f')]
    spec_file: Option<String>,
    /// Only list files with hunk counts
    #[arg(long, conflicts_with = "spec_template")]
    files: bool,
    /// Output a spec template instead of hunks
    #[arg(long = "spec-template", conflicts_with = "files")]
    spec_template: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::List(args) => {
            let mode = if args.files {
                ListMode::Files
            } else if args.spec_template {
                ListMode::SpecTemplate
            } else {
                ListMode::Full
            };

            let options = ListOptions {
                rev: args.rev,
                from: args.from,
                to: args.to,
                include: args.include,
                exclude: args.exclude,
                group: args.group,
                format: args.format,
                mode,
                spec: args.spec,
                spec_file: args.spec_file,
                binary: args.binary,
                truncation: Truncation {
                    max_bytes: args.max_bytes,
                    max_lines: args.max_lines,
                },
            };

            commands::list(options)
        }
        Commands::Select { left, right } => commands::select(&left, &right),
        Commands::Split {
            spec,
            message,
            spec_file,
            rev,
            allow_empty,
        } => {
            let (spec, message) = normalize_spec_message(spec, message, &spec_file, "split")?;
            commands::split(spec.as_deref(), spec_file.as_deref(), &message, rev.as_deref(), allow_empty)
        }
        Commands::Commit {
            spec,
            message,
            spec_file,
            allow_empty,
        } => {
            let (spec, message) = normalize_spec_message(spec, message, &spec_file, "commit")?;
            commands::commit(spec.as_deref(), spec_file.as_deref(), &message, allow_empty)
        }
        Commands::Squash { spec, spec_file, rev, allow_empty } => {
            let spec = normalize_spec_only(spec, &spec_file, "squash")?;
            commands::squash(spec.as_deref(), spec_file.as_deref(), rev.as_deref(), allow_empty)
        }
        Commands::Diffedit {
            spec,
            spec_file,
            rev,
            from,
            to,
            allow_empty,
        } => {
            let spec = normalize_spec_only(spec, &spec_file, "diffedit")?;
            commands::diffedit(
                spec.as_deref(),
                spec_file.as_deref(),
                rev.as_deref(),
                from.as_deref(),
                to.as_deref(),
                allow_empty,
            )
        }
        Commands::Restore {
            spec,
            spec_file,
            changes_in,
            from,
            into,
            allow_empty,
        } => {
            let spec = normalize_spec_only(spec, &spec_file, "restore")?;
            commands::restore(
                spec.as_deref(),
                spec_file.as_deref(),
                changes_in.as_deref(),
                from.as_deref(),
                into.as_deref(),
                allow_empty,
            )
        }
        Commands::Absorb {
            spec,
            spec_file,
            rev,
            dry_run,
            insertions,
        } => {
            // Unlike the other commands, the spec is optional: absorb with no
            // selector considers every hunk, which is the usual way to run it.
            if spec.is_some() && spec_file.is_some() {
                anyhow::bail!("absorb: omit <spec> when using --spec-file");
            }
            absorb::absorb(AbsorbOptions {
                spec,
                spec_file,
                rev,
                dry_run,
                insertions,
            })
        }
    }
}

fn normalize_spec_message(
    mut spec: Option<String>,
    mut message: Option<String>,
    spec_file: &Option<String>,
    command: &str,
) -> Result<(Option<String>, String)> {
    if spec_file.is_some() && message.is_none() {
        message = spec.take();
    }

    let message = message
        .ok_or_else(|| anyhow::anyhow!("{command} requires a commit message"))?;

    if spec_file.is_some() {
        if spec.is_some() {
            anyhow::bail!("{command}: omit <spec> when using --spec-file");
        }
        return Ok((None, message));
    }

    let spec = spec
        .ok_or_else(|| anyhow::anyhow!("{command} requires a spec (or use --spec-file)"))?;
    Ok((Some(spec), message))
}

fn normalize_spec_only(
    spec: Option<String>,
    spec_file: &Option<String>,
    command: &str,
) -> Result<Option<String>> {
    if spec_file.is_some() {
        if spec.is_some() {
            anyhow::bail!("{command}: omit <spec> when using --spec-file");
        }
        return Ok(None);
    }

    let spec = spec
        .ok_or_else(|| anyhow::anyhow!("{command} requires a spec (or use --spec-file)"))?;
    Ok(Some(spec))
}
