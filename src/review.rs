use crate::git::{run_git_command, GitCommandError};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

#[derive(Debug)]
pub enum ReviewError {
    Git(GitCommandError),
    Message(String),
    Rollback {
        original: Box<ReviewError>,
        diagnostics: String,
    },
}

impl From<GitCommandError> for ReviewError {
    fn from(error: GitCommandError) -> Self {
        Self::Git(error)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Entry {
    Directory { mode: u32 },
    File { mode: u32, data: Vec<u8> },
    Symlink { target: PathBuf },
}

pub struct ReviewTransaction {
    root: PathBuf,
    head_ref: Option<String>,
    head_oid: String,
    heads: BTreeMap<String, String>,
    config_path: PathBuf,
    config: Vec<u8>,
    index_path: PathBuf,
    index_existed: bool,
    worktree: BTreeMap<PathBuf, Entry>,
    scratch: TempDir,
    mutation_started: bool,
    finished: bool,
    verbose: bool,
}

impl ReviewTransaction {
    pub fn begin(verbose: bool) -> Result<Self, ReviewError> {
        let root_output = run_git_command(
            "locate repository worktree",
            &["rev-parse", "--show-toplevel"],
            false,
            verbose,
        )?;
        let root = PathBuf::from(String::from_utf8_lossy(&root_output.stdout).trim());
        let head_oid = git_stdout("capture original HEAD", &["rev-parse", "HEAD"], verbose)?;
        let symbolic = run_git_command(
            "capture original branch",
            &["symbolic-ref", "--quiet", "HEAD"],
            true,
            verbose,
        )?;
        let head_ref = symbolic
            .status
            .success()
            .then(|| String::from_utf8_lossy(&symbolic.stdout).trim().to_string());
        let heads = read_heads(verbose)?;
        let config_path = resolve_git_path(&root, "config", verbose)?;
        let index_path = resolve_git_path(&root, "index", verbose)?;
        let config = fs::read(&config_path).map_err(|error| {
            ReviewError::Message(format!("failed to snapshot local config: {error}"))
        })?;
        let index = match fs::read(&index_path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(ReviewError::Message(format!(
                    "failed to snapshot real index: {error}"
                )))
            }
        };
        let worktree = scan_worktree(&root).map_err(|error| {
            ReviewError::Message(format!("failed to snapshot worktree: {error}"))
        })?;
        let scratch = tempfile::Builder::new()
            .prefix("cresca-review-")
            .tempdir()
            .map_err(|error| {
                ReviewError::Message(format!("failed to create scratch area: {error}"))
            })?;
        if let Some(index) = &index {
            fs::write(scratch.path().join("index"), index).map_err(|error| {
                ReviewError::Message(format!("failed to back up real index: {error}"))
            })?;
        }

        Ok(Self {
            root,
            head_ref,
            head_oid,
            heads,
            config_path,
            config,
            index_path,
            index_existed: index.is_some(),
            worktree,
            scratch,
            mutation_started: false,
            finished: false,
            verbose,
        })
    }

    pub fn execute<F, T>(&mut self, operation: F) -> Result<T, ReviewError>
    where
        F: FnOnce() -> Result<T, ReviewError>,
    {
        self.mutation_started = true;
        match operation() {
            Ok(value) => {
                self.finished = true;
                Ok(value)
            }
            Err(original) => {
                let rollback = self.rollback();
                self.finished = true;
                match rollback {
                    Ok(()) => Err(original),
                    Err(diagnostics) => Err(ReviewError::Rollback {
                        original: Box::new(original),
                        diagnostics,
                    }),
                }
            }
        }
    }

    fn rollback(&self) -> Result<(), String> {
        let mut diagnostics = Vec::new();
        let run = |description: &str, args: &[&str]| {
            run_git_command(description, args, false, self.verbose)
                .map(|_| ())
                .map_err(|error| format!("{description}: {error:?}"))
        };

        if let Err(error) = run(
            "detach HEAD for review rollback",
            &["checkout", "--detach", "--force", &self.head_oid],
        ) {
            diagnostics.push(error);
        }

        match read_heads(self.verbose) {
            Ok(current) => {
                for name in current
                    .keys()
                    .filter(|name| !self.heads.contains_key(*name))
                {
                    if let Err(error) =
                        run("remove generated local head", &["update-ref", "-d", name])
                    {
                        diagnostics.push(error);
                    }
                }
                for (name, oid) in &self.heads {
                    if current.get(name) != Some(oid) {
                        if let Err(error) = run("restore local head", &["update-ref", name, oid]) {
                            diagnostics.push(error);
                        }
                    }
                }
            }
            Err(error) => diagnostics.push(format!("read local heads for rollback: {error:?}")),
        }

        let checkout_result = match &self.head_ref {
            Some(reference) => run(
                "restore original branch",
                &["checkout", "--force", &reference["refs/heads/".len()..]],
            ),
            None => run(
                "restore detached HEAD",
                &["checkout", "--detach", "--force", &self.head_oid],
            ),
        };
        if let Err(error) = checkout_result {
            diagnostics.push(error);
        }

        if let Err(error) =
            clear_worktree(&self.root).and_then(|()| restore_worktree(&self.root, &self.worktree))
        {
            diagnostics.push(format!("restore worktree: {error}"));
        }
        if let Err(error) = fs::write(&self.config_path, &self.config) {
            diagnostics.push(format!("restore local config: {error}"));
        }
        if self.index_existed {
            match fs::read(self.scratch.path().join("index")) {
                Ok(index) => {
                    if let Err(error) = fs::write(&self.index_path, index) {
                        diagnostics.push(format!("restore real index: {error}"));
                    }
                }
                Err(error) => {
                    diagnostics.push(format!("restore real index: {error}"));
                }
            }
        } else {
            match fs::remove_file(&self.index_path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => diagnostics.push(format!("remove generated real index: {error}")),
            }
        }

        if let Err(error) = self.verify() {
            diagnostics.push(error);
        }
        if diagnostics.is_empty() {
            Ok(())
        } else {
            Err(diagnostics.join("\n"))
        }
    }

    fn verify(&self) -> Result<(), String> {
        let current_oid = git_stdout("verify restored HEAD", &["rev-parse", "HEAD"], self.verbose)
            .map_err(|error| format!("verify HEAD: {error:?}"))?;
        if current_oid != self.head_oid {
            return Err(format!(
                "HEAD mismatch: expected {}, found {current_oid}",
                self.head_oid
            ));
        }
        let symbolic = run_git_command(
            "verify restored branch",
            &["symbolic-ref", "--quiet", "HEAD"],
            true,
            self.verbose,
        )
        .map_err(|error| format!("verify branch: {error:?}"))?;
        let current_ref = symbolic
            .status
            .success()
            .then(|| String::from_utf8_lossy(&symbolic.stdout).trim().to_string());
        if current_ref != self.head_ref {
            return Err(format!(
                "HEAD attachment mismatch: expected {:?}, found {current_ref:?}",
                self.head_ref
            ));
        }
        let heads = read_heads(self.verbose).map_err(|error| format!("verify refs: {error:?}"))?;
        if heads != self.heads {
            return Err("local heads differ after rollback".to_string());
        }
        if fs::read(&self.config_path).map_err(|error| error.to_string())? != self.config {
            return Err("local config differs after rollback".to_string());
        }
        let index = match fs::read(&self.index_path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
        };
        let expected_index = if self.index_existed {
            Some(fs::read(self.scratch.path().join("index")).map_err(|error| error.to_string())?)
        } else {
            None
        };
        if index != expected_index {
            return Err("real index differs after rollback".to_string());
        }
        let worktree = scan_worktree(&self.root).map_err(|error| error.to_string())?;
        if worktree != self.worktree {
            return Err("worktree differs after rollback".to_string());
        }
        Ok(())
    }
}

impl Drop for ReviewTransaction {
    fn drop(&mut self) {
        if self.mutation_started && !self.finished {
            let _ = self.rollback();
        }
    }
}

fn git_stdout(description: &str, args: &[&str], verbose: bool) -> Result<String, GitCommandError> {
    let output = run_git_command(description, args, false, verbose)?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn resolve_git_path(root: &Path, name: &str, verbose: bool) -> Result<PathBuf, ReviewError> {
    let path = git_stdout(
        &format!("locate Git {name}"),
        &["rev-parse", "--git-path", name],
        verbose,
    )?;
    let path = PathBuf::from(path);
    Ok(if path.is_absolute() {
        path
    } else {
        root.join(path)
    })
}

fn read_heads(verbose: bool) -> Result<BTreeMap<String, String>, GitCommandError> {
    let output = run_git_command(
        "list local heads",
        &[
            "for-each-ref",
            "--sort=refname",
            "--format=%(refname) %(objectname)",
            "refs/heads/",
        ],
        false,
        verbose,
    )?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(name, oid)| (name.to_string(), oid.to_string()))
        .collect())
}

