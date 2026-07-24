mod common;

use common::TempGitRepo;
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

fn install_git_wrapper(repo: &TempGitRepo, script: &str) -> tempfile::TempDir {
    let wrapper_dir = tempfile::TempDir::new().expect("wrapper directory should be created");
    let wrapper_path = wrapper_dir.path().join("git");
    std::fs::write(&wrapper_path, script).expect("git wrapper should be written");
    let mut permissions = std::fs::metadata(&wrapper_path)
        .expect("git wrapper metadata should be readable")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&wrapper_path, permissions).expect("git wrapper should be executable");
    assert!(repo.path().exists());
    wrapper_dir
}

fn real_git_path() -> String {
    let output = std::process::Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("shell should locate git");
    assert!(output.status.success(), "git should be available on PATH");
    String::from_utf8(output.stdout)
        .expect("git path should be UTF-8")
        .trim()
        .to_string()
}

fn run_cresca_with_git_wrapper(
    repo: &TempGitRepo,
    wrapper: &tempfile::TempDir,
    args: &[&str],
) -> std::process::Output {
    std::process::Command::new(TempGitRepo::cresca_binary())
        .args(args)
        .env("NO_COLOR", "1")
        .env("PATH", wrapper.path())
        .env("CRESCA_REAL_GIT", real_git_path())
        .current_dir(repo.path())
        .output()
        .expect("Failed to execute cresca")
}

fn run_cresca_with_git_wrapper_from(
    _repo: &TempGitRepo,
    wrapper: &tempfile::TempDir,
    current_dir: &Path,
    args: &[&str],
) -> std::process::Output {
    std::process::Command::new(TempGitRepo::cresca_binary())
        .args(args)
        .env("NO_COLOR", "1")
        .env("PATH", wrapper.path())
        .env("CRESCA_REAL_GIT", real_git_path())
        .current_dir(current_dir)
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "Failed to execute cresca in {}: {error}",
                current_dir.display()
            )
        })
}

fn add_ignored_paths(repo: &TempGitRepo, patterns: &str) {
    repo.write_file(".gitignore", patterns);
    repo.git(&["add", ".gitignore"]);
    repo.commit("Add ignored rollback fixtures");
    repo.git(&["push", "origin", "main"]);
}

#[derive(Debug, PartialEq, Eq)]
enum TreeEntry {
    Directory(u32),
    File(u32, Vec<u8>),
    Symlink(PathBuf),
}

fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, TreeEntry> {
    fn visit(root: &Path, directory: &Path, entries: &mut BTreeMap<PathBuf, TreeEntry>) {
        for child in std::fs::read_dir(directory).expect("snapshot directory should be readable") {
            let child = child.expect("snapshot entry should be readable");
            let path = child.path();
            let relative = path
                .strip_prefix(root)
                .expect("snapshot entry should be below root")
                .to_path_buf();
            let metadata = std::fs::symlink_metadata(&path)
                .expect("snapshot entry metadata should be readable");
            if metadata.file_type().is_symlink() {
                entries.insert(
                    relative,
                    TreeEntry::Symlink(
                        std::fs::read_link(path).expect("snapshot symlink should be readable"),
                    ),
                );
            } else if metadata.is_dir() {
                entries.insert(relative, TreeEntry::Directory(metadata.mode()));
                visit(root, &path, entries);
            } else {
                entries.insert(
                    relative,
                    TreeEntry::File(
                        metadata.mode(),
                        std::fs::read(path).expect("snapshot file should be readable"),
                    ),
                );
            }
        }
    }

    let mut entries = BTreeMap::new();
    if root.exists() {
        visit(root, root, &mut entries);
    }
    entries
}

struct RestoreDirectoryMode {
    path: PathBuf,
}

impl Drop for RestoreDirectoryMode {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o755));
    }
}

#[test]
fn test_dirty_review_rejects_before_scanning_ignored_fifo() {
    let (repo, _) = setup_linear_range();
    add_ignored_paths(&repo, "ignored.pipe\n");
    let fifo_path = repo.path().join("ignored.pipe");
    let mkfifo = std::process::Command::new("mkfifo")
        .arg(&fifo_path)
        .output()
        .expect("mkfifo should be executable");
    assert!(mkfifo.status.success());
    repo.write_file("README.md", "dirty tracked content\n");
    let before = repo.snapshot();

    let output = repo.run_cresca(&["review", "main", "develop"]);

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Uncommitted changes found"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.snapshot(), before);
    assert!(std::fs::symlink_metadata(fifo_path)
        .expect("ignored FIFO should survive dirty rejection")
        .file_type()
        .is_fifo());
}

#[test]
fn test_tmpdir_inside_worktree_does_not_endanger_transaction_backup() {
    let (repo, _) = setup_linear_range();
    add_ignored_paths(&repo, ".scratch/\n");
    let scratch_parent = repo.path().join(".scratch");
    std::fs::create_dir(&scratch_parent).expect("in-worktree TMPDIR should be created");
    repo.write_file(".scratch/sentinel.txt", "user TMPDIR sentinel\n");
    let before = repo.snapshot();
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\nif [ \"$1\" = reset ]; then\n  \"$CRESCA_REAL_GIT\" \"$@\" || exit $?\n  printf 'late failure with in-worktree TMPDIR\\n' >&2\n  exit 54\nfi\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = std::process::Command::new(TempGitRepo::cresca_binary())
        .args(["review", "main", "develop"])
        .env("NO_COLOR", "1")
        .env("PATH", wrapper.path())
        .env("CRESCA_REAL_GIT", real_git_path())
        .env("TMPDIR", &scratch_parent)
        .current_dir(repo.path())
        .output()
        .expect("Failed to execute cresca");

    assert!(!output.status.success());
    let after = repo.snapshot();
    assert_eq!(after.branch, before.branch);
    assert_eq!(after.head, before.head);
    assert_eq!(after.local_heads, before.local_heads);
    assert_eq!(after.status, before.status);
    assert_eq!(after.cached_diff, before.cached_diff);
    assert_eq!(after.worktree_diff, before.worktree_diff);
    assert_eq!(after.raw_local_config, before.raw_local_config);
    assert_eq!(after.raw_index, before.raw_index);
    assert_eq!(
        repo.read_file(".scratch/sentinel.txt"),
        "user TMPDIR sentinel\n"
    );
    let cresca_scratch: Vec<_> = std::fs::read_dir(&scratch_parent)
        .expect("in-worktree TMPDIR should remain")
        .map(|entry| entry.expect("TMPDIR entry should be readable").file_name())
        .filter(|name| name.to_string_lossy().starts_with("cresca-review-"))
        .collect();
    assert!(
        cresca_scratch.is_empty(),
        "transaction backup must not be placed in ambient TMPDIR: {cresca_scratch:?}"
    );
}

#[test]
fn test_fatal_show_ref_probe_is_rendered_without_mutation() {
    let (repo, _) = setup_linear_range();
    let before = repo.snapshot();
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\nif [ \"$1\" = show-ref ]; then\n  printf 'fatal show-ref failure\\n' >&2\n  exit 128\nfi\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = run_cresca_with_git_wrapper(&repo, &wrapper, &["review", "main", "develop"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("check existence of review branch"),
        "{stderr}"
    );
    assert!(stderr.contains("exit status: 128"), "{stderr}");
    assert!(stderr.contains("fatal show-ref failure"), "{stderr}");
    assert_eq!(repo.snapshot(), before);
    assert!(!repo.ref_exists("refs/heads/review-main-develop"));
}

#[test]
fn test_fatal_skip_to_rev_list_is_rendered_without_mutation() {
    let (repo, range) = setup_linear_range();
    let before = repo.snapshot();
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\nif [ \"$1\" = rev-list ]; then\n  count_file=\"$0.rev-list-count\"\n  count=0\n  if [ -f \"$count_file\" ]; then IFS= read -r count < \"$count_file\"; fi\n  count=$((count + 1))\n  printf '%s\\n' \"$count\" > \"$count_file\"\n  if [ \"$count\" -eq 2 ]; then\n    printf 'fatal skip-to rev-list failure\\n' >&2\n    exit 128\n  fi\nfi\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = run_cresca_with_git_wrapper(
        &repo,
        &wrapper,
        &["review", "main", "develop", "--skip-to", &range.b[..8]],
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("check earlier commits"), "{stderr}");
    assert!(stderr.contains("exit status: 128"), "{stderr}");
    assert!(
        stderr.contains("fatal skip-to rev-list failure"),
        "{stderr}"
    );
    assert_eq!(repo.snapshot(), before);
    assert!(!repo.ref_exists("refs/heads/review-main-develop"));
}

#[test]
fn test_nested_invocation_late_failure_rolls_back_without_losing_cwd() {
    let (repo, _) = setup_linear_range();
    add_ignored_paths(&repo, "nested/\n");
    let nested = repo.path().join("nested/deeper");
    std::fs::create_dir_all(&nested).expect("nested invocation directory should be created");
    repo.write_file("nested/deeper/ignored.txt", "nested original\n");
    let before = repo.snapshot();
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\ncase \"$*\" in\n  *'config --local --replace-all branch.review-main-develop.cresca-scope'*)\n    \"$CRESCA_REAL_GIT\" \"$@\" || exit $?\n    printf 'nested late failure\\n' >&2\n    exit 55\n    ;;\nesac\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output =
        run_cresca_with_git_wrapper_from(&repo, &wrapper, &nested, &["review", "main", "develop"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("nested late failure"), "{stderr}");
    assert!(!stderr.contains("getcwd"), "{stderr}");
    assert_eq!(repo.snapshot(), before);
    assert_eq!(
        repo.read_file("nested/deeper/ignored.txt"),
        "nested original\n"
    );
}

#[test]
fn test_repository_snapshot_detects_ignored_content_mode_and_symlink_changes() {
    let repo = TempGitRepo::new();
    add_ignored_paths(&repo, "ignored.txt\nignored-link\n");
    repo.write_file("ignored.txt", "ignored original\n");
    std::fs::set_permissions(
        repo.path().join("ignored.txt"),
        std::fs::Permissions::from_mode(0o600),
    )
    .expect("ignored file mode should be set");
    std::os::unix::fs::symlink("target-a", repo.path().join("ignored-link"))
        .expect("ignored symlink should be created");
    let before = repo.snapshot();

    repo.write_file("ignored.txt", "ignored changed\n");
    std::fs::set_permissions(
        repo.path().join("ignored.txt"),
        std::fs::Permissions::from_mode(0o755),
    )
    .expect("ignored file mode should change");
    std::fs::remove_file(repo.path().join("ignored-link"))
        .expect("ignored symlink should be removable");
    std::os::unix::fs::symlink("target-b", repo.path().join("ignored-link"))
        .expect("ignored symlink target should change");
    let after = repo.snapshot();

    assert_ne!(after, before, "snapshot must include ignored entry state");
}

#[test]
fn test_late_failure_restores_remote_refs_and_git_admin_files() {
    let (repo, _) = setup_linear_range();
    let stale_tracking = repo.rev_parse("refs/remotes/origin/develop");
    repo.switch_branch("develop");
    repo.write_file("remote-new.txt", "new remote content\n");
    repo.git(&["add", "remote-new.txt"]);
    repo.commit("Advance remote develop");
    repo.git(&["push", "origin", "develop"]);
    repo.git(&["update-ref", "refs/remotes/origin/develop", &stale_tracking]);
    repo.switch_branch("main");
    let admin_fixtures = [
        ("FETCH_HEAD", b"original fetch head\n".as_slice()),
        ("ORIG_HEAD", b"original orig head\n".as_slice()),
        ("SQUASH_MSG", b"original squash message\n".as_slice()),
    ];
    for &(name, content) in &admin_fixtures {
        std::fs::write(repo.git_path(name), content).expect("admin fixture should be written");
    }
    let before = repo.snapshot();
    let before_remote = repo.rev_parse("refs/remotes/origin/develop");
    let before_admin: Vec<_> = admin_fixtures
        .iter()
        .map(|(name, _)| (*name, std::fs::read(repo.git_path(name)).unwrap()))
        .collect();
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\ncase \"$*\" in\n  *'config --local --replace-all branch.review-main-develop.cresca-scope'*)\n    \"$CRESCA_REAL_GIT\" \"$@\" || exit $?\n    printf 'late admin-state failure\\n' >&2\n    exit 56\n    ;;\nesac\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = run_cresca_with_git_wrapper(&repo, &wrapper, &["review", "main", "develop"]);

    assert!(!output.status.success());
    assert_eq!(repo.snapshot(), before);
    assert_eq!(repo.rev_parse("refs/remotes/origin/develop"), before_remote);
    for (name, expected) in before_admin {
        assert_eq!(
            std::fs::read(repo.git_path(name)).unwrap(),
            expected,
            "{name}"
        );
    }
}

#[test]
fn test_partial_reconcile_failure_still_restores_independent_paths() {
    let (repo, _) = setup_linear_range();
    add_ignored_paths(&repo, "a-restore.txt\nzz-blocked/\n");
    repo.write_file("a-restore.txt", "must be restored\n");
    repo.write_file("zz-blocked/child.txt", "blocked original\n");
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\ncase \"$*\" in\n  *'config --local --replace-all branch.review-main-develop.cresca-scope'*)\n    \"$CRESCA_REAL_GIT\" \"$@\" || exit $?\n    /bin/rm -f a-restore.txt\n    /bin/chmod 000 zz-blocked\n    printf 'original injected failure\\n' >&2\n    exit 57\n    ;;\nesac\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = run_cresca_with_git_wrapper(&repo, &wrapper, &["review", "main", "develop"]);
    let blocked = repo.path().join("zz-blocked");
    if blocked.exists() {
        let _ = std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o700));
    }

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("original injected failure"), "{stderr}");
    assert!(
        stderr.contains("Rollback or verification also failed"),
        "{stderr}"
    );
    assert_eq!(repo.read_file("a-restore.txt"), "must be restored\n");
}

