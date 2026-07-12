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

fn assert_review_matches(
    repo: &TempGitRepo,
    expected_branch: &str,
    target_ref: &str,
    source_ref: &str,
) {
    let merge_base = repo.git_stdout(&["merge-base", target_ref, source_ref]);
    assert_eq!(repo.current_branch(), expected_branch);
    assert_eq!(repo.rev_parse("HEAD"), merge_base);
    assert!(
        repo.cached_diff().is_empty(),
        "review must leave the real index empty"
    );
    assert_eq!(repo.worktree_diff(), repo.diff(&merge_base, source_ref));
}

fn prepare_staged_unstaged_and_untracked_changes(repo: &TempGitRepo) {
    repo.write_file("staged.txt", "staged base\n");
    repo.write_file("unstaged.txt", "unstaged base\n");
    repo.git(&["add", "."]);
    repo.commit("Add dirty state fixtures");

    repo.write_file("staged.txt", "staged modification\n");
    repo.git(&["add", "staged.txt"]);
    repo.write_file("unstaged.txt", "unstaged modification\n");
    repo.write_file("untracked.txt", "untracked content\n");
}

#[test]
fn test_helpers_capture_untracked_and_do_not_mutate_real_index() {
    let repo = TempGitRepo::new();

    std::fs::write(repo.path().join("binary.bin"), b"base\0binary\n")
        .expect("binary fixture should be writable");
    repo.write_file("deleted.txt", "tracked deletion fixture\n");
    repo.git(&["add", "."]);
    repo.commit("Add worktree diff fixtures");

    repo.write_file("README.md", "# Updated Test Repository");
    std::fs::write(repo.path().join("binary.bin"), b"updated\0binary\n")
        .expect("binary fixture should be writable");
    repo.git(&["rm", "deleted.txt"]);
    repo.write_file("untracked.txt", "untracked content\n");
    repo.git(&["add", "README.md"]);

    let cached_before = repo.cached_diff();
    let real_index_before = repo.real_index_bytes();
    let logical_diff = repo.worktree_diff();
    let real_index_after = repo.real_index_bytes();

    assert_eq!(repo.cached_diff(), cached_before);
    assert_eq!(
        real_index_after, real_index_before,
        "worktree_diff must leave the real index byte-for-byte unchanged"
    );
    let logical_diff_text = String::from_utf8_lossy(&logical_diff);
    assert!(logical_diff_text.contains("GIT binary patch"));
    assert!(logical_diff_text.contains("diff --git a/binary.bin b/binary.bin"));
    assert!(logical_diff_text.contains("deleted file mode"));
    assert!(logical_diff_text.contains("diff --git a/deleted.txt b/deleted.txt"));
    assert!(logical_diff_text.contains("diff --git a/untracked.txt b/untracked.txt"));
    assert!(logical_diff_text.contains("README.md"));
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

/// Test that `cresca approve` commits exactly the staged tree and discards everything else.
#[test]
fn test_approve_commits_staged() {
    let repo = TempGitRepo::new();

    let base_content = "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\n";
    let partial_expected_content =
        "line 1\nreviewed line 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\n";
    let full_develop_content =
        "line 1\nreviewed line 2\nline 3\nline 4\nline 5\nline 6\nreviewed line 7\nline 8\n";
    let approved_content = "approved addition\n";

    repo.write_file("reviewed.txt", base_content);
    repo.write_file("kept.txt", "keep this file\n");
    repo.git(&["add", "."]);
    repo.commit("Add approval base files");
    repo.git(&["push", "origin", "main"]);

    repo.create_branch("develop");
    repo.write_file("reviewed.txt", full_develop_content);
    repo.write_file("approved.txt", approved_content);
    repo.write_file("unreviewed.txt", "unreviewed addition\n");
    repo.git(&["rm", "kept.txt"]);
    repo.git(&["add", "."]);
    repo.commit("Add mixed approval changes");
    repo.git(&["push", "-u", "origin", "develop"]);

    repo.switch_branch("main");
    let review_output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        review_output.status.success(),
        "cresca review should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&review_output.stdout),
        String::from_utf8_lossy(&review_output.stderr)
    );

    repo.git(&["add", "approved.txt"]);
    repo.write_file("reviewed.txt", partial_expected_content);
    repo.git(&["add", "reviewed.txt"]);
    repo.write_file("reviewed.txt", full_develop_content);

    let expected_tree = repo.git_stdout(&["write-tree"]);
    let expected_commit_diff = repo.cached_diff();
    let parent = repo.rev_parse("HEAD");

    let output = repo.run_cresca(&["approve"]);
    assert!(
        output.status.success(),
        "cresca approve should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        repo.git_stdout(&["show", "-s", "--format=%s", "HEAD"]),
        "Approve reviewed changes"
    );
    assert_eq!(repo.rev_parse("HEAD^"), parent);
    assert_eq!(repo.rev_parse("HEAD^{tree}"), expected_tree);
    assert_eq!(
        repo.git(&[
            "show",
            "--format=",
            "--binary",
            "--no-ext-diff",
            "--no-renames",
            "HEAD",
        ])
        .stdout,
        expected_commit_diff
    );
    assert!(repo.cached_diff().is_empty());
    assert!(repo.worktree_diff().is_empty());
    assert_eq!(repo.read_file("reviewed.txt"), partial_expected_content);
    assert_eq!(repo.read_file("approved.txt"), approved_content);
    assert!(!repo.path().join("unreviewed.txt").exists());
    assert!(repo.path().join("kept.txt").exists());
    assert!(String::from_utf8_lossy(&output.stdout)
        .contains("Reviewed changes were approved successfully"));
    assert!(output.stderr.is_empty());
}

