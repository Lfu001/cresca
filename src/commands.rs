use crate::git::{
    resolve_remote_tracking_branch, run_git_command, select_review_branch, write_review_metadata,
    write_review_scope, ReviewBranchSelection, ReviewMetadata, ReviewScope,
};
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
    let metadata = ReviewMetadata {
        target: to_branch.to_string(),
        source: from_branch.to_string(),
    };
    let resolved_to = resolve_remote_tracking_branch(to_branch, verbose);
    let resolved_from = resolve_remote_tracking_branch(from_branch, verbose);

    let tracking_to = resolved_to.tracking_ref;
    let tracking_from = resolved_from.tracking_ref;

    // Fetch the target branch
    run_git_command(
        &format!("fetch target branch from {}", resolved_to.remote),
        &["fetch", &resolved_to.remote, &resolved_to.remote_branch],
        false,
        verbose,
    );

    // Fetch the source branch
    run_git_command(
        &format!("fetch source branch from {}", resolved_from.remote),
        &["fetch", &resolved_from.remote, &resolved_from.remote_branch],
        false,
        verbose,
    );

    // Get merge-base
    let merge_base_output = run_git_command(
        "get merge base",
        &["merge-base", &tracking_to, &tracking_from],
        false,
        verbose,
    );
    let merge_base = String::from_utf8_lossy(&merge_base_output.stdout)
        .trim()
        .to_string();

    // Get valid commit range (merge_base..tracking_from)
    let valid_commits = run_git_command(
        "get valid commit range",
        &["rev-list", &format!("{}..{}", merge_base, tracking_from)],
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
                &["rev-list", &format!("{}..{}", skip_hash, tracking_from)],
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

    let scope_end_revision = stop_at.unwrap_or(&tracking_from);
    let scope_end_commit = format!("{scope_end_revision}^{{commit}}");
    let scope_end_output = run_git_command(
        "resolve review range endpoint",
        &["rev-parse", "--verify", &scope_end_commit],
        false,
        verbose,
    );
    let scope = ReviewScope {
        end_oid: String::from_utf8_lossy(&scope_end_output.stdout)
            .trim()
            .to_string(),
    };

    let (review_branch, is_new) = match select_review_branch(&metadata, verbose) {
        ReviewBranchSelection::Existing(name) => {
            run_git_command(
                "switch to review branch",
                &["switch", &name],
                false,
                verbose,
            );
            (name, false)
        }
        ReviewBranchSelection::New(name) => {
            run_git_command(
                "create review branch from merge-base",
                &["checkout", "-b", &name, &merge_base],
                false,
                verbose,
            );
            (name, true)
        }
    };

    if let Some(hash) = skip_to {
        // Auto-approve commits before skip_to by squash merging them
        let parent = format!("{}^", hash);

        // Check if there are commits before skip_to
        let has_earlier = run_git_command(
            "check earlier commits",
            &["rev-list", &format!("{}..{}", merge_base, parent)],
            true,
            verbose,
        );

        if !has_earlier.stdout.is_empty() {
            run_git_command(
                "auto-approve earlier commits",
                &[
                    "merge",
                    "--squash",
                    "--ff",
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
    }

    let target_commit = scope.end_oid.clone();

    // Squash merge remaining changes
    run_git_command(
        "squash merge remaining changes",
        &[
            "merge",
            "--squash",
            "--ff",
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
    if is_new {
        write_review_metadata(&review_branch, &metadata, verbose);
    }
    write_review_scope(&review_branch, &scope, verbose);
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
    let resolved_from = resolve_remote_tracking_branch(from_branch, verbose);
    let tracking_from = resolved_from.tracking_ref;

    // Get diff stats summary (use HEAD..branch for direct comparison, not HEAD...branch)
    let stat_output = run_git_command(
        "get diff stats",
        &["diff", "--stat", "HEAD", &tracking_from],
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
        &["diff", "--name-only", "HEAD", &tracking_from],
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