#[test]
fn test_successful_review_publishes_fetched_source_tracking_ref() {
    let (repo, _) = setup_linear_range();
    let stale_source = repo.rev_parse("refs/remotes/origin/develop");
    repo.switch_branch("develop");
    repo.write_file("advanced-source.txt", "advanced remote source\n");
    repo.git(&["add", "advanced-source.txt"]);
    repo.commit("Advance source after tracking ref became stale");
    let advanced_source = repo.rev_parse("HEAD");
    repo.git(&["push", "origin", "develop"]);
    repo.git(&["update-ref", "refs/remotes/origin/develop", &stale_source]);
    repo.switch_branch("main");

    let review = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        review.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&review.stdout),
        String::from_utf8_lossy(&review.stderr)
    );
    assert_eq!(
        repo.rev_parse("refs/remotes/origin/develop"),
        advanced_source,
        "successful review must publish the source fetched during preflight"
    );
}

#[test]
fn test_tracking_ref_publication_failure_rolls_back_exact_state() {
    let (repo, _) = setup_linear_range();
    let before = repo.snapshot();
    let before_target = repo.rev_parse("refs/remotes/origin/main");
    let before_source = repo.rev_parse("refs/remotes/origin/develop");
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\nif [ \"$1\" = update-ref ] && [ \"$2\" = refs/remotes/origin/develop ]; then\n  \"$CRESCA_REAL_GIT\" \"$@\" || exit $?\n  printf 'injected tracking publication failure\\n' >&2\n  exit 58\nfi\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = run_cresca_with_git_wrapper(&repo, &wrapper, &["review", "main", "develop"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("injected tracking publication failure"),
        "{stderr}"
    );
    assert_eq!(repo.snapshot(), before);
    assert_eq!(repo.rev_parse("refs/remotes/origin/main"), before_target);
    assert_eq!(repo.rev_parse("refs/remotes/origin/develop"), before_source);
}

#[test]
fn test_isolated_fetch_does_not_auto_follow_tags_on_late_failure() {
    let (repo, _) = setup_linear_range();
    repo.switch_branch("develop");
    repo.write_file("tagged-source.txt", "tagged source\n");
    repo.git(&["add", "tagged-source.txt"]);
    repo.commit("Add remotely tagged source");
    repo.git(&["tag", "remote-review-tag"]);
    repo.git(&["push", "origin", "develop", "refs/tags/remote-review-tag"]);
    repo.git(&["tag", "-d", "remote-review-tag"]);
    repo.switch_branch("main");
    assert!(!repo.ref_exists("refs/tags/remote-review-tag"));
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\ncase \" $* \" in\n  *' fetch '*)\n    case \" $* \" in\n      *' --no-tags '*) ;;\n      *) printf 'isolated fetch omitted --no-tags\\n' >&2; exit 59 ;;\n    esac\n    ;;\nesac\ncase \"$*\" in\n  *'config --local --replace-all branch.review-main-develop.cresca-scope'*)\n    \"$CRESCA_REAL_GIT\" \"$@\" || exit $?\n    printf 'late tagged review failure\\n' >&2\n    exit 60\n    ;;\nesac\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = run_cresca_with_git_wrapper(&repo, &wrapper, &["review", "main", "develop"]);

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("late tagged review failure"));
    assert!(
        !repo.ref_exists("refs/tags/remote-review-tag"),
        "isolated fetch must not auto-follow a newly reachable remote tag"
    );
}

#[test]
fn test_fatal_remote_tracking_probe_is_rendered_without_mutation() {
    let (repo, _) = setup_linear_range();
    let before = repo.snapshot();
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\nif [ \"$1\" = rev-parse ] && [ \"$2\" = --verify ] && [ \"$3\" = --quiet ]; then\n  printf 'fatal remote tracking probe\\n' >&2\n  exit 128\nfi\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = run_cresca_with_git_wrapper(&repo, &wrapper, &["review", "main", "develop"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("verify if branch is already a remote tracking branch"),
        "{stderr}"
    );
    assert!(stderr.contains("exit status: 128"), "{stderr}");
    assert!(stderr.contains("fatal remote tracking probe"), "{stderr}");
    assert_eq!(repo.snapshot(), before);
}

#[test]
fn test_fatal_upstream_probe_is_rendered_without_mutation() {
    let (repo, _) = setup_linear_range();
    repo.git(&["update-ref", "-d", "refs/remotes/origin/main"]);
    repo.git(&["update-ref", "-d", "refs/remotes/origin/develop"]);
    let before = repo.snapshot();
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\nif [ \"$1\" = rev-parse ] && [ \"$2\" = --abbrev-ref ]; then\n  case \"$3\" in\n    *'@{upstream}') printf 'fatal upstream probe\\n' >&2; exit 128 ;;\n  esac\nfi\nif [ \"$1\" = for-each-ref ]; then\n  case \"$*\" in\n    *'%(upstream:short)'*) printf 'fatal upstream probe\\n' >&2; exit 128 ;;\n  esac\nfi\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = run_cresca_with_git_wrapper(&repo, &wrapper, &["review", "main", "develop"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("get upstream branch"), "{stderr}");
    assert!(stderr.contains("exit status: 128"), "{stderr}");
    assert!(stderr.contains("fatal upstream probe"), "{stderr}");
    assert_eq!(repo.snapshot(), before);
}

#[test]
fn test_fatal_scope_object_probe_is_rendered_without_mutation() {
    let repo = setup_identity_only_review_branch();
    let scope = format!("1:{}", repo.rev_parse("HEAD"));
    repo.git(&[
        "config",
        "--local",
        "branch.review-main-develop.cresca-scope",
        &scope,
    ]);
    let before = repo.snapshot();
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\nif [ \"$1\" = rev-parse ] && [ \"$2\" = --verify ]; then\n  printf 'fatal scope object probe\\n' >&2\n  exit 128\nfi\nif [ \"$1\" = cat-file ]; then\n  case \"$2\" in\n    --batch-check*) printf 'fatal scope object probe\\n' >&2; exit 128 ;;\n  esac\nfi\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = run_cresca_with_git_wrapper(&repo, &wrapper, &["status"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("validate review range endpoint"),
        "{stderr}"
    );
    assert!(stderr.contains("exit status: 128"), "{stderr}");
    assert!(stderr.contains("fatal scope object probe"), "{stderr}");
    assert_eq!(repo.snapshot(), before);
}

#[test]
fn test_late_failure_restores_commit_editmsg_and_preserves_rerere_state() {
    let (repo, range) = setup_linear_range();
    repo.git(&["config", "rerere.enabled", "true"]);
    let commit_editmsg = repo.git_path("COMMIT_EDITMSG");
    let merge_rr = repo.git_path("MERGE_RR");
    let rr_cache = repo.git_path("rr-cache");
    std::fs::write(&commit_editmsg, b"original commit edit message\n")
        .expect("COMMIT_EDITMSG fixture should be written");
    std::fs::write(&merge_rr, b"original merge rr state\n")
        .expect("MERGE_RR fixture should be written");
    std::fs::create_dir_all(rr_cache.join("fixture"))
        .expect("rerere fixture directory should be created");
    std::fs::write(rr_cache.join("fixture/preimage"), b"original preimage\n")
        .expect("rerere preimage should be written");
    std::fs::write(rr_cache.join("fixture/postimage"), b"original postimage\n")
        .expect("rerere postimage should be written");
    let before_commit_editmsg = std::fs::read(&commit_editmsg).unwrap();
    let before_merge_rr = std::fs::read(&merge_rr).unwrap();
    let before_rr_cache = snapshot_tree(&rr_cache);
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\ncase \" $* \" in\n  *' merge '*)\n    case \"$*\" in\n      '-c rerere.enabled=false merge '*) ;;\n      *) printf 'review merge inherited rerere\\n' >&2; exit 60 ;;\n    esac\n    ;;\nesac\ncase \"$*\" in\n  *'config --local --replace-all branch.review-main-develop.cresca-scope'*)\n    \"$CRESCA_REAL_GIT\" \"$@\" || exit $?\n    printf 'late rerere-state failure\\n' >&2\n    exit 61\n    ;;\nesac\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = run_cresca_with_git_wrapper(
        &repo,
        &wrapper,
        &["review", "main", "develop", "--skip-to", &range.b[..8]],
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("late rerere-state failure"), "{stderr}");
    assert_eq!(
        std::fs::read(commit_editmsg).unwrap(),
        before_commit_editmsg
    );
    assert_eq!(std::fs::read(merge_rr).unwrap(), before_merge_rr);
    assert_eq!(snapshot_tree(&rr_cache), before_rr_cache);
}

