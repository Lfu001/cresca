use crate::git::{run_git_command, run_git_command_with_env, GitCommandError};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

#[derive(Debug, PartialEq, Eq)]
pub struct ReviewPreparation {
    pub has_unreviewed_changes: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ReconstructionOutcome {
    pub tree_oid: String,
    pub downgraded_paths: Vec<String>,
}

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

pub fn unique_merge_base(left: &str, right: &str, verbose: bool) -> Result<String, ReviewError> {
    let output = run_git_command(
        "get unique merge base",
        &["merge-base", "--all", left, right],
        &[1],
        verbose,
    )?;
    let bases: Vec<_> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    match bases.as_slice() {
        [base] => Ok(base.clone()),
        [] => Err(ReviewError::Message(format!(
            "No unique safe merge base exists between `{left}` and `{right}`."
        ))),
        _ => Err(ReviewError::Message(format!(
            "Multiple merge bases exist between `{left}` and `{right}`; refusing to guess approval history."
        ))),
    }
}

fn parse_merge_tree_output(stdout: &[u8]) -> (Option<String>, Vec<String>) {
    let mut fields = stdout.split(|byte| *byte == 0);
    let tree = fields
        .next()
        .and_then(|field| std::str::from_utf8(field).ok())
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .map(str::to_owned);
    let mut paths = Vec::new();
    for field in fields {
        if field.is_empty() {
            break;
        }
        paths.push(String::from_utf8_lossy(field).into_owned());
    }
    (tree, paths)
}

fn reconstruction_git(
    description: &str,
    args: &[&str],
    allowed_exit_codes: &[i32],
    verbose: bool,
    env: Option<&[(&str, &OsStr)]>,
) -> Result<std::process::Output, GitCommandError> {
    match env {
        Some(env) => run_git_command_with_env(description, args, env, allowed_exit_codes, verbose),
        None => run_git_command(description, args, allowed_exit_codes, verbose),
    }
}

fn tree_entry(
    revision: &str,
    path: &str,
    verbose: bool,
    env: Option<&[(&str, &OsStr)]>,
) -> Result<Option<(String, String)>, ReviewError> {
    let output = reconstruction_git(
        "inspect reconstruction tree entry",
        &["ls-tree", "-z", revision, "--", path],
        &[],
        verbose,
        env,
    )?;
    if output.stdout.is_empty() {
        return Ok(None);
    }
    let record = output
        .stdout
        .split(|byte| *byte == 0)
        .next()
        .unwrap_or_default();
    let header = record
        .split(|byte| *byte == b'\t')
        .next()
        .ok_or_else(|| ReviewError::Message("Git returned an invalid tree entry".to_string()))?;
    let header = String::from_utf8_lossy(header);
    let mut fields = header.split_whitespace();
    let mode = fields.next().unwrap_or_default().to_string();
    let kind = fields.next().unwrap_or_default();
    let oid = fields.next().unwrap_or_default().to_string();
    if kind != "blob" || mode.is_empty() || oid.is_empty() {
        return Ok(None);
    }
    Ok(Some((mode, oid)))
}

fn is_binary_blob(
    oid: &str,
    verbose: bool,
    env: Option<&[(&str, &OsStr)]>,
) -> Result<bool, ReviewError> {
    let output = reconstruction_git(
        "inspect conflicting blob",
        &["cat-file", "blob", oid],
        &[],
        verbose,
        env,
    )?;
    Ok(output.stdout.contains(&0))
}

fn text_conflict_can_keep_hunks(
    old_base: &str,
    new_base: &str,
    old_review: &str,
    path: &str,
    verbose: bool,
    env: Option<&[(&str, &OsStr)]>,
) -> Result<bool, ReviewError> {
    let Some(old) = tree_entry(old_base, path, verbose, env)? else {
        return Ok(false);
    };
    let Some(new) = tree_entry(new_base, path, verbose, env)? else {
        return Ok(false);
    };
    let Some(review) = tree_entry(old_review, path, verbose, env)? else {
        return Ok(false);
    };
    let regular = |mode: &str| mode == "100644" || mode == "100755";
    if old.0 != new.0 || old.0 != review.0 || !regular(&old.0) {
        return Ok(false);
    }
    Ok(!is_binary_blob(&old.1, verbose, env)?
        && !is_binary_blob(&new.1, verbose, env)?
        && !is_binary_blob(&review.1, verbose, env)?)
}

pub fn reconstruct_approval_tree(
    old_base: &str,
    new_base: &str,
    old_review: &str,
    verbose: bool,
) -> Result<ReconstructionOutcome, ReviewError> {
    reconstruct_approval_tree_with_env(old_base, new_base, old_review, verbose, None)
}

pub fn reconstruct_approval_tree_with_env(
    old_base: &str,
    new_base: &str,
    old_review: &str,
    verbose: bool,
    env: Option<&[(&str, &OsStr)]>,
) -> Result<ReconstructionOutcome, ReviewError> {
    let diagnostic = reconstruction_git(
        "identify approval reconstruction conflicts",
        &[
            "merge-tree",
            "--write-tree",
            &format!("--merge-base={old_base}"),
            "-Xfind-renames=100%",
            "--messages",
            "--name-only",
            "-z",
            new_base,
            old_review,
        ],
        &[1],
        verbose,
        env,
    )?;
    let (_, conflict_paths) = parse_merge_tree_output(&diagnostic.stdout);
    if diagnostic.status.code() == Some(1) && conflict_paths.is_empty() {
        return Err(ReviewError::Message(
            "Approval reconstruction conflicted but Git did not identify any paths; refusing the reconstructed tree."
                .to_string(),
        ));
    }

    let merged = reconstruction_git(
        "reconstruct approved tree",
        &[
            "merge-tree",
            "--write-tree",
            &format!("--merge-base={old_base}"),
            "-Xours",
            "-Xfind-renames=100%",
            "--no-messages",
            new_base,
            old_review,
        ],
        &[1],
        verbose,
        env,
    )?;
    let tree_oid = String::from_utf8_lossy(&merged.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    if tree_oid.is_empty() {
        return Err(ReviewError::Message(
            "Git did not return a reconstructed approval tree.".to_string(),
        ));
    }

    let mut downgraded = Vec::new();
    for path in &conflict_paths {
        if merged.status.code() == Some(1)
            || !text_conflict_can_keep_hunks(old_base, new_base, old_review, path, verbose, env)?
        {
            downgraded.push(path.clone());
        }
    }
    downgraded.sort();
    downgraded.dedup();
    if downgraded.is_empty() {
        return Ok(ReconstructionOutcome {
            tree_oid,
            downgraded_paths: downgraded,
        });
    }

    let scratch = TempDir::new().map_err(|error| {
        ReviewError::Message(format!("failed to create scratch index: {error}"))
    })?;
    let index = scratch.path().join("index");
    let mut scratch_env: Vec<(&str, &OsStr)> = env.unwrap_or_default().to_vec();
    scratch_env.retain(|(key, _)| *key != "GIT_INDEX_FILE");
    scratch_env.push(("GIT_INDEX_FILE", index.as_os_str()));
    run_git_command_with_env(
        "load reconstructed tree into scratch index",
        &["read-tree", &tree_oid],
        &scratch_env,
        &[],
        verbose,
    )?;
    for path in &downgraded {
        run_git_command_with_env(
            "downgrade conflicted path to new base",
            &["reset", new_base, "--", path],
            &scratch_env,
            &[],
            verbose,
        )?;
    }
    let output = run_git_command_with_env(
        "write reconstructed scratch index",
        &["write-tree"],
        &scratch_env,
        &[],
        verbose,
    )?;
    Ok(ReconstructionOutcome {
        tree_oid: String::from_utf8_lossy(&output.stdout).trim().to_string(),
        downgraded_paths: downgraded,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Entry {
    Directory { mode: u32 },
    File { mode: u32 },
    Symlink { target: PathBuf },
    Other { mode: u32 },
}

#[derive(Debug)]
struct AdminFile {
    path: PathBuf,
    backup_name: String,
    existed: bool,
}

pub struct ReviewTransaction {
    root: PathBuf,
    head_ref: Option<String>,
    head_oid: String,
    heads: BTreeMap<String, String>,
    remote_refs: BTreeMap<String, String>,
    config_path: PathBuf,
    config: Vec<u8>,
    index_path: PathBuf,
    index_existed: bool,
    admin_files: Vec<AdminFile>,
    worktree: BTreeMap<PathBuf, Entry>,
    scratch: TempDir,
    mutation_started: bool,
    finished: bool,
    verbose: bool,
}

impl ReviewTransaction {
    pub fn repository_root(verbose: bool) -> Result<PathBuf, ReviewError> {
        let output = run_git_command(
            "locate repository worktree",
            &["rev-parse", "--show-toplevel"],
            &[],
            verbose,
        )?;
        Ok(PathBuf::from(
            String::from_utf8_lossy(&output.stdout).trim(),
        ))
    }

    /// Capture all rollback state. Call this immediately before the first operation that can
    /// change the review checkout, refs, index, config, or worktree.
    pub fn begin(root: PathBuf, verbose: bool) -> Result<Self, ReviewError> {
        let head_oid = git_stdout("capture original HEAD", &["rev-parse", "HEAD"], verbose)?;
        let symbolic = run_git_command(
            "capture original branch",
            &["symbolic-ref", "--quiet", "HEAD"],
            &[1],
            verbose,
        )?;
        let head_ref = symbolic
            .status
            .success()
            .then(|| String::from_utf8_lossy(&symbolic.stdout).trim().to_string());
        // Local branch refs are transactional state; reflogs are intentionally outside the
        // rollback contract because Git appends to them while restoring those refs.
        let heads = read_heads(verbose)?;
        let remote_refs = read_refs("refs/remotes/", verbose)?;
        let config_path = resolve_git_path(&root, "config", verbose)?;
        let index_path = resolve_git_path(&root, "index", verbose)?;
        let scratch_parent = config_path.parent().ok_or_else(|| {
            ReviewError::Message("local Git config has no parent directory".to_string())
        })?;
        let scratch = tempfile::Builder::new()
            .prefix("cresca-review-")
            .tempdir_in(scratch_parent)
            .map_err(|error| {
                ReviewError::Message(format!(
                    "failed to create Git-private scratch area: {error}"
                ))
            })?;

        let config = fs::read(&config_path).map_err(|error| {
            ReviewError::Message(format!("failed to snapshot local config: {error}"))
        })?;
        let index_existed =
            copy_if_present(&index_path, &scratch.path().join("index")).map_err(|error| {
                ReviewError::Message(format!("failed to back up real index: {error}"))
            })?;

        let admin_names = [
            "FETCH_HEAD",
            "ORIG_HEAD",
            "SQUASH_MSG",
            "MERGE_HEAD",
            "MERGE_MSG",
            "MERGE_MODE",
            "MERGE_RR",
            "AUTO_MERGE",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "COMMIT_EDITMSG",
        ];
        let mut admin_files = Vec::new();
        for (number, name) in admin_names.iter().enumerate() {
            let path = resolve_git_path(&root, name, verbose)?;
            let backup_name = format!("admin-{number}");
            let existed =
                copy_if_present(&path, &scratch.path().join(&backup_name)).map_err(|error| {
                    ReviewError::Message(format!("failed to back up {name}: {error}"))
                })?;
            admin_files.push(AdminFile {
                path,
                backup_name,
                existed,
            });
        }

        let worktree = scan_worktree(&root).map_err(|error| {
            ReviewError::Message(format!("failed to snapshot worktree: {error}"))
        })?;
        let backup_root = scratch.path().join("worktree");
        for (relative, entry) in &worktree {
            if matches!(entry, Entry::File { .. }) {
                let destination = backup_root.join(relative);
                if let Some(parent) = destination.parent() {
                    fs::create_dir_all(parent).map_err(|error| {
                        ReviewError::Message(format!(
                            "failed to create worktree backup directory: {error}"
                        ))
                    })?;
                }
                fs::copy(root.join(relative), destination).map_err(|error| {
                    ReviewError::Message(format!(
                        "failed to stream worktree backup for `{}`: {error}",
                        relative.display()
                    ))
                })?;
            }
        }

        Ok(Self {
            root,
            head_ref,
            head_oid,
            heads,
            remote_refs,
            config_path,
            config,
            index_path,
            index_existed,
            admin_files,
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
            run_git_command(description, args, &[], self.verbose)
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
        restore_refs(
            "remote-tracking",
            "refs/remotes/",
            &self.remote_refs,
            self.verbose,
            &mut diagnostics,
        );

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

        reconcile_worktree(
            &self.root,
            &self.worktree,
            &self.scratch.path().join("worktree"),
            &mut diagnostics,
        );
        if let Err(error) = fs::write(&self.config_path, &self.config) {
            diagnostics.push(format!("restore local config: {error}"));
        }
        restore_optional_file(
            &self.index_path,
            &self.scratch.path().join("index"),
            self.index_existed,
            "real index",
            &mut diagnostics,
        );
        for admin in &self.admin_files {
            restore_optional_file(
                &admin.path,
                &self.scratch.path().join(&admin.backup_name),
                admin.existed,
                &format!("Git admin file `{}`", admin.path.display()),
                &mut diagnostics,
            );
        }

        self.verify(&mut diagnostics);
        if diagnostics.is_empty() {
            Ok(())
        } else {
            Err(diagnostics.join("\n"))
        }
    }

    fn verify(&self, diagnostics: &mut Vec<String>) {
        match git_stdout("verify restored HEAD", &["rev-parse", "HEAD"], self.verbose) {
            Ok(current_oid) if current_oid != self.head_oid => diagnostics.push(format!(
                "HEAD mismatch: expected {}, found {current_oid}",
                self.head_oid
            )),
            Err(error) => diagnostics.push(format!("verify HEAD: {error:?}")),
            _ => {}
        }
        match run_git_command(
            "verify restored branch",
            &["symbolic-ref", "--quiet", "HEAD"],
            &[1],
            self.verbose,
        ) {
            Ok(symbolic) => {
                let current_ref = symbolic
                    .status
                    .success()
                    .then(|| String::from_utf8_lossy(&symbolic.stdout).trim().to_string());
                if current_ref != self.head_ref {
                    diagnostics.push(format!(
                        "HEAD attachment mismatch: expected {:?}, found {current_ref:?}",
                        self.head_ref
                    ));
                }
            }
            Err(error) => diagnostics.push(format!("verify branch: {error:?}")),
        }
        match read_heads(self.verbose) {
            Ok(heads) if heads != self.heads => {
                diagnostics.push("local heads differ after rollback".to_string())
            }
            Err(error) => diagnostics.push(format!("verify refs: {error:?}")),
            _ => {}
        }
        match read_refs("refs/remotes/", self.verbose) {
            Ok(refs) if refs != self.remote_refs => {
                diagnostics.push("remote-tracking refs differ after rollback".to_string())
            }
            Err(error) => diagnostics.push(format!("verify remote-tracking refs: {error:?}")),
            _ => {}
        }
        match fs::read(&self.config_path) {
            Ok(config) if config != self.config => {
                diagnostics.push("local config differs after rollback".to_string())
            }
            Err(error) => diagnostics.push(format!("verify local config: {error}")),
            _ => {}
        }
        verify_optional_file(
            &self.index_path,
            &self.scratch.path().join("index"),
            self.index_existed,
            "real index",
            diagnostics,
        );
        for admin in &self.admin_files {
            verify_optional_file(
                &admin.path,
                &self.scratch.path().join(&admin.backup_name),
                admin.existed,
                &format!("Git admin file `{}`", admin.path.display()),
                diagnostics,
            );
        }
        match scan_worktree(&self.root) {
            Ok(worktree) => {
                if worktree != self.worktree {
                    diagnostics.push("worktree entries differ after rollback".to_string());
                }
                for (relative, entry) in &self.worktree {
                    if matches!(entry, Entry::File { .. }) {
                        match files_equal(
                            &self.root.join(relative),
                            &self.scratch.path().join("worktree").join(relative),
                        ) {
                            Ok(true) => {}
                            Ok(false) => diagnostics.push(format!(
                                "worktree file content differs after rollback: `{}`",
                                relative.display()
                            )),
                            Err(error) => diagnostics.push(format!(
                                "verify worktree file `{}`: {error}",
                                relative.display()
                            )),
                        }
                    }
                }
            }
            Err(error) => diagnostics.push(format!("verify worktree: {error}")),
        }
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
    let output = run_git_command(description, args, &[], verbose)?;
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
    read_refs("refs/heads/", verbose)
}

fn read_refs(prefix: &str, verbose: bool) -> Result<BTreeMap<String, String>, GitCommandError> {
    let output = run_git_command(
        "list repository refs",
        &[
            "for-each-ref",
            "--sort=refname",
            "--format=%(refname) %(objectname)",
            prefix,
        ],
        &[],
        verbose,
    )?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(name, oid)| (name.to_string(), oid.to_string()))
        .collect())
}

fn restore_refs(
    label: &str,
    prefix: &str,
    expected: &BTreeMap<String, String>,
    verbose: bool,
    diagnostics: &mut Vec<String>,
) {
    let current = match read_refs(prefix, verbose) {
        Ok(current) => current,
        Err(error) => {
            diagnostics.push(format!("read {label} refs for rollback: {error:?}"));
            return;
        }
    };
    for name in current.keys().filter(|name| !expected.contains_key(*name)) {
        if let Err(error) = run_git_command(
            &format!("remove generated {label} ref"),
            &["update-ref", "-d", name],
            &[],
            verbose,
        ) {
            diagnostics.push(format!("remove generated {label} ref `{name}`: {error:?}"));
        }
    }
    for (name, oid) in expected {
        if current.get(name) != Some(oid) {
            if let Err(error) = run_git_command(
                &format!("restore {label} ref"),
                &["update-ref", name, oid],
                &[],
                verbose,
            ) {
                diagnostics.push(format!("restore {label} ref `{name}`: {error:?}"));
            }
        }
    }
}

fn copy_if_present(source: &Path, destination: &Path) -> io::Result<bool> {
    match fs::copy(source, destination) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
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
            let entry = if metadata.file_type().is_symlink() {
                Entry::Symlink {
                    target: fs::read_link(&path)?,
                }
            } else if metadata.is_dir() {
                Entry::Directory {
                    mode: metadata.mode(),
                }
            } else if metadata.is_file() {
                Entry::File {
                    mode: metadata.mode(),
                }
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported filesystem entry `{}`", relative.display()),
                ));
            };
            let is_directory = matches!(entry, Entry::Directory { .. });
            entries.insert(relative, entry);
            if is_directory {
                visit(root, &path, entries)?;
            }
        }
        Ok(())
    }

    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries)?;
    Ok(entries)
}

fn scan_worktree_tolerant(
    root: &Path,
    expected: &BTreeMap<PathBuf, Entry>,
    diagnostics: &mut Vec<String>,
) -> BTreeMap<PathBuf, Entry> {
    fn visit(
        root: &Path,
        directory: &Path,
        expected: &BTreeMap<PathBuf, Entry>,
        entries: &mut BTreeMap<PathBuf, Entry>,
        diagnostics: &mut Vec<String>,
    ) {
        let children = match fs::read_dir(directory) {
            Ok(children) => children,
            Err(error) => {
                diagnostics.push(format!(
                    "scan `{}` during rollback: {error}",
                    directory.display()
                ));
                return;
            }
        };
        for child in children {
            let child = match child {
                Ok(child) => child,
                Err(error) => {
                    diagnostics.push(format!("read worktree entry during rollback: {error}"));
                    continue;
                }
            };
            if directory == root && child.file_name() == ".git" {
                continue;
            }
            let path = child.path();
            let relative = match path.strip_prefix(root) {
                Ok(relative) => relative.to_path_buf(),
                Err(error) => {
                    diagnostics.push(format!(
                        "resolve rollback path `{}`: {error}",
                        path.display()
                    ));
                    continue;
                }
            };
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    diagnostics.push(format!(
                        "inspect `{}` during rollback: {error}",
                        relative.display()
                    ));
                    continue;
                }
            };
            let entry = if metadata.file_type().is_symlink() {
                match fs::read_link(&path) {
                    Ok(target) => Entry::Symlink { target },
                    Err(error) => {
                        diagnostics.push(format!(
                            "read symlink `{}` during rollback: {error}",
                            relative.display()
                        ));
                        continue;
                    }
                }
            } else if metadata.is_dir() {
                Entry::Directory {
                    mode: metadata.mode(),
                }
            } else if metadata.is_file() {
                Entry::File {
                    mode: metadata.mode(),
                }
            } else {
                Entry::Other {
                    mode: metadata.mode(),
                }
            };
            let is_directory = matches!(entry, Entry::Directory { .. });
            let visit_directory =
                is_directory && matches!(expected.get(&relative), Some(Entry::Directory { .. }));
            entries.insert(relative, entry);
            if visit_directory {
                visit(root, &path, expected, entries, diagnostics);
            }
        }
    }

    let mut entries = BTreeMap::new();
    visit(root, root, expected, &mut entries, diagnostics);
    entries
}

