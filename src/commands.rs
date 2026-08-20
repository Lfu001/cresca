use crate::branch_naming::resolve_new_review_branch_name;
use crate::git::{
    find_existing_review_branch, is_clean, read_review_scope, resolve_remote_tracking_branch,
    run_git_command, run_git_command_machine_output, select_new_review_branch,
    write_review_metadata, write_review_scope, ReviewBranchSelection, ReviewBranchSelectionError,
    ReviewMetadata, ReviewScope, ReviewScopeError,
};
use crate::review::{
    find_unique_merge_base, reconstruct_approval_tree, ReviewError, ReviewPreparation,
    ReviewTransaction,
};
use std::fmt::Write as _;
use std::ops::Not;

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

/// Prepare a review branch with explicit approval-tree reconstruction.
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
    let new_base = find_unique_merge_base(&tracking_to, &endpoint, verbose)?;

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
        base_oid: new_base.clone(),
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

    let map_selection_error = |error| match error {
        ReviewBranchSelectionError::Git(error) => ReviewError::Git(error),
        ReviewBranchSelectionError::Conflict(message) => ReviewError::Message(message),
    };
    let selection =
        match find_existing_review_branch(&metadata, verbose).map_err(map_selection_error)? {
            Some(branch) => ReviewBranchSelection::Existing(branch),
            None => {
                let base = resolve_new_review_branch_name(&metadata, verbose)
                    .map_err(|error| ReviewError::Message(error.to_string()))?;
                select_new_review_branch(&base, &metadata, verbose).map_err(map_selection_error)?
            }
        };
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
            let saved = read_review_scope(branch, verbose).map_err(|error| match error {
                ReviewScopeError::Git(error) => ReviewError::Git(error),
                error => ReviewError::Message(format!(
                    "Cannot read existing review range metadata: {error:?}"
                )),
            })?;
            let old_base = saved.base_oid;
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

fn create_commit_from_tree(
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
        let commit =
            create_commit_from_tree(description, &approved_tree, &parent, message, verbose)?;
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
        &["reset", "--hard", "--quiet", "HEAD"],
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

fn display_diff_path(path: &[u8]) -> String {
    let is_unambiguous = std::str::from_utf8(path)
        .is_ok_and(|text| !text.chars().any(is_unsafe_display_character))
        && !path
            .iter()
            .any(|byte| byte.is_ascii_control() || matches!(*byte, b'\\' | b'"'))
        && !path.windows(b" -> ".len()).any(|window| window == b" -> ");
    if is_unambiguous {
        return String::from_utf8(path.to_vec()).expect("validated UTF-8 path should decode");
    }

    let mut rendered = String::from("\"");
    for &byte in path {
        match byte {
            b'\\' => rendered.push_str("\\\\"),
            b'"' => rendered.push_str("\\\""),
            b'\n' => rendered.push_str("\\n"),
            b'\r' => rendered.push_str("\\r"),
            b'\t' => rendered.push_str("\\t"),
            byte if byte.is_ascii_graphic() || byte == b' ' => rendered.push(byte as char),
            byte => write!(&mut rendered, "\\x{byte:02X}")
                .expect("writing an escaped path to a String should not fail"),
        }
    }
    rendered.push('"');
    rendered
}

fn is_unsafe_display_character(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{00AD}'
                | '\u{061C}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{E0001}'
                | '\u{E0020}'..='\u{E007F}'
        )
}

fn parse_name_status(output: &[u8]) -> Result<Vec<String>, String> {
    let mut fields = output.split(|byte| *byte == 0).peekable();
    let mut files = Vec::new();

    while let Some(status) = fields.next() {
        if status.is_empty() {
            continue;
        }
        let status = String::from_utf8_lossy(status);
        let Some(first_path) = fields.next() else {
            return Err("missing path after status record".to_string());
        };
        if first_path.is_empty() {
            return Err("empty path after status record".to_string());
        }
        if status.starts_with('R') || status.starts_with('C') {
            let Some(second_path) = fields.next() else {
                return Err("missing destination path after rename or copy record".to_string());
            };
            if second_path.is_empty() {
                return Err("missing destination path after rename or copy record".to_string());
            }
            files.push(format!(
                "{status} {} -> {}",
                display_diff_path(first_path),
                display_diff_path(second_path)
            ));
        } else {
            files.push(display_diff_path(first_path));
        }
    }

    Ok(files)
}

fn parse_numstat_totals(output: &[u8]) -> Result<(usize, usize), String> {
    let mut fields = output.split(|byte| *byte == 0).peekable();
    let mut insertions = 0;
    let mut deletions = 0;

    while let Some(record) = fields.next() {
        if record.is_empty() {
            continue;
        }
        let mut stats = record.splitn(3, |byte| *byte == b'\t');
        let added = stats.next().ok_or("missing insertion count")?;
        let deleted = stats.next().ok_or("missing deletion count")?;
        let path = stats.next().ok_or("missing numstat path field")?;
        let parse_count = |count: &[u8], label: &str| {
            if count == b"-" {
                return Ok(0);
            }
            std::str::from_utf8(count)
                .map_err(|_| format!("non-UTF-8 {label} count"))?
                .parse::<usize>()
                .map_err(|_| format!("invalid {label} count"))
        };
        insertions += parse_count(added, "insertion")?;
        deletions += parse_count(deleted, "deletion")?;

        // In -z numstat output, renamed and copied paths are emitted as an empty path field
        // followed by their old and new paths. Their counts belong to the record above.
        if path.is_empty() {
            let old_path = fields
                .next()
                .ok_or("missing old path after renamed numstat record")?;
            let new_path = fields
                .next()
                .ok_or("missing new path after renamed numstat record")?;
            if old_path.is_empty() || new_path.is_empty() {
                return Err("missing new path after renamed numstat record".to_string());
            }
        }
    }

    Ok((insertions, deletions))
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
) -> Result<ReviewStatus, ReviewError> {
    calculate_review_status_between("HEAD", compare_ref, display_label, verbose)
}

