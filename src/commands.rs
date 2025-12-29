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
/// * `verbose` - Whether to print the git command and its output.
pub fn prepare_review_branch(
    to_branch: &str,
    from_branch: &str,
    skip_to: Option<&str>,
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

    // Validate skip_to if provided
    if let Some(hash) = skip_to {
        let valid_commits = run_git_command(
            "get valid commit range",
            &["rev-list", &format!("{}..{}", merge_base, from_branch)],
            false,
            verbose,
        );
        let valid_list = String::from_utf8_lossy(&valid_commits.stdout);
        let is_valid = valid_list.lines().any(|line| line.starts_with(hash));
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

        from_branch.to_string()
    } else {
        from_branch.to_string()
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
