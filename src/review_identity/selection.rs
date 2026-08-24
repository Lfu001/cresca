use super::legacy::{configured_remotes_for_legacy_filter, next_v1_identity};
use super::model::{
    ExistingReview, ReviewIdentity, ReviewRequest, ReviewSelection, ReviewSelectionError,
    ReviewSide, StoredReview, StoredReviewIdentity,
};
use crate::branch_ref::{BranchResolutionError, CanonicalBranch, ResolutionMode, ResolvedBranch};
use std::collections::BTreeSet;

fn validate_stored_anchors<F>(
    stored: &ReviewSide,
    requested: &ReviewSide,
    mut resolve_anchor: F,
) -> Result<BTreeSet<String>, ReviewSelectionError>
where
    F: FnMut(&str) -> Result<Option<CanonicalBranch>, BranchResolutionError>,
{
    let mut next = BTreeSet::new();
    for anchor in &stored.local_anchors {
        match resolve_anchor(anchor)? {
            None => {}
            Some(canonical) if canonical == requested.canonical => {
                next.insert(anchor.clone());
            }
            Some(canonical) => {
                return Err(ReviewSelectionError::Conflict(format!(
                    "Stored local anchor `{anchor}` now resolves to `{}`, not the requested `{}`.",
                    describe_canonical(&canonical),
                    describe_canonical(&requested.canonical)
                )))
            }
        }
    }
    next.extend(requested.local_anchors.iter().cloned());
    Ok(next)
}

fn validate_anchor_transition<F>(
    stored: &ReviewSide,
    requested: &ReviewSide,
    resolve_anchor: F,
) -> Result<BTreeSet<String>, ReviewSelectionError>
where
    F: FnMut(&str) -> Result<Option<CanonicalBranch>, BranchResolutionError>,
{
    if requested.local_anchors.len() != 1 {
        return Err(ReviewSelectionError::Conflict(
            "A plain request needs exactly one local anchor to follow a review transition."
                .to_string(),
        ));
    }
    validate_stored_anchors(stored, requested, resolve_anchor)
}

fn describe_canonical(canonical: &CanonicalBranch) -> String {
    match canonical {
        CanonicalBranch::Local { reference } => reference.clone(),
        CanonicalBranch::Remote { remote, reference } => format!(
            "{remote}/{}",
            reference.strip_prefix("refs/heads/").unwrap_or(reference)
        ),
    }
}

fn can_transition(stored: &ReviewSide, requested: &ResolvedBranch) -> bool {
    if requested.mode != ResolutionMode::Plain {
        return false;
    }
    let Some(anchor) = requested.local_anchor.as_deref() else {
        return false;
    };
    stored.local_anchors.contains(anchor)
}

#[derive(Clone, Copy)]
enum SideCompatibility {
    Exact,
    Transition,
}

fn side_compatibility(
    stored: &ReviewSide,
    requested: &ResolvedBranch,
) -> Option<SideCompatibility> {
    if stored.canonical == requested.canonical {
        Some(SideCompatibility::Exact)
    } else if can_transition(stored, requested) {
        Some(SideCompatibility::Transition)
    } else {
        None
    }
}

