use super::metadata::V2_FIELDS;
use super::model::{ReviewIdentity, ReviewSelectionError, ReviewSide};
use crate::branch_ref::CanonicalBranch;
use crate::git::{review_config_values, run_git_command, GitCommandError};

pub(super) fn canonical_identity_hash(identity: &ReviewIdentity) -> u64 {
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

pub(super) fn suffixed_review_branch(base: &str, identity: &ReviewIdentity) -> String {
    format!("{base}-{:016x}", canonical_identity_hash(identity))
}

pub(super) fn allocate_new_review_branch_with<F>(
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