/// Test that an empty approval ends the work session without approving any source change.
#[test]
fn test_approve_with_no_staged_changes_approves_nothing() {
    let repo = TempGitRepo::new();

    repo.write_file("tracked.txt", "base content\n");
    repo.git(&["add", "."]);
    repo.commit("Add empty approval base");
    repo.git(&["push", "origin", "main"]);

    repo.create_branch("develop");
    repo.write_file("tracked.txt", "develop content\n");
    repo.write_file("added.txt", "new source file\n");
    repo.git(&["add", "."]);
    repo.commit("Add empty approval source changes");
    repo.git(&["push", "-u", "origin", "develop"]);

    let expected_source_diff = repo.diff("main", "develop");
    repo.switch_branch("main");
    let review_output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        review_output.status.success(),
        "cresca review should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&review_output.stdout),
        String::from_utf8_lossy(&review_output.stderr)
    );
    assert_eq!(repo.worktree_diff(), expected_source_diff);
    let head_before = repo.rev_parse("HEAD");

    let output = repo.run_cresca(&["approve"]);
    assert!(
        output.status.success(),
        "empty cresca approve should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.rev_parse("HEAD"), head_before);
    assert!(repo.cached_diff().is_empty());
    assert!(repo.worktree_diff().is_empty());
    assert!(String::from_utf8_lossy(&output.stdout)
        .contains("There are no reviewed changes to approve"));
    assert!(output.stderr.is_empty());

    let rereview_output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        rereview_output.status.success(),
        "cresca re-review should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&rereview_output.stdout),
        String::from_utf8_lossy(&rereview_output.stderr)
    );
    assert_eq!(repo.rev_parse("HEAD"), head_before);
    assert_eq!(repo.worktree_diff(), expected_source_diff);
}

/// Test that `cresca approve` fails on a non-review branch.
#[test]
fn test_approve_on_non_review_branch() {
    let repo = TempGitRepo::new();
    prepare_staged_unstaged_and_untracked_changes(&repo);
    let before = repo.snapshot();

    let output = repo.run_cresca(&["approve"]);

    assert!(
        !output.status.success(),
        "cresca approve should fail on non-review branch"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Not on a review branch"),
        "expected non-review diagnostic, got: {stderr}"
    );
    assert_eq!(repo.snapshot(), before);
}

