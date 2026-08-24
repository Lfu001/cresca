use super::{
    allocate_new_review_branch_with, decode_stored_fields, decode_v2_fields,
    load_candidates_from_branches, load_review_candidates, select_review, suffixed_review_branch,
    write_review_identity_v2, LegacyReviewIdentity, ReviewIdentity, ReviewRequest, ReviewSelection,
    ReviewSide, StoredReview, StoredReviewIdentity, REVIEW_METADATA_V2,
};
use crate::branch_ref::{BranchResolutionError, CanonicalBranch, ResolutionMode, ResolvedBranch};
use std::collections::{BTreeMap, BTreeSet};
fn resolved_source(
    canonical: CanonicalBranch,
    anchor: Option<&str>,
    mode: ResolutionMode,
) -> ResolvedBranch {
    ResolvedBranch {
        requested: anchor.unwrap_or("origin/dev").to_string(),
        canonical,
        local_anchor: anchor.map(str::to_string),
        mode,
        commit_oid: "1111111111111111111111111111111111111111".to_string(),
    }
}

fn remote_side(remote: &str, reference: &str) -> ReviewSide {
    ReviewSide {
        canonical: CanonicalBranch::Remote {
            remote: remote.to_string(),
            reference: reference.to_string(),
        },
        local_anchors: BTreeSet::new(),
    }
}

fn request_for_plain_remote(anchor: &str, remote: &str, reference: &str) -> ReviewRequest {
    ReviewRequest {
        target: resolved_source(
            CanonicalBranch::Remote {
                remote: "origin".to_string(),
                reference: "refs/heads/main".to_string(),
            },
            Some("refs/heads/main"),
            ResolutionMode::Plain,
        ),
        source: resolved_source(
            CanonicalBranch::Remote {
                remote: remote.to_string(),
                reference: reference.to_string(),
            },
            Some(anchor),
            ResolutionMode::Plain,
        ),
    }
}

fn request_for_explicit_remote(remote: &str, reference: &str) -> ReviewRequest {
    ReviewRequest {
        target: resolved_source(
            CanonicalBranch::Remote {
                remote: "origin".to_string(),
                reference: "refs/heads/main".to_string(),
            },
            None,
            ResolutionMode::ExplicitRemote,
        ),
        source: resolved_source(
            CanonicalBranch::Remote {
                remote: remote.to_string(),
                reference: reference.to_string(),
            },
            None,
            ResolutionMode::ExplicitRemote,
        ),
    }
}

fn candidate(branch: &str, source: ReviewSide) -> StoredReview {
    StoredReview {
        branch: branch.to_string(),
        identity: StoredReviewIdentity::V2(ReviewIdentity {
            target: remote_side("origin", "refs/heads/main"),
            source,
        }),
    }
}

fn select_v2<F>(
    request: &ReviewRequest,
    candidates: Vec<StoredReview>,
    resolve_anchor: F,
) -> Result<ReviewSelection, super::ReviewSelectionError>
where
    F: FnMut(&str) -> Result<Option<CanonicalBranch>, BranchResolutionError>,
{
    select_review(
        request,
        candidates,
        resolve_anchor,
        |raw| panic!("unexpected legacy resolver call for `{raw}`"),
        false,
    )
}

fn resolve_legacy_request(
    request: &ReviewRequest,
    raw: &str,
) -> Result<ResolvedBranch, BranchResolutionError> {
    if raw == request.target.requested {
        Ok(request.target.clone())
    } else if raw == request.source.requested {
        Ok(request.source.clone())
    } else {
        Err(BranchResolutionError::Message(format!(
            "unexpected saved legacy endpoint `{raw}`"
        )))
    }
}

fn remote_identity(remote: &str, reference: &str) -> ReviewSide {
    remote_side(remote, reference)
}

fn local_identity(reference: &str) -> ReviewSide {
    ReviewSide {
        canonical: CanonicalBranch::Local {
            reference: reference.to_string(),
        },
        local_anchors: BTreeSet::new(),
    }
}

fn anchored_local_identity(reference: &str) -> ReviewSide {
    ReviewSide {
        canonical: CanonicalBranch::Local {
            reference: reference.to_string(),
        },
        local_anchors: BTreeSet::from([reference.to_string()]),
    }
}

mod allocation;
mod metadata;
mod selection;