fn scan_worktree(root: &Path) -> io::Result<BTreeMap<PathBuf, Entry>> {
    fn visit(
        root: &Path,
        directory: &Path,
        entries: &mut BTreeMap<PathBuf, Entry>,
    ) -> io::Result<()> {
        for child in fs::read_dir(directory)? {
            let child = child?;
            if directory == root && child.file_name() == ".git" {
                continue;
            }
            let path = child.path();
            let relative = path
                .strip_prefix(root)
                .expect("worktree child must be beneath root")
                .to_path_buf();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                entries.insert(
                    relative,
                    Entry::Symlink {
                        target: fs::read_link(path)?,
                    },
                );
            } else if metadata.is_dir() {
                entries.insert(
                    relative.clone(),
                    Entry::Directory {
                        mode: metadata.mode(),
                    },
                );
                visit(root, &path, entries)?;
            } else if metadata.is_file() {
                entries.insert(
                    relative,
                    Entry::File {
                        mode: metadata.mode(),
                        data: fs::read(path)?,
                    },
                );
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported filesystem entry `{}`", relative.display()),
                ));
            }
        }
        Ok(())
    }

    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries)?;
    Ok(entries)
}

fn clear_worktree(root: &Path) -> io::Result<()> {
    for child in fs::read_dir(root)? {
        let child = child?;
        if child.file_name() == ".git" {
            continue;
        }
        let path = child.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn restore_worktree(root: &Path, entries: &BTreeMap<PathBuf, Entry>) -> io::Result<()> {
    for (relative, entry) in entries {
        if matches!(entry, Entry::Directory { .. }) {
            fs::create_dir_all(root.join(relative))?;
        }
    }
    for (relative, entry) in entries {
        let path = root.join(relative);
        match entry {
            Entry::Directory { .. } => {}
            Entry::File { mode, data } => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&path, data)?;
                fs::set_permissions(path, fs::Permissions::from_mode(*mode))?;
            }
            Entry::Symlink { target } => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                symlink(target, path)?;
            }
        }
    }
    for (relative, entry) in entries.iter().rev() {
        if let Entry::Directory { mode } = entry {
            fs::set_permissions(root.join(relative), fs::Permissions::from_mode(*mode))?;
        }
    }
    Ok(())
}
