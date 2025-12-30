use colored::Colorize;
use std::process::{exit, Command, Output};

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
pub fn run_git_command(
    description: &str,
    args: &[&str],
    maybe_error: bool,
    verbose: bool,
) -> Output {
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
pub fn is_clean(verbose: bool) -> bool {
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
pub fn is_review_branch(verbose: bool) -> bool {
    let output = run_git_command(
        "get current branch",
        &["rev-parse", "--abbrev-ref", "HEAD"],
        false,
        verbose,
    );
    let branch_name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    branch_name.starts_with("review")
}

/// Get review branch info (to_branch, from_branch) from current branch name
///
/// # Arguments
///
/// * `verbose` - Whether to print the git command and its output.
///
/// # Returns
///
/// * `Option<(String, String)>` - (to_branch, from_branch) if on a review branch, None otherwise
pub fn get_review_branch_info(verbose: bool) -> Option<(String, String)> {
    let output = run_git_command(
        "get current branch",
        &["rev-parse", "--abbrev-ref", "HEAD"],
        false,
        verbose,
    );
    let branch_name = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if !branch_name.starts_with("review-") {
        return None;
    }

    // Parse "review-{to}-{from}" format
    let rest = branch_name.strip_prefix("review-")?;
    let parts: Vec<&str> = rest.splitn(2, '-').collect();
    if parts.len() == 2 {
        Some((parts[0].to_string(), parts[1].to_string()))
    } else {
        None
    }
}
