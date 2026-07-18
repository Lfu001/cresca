use colored::Colorize;
use std::process::{Command, ExitStatus, Output};

#[derive(Debug, PartialEq, Eq)]
pub struct GitCommandError {
    pub description: String,
    pub args: Vec<String>,
    pub status: Option<ExitStatus>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub const REVIEW_METADATA_VERSION: &str = "1";
pub const REVIEW_SCOPE_VERSION: &str = "1";

#[derive(Debug, PartialEq, Eq)]
pub struct ReviewMetadata {
    pub target: String,
    pub source: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ReviewScope {
    pub end_oid: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReviewMetadataError {
    InvalidBranchName,
    Missing,
    UnsupportedVersion(String),
    Invalid,
    Git(GitCommandError),
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReviewScopeError {
    Missing,
    Duplicate,
    UnsupportedVersion(String),
    Invalid,
    UnavailableCommit(String),
    Git(GitCommandError),
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReviewBranchSelection {
    New(String),
    Existing(String),
}

#[derive(Debug)]
pub enum ReviewBranchSelectionError {
    Git(GitCommandError),
    Conflict(String),
}

fn readable_review_branch(metadata: &ReviewMetadata) -> String {
    format!("review-{}-{}", metadata.target, metadata.source).replace('/', "_")
}

fn review_identity_hash(metadata: &ReviewMetadata) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = OFFSET;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
    };
    feed(&(metadata.target.len() as u64).to_be_bytes());
    feed(metadata.target.as_bytes());
    feed(&(metadata.source.len() as u64).to_be_bytes());
    feed(metadata.source.as_bytes());
    hash
}

fn suffixed_review_branch(base: &str, metadata: &ReviewMetadata) -> String {
    format!("{base}-{:016x}", review_identity_hash(metadata))
}

fn local_branch_exists(branch: &str, verbose: bool) -> Result<bool, GitCommandError> {
    Ok(run_git_command(
        "check existence of review branch",
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
        true,
        verbose,
    )?
    .status
    .success())
}

pub fn select_review_branch(
    metadata: &ReviewMetadata,
    verbose: bool,
) -> Result<ReviewBranchSelection, ReviewBranchSelectionError> {
    let base = readable_review_branch(metadata);
    let base_exists =
        local_branch_exists(&base, verbose).map_err(ReviewBranchSelectionError::Git)?;
    match (base_exists, read_review_metadata(&base, verbose)) {
        (false, Err(ReviewMetadataError::Missing)) => return Ok(ReviewBranchSelection::New(base)),
        (true, Ok(existing)) if existing == *metadata => {
            return Ok(ReviewBranchSelection::Existing(base))
        }
        (_, Err(ReviewMetadataError::Git(error))) => {
            return Err(ReviewBranchSelectionError::Git(error))
        }
        _ => {}
    }

    let suffix = suffixed_review_branch(&base, metadata);
    let suffix_exists =
        local_branch_exists(&suffix, verbose).map_err(ReviewBranchSelectionError::Git)?;
    match (suffix_exists, read_review_metadata(&suffix, verbose)) {
        (false, Err(ReviewMetadataError::Missing)) => {
            return Ok(ReviewBranchSelection::New(suffix))
        }
        (true, Ok(existing)) if existing == *metadata => {
            return Ok(ReviewBranchSelection::Existing(suffix))
        }
        (_, Err(ReviewMetadataError::Git(error))) => {
            return Err(ReviewBranchSelectionError::Git(error))
        }
        _ => {}
    }

    Err(ReviewBranchSelectionError::Conflict(format!(
        "Found conflicting review branches `{}` and `{}` with missing, invalid, or different metadata. Inspect and delete or rename the conflicting local review branch before retrying.",
        base,
        suffix
    )))
}

fn review_config_key(branch: &str, field: &str) -> String {
    format!("branch.{branch}.cresca-{field}")
}

fn review_config_values(
    branch: &str,
    field: &str,
    verbose: bool,
) -> Result<Vec<String>, GitCommandError> {
    let key = review_config_key(branch, field);
    let output = run_git_command(
        "read review metadata",
        &["config", "--local", "--get-all", &key],
        true,
        verbose,
    )?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_owned)
            .collect());
    }

