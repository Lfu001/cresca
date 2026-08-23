use crate::git::{run_git_command, GitCommandError};
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalBranch {
    Local { reference: String },
    Remote { remote: String, reference: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolutionMode {
    ExplicitLocal,
    ExplicitRemote,
    Plain,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BranchRequest {
    ExplicitLocal { reference: String },
    ExplicitRemote { remote: String, branch_ref: String },
    Plain { name: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedBranch {
    pub requested: String,
    pub canonical: CanonicalBranch,
    pub local_anchor: Option<String>,
    pub mode: ResolutionMode,
    pub commit_oid: String,
}

#[derive(Debug)]
pub enum BranchResolutionError {
    Git(GitCommandError),
    Message(String),
}

impl fmt::Display for BranchResolutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Git(error) => formatter.write_str(&error.description),
            Self::Message(message) => formatter.write_str(message),
        }
    }
}

impl From<GitCommandError> for BranchResolutionError {
    fn from(error: GitCommandError) -> Self {
        Self::Git(error)
    }
}

impl From<BranchResolutionError> for crate::review::ReviewError {
    fn from(error: BranchResolutionError) -> Self {
        match error {
            BranchResolutionError::Git(error) => Self::Git(error),
            BranchResolutionError::Message(message) => Self::Message(message),
        }
    }
}

pub fn parse_branch_request(
    input: &str,
    configured_remotes: &[String],
) -> Result<BranchRequest, BranchResolutionError> {
    if let Some(name) = input.strip_prefix("refs/heads/") {
        if name.is_empty() {
            return Err(invalid_branch(input));
        }
        return Ok(BranchRequest::ExplicitLocal {
            reference: input.to_string(),
        });
    }

    let remote_input = input.strip_prefix("refs/remotes/").unwrap_or(input);
    let remote = configured_remotes
        .iter()
        .filter(|remote| remote_input.starts_with(&format!("{remote}/")))
        .max_by_key(|remote| remote.len());
    if let Some(remote) = remote {
        let branch = remote_input
            .strip_prefix(&format!("{remote}/"))
            .expect("matched remote prefix must strip");
        return Ok(BranchRequest::ExplicitRemote {
            remote: remote.clone(),
            branch_ref: format!("refs/heads/{branch}"),
        });
    }

    if input.starts_with("refs/") {
        return Err(BranchResolutionError::Message(format!(
            "Unsupported branch reference `{input}`. Use `refs/heads/<name>` or `<remote>/<name>`."
        )));
    }
    Ok(BranchRequest::Plain {
        name: input.to_string(),
    })
}

fn configured_remotes(verbose: bool) -> Result<Vec<String>, BranchResolutionError> {
    let output = run_git_command("list configured remotes", &["remote"], &[], verbose)?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|remote| !remote.is_empty())
        .map(str::to_owned)
        .collect())
}

fn invalid_branch(input: &str) -> BranchResolutionError {
    BranchResolutionError::Message(format!(
        "Branch input `{input}` is not a branch. Use a plain branch name, `refs/heads/<name>`, or `<remote>/<name>`."
    ))
}

fn validate_branch_name(
    name: &str,
    input: &str,
    verbose: bool,
) -> Result<(), BranchResolutionError> {
    let sha_like = name.len() >= 4 && name.bytes().all(|byte| byte.is_ascii_hexdigit());
    if sha_like {
        return Err(invalid_branch(input));
    }
    let output = run_git_command(
        &format!("validate branch input `{input}`"),
        &["check-ref-format", "--branch", name],
        &[128],
        verbose,
    )?;
    if !output.status.success() {
        return Err(invalid_branch(input));
    }
    Ok(())
}

fn validate_local_branch_ref(reference: &str, verbose: bool) -> Result<(), BranchResolutionError> {
    let name = reference
        .strip_prefix("refs/heads/")
        .ok_or_else(|| invalid_branch(reference))?;
    validate_branch_name(name, reference, verbose)?;
    let output = run_git_command(
        &format!("verify local branch `{reference}`"),
        &["show-ref", "--verify", "--quiet", reference],
        &[1],
        verbose,
    )?;
    if !output.status.success() {
        return Err(BranchResolutionError::Message(format!(
            "Local branch `{reference}` was not found."
        )));
    }
    Ok(())
}

fn validate_plain_branch_name(name: &str, verbose: bool) -> Result<(), BranchResolutionError> {
    validate_branch_name(name, name, verbose)
}

