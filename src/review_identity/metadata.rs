use super::model::{
    LegacyReviewIdentity, ReviewIdentity, ReviewIdentityReadError, ReviewSelectionError,
    ReviewSide, StoredReview, StoredReviewIdentity,
};
use crate::branch_ref::CanonicalBranch;
use crate::git::{
    add_review_config_value, replace_review_config_value, review_config_values,
    run_git_command_machine_output, unset_review_config_values, GitCommandError,
};
use std::collections::{BTreeMap, BTreeSet};

pub const REVIEW_METADATA_V2: &str = "2";

pub(super) const V2_FIELDS: [&str; 9] = [
    "target-kind",
    "target-ref",
    "target-remote",
    "target-anchor",
    "source-kind",
    "source-ref",
    "source-remote",
    "source-anchor",
    "version",
];

fn read_fields(
    branch: &str,
    verbose: bool,
) -> Result<BTreeMap<String, Vec<String>>, ReviewIdentityReadError> {
    let mut fields = BTreeMap::new();
    for field in V2_FIELDS.into_iter().chain(["target", "source"]) {
        fields.insert(
            field.to_string(),
            review_config_values(branch, field, verbose)?,
        );
    }
    Ok(fields)
}

pub(super) fn decode_stored_fields(
    fields: &BTreeMap<String, Vec<String>>,
) -> Result<StoredReviewIdentity, ReviewIdentityReadError> {
    let values = |field: &str| fields.get(field).map(Vec::as_slice).unwrap_or(&[]);
    if fields.values().all(Vec::is_empty) {
        return Err(ReviewIdentityReadError::Missing);
    }
    let version = match values("version") {
        [version] => version,
        _ => {
            return Err(ReviewIdentityReadError::Invalid(
                "review metadata version must have exactly one value".to_string(),
            ))
        }
    };
    match version.as_str() {
        "1" => match (values("target"), values("source")) {
            ([target], [source]) if !target.is_empty() && !source.is_empty() => {
                Ok(StoredReviewIdentity::V1(LegacyReviewIdentity {
                    target: target.to_string(),
                    source: source.to_string(),
                }))
            }
            _ => Err(ReviewIdentityReadError::Invalid(
                "version 1 review metadata must have exactly one non-empty target and source"
                    .to_string(),
            )),
        },
        REVIEW_METADATA_V2 => {
            if !values("target").is_empty() || !values("source").is_empty() {
                return Err(ReviewIdentityReadError::Invalid(
                    "version 2 review metadata must not contain raw target or source fields"
                        .to_string(),
                ));
            }
            decode_v2_fields(fields)
                .map(StoredReviewIdentity::V2)
                .map_err(ReviewIdentityReadError::Invalid)
        }
        other => Err(ReviewIdentityReadError::UnsupportedVersion(
            other.to_string(),
        )),
    }
}

pub fn read_stored_review_identity(
    branch: &str,
    verbose: bool,
) -> Result<StoredReviewIdentity, ReviewIdentityReadError> {
    decode_stored_fields(&read_fields(branch, verbose)?)
}

pub(super) fn load_candidates_from_branches<I, F>(
    branches: I,
    mut read: F,
) -> Result<Vec<StoredReview>, ReviewSelectionError>
where
    I: IntoIterator<Item = String>,
    F: FnMut(&str) -> Result<StoredReviewIdentity, ReviewIdentityReadError>,
{
    let mut branches: Vec<_> = branches.into_iter().collect();
    branches.sort();
    let mut candidates = Vec::new();
    for branch in branches {
        match read(&branch) {
            Ok(identity) => candidates.push(StoredReview { branch, identity }),
            Err(ReviewIdentityReadError::Missing)
            | Err(ReviewIdentityReadError::UnsupportedVersion(_))
            | Err(ReviewIdentityReadError::Invalid(_)) => {}
            Err(ReviewIdentityReadError::Git(error)) => {
                return Err(ReviewSelectionError::Git(error))
            }
        }
    }
    Ok(candidates)
}

pub fn load_review_candidates(verbose: bool) -> Result<Vec<StoredReview>, ReviewSelectionError> {
    let output = run_git_command_machine_output(
        "list local branches for review identity",
        &[
            "for-each-ref",
            "--sort=refname",
            "--format=%(refname:lstrip=2)",
            "refs/heads",
        ],
        &[],
        verbose,
    )?;
    let branches = std::str::from_utf8(&output.stdout).map_err(|_| {
        ReviewSelectionError::Conflict(
            "Cannot identify review branches because Git returned a non-UTF-8 local branch name."
                .to_string(),
        )
    })?;
    load_candidates_from_branches(
        branches
            .lines()
            .filter(|branch| !branch.is_empty())
            .map(str::to_string),
        |branch| read_stored_review_identity(branch, verbose),
    )
}

