mod commands;
mod git;
mod review;

use clap::builder::styling::{AnsiColor, Effects};
use clap::{builder::Styles, ArgAction, Args, Parser, Subcommand};
use colored::Colorize;
use commands::{approve_changes, get_review_status, prepare_review_branch};
use git::{
    current_branch_name, current_review_metadata, is_clean, read_review_scope,
    resolve_remote_tracking_branch, ReviewMetadata, ReviewMetadataError, ReviewScopeError,
};
use std::process::exit;

#[derive(Debug)]
enum CliError {
    Git(git::GitCommandError),
    Review(review::ReviewError),
}

impl From<git::GitCommandError> for CliError {
    fn from(error: git::GitCommandError) -> Self {
        Self::Git(error)
    }
}

impl From<review::ReviewError> for CliError {
    fn from(error: review::ReviewError) -> Self {
        Self::Review(error)
    }
}

const STYLES: Styles = Styles::styled()
    .header(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::Cyan.on_default());

/// Pull request partial review tool
#[derive(Parser)]
#[command(name = "cresca")]
#[command(
    about = "Pull request partial review tool.",
    long_about = "A tool to help with pull request partial review. 
    
It is useful when:
    * assignee pushes new changes after the PR is reviewed
    * assignee requests a review before the PR is ready

With this tool you can identify which changes are already reviewed and which are not. It will prepare a review branch and mark reviewed changes as 'committed'. So if the new changes has been pushed to development branch and the assignee requests a new review, you won't confuse which changes are already reviewed and which are not."
)]
#[command(styles = STYLES)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    /// Print executed git commands and their output.
    #[arg(long, global = true, action = ArgAction::SetTrue)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Partially approve the reviewed changes by committing and discard unreviewed changes.
    Approve,
    /// Prepare a review branch.
    Review(ReviewArgs),
    /// Show remaining diff statistics.
    Status(StatusArgs),
}

#[derive(Args)]
struct StatusArgs {
    /// Show unapproved changes across the full pull request.
    #[arg(long)]
    all: bool,
}

#[derive(Args)]
struct ReviewArgs {
    /// The branch where the PR is planned to be merged into.
    to: String,
    /// The development branch to be reviewed.
    from: String,
    /// Skip to this commit (auto-approve earlier commits).
    /// Use `git log --oneline <to>..<from>` to see available commits.
    #[arg(long = "skip-to")]
    skip_to: Option<String>,
    /// Stop at this commit (exclude later commits from review).
    /// Use `git log --oneline <to>..<from>` to see available commits.
    #[arg(long = "stop-at")]
    stop_at: Option<String>,
}

fn main() {
    if let Err(error) = run() {
        render_error(&error);
        exit(1);
    }
}