fn next_side<F>(
    branch: &str,
    stored: &ReviewSide,
    requested: &ResolvedBranch,
    compatibility: SideCompatibility,
    mut resolve_anchor: F,
) -> Result<Option<ReviewSide>, ReviewSelectionError>
where
    F: FnMut(&str) -> Result<Option<CanonicalBranch>, BranchResolutionError>,
{
    if matches!(compatibility, SideCompatibility::Exact) {
        let mut next = stored.clone();
        if requested.mode == ResolutionMode::Plain {
            let requested_side = ReviewSide {
                canonical: requested.canonical.clone(),
                local_anchors: requested.local_anchor.iter().cloned().collect(),
            };
            next.local_anchors =
                validate_stored_anchors(stored, &requested_side, &mut resolve_anchor).map_err(
                    |error| match error {
                        ReviewSelectionError::Conflict(reason) => {
                            ReviewSelectionError::RelevantReviewInvalid {
                                branch: branch.to_string(),
                                reason,
                            }
                        }
                        other => other,
                    },
                )?;
        }
        return Ok(Some(next));
    }
    let mut next = ReviewSide {
        canonical: requested.canonical.clone(),
        local_anchors: requested.local_anchor.iter().cloned().collect(),
    };
    next.local_anchors = validate_anchor_transition(stored, &next, &mut resolve_anchor).map_err(
        |error| match error {
            ReviewSelectionError::Conflict(reason) => ReviewSelectionError::RelevantReviewInvalid {
                branch: branch.to_string(),
                reason,
            },
            other => other,
        },
    )?;
    Ok(Some(next))
}

fn next_v2_identity<F>(
    branch: &str,
    stored: &ReviewIdentity,
    request: &ReviewRequest,
    mut resolve_anchor: F,
) -> Result<Option<ReviewIdentity>, ReviewSelectionError>
where
    F: FnMut(&str) -> Result<Option<CanonicalBranch>, BranchResolutionError>,
{
    let Some(target_compatibility) = side_compatibility(&stored.target, &request.target) else {
        return Ok(None);
    };
    let Some(source_compatibility) = side_compatibility(&stored.source, &request.source) else {
        return Ok(None);
    };
    let Some(target) = next_side(
        branch,
        &stored.target,
        &request.target,
        target_compatibility,
        &mut resolve_anchor,
    )?
    else {
        return Ok(None);
    };
    let Some(source) = next_side(
        branch,
        &stored.source,
        &request.source,
        source_compatibility,
        &mut resolve_anchor,
    )?
    else {
        return Ok(None);
    };
    Ok(Some(ReviewIdentity { target, source }))
}

fn ambiguous_reviews(mut branches: Vec<String>) -> ReviewSelectionError {
    branches.sort();
    ReviewSelectionError::Conflict(format!(
        "Conflicting review branches: {}. Use explicit local or remote branch syntax, or correct the duplicate review metadata before retrying.",
        branches
            .iter()
            .map(|branch| format!("`{branch}`"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

pub fn select_review<F, G>(
    request: &ReviewRequest,
    candidates: Vec<StoredReview>,
    mut resolve_anchor: F,
    mut resolve_legacy: G,
    verbose: bool,
) -> Result<ReviewSelection, ReviewSelectionError>
where
    F: FnMut(&str) -> Result<Option<CanonicalBranch>, BranchResolutionError>,
    G: FnMut(&str) -> Result<ResolvedBranch, BranchResolutionError>,
{
    let requested_identity = request.identity();
    let remotes = if candidates
        .iter()
        .any(|candidate| matches!(candidate.identity, StoredReviewIdentity::V1(_)))
    {
        configured_remotes_for_legacy_filter(verbose)?
    } else {
        Vec::new()
    };
    let mut matches = Vec::new();
    for candidate in candidates {
        let next_identity = match &candidate.identity {
            StoredReviewIdentity::V1(legacy) => next_v1_identity(
                &candidate.branch,
                legacy,
                request,
                &remotes,
                &mut resolve_legacy,
                verbose,
            )?,
            StoredReviewIdentity::V2(identity) => {
                next_v2_identity(&candidate.branch, identity, request, &mut resolve_anchor)?
            }
        };
        if let Some(next_identity) = next_identity {
            matches.push(ExistingReview {
                branch: candidate.branch,
                stored: candidate.identity,
                next_identity,
            });
        }
    }
    match matches.len() {
        0 => Ok(ReviewSelection::New {
            identity: requested_identity,
        }),
        1 => Ok(ReviewSelection::Existing(
            matches.pop().expect("one match must be present"),
        )),
        _ => Err(ambiguous_reviews(
            matches.into_iter().map(|review| review.branch).collect(),
        )),
    }
}
