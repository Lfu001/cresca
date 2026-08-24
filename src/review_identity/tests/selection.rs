use super::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use tempfile::TempDir;

#[test]
fn exact_and_transition_candidates_are_ambiguous_together() {
    let request = request_for_plain_remote("refs/heads/dev", "origin", "refs/heads/dev");
    let candidates = vec![
        candidate("remote-review", remote_identity("origin", "refs/heads/dev")),
        candidate("local-review", anchored_local_identity("refs/heads/dev")),
    ];

    let requested = request.source.canonical.clone();
    let error = select_v2(&request, candidates, move |_| Ok(Some(requested.clone()))).unwrap_err();

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
