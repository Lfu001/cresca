use crate::git::GitCommandError;
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
            return Err(BranchResolutionError::Message(
                "Local branch reference must include a branch name.".to_string(),
            ));
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
