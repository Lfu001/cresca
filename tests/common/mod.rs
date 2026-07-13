use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

/// A temporary git repository for testing.
/// The repository is automatically cleaned up when this struct is dropped.
/// Includes a bare "remote" repository to simulate `git pull origin`.
pub struct TempGitRepo {
    pub dir: TempDir,
    pub remote_dir: TempDir,
}

#[derive(Debug, PartialEq, Eq)]
pub struct RepoState {
    pub branch: String,
    pub head: String,
    pub local_heads: Vec<u8>,
    pub local_config: Vec<u8>,
    pub status: Vec<u8>,
    pub cached_diff: Vec<u8>,
    pub worktree_diff: Vec<u8>,
}

impl TempGitRepo {
    /// Creates a new temporary git repository with initial setup and a fake remote.
    pub fn new() -> Self {
        let remote_dir = TempDir::new().expect("Failed to create remote temp directory");
        let dir = TempDir::new().expect("Failed to create temp directory");

        // Initialize bare remote repository
        Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .current_dir(remote_dir.path())
            .output()
            .expect("Failed to initialize bare repo");

        let repo = Self { dir, remote_dir };

        // Initialize working git repo
        repo.git(&["init", "-b", "main"]);

        // Configure git user for commits
        repo.git(&["config", "user.name", "Test User"]);
        repo.git(&["config", "user.email", "test@example.com"]);

        // Add the bare repo as origin
        let remote_path = repo.remote_dir.path().to_str().unwrap();
        repo.git(&["remote", "add", "origin", remote_path]);

        // Create initial commit (required for branching)
        repo.write_file("README.md", "# Test Repository");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-m", "Initial commit"]);

        // Push to origin
        repo.git(&["push", "-u", "origin", "main"]);

        repo
    }

