mod common;

use common::TempGitRepo;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn install_git_wrapper(repo: &TempGitRepo, script: &str) -> tempfile::TempDir {
    let wrapper = tempfile::TempDir::new().expect("Git wrapper directory should be created");
    let path = wrapper.path().join("git");
    std::fs::write(&path, script).expect("Git wrapper should be written");
    let mut permissions = std::fs::metadata(&path)
        .expect("Git wrapper should exist")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("Git wrapper should be executable");
    assert!(repo.path().exists());
    wrapper
}

fn run_cresca_with_git_wrapper(
    repo: &TempGitRepo,
    wrapper: &tempfile::TempDir,
    args: &[&str],
) -> std::process::Output {
    let real_git = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("shell should locate Git");
    assert!(real_git.status.success());
    let real_git = String::from_utf8(real_git.stdout)
        .expect("Git path should be UTF-8")
        .trim()
        .to_string();
    let home = tempfile::TempDir::new().expect("isolated Cresca home should be created");
    Command::new(TempGitRepo::cresca_binary())
        .args(args)
        .env("HOME", home.path())
        .env("NO_COLOR", "1")
        .env("PATH", wrapper.path())
        .env("CRESCA_REAL_GIT", real_git)
        .current_dir(repo.path())
        .output()
        .expect("Cresca should execute through Git wrapper")
}

fn assert_rejected_unchanged(
    repo: &TempGitRepo,
    args: &[&str],
    candidate: &str,
    expected_reason: &str,
) {
    let before = repo.snapshot();
    let output = repo.run_cresca(args);
    assert!(
        !output.status.success(),
        "candidate `{candidate}` should be rejected"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(candidate),
        "stderr did not identify `{candidate}`: {stderr}"
    );
    assert!(
        stderr.contains(expected_reason),
        "stderr did not explain `{expected_reason}` for `{candidate}`: {stderr}"
    );
    assert_eq!(repo.snapshot(), before);
}

// Production break caught: omitting any protected remote/admin field from RepoState
// would let a rejecting review mutate Git state without an atomicity assertion noticing.
#[test]
fn repo_state_detects_remote_refs_and_git_admin_files() {
    let repo = TempGitRepo::new();
    let initial = repo.snapshot();

    repo.git(&["update-ref", "refs/remotes/origin/snapshot-probe", "HEAD"]);
    assert_ne!(
        repo.snapshot(),
        initial,
        "remote refs must affect snapshots"
    );
    repo.git(&["update-ref", "-d", "refs/remotes/origin/snapshot-probe"]);
    assert_eq!(repo.snapshot(), initial);

    for name in [
        "FETCH_HEAD",
        "ORIG_HEAD",
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
    ] {
        let path = repo.git_path(name);
        let previous = std::fs::read(&path).ok();
        std::fs::write(&path, format!("{name} snapshot probe\n"))
            .expect("Git admin probe should be writable");
        assert_ne!(repo.snapshot(), initial, "{name} must affect snapshots");
        match previous {
            Some(bytes) => std::fs::write(&path, bytes).expect("admin file should restore"),
            None => std::fs::remove_file(&path).expect("admin probe should be removable"),
        }
        assert_eq!(repo.snapshot(), initial, "{name} absence must be preserved");
    }
}

// Production break caught: treating an explicit refs/heads source as a remote branch
// would reject a valid local-only review instead of materializing its endpoint tree.
#[test]
fn review_uses_local_only_source_without_fetching_origin() {
    let repo = TempGitRepo::new();
    repo.create_branch("local-feature");
    repo.write_file("local.txt", "local-only change");
    repo.git(&["add", "."]);
    repo.commit("Add local-only change");
    repo.switch_branch("main");

    let output = repo.run_cresca(&["review", "main", "refs/heads/local-feature"]);

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.read_file("local.txt"), "local-only change");
}

// Production break caught: accepting HEAD as a branch would let a moving pseudoref
// define review identity instead of requiring a canonical local or remote branch.
#[test]
fn head_is_rejected_as_not_a_branch() {
    let repo = TempGitRepo::new();
    assert_rejected_unchanged(&repo, &["review", "main", "HEAD"], "HEAD", "not a branch");
}

// Production break caught: accepting a raw object ID would make review identity depend
// on a commit spelling even though the review command requires branch endpoints.
#[test]
fn commit_id_is_rejected_as_not_a_branch() {
    let repo = TempGitRepo::new();
    let oid = repo.rev_parse("HEAD");
    assert_rejected_unchanged(&repo, &["review", "main", &oid], &oid, "not a branch");
}