fn resolve_local_commit(reference: &str, verbose: bool) -> Result<String, BranchResolutionError> {
    let commit = format!("{reference}^{{commit}}");
    let output = run_git_command(
        &format!("resolve local branch `{reference}`"),
        &["rev-parse", "--verify", &commit],
        &[],
        verbose,
    )?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn resolve_remote_commit(
    remote: &str,
    branch_ref: &str,
    verbose: bool,
) -> Result<String, BranchResolutionError> {
    let display = format!("{remote}/{}", branch_ref.trim_start_matches("refs/heads/"));
    let output = run_git_command(
        &format!("query remote branch `{display}`"),
        &["ls-remote", "--exit-code", remote, branch_ref],
        &[2],
        verbose,
    )?;
    if output.status.code() == Some(2) {
        return Err(BranchResolutionError::Message(format!(
            "Remote branch `{display}` was confirmed absent on `{remote}`."
        )));
    }
    let oid = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().next())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            BranchResolutionError::Message(format!(
                "Remote branch `{display}` did not resolve to a commit."
            ))
        })?
        .to_string();
    run_git_command(
        &format!("fetch remote branch `{display}`"),
        &[
            "fetch",
            "--no-write-fetch-head",
            "--no-tags",
            "--refmap=",
            remote,
            branch_ref,
        ],
        &[],
        verbose,
    )?;
    let commit = format!("{oid}^{{commit}}");
    let output = run_git_command(
        &format!("validate fetched commit for `{display}`"),
        &["rev-parse", "--verify", &commit],
        &[],
        verbose,
    )?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[derive(Debug, PartialEq, Eq)]
enum UpstreamConfig {
    Absent,
    Local,
    Remote { remote: String, branch_ref: String },
}

fn config_values(key: &str, verbose: bool) -> Result<Vec<String>, BranchResolutionError> {
    let output = run_git_command(
        &format!("read upstream configuration `{key}`"),
        &["config", "--local", "--get-all", key],
        &[1],
        verbose,
    )?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect())
}

fn valid_merge_ref(reference: &str, verbose: bool) -> Result<bool, BranchResolutionError> {
    if !reference.starts_with("refs/heads/") || reference == "refs/heads/" {
        return Ok(false);
    }
    let output = run_git_command(
        &format!("validate upstream branch `{reference}`"),
        &["check-ref-format", reference],
        &[1],
        verbose,
    )?;
    Ok(output.status.success())
}

fn broken_upstream(local_name: &str, detail: &str) -> BranchResolutionError {
    BranchResolutionError::Message(format!(
        "Branch `{local_name}` has invalid configured upstream: {detail}."
    ))
}

fn read_upstream_config(
    local_name: &str,
    configured_remotes: &[String],
    verbose: bool,
) -> Result<UpstreamConfig, BranchResolutionError> {
    let remotes = config_values(&format!("branch.{local_name}.remote"), verbose)?;
    let merges = config_values(&format!("branch.{local_name}.merge"), verbose)?;
    match (remotes.as_slice(), merges.as_slice()) {
        ([], []) => Ok(UpstreamConfig::Absent),
        ([remote], [branch_ref]) if !valid_merge_ref(branch_ref, verbose)? => {
            Err(broken_upstream(local_name, "the merge branch is malformed"))
        }
        ([remote], [branch_ref]) if remote == "." => {
            let _ = branch_ref;
            Ok(UpstreamConfig::Local)
        }
        ([remote], [branch_ref]) if configured_remotes.contains(remote) => {
            Ok(UpstreamConfig::Remote {
                remote: remote.clone(),
                branch_ref: branch_ref.clone(),
            })
        }
        ([remote], [_]) => Err(broken_upstream(
            local_name,
            &format!("configured remote `{remote}` does not exist"),
        )),
        ([], [_]) => Err(broken_upstream(local_name, "the remote setting is missing")),
        ([_], []) => Err(broken_upstream(local_name, "the merge setting is missing")),
        _ => Err(broken_upstream(
            local_name,
            "the settings contain duplicate values",
        )),
    }
}

fn discover_remote_matches(
    name: &str,
    remotes: &[String],
    verbose: bool,
) -> Result<Vec<(String, String)>, BranchResolutionError> {
    let branch_ref = format!("refs/heads/{name}");
    let mut matches = Vec::new();
    for remote in remotes {
        let output = run_git_command(
            &format!("discover branch `{name}` on remote `{remote}`"),
            &["ls-remote", "--exit-code", remote, &branch_ref],
            &[2],
            verbose,
        )?;
        if output.status.success() {
            let oid = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().next())
                .filter(|oid| !oid.is_empty())
                .ok_or_else(|| {
                    BranchResolutionError::Message(format!(
                        "Remote `{remote}` returned an invalid result while discovering branch `{name}`."
                    ))
                })?
                .to_string();
            matches.push((remote.clone(), oid));
        }
    }
    Ok(matches)
}

fn resolve_remote(
    requested: &str,
    remote: String,
    branch_ref: String,
    local_anchor: Option<String>,
    mode: ResolutionMode,
    verbose: bool,
) -> Result<ResolvedBranch, BranchResolutionError> {
    let commit_oid =
        resolve_remote_commit(&remote, &branch_ref, verbose).map_err(|error| match error {
            BranchResolutionError::Git(mut error) => {
                error.description =
                    format!("resolve branch input `{requested}`: {}", error.description);
                BranchResolutionError::Git(error)
            }
            BranchResolutionError::Message(message) if !message.contains(requested) => {
                BranchResolutionError::Message(format!("Branch input `{requested}`: {message}"))
            }
            error => error,
        })?;
    Ok(ResolvedBranch {
        requested: requested.to_string(),
        canonical: CanonicalBranch::Remote {
            remote,
            reference: branch_ref,
        },
        local_anchor,
        mode,
        commit_oid,
    })
}