#[test]
fn test_unchanged_restrictive_ignored_entries_are_not_recreated_during_rollback() {
    let (repo, _) = setup_linear_range();
    add_ignored_paths(&repo, "restrictive/\n");
    let directory = repo.path().join("restrictive");
    std::fs::create_dir(&directory).expect("restrictive directory should be created");
    repo.write_file("restrictive/readonly.txt", "unchanged readonly content\n");
    std::os::unix::fs::symlink("readonly.txt", directory.join("readonly-link"))
        .expect("restrictive symlink should be created");
    std::fs::set_permissions(
        directory.join("readonly.txt"),
        std::fs::Permissions::from_mode(0o444),
    )
    .unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o555)).unwrap();
    let _mode_guard = RestoreDirectoryMode {
        path: directory.clone(),
    };
    let before = repo.snapshot();
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\ncase \"$*\" in\n  *'config --local --replace-all branch.review-main-develop.cresca-scope'*)\n    \"$CRESCA_REAL_GIT\" \"$@\" || exit $?\n    printf 'late restrictive no-op failure\\n' >&2\n    exit 62\n    ;;\nesac\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = run_cresca_with_git_wrapper(&repo, &wrapper, &["review", "main", "develop"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("late restrictive no-op failure"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("Rollback or verification also failed"),
        "{stderr}"
    );
    assert_eq!(repo.snapshot(), before);
}

#[test]
fn test_changed_entry_beneath_restrictive_directory_is_restored() {
    let (repo, _) = setup_linear_range();
    add_ignored_paths(&repo, "restrictive/\n");
    let directory = repo.path().join("restrictive");
    std::fs::create_dir(&directory).expect("restrictive directory should be created");
    repo.write_file("restrictive/readonly.txt", "original readonly content\n");
    std::os::unix::fs::symlink("readonly.txt", directory.join("readonly-link"))
        .expect("restrictive symlink should be created");
    std::fs::set_permissions(
        directory.join("readonly.txt"),
        std::fs::Permissions::from_mode(0o444),
    )
    .unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o555)).unwrap();
    let _mode_guard = RestoreDirectoryMode {
        path: directory.clone(),
    };
    let before = repo.snapshot();
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\ncase \"$*\" in\n  *'config --local --replace-all branch.review-main-develop.cresca-scope'*)\n    \"$CRESCA_REAL_GIT\" \"$@\" || exit $?\n    /bin/chmod 755 restrictive\n    /bin/chmod 644 restrictive/readonly.txt\n    printf 'changed content\\n' > restrictive/readonly.txt\n    /bin/rm -f restrictive/readonly-link\n    /bin/ln -s changed-target restrictive/readonly-link\n    /bin/chmod 555 restrictive\n    printf 'late restrictive mutation failure\\n' >&2\n    exit 63\n    ;;\nesac\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = run_cresca_with_git_wrapper(&repo, &wrapper, &["review", "main", "develop"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("late restrictive mutation failure"),
        "{stderr}"
    );
    assert_eq!(repo.snapshot(), before);
}

#[test]
fn test_failed_backup_copy_restores_widened_file_mode_and_continues() {
    let (repo, _) = setup_linear_range();
    add_ignored_paths(&repo, "copy-failure/\n");
    let directory = repo.path().join("copy-failure");
    std::fs::create_dir(&directory).expect("copy-failure directory should be created");
    repo.write_file("copy-failure/readonly.txt", "original readonly content\n");
    repo.write_file(
        "copy-failure/zz-independent.txt",
        "independent original content\n",
    );
    std::fs::set_permissions(
        directory.join("readonly.txt"),
        std::fs::Permissions::from_mode(0o444),
    )
    .unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o555)).unwrap();
    let _mode_guard = RestoreDirectoryMode {
        path: directory.clone(),
    };
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\ncase \"$*\" in\n  *'config --local --replace-all branch.review-main-develop.cresca-scope'*)\n    \"$CRESCA_REAL_GIT\" \"$@\" || exit $?\n    /bin/chmod 755 copy-failure\n    /bin/chmod 644 copy-failure/readonly.txt\n    printf 'changed readonly content\\n' > copy-failure/readonly.txt\n    /bin/chmod 444 copy-failure/readonly.txt\n    /bin/rm -f copy-failure/zz-independent.txt\n    /bin/chmod 555 copy-failure\n    for backup in .git/cresca-review-*/worktree/copy-failure/readonly.txt; do\n      if [ -f \"$backup\" ]; then /bin/rm -f \"$backup\"; fi\n    done\n    printf 'late backup-copy failure\\n' >&2\n    exit 65\n    ;;\nesac\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = run_cresca_with_git_wrapper(&repo, &wrapper, &["review", "main", "develop"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("late backup-copy failure"), "{stderr}");
    assert!(
        stderr.contains("restore file `copy-failure/readonly.txt`"),
        "{stderr}"
    );
    assert_eq!(
        std::fs::symlink_metadata(directory.join("readonly.txt"))
            .unwrap()
            .mode()
            & 0o777,
        0o444,
        "a failed backup copy must not leave the user's file writable"
    );
    assert_eq!(
        repo.read_file("copy-failure/zz-independent.txt"),
        "independent original content\n",
        "a failed path must not skip later independent restoration"
    );
}

#[test]
fn test_generated_nested_mode_zero_directory_is_removed_on_rollback() {
    let (repo, _) = setup_linear_range();
    let before = repo.snapshot();
    let generated = repo.path().join("generated-locked");
    let nested = generated.join("nested");
    let _nested_guard = RestoreDirectoryMode {
        path: nested.clone(),
    };
    let _generated_guard = RestoreDirectoryMode {
        path: generated.clone(),
    };
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\ncase \"$*\" in\n  *'config --local --replace-all branch.review-main-develop.cresca-scope'*)\n    \"$CRESCA_REAL_GIT\" \"$@\" || exit $?\n    /bin/mkdir -p generated-locked/nested\n    printf 'generated locked content\\n' > generated-locked/nested/file.txt\n    /bin/chmod 000 generated-locked/nested\n    /bin/chmod 000 generated-locked\n    printf 'late generated-directory failure\\n' >&2\n    exit 66\n    ;;\nesac\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = run_cresca_with_git_wrapper(&repo, &wrapper, &["review", "main", "develop"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("late generated-directory failure"),
        "{stderr}"
    );
    let survived = generated.exists();
    if survived {
        let _ = std::fs::set_permissions(&generated, std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o755));
    }
    assert!(!survived, "generated mode-000 directory survived rollback");
    assert_eq!(repo.snapshot(), before);
}

#[test]
fn test_review_preserves_custom_tmpdir_for_git_helpers() {
    let (repo, _) = setup_linear_range();
    let custom_tmpdir = tempfile::TempDir::new().expect("custom TMPDIR should be created");
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\nif [ \"$TMPDIR\" != \"$CRESCA_EXPECTED_TMPDIR\" ]; then\n  printf 'TMPDIR changed: %s\\n' \"$TMPDIR\" >&2\n  exit 64\nfi\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = std::process::Command::new(TempGitRepo::cresca_binary())
        .args(["review", "main", "develop"])
        .env("NO_COLOR", "1")
        .env("PATH", wrapper.path())
        .env("CRESCA_REAL_GIT", real_git_path())
        .env("TMPDIR", custom_tmpdir.path())
        .env("CRESCA_EXPECTED_TMPDIR", custom_tmpdir.path())
        .current_dir(repo.path())
        .output()
        .expect("Failed to execute cresca");

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_git_failure_renders_description_args_status_stdout_and_stderr() {
    let repo = TempGitRepo::new();
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\nprintf 'captured stdout\\n'\nprintf 'captured stderr\\n' >&2\nexit 42\n",
    );

    let output = std::process::Command::new(TempGitRepo::cresca_binary())
        .args(["review", "main", "develop"])
        .env("NO_COLOR", "1")
        .env("PATH", wrapper.path())
        .current_dir(repo.path())
        .output()
        .expect("Failed to execute cresca");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("locate repository worktree"), "{stderr}");
    assert!(stderr.contains("rev-parse --show-toplevel"), "{stderr}");
    assert!(stderr.contains("exit status: 42"), "{stderr}");
    assert!(stderr.contains("captured stdout"), "{stderr}");
    assert!(stderr.contains("captured stderr"), "{stderr}");
}