    /// Returns the path to the repository.
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Runs a git command in the repository.
    pub fn git(&self, args: &[&str]) -> Output {
        let output = self.git_maybe(args);

        if !output.status.success() {
            panic!(
                "Git command failed: git {}\nstderr: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        output
    }

    /// Runs a git command in the repository without requiring it to succeed.
    pub fn git_maybe(&self, args: &[&str]) -> Output {
        Command::new("git")
            .args(args)
            .current_dir(self.path())
            .output()
            .expect("Failed to execute git command")
    }

    /// Runs a git command and returns normalized UTF-8 stdout.
    pub fn git_stdout(&self, args: &[&str]) -> String {
        String::from_utf8(self.git(args).stdout)
            .expect("git stdout should be UTF-8")
            .trim()
            .to_string()
    }

    pub fn git_config_values(&self, key: &str) -> Vec<String> {
        let output = self.git_maybe(&["config", "--local", "--get-all", key]);
        if !output.status.success() {
            return Vec::new();
        }
        String::from_utf8(output.stdout)
            .expect("git config value should be UTF-8")
            .lines()
            .map(str::to_owned)
            .collect()
    }

    pub fn review_metadata_values(&self, branch: &str) -> (Vec<String>, Vec<String>, Vec<String>) {
        (
            self.git_config_values(&format!("branch.{branch}.cresca-version")),
            self.git_config_values(&format!("branch.{branch}.cresca-target")),
            self.git_config_values(&format!("branch.{branch}.cresca-source")),
        )
    }

    pub fn review_scope_values(&self, branch: &str) -> Vec<String> {
        self.git_config_values(&format!("branch.{branch}.cresca-scope"))
    }

    /// Resolves a revision to its object ID.
    pub fn rev_parse(&self, revision: &str) -> String {
        self.git_stdout(&["rev-parse", revision])
    }

    /// Returns whether a full ref exists.
    pub fn ref_exists(&self, full_ref: &str) -> bool {
        self.git_maybe(&["show-ref", "--verify", "--quiet", full_ref])
            .status
            .success()
    }

    /// Reads a UTF-8 file from the repository.
    pub fn read_file(&self, name: &str) -> String {
        std::fs::read_to_string(self.path().join(name)).expect("test file should be readable")
    }

    /// Returns a canonical binary-safe diff between two committed states.
    pub fn diff(&self, old: &str, new: &str) -> Vec<u8> {
        self.git(&[
            "diff",
            "--binary",
            "--no-ext-diff",
            "--no-renames",
            old,
            new,
        ])
        .stdout
    }

    /// Returns the canonical diff staged in the real index.
    pub fn cached_diff(&self) -> Vec<u8> {
        self.git(&[
            "diff",
            "--cached",
            "--binary",
            "--no-ext-diff",
            "--no-renames",
            "HEAD",
        ])
        .stdout
    }

    /// Returns the logical full worktree diff without mutating the real index.
    pub fn worktree_diff(&self) -> Vec<u8> {
        let index_dir = TempDir::new().expect("Failed to create temporary index directory");
        let index_path = index_dir.path().join("index");

        let run_with_index = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .env("GIT_INDEX_FILE", &index_path)
                .current_dir(self.path())
                .output()
                .expect("Failed to execute git command with temporary index");
            assert!(
                output.status.success(),
                "git {} failed with temporary index: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
            output
        };

        run_with_index(&["read-tree", "HEAD"]);
        run_with_index(&["add", "-A"]);
        run_with_index(&[
            "diff",
            "--cached",
            "--binary",
            "--no-ext-diff",
            "--no-renames",
            "HEAD",
        ])
        .stdout
    }

    /// Returns the real index file contents without interpreting them.
    pub fn real_index_bytes(&self) -> Vec<u8> {
        let index_path = PathBuf::from(self.git_stdout(&["rev-parse", "--git-path", "index"]));
        let index_path = if index_path.is_absolute() {
            index_path
        } else {
            self.path().join(index_path)
        };
        std::fs::read(index_path).expect("real git index should be readable")
    }

    /// Captures the repository state used by integration-test assertions.
    pub fn snapshot(&self) -> RepoState {
        RepoState {
            branch: self.current_branch(),
            head: self.rev_parse("HEAD"),
            local_heads: self
                .git(&[
                    "for-each-ref",
                    "--sort=refname",
                    "--format=%(refname) %(objectname)",
                    "refs/heads/",
                ])
                .stdout,
            local_config: self
                .git(&["config", "--local", "--null", "--list", "--show-origin"])
                .stdout,
            status: self
                .git(&["status", "--porcelain=v1", "-z", "--untracked-files=all"])
                .stdout,
            cached_diff: self.cached_diff(),
            worktree_diff: self.worktree_diff(),
        }
    }

    /// Writes a file to the repository.
    pub fn write_file(&self, name: &str, content: &str) {
        let path = self.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("Failed to create parent directories");
        }
        std::fs::write(&path, content).expect("Failed to write file");
    }

    /// Creates a commit with the given message.
    pub fn commit(&self, message: &str) {
        self.git(&["commit", "-m", message]);
    }

    /// Creates a new branch from the current branch.
    pub fn create_branch(&self, name: &str) {
        self.git(&["checkout", "-b", name]);
    }

    /// Switches to an existing branch.
    pub fn switch_branch(&self, name: &str) {
        self.git(&["switch", name]);
    }

    /// Gets the current branch name.
    pub fn current_branch(&self) -> String {
        self.git_stdout(&["rev-parse", "--abbrev-ref", "HEAD"])
    }

    /// Returns the path to the cresca binary.
    pub fn cresca_binary() -> PathBuf {
        assert_cmd::cargo::cargo_bin!("cresca").to_path_buf()
    }

    /// Runs cresca with the given arguments.
    pub fn run_cresca(&self, args: &[&str]) -> Output {
        Command::new(Self::cresca_binary())
            .args(args)
            .env("NO_COLOR", "1")
            .current_dir(self.path())
            .output()
            .expect("Failed to execute cresca")
    }
}