// Production break caught: accepting revision operators would resolve an arbitrary
// commit expression instead of a branch tip.
#[test]
fn revision_expression_is_rejected_as_not_a_branch() {
    let repo = TempGitRepo::new();
    assert_rejected_unchanged(
        &repo,
        &["review", "main", "HEAD~1"],
        "HEAD~1",
        "not a branch",
    );
}

// Production break caught: falling through to rev-parse would let a tag-only name
// masquerade as a branch endpoint.
#[test]
fn tag_only_name_is_rejected_as_not_a_branch() {
    let repo = TempGitRepo::new();
    repo.git(&["tag", "release-only"]);
    assert_rejected_unchanged(
        &repo,
        &["review", "main", "release-only"],
        "release-only",
        "not a branch",
    );
}

// Production break caught: discovering remotes before checking a locally known
// tag-only name would let an unrelated remote outage mask the branch-validation error.
#[test]
fn tag_only_name_is_rejected_before_unavailable_remote_discovery() {
    let repo = TempGitRepo::new();
    repo.git(&["tag", "offline-tag-only"]);
    let offline = repo.add_bare_remote("offline-tag-probe");
    drop(offline);
    assert_rejected_unchanged(
        &repo,
        &["review", "refs/heads/main", "offline-tag-only"],
        "offline-tag-only",
        "not a branch",
    );
}

// Production break caught: skipping check-ref-format would send malformed branch
// names into remote discovery and report misleading availability errors.
#[test]
fn invalid_ref_name_is_rejected_before_remote_discovery() {
    let repo = TempGitRepo::new();
    assert_rejected_unchanged(
        &repo,
        &["review", "main", "bad..branch"],
        "bad..branch",
        "not a branch",
    );
}

// Production break caught: validating only plain names would let malformed explicit
// remote suffixes reach ls-remote and be misreported as remote availability failures.
#[test]
fn invalid_explicit_remote_ref_is_rejected_before_query() {
    let repo = TempGitRepo::new();
    assert_rejected_unchanged(
        &repo,
        &["review", "refs/heads/main", "origin/bad..branch"],
        "origin/bad..branch",
        "not a branch",
    );
}

// Production break caught: accepting an empty qualified local name would lose the
// exact offending input and defer a syntax error to unrelated Git operations.
#[test]
fn empty_explicit_local_ref_is_rejected_with_exact_input() {
    let repo = TempGitRepo::new();
    assert_rejected_unchanged(
        &repo,
        &["review", "refs/heads/main", "refs/heads/"],
        "refs/heads/",
        "not a branch",
    );
}

// Production break caught: accepting `<remote>/` would query refs/heads/ instead of
// rejecting the exact empty branch suffix at the input boundary.
#[test]
fn empty_explicit_remote_ref_is_rejected_with_exact_input() {
    let repo = TempGitRepo::new();
    assert_rejected_unchanged(
        &repo,
        &["review", "refs/heads/main", "origin/"],
        "origin/",
        "not a branch",
    );
}

// Production break caught: checking a same-named tag before refs/heads would reject
// or select the tag even when a valid local branch exists.
#[test]
fn branch_wins_over_same_named_tag() {
    let repo = TempGitRepo::new();
    repo.git(&["tag", "release"]);
    repo.create_branch("release");
    repo.write_file("release.txt", "branch content");
    repo.git(&["add", "."]);
    repo.commit("Add release branch content");
    repo.switch_branch("main");

    let output = repo.run_cresca(&["review", "refs/heads/main", "release"]);

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.read_file("release.txt"), "branch content");
}

fn create_and_push_branch(repo: &TempGitRepo, branch: &str, remote: &str, file: &str) {
    repo.create_branch(branch);
    repo.write_file(file, branch);
    repo.git(&["add", "."]);
    repo.commit(&format!("Add {branch}"));
    repo.git(&["push", remote, branch]);
    repo.switch_branch("main");
}

// Production break caught: resolving explicit remote syntax from a tracking ref
// would miss a live branch whose fetched commit is not already published locally.
#[test]
fn explicit_remote_uses_live_branch_without_publishing_tracking_ref() {
    let repo = TempGitRepo::new();
    create_and_push_branch(&repo, "remote-feature", "origin", "remote.txt");
    repo.git(&["update-ref", "-d", "refs/remotes/origin/remote-feature"]);
    let before_remote_refs = repo.snapshot().remote_refs;

    let output = repo.run_cresca(&["review", "refs/heads/main", "origin/remote-feature"]);

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.read_file("remote.txt"), "remote-feature");
    assert_eq!(repo.snapshot().remote_refs, before_remote_refs);
}