/// Test that `cresca review` fails with uncommitted changes.
#[test]
fn test_review_with_uncommitted_changes() {
    let repo = TempGitRepo::new();

    repo.create_branch("develop");
    repo.write_file("develop.txt", "develop content\n");
    repo.git(&["add", "."]);
    repo.commit("Add develop change");
    repo.git(&["push", "-u", "origin", "develop"]);
    repo.switch_branch("main");

    prepare_staged_unstaged_and_untracked_changes(&repo);
    let before = repo.snapshot();

    let output = repo.run_cresca(&["review", "main", "develop"]);

    assert!(
        !output.status.success(),
        "cresca review should fail with uncommitted changes"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Uncommitted changes found"),
        "expected uncommitted-changes diagnostic, got: {stderr}"
    );
    assert_eq!(repo.snapshot(), before);
    assert!(!repo.ref_exists("refs/heads/review-main-develop"));
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

    repo.write_file("changed.txt", "line one\nline two\n");
    repo.git(&["add", "."]);
    repo.commit("Add status base file");
    repo.git(&["push", "origin", "main"]);

    repo.create_branch("develop");
    repo.write_file("changed.txt", "line one\nchanged line two\n");
    repo.write_file("added.txt", "added line one\nadded line two\n");
    repo.git(&["add", "."]);
    repo.commit("Add status changes");
    repo.git(&["push", "-u", "origin", "develop"]);

    repo.switch_branch("main");
    let review_output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        review_output.status.success(),
        "cresca review should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&review_output.stdout),
        String::from_utf8_lossy(&review_output.stderr)
    );

    let output = repo.run_cresca(&["status"]);
    assert!(
        output.status.success(),
        "cresca status should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("status stdout should be UTF-8"),
        concat!(
            "📋 Review status:\n",
            "  Remaining diff to develop: 2 file(s), +3 insertion(s), -1 deletion(s)\n",
            "  Files remaining:\n",
            "    - added.txt\n",
            "    - changed.txt\n",
        )
    );
    assert!(output.stderr.is_empty());
}

/// Test that `cresca status` fails on a non-review branch.
#[test]
fn test_status_on_non_review_branch() {
    let repo = TempGitRepo::new();
    prepare_staged_unstaged_and_untracked_changes(&repo);
    let before = repo.snapshot();

    let output = repo.run_cresca(&["status"]);

    assert!(
        !output.status.success(),
        "cresca status should fail on non-review branch"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Not on a review branch"),
        "expected non-review diagnostic, got: {stderr}"
    );
    assert_eq!(repo.snapshot(), before);
}

