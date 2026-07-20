use crate::git::{
    is_clean, read_review_scope, resolve_remote_tracking_branch, run_git_command,
    select_review_branch, write_review_metadata, write_review_scope, ReviewBranchSelection,
    ReviewBranchSelectionError, ReviewMetadata, ReviewScope, ReviewScopeError,
};
use crate::review::{
    reconstruct_approval_tree, reconstruct_approval_tree_with_env, unique_merge_base, ReviewError,
    ReviewPreparation, ReviewTransaction,
};
use std::ffi::OsStr;
use std::ops::Not;
use std::{fs, path::PathBuf};

struct ReviewPlan {
    metadata: ReviewMetadata,
    new_base: String,
    auto_approve_parent: Option<String>,
    selection: ReviewBranchSelection,
    old_review: Option<String>,
    old_base: Option<String>,
    scope: ReviewScope,
    tracking_updates: Vec<(String, String)>,
}

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
) -> Result<ReviewPreparation, ReviewError> {
    let root = ReviewTransaction::repository_root(verbose)?;
    std::env::set_current_dir(&root).map_err(|error| {
        ReviewError::Message(format!(
            "failed to anchor review preparation at repository root: {error}"
        ))
    })?;

    if !is_clean(verbose)? {
        return Err(ReviewError::Message(
            "Uncommitted changes found. Please commit or stash them before starting review."
                .to_string(),
        ));
    }

    let plan = prepare_review_plan(to_branch, from_branch, skip_to, stop_at, verbose)?;
    let mut transaction = ReviewTransaction::begin(root, verbose)?;
    transaction.execute(|| apply_review_plan(plan, verbose))
}