// Production break caught: trusting a stale refs/remotes entry would accept a branch
// that the reachable selected remote confirms has been deleted.
#[test]
fn stale_explicit_remote_tracking_ref_is_rejected() {
    let repo = TempGitRepo::new();
    create_and_push_branch(&repo, "deleted-feature", "origin", "deleted.txt");
    let stale = repo.rev_parse("refs/remotes/origin/deleted-feature");
    repo.git(&["push", "origin", "--delete", "deleted-feature"]);
    repo.git(&["update-ref", "refs/remotes/origin/deleted-feature", &stale]);
    assert_rejected_unchanged(
        &repo,
        &[
            "review",
            "refs/heads/main",
            "refs/remotes/origin/deleted-feature",
        ],
        "refs/remotes/origin/deleted-feature",
        "confirmed absent",
    );
}

// Production break caught: treating an unreachable selected remote as branch absence
// would hide an unavailable-state error and could fall back to stale local data.
#[test]
fn unreachable_selected_remote_is_rejected_as_unavailable() {
    let repo = TempGitRepo::new();
    repo.git(&[
        "remote",
        "set-url",
        "origin",
        "/definitely/missing/cresca-remote",
    ]);
    assert_rejected_unchanged(
        &repo,
        &["review", "refs/heads/main", "origin/feature"],
        "origin/feature",
        "query remote branch",
    );
}

// Production break caught: splitting explicit syntax at the first slash would select
// remote `team` instead of the longest configured remote `team/upstream`.
#[test]
fn configured_remote_name_containing_slash_is_supported() {
    let repo = TempGitRepo::new();
    let slash_remote = repo.add_bare_remote("team/upstream");
    create_and_push_branch(&repo, "slash-feature", "team/upstream", "slash-remote.txt");

    let output = repo.run_cresca(&["review", "refs/heads/main", "team/upstream/slash-feature"]);

    assert!(slash_remote.path().exists());
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.read_file("slash-remote.txt"), "slash-feature");
}

// Production break caught: scanning all configured remotes for explicit syntax would
// let an unrelated outage block a fully specified, reachable branch.
#[test]
fn explicit_remote_bypasses_unrelated_unreachable_remote() {
    let repo = TempGitRepo::new();
    create_and_push_branch(&repo, "selected-feature", "origin", "selected.txt");
    let unrelated = repo.add_bare_remote("unrelated");
    drop(unrelated);

    let output = repo.run_cresca(&["review", "refs/heads/main", "origin/selected-feature"]);

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.read_file("selected.txt"), "selected-feature");
}

fn create_untracked_local_branch(repo: &TempGitRepo, branch: &str) {
    repo.create_branch(branch);
    repo.write_file(&format!("{branch}.txt"), branch);
    repo.git(&["add", "."]);
    repo.commit(&format!("Add local {branch}"));
    repo.switch_branch("main");
}

// Production break caught: treating absent upstream keys as broken configuration
// would reject a local-only plain branch instead of applying the discovery table.
#[test]
fn both_upstream_keys_absent_uses_no_upstream_table() {
    let repo = TempGitRepo::new();
    create_untracked_local_branch(&repo, "untracked-topic");

    let output = repo.run_cresca(&["review", "refs/heads/main", "untracked-topic"]);

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.read_file("untracked-topic.txt"), "untracked-topic");
}

// Production break caught: treating remote `.` as a publishing upstream would bypass
// same-named remote discovery and silently choose the local branch.
#[test]
fn remote_dot_uses_no_upstream_resolution_table() {
    let repo = TempGitRepo::new();
    create_untracked_local_branch(&repo, "dot-topic");
    repo.git(&["push", "origin", "dot-topic"]);
    repo.set_upstream("dot-topic", ".", "main");
    assert_rejected_unchanged(
        &repo,
        &["review", "refs/heads/main", "dot-topic"],
        "dot-topic",
        "ambiguous",
    );
}

