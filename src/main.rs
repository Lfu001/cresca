mod commands;
mod git;

use clap::builder::styling::{AnsiColor, Effects};
use clap::{builder::Styles, ArgAction, Args, Parser, Subcommand};
use colored::Colorize;
use commands::{approve_changes, get_review_status, prepare_review_branch};
use git::{get_review_branch_info, is_clean, is_review_branch};
use std::process::exit;

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
    /// The branch where the PR is planned to be merged into.
    to: String,
    /// The development branch to be reviewed.
    from: String,
    /// Skip to this commit (auto-approve earlier commits).
    /// Use `git log --oneline <to>..<from>` to see available commits.
    #[arg(long = "skip-to")]
    skip_to: Option<String>,
}

fn main() {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Approve => {
            if is_review_branch(cli.verbose) {
                let res = approve_changes(cli.verbose);
                match res {
                    Err(_) => {
                        println!("There are no reviewed changes to approve. Ending the review.",)
                    }
                    Ok(_) => println!("Reviewed changes were approved successfully.",),
                };
            } else {
                eprintln!(
                    "{}: Not on a review branch; run `{}` to prepare a review branch.",
                    "error".red().bold(),
                    "cresca review".green()
                );
                exit(1);
            }
        }
        Commands::Review(args) => {
            if !is_clean(cli.verbose) {
                eprintln!("{}: Uncommitted changes found. Please commit or stash them before starting review.", "error".red().bold());
                exit(1);
            }

            prepare_review_branch(&args.to, &args.from, args.skip_to.as_deref(), cli.verbose);
            if is_clean(cli.verbose) {
                println!("Review branch prepared successfully. However, it seems like there are no unreviewed changes.");
            } else {
                println!("Review branch prepared successfully. Stage the changes you have reviewed and run `{}` to approve them.", "cresca approve".green());
            }
        }
        Commands::Status => {
            if let Some((_, from_branch)) = get_review_branch_info(cli.verbose) {
                let status = get_review_status(&from_branch, cli.verbose);
                println!("📋 Review status:");
                println!(
                    "  Remaining diff to {}: {} file(s), {} insertion(s), {} deletion(s)",
                    status.from_branch.green(),
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
            } else {
                eprintln!(
                    "{}: Not on a review branch; run `{}` to prepare a review branch.",
                    "error".red().bold(),
                    "cresca review".green()
                );
                exit(1);
            }
        }
    }
}
