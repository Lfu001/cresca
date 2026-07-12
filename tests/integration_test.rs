mod common;

use common::TempGitRepo;
use std::collections::BTreeSet;

struct LinearRange {
    base: String,
    a: String,
    b: String,
    c: String,
    d: String,
}

fn setup_linear_range() -> (TempGitRepo, LinearRange) {
    let repo = TempGitRepo::new();

    repo.write_file("shared.txt", "shared at base\n");
    repo.write_file("removed-at-c.txt", "present until C\n");
    repo.git(&["add", "."]);
    repo.commit("Add range base files");
    repo.git(&["push", "origin", "main"]);
    let base = repo.rev_parse("HEAD");

    repo.create_branch("develop");
    repo.write_file("a.txt", "added at A\n");
    repo.git(&["add", "."]);
    repo.commit("A: add a.txt");
    let a = repo.rev_parse("HEAD");

    repo.write_file("shared.txt", "shared changed at B\n");
    repo.git(&["add", "."]);
    repo.commit("B: change shared.txt");
    let b = repo.rev_parse("HEAD");

    repo.git(&["rm", "removed-at-c.txt"]);
    repo.commit("C: remove removed-at-c.txt");
    let c = repo.rev_parse("HEAD");

    repo.write_file("d.txt", "added at D\n");
    repo.git(&["add", "."]);
    repo.commit("D: add d.txt");
    let d = repo.rev_parse("HEAD");

    repo.git(&["push", "-u", "origin", "develop"]);
    repo.switch_branch("main");

    (repo, LinearRange { base, a, b, c, d })
}

#[test]
fn test_helpers_capture_untracked_and_do_not_mutate_real_index() {
    let repo = TempGitRepo::new();

    repo.write_file("README.md", "# Updated Test Repository");
    repo.write_file("new.txt", "untracked content");
    repo.git(&["add", "README.md"]);

    let cached_before = repo.cached_diff();
    let logical_diff = repo.worktree_diff();

    assert_eq!(repo.cached_diff(), cached_before);
    assert!(String::from_utf8_lossy(&logical_diff).contains("new.txt"));
    assert!(String::from_utf8_lossy(&logical_diff).contains("README.md"));
}

#[test]
fn test_review_materializes_exact_three_dot_diff() {
    let repo = TempGitRepo::new();

    repo.write_file("shared.txt", "base version\n");
    repo.write_file("deleted.txt", "delete me\n");
    repo.git(&["add", "."]);
    repo.commit("Add shared base files");
    repo.git(&["push", "origin", "main"]);

    repo.create_branch("develop");
    repo.write_file("shared.txt", "develop version\n");
    repo.git(&["rm", "deleted.txt"]);
    repo.write_file("added.txt", "added on develop\n");
    repo.git(&["add", "."]);
    repo.commit("Develop feature changes");
    repo.git(&["push", "-u", "origin", "develop"]);

    repo.switch_branch("main");
    repo.write_file("main-only.txt", "added on main\n");
    repo.git(&["add", "."]);
    repo.commit("Add main-only change");
    repo.git(&["push", "origin", "main"]);

    let merge_base = repo.git_stdout(&["merge-base", "origin/main", "origin/develop"]);
    let expected_diff = repo.diff(&merge_base, "origin/develop");

    let output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.current_branch(), "review-main-develop");
    assert_eq!(repo.rev_parse("HEAD"), merge_base);
    assert!(
        repo.cached_diff().is_empty(),
        "review must leave the real index empty"
    );
    assert_eq!(repo.worktree_diff(), expected_diff);
    assert_eq!(repo.read_file("shared.txt"), "develop version\n");
    assert_eq!(repo.read_file("added.txt"), "added on develop\n");
    assert!(!repo.path().join("deleted.txt").exists());
    assert!(!repo.path().join("main-only.txt").exists());

    let status = String::from_utf8(
        repo.git(&["status", "--porcelain=v1", "-z", "--untracked-files=all"])
            .stdout,
    )
    .expect("porcelain status should be UTF-8");
    let status_records: BTreeSet<&str> = status
        .split('\0')
        .filter(|record| !record.is_empty())
        .collect();
    assert_eq!(
        status_records,
        BTreeSet::from([" M shared.txt", " D deleted.txt", "?? added.txt"])
    );

    assert!(String::from_utf8_lossy(&output.stdout).contains("Review branch prepared successfully"));
    assert!(output.stderr.is_empty());
}

