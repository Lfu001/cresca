use super::metadata::V2_FIELDS;
use super::model::{ReviewIdentity, ReviewSelectionError, ReviewSide};
use crate::branch_ref::CanonicalBranch;
use crate::git::{review_config_values, run_git_command, GitCommandError};

fn canonical_identity_hash(identity: &ReviewIdentity) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = OFFSET;
    let mut feed = |value: &str| {
        for byte in (value.len() as u64)
            .to_be_bytes()
            .into_iter()
            .chain(value.bytes())
        {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(PRIME);
        }
    };
    let mut feed_side = |position: &str, side: &ReviewSide| {
        feed(position);
        match &side.canonical {
            CanonicalBranch::Local { reference } => {
                feed("local");
                feed(reference);
            }
            CanonicalBranch::Remote { remote, reference } => {
                feed("remote");
                feed(remote);
                feed(reference);
            }
        }
    };
    feed_side("target", &identity.target);
    feed_side("source", &identity.source);
    hash
}

fn suffixed_review_branch(base: &str, identity: &ReviewIdentity) -> String {
    format!("{base}-{:016x}", canonical_identity_hash(identity))
}

fn allocate_new_review_branch_with<F>(
    base: &str,
    identity: &ReviewIdentity,
    mut branch_exists: F,
) -> Result<String, ReviewSelectionError>
where
    F: FnMut(&str) -> Result<bool, GitCommandError>,
{
    if !branch_exists(base)? {
        return Ok(base.to_string());
    }
    let suffix = suffixed_review_branch(base, identity);
    if !branch_exists(&suffix)? {
        return Ok(suffix);
    }
    Err(ReviewSelectionError::Conflict(format!(
        "Found conflicting review branches `{base}` and `{suffix}`. Delete or rename an occupied local review branch before retrying."
    )))
}

pub fn allocate_new_review_branch(
    base: &str,
    identity: &ReviewIdentity,
    verbose: bool,
) -> Result<String, ReviewSelectionError> {
    allocate_new_review_branch_with(base, identity, |branch| {
        let branch_exists = run_git_command(
            "check existence of review branch",
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
            &[1],
            verbose,
        )?
        .status
        .success();
        if branch_exists {
            return Ok(true);
        }
        for field in V2_FIELDS.into_iter().chain(["target", "source"]) {
            if !review_config_values(branch, field, verbose)?.is_empty() {
                return Ok(true);
            }
        }
        Ok(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::branch_ref::{ResolutionMode, ResolvedBranch};
    use crate::review_identity::model::ReviewRequest;
    use std::collections::BTreeSet;

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

    #[test]
    fn canonical_collision_hash_has_a_fixed_vector() {
        let identity = ReviewIdentity {
            target: remote_side("origin", "refs/heads/main"),
            source: remote_side("origin", "refs/heads/dev"),
        };

        assert_eq!(
            suffixed_review_branch("review-main-dev", &identity),
            "review-main-dev-89555ad1f942bfb8"
        );
    }

    #[test]
    fn canonical_collision_suffix_ignores_raw_cli_spelling_and_anchors() {
        let mut plain = request_for_plain_remote("refs/heads/dev", "origin", "refs/heads/dev");
        plain.source.requested = "dev".to_string();
        let explicit = request_for_explicit_remote("origin", "refs/heads/dev");

        assert_eq!(
            suffixed_review_branch("review", &plain.identity()),
            suffixed_review_branch("review", &explicit.identity())
        );
    }

    #[test]
    fn occupied_base_and_suffix_are_rejected_by_allocator() {
        let identity = ReviewIdentity {
            target: remote_side("origin", "refs/heads/main"),
            source: remote_side("origin", "refs/heads/dev"),
        };
        let error = allocate_new_review_branch_with("review", &identity, |_| Ok(true))
            .unwrap_err()
            .to_string();

        assert!(error.contains("`review`"));
        assert!(error.contains("`review-89555ad1f942bfb8`"));
    }
}