#[test]
fn test_review_ref_update_failure_rolls_back_exact_repository_state() {
    let (repo, _) = setup_linear_range();
    let before = repo.snapshot();
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\nif [ \"$1\" = checkout ] && [ \"$2\" = -b ]; then\n  \"$CRESCA_REAL_GIT\" \"$@\" || exit $?\n  mkdir generated-by-failed-review\n  printf 'generated content\\n' > generated-by-failed-review/file.txt\n  printf 'injected ref update failure\\n' >&2\n  exit 47\nfi\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = run_cresca_with_git_wrapper(&repo, &wrapper, &["review", "main", "develop"]);

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("create review branch from merge-base")
    );
    assert_eq!(repo.snapshot(), before);
    assert!(!repo.ref_exists("refs/heads/review-main-develop"));
    assert!(!repo.path().join("generated-by-failed-review").exists());
}

#[test]
fn test_rereview_merge_tree_failure_preserves_previous_review_exactly() {
    let repo = TempGitRepo::new();
    repo.create_branch("develop");
    repo.write_file("approved.txt", "approved\n");
    repo.git(&["add", "approved.txt"]);
    repo.commit("Add approved content");
    repo.git(&["push", "-u", "origin", "develop"]);
    repo.switch_branch("main");
    assert!(repo
        .run_cresca(&["review", "main", "develop"])
        .status
        .success());
    repo.git(&["add", "approved.txt"]);
    assert!(repo.run_cresca(&["approve"]).status.success());

    repo.switch_branch("main");
    repo.write_file("target.txt", "target baseline\n");
    repo.git(&["add", "target.txt"]);
    repo.commit("Advance target");
    repo.git(&["push", "origin", "main"]);
    repo.git(&["checkout", "-B", "develop", "main"]);
    repo.write_file("approved.txt", "approved\n");
    repo.git(&["add", "approved.txt"]);
    repo.commit("Rebase approved content");
    repo.git(&["push", "--force", "origin", "develop"]);
    repo.switch_branch("review-main-develop");
    let before = repo.snapshot();
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\ncase \" $* \" in\n  *' merge-tree '*' -Xours '*)\n    printf 'injected merge-tree stdout\\n'\n    printf 'injected merge-tree failure\\n' >&2\n    exit 63\n    ;;\nesac\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = run_cresca_with_git_wrapper(&repo, &wrapper, &["review", "main", "develop"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("reconstruct approved tree"), "{stderr}");
    assert!(stderr.contains("injected merge-tree stdout"), "{stderr}");
    assert!(stderr.contains("injected merge-tree failure"), "{stderr}");
    assert_eq!(repo.snapshot(), before);
    assert!(repo.run_cresca(&["status"]).status.success());
}

#[test]
fn test_post_transaction_status_failure_rolls_back_new_review_exactly() {
    let (repo, _) = setup_linear_range();
    let before = repo.snapshot();
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\nif [ \"$1\" = status ] && [ \"$2\" = --porcelain ]; then\n  count_file=\"$0.count\"\n  count=0\n  if [ -f \"$count_file\" ]; then IFS= read -r count < \"$count_file\"; fi\n  count=$((count + 1))\n  printf '%s\\n' \"$count\" > \"$count_file\"\n  if [ \"$count\" -eq 2 ]; then\n    printf 'injected post-transaction status failure\\n' >&2\n    exit 52\n  fi\nfi\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = run_cresca_with_git_wrapper(&repo, &wrapper, &["review", "main", "develop"]);

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("check working directory status"));
    assert_eq!(repo.snapshot(), before);
    assert!(!repo.ref_exists("refs/heads/review-main-develop"));
    assert_eq!(
        repo.review_metadata_values("review-main-develop"),
        (Vec::new(), Vec::new(), Vec::new())
    );
    assert!(repo.review_scope_values("review-main-develop").is_empty());
}

#[test]
fn test_review_rejects_unsupported_fifo_before_mutation_and_preserves_it() {
    let (repo, _) = setup_linear_range();
    repo.write_file(".gitignore", "review.pipe\n");
    repo.git(&["add", ".gitignore"]);
    repo.commit("Ignore review FIFO fixture");
    repo.git(&["push", "origin", "main"]);
    let fifo_path = repo.path().join("review.pipe");
    let mkfifo = std::process::Command::new("mkfifo")
        .arg(&fifo_path)
        .output()
        .expect("mkfifo should be executable");
    assert!(
        mkfifo.status.success(),
        "mkfifo failed: {}",
        String::from_utf8_lossy(&mkfifo.stderr)
    );
    let before = repo.snapshot();
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\nif [ \"$1\" = checkout ] && [ \"$2\" = -b ]; then\n  \"$CRESCA_REAL_GIT\" \"$@\" || exit $?\n  printf 'mutation should not be reached\\n' >&2\n  exit 53\nfi\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = run_cresca_with_git_wrapper(&repo, &wrapper, &["review", "main", "develop"]);

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unsupported filesystem entry"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.snapshot(), before);
    assert!(
        std::fs::symlink_metadata(&fifo_path)
            .expect("pre-existing FIFO must survive")
            .file_type()
            .is_fifo(),
        "pre-existing FIFO should remain a FIFO"
    );
    assert!(!repo.ref_exists("refs/heads/review-main-develop"));
    assert_eq!(
        repo.review_metadata_values("review-main-develop"),
        (Vec::new(), Vec::new(), Vec::new())
    );
}

#[test]
fn test_review_config_failure_removes_partial_identity_scope_and_restores_config_exactly() {
    let (repo, _) = setup_linear_range();
    repo.git(&["config", "--local", "--add", "test.duplicate", "first"]);
    repo.git(&["config", "--local", "--add", "test.duplicate", "second"]);
    let before = repo.snapshot();
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\ncase \"$*\" in\n  *'config --local --replace-all branch.review-main-develop.cresca-source'*)\n    \"$CRESCA_REAL_GIT\" \"$@\" || exit $?\n    printf 'partial config stdout\\n'\n    printf 'injected config failure\\n' >&2\n    exit 48\n    ;;\nesac\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = run_cresca_with_git_wrapper(&repo, &wrapper, &["review", "main", "develop"]);

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("record review source"));
    assert_eq!(repo.snapshot(), before);
    assert!(!repo.ref_exists("refs/heads/review-main-develop"));
    assert_eq!(
        repo.review_metadata_values("review-main-develop"),
        (Vec::new(), Vec::new(), Vec::new())
    );
    assert!(repo.review_scope_values("review-main-develop").is_empty());
    assert_eq!(
        repo.git_config_values("test.duplicate"),
        vec!["first", "second"]
    );
}

#[test]
fn test_review_materialization_failure_restores_index_worktree_and_generated_paths() {
    let (repo, _) = setup_linear_range();
    std::fs::create_dir_all(repo.path().join("pre-existing-empty/nested"))
        .expect("empty directory fixture should be created");
    let before = repo.snapshot();
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\nif [ \"$1\" = reset ]; then\n  \"$CRESCA_REAL_GIT\" \"$@\" || exit $?\n  mkdir generated-after-materialization\n  printf 'generated content\\n' > generated-after-materialization/file.txt\n  printf 'injected materialization failure\\n' >&2\n  exit 49\nfi\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = run_cresca_with_git_wrapper(&repo, &wrapper, &["review", "main", "develop"]);

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unstage changes for review"));
    assert_eq!(repo.snapshot(), before);
    assert!(!repo.path().join("generated-after-materialization").exists());
}

#[test]
fn test_repository_snapshot_includes_raw_config_index_and_empty_directories() {
    let repo = TempGitRepo::new();
    std::fs::create_dir_all(repo.path().join("empty/also-empty"))
        .expect("empty directory fixture should be created");

    let snapshot = repo.snapshot();

    assert_eq!(snapshot.raw_local_config, repo.raw_local_config_bytes());
    assert_eq!(snapshot.raw_index, repo.real_index_bytes());
    assert!(snapshot
        .directories
        .contains(&std::path::PathBuf::from("empty")));
    assert!(snapshot
        .directories
        .contains(&std::path::PathBuf::from("empty/also-empty")));
}

#[test]
fn test_failed_rereview_restores_previous_review_and_status_remains_usable() {
    let (repo, range) = setup_linear_range();
    assert!(repo
        .run_cresca(&["review", "main", "develop", "--stop-at", &range.c[..8]])
        .status
        .success());
    repo.git(&["add", "-A"]);
    assert!(repo.run_cresca(&["approve"]).status.success());
    let before = repo.snapshot();
    let old_scope = repo.review_scope_values("review-main-develop");
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\ncase \"$*\" in\n  *'config --local --replace-all branch.review-main-develop.cresca-scope'*)\n    \"$CRESCA_REAL_GIT\" \"$@\" || exit $?\n    printf 'injected rereview scope failure\\n' >&2\n    exit 50\n    ;;\nesac\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = run_cresca_with_git_wrapper(
        &repo,
        &wrapper,
        &["review", "main", "develop", "--stop-at", &range.d[..8]],
    );

    assert!(!output.status.success());
    assert_eq!(repo.snapshot(), before);
    assert_eq!(repo.review_scope_values("review-main-develop"), old_scope);
    let status = repo.run_cresca(&["status"]);
    assert!(
        status.status.success(),
        "restored status failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    assert!(String::from_utf8_lossy(&status.stdout).contains("current range"));
}

#[test]
fn test_review_failure_cleans_transaction_scratch_directory() {
    let (repo, _) = setup_linear_range();
    let scratch_parent = repo
        .git_path("config")
        .parent()
        .expect("Git config should have a private parent")
        .to_path_buf();
    let wrapper = install_git_wrapper(
        &repo,
        "#!/bin/sh\nif [ \"$1\" = checkout ] && [ \"$2\" = -b ]; then\n  \"$CRESCA_REAL_GIT\" \"$@\" || exit $?\n  printf 'injected scratch cleanup failure\\n' >&2\n  exit 51\nfi\nexec \"$CRESCA_REAL_GIT\" \"$@\"\n",
    );

    let output = std::process::Command::new(TempGitRepo::cresca_binary())
        .args(["review", "main", "develop"])
        .env("NO_COLOR", "1")
        .env("PATH", wrapper.path())
        .env("CRESCA_REAL_GIT", real_git_path())
        .current_dir(repo.path())
        .output()
        .expect("Failed to execute cresca");

    assert!(!output.status.success());
    let cresca_scratch: Vec<_> = std::fs::read_dir(&scratch_parent)
        .expect("scratch parent should remain readable")
        .map(|entry| entry.expect("scratch entry should be readable").file_name())
        .filter(|name| name.to_string_lossy().starts_with("cresca-review-"))
        .collect();
    assert!(
        cresca_scratch.is_empty(),
        "transaction scratch directory must be removed by RAII: {cresca_scratch:?}"
    );
}

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
        vec![format!("2:{}:{}", range.base, range.d)]
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
        vec![format!("2:{}:{}", range.base, range.c)]
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
        vec![format!("2:{}:{}", range.base, range.c)]
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
        vec![format!("2:{}:{}", range.base, range.d)]
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
    assert_eq!(before, vec![format!("2:{}:{}", range.base, range.c)]);
    let output = repo.run_cresca(&["review", "main", "develop", "--stop-at", "does-not-exist"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("does-not-exist") && stderr.contains("is not in the range"));
    assert_eq!(repo.review_scope_values("review-main-develop"), before);
}

#[test]
fn test_rereview_migrates_version_one_scope_to_explicit_base() {
    let (repo, range) = setup_linear_range();
    assert!(repo
        .run_cresca(&["review", "main", "develop", "--stop-at", &range.c[..8]])
        .status
        .success());
    repo.git(&["add", "-A"]);
    assert!(repo.run_cresca(&["approve"]).status.success());
    repo.git(&[
        "config",
        "--local",
        "branch.review-main-develop.cresca-scope",
        &format!("1:{}", range.c),
    ]);

    let output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        repo.review_scope_values("review-main-develop"),
        vec![format!("2:{}:{}", range.base, range.d)]
    );
}

