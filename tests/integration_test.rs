mod common;

use common::TempGitRepo;
use std::collections::BTreeSet;

struct RemoveFileOnDrop(std::path::PathBuf);

impl Drop for RemoveFileOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

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

fn run_status_stdout(repo: &TempGitRepo, args: &[&str]) -> String {
    let output = repo.run_cresca(args);
    assert!(
        output.status.success(),
        "cresca {} failed\nstdout: {}\nstderr: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    String::from_utf8(output.stdout).expect("status stdout should be UTF-8")
}

fn setup_identity_only_review_branch() -> TempGitRepo {
    let repo = TempGitRepo::new();
    repo.create_branch("develop");
    repo.write_file("develop.txt", "develop content\n");
    repo.git(&["add", "."]);
    repo.commit("Add develop content");
    repo.git(&["push", "-u", "origin", "develop"]);
    repo.switch_branch("main");
    repo.create_branch("review-main-develop");
    repo.git(&[
        "config",
        "--local",
        "branch.review-main-develop.cresca-version",
        "1",
    ]);
    repo.git(&[
        "config",
        "--local",
        "branch.review-main-develop.cresca-target",
        "main",
    ]);
    repo.git(&[
        "config",
        "--local",
        "branch.review-main-develop.cresca-source",
        "develop",
    ]);
    repo
}

#[test]
fn test_review_records_source_tip_as_full_scope_end_without_stop_at() {
    let (repo, range) = setup_linear_range();
    let output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        repo.review_scope_values("review-main-develop"),
        vec![format!("1:{}", range.d)]
    );
}

#[test]
fn test_review_records_stop_at_as_full_scope_end() {
    let (repo, range) = setup_linear_range();
    let output = repo.run_cresca(&["review", "main", "develop", "--stop-at", &range.c[..8]]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        repo.review_scope_values("review-main-develop"),
        vec![format!("1:{}", range.c)]
    );
}

#[test]
fn test_review_scope_end_is_independent_of_skip_to() {
    let (repo, range) = setup_linear_range();
    let output = repo.run_cresca(&[
        "review",
        "main",
        "develop",
        "--skip-to",
        &range.b[..8],
        "--stop-at",
        &range.c[..8],
    ]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        repo.review_scope_values("review-main-develop"),
        vec![format!("1:{}", range.c)]
    );
}

#[test]
fn test_successful_rereview_replaces_scope_end() {
    let (repo, range) = setup_linear_range();
    assert!(repo
        .run_cresca(&["review", "main", "develop", "--stop-at", &range.c[..8],])
        .status
        .success());
    repo.git(&["add", "-A"]);
    assert!(repo.run_cresca(&["approve"]).status.success());
    let output = repo.run_cresca(&["review", "main", "develop", "--stop-at", &range.d[..8]]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        repo.review_scope_values("review-main-develop"),
        vec![format!("1:{}", range.d)]
    );
}

