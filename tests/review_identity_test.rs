mod common;

use common::TempGitRepo;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::process::Command;

#[cfg(unix)]
fn cresca_home_with_naming_hook(script: &[u8]) -> tempfile::TempDir {
    let home = tempfile::TempDir::new().expect("isolated Cresca home should be created");
    let config_dir = home.path().join(".cresca");
    let hook = home.path().join("review-name");
    std::fs::create_dir(&config_dir).expect("Cresca config directory should be created");
    std::fs::write(&hook, script).expect("naming hook should be written");
    let mut permissions = std::fs::metadata(&hook)
        .expect("naming hook metadata should be readable")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&hook, permissions).expect("naming hook should be executable");
    std::fs::write(
        config_dir.join("config.toml"),
        format!(
            "[review_branch.naming_hook]\nprogram = {:?}\n",
            hook.to_string_lossy()
        ),
    )
    .expect("Cresca config should be written");
    home
}

#[cfg(unix)]
fn install_offline_git_wrapper() -> (tempfile::TempDir, String) {
    let real_git = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("shell should locate Git");
    assert!(real_git.status.success());
    let real_git = String::from_utf8(real_git.stdout)
        .expect("Git path should be UTF-8")
        .trim()
        .to_string();
    let wrapper = tempfile::TempDir::new().expect("Git wrapper directory should be created");
    let path = wrapper.path().join("git");
    std::fs::write(
        &path,
        b"#!/bin/sh\ncase \"$1\" in\n  ls-remote|fetch) printf 'network Git command forbidden\\n' >&2; exit 97 ;;\nesac\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    )
    .expect("Git wrapper should be written");
    let mut permissions = std::fs::metadata(&path)
        .expect("Git wrapper metadata should be readable")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("Git wrapper should be executable");
    (wrapper, real_git)
}

#[cfg(unix)]
fn run_cresca_offline(
    repo: &TempGitRepo,
    wrapper: &tempfile::TempDir,
    real_git: &str,
    args: &[&str],
) -> std::process::Output {
    Command::new(TempGitRepo::cresca_binary())
        .args(args)
        .env("HOME", repo.path().join(".offline-cresca-home"))
        .env("NO_COLOR", "1")
        .env("PATH", wrapper.path())
        .env("CRESCA_REAL_GIT", real_git)
        .current_dir(repo.path())
        .output()
        .expect("Cresca should run with the offline Git wrapper")
}

// Production break caught: selecting reviews by raw command-line spelling creates
// duplicate reviews for local branches and their configured upstream identities.
#[test]
fn equivalent_local_and_upstream_spellings_reuse_one_review_branch() {
    let repo = TempGitRepo::new();
    repo.create_branch("dev");
    repo.write_file("dev.txt", "change");
    repo.git(&["add", "."]);
    repo.commit("Add dev change");
    repo.git(&["push", "-u", "origin", "dev"]);
    repo.switch_branch("main");

    let mut selected_review = None;
    for args in [
        ["review", "main", "dev"],
        ["review", "main", "origin/dev"],
        ["review", "origin/main", "dev"],
        ["review", "origin/main", "origin/dev"],
    ] {
        let output = repo.run_cresca(&args);
        assert!(
            output.status.success(),
            "args: {args:?}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let current = repo.current_branch();
        match selected_review.as_deref() {
            Some(expected) => assert_eq!(current, expected, "args: {args:?}"),
            None => selected_review = Some(current),
        }
        repo.git(&["reset", "--hard"]);
        repo.git(&["clean", "-fd"]);
        repo.switch_branch("main");
    }

    let reviews = repo
        .git_stdout(&["for-each-ref", "--format=%(refname:short)", "refs/heads"])
        .lines()
        .filter(|branch| {
            repo.git_config_values(&format!("branch.{branch}.cresca-version")) == ["2"]
        })
        .count();
    assert_eq!(reviews, 1);
}

// Production break caught: finding an existing canonical review after an equivalent
// spelling change must not invoke the naming hook reserved for new review creation.
#[cfg(unix)]
#[test]
fn equivalent_existing_review_does_not_run_failing_hook() {
    let repo = TempGitRepo::new();
    repo.create_branch("dev");
    repo.write_file("dev.txt", "change");
    repo.git(&["add", "."]);
    repo.commit("Add dev change");
    repo.git(&["push", "-u", "origin", "dev"]);
    repo.switch_branch("main");

    let first = repo.run_cresca(&["review", "main", "dev"]);
    assert!(first.status.success());
    let review_branch = repo.current_branch();
    repo.git(&["reset", "--hard"]);
    repo.git(&["clean", "-fd"]);
    repo.switch_branch("main");
    let home = cresca_home_with_naming_hook(b"#!/bin/sh\nprintf 'must not run\\n' >&2\nexit 41\n");

    let output = repo.run_cresca_with_home(&["review", "origin/main", "origin/dev"], home.path());

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.current_branch(), review_branch);
}

// Production break caught: failing to persist and validate the plain local anchor
// loses review continuity when that branch is later published with an upstream.
#[test]
fn local_review_survives_push_u_and_only_new_changes_remain_unreviewed() {
    let repo = TempGitRepo::new();
    repo.create_branch("dev");
    repo.write_file("approved.txt", "approved change\n");
    repo.git(&["add", "."]);
    repo.commit("Add local dev change");
    let approved_source = repo.rev_parse("dev");
    repo.switch_branch("main");

    let first = repo.run_cresca(&["review", "main", "dev"]);
    assert!(
        first.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    let review_branch = repo.current_branch();
    assert_eq!(
        repo.git_config_values(&format!("branch.{review_branch}.cresca-version")),
        ["2"]
    );
    repo.git(&["add", "-A"]);
    assert!(repo.run_cresca(&["approve"]).status.success());

    repo.switch_branch("dev");
    repo.git(&["push", "-u", "origin", "dev"]);
    repo.write_file("new.txt", "new change\n");
    repo.git(&["add", "."]);
    repo.commit("Add later dev change");
    repo.git(&["push", "origin", "dev"]);
    repo.switch_branch("main");

    let second = repo.run_cresca(&["review", "main", "dev"]);

    assert!(
        second.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(repo.current_branch(), review_branch);
    assert_eq!(repo.cached_diff(), Vec::<u8>::new());
    assert_eq!(
        repo.worktree_diff(),
        repo.diff(&approved_source, "origin/dev")
    );
    assert_eq!(repo.read_file("approved.txt"), "approved change\n");
    assert_eq!(repo.read_file("new.txt"), "new change\n");
}

// Production break caught: reparsing a verified stored local anchor lets a configured
// remote prefix turn `refs/heads/team/dev` into the unrelated shorthand `team/dev`.
#[test]
fn stored_remote_prefix_local_anchor_uses_forced_plain_transition() {
    let repo = TempGitRepo::new();
    repo.create_branch("dev");
    repo.write_file("dev.txt", "change\n");
    repo.git(&["add", "."]);
    repo.commit("Add dev change");
    repo.git(&["push", "-u", "origin", "dev"]);
    repo.git(&["branch", "team/dev", "dev"]);
    repo.set_upstream("team/dev", "origin", "dev");
    repo.switch_branch("main");

    let first = repo.run_cresca(&["review", "refs/heads/main", "dev"]);
    assert!(
        first.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    let review_branch = repo.current_branch();
    repo.git(&["reset", "--hard"]);
    repo.git(&["clean", "-fd"]);
    repo.switch_branch("main");

    let source_kind = format!("branch.{review_branch}.cresca-source-kind");
    let source_ref = format!("branch.{review_branch}.cresca-source-ref");
    let source_remote = format!("branch.{review_branch}.cresca-source-remote");
    let source_anchor = format!("branch.{review_branch}.cresca-source-anchor");
    repo.git(&["config", "--local", "--replace-all", &source_kind, "local"]);
    repo.git(&[
        "config",
        "--local",
        "--replace-all",
        &source_ref,
        "refs/heads/team/dev",
    ]);
    let _ = repo.git_maybe(&["config", "--local", "--unset-all", &source_remote]);
    repo.git(&[
        "config",
        "--local",
        "--add",
        &source_anchor,
        "refs/heads/team/dev",
    ]);
    let _team_remote = repo.add_bare_remote("team");

    let output = repo.run_cresca(&["review", "refs/heads/main", "dev"]);

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.current_branch(), review_branch);
    assert_eq!(repo.git_config_values(&source_kind), ["remote"]);
    assert_eq!(repo.git_config_values(&source_remote), ["origin"]);
    assert_eq!(repo.git_config_values(&source_ref), ["refs/heads/dev"]);
    assert_eq!(
        repo.git_config_values(&source_anchor),
        ["refs/heads/dev", "refs/heads/team/dev"]
    );
}

// Production break caught: checking only endpoint OIDs would accept one canonical
// branch as both sides and mutate repository state for a meaningless review.
#[test]
fn same_canonical_target_and_source_are_rejected_without_mutation() {
    let repo = TempGitRepo::new();
    let before = repo.snapshot();

    let output = repo.run_cresca(&["review", "main", "origin/main"]);

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("same branch"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.snapshot(), before);
}

// Production break caught: rejecting equal endpoint OIDs would conflate branch
// identity with branch position and forbid a valid empty review.
#[test]
fn different_identities_at_the_same_commit_create_an_empty_review() {
    let repo = TempGitRepo::new();
    repo.git(&["branch", "same-tip", "main"]);

    let output = repo.run_cresca(&["review", "refs/heads/main", "refs/heads/same-tip"]);

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("no unreviewed changes"));
    assert_eq!(repo.git_stdout(&["status", "--porcelain"]), "");
    let branch = repo.current_branch();
    assert_eq!(
        repo.git_config_values(&format!("branch.{branch}.cresca-version")),
        ["2"]
    );
}

// Production break caught: sorting the two canonical endpoints would merge the
// directed reviews target<-source and source<-target into one history.
#[test]
fn reversing_target_and_source_is_a_distinct_review() {
    let repo = TempGitRepo::new();
    repo.create_branch("dev");
    repo.write_file("dev.txt", "change\n");
    repo.git(&["add", "."]);
    repo.commit("Add dev change");
    repo.switch_branch("main");

    let first = repo.run_cresca(&["review", "refs/heads/main", "refs/heads/dev"]);
    assert!(first.status.success());
    let first_branch = repo.current_branch();
    repo.git(&["reset", "--hard"]);
    repo.git(&["clean", "-fd"]);
    repo.switch_branch("main");

    let second = repo.run_cresca(&["review", "refs/heads/dev", "refs/heads/main"]);

    assert!(
        second.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    assert_ne!(repo.current_branch(), first_branch);
    assert_eq!(
        repo.git_config_values(&format!("branch.{first_branch}.cresca-version")),
        ["2"]
    );
    assert_eq!(
        repo.git_config_values(&format!("branch.{}.cresca-version", repo.current_branch())),
        ["2"]
    );
}

// Production break caught: using remote URL or commit equality as identity would
// merge independent names configured for the same remote repository.
#[test]
fn different_remote_names_at_the_same_url_and_oid_are_distinct() {
    let repo = TempGitRepo::new();
    repo.create_branch("dev");
    repo.write_file("dev.txt", "change\n");
    repo.git(&["add", "."]);
    repo.commit("Add dev change");
    repo.git(&["push", "origin", "dev"]);
    repo.switch_branch("main");
    repo.git(&[
        "remote",
        "add",
        "mirror",
        repo.remote_dir
            .path()
            .to_str()
            .expect("remote path should be UTF-8"),
    ]);

    let first = repo.run_cresca(&["review", "refs/heads/main", "origin/dev"]);
    assert!(first.status.success());
    let first_branch = repo.current_branch();
    repo.git(&["reset", "--hard"]);
    repo.git(&["clean", "-fd"]);
    repo.switch_branch("main");

    let second = repo.run_cresca(&["review", "refs/heads/main", "mirror/dev"]);

    assert!(
        second.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    assert_ne!(repo.current_branch(), first_branch);
    assert_eq!(
        repo.git_config_values(&format!("branch.{first_branch}.cresca-source-remote")),
        ["origin"]
    );
    assert_eq!(
        repo.git_config_values(&format!(
            "branch.{}.cresca-source-remote",
            repo.current_branch()
        )),
        ["mirror"]
    );
}

// Production break caught: formatting status from live branches—or omitting saved
// identity entirely—would hide the exact canonical endpoints of the prepared review.
#[test]
fn version_two_status_displays_saved_target_and_source() {
    let repo = TempGitRepo::new();
    repo.create_branch("dev");
    repo.write_file("dev.txt", "change\n");
    repo.git(&["add", "."]);
    repo.commit("Add local dev change");
    repo.switch_branch("main");
    assert!(repo
        .run_cresca(&["review", "origin/main", "refs/heads/dev",])
        .status
        .success());

    let output = repo.run_cresca(&["status"]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Target: origin/main"), "stdout: {stdout}");
    assert!(
        stdout.contains("Source: refs/heads/dev"),
        "stdout: {stdout}"
    );
}

// Production break caught: treating v1 raw strings as canonical display values would
// conceal that legacy identity has not yet been resolved or migrated.
#[test]
fn version_one_status_labels_raw_target_and_source_as_unresolved_legacy() {
    let repo = TempGitRepo::new();
    repo.create_branch("dev");
    repo.write_file("dev.txt", "change\n");
    repo.git(&["add", "."]);
    repo.commit("Add dev change");
    repo.switch_branch("main");
    assert!(repo
        .run_cresca(&["review", "refs/heads/main", "refs/heads/dev"])
        .status
        .success());
    let branch = repo.current_branch();
    for field in [
        "target-kind",
        "target-ref",
        "target-remote",
        "target-anchor",
        "source-kind",
        "source-ref",
        "source-remote",
        "source-anchor",
    ] {
        let _ = repo.git_maybe(&[
            "config",
            "--local",
            "--unset-all",
            &format!("branch.{branch}.cresca-{field}"),
        ]);
    }
    repo.git(&[
        "config",
        "--local",
        "--replace-all",
        &format!("branch.{branch}.cresca-target"),
        "legacy-target/raw",
    ]);
    repo.git(&[
        "config",
        "--local",
        "--replace-all",
        &format!("branch.{branch}.cresca-source"),
        "legacy-source/raw",
    ]);
    repo.git(&[
        "config",
        "--local",
        "--replace-all",
        &format!("branch.{branch}.cresca-version"),
        "1",
    ]);

    let output = repo.run_cresca(&["status"]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Unresolved legacy target: legacy-target/raw"),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("Unresolved legacy source: legacy-source/raw"),
        "stdout: {stdout}"
    );
}

// Production break caught: refreshing identity or scope during approve/status would
// contact the moved remote and replace the saved review endpoint implicitly.
#[cfg(unix)]
#[test]
fn approve_and_status_do_not_refresh_a_moved_branch() {
    let repo = TempGitRepo::new();
    repo.create_branch("dev");
    repo.write_file("first.txt", "first\n");
    repo.git(&["add", "."]);
    repo.commit("Add first dev change");
    repo.git(&["push", "origin", "dev"]);
    repo.switch_branch("main");
    assert!(repo
        .run_cresca(&["review", "origin/main", "origin/dev"])
        .status
        .success());
    let review_branch = repo.current_branch();
    repo.git(&["reset", "--hard"]);
    repo.git(&["clean", "-fd"]);
    repo.switch_branch("dev");
    repo.write_file("later.txt", "later\n");
    repo.git(&["add", "."]);
    repo.commit("Move remote dev");
    repo.git(&["push", "origin", "dev"]);
    repo.switch_branch(&review_branch);
    let (wrapper, real_git) = install_offline_git_wrapper();

    let status = run_cresca_offline(&repo, &wrapper, &real_git, &["status"]);
    assert!(
        status.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&status.stdout),
        String::from_utf8_lossy(&status.stderr)
    );
    assert!(String::from_utf8_lossy(&status.stdout).contains("Source: origin/dev"));
    let approve = run_cresca_offline(&repo, &wrapper, &real_git, &["approve"]);
    assert!(
        approve.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&approve.stdout),
        String::from_utf8_lossy(&approve.stderr)
    );
}

// Production break caught: resolving saved local identities during status/approve
// would make deleting original endpoint branches invalidate an otherwise saved range.
#[test]
fn approve_and_status_work_after_target_and_source_deletion() {
    let repo = TempGitRepo::new();
    repo.create_branch("target");
    repo.create_branch("source");
    repo.write_file("source.txt", "change\n");
    repo.git(&["add", "."]);
    repo.commit("Add source change");
    repo.switch_branch("main");
    assert!(repo
        .run_cresca(&["review", "refs/heads/target", "refs/heads/source",])
        .status
        .success());
    let review_branch = repo.current_branch();
    assert_eq!(
        repo.git_config_values(&format!("branch.{review_branch}.cresca-version")),
        ["2"]
    );
    repo.git(&["branch", "-D", "target", "source"]);

    let status = repo.run_cresca(&["status"]);
    assert!(
        status.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&status.stdout),
        String::from_utf8_lossy(&status.stderr)
    );
    let approve = repo.run_cresca(&["approve"]);
    assert!(
        approve.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&approve.stdout),
        String::from_utf8_lossy(&approve.stderr)
    );
}

// Production break caught: approve that validates only identity can mutate staged
// approvals before discovering that the saved range endpoint has been pruned.
#[test]
fn approve_fails_without_mutation_when_a_saved_commit_is_pruned() {
    let repo = TempGitRepo::new();
    repo.create_branch("target");
    repo.create_branch("source");
    repo.write_file("source.txt", "change\n");
    repo.git(&["add", "."]);
    repo.commit("Add unreachable source change");
    let endpoint = repo.rev_parse("source");
    repo.switch_branch("main");
    assert!(repo
        .run_cresca(&["review", "refs/heads/target", "refs/heads/source",])
        .status
        .success());
    repo.git(&["reset", "--hard"]);
    repo.git(&["clean", "-fd"]);
    repo.git(&["branch", "-D", "source"]);
    repo.git(&["reflog", "expire", "--expire=now", "--all"]);
    repo.git(&["gc", "--prune=now"]);
    assert!(!repo
        .git_maybe(&["cat-file", "-e", &format!("{endpoint}^{{commit}}")])
        .status
        .success());
    repo.write_file("staged.txt", "staged\n");
    repo.git(&["add", "staged.txt"]);
    repo.write_file("unstaged.txt", "unstaged\n");
    let before = repo.snapshot();

    let output = repo.run_cresca(&["approve"]);

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(&endpoint),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.snapshot(), before);
}
