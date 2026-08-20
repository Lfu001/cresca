use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
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
    pub status: Vec<u8>,
    pub cached_diff: Vec<u8>,
    pub worktree_diff: Vec<u8>,
    pub raw_local_config: Vec<u8>,
    pub raw_index: Vec<u8>,
    pub directories: BTreeSet<PathBuf>,
    pub direct_worktree: BTreeMap<PathBuf, WorktreeEntryState>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum WorktreeEntryState {
    Directory { mode: u32 },
    File { mode: u32, bytes: Vec<u8> },
    Symlink { target: PathBuf },
    Other { mode: u32 },
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

    fn git_without_optional_locks(&self, args: &[&str]) -> Output {
        let output = Command::new("git")
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .current_dir(self.path())
            .output()
            .expect("Failed to execute git command without optional locks");
        assert!(
            output.status.success(),
            "Git command failed: git {}\nstderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        output
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
        std::fs::read(self.git_path("index")).expect("real git index should be readable")
    }

    /// Returns the local config file contents without Git normalization.
    pub fn raw_local_config_bytes(&self) -> Vec<u8> {
        std::fs::read(self.git_path("config")).expect("local Git config should be readable")
    }

    /// Returns every worktree directory, including empty directories.
    pub fn directory_set(&self) -> BTreeSet<PathBuf> {
        fn visit(root: &Path, directory: &Path, result: &mut BTreeSet<PathBuf>) {
            for child in
                std::fs::read_dir(directory).expect("worktree directory should be readable")
            {
                let child = child.expect("worktree entry should be readable");
                if directory == root && child.file_name() == ".git" {
                    continue;
                }
                let path = child.path();
                let metadata = std::fs::symlink_metadata(&path)
                    .expect("worktree entry metadata should be readable");
                if metadata.is_dir() && !metadata.file_type().is_symlink() {
                    let relative = path
                        .strip_prefix(root)
                        .expect("worktree directory must be under root")
                        .to_path_buf();
                    result.insert(relative);
                    visit(root, &path, result);
                }
            }
        }

        let mut result = BTreeSet::new();
        visit(self.path(), self.path(), &mut result);
        result
    }

    /// Captures direct filesystem state, including ignored entries Git diffs cannot observe.
    pub fn direct_worktree_state(&self) -> BTreeMap<PathBuf, WorktreeEntryState> {
        fn visit(
            root: &Path,
            directory: &Path,
            result: &mut BTreeMap<PathBuf, WorktreeEntryState>,
        ) {
            for child in
                std::fs::read_dir(directory).expect("worktree directory should be readable")
            {
                let child = child.expect("worktree entry should be readable");
                if directory == root && child.file_name() == ".git" {
                    continue;
                }
                let path = child.path();
                let relative = path
                    .strip_prefix(root)
                    .expect("worktree entry must be under root")
                    .to_path_buf();
                let metadata = std::fs::symlink_metadata(&path)
                    .expect("worktree entry metadata should be readable");
                let entry = if metadata.file_type().is_symlink() {
                    WorktreeEntryState::Symlink {
                        target: std::fs::read_link(&path)
                            .expect("worktree symlink target should be readable"),
                    }
                } else if metadata.is_dir() {
                    WorktreeEntryState::Directory {
                        mode: metadata.mode(),
                    }
                } else if metadata.is_file() {
                    WorktreeEntryState::File {
                        mode: metadata.mode(),
                        bytes: std::fs::read(&path).expect("worktree file should be readable"),
                    }
                } else {
                    assert!(
                        metadata.file_type().is_fifo()
                            || metadata.file_type().is_socket()
                            || metadata.file_type().is_block_device()
                            || metadata.file_type().is_char_device(),
                        "unexpected worktree entry type: {}",
                        relative.display()
                    );
                    WorktreeEntryState::Other {
                        mode: metadata.mode(),
                    }
                };
                let recurse = matches!(entry, WorktreeEntryState::Directory { .. });
                result.insert(relative, entry);
                if recurse {
                    visit(root, &path, result);
                }
            }
        }

        let mut result = BTreeMap::new();
        visit(self.path(), self.path(), &mut result);
        result
    }

    pub fn git_path(&self, name: &str) -> PathBuf {
        let path = PathBuf::from(self.git_stdout(&["rev-parse", "--git-path", name]));
        if path.is_absolute() {
            path
        } else {
            self.path().join(path)
        }
    }

    /// Captures the repository state used by integration-test assertions.
    pub fn snapshot(&self) -> RepoState {
        let branch = self.current_branch();
        let head = self.rev_parse("HEAD");
        let local_heads = self
            .git(&[
                "for-each-ref",
                "--sort=refname",
                "--format=%(refname) %(objectname)",
                "refs/heads/",
            ])
            .stdout;
        let status = self
            .git_without_optional_locks(&[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
            ])
            .stdout;
        let cached_diff = self.cached_diff();
        let worktree_diff = self.worktree_diff();
        let raw_local_config = self.raw_local_config_bytes();
        let raw_index = self.real_index_bytes();
        let directories = self.directory_set();
        let direct_worktree = self.direct_worktree_state();

        RepoState {
            branch,
            head,
            local_heads,
            status,
            cached_diff,
            worktree_diff,
            raw_local_config,
            raw_index,
            directories,
            direct_worktree,
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
        let home = TempDir::new().expect("isolated default Cresca home should be created");
        Command::new(Self::cresca_binary())
            .args(args)
            .env("HOME", home.path())
            .env("NO_COLOR", "1")
            .current_dir(self.path())
            .output()
            .expect("Failed to execute cresca")
    }

    /// Runs cresca with an isolated user home directory.
    pub fn run_cresca_with_home(&self, args: &[&str], home: &Path) -> Output {
        Command::new(Self::cresca_binary())
            .args(args)
            .env("HOME", home)
            .env("NO_COLOR", "1")
            .current_dir(self.path())
            .output()
            .expect("Failed to execute cresca with isolated home")
    }
}