fn write_side(
    branch: &str,
    name: &str,
    side: &ReviewSide,
    verbose: bool,
) -> Result<(), GitCommandError> {
    let (kind, reference, remote) = match &side.canonical {
        CanonicalBranch::Local { reference } => ("local", reference.as_str(), None),
        CanonicalBranch::Remote { remote, reference } => {
            ("remote", reference.as_str(), Some(remote.as_str()))
        }
    };
    replace_review_config_value(
        branch,
        &format!("{name}-kind"),
        kind,
        &format!("record review {name} kind"),
        verbose,
    )?;
    replace_review_config_value(
        branch,
        &format!("{name}-ref"),
        reference,
        &format!("record review {name} ref"),
        verbose,
    )?;
    match remote {
        Some(remote) => replace_review_config_value(
            branch,
            &format!("{name}-remote"),
            remote,
            &format!("record review {name} remote"),
            verbose,
        ),
        None => unset_review_config_values(
            branch,
            &format!("{name}-remote"),
            &format!("clear review {name} remote"),
            verbose,
        ),
    }
}

fn write_anchors(
    branch: &str,
    name: &str,
    anchors: &BTreeSet<String>,
    verbose: bool,
) -> Result<(), GitCommandError> {
    let field = format!("{name}-anchor");
    unset_review_config_values(
        branch,
        &field,
        &format!("clear review {name} anchors"),
        verbose,
    )?;
    for anchor in anchors {
        add_review_config_value(
            branch,
            &field,
            anchor,
            &format!("record review {name} anchor"),
            verbose,
        )?;
    }
    Ok(())
}

pub fn write_review_identity_v2(
    branch: &str,
    identity: &ReviewIdentity,
    verbose: bool,
) -> Result<(), GitCommandError> {
    unset_review_config_values(
        branch,
        "version",
        "clear review metadata version marker",
        verbose,
    )?;
    write_side(branch, "target", &identity.target, verbose)?;
    write_side(branch, "source", &identity.source, verbose)?;
    write_anchors(branch, "target", &identity.target.local_anchors, verbose)?;
    write_anchors(branch, "source", &identity.source.local_anchors, verbose)?;
    unset_review_config_values(branch, "target", "clear version 1 review target", verbose)?;
    unset_review_config_values(branch, "source", "clear version 1 review source", verbose)?;
    replace_review_config_value(
        branch,
        "version",
        REVIEW_METADATA_V2,
        "commit review metadata",
        verbose,
    )
}

fn decode_side(fields: &BTreeMap<String, Vec<String>>, name: &str) -> Result<ReviewSide, String> {
    let singleton = |field: String| match fields.get(&field).map(Vec::as_slice).unwrap_or(&[]) {
        [value] if !value.is_empty() => Ok(value),
        [] => Err(format!("missing {field}")),
        _ => Err(format!("{field} must have exactly one non-empty value")),
    };
    let optional_singleton =
        |field: String| match fields.get(&field).map(Vec::as_slice).unwrap_or(&[]) {
            [] => Ok(None),
            [value] if !value.is_empty() => Ok(Some(value)),
            _ => Err(format!("{field} must have at most one non-empty value")),
        };
    let kind = singleton(format!("{name}-kind"))?;
    let reference = singleton(format!("{name}-ref"))?;
    let valid_reference = |value: &str| {
        value
            .strip_prefix("refs/heads/")
            .is_some_and(|branch| !branch.is_empty())
    };
    if !valid_reference(reference) {
        return Err(format!("{name} ref must be a refs/heads/ branch reference"));
    }
    let remote = optional_singleton(format!("{name}-remote"))?;
    let canonical = match kind.as_str() {
        "local" => {
            if remote.is_some() {
                return Err(format!("local {name} has a remote"));
            }
            CanonicalBranch::Local {
                reference: reference.clone(),
            }
        }
        "remote" => CanonicalBranch::Remote {
            remote: remote
                .ok_or_else(|| format!("missing {name} remote"))?
                .clone(),
            reference: reference.clone(),
        },
        _ => return Err(format!("unknown {name} kind")),
    };
    let local_anchors = fields
        .get(&format!("{name}-anchor"))
        .into_iter()
        .flatten()
        .map(|anchor| {
            if valid_reference(anchor) {
                Ok(anchor.clone())
            } else {
                Err(format!(
                    "{name} anchor must be a refs/heads/ branch reference"
                ))
            }
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    Ok(ReviewSide {
        canonical,
        local_anchors,
    })
}

pub(super) fn decode_v2_fields(
    fields: &BTreeMap<String, Vec<String>>,
) -> Result<ReviewIdentity, String> {
    Ok(ReviewIdentity {
        target: decode_side(fields, "target")?,
        source: decode_side(fields, "source")?,
    })
}