/// Test that `cresca status` updates after partial approval.
#[test]
fn test_status_after_partial_approval() {
    let repo = TempGitRepo::new();

    repo.write_file("changed.txt", "line one\nline two\n");
    repo.git(&["add", "."]);
    repo.commit("Add status base file");
    repo.git(&["push", "origin", "main"]);

    repo.create_branch("develop");
    repo.write_file("changed.txt", "line one\nchanged line two\n");
    repo.write_file("added.txt", "added line one\nadded line two\n");
    repo.git(&["add", "."]);
    repo.commit("Add status changes");
    repo.git(&["push", "-u", "origin", "develop"]);

    repo.switch_branch("main");
    let review_output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        review_output.status.success(),
        "cresca review should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&review_output.stdout),
        String::from_utf8_lossy(&review_output.stderr)
    );

    repo.git(&["add", "added.txt"]);
    let approve_output = repo.run_cresca(&["approve"]);
    assert!(
        approve_output.status.success(),
        "partial cresca approve should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&approve_output.stdout),
        String::from_utf8_lossy(&approve_output.stderr)
    );

    let output = repo.run_cresca(&["status"]);
    assert!(
        output.status.success(),
        "cresca status should succeed after partial approval\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("status stdout should be UTF-8"),
        concat!(
            "📋 Review status:\n",
            "  Remaining diff to develop: 1 file(s), +1 insertion(s), -1 deletion(s)\n",
            "  Files remaining:\n",
            "    - changed.txt\n",
        )
    );
    assert!(output.stderr.is_empty());

    let rereview_output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        rereview_output.status.success(),
        "cresca re-review should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&rereview_output.stdout),
        String::from_utf8_lossy(&rereview_output.stderr)
    );
    repo.git(&["add", "."]);
    let final_approve_output = repo.run_cresca(&["approve"]);
    assert!(
        final_approve_output.status.success(),
        "final cresca approve should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&final_approve_output.stdout),
        String::from_utf8_lossy(&final_approve_output.stderr)
    );

    let output = repo.run_cresca(&["status"]);
    assert!(
        output.status.success(),
        "cresca status should succeed after all changes are approved\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("status stdout should be UTF-8"),
        concat!(
            "📋 Review status:\n",
            "  Remaining diff to develop: 0 file(s), +0 insertion(s), -0 deletion(s)\n",
        )
    );
    assert!(output.stderr.is_empty());
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

    repo.switch_branch("main");
    let before = repo.snapshot();

    let output = repo.run_cresca(&["review", "main", "develop", "--stop-at", "invalidhash"]);

    assert!(
        !output.status.success(),
        "cresca review --stop-at with invalid hash should fail"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalidhash") && stderr.contains("is not in the range"),
        "expected invalid range diagnostic, got: {stderr}"
    );
    assert_eq!(repo.snapshot(), before);
    assert!(!repo.ref_exists("refs/heads/review-main-develop"));
}

#[test]
fn test_review_with_nonexistent_skip_to_is_atomic() {
    let (repo, _) = setup_linear_range();
    let before = repo.snapshot();

    let output = repo.run_cresca(&["review", "main", "develop", "--skip-to", "does-not-exist"]);

    assert!(
        !output.status.success(),
        "cresca review --skip-to with a nonexistent revision should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("does-not-exist") && stderr.contains("is not in the range"),
        "expected invalid range diagnostic, got: {stderr}"
    );
    assert_eq!(repo.snapshot(), before);
    assert!(!repo.ref_exists("refs/heads/review-main-develop"));
}

#[test]
fn test_review_with_out_of_range_skip_to_is_atomic() {
    let (repo, _) = setup_linear_range();
    repo.create_branch("unrelated");
    repo.write_file("unrelated.txt", "outside the review range\n");
    repo.git(&["add", "."]);
    repo.commit("Add unrelated commit");
    let unrelated_commit = repo.rev_parse("HEAD");
    repo.switch_branch("main");
    let before = repo.snapshot();

    let output = repo.run_cresca(&["review", "main", "develop", "--skip-to", &unrelated_commit]);

    assert!(
        !output.status.success(),
        "cresca review --skip-to with an out-of-range revision should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&unrelated_commit) && stderr.contains("is not in the range"),
        "expected invalid range diagnostic, got: {stderr}"
    );
    assert_eq!(repo.snapshot(), before);
    assert!(!repo.ref_exists("refs/heads/review-main-develop"));
}

/// Test that `cresca review` fails when --stop-at is before --skip-to.
#[test]
fn test_review_with_stop_at_before_skip_to() {
    let (repo, range) = setup_linear_range();
    let before = repo.snapshot();

    let output = repo.run_cresca(&[
        "review",
        "main",
        "develop",
        "--skip-to",
        &range.c,
        "--stop-at",
        &range.a,
    ]);

    assert!(
        !output.status.success(),
        "cresca review should fail when --stop-at is before --skip-to"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("must be at or after"),
        "expected reversed range diagnostic, got: {stderr}"
    );
    assert_eq!(repo.snapshot(), before);
    assert!(!repo.ref_exists("refs/heads/review-main-develop"));
}

