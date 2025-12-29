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
        let output = Command::new("git")
            .args(args)
            .current_dir(self.path())
            .output()
            .expect("Failed to execute git command");

        if !output.status.success() {
            panic!(
                "Git command failed: git {}\nstderr: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        output
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
        let output = self.git(&["rev-parse", "--abbrev-ref", "HEAD"]);
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// Returns the path to the cresca binary.
    pub fn cresca_binary() -> PathBuf {
        assert_cmd::cargo::cargo_bin!("cresca").to_path_buf()
    }

    /// Runs cresca with the given arguments.
    pub fn run_cresca(&self, args: &[&str]) -> Output {
        Command::new(Self::cresca_binary())
            .args(args)
            .current_dir(self.path())
            .output()
            .expect("Failed to execute cresca")
    }

    /// Checks if there are uncommitted changes.
    pub fn has_uncommitted_changes(&self) -> bool {
        let output = self.git(&["status", "--porcelain"]);
        !output.stdout.is_empty()
    }
}