// Production break caught: silently tolerating a remote-only upstream setting would
// fall back to local resolution despite incomplete user configuration.
#[test]
fn upstream_with_only_remote_is_broken() {
    let repo = TempGitRepo::new();
    create_untracked_local_branch(&repo, "only-remote");
    repo.git(&["config", "branch.only-remote.remote", "origin"]);
    assert_rejected_unchanged(
        &repo,
        &["review", "refs/heads/main", "only-remote"],
        "only-remote",
        "invalid configured upstream",
    );
}

// Production break caught: silently tolerating a merge-only upstream setting would
// fall back to local resolution despite incomplete user configuration.
#[test]
fn upstream_with_only_merge_is_broken() {
    let repo = TempGitRepo::new();
    create_untracked_local_branch(&repo, "only-merge");
    repo.git(&["config", "branch.only-merge.merge", "refs/heads/only-merge"]);
    assert_rejected_unchanged(
        &repo,
        &["review", "refs/heads/main", "only-merge"],
        "only-merge",
        "invalid configured upstream",
    );
}

// Production break caught: selecting the first duplicate remote value would make
// ambiguous Git configuration appear authoritative.
#[test]
fn duplicate_upstream_remote_is_broken() {
    let repo = TempGitRepo::new();
    create_untracked_local_branch(&repo, "duplicate-remote");
    repo.set_upstream("duplicate-remote", "origin", "duplicate-remote");
    repo.git(&[
        "config",
        "--add",
        "branch.duplicate-remote.remote",
        "origin",
    ]);
    assert_rejected_unchanged(
        &repo,
        &["review", "refs/heads/main", "duplicate-remote"],
        "duplicate-remote",
        "duplicate values",
    );
}

// Production break caught: selecting the first duplicate merge value would make
// ambiguous Git configuration appear authoritative.
#[test]
fn duplicate_upstream_merge_is_broken() {
    let repo = TempGitRepo::new();
    create_untracked_local_branch(&repo, "duplicate-merge");
    repo.set_upstream("duplicate-merge", "origin", "duplicate-merge");
    repo.git(&[
        "config",
        "--add",
        "branch.duplicate-merge.merge",
        "refs/heads/other",
    ]);
    assert_rejected_unchanged(
        &repo,
        &["review", "refs/heads/main", "duplicate-merge"],
        "duplicate-merge",
        "duplicate values",
    );
}

// Production break caught: accepting an upstream remote absent from `git remote`
// would misreport broken configuration as a transient network failure.
#[test]
fn missing_configured_upstream_remote_is_broken() {
    let repo = TempGitRepo::new();
    create_untracked_local_branch(&repo, "missing-remote");
    repo.set_upstream("missing-remote", "vanished", "missing-remote");
    assert_rejected_unchanged(
        &repo,
        &["review", "refs/heads/main", "missing-remote"],
        "missing-remote",
        "does not exist",
    );
}

// Production break caught: accepting a malformed merge value would construct and
// query a remote ref that Git configuration never identified as a branch.
#[test]
fn malformed_upstream_merge_ref_is_broken() {
    let repo = TempGitRepo::new();
    create_untracked_local_branch(&repo, "malformed-merge");
    repo.git(&["config", "branch.malformed-merge.remote", "origin"]);
    repo.git(&[
        "config",
        "branch.malformed-merge.merge",
        "refs/tags/not-a-branch",
    ]);
    assert_rejected_unchanged(
        &repo,
        &["review", "refs/heads/main", "malformed-merge"],
        "malformed-merge",
        "malformed",
    );
}

// Production break caught: falling back to a local branch after its reachable remote
// confirms the configured upstream was deleted would hide a broken publishing link.
#[test]
fn confirmed_deleted_upstream_is_rejected() {
    let repo = TempGitRepo::new();
    create_untracked_local_branch(&repo, "deleted-upstream");
    repo.set_upstream("deleted-upstream", "origin", "deleted-upstream");
    assert_rejected_unchanged(
        &repo,
        &["review", "refs/heads/main", "deleted-upstream"],
        "deleted-upstream",
        "confirmed absent",
    );
}

// Production break caught: reporting an unreachable configured upstream as deleted
// would turn an unknown remote state into a false conclusive absence.
#[test]
fn unreachable_configured_upstream_is_unavailable() {
    let repo = TempGitRepo::new();
    create_untracked_local_branch(&repo, "offline-upstream");
    repo.set_upstream("offline-upstream", "origin", "offline-upstream");
    repo.git(&[
        "remote",
        "set-url",
        "origin",
        "/definitely/missing/upstream",
    ]);
    assert_rejected_unchanged(
        &repo,
        &["review", "refs/heads/main", "offline-upstream"],
        "offline-upstream",
        "query remote branch",
    );
}