fn prepare_review_plan(
    to_branch: &str,
    from_branch: &str,
    skip_to: Option<&str>,
    stop_at: Option<&str>,
    verbose: bool,
) -> Result<ReviewPlan, ReviewError> {
    let metadata = ReviewMetadata {
        target: to_branch.to_string(),
        source: from_branch.to_string(),
    };
    let resolved_to = resolve_remote_tracking_branch(to_branch, verbose)?;
    let resolved_from = resolve_remote_tracking_branch(from_branch, verbose)?;

    let tracking_from = fetch_remote_commit(
        "source",
        &resolved_from.remote,
        &resolved_from.remote_branch,
        verbose,
    )?;
    let scope_end_revision = stop_at.unwrap_or(&tracking_from);
    let scope_end_commit = format!("{scope_end_revision}^{{commit}}");
    let scope_end_output = run_git_command(
        "resolve review range endpoint",
        &["rev-parse", "--verify", &scope_end_commit],
        &[128],
        verbose,
    )?;
    if !scope_end_output.status.success() {
        return Err(ReviewError::Message(format!(
            "Commit {} is not in the range {}..{}",
            scope_end_revision, to_branch, from_branch
        )));
    }
    let endpoint = String::from_utf8_lossy(&scope_end_output.stdout)
        .trim()
        .to_string();
    let tracking_to = fetch_remote_commit(
        "target",
        &resolved_to.remote,
        &resolved_to.remote_branch,
        verbose,
    )?;
    let new_base = unique_merge_base(&tracking_to, &endpoint, verbose)?;

    let valid_commits = run_git_command(
        "get valid commit range",
        &["rev-list", &format!("{}..{}", new_base, tracking_from)],
        &[],
        verbose,
    )?;
    let valid_list = String::from_utf8_lossy(&valid_commits.stdout);
    let valid_hashes: Vec<&str> = valid_list.lines().collect();

    // Validate skip_to if provided
    if let Some(hash) = skip_to {
        let is_valid = valid_hashes.iter().any(|line| line.starts_with(hash));
        if !is_valid {
            return Err(ReviewError::Message(format!(
                "Commit {} is not in the range {}..{}",
                hash, to_branch, from_branch
            )));
        }
    }

    // Validate stop_at if provided
    if let Some(hash) = stop_at {
        // stop_at must be in the valid range
        let is_valid = valid_hashes.iter().any(|line| line.starts_with(hash));
        if !is_valid {
            return Err(ReviewError::Message(format!(
                "Commit {} is not in the range {}..{}",
                hash, to_branch, from_branch
            )));
        }

        // If skip_to is also specified, stop_at must be at or after skip_to
        if let Some(skip_hash) = skip_to {
            let skip_to_commits = run_git_command(
                "get commits after skip_to",
                &["rev-list", &format!("{}..{}", skip_hash, tracking_from)],
                &[],
                verbose,
            )?;
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
                return Err(ReviewError::Message(format!(
                    "--stop-at ({}) must be at or after --skip-to ({})",
                    hash, skip_hash
                )));
            }
        }
    }

    let scope = ReviewScope {
        base_oid: Some(new_base.clone()),
        end_oid: endpoint,
    };

    let auto_approve_parent = if let Some(hash) = skip_to {
        let parent = format!("{}^", hash);
        let has_earlier = run_git_command(
            "check earlier commits",
            &["rev-list", &format!("{}..{}", new_base, parent)],
            &[],
            verbose,
        )?;
        (!has_earlier.stdout.is_empty()).then_some(parent)
    } else {
        None
    };

    let selection = select_review_branch(&metadata, verbose).map_err(|error| match error {
        ReviewBranchSelectionError::Git(error) => ReviewError::Git(error),
        ReviewBranchSelectionError::Conflict(message) => ReviewError::Message(message),
    })?;
    let (old_review, old_base) = match &selection {
        ReviewBranchSelection::New(_) => (None, None),
        ReviewBranchSelection::Existing(branch) => {
            let old_review = String::from_utf8_lossy(
                &run_git_command(
                    "resolve previous review head",
                    &[
                        "rev-parse",
                        "--verify",
                        &format!("refs/heads/{branch}^{{commit}}"),
                    ],
                    &[],
                    verbose,
                )?
                .stdout,
            )
            .trim()
            .to_string();
            let old_base = match read_review_scope(branch, verbose) {
                Ok(saved) => match saved.base_oid {
                    Some(base) => base,
                    None => unique_merge_base(&old_review, &saved.end_oid, verbose)?,
                },
                Err(ReviewScopeError::Missing) => {
                    unique_merge_base(&old_review, &tracking_from, verbose)?
                }
                Err(ReviewScopeError::Git(error)) => return Err(error.into()),
                Err(error) => {
                    return Err(ReviewError::Message(format!(
                        "Cannot safely migrate existing review range metadata: {error:?}"
                    )))
                }
            };
            (Some(old_review), Some(old_base))
        }
    };
    let tracking_updates = vec![
        (
            format!("refs/remotes/{}", resolved_to.tracking_ref),
            tracking_to,
        ),
        (
            format!("refs/remotes/{}", resolved_from.tracking_ref),
            tracking_from,
        ),
    ];

    Ok(ReviewPlan {
        metadata,
        new_base,
        auto_approve_parent,
        selection,
        old_review,
        old_base,
        scope,
        tracking_updates,
    })
}

