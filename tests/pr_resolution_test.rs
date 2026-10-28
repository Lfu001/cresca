mod common;

use common::TempGitRepo;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output};
use tempfile::TempDir;

const RESOLVER: &str = r##"#!/bin/sh
set -eu
[ "$#" = 1 ] && [ "$1" = 'fixed $value; literal' ] || exit 31
dir=$(dirname "$0")
cat > "$dir/pr-input.json"
pwd > "$dir/cwd"
printf 'called\n' >> "$dir/pr-calls"
cat "$dir/response.json"
"##;
const NAMING: &str = r##"#!/bin/sh
set -eu
[ "$#" = 1 ] && [ "$1" = 'fixed $value; literal' ] || exit 32
dir=$(dirname "$0")
cat > "$dir/naming-input.json"
pwd > "$dir/naming-cwd"
printf 'called\n' >> "$dir/naming-calls"
printf 'custom-review\n'
"##;

struct Hooks {
    home: TempDir,
}

impl Hooks {
    fn new(resolver: Option<&str>, naming: Option<&str>) -> Self {
        let home = TempDir::new().unwrap();
        std::fs::create_dir(home.path().join(".cresca")).unwrap();
        let hooks = Self { home };
        let mut config = String::new();
        for (name, script) in [("pr", resolver), ("naming", naming)] {
            if let Some(script) = script {
                // Spaces and shell metacharacters in the program path must remain literal.
                let program = hooks.home.path().join(format!("{name} hook $literal"));
                std::fs::write(&program, script).unwrap();
                std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
                config.push_str(&format!("[review_branch.{name}_hook]\nprogram = {:?}\nargs = ['fixed $value; literal']\n", program.to_str().unwrap()));
            }
        }
        std::fs::write(hooks.home.path().join(".cresca/config.toml"), config).unwrap();
        hooks
            .response(json!({"branches":{"target":"refs/heads/main","source":"refs/heads/topic"}}));
        hooks
    }

    fn response(&self, value: Value) {
        self.raw_response(&serde_json::to_vec(&value).unwrap());
    }

    fn raw_response(&self, bytes: &[u8]) {
        std::fs::write(self.home.path().join("response.json"), bytes).unwrap();
    }

    fn read_json(&self, name: &str) -> Value {
        serde_json::from_slice(&std::fs::read(self.home.path().join(name)).unwrap()).unwrap()
    }

    fn calls(&self, name: &str) -> usize {
        std::fs::read_to_string(self.home.path().join(format!("{name}-calls")))
            .unwrap_or_default()
            .lines()
            .count()
    }

    fn run(&self, repo: &TempGitRepo, args: &[&str]) -> Output {
        repo.run_cresca_with_home(args, self.home.path())
    }
}