// Production break caught: preferring a local branch when a reachable same-named
// remote also exists would violate the no-upstream uniqueness table.
#[test]
fn plain_local_and_same_named_remote_is_ambiguous() {
    let repo = TempGitRepo::new();
    create_untracked_local_branch(&repo, "shared-topic");
    repo.git(&["push", "origin", "shared-topic"]);
    let before = repo.snapshot();

    let output = repo.run_cresca(&["review", "refs/heads/main", "shared-topic"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "Branch `shared-topic` is ambiguous. Candidates: `refs/heads/shared-topic`, `origin/shared-topic`. Use one explicitly: `refs/heads/shared-topic`, `origin/shared-topic`."
        ),
        "{stderr}"
    );
    assert_eq!(repo.snapshot(), before);
}

// Production break caught: requiring a local tracking ref for a plain remote-only
// branch would reject the unique live remote match.
#[test]
fn plain_remote_only_branch_uses_the_unique_remote() {
    let repo = TempGitRepo::new();
    create_and_push_branch(&repo, "remote-only", "origin", "remote-only.txt");
    repo.git(&["branch", "-D", "remote-only"]);

    let output = repo.run_cresca(&["review", "refs/heads/main", "remote-only"]);

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.read_file("remote-only.txt"), "remote-only");
}

// Production break caught: taking the first matching remote would make plain branch
// resolution depend on configured-remote enumeration order.
#[test]
fn plain_branch_on_two_remotes_is_ambiguous() {
    let repo = TempGitRepo::new();
    let second = repo.add_bare_remote("second");
    create_and_push_branch(&repo, "multi-remote", "origin", "multi.txt");
    repo.git(&["push", "second", "multi-remote"]);
    repo.git(&["branch", "-D", "multi-remote"]);
    assert!(second.path().exists());
    let before = repo.snapshot();

    let output = repo.run_cresca(&["review", "main", "multi-remote"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "Branch `multi-remote` is ambiguous. Candidates: `origin/multi-remote`, `second/multi-remote`. Use one explicitly: `origin/multi-remote`, `second/multi-remote`."
        ),
        "{stderr}"
    );
    assert_eq!(repo.snapshot(), before);
}

// Production break caught: falling back to an assumed origin branch would turn a
// conclusive all-remote miss into a later fetch error instead of branch-not-found.
#[test]
fn plain_missing_branch_is_rejected_after_all_remotes_answer() {
    let repo = TempGitRepo::new();
    assert_rejected_unchanged(
        &repo,
        &["review", "refs/heads/main", "nowhere-topic"],
        "nowhere-topic",
        "not found",
    );
}

// Production break caught: accepting a no-match conclusion after only some remotes
// answer would claim uniqueness while another configured remote is unreachable.
#[test]
fn unreachable_unrelated_remote_prevents_plain_uniqueness() {
    let repo = TempGitRepo::new();
    create_and_push_branch(&repo, "discoverable", "origin", "discoverable.txt");
    repo.git(&["branch", "-D", "discoverable"]);
    let offline = repo.add_bare_remote("offline");
    drop(offline);
    assert_rejected_unchanged(
        &repo,
        &["review", "refs/heads/main", "discoverable"],
        "discoverable",
        "offline",
    );
}

// Production break caught: contacting remotes for refs/heads syntax would let an
// unrelated outage block an explicitly selected local branch.
#[test]
fn explicit_local_bypasses_unreachable_remote() {
    let repo = TempGitRepo::new();
    create_untracked_local_branch(&repo, "local-explicit");
    repo.git(&["remote", "set-url", "origin", "/definitely/missing/origin"]);

    let output = repo.run_cresca(&["review", "refs/heads/main", "refs/heads/local-explicit"]);

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.read_file("local-explicit.txt"), "local-explicit");
}

// Production break caught: enumerating remotes before recognizing refs/heads syntax
// would reject an explicit-local review when the independent `git remote` command fails.
#[test]
fn explicit_local_bypasses_failing_remote_enumeration() {
    let repo = TempGitRepo::new();
    create_untracked_local_branch(&repo, "local-enumeration-bypass");
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\nif [ \"$1\" = remote ] && [ \"$#\" -eq 1 ]; then\n  printf 'remote enumeration must be bypassed\\n' >&2\n  exit 71\nfi\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = run_cresca_with_git_wrapper(
        &repo,
        &wrapper,
        &[
            "review",
            "refs/heads/main",
            "refs/heads/local-enumeration-bypass",
        ],
    );

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        repo.read_file("local-enumeration-bypass.txt"),
        "local-enumeration-bypass"
    );
}

