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

/// Resolved remote tracking branch information.
pub struct ResolvedBranch {
    /// The remote name (e.g. "origin", "upstream")
    pub remote: String,
    /// The local branch name on the remote (e.g. "develop", "feature/login")
    pub remote_branch: String,
    /// The full tracking ref (e.g. "origin/develop")
    pub tracking_ref: String,
}

/// Resolves a branch name to its remote tracking branch information.
///
/// It checks:
/// 1. If it's already a valid remote tracking branch (e.g., origin/main).
/// 2. If it's a local branch with an upstream configured (e.g., @{upstream}).
/// 3. Fallback: assumes it's on 'origin' if it exists there.
pub fn resolve_remote_tracking_branch(branch_or_ref: &str, verbose: bool) -> ResolvedBranch {
    // 1. Check if it's already a valid remote-tracking branch
    let verify_output = run_git_command(
        "verify if branch is already a remote tracking branch",
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/remotes/{}", branch_or_ref),
        ],
        true,
        verbose,
    );

    if verify_output.status.success() {
        // It's a remote tracking branch. We need to split it into remote and branch.
        // Assuming format is <remote>/<branch_name>. We can get remotes to find the remote name.
        let remotes_output = run_git_command("get remotes", &["remote"], false, verbose);
        let remotes_str = String::from_utf8_lossy(&remotes_output.stdout);
        let mut best_remote = String::new();
        for remote in remotes_str.lines() {
            let remote = remote.trim();
            if branch_or_ref.starts_with(&format!("{}/", remote))
                && remote.len() > best_remote.len()
            {
                best_remote = remote.to_string();
            }
        }

        if !best_remote.is_empty() {
            let remote_branch = branch_or_ref
                .strip_prefix(&format!("{}/", best_remote))
                .unwrap()
                .to_string();
            return ResolvedBranch {
                remote: best_remote,
                remote_branch,
                tracking_ref: branch_or_ref.to_string(),
            };
        }
    }

    // 2. Check if it's a local branch with an upstream configured
    let upstream_output = run_git_command(
        "get upstream branch",
        &[
            "rev-parse",
            "--abbrev-ref",
            &format!("{}@{{upstream}}", branch_or_ref),
        ],
        true,
        verbose,
    );

    if upstream_output.status.success() {
        let tracking_ref = String::from_utf8_lossy(&upstream_output.stdout)
            .trim()
            .to_string();

        // Extract remote and remote_branch from tracking_ref using the configured remote for the branch
        let remote_output = run_git_command(
            "get configured remote",
            &["config", &format!("branch.{}.remote", branch_or_ref)],
            true,
            verbose,
        );

        let remote = if remote_output.status.success() {
            String::from_utf8_lossy(&remote_output.stdout)
                .trim()
                .to_string()
        } else {
            "origin".to_string() // fallback if something is weird
        };

        let remote_branch = if tracking_ref.starts_with(&format!("{}/", remote)) {
            tracking_ref
                .strip_prefix(&format!("{}/", remote))
                .unwrap()
                .to_string()
        } else {
            branch_or_ref.to_string() // fallback
        };

        return ResolvedBranch {
            remote,
            remote_branch,
            tracking_ref,
        };
    }

    // 3. Fallback: check if the branch exists on any remote (default to 'origin' if it's there)
    // First, let's just see if it exists on origin.
    let ls_remote_output = run_git_command(
        "check if branch exists on origin",
        &[
            "ls-remote",
            "--exit-code",
            "origin",
            &format!("refs/heads/{}", branch_or_ref),
        ],
        true,
        verbose,
    );

    if ls_remote_output.status.success() {
        return ResolvedBranch {
            remote: "origin".to_string(),
            remote_branch: branch_or_ref.to_string(),
            tracking_ref: format!("origin/{}", branch_or_ref),
        };
    }

    // If we reach here, we can't reliably resolve it. Default to origin/branch and let git fail natively later
    // if it's really invalid.
    ResolvedBranch {
        remote: "origin".to_string(),
        remote_branch: branch_or_ref.to_string(),
        tracking_ref: format!("origin/{}", branch_or_ref),
    }
}