#[test]
fn test_failed_rereview_preserves_previous_scope_end() {
    let (repo, range) = setup_linear_range();
    assert!(repo
        .run_cresca(&["review", "main", "develop", "--stop-at", &range.c[..8],])
        .status
        .success());
    repo.git(&["add", "-A"]);
    assert!(repo.run_cresca(&["approve"]).status.success());
    let before = repo.review_scope_values("review-main-develop");
    assert_eq!(before, vec![format!("1:{}", range.c)]);
    let output = repo.run_cresca(&["review", "main", "develop", "--stop-at", "does-not-exist"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("does-not-exist") && stderr.contains("is not in the range"));
    assert_eq!(repo.review_scope_values("review-main-develop"), before);
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

fn assert_identity_suffixed_branch(branch: &str, base: &str) {
    let suffix = branch
        .strip_prefix(&format!("{base}-"))
        .expect("review branch should retain the readable base name");
    assert_eq!(suffix.len(), 16);
    assert!(
        suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "identity suffix should be 16 lowercase hexadecimal characters: {branch}"
    );
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

fn assert_invalid_review_command_preserves_state(repo: &TempGitRepo, args: &[&str]) {
    let before = repo.snapshot();
    let output = repo.run_cresca(args);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("valid cresca review branch"));
    assert_eq!(repo.snapshot(), before);
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
    assert_invalid_review_command_preserves_state(&repo, &["approve"]);
}

#[test]
fn test_approve_rejects_review_prefixed_branch_without_metadata_and_preserves_state() {
    let repo = TempGitRepo::new();
    repo.create_branch("review-not-created-by-cresca");
    prepare_staged_unstaged_and_untracked_changes(&repo);
    assert_invalid_review_command_preserves_state(&repo, &["approve"]);
}

#[test]
fn test_approve_rejects_unsupported_review_metadata_and_preserves_state() {
    let repo = TempGitRepo::new();
    repo.create_branch("review-corrupt");
    repo.git(&[
        "config",
        "--local",
        "branch.review-corrupt.cresca-target",
        "main",
    ]);
    repo.git(&[
        "config",
        "--local",
        "branch.review-corrupt.cresca-source",
        "develop",
    ]);
    repo.git(&[
        "config",
        "--local",
        "branch.review-corrupt.cresca-version",
        "999",
    ]);
    prepare_staged_unstaged_and_untracked_changes(&repo);
    assert_invalid_review_command_preserves_state(&repo, &["approve"]);
}

#[test]
fn test_approve_rejects_non_review_name_even_with_valid_metadata() {
    let repo = TempGitRepo::new();
    repo.git(&["config", "--local", "branch.main.cresca-version", "1"]);
    repo.git(&["config", "--local", "branch.main.cresca-target", "main"]);
    repo.git(&["config", "--local", "branch.main.cresca-source", "develop"]);
    prepare_staged_unstaged_and_untracked_changes(&repo);
    assert_invalid_review_command_preserves_state(&repo, &["approve"]);
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
    assert_eq!(
        repo.review_metadata_values("review-main-develop"),
        (Vec::new(), Vec::new(), Vec::new())
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

#[test]
fn test_failed_review_preparation_does_not_commit_review_metadata() {
    let (repo, range) = setup_linear_range();
    repo.git(&["config", "user.useConfigOnly", "true"]);
    repo.git(&["config", "--unset-all", "user.email"]);

    let output = std::process::Command::new(TempGitRepo::cresca_binary())
        .args(["review", "main", "develop", "--skip-to", &range.b[..8]])
        .env("NO_COLOR", "1")
        .env("LC_ALL", "C")
        .env("LANG", "C")
        // Isolate a possible global identity so the commit failure is reproducible.
        .env(
            "GIT_CONFIG_GLOBAL",
            repo.path().join("missing-global-gitconfig"),
        )
        .env("GIT_CONFIG_NOSYSTEM", "1")
        // GIT_CONFIG_COUNT is the authority for indexed KEY/VALUE pairs; without
        // it, inherited GIT_CONFIG_KEY_n/GIT_CONFIG_VALUE_n entries are ignored.
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_AUTHOR_NAME")
        .env_remove("GIT_AUTHOR_EMAIL")
        .env_remove("GIT_COMMITTER_NAME")
        .env_remove("GIT_COMMITTER_EMAIL")
        .env_remove("EMAIL")
        .current_dir(repo.path())
        .output()
        .expect("Failed to execute cresca");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("commit auto-approved changes")
            && stderr.contains("Author identity unknown"),
        "expected failed Git commit diagnostic, got: {stderr}"
    );
    assert_eq!(
        repo.review_metadata_values("review-main-develop"),
        (Vec::new(), Vec::new(), Vec::new()),
        "a failed materialization must not be marked as valid review state",
    );
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
            "📋 Review status (current range):\n",
            "  Remaining diff in current review range: 2 file(s), +3 insertion(s), -1 deletion(s)\n",
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
    assert_invalid_review_command_preserves_state(&repo, &["status"]);
}

#[test]
fn test_status_rejects_parseable_legacy_branch_without_metadata_and_preserves_state() {
    let repo = TempGitRepo::new();
    repo.create_branch("develop");
    repo.write_file("develop.txt", "develop content\n");
    repo.git(&["add", "."]);
    repo.commit("Add develop content");
    repo.git(&["push", "-u", "origin", "develop"]);
    repo.switch_branch("main");
    repo.create_branch("review-main-develop");
    prepare_staged_unstaged_and_untracked_changes(&repo);
    assert_invalid_review_command_preserves_state(&repo, &["status"]);
}

#[test]
fn test_status_rejects_duplicate_review_metadata_and_preserves_state() {
    let repo = TempGitRepo::new();
    repo.create_branch("develop");
    repo.write_file("develop.txt", "develop content\n");
    repo.git(&["add", "."]);
    repo.commit("Add develop content");
    repo.git(&["push", "-u", "origin", "develop"]);
    repo.switch_branch("main");
    repo.create_branch("review-main-develop");
    repo.git(&[
        "config",
        "--local",
        "branch.review-main-develop.cresca-version",
        "1",
    ]);
    repo.git(&[
        "config",
        "--local",
        "branch.review-main-develop.cresca-target",
        "main",
    ]);
    repo.git(&[
        "config",
        "--local",
        "--add",
        "branch.review-main-develop.cresca-source",
        "develop",
    ]);
    repo.git(&[
        "config",
        "--local",
        "--add",
        "branch.review-main-develop.cresca-source",
        "other-develop",
    ]);
    assert_invalid_review_command_preserves_state(&repo, &["status"]);
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
            "📋 Review status (current range):\n",
            "  Remaining diff in current review range: 1 file(s), +1 insertion(s), -1 deletion(s)\n",
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
            "📋 Review status (current range):\n",
            "  Remaining diff in current review range: 0 file(s), +0 insertion(s), -0 deletion(s)\n",
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
    assert_eq!(
        repo.review_metadata_values("review-main-develop"),
        (Vec::new(), Vec::new(), Vec::new())
    );
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
    assert_eq!(
        repo.review_metadata_values("review-main-develop"),
        (Vec::new(), Vec::new(), Vec::new())
    );
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
    assert_eq!(
        repo.review_metadata_values("review-main-develop"),
        (Vec::new(), Vec::new(), Vec::new())
    );
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
    assert_eq!(
        repo.review_metadata_values("review-main-develop"),
        (Vec::new(), Vec::new(), Vec::new())
    );
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

#[test]
fn test_review_treats_orphan_base_metadata_as_occupied() {
    let repo = TempGitRepo::new();

    repo.create_branch("develop");
    repo.write_file("develop.txt", "develop change\n");
    repo.git(&["add", "."]);
    repo.commit("Add develop change");
    repo.git(&["push", "-u", "origin", "develop"]);
    repo.switch_branch("main");

    let base = "review-main-develop";
    repo.git(&[
        "config",
        "--local",
        &format!("branch.{base}.cresca-version"),
        "1",
    ]);
    repo.git(&[
        "config",
        "--local",
        &format!("branch.{base}.cresca-target"),
        "other-target",
    ]);
    repo.git(&[
        "config",
        "--local",
        &format!("branch.{base}.cresca-source"),
        "other-source",
    ]);
    let orphan_metadata = repo.review_metadata_values(base);

    let output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let suffix = "review-main-develop-5ee67b20f1cad176";
    assert_eq!(repo.current_branch(), suffix);
    assert!(!repo.ref_exists(&format!("refs/heads/{base}")));
    assert_eq!(repo.review_metadata_values(base), orphan_metadata);
    assert_eq!(
        repo.review_metadata_values(suffix),
        (
            vec!["1".to_string()],
            vec!["main".to_string()],
            vec!["develop".to_string()],
        )
    );
    assert!(repo.cached_diff().is_empty());
    let merge_base = repo.git_stdout(&["merge-base", "origin/main", "origin/develop"]);
    assert_eq!(
        repo.worktree_diff(),
        repo.diff(&merge_base, "origin/develop")
    );
}

#[test]
fn test_review_fails_closed_when_orphan_metadata_occupies_suffix() {
    let repo = TempGitRepo::new();

    repo.create_branch("develop");
    repo.write_file("develop.txt", "develop change\n");
    repo.git(&["add", "."]);
    repo.commit("Add develop change");
    repo.git(&["push", "-u", "origin", "develop"]);
    repo.switch_branch("main");

    let base = "review-main-develop";
    repo.create_branch(base);
    repo.switch_branch("main");
    let suffix = "review-main-develop-5ee67b20f1cad176";
    repo.git(&[
        "config",
        "--local",
        &format!("branch.{suffix}.cresca-source"),
        "orphan-source",
    ]);
    let before = repo.snapshot();

    let output = repo.run_cresca(&["review", "main", "develop"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("conflicting review branches")
            && stderr.contains(base)
            && stderr.contains(suffix),
        "expected orphan suffix conflict diagnostic, got: {stderr}"
    );
    assert_eq!(repo.snapshot(), before);
    assert!(!repo.ref_exists(&format!("refs/heads/{suffix}")));
}

#[cfg(unix)]
#[test]
fn test_review_fails_closed_when_metadata_config_read_fails() {
    use std::os::unix::fs::PermissionsExt;

    let repo = TempGitRepo::new();

    repo.create_branch("develop");
    repo.write_file("develop.txt", "develop change\n");
    repo.git(&["add", "."]);
    repo.commit("Add develop change");
    repo.git(&["push", "-u", "origin", "develop"]);
    repo.switch_branch("main");
    repo.create_branch("review-main-develop");
    repo.switch_branch("main");
    let before = repo.snapshot();

    let git_path = String::from_utf8(
        std::process::Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .expect("git path lookup should execute")
            .stdout,
    )
    .expect("git path should be UTF-8")
    .trim()
    .to_string();
    let shim_dir = tempfile::TempDir::new().expect("git shim directory should be created");
    let shim_path = shim_dir.path().join("git");
    let shim = format!(
        "#!/bin/sh\nif [ \"$1\" = config ] && [ \"$2\" = --local ] && [ \"$3\" = --get-all ] && [ \"$4\" = branch.review-main-develop.cresca-version ]; then\n  echo injected metadata read failure >&2\n  exit 2\nfi\nexec '{git_path}' \"$@\"\n"
    );
    std::fs::write(&shim_path, shim).expect("git shim should be writable");
    let mut permissions = std::fs::metadata(&shim_path)
        .expect("git shim metadata should be readable")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&shim_path, permissions).expect("git shim should be executable");
    let path = std::env::join_paths(std::iter::once(shim_dir.path().to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .expect("git shim PATH should be valid");

    let output = std::process::Command::new(TempGitRepo::cresca_binary())
        .args(["review", "main", "develop"])
        .env("NO_COLOR", "1")
        .env("PATH", path)
        .current_dir(repo.path())
        .output()
        .expect("cresca should execute with git shim");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Failed to read review metadata")
            && stderr.contains("injected metadata read failure"),
        "expected strict metadata read failure, got: {stderr}"
    );
    assert_eq!(repo.snapshot(), before);
}

#[test]
fn test_review_does_not_materialize_orphan_metadata_when_config_write_fails() {
    let repo = TempGitRepo::new();

    repo.create_branch("develop");
    repo.write_file("develop.txt", "develop change\n");
    repo.git(&["add", "."]);
    repo.commit("Add develop change");
    repo.git(&["push", "-u", "origin", "develop"]);
    repo.switch_branch("main");

    let review_branch = "review-main-develop";
    repo.git(&[
        "config",
        "--local",
        &format!("branch.{review_branch}.cresca-version"),
        "1",
    ]);
    repo.git(&[
        "config",
        "--local",
        &format!("branch.{review_branch}.cresca-target"),
        "main",
    ]);
    repo.git(&[
        "config",
        "--local",
        &format!("branch.{review_branch}.cresca-source"),
        "develop",
    ]);
    let metadata_before = repo.review_metadata_values(review_branch);
    assert!(!repo.ref_exists(&format!("refs/heads/{review_branch}")));

    let config_lock_path = repo.path().join(".git/config.lock");
    std::fs::write(&config_lock_path, "lock metadata writes")
        .expect("config lock fixture should be writable");
    let config_lock_cleanup = RemoveFileOnDrop(config_lock_path);

    let output = repo.run_cresca(&["--verbose", "review", "main", "develop"]);
    drop(config_lock_cleanup);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Failed to record review target"),
        "expected metadata write failure, got: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains(&format!("git checkout -b {review_branch} ")),
        "orphan metadata must prevent the base branch from being created: {stdout}"
    );
    assert!(!repo.ref_exists(&format!("refs/heads/{review_branch}")));
    assert_eq!(repo.review_metadata_values(review_branch), metadata_before);

    let suffix = "review-main-develop-5ee67b20f1cad176";
    assert_eq!(
        repo.review_metadata_values(suffix),
        (Vec::new(), Vec::new(), Vec::new()),
        "a failed write must not leave a valid review identity on the new branch"
    );
}

#[test]
fn test_review_does_not_reuse_branch_for_slash_underscore_collision() {
    let repo = TempGitRepo::new();

    repo.create_branch("feature/foo");
    repo.write_file("slash.txt", "slash branch\n");
    repo.git(&["add", "."]);
    repo.commit("Add slash branch change");
    repo.git(&["push", "-u", "origin", "feature/foo"]);

    repo.switch_branch("main");
    repo.create_branch("feature_foo");
    repo.write_file("underscore.txt", "underscore branch\n");
    repo.git(&["add", "."]);
    repo.commit("Add underscore branch change");
    repo.git(&["push", "-u", "origin", "feature_foo"]);

    repo.switch_branch("main");
    let first = repo.run_cresca(&["review", "main", "feature/foo"]);
    assert!(first.status.success());
    assert_eq!(repo.current_branch(), "review-main-feature_foo");
    assert!(repo.run_cresca(&["approve"]).status.success());
    repo.switch_branch("main");
    let first_oid = repo.rev_parse("refs/heads/review-main-feature_foo");

    let second = repo.run_cresca(&["review", "main", "feature_foo"]);
    assert!(
        second.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    let second_branch = repo.current_branch();
    assert_ne!(second_branch, "review-main-feature_foo");
    assert_identity_suffixed_branch(&second_branch, "review-main-feature_foo");
    assert_eq!(
        repo.rev_parse("refs/heads/review-main-feature_foo"),
        first_oid
    );
    assert_eq!(
        repo.review_metadata_values(&second_branch),
        (
            vec!["1".to_string()],
            vec!["main".to_string()],
            vec!["feature_foo".to_string()],
        )
    );
    assert!(repo.cached_diff().is_empty());
    let merge_base = repo.git_stdout(&["merge-base", "origin/main", "origin/feature_foo"]);
    assert_eq!(
        repo.worktree_diff(),
        repo.diff(&merge_base, "origin/feature_foo")
    );
    assert!(!repo.path().join("slash.txt").exists());
    assert_eq!(repo.read_file("underscore.txt"), "underscore branch\n");
}

#[test]
fn test_review_does_not_reuse_branch_for_ambiguous_pair_boundary() {
    let repo = TempGitRepo::new();

    repo.create_branch("release");
    repo.git(&["push", "-u", "origin", "release"]);
    repo.create_branch("v1-feature");
    repo.write_file("pair-one.txt", "first pair\n");
    repo.git(&["add", "."]);
    repo.commit("Add first pair change");
    repo.git(&["push", "-u", "origin", "v1-feature"]);

    repo.switch_branch("main");
    repo.create_branch("release-v1");
    repo.write_file("release-v1-base.txt", "second target base\n");
    repo.git(&["add", "."]);
    repo.commit("Add second target base");
    repo.git(&["push", "-u", "origin", "release-v1"]);
    repo.create_branch("feature");
    repo.write_file("pair-two.txt", "second pair\n");
    repo.git(&["add", "."]);
    repo.commit("Add second pair change");
    repo.git(&["push", "-u", "origin", "feature"]);

    repo.switch_branch("main");
    let first = repo.run_cresca(&["review", "release", "v1-feature"]);
    assert!(first.status.success());
    assert_eq!(repo.current_branch(), "review-release-v1-feature");
    assert!(repo.run_cresca(&["approve"]).status.success());
    repo.switch_branch("main");
    let first_oid = repo.rev_parse("refs/heads/review-release-v1-feature");

    let second = repo.run_cresca(&["review", "release-v1", "feature"]);
    assert!(
        second.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    let second_branch = repo.current_branch();
    assert_ne!(second_branch, "review-release-v1-feature");
    assert_identity_suffixed_branch(&second_branch, "review-release-v1-feature");
    assert_eq!(
        repo.rev_parse("refs/heads/review-release-v1-feature"),
        first_oid
    );
    assert_eq!(
        repo.review_metadata_values(&second_branch),
        (
            vec!["1".to_string()],
            vec!["release-v1".to_string()],
            vec!["feature".to_string()],
        )
    );
    assert!(repo.cached_diff().is_empty());
    let merge_base = repo.git_stdout(&["merge-base", "origin/release-v1", "origin/feature"]);
    assert_eq!(
        repo.worktree_diff(),
        repo.diff(&merge_base, "origin/feature")
    );
    assert!(!repo.path().join("pair-one.txt").exists());
    assert_eq!(repo.read_file("pair-two.txt"), "second pair\n");
}

#[test]
fn test_review_leaves_legacy_branch_untouched_and_creates_metadata_backed_branch() {
    let repo = TempGitRepo::new();

    repo.create_branch("develop");
    repo.write_file("develop.txt", "develop change\n");
    repo.git(&["add", "."]);
    repo.commit("Add develop change");
    repo.git(&["push", "-u", "origin", "develop"]);

    repo.switch_branch("main");
    repo.create_branch("review-main-develop");
    let legacy_oid = repo.rev_parse("HEAD");
    assert_eq!(
        repo.review_metadata_values("review-main-develop"),
        (Vec::new(), Vec::new(), Vec::new())
    );
    repo.switch_branch("main");

    let first = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        first.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    let metadata_branch = repo.current_branch();
    assert_identity_suffixed_branch(&metadata_branch, "review-main-develop");
    assert_eq!(repo.rev_parse("refs/heads/review-main-develop"), legacy_oid);
    assert_eq!(
        repo.review_metadata_values("review-main-develop"),
        (Vec::new(), Vec::new(), Vec::new())
    );
    assert_eq!(
        repo.review_metadata_values(&metadata_branch),
        (
            vec!["1".to_string()],
            vec!["main".to_string()],
            vec!["develop".to_string()],
        )
    );
    assert!(repo.cached_diff().is_empty());
    let merge_base = repo.git_stdout(&["merge-base", "origin/main", "origin/develop"]);
    assert_eq!(
        repo.worktree_diff(),
        repo.diff(&merge_base, "origin/develop")
    );

    assert!(repo.run_cresca(&["approve"]).status.success());
    repo.switch_branch("main");
    let heads_before = repo
        .git(&[
            "for-each-ref",
            "--sort=refname",
            "--format=%(refname) %(objectname)",
            "refs/heads/",
        ])
        .stdout;

    let second = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        second.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(repo.current_branch(), metadata_branch);
    assert_eq!(
        repo.git(&[
            "for-each-ref",
            "--sort=refname",
            "--format=%(refname) %(objectname)",
            "refs/heads/",
        ])
        .stdout,
        heads_before
    );
}

#[test]
fn test_review_fails_atomically_when_base_and_identity_suffix_belong_to_other_reviews() {
    let repo = TempGitRepo::new();

    repo.create_branch("develop");
    repo.write_file("develop.txt", "develop change\n");
    repo.git(&["add", "."]);
    repo.commit("Add develop change");
    repo.git(&["push", "-u", "origin", "develop"]);

    repo.switch_branch("main");
    repo.create_branch("review-main-develop");
    repo.switch_branch("main");

    let first = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        first.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    let suffixed_branch = repo.current_branch();
    assert_identity_suffixed_branch(&suffixed_branch, "review-main-develop");
    assert!(repo.run_cresca(&["approve"]).status.success());

    repo.git(&[
        "config",
        "--local",
        &format!("branch.{suffixed_branch}.cresca-target"),
        "other-target",
    ]);
    repo.git(&[
        "config",
        "--local",
        &format!("branch.{suffixed_branch}.cresca-source"),
        "other-source",
    ]);
    assert_eq!(
        repo.git_config_values(&format!("branch.{suffixed_branch}.cresca-version")),
        vec!["1".to_string()]
    );

    repo.switch_branch("main");
    let before = repo.snapshot();
    let output = repo.run_cresca(&["review", "main", "develop"]);

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("conflicting review branches")
            && stderr.contains("review-main-develop")
            && stderr.contains(&suffixed_branch),
        "expected conflicting review branch diagnostic, got: {stderr}"
    );
    assert_eq!(repo.snapshot(), before);
}

#[test]
fn test_status_uses_metadata_for_hyphenated_target_and_slash_source() {
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

    let review_output = repo.run_cresca(&["review", "release-v1", "feature/login-page"]);
    assert!(
        review_output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&review_output.stdout),
        String::from_utf8_lossy(&review_output.stderr)
    );

    let output = repo.run_cresca(&["status"]);

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Remaining diff in current review range:"));
    assert!(stdout.contains("1 file(s), +1 insertion(s), -0 deletion(s)"));
    assert!(stdout.contains("    - login.txt\n"));
    assert!(output.stderr.is_empty());
}

fn assert_scope_error_and_all_available(repo: &TempGitRepo, expected_error: &str) {
    let before = repo.snapshot();
    let output = repo.run_cresca(&["status"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(expected_error),
        "unexpected diagnostic: {stderr}"
    );
    assert!(stderr.contains("cresca review main develop"));
    assert!(stderr.contains("cresca status --all"));
    assert_eq!(repo.snapshot(), before);

    let all = repo.run_cresca(&["status", "--all"]);
    assert!(
        all.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&all.stdout),
        String::from_utf8_lossy(&all.stderr)
    );
    assert!(String::from_utf8_lossy(&all.stdout).contains("    - develop.txt\n"));
    assert!(all.stderr.is_empty());
}

#[test]
fn test_status_requires_scope_metadata_but_all_works_for_pr12_identity() {
    let repo = setup_identity_only_review_branch();
    assert_scope_error_and_all_available(&repo, "range metadata is missing");
}

#[test]
fn test_status_rejects_duplicate_scope_values_but_all_still_works() {
    let repo = setup_identity_only_review_branch();
    let value = format!("1:{}", repo.rev_parse("HEAD"));
    repo.git(&[
        "config",
        "--local",
        "--add",
        "branch.review-main-develop.cresca-scope",
        &value,
    ]);
    repo.git(&[
        "config",
        "--local",
        "--add",
        "branch.review-main-develop.cresca-scope",
        &value,
    ]);
    assert_scope_error_and_all_available(&repo, "range metadata has duplicate values");
}

#[test]
fn test_status_rejects_unsupported_scope_version_but_all_still_works() {
    let repo = setup_identity_only_review_branch();
    let value = format!("2:{}", repo.rev_parse("HEAD"));
    repo.git(&[
        "config",
        "--local",
        "branch.review-main-develop.cresca-scope",
        &value,
    ]);
    assert_scope_error_and_all_available(&repo, "range metadata version '2' is unsupported");
}

#[test]
fn test_status_rejects_malformed_scope_oid_but_all_still_works() {
    let repo = setup_identity_only_review_branch();
    repo.git(&[
        "config",
        "--local",
        "branch.review-main-develop.cresca-scope",
        "1:not-an-oid",
    ]);
    assert_scope_error_and_all_available(&repo, "range metadata is invalid");
}

#[test]
fn test_status_rejects_abbreviated_scope_oid() {
    let repo = setup_identity_only_review_branch();
    let head = repo.rev_parse("HEAD");
    let value = format!("1:{}", &head[..8]);
    repo.git(&[
        "config",
        "--local",
        "branch.review-main-develop.cresca-scope",
        &value,
    ]);
    assert_scope_error_and_all_available(&repo, "range metadata is invalid");
}

#[test]
fn test_status_rejects_unavailable_scope_commit_but_all_still_works() {
    let repo = setup_identity_only_review_branch();
    let oid = "0000000000000000000000000000000000000000";
    let value = format!("1:{oid}");
    repo.git(&[
        "config",
        "--local",
        "branch.review-main-develop.cresca-scope",
        &value,
    ]);
    assert_scope_error_and_all_available(
        &repo,
        &format!("saved range endpoint '{oid}' is unavailable"),
    );
}

#[test]
fn test_status_keeps_unbounded_review_tip_fixed_until_next_review() {
    let (repo, _) = setup_linear_range();
    assert!(repo
        .run_cresca(&["review", "main", "develop"])
        .status
        .success());
    repo.git(&["add", "-A"]);
    assert!(repo.run_cresca(&["approve"]).status.success());
    repo.switch_branch("develop");
    repo.write_file("e.txt", "added at E\n");
    repo.git(&["add", "e.txt"]);
    repo.commit("E: add e.txt");
    repo.git(&["push", "origin", "develop"]);
    repo.switch_branch("review-main-develop");
    assert_eq!(
        run_status_stdout(&repo, &["status"]),
        concat!(
            "📋 Review status (current range):\n",
            "  Remaining diff in current review range: 0 file(s), +0 insertion(s), -0 deletion(s)\n",
        )
    );
    assert_eq!(
        run_status_stdout(&repo, &["status", "--all"]),
        concat!(
            "📋 Review status (full pull request):\n",
            "  Remaining diff to develop: 1 file(s), +1 insertion(s), -0 deletion(s)\n",
            "  Files remaining:\n",
            "    - e.txt\n",
        )
    );
}

#[test]
fn test_status_default_excludes_commits_after_stop_at_while_all_includes_them() {
    let (repo, range) = setup_linear_range();
    assert!(repo
        .run_cresca(&["review", "main", "develop", "--stop-at", &range.c[..8],])
        .status
        .success());
    let current = run_status_stdout(&repo, &["status"]);
    assert!(current.contains("📋 Review status (current range):"));
    assert!(current.contains("3 file(s), +2 insertion(s), -2 deletion(s)"));
    assert!(current.contains("    - a.txt\n"));
    assert!(current.contains("    - removed-at-c.txt\n"));
    assert!(current.contains("    - shared.txt\n"));
    assert!(!current.contains("    - d.txt\n"));
    let all = run_status_stdout(&repo, &["status", "--all"]);
    assert!(all.contains("📋 Review status (full pull request):"));
    assert!(all.contains("4 file(s), +3 insertion(s), -2 deletion(s)"));
    for file in ["a.txt", "d.txt", "removed-at-c.txt", "shared.txt"] {
        assert!(all.contains(&format!("    - {file}\n")));
    }
}

#[test]
fn test_status_after_skip_stop_and_partial_approve_excludes_auto_approved_changes() {
    let (repo, range) = setup_linear_range();
    assert!(repo
        .run_cresca(&[
            "review",
            "main",
            "develop",
            "--skip-to",
            &range.b[..8],
            "--stop-at",
            &range.c[..8],
        ])
        .status
        .success());
    repo.git(&["add", "shared.txt"]);
    assert!(repo.run_cresca(&["approve"]).status.success());
    let current = run_status_stdout(&repo, &["status"]);
    assert!(current.contains("1 file(s), +0 insertion(s), -1 deletion(s)"));
    assert!(current.contains("    - removed-at-c.txt\n"));
    assert!(!current.contains("a.txt"));
    assert!(!current.contains("d.txt"));
    let all = run_status_stdout(&repo, &["status", "--all"]);
    assert!(all.contains("2 file(s), +1 insertion(s), -1 deletion(s)"));
    assert!(all.contains("    - d.txt\n"));
    assert!(all.contains("    - removed-at-c.txt\n"));
    assert!(!all.contains("a.txt"));
}

#[test]
fn test_status_current_range_can_be_complete_while_full_pull_request_remains() {
    let (repo, range) = setup_linear_range();
    assert!(repo
        .run_cresca(&["review", "main", "develop", "--stop-at", &range.c[..8],])
        .status
        .success());
    repo.git(&["add", "-A"]);
    assert!(repo.run_cresca(&["approve"]).status.success());
    assert_eq!(
        run_status_stdout(&repo, &["status"]),
        concat!(
            "📋 Review status (current range):\n",
            "  Remaining diff in current review range: 0 file(s), +0 insertion(s), -0 deletion(s)\n",
        )
    );
    assert_eq!(
        run_status_stdout(&repo, &["status", "--all"]),
        concat!(
            "📋 Review status (full pull request):\n",
            "  Remaining diff to develop: 1 file(s), +1 insertion(s), -0 deletion(s)\n",
            "  Files remaining:\n",
            "    - d.txt\n",
        )
    );
}

#[test]
fn test_status_help_describes_all_scope() {
    let repo = TempGitRepo::new();
    let output = repo.run_cresca(&["status", "--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: cresca status [OPTIONS]"));
    assert!(stdout.contains("--all"));
    assert!(stdout.contains("full pull request"));
    assert!(output.stderr.is_empty());
}

#[test]
fn test_status_rejects_unknown_option() {
    let repo = TempGitRepo::new();
    let before = repo.snapshot();
    let output = repo.run_cresca(&["status", "--unknown"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument '--unknown'"));
    assert_eq!(repo.snapshot(), before);
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