#[test]
fn test_existing_identity_review_without_scope_migrates_only_from_unique_base() {
    let repo = setup_identity_only_review_branch();
    let old_head = repo.rev_parse("HEAD");

    let output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let endpoint = repo.rev_parse("origin/develop");
    assert_eq!(repo.rev_parse("HEAD^"), old_head);
    assert_eq!(
        repo.review_scope_values("review-main-develop"),
        vec![format!("2:{old_head}:{endpoint}")]
    );
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

#[test]
fn test_review_includes_all_changes_from_source_only_merge() {
    let repo = TempGitRepo::new();
    let base = repo.rev_parse("HEAD");
    repo.create_branch("side");
    repo.write_file("side.txt", "source-only side change\n");
    repo.git(&["add", "side.txt"]);
    repo.commit("Add side change");
    repo.git(&["checkout", "-b", "develop", &base]);
    repo.write_file("direct.txt", "direct source change\n");
    repo.git(&["add", "direct.txt"]);
    repo.commit("Add direct source change");
    repo.git(&[
        "merge",
        "--no-ff",
        "side",
        "-m",
        "Merge source-only side branch",
    ]);
    let endpoint = repo.rev_parse("HEAD");
    repo.git(&["push", "-u", "origin", "develop"]);
    repo.switch_branch("main");

    let output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.rev_parse("HEAD"), base);
    assert_eq!(repo.worktree_diff(), repo.diff(&base, &endpoint));
    assert_eq!(repo.read_file("direct.txt"), "direct source change\n");
    assert_eq!(repo.read_file("side.txt"), "source-only side change\n");
}

#[test]
fn test_review_excludes_target_only_changes_after_clean_target_merge() {
    let repo = TempGitRepo::new();
    repo.create_branch("develop");
    repo.write_file(
        "source-before-merge.txt",
        "source work before target merge\n",
    );
    repo.git(&["add", "source-before-merge.txt"]);
    repo.commit("Add source work before target merge");
    repo.switch_branch("main");
    repo.write_file("target-only.txt", "updated target baseline\n");
    repo.git(&["add", "target-only.txt"]);
    repo.commit("Advance target independently");
    repo.git(&["push", "origin", "main"]);
    let updated_target = repo.rev_parse("HEAD");
    repo.switch_branch("develop");
    repo.git(&[
        "merge",
        "--no-ff",
        "main",
        "-m",
        "Merge updated target cleanly",
    ]);
    repo.write_file("source-after-merge.txt", "source work after target merge\n");
    repo.git(&["add", "source-after-merge.txt"]);
    repo.commit("Add source work after target merge");
    let endpoint = repo.rev_parse("HEAD");
    repo.git(&["push", "-u", "origin", "develop"]);
    repo.switch_branch("main");

    let output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.rev_parse("HEAD"), updated_target);
    assert_eq!(repo.worktree_diff(), repo.diff(&updated_target, &endpoint));
    let status = repo.git_stdout(&["status", "--porcelain", "--untracked-files=all"]);
    assert!(status.contains("source-before-merge.txt"), "{status}");
    assert!(status.contains("source-after-merge.txt"), "{status}");
    assert!(!status.contains("target-only.txt"), "{status}");
    assert_eq!(
        repo.read_file("target-only.txt"),
        "updated target baseline\n"
    );
}

#[test]
fn test_review_uses_endpoint_tree_when_addition_and_deletion_cancel() {
    let repo = TempGitRepo::new();
    let base = repo.rev_parse("HEAD");
    repo.create_branch("develop");
    repo.write_file("transient.txt", "temporary source content\n");
    repo.git(&["add", "transient.txt"]);
    repo.commit("Add transient source file");
    repo.git(&["rm", "transient.txt"]);
    repo.commit("Delete transient source file");
    repo.write_file("lasting.txt", "lasting endpoint content\n");
    repo.git(&["add", "lasting.txt"]);
    repo.commit("Add lasting endpoint file");
    let endpoint = repo.rev_parse("HEAD");
    repo.git(&["push", "-u", "origin", "develop"]);
    repo.switch_branch("main");

    let output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.worktree_diff(), repo.diff(&base, &endpoint));
    assert!(!repo.path().join("transient.txt").exists());
    assert_eq!(repo.read_file("lasting.txt"), "lasting endpoint content\n");
    assert_eq!(
        repo.git_stdout(&["status", "--porcelain", "--untracked-files=all"]),
        "?? lasting.txt"
    );
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
    assert_eq!(repo.rev_parse("HEAD^"), head_before);
    assert_eq!(
        repo.rev_parse("HEAD^{tree}"),
        repo.rev_parse(&format!("{head_before}^{{tree}}"))
    );
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

fn setup_multiple_merge_bases() -> TempGitRepo {
    let repo = TempGitRepo::new();
    let root = repo.rev_parse("HEAD");
    repo.create_branch("left-base");
    repo.write_file("left.txt", "left base\n");
    repo.git(&["add", "left.txt"]);
    repo.commit("Create left merge base");
    let left = repo.rev_parse("HEAD");
    repo.git(&["checkout", "-b", "right-base", &root]);
    repo.write_file("right.txt", "right base\n");
    repo.git(&["add", "right.txt"]);
    repo.commit("Create right merge base");
    let right = repo.rev_parse("HEAD");
    let left_tree = repo.rev_parse(&format!("{left}^{{tree}}"));
    let right_tree = repo.rev_parse(&format!("{right}^{{tree}}"));
    let target = repo.git_stdout(&[
        "commit-tree",
        &left_tree,
        "-p",
        &left,
        "-p",
        &right,
        "-m",
        "target merge",
    ]);
    let source = repo.git_stdout(&[
        "commit-tree",
        &right_tree,
        "-p",
        &right,
        "-p",
        &left,
        "-m",
        "source merge",
    ]);
    repo.git(&["update-ref", "refs/heads/main", &target]);
    repo.git(&["update-ref", "refs/heads/develop", &source]);
    repo.git(&["push", "--force", "origin", "main", "develop"]);
    repo.git(&["checkout", "--force", "main"]);
    repo
}

#[test]
fn test_review_fails_closed_for_multiple_merge_bases_without_mutation() {
    let repo = setup_multiple_merge_bases();
    let before = repo.snapshot();

    let output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Multiple merge bases"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
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

#[test]
fn test_rereview_reconstructs_approved_tree_after_target_update_and_source_rebase() {
    let repo = TempGitRepo::new();

    repo.create_branch("develop");
    repo.write_file("approved.txt", "approved source content\n");
    repo.git(&["add", "approved.txt"]);
    repo.commit("Add source change");
    repo.git(&["push", "-u", "origin", "develop"]);
    let old_endpoint = repo.rev_parse("HEAD");
    repo.switch_branch("main");

    let first = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    repo.git(&["add", "approved.txt"]);
    assert!(repo.run_cresca(&["approve"]).status.success());
    let old_review_head = repo.rev_parse("HEAD");

    repo.switch_branch("main");
    repo.write_file("target-only.txt", "new target baseline\n");
    repo.git(&["add", "target-only.txt"]);
    repo.commit("Advance target");
    repo.git(&["push", "origin", "main"]);
    let new_base = repo.rev_parse("HEAD");

    repo.switch_branch("develop");
    repo.git(&["rebase", "main"]);
    repo.git(&["push", "--force", "origin", "develop"]);
    let endpoint = repo.rev_parse("HEAD");
    assert_ne!(
        endpoint, old_endpoint,
        "rebase must change source commit identity"
    );
    repo.switch_branch("review-main-develop");

    let rereview = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        rereview.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&rereview.stdout),
        String::from_utf8_lossy(&rereview.stderr)
    );
    assert_eq!(repo.rev_parse("HEAD^"), old_review_head);
    assert_eq!(
        repo.rev_parse("HEAD^{tree}"),
        repo.rev_parse(&format!("{endpoint}^{{tree}}"))
    );
    assert!(repo.cached_diff().is_empty());
    assert!(repo.worktree_diff().is_empty());
    assert_eq!(
        repo.review_scope_values("review-main-develop"),
        vec![format!("2:{new_base}:{endpoint}")]
    );
}