fn fetch_remote_commit(
    role: &str,
    remote: &str,
    remote_branch: &str,
    verbose: bool,
) -> Result<String, ReviewError> {
    let remote_ref = format!("refs/heads/{remote_branch}");
    let output = run_git_command(
        &format!("resolve {role} branch on {remote}"),
        &["ls-remote", "--exit-code", remote, &remote_ref],
        &[],
        verbose,
    )?;
    let oid = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().next())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ReviewError::Message(format!(
                "Remote branch `{remote}/{remote_branch}` did not resolve to a commit."
            ))
        })?
        .to_string();
    run_git_command(
        &format!("fetch {role} branch from {remote}"),
        &[
            "fetch",
            "--no-write-fetch-head",
            "--no-tags",
            "--refmap=",
            remote,
            &remote_ref,
        ],
        &[],
        verbose,
    )?;
    let commit = format!("{oid}^{{commit}}");
    let output = run_git_command(
        &format!("validate fetched {role} commit"),
        &["rev-parse", "--verify", &commit],
        &[],
        verbose,
    )?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn merge_auto_approved_tree(
    base: &str,
    approved_tree: &str,
    auto_approve_parent: &str,
    verbose: bool,
) -> Result<String, ReviewError> {
    let output = run_git_command(
        "compose explicitly auto-approved tree",
        &[
            "merge-tree",
            "--write-tree",
            &format!("--merge-base={base}"),
            "-Xtheirs",
            "-Xfind-renames=100%",
            "--no-messages",
            approved_tree,
            auto_approve_parent,
        ],
        &[],
        verbose,
    )?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn commit_tree(
    description: &str,
    tree: &str,
    parent: &str,
    message: &str,
    verbose: bool,
) -> Result<String, ReviewError> {
    let output = run_git_command(
        description,
        &["commit-tree", tree, "-p", parent, "-m", message],
        &[],
        verbose,
    )?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn apply_review_plan(plan: ReviewPlan, verbose: bool) -> Result<ReviewPreparation, ReviewError> {
    let ReviewPlan {
        metadata,
        new_base,
        auto_approve_parent,
        selection,
        old_review,
        old_base,
        scope,
        tracking_updates,
    } = plan;
    for (number, (tracking_ref, oid)) in tracking_updates.iter().enumerate() {
        let role = if number == 0 { "target" } else { "source" };
        run_git_command(
            &format!("publish fetched {role} tracking ref"),
            &["update-ref", tracking_ref, oid],
            &[],
            verbose,
        )?;
    }
    let (review_branch, is_new) = match selection {
        ReviewBranchSelection::Existing(name) => {
            run_git_command("switch to review branch", &["switch", &name], &[], verbose)?;
            (name, false)
        }
        ReviewBranchSelection::New(name) => {
            run_git_command(
                "create review branch from merge-base",
                &["checkout", "-b", &name, &new_base],
                &[],
                verbose,
            )?;
            (name, true)
        }
    };

    let (mut approved_tree, parent) = if is_new {
        (format!("{new_base}^{{tree}}"), new_base.clone())
    } else {
        let old_review = old_review
            .as_deref()
            .expect("existing review must have a head");
        let old_base = old_base
            .as_deref()
            .expect("existing review must have a base");
        (
            reconstruct_approval_tree(old_base, &new_base, old_review, verbose)?.tree_oid,
            old_review.to_string(),
        )
    };
    if let Some(auto_parent) = auto_approve_parent.as_deref() {
        approved_tree = merge_auto_approved_tree(&new_base, &approved_tree, auto_parent, verbose)?;
    }

    let new_review = if !is_new || auto_approve_parent.is_some() {
        let message = if is_new {
            "Auto-approve earlier commits"
        } else {
            "Reconstruct approved changes"
        };
        let description = if is_new {
            "commit auto-approved changes"
        } else {
            "commit reconstructed approved tree"
        };
        let commit = commit_tree(description, &approved_tree, &parent, message, verbose)?;
        run_git_command(
            "update review ref to reconstructed approval",
            &[
                "update-ref",
                &format!("refs/heads/{review_branch}"),
                &commit,
                &parent,
            ],
            &[],
            verbose,
        )?;
        commit
    } else {
        new_base.clone()
    };

    run_git_command(
        "materialize review endpoint tree",
        &["read-tree", "--reset", "-u", &scope.end_oid],
        &[],
        verbose,
    )?;
    run_git_command(
        "unstage changes for review",
        &["reset", "--mixed", &new_review],
        &[],
        verbose,
    )?;
    if is_new {
        write_review_metadata(&review_branch, &metadata, verbose)?;
    }
    write_review_scope(&review_branch, &scope, verbose)?;
    Ok(ReviewPreparation {
        has_unreviewed_changes: !is_clean(verbose)?,
    })
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
pub fn approve_changes(verbose: bool) -> Result<bool, crate::git::GitCommandError> {
    // Check if there are staged changes
    let has_staged_changes =
        run_git_command("check staged changes", &["diff", "--cached"], &[], verbose)?
            .stdout
            .is_empty()
            .not();

    if has_staged_changes {
        run_git_command(
            "commit reviewed changes",
            &["commit", "--quiet", "-m", "Approve reviewed changes"],
            &[],
            verbose,
        )?;
    }

    run_git_command(
        "discard unreviewed changes",
        &["restore", "--source=HEAD", "--worktree", "--", "."],
        &[],
        verbose,
    )?;
    run_git_command("discard untracked files", &["clean", "-fd"], &[], verbose)?;

    Ok(has_staged_changes)
}

/// Review status information
pub struct ReviewStatus {
    pub display_label: String,
    pub file_count: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub files: Vec<String>,
}

pub fn get_full_review_status(
    metadata: &ReviewMetadata,
    verbose: bool,
) -> Result<ReviewStatus, ReviewError> {
    let target = resolve_remote_tracking_branch(&metadata.target, verbose)?;
    let source = resolve_remote_tracking_branch(&metadata.source, verbose)?;
    let resolve = |description: &str, revision: &str| -> Result<String, ReviewError> {
        let output = run_git_command(
            description,
            &["rev-parse", "--verify", &format!("{revision}^{{commit}}")],
            &[],
            verbose,
        )?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    };
    let target_oid = resolve("resolve current review target", &target.tracking_ref)?;
    let source_oid = resolve("resolve current review source", &source.tracking_ref)?;
    let review_head = resolve("resolve current review head", "HEAD")?;
    let old_base = unique_merge_base(&review_head, &source_oid, verbose)?;
    let new_base = unique_merge_base(&target_oid, &source_oid, verbose)?;

    let object_path = run_git_command(
        "locate Git object database",
        &["rev-parse", "--git-path", "objects"],
        &[],
        verbose,
    )?;
    let object_path = PathBuf::from(String::from_utf8_lossy(&object_path.stdout).trim());
    let object_path = if object_path.is_absolute() {
        object_path
    } else {
        ReviewTransaction::repository_root(verbose)?.join(object_path)
    };
    let scratch = tempfile::TempDir::new().map_err(|error| {
        ReviewError::Message(format!(
            "failed to create temporary status reconstruction area: {error}"
        ))
    })?;
    let scratch_objects = scratch.path().join("objects");
    fs::create_dir(&scratch_objects).map_err(|error| {
        ReviewError::Message(format!(
            "failed to initialize temporary status object database: {error}"
        ))
    })?;
    let env = [
        ("GIT_OBJECT_DIRECTORY", scratch_objects.as_os_str()),
        ("GIT_ALTERNATE_OBJECT_DIRECTORIES", object_path.as_os_str()),
    ];
    let reconstructed = reconstruct_approval_tree_with_env(
        &old_base,
        &new_base,
        &review_head,
        verbose,
        Some(&env),
    )?;
    get_review_status_between(
        &reconstructed.tree_oid,
        &source_oid,
        &format!("to {}", metadata.source),
        verbose,
        Some(&env),
    )
    .map_err(ReviewError::Git)
}

/// Get review status (remaining diff stats)
///
/// # Arguments
///
/// * `compare_ref` - The Git ref to compare against.
/// * `display_label` - The human-readable label for the comparison.
/// * `verbose` - Whether to print the git command and its output.
///
/// # Returns
///
/// * `ReviewStatus` - The remaining diff statistics
pub fn get_review_status(
    compare_ref: &str,
    display_label: &str,
    verbose: bool,
) -> Result<ReviewStatus, crate::git::GitCommandError> {
    get_review_status_between("HEAD", compare_ref, display_label, verbose, None)
}

fn status_git(
    description: &str,
    args: &[&str],
    verbose: bool,
    env: Option<&[(&str, &OsStr)]>,
) -> Result<std::process::Output, crate::git::GitCommandError> {
    match env {
        Some(env) => crate::git::run_git_command_with_env(description, args, env, &[], verbose),
        None => run_git_command(description, args, &[], verbose),
    }
}

fn get_review_status_between(
    review_ref: &str,
    compare_ref: &str,
    display_label: &str,
    verbose: bool,
    env: Option<&[(&str, &OsStr)]>,
) -> Result<ReviewStatus, crate::git::GitCommandError> {
    // Get diff stats summary (use HEAD..branch for direct comparison, not HEAD...branch)
    let stat_output = status_git(
        "get diff stats",
        &["diff", "--stat", review_ref, compare_ref],
        verbose,
        env,
    )?;
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
    let files_output = status_git(
        "get changed files",
        &["diff", "--name-only", review_ref, compare_ref],
        verbose,
        env,
    )?;
    let files: Vec<String> = String::from_utf8_lossy(&files_output.stdout)
        .lines()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    Ok(ReviewStatus {
        display_label: display_label.to_string(),
        file_count,
        insertions,
        deletions,
        files,
    })
}