fn remove_path(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            let widened = metadata.permissions().mode() | 0o700;
            if widened != metadata.permissions().mode() {
                fs::set_permissions(path, fs::Permissions::from_mode(widened))?;
            }
            for child in fs::read_dir(path)? {
                remove_path(&child?.path())?;
            }
            fs::remove_dir(path)
        }
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn reconcile_worktree(
    root: &Path,
    expected: &BTreeMap<PathBuf, Entry>,
    backup_root: &Path,
    diagnostics: &mut Vec<String>,
) {
    let current = scan_worktree_tolerant(root, expected, diagnostics);
    let mut extras: Vec<_> = current
        .keys()
        .filter(|relative| !expected.contains_key(*relative))
        .cloned()
        .collect();
    let mismatches: Vec<_> = expected
        .iter()
        .filter_map(|(relative, entry)| {
            (!entry_matches(root, relative, entry, backup_root).unwrap_or(false))
                .then_some(relative.clone())
        })
        .collect();

    let mut directories_to_widen = BTreeSet::new();
    for relative in extras.iter().chain(mismatches.iter()) {
        let mut ancestor = Some(relative.as_path());
        while let Some(path) = ancestor {
            if matches!(expected.get(path), Some(Entry::Directory { .. })) {
                directories_to_widen.insert(path.to_path_buf());
            }
            ancestor = path.parent();
        }
    }
    let mut deferred_directory_modes = BTreeMap::new();
    for relative in directories_to_widen {
        let Some(Entry::Directory { mode }) = expected.get(&relative) else {
            continue;
        };
        let widened = *mode | 0o700;
        let current_mode = fs::symlink_metadata(root.join(&relative))
            .ok()
            .filter(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
            .map(|metadata| metadata.mode());
        let ready_for_descendants = match current_mode {
            Some(current) if current == widened => true,
            Some(_) => {
                if let Err(error) =
                    fs::set_permissions(root.join(&relative), fs::Permissions::from_mode(widened))
                {
                    diagnostics.push(format!(
                        "temporarily widen directory `{}` for rollback: {error}",
                        relative.display()
                    ));
                    false
                } else {
                    true
                }
            }
            None => false,
        };
        if ready_for_descendants && widened != *mode {
            deferred_directory_modes.insert(relative, *mode);
        }
    }

    let current = scan_worktree_tolerant(root, expected, diagnostics);
    extras = current
        .keys()
        .filter(|relative| !expected.contains_key(*relative))
        .cloned()
        .collect();
    extras.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for relative in extras {
        if let Err(error) = remove_path(&root.join(&relative)) {
            diagnostics.push(format!(
                "remove generated path `{}`: {error}",
                relative.display()
            ));
        }
    }

    for (relative, entry) in expected {
        if entry_matches(root, relative, entry, backup_root).unwrap_or(false) {
            continue;
        }
        if let Entry::Directory { mode } = entry {
            let path = root.join(relative);
            if matches!(fs::symlink_metadata(&path), Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink())
            {
                if !deferred_directory_modes.contains_key(relative) {
                    if let Err(error) =
                        fs::set_permissions(&path, fs::Permissions::from_mode(*mode))
                    {
                        diagnostics.push(format!(
                            "restore directory mode `{}`: {error}",
                            relative.display()
                        ));
                    }
                }
            } else {
                if let Err(error) = remove_path(&path) {
                    diagnostics.push(format!(
                        "replace path `{}` with directory: {error}",
                        relative.display()
                    ));
                    continue;
                }
                if let Err(error) = fs::create_dir_all(&path) {
                    diagnostics.push(format!(
                        "restore directory `{}`: {error}",
                        relative.display()
                    ));
                } else {
                    deferred_directory_modes.insert(relative.clone(), *mode);
                }
            }
        }
    }
    for (relative, entry) in expected {
        let path = root.join(relative);
        match entry {
            Entry::Directory { .. } | Entry::Other { .. } => {}
            Entry::File { mode } => {
                let mut widened_existing_file = false;
                let existing_regular = matches!(
                    fs::symlink_metadata(&path),
                    Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink()
                );
                if existing_regular {
                    if matches!(files_equal(&path, &backup_root.join(relative)), Ok(true)) {
                        if let Err(error) =
                            fs::set_permissions(&path, fs::Permissions::from_mode(*mode))
                        {
                            diagnostics.push(format!(
                                "restore mode for `{}`: {error}",
                                relative.display()
                            ));
                        }
                        continue;
                    }
                    if let Err(error) =
                        fs::set_permissions(&path, fs::Permissions::from_mode(*mode | 0o600))
                    {
                        diagnostics.push(format!(
                            "temporarily make file `{}` writable: {error}",
                            relative.display()
                        ));
                        continue;
                    }
                    widened_existing_file = true;
                }
                if matches!(fs::symlink_metadata(&path), Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink())
                {
                    if let Err(error) = remove_path(&path) {
                        diagnostics.push(format!(
                            "replace path `{}` with file: {error}",
                            relative.display()
                        ));
                        continue;
                    }
                }
                if let Some(parent) = path.parent() {
                    if let Err(error) = fs::create_dir_all(parent) {
                        diagnostics.push(format!(
                            "create parent for `{}`: {error}",
                            relative.display()
                        ));
                        if widened_existing_file {
                            if let Err(error) =
                                fs::set_permissions(&path, fs::Permissions::from_mode(*mode))
                            {
                                diagnostics.push(format!(
                                    "restore mode for `{}`: {error}",
                                    relative.display()
                                ));
                            }
                        }
                        continue;
                    }
                }
                if let Err(error) = fs::copy(backup_root.join(relative), &path) {
                    diagnostics.push(format!("restore file `{}`: {error}", relative.display()));
                }
                if let Err(error) = fs::set_permissions(&path, fs::Permissions::from_mode(*mode)) {
                    diagnostics.push(format!(
                        "restore mode for `{}`: {error}",
                        relative.display()
                    ));
                }
            }
            Entry::Symlink { target } => {
                if matches!(
                    fs::symlink_metadata(&path),
                    Ok(metadata) if metadata.file_type().is_symlink()
                ) && fs::read_link(&path).ok().as_ref() == Some(target)
                {
                    continue;
                }
                if let Err(error) = remove_path(&path) {
                    diagnostics.push(format!(
                        "replace path `{}` with symlink: {error}",
                        relative.display()
                    ));
                    continue;
                }
                if let Some(parent) = path.parent() {
                    if let Err(error) = fs::create_dir_all(parent) {
                        diagnostics.push(format!(
                            "create parent for `{}`: {error}",
                            relative.display()
                        ));
                        continue;
                    }
                }
                if let Err(error) = symlink(target, &path) {
                    diagnostics.push(format!("restore symlink `{}`: {error}", relative.display()));
                }
            }
        }
    }
    for (relative, mode) in deferred_directory_modes.iter().rev() {
        if let Err(error) =
            fs::set_permissions(root.join(relative), fs::Permissions::from_mode(*mode))
        {
            diagnostics.push(format!(
                "restore directory mode `{}`: {error}",
                relative.display()
            ));
        }
    }
}

fn entry_matches(
    root: &Path,
    relative: &Path,
    expected: &Entry,
    backup_root: &Path,
) -> io::Result<bool> {
    let path = root.join(relative);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    match expected {
        Entry::Directory { mode } => {
            Ok(metadata.is_dir() && !metadata.file_type().is_symlink() && metadata.mode() == *mode)
        }
        Entry::File { mode } => Ok(metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.mode() == *mode
            && files_equal(&path, &backup_root.join(relative))?),
        Entry::Symlink { target } => {
            Ok(metadata.file_type().is_symlink() && fs::read_link(path)? == *target)
        }
        Entry::Other { mode } => Ok(metadata.mode() == *mode),
    }
}

fn restore_optional_file(
    path: &Path,
    backup: &Path,
    existed: bool,
    label: &str,
    diagnostics: &mut Vec<String>,
) {
    let result = if existed {
        fs::copy(backup, path).map(|_| ())
    } else {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    };
    if let Err(error) = result {
        diagnostics.push(format!("restore {label}: {error}"));
    }
}

fn verify_optional_file(
    path: &Path,
    backup: &Path,
    existed: bool,
    label: &str,
    diagnostics: &mut Vec<String>,
) {
    match (existed, fs::symlink_metadata(path)) {
        (false, Err(error)) if error.kind() == io::ErrorKind::NotFound => {}
        (false, Ok(_)) => diagnostics.push(format!("generated {label} remains after rollback")),
        (false, Err(error)) => diagnostics.push(format!("verify {label}: {error}")),
        (true, Err(error)) => diagnostics.push(format!("verify {label}: {error}")),
        (true, Ok(_)) => match files_equal(path, backup) {
            Ok(true) => {}
            Ok(false) => diagnostics.push(format!("{label} differs after rollback")),
            Err(error) => diagnostics.push(format!("verify {label}: {error}")),
        },
    }
}

fn files_equal(left: &Path, right: &Path) -> io::Result<bool> {
    let mut left = fs::File::open(left)?;
    let mut right = fs::File::open(right)?;
    let mut left_buffer = [0_u8; 64 * 1024];
    let mut right_buffer = [0_u8; 64 * 1024];
    loop {
        let left_count = left.read(&mut left_buffer)?;
        let right_count = right.read(&mut right_buffer)?;
        if left_count != right_count || left_buffer[..left_count] != right_buffer[..right_count] {
            return Ok(false);
        }
        if left_count == 0 {
            return Ok(true);
        }
    }
}
