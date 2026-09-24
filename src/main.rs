mod branch_naming;
mod branch_ref;
mod commands;
mod git;
mod progress;
mod review;
mod review_identity;

use clap::builder::styling::{AnsiColor, Effects};
use clap::{builder::Styles, ArgAction, Args, Parser, Subcommand};
use colored::Colorize;
use commands::{approve_changes, get_review_status, prepare_review_branch};
use git::{current_branch_name, current_review_metadata, read_review_scope, ReviewScopeError};
use progress::WaitIndicator;
use review_identity::{ReviewIdentityReadError, ReviewSide, StoredReviewIdentity};
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
    Status,
}

#[derive(Args)]
struct ReviewArgs {
    /// Target as a plain branch name, `refs/heads/<name>`, or `<remote>/<name>`.
    to: String,
    /// Source as a plain branch name, `refs/heads/<name>`, or `<remote>/<name>`.
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
            let indicator = WaitIndicator::start("Approving reviewed changes", cli.verbose);
            let metadata = match current_review_metadata(cli.verbose) {
                Ok(metadata) => metadata,
                Err(ReviewIdentityReadError::Git(error)) => return Err(error.into()),
                Err(error) => {
                    indicator.finish();
                    exit_invalid_review_branch(error)
                }
            };
            let branch = current_branch_name(cli.verbose)?;
            match read_review_scope(&branch, cli.verbose) {
                Ok(_) => {}
                Err(ReviewScopeError::Git(error)) => return Err(error.into()),
                Err(error) => {
                    indicator.finish();
                    exit_invalid_review_scope(error, &metadata)
                }
            };
            let approved = approve_changes(cli.verbose)?;
            indicator.finish();
            match approved {
                false => println!("There are no reviewed changes to approve. Ending the review."),
                true => println!("Reviewed changes were approved successfully."),
            };
        }
        Commands::Review(args) => {
            let indicator = WaitIndicator::start_review(cli.verbose);
            let preparation = prepare_review_branch(
                &args.to,
                &args.from,
                args.skip_to.as_deref(),
                args.stop_at.as_deref(),
                cli.verbose,
                &|percent| indicator.update(percent),
            )?;
            indicator.complete();
            if !preparation.has_unreviewed_changes {
                println!("Review branch prepared successfully. However, it seems like there are no unreviewed changes.");
            } else {
                println!("Review branch prepared successfully. Stage the changes you have reviewed and run `{}` to approve them.", "cresca approve".green());
            }
        }
        Commands::Status => {
            let indicator = WaitIndicator::start("Checking review status", cli.verbose);
            let metadata = match current_review_metadata(cli.verbose) {
                Ok(metadata) => metadata,
                Err(ReviewIdentityReadError::Git(error)) => return Err(error.into()),
                Err(error) => {
                    indicator.finish();
                    exit_invalid_review_branch(error)
                }
            };
            let branch = current_branch_name(cli.verbose)?;
            let scope = match read_review_scope(&branch, cli.verbose) {
                Ok(scope) => scope,
                Err(ReviewScopeError::Git(error)) => return Err(error.into()),
                Err(error) => {
                    indicator.finish();
                    exit_invalid_review_scope(error, &metadata)
                }
            };
            let status = get_review_status(&scope.end_oid, "in current review range", cli.verbose)?;
            indicator.finish();
            println!("📋 Review status (current range):");
            print_saved_review_identity(&metadata);
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

fn display_review_side(side: &ReviewSide) -> String {
    match &side.canonical {
        branch_ref::CanonicalBranch::Local { reference } => reference.clone(),
        branch_ref::CanonicalBranch::Remote { remote, reference } => format!(
            "{remote}/{}",
            reference.strip_prefix("refs/heads/").unwrap_or(reference)
        ),
    }
}

fn saved_target_and_source(metadata: &StoredReviewIdentity) -> (String, String) {
    match metadata {
        StoredReviewIdentity::V1(identity) => (identity.target.clone(), identity.source.clone()),
        StoredReviewIdentity::V2(identity) => (
            display_review_side(&identity.target),
            display_review_side(&identity.source),
        ),
    }
}

fn print_saved_review_identity(metadata: &StoredReviewIdentity) {
    match metadata {
        StoredReviewIdentity::V1(identity) => {
            println!("  Unresolved legacy target: {}", identity.target);
            println!("  Unresolved legacy source: {}", identity.source);
        }
        StoredReviewIdentity::V2(identity) => {
            println!("  Target: {}", display_review_side(&identity.target));
            println!("  Source: {}", display_review_side(&identity.source));
        }
    }
}

fn exit_invalid_review_scope(error: ReviewScopeError, metadata: &StoredReviewIdentity) -> ! {
    let reason = match error {
        ReviewScopeError::Missing => "range metadata is missing".to_string(),
        ReviewScopeError::Duplicate => "range metadata has duplicate values".to_string(),
        ReviewScopeError::UnsupportedVersion(version) => {
            format!("range metadata version '{version}' is unsupported")
        }
        ReviewScopeError::Invalid => "range metadata is invalid".to_string(),
        ReviewScopeError::UnavailableCommit(oid) => {
            format!("saved review object '{oid}' is unavailable")
        }
        ReviewScopeError::Git(error) => {
            render_git_error(&error);
            exit(1);
        }
    };
    let (target, source) = saved_target_and_source(metadata);
    eprintln!(
        "{}: Cannot show current review range because {}. This review branch must be recreated. Switch away from it, delete it, then run `cresca review {} {}`.",
        "error".red().bold(),
        reason,
        target,
        source
    );
    exit(1);
}

fn exit_invalid_review_branch(error: ReviewIdentityReadError) -> ! {
    let reason = match error {
        ReviewIdentityReadError::Missing => "its metadata is missing".to_string(),
        ReviewIdentityReadError::UnsupportedVersion(version) => {
            format!("metadata version '{version}' is unsupported")
        }
        ReviewIdentityReadError::Invalid(reason) => format!("its metadata is invalid: {reason}"),
        ReviewIdentityReadError::Git(error) => {
            render_git_error(&error);
            exit(1);
        }
    };
    eprintln!(
        "{}: Current branch is not a valid cresca review branch because {reason}; run `{}` to prepare one.",
        "error".red().bold(),
        "cresca review <target> <source>".green()
    );
    exit(1);
}
