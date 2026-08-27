mod common;

use common::TempGitRepo;

#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::os::fd::FromRawFd;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::process::{Command, Output, Stdio};
#[cfg(unix)]
use std::sync::mpsc;
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use tempfile::TempDir;

#[cfg(unix)]
struct SlowGit {
    _directory: TempDir,
    path: OsString,
    real_path: OsString,
    marker: PathBuf,
}

#[cfg(unix)]
impl SlowGit {
    fn new() -> Self {
        let directory = TempDir::new().unwrap();
        let wrapper_path = directory.path().join("git");
        std::fs::write(
            &wrapper_path,
            "#!/bin/sh\nif [ ! -e \"$CRESCA_SLOW_GIT_MARKER\" ]; then\n  : > \"$CRESCA_SLOW_GIT_MARKER\"\n  sleep 0.35\nfi\nPATH=\"$CRESCA_REAL_PATH\" exec git \"$@\"\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&wrapper_path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&wrapper_path, permissions).unwrap();

        let mut path = directory.path().as_os_str().to_os_string();
        path.push(":");
        let real_path = std::env::var_os("PATH").unwrap();
        path.push(&real_path);
        let marker = directory.path().join("first-git-finished");

        Self {
            _directory: directory,
            path,
            real_path,
            marker,
        }
    }
}

#[cfg(unix)]
fn repo_with_reviewable_change(file_name: &str) -> TempGitRepo {
    let repo = TempGitRepo::new();
    repo.create_branch("develop");
    repo.write_file(file_name, "new feature");
    repo.git(&["add", "."]);
    repo.commit("Add feature");
    repo.git(&["push", "-u", "origin", "develop"]);
    repo.switch_branch("main");
    repo
}

#[cfg(unix)]
fn run_cresca_with_stderr_pty(repo: &TempGitRepo, args: &[&str], slow_git: &SlowGit) -> Output {
    run_cresca_with_stderr_pty_inner(repo, args, slow_git, false)
}

#[cfg(unix)]
fn run_cresca_with_stderr_pty_inner(
    repo: &TempGitRepo,
    args: &[&str],
    slow_git: &SlowGit,
    interrupt_when_progress_is_visible: bool,
) -> Output {
    let mut master_fd = -1;
    let mut slave_fd = -1;
    let result = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(result, 0, "failed to open a pseudo-terminal");

    let mut master = unsafe { File::from_raw_fd(master_fd) };
    let slave = unsafe { File::from_raw_fd(slave_fd) };
    let (progress_sender, progress_receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut output = Vec::new();
        let mut buffer = [0; 1024];
        let mut progress_reported = false;
        loop {
            match master.read(&mut buffer) {
                Ok(0) => break,
                Ok(bytes_read) => {
                    output.extend_from_slice(&buffer[..bytes_read]);
                    if !progress_reported
                        && output
                            .windows("Preparing review branch".len())
                            .any(|window| window == b"Preparing review branch")
                    {
                        progress_reported = true;
                        let _ = progress_sender.send(());
                    }
                }
                Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                Err(error) => panic!("failed to read pseudo-terminal: {error}"),
            }
        }
        output
    });

    let child = Command::new(TempGitRepo::cresca_binary())
        .args(args)
        .current_dir(repo.path())
        .env("PATH", &slow_git.path)
        .env("CRESCA_REAL_PATH", &slow_git.real_path)
        .env("CRESCA_SLOW_GIT_MARKER", &slow_git.marker)
        .stdout(Stdio::piped())
        .stderr(Stdio::from(slave))
        .spawn()
        .expect("Failed to execute cresca");
    if interrupt_when_progress_is_visible {
        progress_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("progress did not become visible before interrupt");
        let result = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
        assert_eq!(result, 0, "failed to interrupt cresca");
    }
    let output = child.wait_with_output().expect("Failed to wait for cresca");
    let stderr = reader.join().expect("pseudo-terminal reader panicked");

    Output {
        status: output.status,
        stdout: output.stdout,
        stderr,
    }
}

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