/// Test that `cresca review` records the exact CLI target and source values.
#[test]
fn test_review_records_versioned_target_and_source_metadata() {
    let repo = TempGitRepo::new();

    repo.create_branch("release-v1");
    repo.write_file("release.txt", "release base\n");
    repo.git(&["add", "."]);
    repo.commit("Add release base");
    repo.git(&["push", "-u", "origin", "release-v1"]);

    repo.create_branch("feature/login-page");
    repo.write_file("login.txt", "login page\n");
    repo.git(&["add", "."]);
    repo.commit("Add login page");
    repo.git(&["push", "-u", "origin", "feature/login-page"]);
    repo.switch_branch("main");

    let output = repo.run_cresca(&["review", "release-v1", "feature/login-page"]);
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        repo.current_branch(),
        "review-release-v1-feature_login-page"
    );
    assert_eq!(
        repo.review_metadata_values("review-release-v1-feature_login-page"),
        (
            vec!["1".to_string()],
            vec!["release-v1".to_string()],
            vec!["feature/login-page".to_string()],
        )
    );
    assert!(repo.cached_diff().is_empty());
    assert_eq!(
        repo.worktree_diff(),
        repo.diff(
            &repo.git_stdout(&[
                "merge-base",
                "origin/release-v1",
                "origin/feature/login-page",
            ]),
            "origin/feature/login-page",
        )
    );
}

/// Test that `cresca review` works with branch names containing slashes.
#[test]
fn test_review_with_slash_in_branch_name() {
    let repo = TempGitRepo::new();
    repo.create_branch("feature/login-page");
    repo.write_file("login.txt", "login stuff\n");
    repo.git(&["add", "."]);
    repo.commit("Add login");
    repo.git(&["push", "-u", "origin", "feature/login-page"]);

    repo.write_file("local-source-decoy.txt", "must not be reviewed\n");
    repo.git(&["add", "."]);
    repo.commit("Add unpushed source decoy");

    repo.switch_branch("main");
    repo.write_file(
        "local-target-decoy.txt",
        "must not become the review base\n",
    );
    repo.git(&["add", "."]);
    repo.commit("Add unpushed target decoy");
    let output = repo.run_cresca(&["review", "main", "feature/login-page"]);
    assert!(
        output.status.success(),
        "cresca review should succeed with slash in branch name\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_review_matches(
        &repo,
        "review-main-feature_login-page",
        "origin/main",
        "origin/feature/login-page",
    );
    assert_eq!(repo.read_file("login.txt"), "login stuff\n");
    assert!(!repo.path().join("local-source-decoy.txt").exists());
    assert!(!repo.path().join("local-target-decoy.txt").exists());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Review branch prepared successfully"));
    assert!(output.stderr.is_empty());
}

/// Test that `cresca review` works when the local branch does not exist.
#[test]
fn test_review_without_local_branch() {
    let repo = TempGitRepo::new();
    // Simulate another user pushing a branch
    repo.create_branch("other-users-feature");
    repo.write_file("other.txt", "other stuff\n");
    repo.git(&["add", "."]);
    repo.commit("Add other stuff");
    repo.git(&["push", "-u", "origin", "other-users-feature"]);

    // Switch to main and completely delete the local branch
    repo.switch_branch("main");
    repo.git(&["branch", "-D", "other-users-feature"]);
    assert!(!repo.ref_exists("refs/heads/other-users-feature"));
    repo.write_file(
        "local-target-decoy.txt",
        "must not become the review base\n",
    );
    repo.git(&["add", "."]);
    repo.commit("Add unpushed target decoy");

    let output = repo.run_cresca(&["review", "main", "other-users-feature"]);
    assert!(
        output.status.success(),
        "cresca review should succeed even if local branch does not exist\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_review_matches(
        &repo,
        "review-main-other-users-feature",
        "origin/main",
        "origin/other-users-feature",
    );
    assert_eq!(repo.read_file("other.txt"), "other stuff\n");
    assert!(!repo.path().join("local-target-decoy.txt").exists());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Review branch prepared successfully"));
    assert!(output.stderr.is_empty());
}