#[test]
fn test_rereview_preserves_approved_modification_deletion_and_exposes_revert() {
    let repo = TempGitRepo::new();
    repo.write_file("modified.txt", "base modification value\n");
    repo.write_file("deleted.txt", "base deletion value\n");
    repo.write_file("reverted.txt", "base revert value\n");
    repo.git(&["add", "."]);
    repo.commit("Add approval mutation fixtures");
    repo.git(&["push", "origin", "main"]);
    repo.create_branch("develop");
    repo.write_file("modified.txt", "approved modification\n");
    repo.git(&["rm", "deleted.txt"]);
    repo.write_file("reverted.txt", "approved intermediate value\n");
    repo.git(&["add", "."]);
    repo.commit("Create approved mutations");
    repo.git(&["push", "-u", "origin", "develop"]);
    repo.switch_branch("main");
    assert!(repo
        .run_cresca(&["review", "main", "develop"])
        .status
        .success());
    repo.git(&["add", "-A"]);
    assert!(repo.run_cresca(&["approve"]).status.success());

    repo.switch_branch("develop");
    repo.write_file("modified.txt", "later modification\n");
    repo.write_file("deleted.txt", "later recreation\n");
    repo.write_file("reverted.txt", "base revert value\n");
    repo.git(&["add", "."]);
    repo.commit("Advance, recreate, and revert source changes");
    repo.git(&["push", "origin", "develop"]);
    repo.switch_branch("review-main-develop");

    let output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        repo.git_stdout(&["show", "HEAD:modified.txt"]),
        "approved modification"
    );
    assert!(!repo
        .git_maybe(&["cat-file", "-e", "HEAD:deleted.txt"])
        .status
        .success());
    assert_eq!(
        repo.git_stdout(&["show", "HEAD:reverted.txt"]),
        "approved intermediate value"
    );
    assert_eq!(repo.read_file("modified.txt"), "later modification\n");
    assert_eq!(repo.read_file("deleted.txt"), "later recreation\n");
    assert_eq!(repo.read_file("reverted.txt"), "base revert value\n");
    let status: BTreeSet<_> = repo
        .git_stdout(&["status", "--porcelain", "--untracked-files=all"])
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        status,
        BTreeSet::from([
            "M modified.txt".to_string(),
            " M reverted.txt".to_string(),
            "?? deleted.txt".to_string(),
        ])
    );
}

#[test]
fn test_rereview_downgrades_only_conflicting_text_hunks_to_new_base() {
    let repo = TempGitRepo::new();
    repo.write_file(
        "lines.txt",
        "base conflict\nkeep 1\nkeep 2\nkeep 3\nkeep 4\nbase approved\nbase untouched\n",
    );
    repo.git(&["add", "lines.txt"]);
    repo.commit("Add line reconstruction fixture");
    repo.git(&["push", "origin", "main"]);

    repo.create_branch("develop");
    repo.write_file(
        "lines.txt",
        "old source conflict\nkeep 1\nkeep 2\nkeep 3\nkeep 4\napproved line\nbase untouched\n",
    );
    repo.git(&["add", "lines.txt"]);
    repo.commit("Change source lines");
    repo.git(&["push", "-u", "origin", "develop"]);
    repo.switch_branch("main");
    assert!(repo
        .run_cresca(&["review", "main", "develop"])
        .status
        .success());
    repo.git(&["add", "lines.txt"]);
    assert!(repo.run_cresca(&["approve"]).status.success());

    repo.switch_branch("main");
    repo.write_file(
        "lines.txt",
        "target conflict\nkeep 1\nkeep 2\nkeep 3\nkeep 4\nbase approved\nbase untouched\n",
    );
    repo.git(&["add", "lines.txt"]);
    repo.commit("Change target conflict line");
    repo.git(&["push", "origin", "main"]);
    repo.git(&["checkout", "-B", "develop", "main"]);
    repo.write_file(
        "lines.txt",
        "new source conflict\nkeep 1\nkeep 2\nkeep 3\nkeep 4\napproved line\nbase untouched\n",
    );
    repo.git(&["add", "lines.txt"]);
    repo.commit("Reapply source on new target");
    repo.git(&["push", "--force", "origin", "develop"]);
    repo.switch_branch("review-main-develop");

    let output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        repo.git_stdout(&["show", "HEAD:lines.txt"]),
        "target conflict\nkeep 1\nkeep 2\nkeep 3\nkeep 4\napproved line\nbase untouched"
    );
    assert_eq!(
        repo.read_file("lines.txt"),
        "new source conflict\nkeep 1\nkeep 2\nkeep 3\nkeep 4\napproved line\nbase untouched\n"
    );
    assert!(repo
        .git_stdout(&["diff", "--name-only"])
        .contains("lines.txt"));
}

#[test]
fn test_rereview_downgrades_binary_conflict_to_new_base_entry() {
    let repo = TempGitRepo::new();
    std::fs::write(repo.path().join("binary.bin"), b"base\0value")
        .expect("binary base should be writable");
    repo.git(&["add", "binary.bin"]);
    repo.commit("Add binary fixture");
    repo.git(&["push", "origin", "main"]);

    repo.create_branch("develop");
    std::fs::write(repo.path().join("binary.bin"), b"old-source\0value")
        .expect("binary source should be writable");
    repo.git(&["add", "binary.bin"]);
    repo.commit("Change source binary");
    repo.git(&["push", "-u", "origin", "develop"]);
    repo.switch_branch("main");
    assert!(repo
        .run_cresca(&["review", "main", "develop"])
        .status
        .success());
    repo.git(&["add", "binary.bin"]);
    assert!(repo.run_cresca(&["approve"]).status.success());

    repo.switch_branch("main");
    std::fs::write(repo.path().join("binary.bin"), b"target\0value")
        .expect("binary target should be writable");
    repo.git(&["add", "binary.bin"]);
    repo.commit("Change target binary");
    repo.git(&["push", "origin", "main"]);
    repo.git(&["checkout", "-B", "develop", "main"]);
    std::fs::write(repo.path().join("binary.bin"), b"new-source\0value")
        .expect("binary endpoint should be writable");
    repo.git(&["add", "binary.bin"]);
    repo.commit("Reapply binary source");
    repo.git(&["push", "--force", "origin", "develop"]);
    repo.switch_branch("review-main-develop");

    let output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        repo.git(&["show", "HEAD:binary.bin"]).stdout,
        b"target\0value"
    );
    assert_eq!(
        std::fs::read(repo.path().join("binary.bin")).expect("binary worktree should be readable"),
        b"new-source\0value"
    );
}

#[test]
fn test_rereview_handles_mode_symlink_modify_delete_and_rename_delete_safely() {
    let repo = TempGitRepo::new();
    repo.write_file("mode.sh", "base script\n");
    repo.write_file("modify-delete.txt", "base file\n");
    repo.write_file("rename-old.txt", "rename content\n");
    std::os::unix::fs::symlink("base-target", repo.path().join("link"))
        .expect("base symlink should be created");
    repo.git(&["add", "."]);
    repo.commit("Add tree safety fixtures");
    repo.git(&["push", "origin", "main"]);

    repo.create_branch("develop");
    let mut mode = std::fs::metadata(repo.path().join("mode.sh"))
        .expect("mode fixture should exist")
        .permissions();
    mode.set_mode(0o755);
    std::fs::set_permissions(repo.path().join("mode.sh"), mode)
        .expect("mode fixture should become executable");
    repo.write_file("modify-delete.txt", "approved modification\n");
    repo.git(&["mv", "rename-old.txt", "rename-approved.txt"]);
    std::fs::remove_file(repo.path().join("link")).expect("old symlink should be removable");
    std::os::unix::fs::symlink("approved-target", repo.path().join("link"))
        .expect("approved symlink should be created");
    repo.git(&["add", "."]);
    repo.commit("Approve tree-kind changes");
    repo.git(&["push", "-u", "origin", "develop"]);
    repo.switch_branch("main");
    assert!(repo
        .run_cresca(&["review", "main", "develop"])
        .status
        .success());
    repo.git(&["add", "-A"]);
    assert!(repo.run_cresca(&["approve"]).status.success());

    repo.switch_branch("main");
    repo.write_file("mode.sh", "target script\n");
    repo.git(&["rm", "modify-delete.txt", "rename-old.txt"]);
    std::fs::remove_file(repo.path().join("link")).expect("base symlink should be removable");
    std::os::unix::fs::symlink("target-link", repo.path().join("link"))
        .expect("target symlink should be created");
    repo.git(&["add", "."]);
    repo.commit("Advance target tree kinds");
    repo.git(&["push", "origin", "main"]);
    repo.git(&["checkout", "-B", "develop", "main"]);
    let mut mode = std::fs::metadata(repo.path().join("mode.sh"))
        .expect("rebased mode fixture should exist")
        .permissions();
    mode.set_mode(0o755);
    std::fs::set_permissions(repo.path().join("mode.sh"), mode)
        .expect("rebased mode fixture should become executable");
    repo.write_file("modify-delete.txt", "new source recreation\n");
    repo.write_file("rename-approved.txt", "rename content\n");
    std::fs::remove_file(repo.path().join("link")).expect("target symlink should be removable");
    std::os::unix::fs::symlink("new-source-link", repo.path().join("link"))
        .expect("new source symlink should be created");
    repo.git(&["add", "."]);
    repo.commit("Reapply tree-kind source changes");
    repo.git(&["push", "--force", "origin", "develop"]);
    repo.switch_branch("review-main-develop");

    let output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.git_stdout(&["show", "HEAD:mode.sh"]), "target script");
    assert!(repo
        .git_stdout(&["ls-tree", "HEAD", "mode.sh"])
        .starts_with("100755 "));
    assert_eq!(repo.git_stdout(&["show", "HEAD:link"]), "target-link");
    assert!(repo
        .git_maybe(&["cat-file", "-e", "HEAD:modify-delete.txt"])
        .status
        .code()
        .is_some_and(|code| code != 0));
    assert!(repo
        .git_maybe(&["cat-file", "-e", "HEAD:rename-approved.txt"])
        .status
        .code()
        .is_some_and(|code| code != 0));
    assert_eq!(
        std::fs::read_link(repo.path().join("link")).unwrap(),
        PathBuf::from("new-source-link")
    );
    assert!(repo.path().join("modify-delete.txt").exists());
    assert!(repo.path().join("rename-approved.txt").exists());
}

