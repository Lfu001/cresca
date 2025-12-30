mod common;

use common::TempGitRepo;

/// Test that `cresca review` creates a review branch with the correct name.
#[test]
fn test_review_creates_branch() {
    let repo = TempGitRepo::new();

    // Create a develop branch with some changes
    repo.create_branch("develop");
    repo.write_file("feature.txt", "new feature");
    repo.git(&["add", "."]);
    repo.commit("Add feature");
    repo.git(&["push", "-u", "origin", "develop"]);

    // Switch back to main
    repo.switch_branch("main");

    // Run cresca review
    let output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        output.status.success(),
        "cresca review should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify we're now on the review branch
    let current = repo.current_branch();
    assert_eq!(current, "review-main-develop");
}

/// Test that `cresca review` shows the diff as unstaged changes.
#[test]
fn test_review_shows_diff() {
    let repo = TempGitRepo::new();

    // Create a develop branch with some changes
    repo.create_branch("develop");
    repo.write_file("feature.txt", "new feature content");
    repo.git(&["add", "."]);
    repo.commit("Add feature");
    repo.git(&["push", "-u", "origin", "develop"]);

    // Switch back to main
    repo.switch_branch("main");

    // Run cresca review
    repo.run_cresca(&["review", "main", "develop"]);

    // Verify the changes are shown as unstaged
    assert!(
        repo.has_uncommitted_changes(),
        "Should have uncommitted changes"
    );

    // Check status for new files
    let status = repo.git(&["status", "--porcelain"]);
    let status_str = String::from_utf8_lossy(&status.stdout);
    assert!(
        status_str.contains("feature.txt"),
        "feature.txt should appear in status"
    );
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

    // Get the hash of second commit (file2)
    let log_output = repo.git(&["log", "--oneline", "main..develop"]);
    let log_str = String::from_utf8_lossy(&log_output.stdout);
    let commits: Vec<&str> = log_str.lines().collect();
    // commits[0] = file3, commits[1] = file2, commits[2] = file1
    let file2_hash = commits[1].split_whitespace().next().unwrap();

    // Switch back to main
    repo.switch_branch("main");

    // Run cresca review with --skip-to option (skip to file2 commit)
    let output = repo.run_cresca(&["review", "main", "develop", "--skip-to", file2_hash]);
    assert!(
        output.status.success(),
        "cresca review --skip-to should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify: file1.txt should be auto-approved (committed)
    let files_in_head = repo.git(&["ls-tree", "--name-only", "HEAD"]);
    let files_str = String::from_utf8_lossy(&files_in_head.stdout);
    assert!(
        files_str.contains("file1.txt"),
        "file1.txt should be auto-approved and committed"
    );

    // Verify: file2.txt and file3.txt should be unstaged changes
    let status = repo.git(&["status", "--porcelain"]);
    let status_str = String::from_utf8_lossy(&status.stdout);
    assert!(
        status_str.contains("file2.txt"),
        "file2.txt should be an unstaged change"
    );
    assert!(
        status_str.contains("file3.txt"),
        "file3.txt should be an unstaged change"
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
