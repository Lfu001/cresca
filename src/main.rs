use clap::builder::styling::{AnsiColor, Effects};
use clap::{builder::Styles, Args, Parser, Subcommand};
use colored::Colorize;
use std::ops::Not;
use std::process::{exit, Command};

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
            if is_review_branch() {
                let res = approve_changes();
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

            prepare_review_branch(&args.to, &args.from);
            if is_clean() {
                println!("Review branch prepared successfully. However, it seems like there are no unreviewed changes.");
            } else {
                println!("Review branch prepared successfully. Stage the changes you have reviewed and run `{}` to approve them.", "cresca approve".green());
            }
        }
    }
}

/// Check if the working directory is clean
fn is_clean() -> bool {
    Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .expect("Failed to check working directory status")
        .stdout
        .is_empty()
}

/// Check if the current branch is a review branch
fn is_review_branch() -> bool {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .stdout(std::process::Stdio::piped())
        .output()
        .expect("Failed to get current branch");
    let branch_name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    branch_name.starts_with("review")
}

/// Prepare the review branch by merging the development branch without committing.
///
/// # Arguments
///
/// * `to_branch` - The branch where the PR is planned to be merged into.
/// * `from_branch` - The development branch to be reviewed.
///
/// # Panics
///
/// Panics if the git command fails.
fn prepare_review_branch(to_branch: &str, from_branch: &str) {
    let review_branch = format!("review-{}-{}", to_branch, from_branch);

    // Pull both branches
    let _ = Command::new("git")
        .args(["switch", "--quiet", from_branch])
        .status()
        .expect("Failed to switch to development branch");
    let _ = Command::new("git")
        .args(["pull", "--quiet", "origin", from_branch])
        .status()
        .expect("Failed to pull development branch");
    let _ = Command::new("git")
        .args(["switch", "--quiet", to_branch])
        .status()
        .expect("Failed to switch to main branch");
    let _ = Command::new("git")
        .args(["pull", "--quiet", "origin", to_branch])
        .status()
        .expect("Failed to pull main branch");

    // Check if review branch exists
    let review_branch_exists = Command::new("git")
        .args(["rev-parse", "--verify", &review_branch])
        .stdout(std::process::Stdio::null())
        .status()
        .expect("Failed to verify review branch")
        .success();

    // Create or switch to review branch
    if review_branch_exists {
        Command::new("git")
            .args(["switch", "--quiet", &review_branch])
            .status()
            .expect("Failed to switch to review branch");
    } else {
        Command::new("git")
            .args(["checkout", "--quiet", "-b", &review_branch])
            .stdout(std::process::Stdio::null())
            .status()
            .expect("Failed to create review branch");
    }

    // Merge unreviewed changes
    Command::new("git")
        .args([
            "merge",
            "--quiet",
            "--no-stat",
            "--no-commit",
            "--no-ff",
            "-X",
            "theirs",
            from_branch,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("Failed to collect unreviewed changes");

    // Unstage changes
    Command::new("git")
        .args(["reset", "--quiet"])
        .status()
        .expect("Failed to unstage changes");
}

/// Commit reviewed changes and discard unreviewed ones
///
/// # Panics
///
/// Panics if the git command fails.
///
/// # Returns
///
/// * `Ok(())` - If there are staged changes
/// * `Err(())` - If there are no staged changes
fn approve_changes() -> Result<(), ()> {
    // Check if there are staged changes
    let has_staged_changes = Command::new("git")
        .args(["diff", "--cached"])
        .output()
        .expect("Failed to check staged changes")
        .stdout
        .is_empty()
        .not();

    if has_staged_changes {
        Command::new("git")
            .args(["commit", "--quiet", "-m", "Approve reviewed changes"])
            .status()
            .expect("Failed to commit reviewed changes");
    }

    Command::new("git")
        .args([
            "restore",
            "--quiet",
            "--source=HEAD",
            "--worktree",
            "--",
            ".",
        ])
        .status()
        .expect("Failed to discard unreviewed changes");
    Command::new("git")
        .args(["clean", "-fd", "--quiet"])
        .status()
        .expect("Failed to discard untracked files");

    match has_staged_changes {
        true => Ok(()),
        false => Err(()),
    }
}