    if output.status.code() == Some(1) && output.stderr.is_empty() {
        return Ok(Vec::new());
    }

    Err(GitCommandError {
        description: "read review metadata".to_string(),
        args: vec![
            "config".to_string(),
            "--local".to_string(),
            "--get-all".to_string(),
            key,
        ],
        status: Some(output.status),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

pub fn read_review_metadata(
    branch: &str,
    verbose: bool,
) -> Result<ReviewMetadata, ReviewMetadataError> {
    let versions =
        review_config_values(branch, "version", verbose).map_err(ReviewMetadataError::Git)?;
    let targets =
        review_config_values(branch, "target", verbose).map_err(ReviewMetadataError::Git)?;
    let sources =
        review_config_values(branch, "source", verbose).map_err(ReviewMetadataError::Git)?;

    match (versions.as_slice(), targets.as_slice(), sources.as_slice()) {
        ([version], [target], [source])
            if version == REVIEW_METADATA_VERSION && !target.is_empty() && !source.is_empty() =>
        {
            Ok(ReviewMetadata {
                target: target.clone(),
                source: source.clone(),
            })
        }
        ([], [], []) => Err(ReviewMetadataError::Missing),
        ([version], _, _) if version != REVIEW_METADATA_VERSION => {
            Err(ReviewMetadataError::UnsupportedVersion(version.clone()))
        }
        _ => Err(ReviewMetadataError::Invalid),
    }
}

pub fn write_review_metadata(
    branch: &str,
    metadata: &ReviewMetadata,
    verbose: bool,
) -> Result<(), GitCommandError> {
    let version_key = review_config_key(branch, "version");
    let target_key = review_config_key(branch, "target");
    let source_key = review_config_key(branch, "source");

    let existing_versions = review_config_values(branch, "version", verbose)?;
    if !existing_versions.is_empty() {
        run_git_command(
            "clear review metadata version marker",
            &["config", "--local", "--unset-all", &version_key],
            false,
            verbose,
        )?;
    }
    run_git_command(
        "record review target",
        &[
            "config",
            "--local",
            "--replace-all",
            &target_key,
            &metadata.target,
        ],
        false,
        verbose,
    )?;
    run_git_command(
        "record review source",
        &[
            "config",
            "--local",
            "--replace-all",
            &source_key,
            &metadata.source,
        ],
        false,
        verbose,
    )?;
    run_git_command(
        "commit review metadata",
        &[
            "config",
            "--local",
            "--replace-all",
            &version_key,
            REVIEW_METADATA_VERSION,
        ],
        false,
        verbose,
    )?;
    Ok(())
}

pub fn write_review_scope(
    branch: &str,
    scope: &ReviewScope,
    verbose: bool,
) -> Result<(), GitCommandError> {
    let key = review_config_key(branch, "scope");
    let value = format!("{}:{}", REVIEW_SCOPE_VERSION, scope.end_oid);
    run_git_command(
        "record review range",
        &["config", "--local", "--replace-all", &key, &value],
        false,
        verbose,
    )?;
    Ok(())
}

pub fn read_review_scope(branch: &str, verbose: bool) -> Result<ReviewScope, ReviewScopeError> {
    let values = review_config_values(branch, "scope", verbose).map_err(ReviewScopeError::Git)?;
    let value = match values.as_slice() {
        [] => return Err(ReviewScopeError::Missing),
        [value] => value,
        _ => return Err(ReviewScopeError::Duplicate),
    };
    let Some((version, end_oid)) = value.split_once(':') else {
        return Err(ReviewScopeError::Invalid);
    };
    if version != REVIEW_SCOPE_VERSION {
        return Err(ReviewScopeError::UnsupportedVersion(version.to_string()));
    }
    if end_oid.is_empty()
        || end_oid.contains(':')
        || !end_oid.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ReviewScopeError::Invalid);
    }
    let revision = format!("{end_oid}^{{commit}}");
    let output = run_git_command(
        "validate review range endpoint",
        &["rev-parse", "--verify", &revision],
        true,
        verbose,
    )
    .map_err(ReviewScopeError::Git)?;
    if !output.status.success() {
        return Err(ReviewScopeError::UnavailableCommit(end_oid.to_string()));
    }
    let canonical = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if canonical != end_oid {
        return Err(ReviewScopeError::Invalid);
    }
    Ok(ReviewScope {
        end_oid: end_oid.to_string(),
    })
}

/// Run a git command and return the output
///
/// # Arguments
///
/// * `description` - The description of the git command.
/// * `args` - The arguments to pass to the git command.
/// * `maybe_error` - Whether the git command might fail intentionally.
/// * `verbose` - Whether to print the git command and its output.
///
/// # Returns
///
/// * `std::process::Output` - The output of the git command.
pub fn run_git_command(
    description: &str,
    args: &[&str],
    maybe_error: bool,
    verbose: bool,
) -> Result<Output, GitCommandError> {
    if verbose {
        println!("[git {}]", args.join(" ").yellow());
    }
    let output = Command::new("git").args(args).output();
    match output {
        Ok(output) => {
            if output.status.success() && !output.stdout.is_empty() && verbose {
                println!("{}", String::from_utf8_lossy(&output.stdout));
            }
            if !output.status.success() && !maybe_error {
                return Err(GitCommandError {
                    description: description.to_string(),
                    args: args.iter().map(|arg| (*arg).to_string()).collect(),
                    status: Some(output.status),
                    stdout: output.stdout,
                    stderr: output.stderr,
                });
            }
            Ok(output)
        }
        Err(e) => Err(GitCommandError {
            description: description.to_string(),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            status: None,
            stdout: Vec::new(),
            stderr: e.to_string().into_bytes(),
        }),
    }
}

/// Check if the working directory is clean
///
/// # Arguments
///
/// * `verbose` - Whether to print the git command and its output.
pub fn is_clean(verbose: bool) -> Result<bool, GitCommandError> {
    Ok(run_git_command(
        "check working directory status",
        &["status", "--porcelain"],
        false,
        verbose,
    )?
    .stdout
    .is_empty())
}

pub fn current_branch_name(verbose: bool) -> Result<String, GitCommandError> {
    let output = run_git_command(
        "get current branch",
        &["rev-parse", "--abbrev-ref", "HEAD"],
        false,
        verbose,
    )?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn current_review_metadata(verbose: bool) -> Result<ReviewMetadata, ReviewMetadataError> {
    let branch = current_branch_name(verbose).map_err(ReviewMetadataError::Git)?;
    if !branch.starts_with("review-") {
        return Err(ReviewMetadataError::InvalidBranchName);
    }
    read_review_metadata(&branch, verbose)
}

/// Resolved remote tracking branch information.
pub struct ResolvedBranch {
    /// The remote name (e.g. "origin", "upstream")
    pub remote: String,
    /// The local branch name on the remote (e.g. "develop", "feature/login")
    pub remote_branch: String,
    /// The full tracking ref (e.g. "origin/develop")
    pub tracking_ref: String,
}

/// Resolves a branch name to its remote tracking branch information.
///
/// It checks:
/// 1. If it's already a valid remote tracking branch (e.g., origin/main).
/// 2. If it's a local branch with an upstream configured (e.g., @{upstream}).
/// 3. Fallback: assumes it's on 'origin' if it exists there.
pub fn resolve_remote_tracking_branch(
    branch_or_ref: &str,
    verbose: bool,
) -> Result<ResolvedBranch, GitCommandError> {
    // 1. Check if it's already a valid remote-tracking branch
    let verify_output = run_git_command(
        "verify if branch is already a remote tracking branch",
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/remotes/{}", branch_or_ref),
        ],
        true,
        verbose,
    )?;

    if verify_output.status.success() {
        // It's a remote tracking branch. We need to split it into remote and branch.
        // Assuming format is <remote>/<branch_name>. We can get remotes to find the remote name.
        let remotes_output = run_git_command("get remotes", &["remote"], false, verbose)?;
        let remotes_str = String::from_utf8_lossy(&remotes_output.stdout);
        let mut best_remote = String::new();
        for remote in remotes_str.lines() {
            let remote = remote.trim();
            if branch_or_ref.starts_with(&format!("{}/", remote))
                && remote.len() > best_remote.len()
            {
                best_remote = remote.to_string();
            }
        }

        if !best_remote.is_empty() {
            let remote_branch = branch_or_ref
                .strip_prefix(&format!("{}/", best_remote))
                .unwrap()
                .to_string();
            return Ok(ResolvedBranch {
                remote: best_remote,
                remote_branch,
                tracking_ref: branch_or_ref.to_string(),
            });
        }
    }

    // 2. Check if it's a local branch with an upstream configured
    let upstream_output = run_git_command(
        "get upstream branch",
        &[
            "rev-parse",
            "--abbrev-ref",
            &format!("{}@{{upstream}}", branch_or_ref),
        ],
        true,
        verbose,
    )?;

    if upstream_output.status.success() {
        let tracking_ref = String::from_utf8_lossy(&upstream_output.stdout)
            .trim()
            .to_string();

        // Extract remote and remote_branch from tracking_ref using the configured remote for the branch
        let remote_output = run_git_command(
            "get configured remote",
            &["config", &format!("branch.{}.remote", branch_or_ref)],
            true,
            verbose,
        )?;

        let remote = if remote_output.status.success() {
            String::from_utf8_lossy(&remote_output.stdout)
                .trim()
                .to_string()
        } else {
            "origin".to_string() // fallback if something is weird
        };

        let remote_branch = if tracking_ref.starts_with(&format!("{}/", remote)) {
            tracking_ref
                .strip_prefix(&format!("{}/", remote))
                .unwrap()
                .to_string()
        } else {
            branch_or_ref.to_string() // fallback
        };

        return Ok(ResolvedBranch {
            remote,
            remote_branch,
            tracking_ref,
        });
    }

    // 3. Fallback: check if the branch exists on any remote (default to 'origin' if it's there)
    // First, let's just see if it exists on origin.
    let ls_remote_output = run_git_command(
        "check if branch exists on origin",
        &[
            "ls-remote",
            "--exit-code",
            "origin",
            &format!("refs/heads/{}", branch_or_ref),
        ],
        true,
        verbose,
    )?;

    if ls_remote_output.status.success() {
        return Ok(ResolvedBranch {
            remote: "origin".to_string(),
            remote_branch: branch_or_ref.to_string(),
            tracking_ref: format!("origin/{}", branch_or_ref),
        });
    }

    // If we reach here, we can't reliably resolve it. Default to origin/branch and let git fail natively later
    // if it's really invalid.
    Ok(ResolvedBranch {
        remote: "origin".to_string(),
        remote_branch: branch_or_ref.to_string(),
        tracking_ref: format!("origin/{}", branch_or_ref),
    })
}

#[cfg(test)]
mod tests {
    use super::{suffixed_review_branch, ReviewMetadata};

    #[test]
    fn review_identity_suffix_matches_fixed_length_framed_fnv1a_vector() {
        let metadata = ReviewMetadata {
            target: "main".to_string(),
            source: "feature/foo".to_string(),
        };

        assert_eq!(
            suffixed_review_branch("review-main-feature_foo", &metadata),
            "review-main-feature_foo-49caf74ca44ff0fe"
        );
    }
}
