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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review_identity::metadata::{load_review_candidates, write_review_identity_v2};
    use crate::review_identity::model::LegacyReviewIdentity;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::{Mutex, OnceLock};
    use tempfile::TempDir;

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
    ) -> Result<ReviewSelection, ReviewSelectionError>
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

    #[test]
    fn exact_and_transition_candidates_are_ambiguous_together() {
        let request = request_for_plain_remote("refs/heads/dev", "origin", "refs/heads/dev");
        let candidates = vec![
            candidate("remote-review", remote_identity("origin", "refs/heads/dev")),
            candidate("local-review", anchored_local_identity("refs/heads/dev")),
        ];

        let requested = request.source.canonical.clone();
        let error =
            select_v2(&request, candidates, move |_| Ok(Some(requested.clone()))).unwrap_err();

        assert!(error.to_string().contains("remote-review"));
        assert!(error.to_string().contains("local-review"));
    }

    #[test]
    fn explicit_remote_does_not_match_local_anchor_transition() {
        let request = request_for_explicit_remote("origin", "refs/heads/dev");
        let candidates = vec![
            candidate("remote-review", remote_identity("origin", "refs/heads/dev")),
            candidate("local-review", anchored_local_identity("refs/heads/dev")),
        ];

        let ReviewSelection::Existing(existing) =
            select_v2(&request, candidates, |_| Ok(None)).unwrap()
        else {
            panic!("the exact remote review must be selected");
        };
        assert_eq!(existing.branch, "remote-review");
    }

    #[test]
    fn zero_candidates_returns_new_identity() {
        let request = request_for_plain_remote("refs/heads/dev", "origin", "refs/heads/dev");

        assert_eq!(
            select_v2(&request, Vec::new(), |_| Ok(None)).unwrap(),
            ReviewSelection::New {
                identity: ReviewIdentity {
                    target: ReviewSide {
                        canonical: CanonicalBranch::Remote {
                            remote: "origin".to_string(),
                            reference: "refs/heads/main".to_string(),
                        },
                        local_anchors: BTreeSet::from(["refs/heads/main".to_string()]),
                    },
                    source: ReviewSide {
                        canonical: CanonicalBranch::Remote {
                            remote: "origin".to_string(),
                            reference: "refs/heads/dev".to_string(),
                        },
                        local_anchors: BTreeSet::from(["refs/heads/dev".to_string()]),
                    },
                }
            }
        );
    }

    #[test]
    fn one_exact_v2_candidate_is_reused() {
        let request = request_for_plain_remote("refs/heads/dev", "origin", "refs/heads/dev");
        let stored = ReviewIdentity {
            target: remote_side("origin", "refs/heads/main"),
            source: remote_side("origin", "refs/heads/dev"),
        };

        let selected = select_v2(
            &request,
            vec![StoredReview {
                branch: "review-dev".to_string(),
                identity: StoredReviewIdentity::V2(stored.clone()),
            }],
            |_| Ok(None),
        )
        .unwrap();

        let ReviewSelection::Existing(existing) = selected else {
            panic!("expected the exact review to be reused");
        };
        assert_eq!(existing.branch, "review-dev");
        assert_eq!(existing.stored, StoredReviewIdentity::V2(stored));
        assert_eq!(
            existing.next_identity.source.local_anchors,
            BTreeSet::from(["refs/heads/dev".to_string()])
        );
    }

    #[test]
    fn two_exact_v2_candidates_are_rejected_in_sorted_order() {
        let request = request_for_explicit_remote("origin", "refs/heads/dev");
        let stored = ReviewIdentity {
            target: remote_side("origin", "refs/heads/main"),
            source: remote_side("origin", "refs/heads/dev"),
        };
        let error = select_v2(
            &request,
            vec![
                StoredReview {
                    branch: "z-review".to_string(),
                    identity: StoredReviewIdentity::V2(stored.clone()),
                },
                StoredReview {
                    branch: "a-review".to_string(),
                    identity: StoredReviewIdentity::V2(stored),
                },
            ],
            |_| Ok(None),
        )
        .unwrap_err()
        .to_string();

        assert!(error.find("a-review").unwrap() < error.find("z-review").unwrap());
    }

    #[test]
    fn exact_raw_v1_candidate_is_reused() {
        let request = request_for_plain_remote("refs/heads/dev", "origin", "refs/heads/dev");
        let legacy = LegacyReviewIdentity {
            target: "refs/heads/main".to_string(),
            source: "refs/heads/dev".to_string(),
        };

        let selected = select_review(
            &request,
            vec![StoredReview {
                branch: "legacy-review".to_string(),
                identity: StoredReviewIdentity::V1(legacy.clone()),
            }],
            |_| Ok(None),
            |raw| resolve_legacy_request(&request, raw),
            false,
        )
        .unwrap();

        let ReviewSelection::Existing(existing) = selected else {
            panic!("expected the legacy review to be reused");
        };
        assert_eq!(existing.branch, "legacy-review");
        assert_eq!(existing.stored, StoredReviewIdentity::V1(legacy));
        assert_eq!(existing.next_identity, request.identity());
    }

    #[test]
    fn exact_v1_and_exact_v2_candidates_are_ambiguous_together() {
        let request = request_for_plain_remote("refs/heads/dev", "origin", "refs/heads/dev");
        let error = select_review(
            &request,
            vec![
                StoredReview {
                    branch: "legacy-review".to_string(),
                    identity: StoredReviewIdentity::V1(LegacyReviewIdentity {
                        target: "refs/heads/main".to_string(),
                        source: "refs/heads/dev".to_string(),
                    }),
                },
                StoredReview {
                    branch: "canonical-review".to_string(),
                    identity: StoredReviewIdentity::V2(ReviewIdentity {
                        target: remote_side("origin", "refs/heads/main"),
                        source: remote_side("origin", "refs/heads/dev"),
                    }),
                },
            ],
            |_| Ok(None),
            |raw| resolve_legacy_request(&request, raw),
            false,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("legacy-review"));
        assert!(error.contains("canonical-review"));
    }

    #[test]
    fn unmatched_candidate_does_not_validate_partial_transition() {
        let mut request = request_for_plain_remote("refs/heads/dev", "origin", "refs/heads/dev");
        request.target.canonical = CanonicalBranch::Remote {
            remote: "upstream".to_string(),
            reference: "refs/heads/main".to_string(),
        };
        let stored = ReviewIdentity {
            target: ReviewSide {
                canonical: CanonicalBranch::Remote {
                    remote: "origin".to_string(),
                    reference: "refs/heads/main".to_string(),
                },
                local_anchors: BTreeSet::from(["refs/heads/main".to_string()]),
            },
            source: local_identity("refs/heads/unrelated"),
        };

        assert!(matches!(
            select_v2(
                &request,
                vec![StoredReview {
                    branch: "unrelated-review".to_string(),
                    identity: StoredReviewIdentity::V2(stored),
                }],
                |_| Ok(Some(CanonicalBranch::Local {
                    reference: "refs/heads/elsewhere".to_string(),
                })),
            )
            .unwrap(),
            ReviewSelection::New { .. }
        ));
    }

    fn git_at(repo: &Path, args: &[&str]) {
        assert!(Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .unwrap()
            .success());
    }

    struct CurrentDirectory {
        original: PathBuf,
    }

    impl CurrentDirectory {
        fn enter(path: &Path) -> Self {
            let original = std::env::current_dir().unwrap();
            std::env::set_current_dir(path).unwrap();
            Self { original }
        }
    }

    impl Drop for CurrentDirectory {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.original).unwrap();
        }
    }

    #[test]
    fn same_named_tag_does_not_hide_existing_review_candidate() {
        static CURRENT_DIR_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _lock = CURRENT_DIR_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let repo = TempDir::new().unwrap();
        git_at(repo.path(), &["init", "--initial-branch=main", "--quiet"]);
        git_at(repo.path(), &["config", "user.name", "Cresca Test"]);
        git_at(
            repo.path(),
            &["config", "user.email", "cresca@example.invalid"],
        );
        git_at(
            repo.path(),
            &["commit", "--allow-empty", "--quiet", "-m", "base"],
        );
        git_at(repo.path(), &["branch", "review-demo"]);
        git_at(repo.path(), &["tag", "review-demo"]);

        let identity = ReviewIdentity {
            target: remote_side("origin", "refs/heads/main"),
            source: remote_side("origin", "refs/heads/dev"),
        };
        let candidates = {
            let _directory = CurrentDirectory::enter(repo.path());
            write_review_identity_v2("review-demo", &identity, false).unwrap();
            load_review_candidates(false).unwrap()
        };

        let request = request_for_explicit_remote("origin", "refs/heads/dev");
        let ReviewSelection::Existing(existing) =
            select_v2(&request, candidates, |_| Ok(None)).unwrap()
        else {
            panic!("the existing review must be selected instead of allocating a duplicate");
        };
        assert_eq!(existing.branch, "review-demo");
        assert_eq!(existing.stored, StoredReviewIdentity::V2(identity));
    }

    #[test]
    fn compatible_transition_prunes_retains_and_inserts_anchors() {
        let request = request_for_plain_remote("refs/heads/dev", "upstream", "refs/heads/team-dev");
        let stored = ReviewIdentity {
            target: remote_side("origin", "refs/heads/main"),
            source: ReviewSide {
                canonical: CanonicalBranch::Local {
                    reference: "refs/heads/dev".to_string(),
                },
                local_anchors: BTreeSet::from([
                    "refs/heads/deleted".to_string(),
                    "refs/heads/dev".to_string(),
                    "refs/heads/kept".to_string(),
                ]),
            },
        };
        let requested = request.source.canonical.clone();

        let ReviewSelection::Existing(existing) = select_v2(
            &request,
            vec![StoredReview {
                branch: "transition-review".to_string(),
                identity: StoredReviewIdentity::V2(stored),
            }],
            move |anchor| match anchor {
                "refs/heads/deleted" => Ok(None),
                "refs/heads/dev" => Ok(Some(requested.clone())),
                "refs/heads/kept" => Ok(Some(requested.clone())),
                other => panic!("unexpected stored anchor: {other}"),
            },
        )
        .unwrap() else {
            panic!("the fully compatible transition must reuse its review");
        };

        assert_eq!(
            existing.next_identity.source,
            ReviewSide {
                canonical: CanonicalBranch::Remote {
                    remote: "upstream".to_string(),
                    reference: "refs/heads/team-dev".to_string(),
                },
                local_anchors: BTreeSet::from([
                    "refs/heads/dev".to_string(),
                    "refs/heads/kept".to_string(),
                ]),
            }
        );
    }

    #[test]
    fn compatible_transition_rejects_differing_anchor_destination() {
        let request = request_for_plain_remote("refs/heads/dev", "upstream", "refs/heads/team-dev");
        let stored = ReviewIdentity {
            target: remote_side("origin", "refs/heads/main"),
            source: ReviewSide {
                canonical: CanonicalBranch::Local {
                    reference: "refs/heads/dev".to_string(),
                },
                local_anchors: BTreeSet::from([
                    "refs/heads/dev".to_string(),
                    "refs/heads/diverged".to_string(),
                ]),
            },
        };
        let requested = request.source.canonical.clone();

        let error = select_v2(
            &request,
            vec![StoredReview {
                branch: "transition-review".to_string(),
                identity: StoredReviewIdentity::V2(stored),
            }],
            move |anchor| match anchor {
                "refs/heads/dev" => Ok(Some(requested.clone())),
                "refs/heads/diverged" => Ok(Some(CanonicalBranch::Remote {
                    remote: "fork".to_string(),
                    reference: "refs/heads/other".to_string(),
                })),
                other => panic!("unexpected stored anchor: {other}"),
            },
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("transition-review"));
        assert!(error.contains("refs/heads/diverged"));
        assert!(error.contains("fork/other"));
        assert!(error.contains("upstream/team-dev"));
    }
}