fn calculate_review_status_between(
    review_ref: &str,
    compare_ref: &str,
    display_label: &str,
    verbose: bool,
) -> Result<ReviewStatus, ReviewError> {
    // Use NUL-delimited machine-readable output. Human-oriented --stat text is localized and
    // cannot distinguish renames or tree entry kinds reliably.
    let name_status_output = run_git_command_machine_output(
        "get changed file statuses",
        &[
            "diff",
            "--name-status",
            "-z",
            "--find-renames=50%",
            review_ref,
            compare_ref,
        ],
        &[],
        verbose,
    )?;
    let files = parse_name_status(&name_status_output.stdout).map_err(|error| {
        ReviewError::Message(format!("Git returned invalid name-status output: {error}"))
    })?;
    let numstat_output = run_git_command_machine_output(
        "get changed line counts",
        &[
            "diff",
            "--numstat",
            "-z",
            "--find-renames=50%",
            review_ref,
            compare_ref,
        ],
        &[],
        verbose,
    )?;
    let (insertions, deletions) =
        parse_numstat_totals(&numstat_output.stdout).map_err(|error| {
            ReviewError::Message(format!("Git returned invalid numstat output: {error}"))
        })?;

    Ok(ReviewStatus {
        display_label: display_label.to_string(),
        file_count: files.len(),
        insertions,
        deletions,
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::{display_diff_path, parse_name_status, parse_numstat_totals};
    use std::io::Write;
    use std::path::Path;
    use std::process::{Command, Stdio};

    fn git_stdout(repository: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
        let mut child = Command::new("git")
            .args(args)
            .current_dir(repository)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Git should start");
        child
            .stdin
            .as_mut()
            .expect("Git stdin should be available")
            .write_all(input)
            .expect("Git stdin should accept fixture data");
        let output = child.wait_with_output().expect("Git should finish");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    fn git_object_name(repository: &Path, args: &[&str], input: &[u8]) -> String {
        String::from_utf8(git_stdout(repository, args, input))
            .expect("Git object name should be UTF-8")
            .trim()
            .to_string()
    }

    #[test]
    fn parse_name_status_escapes_non_utf8_rename_paths_from_git_tree_records() {
        let repository = tempfile::tempdir().expect("temporary repository should be created");
        let root = repository.path();
        git_stdout(root, &["init", "-q"], b"");
        git_stdout(root, &["config", "user.name", "Test User"], b"");
        git_stdout(root, &["config", "user.email", "test@example.com"], b"");

        let blob = git_object_name(root, &["hash-object", "-w", "--stdin"], b"fixture\n");
        let tree_for = |name: &[u8]| {
            let mut record = format!("100644 blob {blob}\t").into_bytes();
            record.extend_from_slice(name);
            record.push(0);
            git_object_name(root, &["mktree", "-z"], &record)
        };
        let old_tree = tree_for(b"invalid-\xff-old.txt");
        let new_tree = tree_for(b"invalid-\xff-new.txt");
        let base = git_object_name(root, &["commit-tree", &old_tree, "-m", "base"], b"");
        let endpoint = git_object_name(
            root,
            &["commit-tree", &new_tree, "-p", &base, "-m", "rename"],
            b"",
        );
        let output = git_stdout(
            root,
            &[
                "diff",
                "--name-status",
                "-z",
                "--find-renames=50%",
                &base,
                &endpoint,
            ],
            b"",
        );

        assert_eq!(
            parse_name_status(&output).expect("Git output should parse"),
            vec!["R100 \"invalid-\\xFF-old.txt\" -> \"invalid-\\xFF-new.txt\""],
        );
    }

    #[test]
    fn parse_name_status_rejects_incomplete_rename_record() {
        assert_eq!(
            parse_name_status(b"R100\0old-name.txt\0").unwrap_err(),
            "missing destination path after rename or copy record"
        );
    }

    #[test]
    fn parse_numstat_rejects_invalid_counts_and_incomplete_renames() {
        assert_eq!(
            parse_numstat_totals(b"not-a-number\t0\tfile.txt\0").unwrap_err(),
            "invalid insertion count"
        );
        assert_eq!(
            parse_numstat_totals(b"1\t0\t\0old-name.txt\0").unwrap_err(),
            "missing new path after renamed numstat record"
        );
    }

    #[test]
    fn display_diff_path_escapes_unicode_control_and_bidi_characters() {
        assert_eq!(
            display_diff_path("c1-\u{009B}-path".as_bytes()),
            "\"c1-\\xC2\\x9B-path\""
        );
        assert_eq!(
            display_diff_path("bidi-\u{202E}-path".as_bytes()),
            "\"bidi-\\xE2\\x80\\xAE-path\""
        );
    }
}
