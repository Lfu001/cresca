use colored::Colorize;
use std::ffi::OsStr;
use std::io::Write;
use std::process::{Command, ExitStatus, Output, Stdio};

#[derive(Debug, PartialEq, Eq)]
pub struct GitCommandError {
    pub description: String,
    pub args: Vec<String>,
    pub status: Option<ExitStatus>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub const REVIEW_SCOPE_VERSION: &str = "1";

#[derive(Debug, PartialEq, Eq)]
pub struct ReviewScope {
    pub base_oid: String,
    pub end_oid: String,
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

pub(crate) fn review_config_key(branch: &str, field: &str) -> String {
    format!("branch.{branch}.cresca-{field}")
}

pub(crate) fn review_config_values(
    branch: &str,
    field: &str,
    verbose: bool,
) -> Result<Vec<String>, GitCommandError> {
    let key = review_config_key(branch, field);
    let output = run_git_command(
        "read review metadata",
        &["config", "--local", "--get-all", &key],
        &[1],
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

pub(crate) fn replace_review_config_value(
    branch: &str,
    field: &str,
    value: &str,
    description: &str,
    verbose: bool,
) -> Result<(), GitCommandError> {
    let key = review_config_key(branch, field);
    run_git_command(
        description,
        &["config", "--local", "--replace-all", &key, value],
        &[],
        verbose,
    )?;
    Ok(())
}

pub(crate) fn unset_review_config_values(
    branch: &str,
    field: &str,
    description: &str,
    verbose: bool,
) -> Result<(), GitCommandError> {
    let values = review_config_values(branch, field, verbose)?;
    if values.is_empty() {
        return Ok(());
    }
    let key = review_config_key(branch, field);
    run_git_command(
        description,
        &["config", "--local", "--unset-all", &key],
        &[],
        verbose,
    )?;
    Ok(())
}

pub(crate) fn add_review_config_value(
    branch: &str,
    field: &str,
    value: &str,
    description: &str,
    verbose: bool,
) -> Result<(), GitCommandError> {
    let key = review_config_key(branch, field);
    run_git_command(
        description,
        &["config", "--local", "--add", &key, value],
        &[],
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
    let value = format!(
        "{}:{}:{}",
        REVIEW_SCOPE_VERSION, scope.base_oid, scope.end_oid
    );
    run_git_command(
        "record review range",
        &["config", "--local", "--replace-all", &key, &value],
        &[],
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
    let mut fields = value.split(':');
    let (Some(version), Some(base_oid), Some(end_oid), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return Err(ReviewScopeError::Invalid);
    };
    if version != REVIEW_SCOPE_VERSION {
        return Err(ReviewScopeError::UnsupportedVersion(version.to_string()));
    }
    let valid_oid = |oid: &str| {
        !oid.is_empty() && oid.len() == 40 && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
    };
    if !valid_oid(base_oid) || !valid_oid(end_oid) {
        return Err(ReviewScopeError::Invalid);
    }
    let revisions = format!("{base_oid}^{{commit}}\n{end_oid}^{{commit}}\n");
    let output = run_git_command_with_input(
        "validate review range endpoint",
        &["cat-file", "--batch-check=%(objectname)"],
        revisions.as_bytes(),
        &[],
        verbose,
    )
    .map_err(ReviewScopeError::Git)?;
    let results: Vec<_> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    let expected = [base_oid, end_oid];
    if let Some((_, missing_oid)) = results
        .iter()
        .zip(&expected)
        .find(|(result, _)| result.ends_with(" missing"))
    {
        return Err(ReviewScopeError::UnavailableCommit(
            (*missing_oid).to_string(),
        ));
    }
    if results != expected {
        return Err(ReviewScopeError::Invalid);
    }
    Ok(ReviewScope {
        base_oid: base_oid.to_string(),
        end_oid: end_oid.to_string(),
    })
}

/// Run a git command and return the output
///
/// # Arguments
///
/// * `description` - The description of the git command.
/// * `args` - The arguments to pass to the git command.
/// * `allowed_exit_codes` - Exact nonzero exit codes accepted for an expected negative probe.
/// * `verbose` - Whether to print the git command and its output.
///
/// # Returns
///
/// * `Result<Output, GitCommandError>` - The output, or a fully captured Git failure.
pub fn run_git_command(
    description: &str,
    args: &[&str],
    allowed_exit_codes: &[i32],
    verbose: bool,
) -> Result<Output, GitCommandError> {
    if verbose {
        println!("[git {}]", args.join(" ").yellow());
    }
    let mut command = Command::new("git");
    command.args(args);
    if args.first() == Some(&"status") {
        command.env("GIT_OPTIONAL_LOCKS", "0");
    }
    evaluate_git_output(
        description,
        args,
        allowed_exit_codes,
        verbose,
        true,
        command.output(),
    )
}

/// Run a command that returns machine-readable stdout. Verbose mode still logs the command, but
/// never writes raw machine records to the terminal.
pub fn run_git_command_machine_output(
    description: &str,
    args: &[&str],
    allowed_exit_codes: &[i32],
    verbose: bool,
) -> Result<Output, GitCommandError> {
    if verbose {
        println!("[git {}]", args.join(" ").yellow());
    }
    let mut command = Command::new("git");
    command.args(args);
    evaluate_git_output(
        description,
        args,
        allowed_exit_codes,
        verbose,
        false,
        command.output(),
    )
}

pub fn run_git_command_with_env(
    description: &str,
    args: &[&str],
    env: &[(&str, &OsStr)],
    allowed_exit_codes: &[i32],
    verbose: bool,
) -> Result<Output, GitCommandError> {
    if verbose {
        println!("[git {}]", args.join(" ").yellow());
    }
    let mut command = Command::new("git");
    command.args(args);
    for (key, value) in env {
        command.env(key, value);
    }
    evaluate_git_output(
        description,
        args,
        allowed_exit_codes,
        verbose,
        true,
        command.output(),
    )
}

pub fn run_git_command_with_input(
    description: &str,
    args: &[&str],
    input: &[u8],
    allowed_exit_codes: &[i32],
    verbose: bool,
) -> Result<Output, GitCommandError> {
    if verbose {
        println!("[git {}]", args.join(" ").yellow());
    }
    let mut command = Command::new("git");
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = match command.spawn() {
        Ok(mut child) => {
            let write_result = child
                .stdin
                .take()
                .expect("piped Git stdin must be available")
                .write_all(input);
            match (write_result, child.wait_with_output()) {
                (Err(error), Ok(output)) if output.status.success() => Err(error),
                (_, output) => output,
            }
        }
        Err(error) => Err(error),
    };
    evaluate_git_output(description, args, allowed_exit_codes, verbose, true, output)
}

fn evaluate_git_output(
    description: &str,
    args: &[&str],
    allowed_exit_codes: &[i32],
    verbose: bool,
    print_success_stdout: bool,
    output: std::io::Result<Output>,
) -> Result<Output, GitCommandError> {
    match output {
        Ok(output) => {
            if output.status.success()
                && !output.stdout.is_empty()
                && verbose
                && print_success_stdout
            {
                println!("{}", String::from_utf8_lossy(&output.stdout));
            }
            let allowed = output
                .status
                .code()
                .is_some_and(|code| allowed_exit_codes.contains(&code));
            if !output.status.success() && !allowed {
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
        &[],
        verbose,
    )?
    .stdout
    .is_empty())
}

pub fn current_branch_name(verbose: bool) -> Result<String, GitCommandError> {
    let output = run_git_command(
        "get current branch",
        &["rev-parse", "--abbrev-ref", "HEAD"],
        &[],
        verbose,
    )?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn current_review_metadata(
    verbose: bool,
) -> Result<
    crate::review_identity::StoredReviewIdentity,
    crate::review_identity::ReviewIdentityReadError,
> {
    let branch = current_branch_name(verbose)
        .map_err(crate::review_identity::ReviewIdentityReadError::Git)?;
    crate::review_identity::read_stored_review_identity(&branch, verbose)
}