pub fn resolve_branch(input: &str, verbose: bool) -> Result<ResolvedBranch, BranchResolutionError> {
    let remotes = configured_remotes(verbose)?;
    match parse_branch_request(input, &remotes)? {
        BranchRequest::ExplicitLocal { reference } => {
            validate_local_branch_ref(&reference, verbose)?;
            let commit_oid = resolve_local_commit(&reference, verbose)?;
            Ok(ResolvedBranch {
                requested: input.to_string(),
                canonical: CanonicalBranch::Local { reference },
                local_anchor: None,
                mode: ResolutionMode::ExplicitLocal,
                commit_oid,
            })
        }
        BranchRequest::ExplicitRemote { remote, branch_ref } => {
            let name = branch_ref
                .strip_prefix("refs/heads/")
                .expect("parsed remote branch must use refs/heads");
            validate_branch_name(name, input, verbose)?;
            resolve_remote(
                input,
                remote,
                branch_ref,
                None,
                ResolutionMode::ExplicitRemote,
                verbose,
            )
        }
        BranchRequest::Plain { name } => {
            validate_plain_branch_name(&name, verbose)?;
            let local_ref = format!("refs/heads/{name}");
            let local_probe = run_git_command(
                &format!("check local branch `{input}`"),
                &["show-ref", "--verify", "--quiet", &local_ref],
                &[1],
                verbose,
            )?;
            let local_exists = local_probe.status.success();
            if local_exists {
                if let UpstreamConfig::Remote { remote, branch_ref } =
                    read_upstream_config(&name, &remotes, verbose)?
                {
                    return resolve_remote(
                        input,
                        remote,
                        branch_ref,
                        Some(local_ref),
                        ResolutionMode::Plain,
                        verbose,
                    );
                }
            }

            let matches = discover_remote_matches(&name, &remotes, verbose)?;
            match (local_exists, matches.as_slice()) {
                (true, []) => {
                    let commit_oid = resolve_local_commit(&local_ref, verbose)?;
                    Ok(ResolvedBranch {
                        requested: input.to_string(),
                        canonical: CanonicalBranch::Local {
                            reference: local_ref.clone(),
                        },
                        local_anchor: Some(local_ref),
                        mode: ResolutionMode::Plain,
                        commit_oid,
                    })
                }
                (true, _) => Err(BranchResolutionError::Message(format!(
                    "Branch `{input}` is ambiguous between local `{local_ref}` and remote branches. Use `{local_ref}` or `<remote>/{name}` explicitly."
                ))),
                (false, [(remote, _)]) => resolve_remote(
                    input,
                    remote.clone(),
                    format!("refs/heads/{name}"),
                    None,
                    ResolutionMode::Plain,
                    verbose,
                ),
                (false, []) => {
                    let tag_ref = format!("refs/tags/{name}");
                    let tag = run_git_command(
                        &format!("check whether `{input}` is only a tag"),
                        &["show-ref", "--verify", "--quiet", &tag_ref],
                        &[1],
                        verbose,
                    )?;
                    if tag.status.success() {
                        Err(invalid_branch(input))
                    } else {
                        Err(BranchResolutionError::Message(format!(
                            "Branch `{input}` was not found locally or on any configured remote."
                        )))
                    }
                }
                (false, _) => Err(BranchResolutionError::Message(format!(
                    "Branch `{input}` is ambiguous across multiple remotes. Use `<remote>/{name}` explicitly."
                ))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_branch_request, BranchRequest};

    fn remotes() -> Vec<String> {
        vec!["origin".to_string(), "team/upstream".to_string()]
    }

    #[test]
    fn parses_fully_qualified_local_reference() {
        assert_eq!(
            parse_branch_request("refs/heads/origin/dev", &remotes()).unwrap(),
            BranchRequest::ExplicitLocal {
                reference: "refs/heads/origin/dev".to_string(),
            }
        );
    }

    #[test]
    fn parses_remote_shorthand_with_longest_configured_prefix() {
        assert_eq!(
            parse_branch_request("team/upstream/feature/dev", &remotes()).unwrap(),
            BranchRequest::ExplicitRemote {
                remote: "team/upstream".to_string(),
                branch_ref: "refs/heads/feature/dev".to_string(),
            }
        );
    }

    #[test]
    fn parses_fully_qualified_remote_tracking_reference() {
        assert_eq!(
            parse_branch_request("refs/remotes/origin/dev", &remotes()).unwrap(),
            BranchRequest::ExplicitRemote {
                remote: "origin".to_string(),
                branch_ref: "refs/heads/dev".to_string(),
            }
        );
    }

    #[test]
    fn leaves_unqualified_name_for_plain_resolution() {
        assert_eq!(
            parse_branch_request("dev", &remotes()).unwrap(),
            BranchRequest::Plain {
                name: "dev".to_string(),
            }
        );
    }
}
