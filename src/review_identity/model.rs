use crate::branch_ref::{BranchResolutionError, CanonicalBranch, ResolutionMode, ResolvedBranch};
use crate::git::GitCommandError;
use std::collections::BTreeSet;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewSide {
    pub canonical: CanonicalBranch,
    pub local_anchors: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewIdentity {
    pub target: ReviewSide,
    pub source: ReviewSide,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyReviewIdentity {
    pub target: String,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoredReviewIdentity {
    V1(LegacyReviewIdentity),
    V2(ReviewIdentity),
}

#[derive(Clone, Debug)]
pub struct ReviewRequest {
    pub target: ResolvedBranch,
    pub source: ResolvedBranch,
}

impl ReviewRequest {
    pub fn identity(&self) -> ReviewIdentity {
        fn side(resolved: &ResolvedBranch) -> ReviewSide {
            let local_anchors = if resolved.mode == ResolutionMode::Plain {
                resolved.local_anchor.iter().cloned().collect()
            } else {
                BTreeSet::new()
            };
            ReviewSide {
                canonical: resolved.canonical.clone(),
                local_anchors,
            }
        }

        ReviewIdentity {
            target: side(&self.target),
            source: side(&self.source),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExistingReview {
    pub branch: String,
    pub stored: StoredReviewIdentity,
    pub next_identity: ReviewIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewSelection {
    New { identity: ReviewIdentity },
    Existing(ExistingReview),
}

#[derive(Clone, Debug)]
pub struct StoredReview {
    pub branch: String,
    pub identity: StoredReviewIdentity,
}

#[derive(Debug)]
pub enum ReviewSelectionError {
    Git(GitCommandError),
    Conflict(String),
    RelevantReviewInvalid { branch: String, reason: String },
}

impl fmt::Display for ReviewSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Git(error) => formatter.write_str(&error.description),
            Self::Conflict(message) => formatter.write_str(message),
            Self::RelevantReviewInvalid { branch, reason } => write!(
                formatter,
                "Review branch `{branch}` has invalid relevant metadata: {reason}"
            ),
        }
    }
}

impl From<GitCommandError> for ReviewSelectionError {
    fn from(error: GitCommandError) -> Self {
        Self::Git(error)
    }
}

impl From<BranchResolutionError> for ReviewSelectionError {
    fn from(error: BranchResolutionError) -> Self {
        match error {
            BranchResolutionError::Git(error) => Self::Git(error),
            BranchResolutionError::Message(message) => Self::Conflict(message),
        }
    }
}

#[derive(Debug)]
pub enum ReviewIdentityReadError {
    Missing,
    UnsupportedVersion(String),
    Invalid(String),
    Git(GitCommandError),
}

impl From<GitCommandError> for ReviewIdentityReadError {
    fn from(error: GitCommandError) -> Self {
        Self::Git(error)
    }
}