/// Test that progress remains visible on an interactive stderr while stdout is piped.
#[cfg(unix)]
#[test]
fn test_review_shows_progress_on_stderr_tty_when_stdout_is_piped() {
    let repo = repo_with_reviewable_change("feature.txt");

    let slow_git = SlowGit::new();
    let output = run_cresca_with_stderr_pty(&repo, &["review", "main", "develop"], &slow_git);

    assert!(
        output.status.success(),
        "cresca review should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stdout.contains("Review branch prepared successfully"));
    assert!(!stdout.contains("Preparing review branch"));
    assert!(stderr.contains("⠋ Preparing review branch"));
    assert!(stderr.ends_with("\r\x1b[2K"));
}

/// Test that verbose Git output replaces animated progress.
#[cfg(unix)]
#[test]
fn test_review_verbose_mode_does_not_show_progress() {
    let repo = repo_with_reviewable_change("feature.txt");

    let slow_git = SlowGit::new();
    let output = run_cresca_with_stderr_pty(
        &repo,
        &["--verbose", "review", "main", "develop"],
        &slow_git,
    );

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stdout.contains("[git status --porcelain]"));
    assert!(!stdout.contains("Preparing review branch"));
    assert!(!stderr.contains("Preparing review branch"));
    assert!(!stderr.contains("\x1b[2K"));
}

/// Test that captured stderr never receives animated progress or control sequences.
#[cfg(unix)]
#[test]
fn test_review_non_tty_stderr_does_not_show_progress() {
    let repo = repo_with_reviewable_change("feature.txt");

    let slow_git = SlowGit::new();
    let output = Command::new(TempGitRepo::cresca_binary())
        .args(["review", "main", "develop"])
        .current_dir(repo.path())
        .env("PATH", &slow_git.path)
        .env("CRESCA_REAL_PATH", &slow_git.real_path)
        .env("CRESCA_SLOW_GIT_MARKER", &slow_git.marker)
        .output()
        .expect("Failed to execute cresca");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stdout.contains("Preparing review branch"));
    assert!(!stderr.contains("Preparing review branch"));
    assert!(!stderr.contains("\x1b[2K"));
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