// Production break caught: falling back after a partial upstream configuration would
// ignore an explicit but broken Git relationship and select the local tip.
#[test]
fn broken_upstream_does_not_fall_back_to_local() {
    let repo = TempGitRepo::new();
    create_untracked_local_branch(&repo, "broken-topic");
    repo.git(&["config", "branch.broken-topic.remote", "origin"]);
    assert_rejected_unchanged(
        &repo,
        &["review", "refs/heads/main", "broken-topic"],
        "broken-topic",
        "invalid configured upstream",
    );
}

// Production break caught: assuming upstream and local branch names match would query
// origin/dev instead of the configured refs/heads/feature/alice.
#[test]
fn differently_named_upstream_resolves_the_remote_branch() {
    let repo = TempGitRepo::new();
    create_and_push_branch(&repo, "feature/alice", "origin", "configured-upstream.txt");
    create_untracked_local_branch(&repo, "dev");
    repo.write_file("local-decoy.txt", "local-only");
    repo.switch_branch("dev");
    repo.git(&["add", "."]);
    repo.commit("Add local dev decoy");
    repo.switch_branch("main");
    repo.set_upstream("dev", "origin", "feature/alice");

    let output = repo.run_cresca(&["review", "refs/heads/main", "dev"]);

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.read_file("configured-upstream.txt"), "feature/alice");
    assert!(!repo.path().join("local-decoy.txt").exists());
}

// Production break caught: resolving only the source with the new rules would still
// reject a valid target branch that exists exclusively under refs/heads.
#[test]
fn local_only_target_is_reviewable() {
    let repo = TempGitRepo::new();
    repo.create_branch("local-target");
    repo.write_file("target.txt", "target");
    repo.git(&["add", "."]);
    repo.commit("Add local target");
    repo.create_branch("local-source");
    repo.write_file("source.txt", "source");
    repo.git(&["add", "."]);
    repo.commit("Add local source");
    repo.switch_branch("main");

    let output = repo.run_cresca(&[
        "review",
        "refs/heads/local-target",
        "refs/heads/local-source",
    ]);

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.read_file("source.txt"), "source");
}

// Production break caught: resolving plain tracked branches to their local tip would
// incorrectly include unpushed commits in the review range.
#[test]
fn tracked_local_ahead_of_upstream_reviews_only_upstream_tip() {
    let repo = TempGitRepo::new();
    repo.create_branch("ahead-topic");
    repo.write_file("upstream.txt", "pushed");
    repo.git(&["add", "."]);
    repo.commit("Add pushed topic");
    repo.git(&["push", "-u", "origin", "ahead-topic"]);
    repo.write_file("unpushed.txt", "local ahead");
    repo.git(&["add", "."]);
    repo.commit("Add unpushed topic");
    repo.switch_branch("main");

    let output = repo.run_cresca(&["review", "main", "ahead-topic"]);

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.read_file("upstream.txt"), "pushed");
    assert!(!repo.path().join("unpushed.txt").exists());
}

// Production break caught: using a diverged local tip would include local-only work
// and omit a remote-only upstream commit from the review endpoint.
#[test]
fn tracked_local_diverged_from_upstream_reviews_only_upstream_tip() {
    let repo = TempGitRepo::new();
    repo.create_branch("diverged-topic");
    repo.write_file("common.txt", "common");
    repo.git(&["add", "."]);
    repo.commit("Add common topic");
    repo.git(&["push", "-u", "origin", "diverged-topic"]);
    repo.write_file("remote-only.txt", "remote tip");
    repo.git(&["add", "."]);
    repo.commit("Add remote-only topic");
    repo.git(&["push", "origin", "diverged-topic"]);
    repo.git(&["reset", "--hard", "HEAD^"]);
    repo.write_file("local-only.txt", "local tip");
    repo.git(&["add", "."]);
    repo.commit("Add local-only topic");
    repo.switch_branch("main");

    let output = repo.run_cresca(&["review", "main", "diverged-topic"]);

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.read_file("remote-only.txt"), "remote tip");
    assert!(!repo.path().join("local-only.txt").exists());
}
