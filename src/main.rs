use clap::builder::styling::{AnsiColor, Effects};
use clap::{builder::Styles, ArgAction, Args, Parser, Subcommand};
use colored::Colorize;
use std::ops::Not;
use std::process::{exit, Command, Output};

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
}

#[derive(Args)]
struct ReviewArgs {
    /// The branch where the PR is planned to be merged into.
    to: String,
    /// The development branch to be reviewed.
    from: String,
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
            if !is_clean() {
                eprintln!("{}: Uncommitted changes found. Please commit or stash them before starting review.", "error".red().bold());
                exit(1);
            }

            prepare_review_branch(&args.to, &args.from, cli.verbose);
            if is_clean(cli.verbose) {
                println!("Review branch prepared successfully. However, it seems like there are no unreviewed changes.");
            } else {
                println!("Review branch prepared successfully. Stage the changes you have reviewed and run `{}` to approve them.", "cresca approve".green());
            }
        }
    }
}

/// Run a git command and return the output
///
/// # Arguments
///
/// * `description` - The description of the git command.
/// * `args` - The arguments to pass to the git command.
/// * `maybe_error` - Whether the git command might fail intentionally.
/// * `verbose` - Whether to print the git command and its output.
///
/// # Returns
///
/// * `std::process::Output` - The output of the git command.
fn run_git_command(description: &str, args: &[&str], maybe_error: bool, verbose: bool) -> Output {
    if verbose {
        println!("[git {}]", args.join(" ").yellow());
    }
    let output = Command::new("git").args(args).output();
    match output {
        Ok(output) => {
            if output.status.success() && !output.stdout.is_empty() && verbose {
                println!("{}", String::from_utf8_lossy(&output.stdout));
            }
            if !output.status.success() && !maybe_error {
                eprintln!("{}: Failed to {}.", "error".red().bold(), description);
                eprintln!("Original error from git:");
                eprintln!("\t{}", String::from_utf8_lossy(&output.stderr));
                exit(1);
            }
            output
        }
        Err(e) => {
            eprintln!("{}: Failed to {}.", "error".red().bold(), description);
            eprintln!("{}", e);
            exit(1);
        }
    }
}

/// Check if the working directory is clean
///
/// # Arguments
///
/// * `verbose` - Whether to print the git command and its output.
fn is_clean(verbose: bool) -> bool {
    run_git_command(
        "check working directory status",
        &["status", "--porcelain"],
        false,
        verbose,
    )
    .stdout
    .is_empty()
}

/// Check if the current branch is a review branch
///
/// # Arguments
///
/// * `verbose` - Whether to print the git command and its output.
fn is_review_branch(verbose: bool) -> bool {
    let output = run_git_command(
        "get current branch",
        &["rev-parse", "--abbrev-ref", "HEAD"],
        false,
        verbose,
    );
    let branch_name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    branch_name.starts_with("review")
}

/// Prepare the review branch by merging the development branch without committing.
///
/// # Arguments
///
/// * `to_branch` - The branch where the PR is planned to be merged into.
/// * `from_branch` - The development branch to be reviewed.
/// * `verbose` - Whether to print the git command and its output.
fn prepare_review_branch(to_branch: &str, from_branch: &str, verbose: bool) {
    let review_branch = format!("review-{}-{}", to_branch, from_branch);

    // Pull both branches
    run_git_command(
        &format!("switch to {} branch", from_branch),
        &["switch", from_branch],
        false,
        verbose,
    );
    run_git_command(
        &format!("pull {} branch", from_branch),
        &["pull", "origin", from_branch],
        false,
        verbose,
    );
    run_git_command(
        &format!("switch to {} branch", to_branch),
        &["switch", to_branch],
        false,
        verbose,
    );
    run_git_command(
        &format!("pull {} branch", to_branch),
        &["pull", "origin", to_branch],
        false,
        verbose,
    );

    // Check if review branch exists
    let review_branch_exists = run_git_command(
        "check existence of review branch",
        &[
            "show-ref",
            "--verify",
            &format!("refs/heads/{}", review_branch),
        ],
        true,
        verbose,
    )
    .status
    .success();

    // Create or switch to review branch
    if review_branch_exists {
        run_git_command(
            "switch to review branch",
            &["switch", &review_branch],
            false,
            verbose,
        );
    } else {
        run_git_command(
            "create review branch",
            &["checkout", "-b", &review_branch],
            false,
            verbose,
        );
    }

    // Merge unreviewed changes
    run_git_command(
        "merge unreviewed changes",
        &[
            "merge",
            "--quiet",
            "--no-stat",
            "--no-commit",
            "--no-ff",
            "-X",
            "theirs",
            from_branch,
        ],
        false,
        verbose,
    );

    // Unstage changes
    run_git_command("unstage changes", &["reset"], false, verbose);
}

/// Commit reviewed changes and discard unreviewed ones
///
/// # Arguments
///
/// * `verbose` - Whether to print the git command and its output.
///
/// # Returns
///
/// * `Ok(())` - If there are staged changes
/// * `Err(())` - If there are no staged changes
fn approve_changes(verbose: bool) -> Result<(), ()> {
    // Check if there are staged changes
    let has_staged_changes = run_git_command(
        "check staged changes",
        &["diff", "--cached"],
        false,
        verbose,
    )
    .stdout
    .is_empty()
    .not();

    if has_staged_changes {
        run_git_command(
            "commit reviewed changes",
            &["commit", "--quiet", "-m", "Approve reviewed changes"],
            false,
            verbose,
        );
    }

    run_git_command(
        "discard unreviewed changes",
        &["restore", "--source=HEAD", "--worktree", "--", "."],
        false,
        verbose,
    );
    run_git_command("discard untracked files", &["clean", "-fd"], false, verbose);

    match has_staged_changes {
        true => Ok(()),
        false => Err(()),
    }
}