/// Test that `cresca review` works with a custom remote name (e.g. upstream).
#[test]
fn test_review_with_custom_remote() {
    // This is tricky with TempGitRepo since it sets up 'origin' by default.
    // We will rename the remote to 'upstream' for this test.
    let repo = TempGitRepo::new();
    repo.git(&["remote", "rename", "origin", "upstream"]);

    repo.create_branch("develop");
    repo.write_file("dev.txt", "dev stuff\n");
    repo.git(&["add", "."]);
    repo.commit("Add dev stuff");
    repo.git(&["push", "-u", "upstream", "develop"]);

    let wrong_remote = tempfile::TempDir::new().expect("wrong remote should be creatable");
    repo.git(&["init", "--bare", wrong_remote.path().to_str().unwrap()]);
    repo.git(&[
        "remote",
        "add",
        "origin",
        wrong_remote.path().to_str().unwrap(),
    ]);
    repo.write_file("origin-source-decoy.txt", "must not be reviewed\n");
    repo.git(&["add", "."]);
    repo.commit("Add origin-only source decoy");
    repo.git(&["push", "origin", "develop"]);
    repo.write_file("local-source-decoy.txt", "must not be reviewed\n");
    repo.git(&["add", "."]);
    repo.commit("Add unpushed source decoy");

    repo.switch_branch("main");
    repo.git(&["push", "-u", "upstream", "main"]);
    repo.write_file(
        "origin-target-decoy.txt",
        "must not become the review base\n",
    );
    repo.git(&["add", "."]);
    repo.commit("Add origin-only target decoy");
    repo.git(&["push", "origin", "main"]);
    repo.write_file(
        "local-target-decoy.txt",
        "must not become the review base\n",
    );
    repo.git(&["add", "."]);
    repo.commit("Add unpushed target decoy");
    let output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        output.status.success(),
        "cresca review should succeed with a custom remote named 'upstream'\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_review_matches(
        &repo,
        "review-main-develop",
        "upstream/main",
        "upstream/develop",
    );
    assert_eq!(repo.read_file("dev.txt"), "dev stuff\n");
    assert!(!repo.path().join("origin-source-decoy.txt").exists());
    assert!(!repo.path().join("local-source-decoy.txt").exists());
    assert!(!repo.path().join("origin-target-decoy.txt").exists());
    assert!(!repo.path().join("local-target-decoy.txt").exists());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Review branch prepared successfully"));
    assert!(output.stderr.is_empty());
}

/// Test that `cresca review` works when given remote tracking branch explicitly.
#[test]
fn test_review_with_explicit_remote_tracking_branch() {
    let repo = TempGitRepo::new();
    repo.create_branch("develop");
    repo.write_file("dev.txt", "dev stuff\n");
    repo.git(&["add", "."]);
    repo.commit("Add dev stuff");
    repo.git(&["push", "-u", "origin", "develop"]);

    repo.write_file("local-source-decoy.txt", "must not be reviewed\n");
    repo.git(&["add", "."]);
    repo.commit("Add unpushed source decoy");

    repo.switch_branch("main");
    repo.write_file(
        "local-target-decoy.txt",
        "must not become the review base\n",
    );
    repo.git(&["add", "."]);
    repo.commit("Add unpushed target decoy");
    // Pass 'origin/develop' instead of 'develop'
    let output = repo.run_cresca(&["review", "main", "origin/develop"]);
    assert!(
        output.status.success(),
        "cresca review should succeed with explicit remote tracking branch\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_review_matches(
        &repo,
        "review-main-origin_develop",
        "origin/main",
        "origin/develop",
    );
    assert_eq!(repo.read_file("dev.txt"), "dev stuff\n");
    assert!(!repo.path().join("local-source-decoy.txt").exists());
    assert!(!repo.path().join("local-target-decoy.txt").exists());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Review branch prepared successfully"));
    assert!(output.stderr.is_empty());
}