/// Test that `cresca approve` commits staged changes and discards unstaged ones.
#[test]
fn test_approve_commits_staged() {
    let repo = TempGitRepo::new();

    // Setup: create develop with two files
    repo.create_branch("develop");
    repo.write_file("reviewed.txt", "reviewed content");
    repo.write_file("not_reviewed.txt", "not reviewed content");
    repo.git(&["add", "."]);
    repo.commit("Add features");
    repo.git(&["push", "-u", "origin", "develop"]);

    // Switch back to main and run review
    repo.switch_branch("main");
    repo.run_cresca(&["review", "main", "develop"]);

    // Stage only one file (simulating partial review)
    repo.git(&["add", "reviewed.txt"]);

    // Run approve
    let output = repo.run_cresca(&["approve"]);
    assert!(
        output.status.success(),
        "cresca approve should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify: reviewed.txt should be committed
    let files_in_head = repo.git(&["ls-tree", "--name-only", "HEAD"]);
    let files_str = String::from_utf8_lossy(&files_in_head.stdout);
    assert!(
        files_str.contains("reviewed.txt"),
        "reviewed.txt should be committed"
    );

    // Verify: not_reviewed.txt should NOT exist (discarded)
    let not_reviewed_path = repo.path().join("not_reviewed.txt");
    assert!(
        !not_reviewed_path.exists(),
        "not_reviewed.txt should be discarded"
    );

    // Verify: working directory is clean
    assert!(
        !repo.has_uncommitted_changes(),
        "Working directory should be clean after approve"
    );
}

/// Test that `cresca approve` fails on a non-review branch.
#[test]
fn test_approve_on_non_review_branch() {
    let repo = TempGitRepo::new();

    // Try to approve on main (not a review branch)
    let output = repo.run_cresca(&["approve"]);

    assert!(
        !output.status.success(),
        "cresca approve should fail on non-review branch"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error") || stderr.contains("Not on a review branch"),
        "Should show error message about not being on review branch"
    );
}

/// Test that `cresca review` fails with uncommitted changes.
#[test]
fn test_review_with_uncommitted_changes() {
    let repo = TempGitRepo::new();

    // Create develop branch and push it
    repo.create_branch("develop");
    repo.git(&["push", "-u", "origin", "develop"]);
    repo.switch_branch("main");

    // Create uncommitted changes
    repo.write_file("uncommitted.txt", "uncommitted content");

    // Try to run review
    let output = repo.run_cresca(&["review", "main", "develop"]);

    assert!(
        !output.status.success(),
        "cresca review should fail with uncommitted changes"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error") || stderr.contains("Uncommitted"),
        "Should show error about uncommitted changes"
    );
}

/// Test that running review twice updates the review branch correctly.
#[test]
fn test_review_updates_existing_branch() {
    let repo = TempGitRepo::new();

    // Create develop branch with initial change
    repo.create_branch("develop");
    repo.write_file("file1.txt", "content 1");
    repo.git(&["add", "."]);
    repo.commit("Add file1");
    repo.git(&["push", "-u", "origin", "develop"]);

    // First review
    repo.switch_branch("main");
    repo.run_cresca(&["review", "main", "develop"]);

    // Approve all changes
    repo.git(&["add", "."]);
    repo.run_cresca(&["approve"]);

    // Add more changes to develop
    repo.switch_branch("develop");
    repo.write_file("file2.txt", "content 2");
    repo.git(&["add", "."]);
    repo.commit("Add file2");
    repo.git(&["push", "origin", "develop"]);

    // Second review (from the review branch)
    repo.switch_branch("review-main-develop");
    repo.run_cresca(&["review", "main", "develop"]);

    // Verify: file1.txt should still be present (previously approved)
    assert!(
        repo.path().join("file1.txt").exists(),
        "file1.txt should exist from previous approval"
    );

    // Verify: file2.txt should appear as new change
    let status = repo.git(&["status", "--porcelain"]);
    let status_str = String::from_utf8_lossy(&status.stdout);
    assert!(
        status_str.contains("file2.txt"),
        "file2.txt should appear as new unreviewed change"
    );
}

/// Test that `cresca review --skip-to` auto-approves earlier commits.
#[test]
fn test_review_with_skip_to_option() {
    let (repo, range) = setup_linear_range();
    let skip_to = &range.b[..8];

    let output = repo.run_cresca(&["review", "main", "develop", "--skip-to", skip_to]);
    assert!(
        output.status.success(),
        "cresca review --skip-to should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(
        repo.git_stdout(&["show", "-s", "--format=%s", "HEAD"]),
        "Auto-approve earlier commits"
    );
    assert_eq!(
        repo.git_stdout(&["rev-list", "--count", &format!("{}..HEAD", range.base)]),
        "1"
    );
    assert_eq!(
        repo.rev_parse("HEAD^{tree}"),
        repo.rev_parse(&format!("{}^{{tree}}", range.a))
    );
    assert!(repo.cached_diff().is_empty());
    assert_eq!(repo.worktree_diff(), repo.diff("HEAD", &range.d));
}

#[test]
fn test_review_with_skip_to_first_commit_keeps_full_range_unstaged() {
    let (repo, range) = setup_linear_range();
    let skip_to = &range.a[..8];

    let output = repo.run_cresca(&["review", "main", "develop", "--skip-to", skip_to]);
    assert!(
        output.status.success(),
        "cresca review --skip-to A should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(repo.rev_parse("HEAD"), range.base);
    assert!(repo.cached_diff().is_empty());
    assert_eq!(repo.worktree_diff(), repo.diff(&range.base, &range.d));
}

/// Test that `cresca review --skip-to` with already approved commits works correctly.
#[test]
fn test_review_with_skip_to_already_approved() {
    let repo = TempGitRepo::new();

    // Create develop branch with multiple commits
    repo.create_branch("develop");
    repo.write_file("file1.txt", "content 1");
    repo.git(&["add", "."]);
    repo.commit("Add file1");

    repo.write_file("file2.txt", "content 2");
    repo.git(&["add", "."]);
    repo.commit("Add file2");

    repo.git(&["push", "-u", "origin", "develop"]);

    // Get hashes
    let log_output = repo.git(&["log", "--oneline", "main..develop"]);
    let log_str = String::from_utf8_lossy(&log_output.stdout);
    let commits: Vec<&str> = log_str.lines().collect();
    let file2_hash = commits[0].split_whitespace().next().unwrap();
    let file1_hash = commits[1].split_whitespace().next().unwrap();

    // Switch back to main and do first review with --skip-to file2 (file1 auto-approved)
    repo.switch_branch("main");
    repo.run_cresca(&["review", "main", "develop", "--skip-to", file2_hash]);

    // Approve file2
    repo.git(&["add", "."]);
    repo.run_cresca(&["approve"]);

    // Now try to run review again with --skip-to file1 (file1 already committed)
    let output = repo.run_cresca(&["review", "main", "develop", "--skip-to", file1_hash]);
    assert!(
        output.status.success(),
        "cresca review --skip-to with already approved commits should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Test that `cresca status` shows remaining diff statistics on a review branch.
#[test]
fn test_status_shows_diff_stats() {
    let repo = TempGitRepo::new();

    // Create a develop branch with some changes
    repo.create_branch("develop");
    repo.write_file("feature1.txt", "new feature 1");
    repo.write_file("feature2.txt", "new feature 2");
    repo.git(&["add", "."]);
    repo.commit("Add features");
    repo.git(&["push", "-u", "origin", "develop"]);

    // Switch back to main and run review
    repo.switch_branch("main");
    repo.run_cresca(&["review", "main", "develop"]);

    // Run status
    let output = repo.run_cresca(&["status"]);
    assert!(
        output.status.success(),
        "cresca status should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Review status"),
        "Should show review status header"
    );
    assert!(
        stdout.contains("Remaining diff to develop"),
        "Should mention develop branch"
    );
    assert!(stdout.contains("2 file(s)"), "Should show 2 files changed");
    assert!(stdout.contains("feature1.txt"), "Should list feature1.txt");
    assert!(stdout.contains("feature2.txt"), "Should list feature2.txt");
}

/// Test that `cresca status` fails on a non-review branch.
#[test]
fn test_status_on_non_review_branch() {
    let repo = TempGitRepo::new();

    // Try to run status on main (not a review branch)
    let output = repo.run_cresca(&["status"]);

    assert!(
        !output.status.success(),
        "cresca status should fail on non-review branch"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error") || stderr.contains("Not on a review branch"),
        "Should show error message about not being on review branch"
    );
}

/// Test that `cresca status` updates after partial approval.
#[test]
fn test_status_after_partial_approval() {
    let repo = TempGitRepo::new();

    // Create develop branch with multiple files
    repo.create_branch("develop");
    repo.write_file("file1.txt", "content 1");
    repo.write_file("file2.txt", "content 2");
    repo.write_file("file3.txt", "content 3");
    repo.git(&["add", "."]);
    repo.commit("Add three files");
    repo.git(&["push", "-u", "origin", "develop"]);

    // Switch back to main and run review
    repo.switch_branch("main");
    repo.run_cresca(&["review", "main", "develop"]);

    // Initial status should show 3 files
    let output = repo.run_cresca(&["status"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("3 file(s)"),
        "Should initially show 3 files, got: {}",
        stdout
    );

    // Approve only one file
    repo.git(&["add", "file1.txt"]);
    repo.run_cresca(&["approve"]);

    // Run review again to see remaining changes
    repo.run_cresca(&["review", "main", "develop"]);

    // Status should show remaining files (file2.txt and file3.txt)
    let output = repo.run_cresca(&["status"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // After partial approval, approved file should not appear in unstaged diff
    assert!(
        stdout.contains("file2.txt"),
        "file2.txt should be in remaining files, got: {}",
        stdout
    );
    assert!(
        stdout.contains("file3.txt"),
        "file3.txt should be in remaining files, got: {}",
        stdout
    );
}

/// Test that `cresca review --stop-at` excludes later commits from review.
#[test]
fn test_review_with_stop_at_option() {
    let (repo, range) = setup_linear_range();
    let stop_at = &range.c[..8];

    let output = repo.run_cresca(&["review", "main", "develop", "--stop-at", stop_at]);
    assert!(
        output.status.success(),
        "cresca review --stop-at should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(repo.rev_parse("HEAD"), range.base);
    assert!(repo.cached_diff().is_empty());
    assert_eq!(repo.worktree_diff(), repo.diff(&range.base, &range.c));
    assert!(!repo.path().join("d.txt").exists());
}

#[test]
fn test_review_with_stop_at_last_commit_keeps_full_range_unstaged() {
    let (repo, range) = setup_linear_range();
    let stop_at = &range.d[..8];

    let output = repo.run_cresca(&["review", "main", "develop", "--stop-at", stop_at]);
    assert!(
        output.status.success(),
        "cresca review --stop-at D should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(repo.rev_parse("HEAD"), range.base);
    assert!(repo.cached_diff().is_empty());
    assert_eq!(repo.worktree_diff(), repo.diff(&range.base, &range.d));
}

/// Test that `cresca review --skip-to --stop-at` limits review to specific range.
#[test]
fn test_review_with_skip_to_and_stop_at() {
    let (repo, range) = setup_linear_range();
    let skip_to = &range.b[..8];
    let stop_at = &range.c[..8];

    let output = repo.run_cresca(&[
        "review",
        "main",
        "develop",
        "--skip-to",
        skip_to,
        "--stop-at",
        stop_at,
    ]);
    assert!(
        output.status.success(),
        "cresca review --skip-to --stop-at should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(
        repo.git_stdout(&["show", "-s", "--format=%s", "HEAD"]),
        "Auto-approve earlier commits"
    );
    assert_eq!(
        repo.git_stdout(&["rev-list", "--count", &format!("{}..HEAD", range.base)]),
        "1"
    );
    assert_eq!(
        repo.rev_parse("HEAD^{tree}"),
        repo.rev_parse(&format!("{}^{{tree}}", range.a))
    );
    assert!(repo.cached_diff().is_empty());
    assert_eq!(repo.worktree_diff(), repo.diff("HEAD", &range.c));
    assert!(!repo.path().join("d.txt").exists());
}

#[test]
fn test_review_with_skip_to_and_stop_at_same_commit_includes_that_commit() {
    let (repo, range) = setup_linear_range();
    let boundary = &range.c[..8];

    let output = repo.run_cresca(&[
        "review",
        "main",
        "develop",
        "--skip-to",
        boundary,
        "--stop-at",
        boundary,
    ]);
    assert!(
        output.status.success(),
        "cresca review --skip-to C --stop-at C should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(
        repo.git_stdout(&["show", "-s", "--format=%s", "HEAD"]),
        "Auto-approve earlier commits"
    );
    assert_eq!(
        repo.git_stdout(&["rev-list", "--count", &format!("{}..HEAD", range.base)]),
        "1"
    );
    assert_eq!(
        repo.rev_parse("HEAD^{tree}"),
        repo.rev_parse(&format!("{}^{{tree}}", range.b))
    );
    assert!(repo.cached_diff().is_empty());
    assert_eq!(repo.worktree_diff(), repo.diff("HEAD", &range.c));
    assert!(!repo.path().join("d.txt").exists());
}

/// Test that `cresca review --stop-at` fails with invalid commit hash.
#[test]
fn test_review_with_invalid_stop_at() {
    let repo = TempGitRepo::new();

    // Create develop branch with a commit
    repo.create_branch("develop");
    repo.write_file("file1.txt", "content 1");
    repo.git(&["add", "."]);
    repo.commit("Add file1");
    repo.git(&["push", "-u", "origin", "develop"]);

    // Switch back to main
    repo.switch_branch("main");

    // Run cresca review with invalid --stop-at hash
    let output = repo.run_cresca(&["review", "main", "develop", "--stop-at", "invalidhash"]);

    assert!(
        !output.status.success(),
        "cresca review --stop-at with invalid hash should fail"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error") && stderr.contains("invalidhash"),
        "Should show error about invalid commit hash, got: {}",
        stderr
    );
}

/// Test that `cresca review` fails when --stop-at is before --skip-to.
#[test]
fn test_review_with_stop_at_before_skip_to() {
    let repo = TempGitRepo::new();

    // Create develop branch with multiple commits
    repo.create_branch("develop");
    repo.write_file("file1.txt", "content 1");
    repo.git(&["add", "."]);
    repo.commit("Add file1");

    repo.write_file("file2.txt", "content 2");
    repo.git(&["add", "."]);
    repo.commit("Add file2");

    repo.write_file("file3.txt", "content 3");
    repo.git(&["add", "."]);
    repo.commit("Add file3");

    repo.git(&["push", "-u", "origin", "develop"]);

    // Get commit hashes: file3 (newest), file2, file1 (oldest)
    let log_output = repo.git(&["log", "--oneline", "main..develop"]);
    let log_str = String::from_utf8_lossy(&log_output.stdout);
    let commits: Vec<&str> = log_str.lines().collect();
    let file3_hash = commits[0].split_whitespace().next().unwrap();
    let file1_hash = commits[2].split_whitespace().next().unwrap();

    // Switch back to main
    repo.switch_branch("main");

    // Run cresca review with --stop-at BEFORE --skip-to (invalid)
    let output = repo.run_cresca(&[
        "review",
        "main",
        "develop",
        "--skip-to",
        file3_hash,
        "--stop-at",
        file1_hash,
    ]);

    assert!(
        !output.status.success(),
        "cresca review should fail when --stop-at is before --skip-to"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error") && stderr.contains("--stop-at"),
        "Should show error about --stop-at being before --skip-to, got: {}",
        stderr
    );
}

/// Test that `cresca review` works with branch names containing slashes.
#[test]
fn test_review_with_slash_in_branch_name() {
    let repo = TempGitRepo::new();
    repo.create_branch("feature/login-page");
    repo.write_file("login.txt", "login stuff");
    repo.git(&["add", "."]);
    repo.commit("Add login");
    repo.git(&["push", "-u", "origin", "feature/login-page"]);

    repo.switch_branch("main");
    let output = repo.run_cresca(&["review", "main", "feature/login-page"]);
    assert!(
        output.status.success(),
        "cresca review should succeed with slash in branch name\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Test that `cresca review` works when the local branch does not exist.
#[test]
fn test_review_without_local_branch() {
    let repo = TempGitRepo::new();
    // Simulate another user pushing a branch
    repo.create_branch("other-users-feature");
    repo.write_file("other.txt", "other stuff");
    repo.git(&["add", "."]);
    repo.commit("Add other stuff");
    repo.git(&["push", "-u", "origin", "other-users-feature"]);

    // Switch to main and completely delete the local branch
    repo.switch_branch("main");
    repo.git(&["branch", "-D", "other-users-feature"]);

    let output = repo.run_cresca(&["review", "main", "other-users-feature"]);
    assert!(
        output.status.success(),
        "cresca review should succeed even if local branch does not exist\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Test that `cresca review` works with a custom remote name (e.g. upstream).
#[test]
fn test_review_with_custom_remote() {
    // This is tricky with TempGitRepo since it sets up 'origin' by default.
    // We will rename the remote to 'upstream' for this test.
    let repo = TempGitRepo::new();
    repo.git(&["remote", "rename", "origin", "upstream"]);

    repo.create_branch("develop");
    repo.write_file("dev.txt", "dev stuff");
    repo.git(&["add", "."]);
    repo.commit("Add dev stuff");
    // Push setting upstream explicitly
    repo.git(&["push", "-u", "upstream", "develop"]);
    repo.git(&["push", "-u", "upstream", "main"]);

    repo.switch_branch("main");
    let output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        output.status.success(),
        "cresca review should succeed with a custom remote named 'upstream'\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Test that `cresca review` works when given remote tracking branch explicitly.
#[test]
fn test_review_with_explicit_remote_tracking_branch() {
    let repo = TempGitRepo::new();
    repo.create_branch("develop");
    repo.write_file("dev.txt", "dev stuff");
    repo.git(&["add", "."]);
    repo.commit("Add dev stuff");
    repo.git(&["push", "-u", "origin", "develop"]);

    repo.switch_branch("main");
    // Pass 'origin/develop' instead of 'develop'
    let output = repo.run_cresca(&["review", "main", "origin/develop"]);
    assert!(
        output.status.success(),
        "cresca review should succeed with explicit remote tracking branch\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