#[test]
fn test_rereview_does_not_transfer_approval_through_ambiguous_identical_rename() {
    let repo = TempGitRepo::new();
    repo.write_file("left.txt", "identical base content\n");
    repo.write_file("right.txt", "identical base content\n");
    repo.git(&["add", "."]);
    repo.commit("Add identical rename candidates");
    repo.git(&["push", "origin", "main"]);
    repo.create_branch("develop");
    repo.git(&["rm", "left.txt", "right.txt"]);
    repo.write_file("renamed-identical.txt", "identical base content\n");
    repo.write_file("approved-addition.txt", "unambiguous approved addition\n");
    repo.git(&["add", "."]);
    repo.commit("Create ambiguous identical rename approval");
    repo.git(&["push", "-u", "origin", "develop"]);
    repo.switch_branch("main");
    assert!(repo
        .run_cresca(&["review", "main", "develop"])
        .status
        .success());
    repo.git(&["add", "-A"]);
    assert!(repo.run_cresca(&["approve"]).status.success());

    repo.switch_branch("main");
    repo.write_file("left.txt", "new target left content\n");
    repo.write_file("right.txt", "new target right content\n");
    repo.git(&["add", "."]);
    repo.commit("Change both ambiguous target candidates");
    repo.git(&["push", "origin", "main"]);
    let new_base = repo.rev_parse("HEAD");
    repo.git(&["checkout", "-B", "develop", "main"]);
    repo.git(&["rm", "left.txt", "right.txt"]);
    repo.write_file("renamed-identical.txt", "identical base content\n");
    repo.write_file("approved-addition.txt", "unambiguous approved addition\n");
    repo.git(&["add", "."]);
    repo.commit("Reapply ambiguous rename after base movement");
    let endpoint = repo.rev_parse("HEAD");
    repo.git(&["push", "--force", "origin", "develop"]);
    repo.switch_branch("review-main-develop");

    let output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        repo.git_stdout(&["show", "HEAD:left.txt"]),
        "new target left content"
    );
    assert_eq!(
        repo.git_stdout(&["show", "HEAD:right.txt"]),
        "new target right content"
    );
    assert!(!repo
        .git_maybe(&["cat-file", "-e", "HEAD:renamed-identical.txt"])
        .status
        .success());
    assert_eq!(
        repo.git_stdout(&["show", "HEAD:approved-addition.txt"]),
        "unambiguous approved addition"
    );
    assert_eq!(repo.worktree_diff(), repo.diff("HEAD", &endpoint));
    assert_ne!(
        repo.rev_parse("HEAD^{tree}"),
        repo.rev_parse(&format!("{new_base}^{{tree}}"))
    );
}

#[test]
fn test_rereview_downgrades_ambiguous_identical_rename_across_executable_mode() {
    let repo = TempGitRepo::new();
    repo.write_file("left-mode.txt", "identical mixed-mode content\n");
    repo.write_file("right-mode.txt", "identical mixed-mode content\n");
    repo.git(&["add", "."]);
    repo.commit("Add mixed-mode rename candidates");
    repo.git(&["push", "origin", "main"]);
    repo.create_branch("develop");
    repo.git(&["rm", "left-mode.txt", "right-mode.txt"]);
    repo.write_file("renamed-executable.txt", "identical mixed-mode content\n");
    let mut permissions = std::fs::metadata(repo.path().join("renamed-executable.txt"))
        .expect("mixed-mode destination should exist")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(repo.path().join("renamed-executable.txt"), permissions)
        .expect("mixed-mode destination should become executable");
    repo.write_file("unrelated-approved.txt", "unrelated approved content\n");
    repo.git(&["add", "."]);
    repo.commit("Approve ambiguous executable rename");
    repo.git(&["push", "-u", "origin", "develop"]);
    repo.switch_branch("main");
    assert!(repo
        .run_cresca(&["review", "main", "develop"])
        .status
        .success());
    repo.git(&["add", "-A"]);
    assert!(repo.run_cresca(&["approve"]).status.success());

    repo.switch_branch("main");
    repo.write_file("left-mode.txt", "new target left mode content\n");
    repo.write_file("right-mode.txt", "new target right mode content\n");
    repo.git(&["add", "."]);
    repo.commit("Change mixed-mode target candidates");
    repo.git(&["push", "origin", "main"]);
    repo.git(&["checkout", "-B", "develop", "main"]);
    repo.git(&["rm", "left-mode.txt", "right-mode.txt"]);
    repo.write_file("renamed-executable.txt", "identical mixed-mode content\n");
    let mut permissions = std::fs::metadata(repo.path().join("renamed-executable.txt"))
        .expect("rebased mixed-mode destination should exist")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(repo.path().join("renamed-executable.txt"), permissions)
        .expect("rebased mixed-mode destination should become executable");
    repo.write_file("unrelated-approved.txt", "unrelated approved content\n");
    repo.git(&["add", "."]);
    repo.commit("Reapply executable rename after base movement");
    let endpoint = repo.rev_parse("HEAD");
    repo.git(&["push", "--force", "origin", "develop"]);
    repo.switch_branch("review-main-develop");

    let output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        repo.git_stdout(&["show", "HEAD:left-mode.txt"]),
        "new target left mode content"
    );
    assert_eq!(
        repo.git_stdout(&["show", "HEAD:right-mode.txt"]),
        "new target right mode content"
    );
    assert!(!repo
        .git_maybe(&["cat-file", "-e", "HEAD:renamed-executable.txt"])
        .status
        .success());
    assert_eq!(
        repo.git_stdout(&["show", "HEAD:unrelated-approved.txt"]),
        "unrelated approved content"
    );
    assert_eq!(repo.worktree_diff(), repo.diff("HEAD", &endpoint));
}

#[test]
fn test_rereview_downgrades_target_side_ambiguous_identical_rename() {
    let repo = TempGitRepo::new();
    repo.write_file("target-left.txt", "identical target rename content\n");
    repo.write_file("target-right.txt", "identical target rename content\n");
    repo.git(&["add", "."]);
    repo.commit("Add target-side rename candidates");
    repo.git(&["push", "origin", "main"]);

    repo.create_branch("develop");
    repo.write_file("target-left.txt", "approved candidate modification\n");
    repo.write_file("unrelated-approved.txt", "unrelated approved content\n");
    repo.git(&["add", "."]);
    repo.commit("Approve one candidate and an unrelated addition");
    repo.git(&["push", "-u", "origin", "develop"]);
    repo.switch_branch("main");
    assert!(repo
        .run_cresca(&["review", "main", "develop"])
        .status
        .success());
    repo.git(&["add", "target-left.txt", "unrelated-approved.txt"]);
    assert!(repo.run_cresca(&["approve"]).status.success());

    repo.switch_branch("main");
    repo.git(&["rm", "target-left.txt", "target-right.txt"]);
    repo.write_file("target-renamed.txt", "identical target rename content\n");
    let mut permissions = std::fs::metadata(repo.path().join("target-renamed.txt"))
        .expect("target rename destination should exist")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(repo.path().join("target-renamed.txt"), permissions)
        .expect("target rename destination should become executable");
    repo.git(&["add", "."]);
    repo.commit("Ambiguously rename identical candidates on target");
    repo.git(&["push", "origin", "main"]);
    let new_base = repo.rev_parse("HEAD");

    repo.git(&["checkout", "-b", "expected-target-ambiguity", &new_base]);
    repo.write_file("unrelated-approved.txt", "unrelated approved content\n");
    repo.git(&["add", "unrelated-approved.txt"]);
    repo.commit("Build expected target ambiguity tree");
    let expected_tree = repo.rev_parse("HEAD^{tree}");

    repo.git(&["checkout", "-B", "develop", &new_base]);
    repo.write_file("target-renamed.txt", "approved candidate modification\n");
    repo.write_file("unrelated-approved.txt", "unrelated approved content\n");
    repo.write_file("later.txt", "unreviewed endpoint content\n");
    repo.git(&["add", "."]);
    repo.commit("Reapply source after target-side ambiguous rename");
    let endpoint = repo.rev_parse("HEAD");
    repo.git(&["push", "--force", "origin", "develop"]);
    repo.switch_branch("review-main-develop");

    let output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.rev_parse("HEAD^{tree}"), expected_tree);
    assert_eq!(
        repo.git_stdout(&["show", "HEAD:target-renamed.txt"]),
        "identical target rename content"
    );
    assert_eq!(
        repo.git_stdout(&["show", "HEAD:unrelated-approved.txt"]),
        "unrelated approved content"
    );
    assert_eq!(repo.worktree_diff(), repo.diff("HEAD", &endpoint));
}