fn run() -> Result<(), CliError> {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Approve => {
            match current_review_metadata(cli.verbose) {
                Ok(_) => {}
                Err(ReviewMetadataError::Git(error)) => return Err(error.into()),
                Err(error) => exit_invalid_review_branch(error),
            }
            let res = approve_changes(cli.verbose);
            match res? {
                false => println!("There are no reviewed changes to approve. Ending the review."),
                true => println!("Reviewed changes were approved successfully."),
            };
        }
        Commands::Review(args) => {
            if !is_clean(cli.verbose)? {
                eprintln!("{}: Uncommitted changes found. Please commit or stash them before starting review.", "error".red().bold());
                exit(1);
            }

            prepare_review_branch(
                &args.to,
                &args.from,
                args.skip_to.as_deref(),
                args.stop_at.as_deref(),
                cli.verbose,
            )?;
            if is_clean(cli.verbose)? {
                println!("Review branch prepared successfully. However, it seems like there are no unreviewed changes.");
            } else {
                println!("Review branch prepared successfully. Stage the changes you have reviewed and run `{}` to approve them.", "cresca approve".green());
            }
        }
        Commands::Status(args) => {
            let metadata = match current_review_metadata(cli.verbose) {
                Ok(metadata) => metadata,
                Err(ReviewMetadataError::Git(error)) => return Err(error.into()),
                Err(error) => exit_invalid_review_branch(error),
            };
            let (heading, compare_ref, display_label) = if args.all {
                let resolved = resolve_remote_tracking_branch(&metadata.source, cli.verbose)?;
                (
                    "full pull request",
                    resolved.tracking_ref,
                    format!("to {}", metadata.source),
                )
            } else {
                let branch = current_branch_name(cli.verbose)?;
                let scope = match read_review_scope(&branch, cli.verbose) {
                    Ok(scope) => scope,
                    Err(ReviewScopeError::Git(error)) => return Err(error.into()),
                    Err(error) => exit_invalid_review_scope(error, &metadata),
                };
                (
                    "current range",
                    scope.end_oid,
                    "in current review range".to_string(),
                )
            };
            let status = get_review_status(&compare_ref, &display_label, cli.verbose)?;
            println!("📋 Review status ({}):", heading);
            println!(
                "  Remaining diff {}: {} file(s), {} insertion(s), {} deletion(s)",
                status.display_label,
                status.file_count.to_string().yellow(),
                format!("+{}", status.insertions).green(),
                format!("-{}", status.deletions).red()
            );
            if !status.files.is_empty() {
                const MAX_FILES: usize = 10;
                println!("  Files remaining:");
                for file in status.files.iter().take(MAX_FILES) {
                    println!("    - {}", file);
                }
                if status.files.len() > MAX_FILES {
                    println!(
                        "    ... and {} more file(s)",
                        status.files.len() - MAX_FILES
                    );
                }
            }
        }
    }
    Ok(())
}

fn render_error(error: &CliError) {
    match error {
        CliError::Git(error) => render_git_error(error),
        CliError::Review(error) => render_review_error(error),
    }
}

fn render_review_error(error: &review::ReviewError) {
    match error {
        review::ReviewError::Git(error) => render_git_error(error),
        review::ReviewError::Message(message) => {
            eprintln!("{}: {message}", "error".red().bold());
        }
        review::ReviewError::Rollback {
            original,
            diagnostics,
        } => {
            render_review_error(original);
            eprintln!("Rollback or verification also failed:");
            eprintln!("{diagnostics}");
        }
    }
}

fn render_git_error(error: &git::GitCommandError) {
    eprintln!("{}: Failed to {}.", "error".red().bold(), error.description);
    eprintln!("Git arguments: {}", error.args.join(" "));
    match error.status {
        Some(status) => eprintln!("Git exit status: {status}"),
        None => eprintln!("Git exit status: unavailable"),
    }
    eprintln!("Git stdout:");
    eprintln!("{}", String::from_utf8_lossy(&error.stdout));
    eprintln!("Git stderr:");
    eprintln!("{}", String::from_utf8_lossy(&error.stderr));
}

fn exit_invalid_review_scope(error: ReviewScopeError, metadata: &ReviewMetadata) -> ! {
    let reason = match error {
        ReviewScopeError::Missing => "range metadata is missing".to_string(),
        ReviewScopeError::Duplicate => "range metadata has duplicate values".to_string(),
        ReviewScopeError::UnsupportedVersion(version) => {
            format!("range metadata version '{version}' is unsupported")
        }
        ReviewScopeError::Invalid => "range metadata is invalid".to_string(),
        ReviewScopeError::UnavailableCommit(oid) => {
            format!("saved range endpoint '{oid}' is unavailable")
        }
        ReviewScopeError::Git(error) => {
            render_git_error(&error);
            exit(1);
        }
    };
    eprintln!(
        "{}: Cannot show current review range because {}. Rerun `cresca review {} {}` to record the range. `cresca status --all` is still available.",
        "error".red().bold(),
        reason,
        metadata.target,
        metadata.source
    );
    exit(1);
}

fn exit_invalid_review_branch(_: ReviewMetadataError) -> ! {
    eprintln!(
        "{}: Current branch is not a valid cresca review branch because its metadata is missing or invalid; run `{}` to prepare one.",
        "error".red().bold(),
        "cresca review <target> <source>".green()
    );
    exit(1);
}