/// Test that `cresca approve` reports progress when its stderr is interactive.
#[cfg(unix)]
#[test]
fn test_approve_shows_progress_on_stderr_tty() {
    let repo = repo_with_reviewable_change("reviewed.txt");
    repo.run_cresca(&["review", "main", "develop"]);
    repo.git(&["add", "reviewed.txt"]);

    let slow_git = SlowGit::new();
    let output = run_cresca_with_stderr_pty(&repo, &["approve"], &slow_git);

    assert!(
        output.status.success(),
        "cresca approve should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stdout.contains("Reviewed changes were approved successfully"));
    assert!(stderr.contains("⠋ Approving reviewed changes"));
    assert!(stderr.ends_with("\r\x1b[2K"));
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

/// Test that `cresca status` reports progress when its stderr is interactive.
#[cfg(unix)]
#[test]
fn test_status_shows_progress_on_stderr_tty() {
    let repo = repo_with_reviewable_change("feature.txt");
    repo.run_cresca(&["review", "main", "develop"]);

    let slow_git = SlowGit::new();
    let output = run_cresca_with_stderr_pty(&repo, &["status"], &slow_git);

    assert!(
        output.status.success(),
        "cresca status should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stdout.contains("Review status"));
    assert!(stderr.contains("⠋ Checking review status"));
    assert!(stderr.ends_with("\r\x1b[2K"));
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

    // Run cresca review with --stop-at option (stop at file2 commit)
    let output = repo.run_cresca(&["review", "main", "develop", "--stop-at", file2_hash]);
    assert!(
        output.status.success(),
        "cresca review --stop-at should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify: file1.txt and file2.txt should be unstaged changes (in review)
    let status = repo.git(&["status", "--porcelain"]);
    let status_str = String::from_utf8_lossy(&status.stdout);
    assert!(
        status_str.contains("file1.txt"),
        "file1.txt should be an unstaged change"
    );
    assert!(
        status_str.contains("file2.txt"),
        "file2.txt should be an unstaged change"
    );

    // Verify: file3.txt should NOT appear (excluded from review)
    assert!(
        !status_str.contains("file3.txt"),
        "file3.txt should NOT appear (excluded by --stop-at)"
    );
}

/// Test that `cresca review --skip-to --stop-at` limits review to specific range.
#[test]
fn test_review_with_skip_to_and_stop_at() {
    let repo = TempGitRepo::new();

    // Create develop branch with multiple commits: A -> B -> C -> D
    repo.create_branch("develop");
    repo.write_file("fileA.txt", "content A");
    repo.git(&["add", "."]);
    repo.commit("Add fileA");

    repo.write_file("fileB.txt", "content B");
    repo.git(&["add", "."]);
    repo.commit("Add fileB");

    repo.write_file("fileC.txt", "content C");
    repo.git(&["add", "."]);
    repo.commit("Add fileC");

    repo.write_file("fileD.txt", "content D");
    repo.git(&["add", "."]);
    repo.commit("Add fileD");

    repo.git(&["push", "-u", "origin", "develop"]);

    // Get commit hashes
    // Log order: D (newest) -> C -> B -> A (oldest)
    let log_output = repo.git(&["log", "--oneline", "main..develop"]);
    let log_str = String::from_utf8_lossy(&log_output.stdout);
    let commits: Vec<&str> = log_str.lines().collect();
    let _file_d_hash = commits[0].split_whitespace().next().unwrap();
    let file_c_hash = commits[1].split_whitespace().next().unwrap();
    let file_b_hash = commits[2].split_whitespace().next().unwrap();
    let _file_a_hash = commits[3].split_whitespace().next().unwrap();

    // Switch back to main
    repo.switch_branch("main");

    // Run cresca review: skip A, review B and C, exclude D
    let output = repo.run_cresca(&[
        "review",
        "main",
        "develop",
        "--skip-to",
        file_b_hash,
        "--stop-at",
        file_c_hash,
    ]);
    assert!(
        output.status.success(),
        "cresca review --skip-to --stop-at should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify: fileA.txt should be auto-approved (committed)
    let files_in_head = repo.git(&["ls-tree", "--name-only", "HEAD"]);
    let files_str = String::from_utf8_lossy(&files_in_head.stdout);
    assert!(
        files_str.contains("fileA.txt"),
        "fileA.txt should be auto-approved and committed"
    );

    // Verify: fileB.txt and fileC.txt should be unstaged changes (in review)
    let status = repo.git(&["status", "--porcelain"]);
    let status_str = String::from_utf8_lossy(&status.stdout);
    assert!(
        status_str.contains("fileB.txt"),
        "fileB.txt should be an unstaged change"
    );
    assert!(
        status_str.contains("fileC.txt"),
        "fileC.txt should be an unstaged change"
    );

    // Verify: fileD.txt should NOT appear (excluded by --stop-at)
    assert!(
        !status_str.contains("fileD.txt"),
        "fileD.txt should NOT appear (excluded by --stop-at), got: {}",
        status_str
    );

    // Additional check: fileD.txt should not be committed either
    assert!(
        !files_str.contains("fileD.txt"),
        "fileD.txt should not be in HEAD"
    );
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

/// Test that an error clears visible progress before printing diagnostics.
#[cfg(unix)]
#[test]
fn test_review_clears_progress_before_error_output() {
    let repo = repo_with_reviewable_change("feature.txt");

    let slow_git = SlowGit::new();
    let output = run_cresca_with_stderr_pty(
        &repo,
        &["review", "main", "develop", "--stop-at", "invalidhash"],
        &slow_git,
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    let clear_position = stderr
        .find("\r\x1b[2K")
        .expect("visible progress should be cleared on failure");
    let error_position = stderr
        .find("error:")
        .expect("error message should be printed");
    assert!(
        clear_position < error_position,
        "progress must be cleared before diagnostics: {stderr:?}"
    );
    assert!(stderr.contains("invalidhash"));
}

/// Test that a Git failure clears visible progress before printing diagnostics.
#[cfg(unix)]
#[test]
fn test_review_clears_progress_before_git_error_output() {
    let repo = repo_with_reviewable_change("feature.txt");
    repo.git(&["remote", "set-url", "origin", "/path/that/does/not/exist"]);

    let slow_git = SlowGit::new();
    let output = run_cresca_with_stderr_pty(&repo, &["review", "main", "develop"], &slow_git);

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    let clear_position = stderr
        .find("\r\x1b[2K")
        .expect("visible progress should be cleared on Git failure");
    let error_position = stderr.find("error:").expect("Git error should be printed");
    assert!(
        clear_position < error_position,
        "progress must be cleared before Git diagnostics: {stderr:?}"
    );
    assert!(stderr.contains("Failed to fetch target branch"));
}

/// Test that Ctrl-C clears visible progress before exiting.
#[cfg(unix)]
#[test]
fn test_review_clears_progress_on_interrupt() {
    let repo = repo_with_reviewable_change("feature.txt");

    let slow_git = SlowGit::new();
    let output =
        run_cresca_with_stderr_pty_inner(&repo, &["review", "main", "develop"], &slow_git, true);

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        output.status.code(),
        Some(130),
        "unexpected interrupt status; stderr: {stderr:?}"
    );
    assert!(stderr.contains("⠋ Preparing review branch"));
    assert!(stderr.ends_with("\r\x1b[2K"));
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