fn success(output: Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn setup() -> TempGitRepo {
    let repo = TempGitRepo::new();
    repo.create_branch("topic");
    repo.write_file("approved.txt", "first\n");
    repo.write_file("pending.txt", "second\n");
    repo.git(&["add", "."]);
    repo.commit("topic changes");
    repo.switch_branch("main");
    repo
}

#[test]
fn cli_requires_exactly_one_complete_mode_before_executing_hooks() {
    let repo = setup();
    let hooks = Hooks::new(Some(RESOLVER), Some(NAMING));
    let before = repo.snapshot();
    for args in [
        vec!["review"],
        vec!["review", "main"],
        vec!["review", "--pr", ""],
        vec!["review", "--pr", "123", "main"],
        vec!["review", "main", "topic", "--pr", "123"],
        vec!["review", "--pr", "1", "--pr", "2"],
    ] {
        let output = hooks.run(&repo, &args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(hooks.calls("pr"), 0);
    assert_eq!(hooks.calls("naming"), 0);
    assert_eq!(repo.snapshot(), before);
}

#[test]
fn resolver_gets_opaque_identifier_fixed_args_eof_and_repository_root() {
    let repo = setup();
    let hooks = Hooks::new(Some(RESOLVER), Some(NAMING));
    let identifier = "https://example.test/pull/123?query=$x; unicode=日本語\nnext";
    // An ignored directory permits invoking from a subdirectory without dirtying the repo.
    repo.git(&["config", "core.excludesFile", "/dev/null"]);
    std::fs::write(repo.git_path("info/exclude"), "nested/\n").unwrap();
    std::fs::create_dir(repo.path().join("nested")).unwrap();
    let output = Command::new(TempGitRepo::cresca_binary())
        .args(["review", "--pr", identifier])
        .env("HOME", hooks.home.path())
        .env("NO_COLOR", "1")
        .current_dir(repo.path().join("nested"))
        .output()
        .unwrap();
    success(output);
    assert_eq!(
        hooks.read_json("pr-input.json"),
        json!({"request":{"pr":identifier}})
    );
    let cwd = std::fs::read_to_string(hooks.home.path().join("cwd")).unwrap();
    assert_eq!(
        std::fs::canonicalize(cwd.trim()).unwrap(),
        repo.path().canonicalize().unwrap()
    );
    let naming_cwd = std::fs::read_to_string(hooks.home.path().join("naming-cwd")).unwrap();
    assert_eq!(cwd, naming_cwd);
    assert_eq!(repo.current_branch(), "custom-review");
    assert_eq!(
        hooks.read_json("naming-input.json"),
        json!({"request":{"pr":identifier},"branches":{"target":"refs/heads/main","source":"refs/heads/topic"},"metadata":{}})
    );
}

#[test]
fn metadata_handoff_keeps_reserved_names_nested_and_handles_large_input() {
    let repo = setup();
    let naming = NAMING.replace("cat >", "cat \"$dir/noise\" >&2\ncat >");
    let hooks = Hooks::new(Some(RESOLVER), Some(&naming));
    std::fs::write(
        hooks.home.path().join("noise"),
        "diagnostic\n".repeat(20_000),
    )
    .unwrap();
    let metadata = json!({"request":{"pr":"metadata-only"},"branches":null,"ticket":[true,42,"ABC-456"],"large":"x".repeat(200_000),"large_number":serde_json::from_str::<Value>("1234567890123456789012345678901234567890").unwrap()});
    hooks.response(json!({"branches":{"target":"refs/heads/main","source":"refs/heads/topic"},"metadata":metadata}));
    success(hooks.run(&repo, &["review", "--pr=-opaque"]));
    let input = hooks.read_json("naming-input.json");
    assert_eq!(input["request"], json!({"pr":"-opaque"}));
    assert_eq!(input["metadata"], metadata);
}

#[test]
fn positional_naming_keeps_original_spellings_and_has_empty_context() {
    let repo = setup();
    let hooks = Hooks::new(Some("#!/bin/sh\nexit 47\n"), Some(NAMING));
    success(hooks.run(&repo, &["review", "main", "refs/heads/topic"]));
    assert_eq!(hooks.calls("pr"), 0);
    assert_eq!(
        hooks.read_json("naming-input.json"),
        json!({"request":{},"branches":{"target":"main","source":"refs/heads/topic"},"metadata":{}})
    );
}

#[test]
fn positional_branch_named_pr_still_works() {
    let repo = setup();
    repo.git(&["branch", "pr", "main"]);
    success(repo.run_cresca(&["review", "pr", "topic"]));
    assert_eq!(repo.current_branch(), "review-pr-topic");
}

#[test]
fn approval_continues_across_modes_and_identifiers_and_source_updates() {
    let repo = setup();
    let hooks = Hooks::new(Some(RESOLVER), Some(NAMING));
    success(hooks.run(&repo, &["review", "--pr", "123"]));
    repo.git(&["add", "approved.txt"]);
    success(hooks.run(&repo, &["approve"]));
    let approved_head = repo.rev_parse("HEAD");
    repo.switch_branch("topic");
    repo.write_file("new.txt", "new change\n");
    repo.git(&["add", "."]);
    repo.commit("source update");
    repo.switch_branch("main");
    success(hooks.run(&repo, &["review", "refs/heads/main", "refs/heads/topic"]));
    assert_eq!(repo.current_branch(), "custom-review");
    assert_eq!(repo.git_stdout(&["show", "HEAD:approved.txt"]), "first");
    assert!(!repo
        .git_maybe(&["cat-file", "-e", "HEAD:pending.txt"])
        .status
        .success());
    assert!(!repo
        .git_maybe(&["cat-file", "-e", "HEAD:new.txt"])
        .status
        .success());
    success(hooks.run(&repo, &["approve"])); // discard remaining unapproved worktree changes
    let updated_approval = repo.rev_parse("HEAD^{tree}");
    success(hooks.run(&repo, &["review", "--pr", "different-id"]));
    assert_eq!(repo.current_branch(), "custom-review");
    assert_eq!(repo.rev_parse("HEAD^{tree}"), updated_approval);
    assert_eq!(repo.git_stdout(&["show", "HEAD:approved.txt"]), "first");
    assert_eq!(repo.diff(&approved_head, "HEAD"), Vec::<u8>::new());
    assert_eq!(hooks.calls("pr"), 2);
    assert_eq!(hooks.calls("naming"), 1);
}

#[test]
fn new_positional_review_does_not_inherit_pr_context() {
    let repo = setup();
    let hooks = Hooks::new(Some(RESOLVER), Some(NAMING));
    hooks.response(json!({"branches":{"target":"refs/heads/main","source":"refs/heads/topic"},"metadata":{"ticket":"ABC"}}));
    success(hooks.run(&repo, &["review", "--pr", "123"]));
    success(hooks.run(&repo, &["approve"]));
    repo.switch_branch("main");
    repo.git(&["branch", "other", "topic"]);
    success(hooks.run(&repo, &["review", "refs/heads/main", "refs/heads/other"]));
    assert_eq!(
        hooks.read_json("naming-input.json"),
        json!({"request":{},"branches":{"target":"refs/heads/main","source":"refs/heads/other"},"metadata":{}})
    );
    assert_eq!(hooks.calls("naming"), 2);
}

#[test]
fn same_pr_with_changed_branches_follows_branch_identity() {
    let repo = setup();
    let hooks = Hooks::new(Some(RESOLVER), None);
    success(hooks.run(&repo, &["review", "--pr", "123"]));
    let first = repo.current_branch();
    success(hooks.run(&repo, &["approve"]));
    repo.switch_branch("main");
    repo.git(&["branch", "other", "topic"]);
    hooks.response(json!({"branches":{"target":"refs/heads/main","source":"refs/heads/other"}}));
    success(hooks.run(&repo, &["review", "--pr", "123"]));
    assert_ne!(repo.current_branch(), first);
    assert_eq!(hooks.calls("pr"), 2);
}

#[test]
fn missing_configuration_and_launch_failure_preserve_repository() {
    let repo = setup();
    let hooks = Hooks::new(None, None);
    let before = repo.snapshot();
    let output = hooks.run(&repo, &["review", "--pr", "123"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("[review_branch.pr_hook]"));
    assert_eq!(repo.snapshot(), before);
    std::fs::write(
        hooks.home.path().join(".cresca/config.toml"),
        "[review_branch.pr_hook]\nprogram = '/definitely/missing/pr-resolver'\n",
    )
    .unwrap();
    let output = hooks.run(&repo, &["review", "--pr", "123"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Failed to run") && stderr.contains("/definitely/missing/pr-resolver"),
        "{stderr}"
    );
    assert_eq!(repo.snapshot(), before);
    // An unavailable resolver must never affect ordinary branch review.
    success(hooks.run(&repo, &["review", "refs/heads/main", "refs/heads/topic"]));
}

#[test]
fn unsuccessful_hook_reports_status_stderr_and_ignores_valid_stdout() {
    let repo = setup();
    let hooks = Hooks::new(Some("#!/bin/sh\ncat >/dev/null\nprintf '{\"branches\":{\"target\":\"main\",\"source\":\"topic\"}}'\nprintf 'authentication failed\\n' >&2\nexit 23\n"), Some(NAMING));
    let before = repo.snapshot();
    let output = hooks.run(&repo, &["review", "--pr", "123"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("23")
            && stderr.contains("authentication failed")
            && stderr.contains("PR resolution hook"),
        "{stderr}"
    );
    assert_eq!(hooks.calls("naming"), 0);
    assert_eq!(repo.snapshot(), before);
}

#[test]
fn invalid_resolver_output_preserves_state_and_never_runs_naming() {
    let repo = setup();
    let hooks = Hooks::new(Some(RESOLVER), Some(NAMING));
    let before = repo.snapshot();
    for bytes in [
        b"not JSON".as_slice(),
        b"[]",
        b"{\"branches\":[\"main\",\"topic\"]}",
        b"{\"branches\":{\"target\":\"main\",\"source\":\"topic\"},\"request\":{}}",
        &[0xff],
    ] {
        hooks.raw_response(bytes);
        let output = hooks.run(&repo, &["review", "--pr", "123"]);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("Invalid PR resolution hook output")
        );
        assert_eq!(repo.snapshot(), before);
    }
    assert_eq!(hooks.calls("naming"), 0);
}

#[test]
fn invalid_branch_uses_existing_diagnostics_without_preparing_review() {
    let repo = setup();
    let hooks = Hooks::new(Some(RESOLVER), Some(NAMING));
    hooks.response(json!({"branches":{"target":"refs/heads/main","source":"refs/heads/missing"}}));
    let before = repo.snapshot();
    let output = hooks.run(&repo, &["review", "--pr", "123"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing"));
    assert_eq!(repo.snapshot(), before);
    assert_eq!(hooks.calls("naming"), 0);
}

#[test]
fn dirty_worktree_blocks_resolver_and_hook_changes_are_rechecked() {
    let repo = setup();
    let hooks = Hooks::new(Some(RESOLVER), Some(NAMING));
    repo.write_file("dirty.txt", "untracked\n");
    let before = repo.snapshot();
    let output = hooks.run(&repo, &["review", "--pr", "123"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Uncommitted changes"));
    assert_eq!(hooks.calls("pr"), 0);
    assert_eq!(repo.snapshot(), before);
    std::fs::remove_file(repo.path().join("dirty.txt")).unwrap();
    let hooks = Hooks::new(
        Some(&format!(
            "{RESOLVER}\nprintf 'hook side effect' > dirty.txt\n"
        )),
        Some(NAMING),
    );
    let head = repo.rev_parse("HEAD");
    let output = hooks.run(&repo, &["review", "--pr", "123"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Uncommitted changes"));
    assert_eq!(repo.current_branch(), "main");
    assert_eq!(repo.rev_parse("HEAD"), head);
    assert_eq!(repo.read_file("dirty.txt"), "hook side effect");
    assert_eq!(hooks.calls("naming"), 0);
}

#[test]
fn range_options_keep_their_effect_after_resolution() {
    let repo = setup();
    repo.switch_branch("topic");
    let first = repo.rev_parse("HEAD");
    repo.write_file("middle.txt", "middle\n");
    repo.git(&["add", "."]);
    repo.commit("middle");
    let middle = repo.rev_parse("HEAD");
    repo.write_file("last.txt", "last\n");
    repo.git(&["add", "."]);
    repo.commit("last");
    repo.switch_branch("main");
    let hooks = Hooks::new(Some(RESOLVER), None);
    success(hooks.run(
        &repo,
        &[
            "review",
            "--pr",
            "123",
            "--skip-to",
            &middle,
            "--stop-at",
            &middle,
        ],
    ));
    assert_eq!(repo.diff(&first, "HEAD"), Vec::<u8>::new());
    assert!(repo.path().join("middle.txt").exists());
    assert!(!repo.path().join("last.txt").exists());
    assert_eq!(
        hooks.read_json("pr-input.json"),
        json!({"request":{"pr":"123"}})
    );
    success(hooks.run(&repo, &["approve"]));
    let output = hooks.run(&repo, &["review", "--pr", "123", "--skip-to", "invalid"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("not in the range"));
}

#[test]
fn successful_resolver_stderr_is_verbose_only() {
    let repo = setup();
    let script = format!("{RESOLVER}\nprintf 'resolver note\\n' >&2\n");
    let hooks = Hooks::new(Some(&script), None);
    let output = hooks.run(&repo, &["review", "--pr", "123"]);
    assert!(!String::from_utf8_lossy(&output.stderr).contains("resolver note"));
    success(output);
    success(hooks.run(&repo, &["approve"]));
    let output = hooks.run(&repo, &["review", "--pr", "123", "--verbose"]);
    assert!(String::from_utf8_lossy(&output.stderr).contains("resolver note"));
    success(output);
}
