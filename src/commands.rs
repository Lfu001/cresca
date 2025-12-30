use crate::git::run_git_command;
use colored::Colorize;
use std::ops::Not;
use std::process::exit;

/// Prepare the review branch using Squash Merge approach.
///
/// # Arguments
///
/// * `to_branch` - The branch where the PR is planned to be merged into.
/// * `from_branch` - The development branch to be reviewed.
/// * `skip_to` - Optional commit hash to skip to (auto-approve earlier commits).
/// * `stop_at` - Optional commit hash to stop at (exclude later commits from review).
/// * `verbose` - Whether to print the git command and its output.
pub fn prepare_review_branch(
    to_branch: &str,
    from_branch: &str,
    skip_to: Option<&str>,
    stop_at: Option<&str>,
    verbose: bool,
) {
    let review_branch = format!("review-{}-{}", to_branch, from_branch);

    // Fetch and update both branches
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

    // Get merge-base
    let merge_base_output = run_git_command(
        "get merge base",
        &["merge-base", to_branch, from_branch],
        false,
        verbose,
    );
    let merge_base = String::from_utf8_lossy(&merge_base_output.stdout)
        .trim()
        .to_string();

    // Get valid commit range (merge_base..from_branch)
    let valid_commits = run_git_command(
        "get valid commit range",
        &["rev-list", &format!("{}..{}", merge_base, from_branch)],
        false,
        verbose,
    );
    let valid_list = String::from_utf8_lossy(&valid_commits.stdout);
    let valid_hashes: Vec<&str> = valid_list.lines().collect();

    // Validate skip_to if provided
    if let Some(hash) = skip_to {
        let is_valid = valid_hashes.iter().any(|line| line.starts_with(hash));
        if !is_valid {
            eprintln!(
                "{}: Commit {} is not in the range {}..{}",
                "error".red().bold(),
                hash,
                to_branch,
                from_branch
            );
            exit(1);
        }
    }

    // Validate stop_at if provided
    if let Some(hash) = stop_at {
        // stop_at must be in the valid range
        let is_valid = valid_hashes.iter().any(|line| line.starts_with(hash));
        if !is_valid {
            eprintln!(
                "{}: Commit {} is not in the range {}..{}",
                "error".red().bold(),
                hash,
                to_branch,
                from_branch
            );
            exit(1);
        }

        // If skip_to is also specified, stop_at must be at or after skip_to
        if let Some(skip_hash) = skip_to {
            let skip_to_commits = run_git_command(
                "get commits after skip_to",
                &["rev-list", &format!("{}..{}", skip_hash, from_branch)],
                false,
                verbose,
            );
            let skip_to_list = String::from_utf8_lossy(&skip_to_commits.stdout);
            let is_after_skip = skip_to_list.lines().any(|line| line.starts_with(hash))
                || valid_hashes
                    .iter()
                    .any(|line| line.starts_with(hash) && line.starts_with(skip_hash));

            // Check if stop_at equals skip_to (valid) or is after skip_to
            let stop_at_equals_skip_to = valid_hashes
                .iter()
                .any(|line| line.starts_with(hash) && line.starts_with(skip_hash));

            if !is_after_skip && !stop_at_equals_skip_to {
                eprintln!(
                    "{}: --stop-at ({}) must be at or after --skip-to ({})",
                    "error".red().bold(),
                    hash,
                    skip_hash
                );
                exit(1);
            }
        }
    }

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

    if review_branch_exists {
        // Switch to existing review branch
        run_git_command(
            "switch to review branch",
            &["switch", &review_branch],
            false,
            verbose,
        );
    } else {
        // Create review branch from merge-base
        run_git_command(
            "create review branch from merge-base",
            &["checkout", "-b", &review_branch, &merge_base],
            false,
            verbose,
        );
    }

    // Determine target commit for squash merge
    let target_commit = if let Some(hash) = skip_to {
        // Auto-approve commits before skip_to by squash merging them
        let parent = format!("{}^", hash);

        // Check if there are commits before skip_to
        let has_earlier = run_git_command(
            "check earlier commits",
            &["rev-list", &format!("{}..{}", merge_base, &parent)],
            true,
            verbose,
        );

        if !has_earlier.stdout.is_empty() {
            run_git_command(
                "auto-approve earlier commits",
                &[
                    "merge",
                    "--squash",
                    "--quiet",
                    "--no-stat",
                    "-X",
                    "theirs",
                    &parent,
                ],
                false,
                verbose,
            );
            run_git_command(
                "commit auto-approved changes",
                &["commit", "--quiet", "-m", "Auto-approve earlier commits"],
                false,
                verbose,
            );
        }

        // Use stop_at if specified, otherwise from_branch
        stop_at.unwrap_or(from_branch).to_string()
    } else {
        // Use stop_at if specified, otherwise from_branch
        stop_at.unwrap_or(from_branch).to_string()
    };

    // Squash merge remaining changes
    run_git_command(
        "squash merge remaining changes",
        &[
            "merge",
            "--squash",
            "--quiet",
            "--no-stat",
            "-X",
            "theirs",
            &target_commit,
        ],
        false,
        verbose,
    );

    // Unstage changes for review
    run_git_command("unstage changes for review", &["reset"], false, verbose);
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
pub fn approve_changes(verbose: bool) -> Result<(), ()> {
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

/// Review status information
pub struct ReviewStatus {
    pub from_branch: String,
    pub file_count: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub files: Vec<String>,
}

/// Get review status (remaining diff stats)
///
/// # Arguments
///
/// * `from_branch` - The development branch to compare against.
/// * `verbose` - Whether to print the git command and its output.
///
/// # Returns
///
/// * `ReviewStatus` - The remaining diff statistics
pub fn get_review_status(from_branch: &str, verbose: bool) -> ReviewStatus {
    // Get diff stats summary (use HEAD..branch for direct comparison, not HEAD...branch)
    let stat_output = run_git_command(
        "get diff stats",
        &["diff", "--stat", "HEAD", from_branch],
        false,
        verbose,
    );
    let stat_str = String::from_utf8_lossy(&stat_output.stdout);

    // Parse stats from last line (e.g., " 4 files changed, 7 insertions(+), 2 deletions(-)")
    let mut file_count = 0;
    let mut insertions = 0;
    let mut deletions = 0;

    if let Some(last_line) = stat_str.lines().last() {
        for part in last_line.split(',') {
            let part = part.trim();
            if part.contains("file") {
                if let Some(num) = part.split_whitespace().next() {
                    file_count = num.parse().unwrap_or(0);
                }
            } else if part.contains("insertion") {
                if let Some(num) = part.split_whitespace().next() {
                    insertions = num.parse().unwrap_or(0);
                }
            } else if part.contains("deletion") {
                if let Some(num) = part.split_whitespace().next() {
                    deletions = num.parse().unwrap_or(0);
                }
            }
        }
    }

    // Get list of changed files
    let files_output = run_git_command(
        "get changed files",
        &["diff", "--name-only", "HEAD", from_branch],
        false,
        verbose,
    );
    let files: Vec<String> = String::from_utf8_lossy(&files_output.stdout)
        .lines()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    ReviewStatus {
        from_branch: from_branch.to_string(),
        file_count,
        insertions,
        deletions,
        files,
    }
}