#[test]
fn test_rereview_treats_repository_conflict_paths_as_literal_pathspecs() {
    let repo = TempGitRepo::new();
    let path_pairs = [
        ("wild*.bin", "wild-neighbor.bin"),
        ("question?.bin", "questionX.bin"),
        ("bracket[ab].bin", "bracketa.bin"),
        (":(glob)magic*.bin", "magic-neighbor.bin"),
    ];
    for (unsafe_path, neighbor) in path_pairs {
        std::fs::write(repo.path().join(unsafe_path), b"base unsafe\0content")
            .expect("unsafe base path should be writable");
        std::fs::write(repo.path().join(neighbor), b"base neighbor\0content")
            .expect("neighbor base path should be writable");
    }
    repo.git(&["add", "-A"]);
    repo.commit("Add literal conflict path fixtures");
    repo.git(&["push", "origin", "main"]);

    repo.create_branch("develop");
    for (unsafe_path, neighbor) in path_pairs {
        std::fs::write(repo.path().join(unsafe_path), b"approved unsafe\0content")
            .expect("unsafe approved path should be writable");
        std::fs::write(repo.path().join(neighbor), b"approved neighbor\0content")
            .expect("neighbor approved path should be writable");
    }
    repo.write_file("unrelated-approved.txt", "unrelated approved content\n");
    repo.git(&["add", "-A"]);
    repo.commit("Approve literal conflict paths and neighbors");
    repo.git(&["push", "-u", "origin", "develop"]);
    repo.switch_branch("main");
    assert!(repo
        .run_cresca(&["review", "main", "develop"])
        .status
        .success());
    repo.git(&["add", "-A"]);
    assert!(repo.run_cresca(&["approve"]).status.success());

    repo.switch_branch("main");
    for (unsafe_path, _) in path_pairs {
        std::fs::write(repo.path().join(unsafe_path), b"target unsafe\0content")
            .expect("unsafe target path should be writable");
    }
    repo.git(&["add", "-A"]);
    repo.commit("Conflict on literal pathspec names");
    repo.git(&["push", "origin", "main"]);
    let new_base = repo.rev_parse("HEAD");

    repo.git(&["checkout", "-b", "expected-literal-conflicts", &new_base]);
    for (_, neighbor) in path_pairs {
        std::fs::write(repo.path().join(neighbor), b"approved neighbor\0content")
            .expect("expected neighbor path should be writable");
    }
    repo.write_file("unrelated-approved.txt", "unrelated approved content\n");
    repo.git(&["add", "-A"]);
    repo.commit("Build expected literal conflict tree");
    let expected_tree = repo.rev_parse("HEAD^{tree}");

    repo.git(&["checkout", "-B", "develop", &new_base]);
    for (unsafe_path, neighbor) in path_pairs {
        std::fs::write(repo.path().join(unsafe_path), b"approved unsafe\0content")
            .expect("rebased unsafe path should be writable");
        std::fs::write(repo.path().join(neighbor), b"approved neighbor\0content")
            .expect("rebased neighbor path should be writable");
    }
    repo.write_file("unrelated-approved.txt", "unrelated approved content\n");
    repo.write_file("later.txt", "unreviewed endpoint content\n");
    repo.git(&["add", "-A"]);
    repo.commit("Reapply literal conflict source changes");
    let endpoint = repo.rev_parse("HEAD");
    repo.git(&["push", "--force", "origin", "develop"]);
    repo.switch_branch("review-main-develop");

    let output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.rev_parse("HEAD^{tree}"), expected_tree);
    for (unsafe_path, neighbor) in path_pairs {
        assert_eq!(
            repo.git(&["show", &format!("HEAD:{unsafe_path}")]).stdout,
            b"target unsafe\0content"
        );
        assert_eq!(
            repo.git(&["show", &format!("HEAD:{neighbor}")]).stdout,
            b"approved neighbor\0content"
        );
    }
    assert_eq!(repo.worktree_diff(), repo.diff("HEAD", &endpoint));
}

#[test]
fn test_rereview_treats_repository_ambiguity_paths_as_literal_pathspecs() {
    let repo = TempGitRepo::new();
    let path_groups = [
        (
            "star*.txt",
            "star-source.txt",
            "star-renamed.txt",
            "star-neighbor.txt",
            "star group content\n",
        ),
        (
            "question?.txt",
            "questionA.txt",
            "questionB.txt",
            "questionN.txt",
            "question group content\n",
        ),
        (
            "bracket[ab].txt",
            "bracketa.txt",
            "bracket-renamed.txt",
            "bracketb.txt",
            "bracket group content\n",
        ),
        (
            ":(glob)colon*.txt",
            "colon-source.txt",
            "colon-renamed.txt",
            "colon-neighbor.txt",
            "colon group content\n",
        ),
    ];
    for (unsafe_path, second_candidate, _, neighbor, content) in path_groups {
        repo.write_file(unsafe_path, content);
        repo.write_file(second_candidate, content);
        repo.write_file(neighbor, "base neighbor content\n");
    }
    repo.git(&["add", "-A"]);
    repo.commit("Add literal ambiguity path fixtures");
    repo.git(&["push", "origin", "main"]);

    repo.create_branch("develop");
    for (unsafe_path, second_candidate, destination, neighbor, content) in path_groups {
        repo.git(&[
            "--literal-pathspecs",
            "rm",
            "--",
            unsafe_path,
            second_candidate,
        ]);
        repo.write_file(destination, content);
        repo.write_file(neighbor, "approved neighbor content\n");
    }
    repo.write_file("unrelated-approved.txt", "unrelated approved content\n");
    repo.git(&["add", "-A"]);
    repo.commit("Approve literal ambiguity paths and neighbors");
    repo.git(&["push", "-u", "origin", "develop"]);
    repo.switch_branch("main");
    assert!(repo
        .run_cresca(&["review", "main", "develop"])
        .status
        .success());
    repo.git(&["add", "-A"]);
    assert!(repo.run_cresca(&["approve"]).status.success());

    repo.switch_branch("main");
    repo.write_file("target-only.txt", "new target baseline\n");
    repo.git(&["add", "target-only.txt"]);
    repo.commit("Advance target for literal ambiguity reconstruction");
    repo.git(&["push", "origin", "main"]);
    let new_base = repo.rev_parse("HEAD");

    repo.git(&["checkout", "-b", "expected-literal-ambiguity", &new_base]);
    for (_, _, _, neighbor, _) in path_groups {
        repo.write_file(neighbor, "approved neighbor content\n");
    }
    repo.write_file("unrelated-approved.txt", "unrelated approved content\n");
    repo.git(&["add", "-A"]);
    repo.commit("Build expected literal ambiguity tree");
    let expected_tree = repo.rev_parse("HEAD^{tree}");

    repo.git(&["checkout", "-B", "develop", &new_base]);
    for (unsafe_path, second_candidate, destination, neighbor, content) in path_groups {
        repo.git(&[
            "--literal-pathspecs",
            "rm",
            "--",
            unsafe_path,
            second_candidate,
        ]);
        repo.write_file(destination, content);
        repo.write_file(neighbor, "approved neighbor content\n");
    }
    repo.write_file("unrelated-approved.txt", "unrelated approved content\n");
    repo.write_file("later.txt", "unreviewed endpoint content\n");
    repo.git(&["add", "-A"]);
    repo.commit("Reapply literal ambiguity source changes");
    let endpoint = repo.rev_parse("HEAD");
    repo.git(&["push", "--force", "origin", "develop"]);
    repo.switch_branch("review-main-develop");

    let output = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(repo.rev_parse("HEAD^{tree}"), expected_tree);
    for (unsafe_path, second_candidate, destination, neighbor, content) in path_groups {
        assert_eq!(
            repo.git_stdout(&["show", &format!("HEAD:{unsafe_path}")]),
            content.trim_end()
        );
        assert_eq!(
            repo.git_stdout(&["show", &format!("HEAD:{second_candidate}")]),
            content.trim_end()
        );
        assert!(!repo
            .git_maybe(&["cat-file", "-e", &format!("HEAD:{destination}")])
            .status
            .success());
        assert_eq!(
            repo.git_stdout(&["show", &format!("HEAD:{neighbor}")]),
            "approved neighbor content"
        );
    }
    assert_eq!(repo.worktree_diff(), repo.diff("HEAD", &endpoint));
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
    let previous_review = repo.rev_parse(&format!("refs/heads/{metadata_branch}"));

    let second = repo.run_cresca(&["review", "main", "develop"]);
    assert!(
        second.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(repo.current_branch(), metadata_branch);
    assert_eq!(repo.rev_parse("HEAD^"), previous_review);
    assert_eq!(repo.rev_parse("refs/heads/review-main-develop"), legacy_oid);
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

fn assert_scope_error_preserves_state(repo: &TempGitRepo, expected_error: &str) {
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
    assert_eq!(repo.snapshot(), before);
}

#[test]
fn test_status_requires_scope_metadata_for_pr12_identity() {
    let repo = setup_identity_only_review_branch();
    assert_scope_error_preserves_state(&repo, "range metadata is missing");
}

#[test]
fn test_status_rejects_duplicate_scope_values() {
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
    assert_scope_error_preserves_state(&repo, "range metadata has duplicate values");
}

#[test]
fn test_status_rejects_unsupported_scope_version() {
    let repo = setup_identity_only_review_branch();
    let value = format!("3:{}", repo.rev_parse("HEAD"));
    repo.git(&[
        "config",
        "--local",
        "branch.review-main-develop.cresca-scope",
        &value,
    ]);
    assert_scope_error_preserves_state(&repo, "range metadata version '3' is unsupported");
}

#[test]
fn test_status_rejects_malformed_scope_oid() {
    let repo = setup_identity_only_review_branch();
    repo.git(&[
        "config",
        "--local",
        "branch.review-main-develop.cresca-scope",
        "1:not-an-oid",
    ]);
    assert_scope_error_preserves_state(&repo, "range metadata is invalid");
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
    assert_scope_error_preserves_state(&repo, "range metadata is invalid");
}

#[test]
fn test_status_rejects_unavailable_scope_commit() {
    let repo = setup_identity_only_review_branch();
    let oid = "0000000000000000000000000000000000000000";
    let value = format!("1:{oid}");
    repo.git(&[
        "config",
        "--local",
        "branch.review-main-develop.cresca-scope",
        &value,
    ]);
    assert_scope_error_preserves_state(
        &repo,
        &format!("saved review object '{oid}' is unavailable"),
    );
}

#[test]
fn test_status_reports_missing_version_two_base_oid() {
    let repo = setup_identity_only_review_branch();
    let missing_base = "0000000000000000000000000000000000000000";
    let endpoint = repo.rev_parse("HEAD");
    repo.git(&[
        "config",
        "--local",
        "branch.review-main-develop.cresca-scope",
        &format!("2:{missing_base}:{endpoint}"),
    ]);
    assert_scope_error_preserves_state(
        &repo,
        &format!("saved review object '{missing_base}' is unavailable"),
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
}

#[test]
fn test_status_default_excludes_commits_after_stop_at() {
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
}

#[test]
fn test_status_current_range_can_be_complete() {
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
}

#[test]
fn test_status_rejects_all_option() {
    let repo = TempGitRepo::new();
    let before = repo.snapshot();
    let output = repo.run_cresca(&["status", "--all"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument '--all'"));
    assert_eq!(repo.snapshot(), before);
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
