use crate::branch_ref::CanonicalBranch;
use crate::git::{
    add_review_config_value, replace_review_config_value, review_config_values,
    unset_review_config_values, GitCommandError,
};
use std::collections::{BTreeMap, BTreeSet};

pub const REVIEW_METADATA_V2: &str = "2";

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

const V2_FIELDS: [&str; 9] = [
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

fn decode_stored_fields(
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

fn encode_side(fields: &mut BTreeMap<String, Vec<String>>, name: &str, side: &ReviewSide) {
    match &side.canonical {
        CanonicalBranch::Local { reference } => {
            fields.insert(format!("{name}-kind"), vec!["local".to_string()]);
            fields.insert(format!("{name}-ref"), vec![reference.clone()]);
        }
        CanonicalBranch::Remote { remote, reference } => {
            fields.insert(format!("{name}-kind"), vec!["remote".to_string()]);
            fields.insert(format!("{name}-ref"), vec![reference.clone()]);
            fields.insert(format!("{name}-remote"), vec![remote.clone()]);
        }
    }
    fields.insert(
        format!("{name}-anchor"),
        side.local_anchors.iter().cloned().collect(),
    );
}

fn encode_v2_fields(identity: &ReviewIdentity) -> BTreeMap<String, Vec<String>> {
    let mut fields = BTreeMap::new();
    fields.insert("version".to_string(), vec![REVIEW_METADATA_V2.to_string()]);
    encode_side(&mut fields, "target", &identity.target);
    encode_side(&mut fields, "source", &identity.source);
    fields
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

fn decode_v2_fields(fields: &BTreeMap<String, Vec<String>>) -> Result<ReviewIdentity, String> {
    Ok(ReviewIdentity {
        target: decode_side(fields, "target")?,
        source: decode_side(fields, "source")?,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        decode_stored_fields, decode_v2_fields, encode_v2_fields, LegacyReviewIdentity,
        ReviewIdentity, ReviewSide, StoredReviewIdentity, REVIEW_METADATA_V2,
    };
    use crate::branch_ref::CanonicalBranch;
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn version_two_fields_round_trip_remote_identity_and_anchors() {
        let identity = ReviewIdentity {
            target: ReviewSide {
                canonical: CanonicalBranch::Remote {
                    remote: "origin".to_string(),
                    reference: "refs/heads/main".to_string(),
                },
                local_anchors: BTreeSet::from(["refs/heads/main".to_string()]),
            },
            source: ReviewSide {
                canonical: CanonicalBranch::Remote {
                    remote: "upstream".to_string(),
                    reference: "refs/heads/feature/alice".to_string(),
                },
                local_anchors: BTreeSet::from(["refs/heads/dev".to_string()]),
            },
        };

        let fields: BTreeMap<String, Vec<String>> = encode_v2_fields(&identity);
        assert_eq!(decode_v2_fields(&fields).unwrap(), identity);
    }

    #[test]
    fn decodes_version_one_raw_target_and_source() {
        let fields = BTreeMap::from([
            ("version".to_string(), vec!["1".to_string()]),
            ("target".to_string(), vec!["origin/main".to_string()]),
            ("source".to_string(), vec!["feature/alice".to_string()]),
        ]);

        assert_eq!(
            decode_stored_fields(&fields).unwrap(),
            StoredReviewIdentity::V1(LegacyReviewIdentity {
                target: "origin/main".to_string(),
                source: "feature/alice".to_string(),
            })
        );
    }

    #[test]
    fn rejects_remote_side_without_remote_name() {
        let fields = BTreeMap::from([
            ("version".to_string(), vec![REVIEW_METADATA_V2.to_string()]),
            ("target-kind".to_string(), vec!["remote".to_string()]),
            (
                "target-ref".to_string(),
                vec!["refs/heads/main".to_string()],
            ),
            ("source-kind".to_string(), vec!["local".to_string()]),
            ("source-ref".to_string(), vec!["refs/heads/dev".to_string()]),
        ]);

        assert!(decode_v2_fields(&fields).is_err());
    }

    #[test]
    fn rejects_local_side_with_remote_name() {
        let fields = BTreeMap::from([
            ("target-kind".to_string(), vec!["local".to_string()]),
            (
                "target-ref".to_string(),
                vec!["refs/heads/main".to_string()],
            ),
            ("target-remote".to_string(), vec!["origin".to_string()]),
            ("source-kind".to_string(), vec!["local".to_string()]),
            ("source-ref".to_string(), vec!["refs/heads/dev".to_string()]),
        ]);

        assert!(decode_v2_fields(&fields).is_err());
    }

    #[test]
    fn rejects_duplicate_singleton_field() {
        let fields = BTreeMap::from([
            (
                "target-kind".to_string(),
                vec!["local".to_string(), "local".to_string()],
            ),
            (
                "target-ref".to_string(),
                vec!["refs/heads/main".to_string()],
            ),
            ("source-kind".to_string(), vec!["local".to_string()]),
            ("source-ref".to_string(), vec!["refs/heads/dev".to_string()]),
        ]);

        assert!(decode_v2_fields(&fields).is_err());
    }

    #[test]
    fn sorts_and_deduplicates_local_anchors() {
        let fields = BTreeMap::from([
            ("target-kind".to_string(), vec!["local".to_string()]),
            (
                "target-ref".to_string(),
                vec!["refs/heads/main".to_string()],
            ),
            (
                "target-anchor".to_string(),
                vec![
                    "refs/heads/zeta".to_string(),
                    "refs/heads/alpha".to_string(),
                    "refs/heads/zeta".to_string(),
                ],
            ),
            ("source-kind".to_string(), vec!["local".to_string()]),
            ("source-ref".to_string(), vec!["refs/heads/dev".to_string()]),
        ]);

        let identity = decode_v2_fields(&fields).unwrap();
        assert_eq!(
            identity
                .target
                .local_anchors
                .into_iter()
                .collect::<Vec<_>>(),
            vec!["refs/heads/alpha", "refs/heads/zeta"]
        );
    }

    #[test]
    fn rejects_non_branch_reference_namespaces() {
        let fields = BTreeMap::from([
            ("target-kind".to_string(), vec!["local".to_string()]),
            ("target-ref".to_string(), vec!["refs/tags/v1".to_string()]),
            ("source-kind".to_string(), vec!["local".to_string()]),
            ("source-ref".to_string(), vec!["refs/heads/dev".to_string()]),
        ]);

        assert!(decode_v2_fields(&fields).is_err());
    }

    #[test]
    fn rejects_version_two_raw_cli_fields() {
        let fields = BTreeMap::from([
            ("version".to_string(), vec![REVIEW_METADATA_V2.to_string()]),
            ("target".to_string(), vec!["origin/main".to_string()]),
            ("target-kind".to_string(), vec!["local".to_string()]),
            (
                "target-ref".to_string(),
                vec!["refs/heads/main".to_string()],
            ),
            ("source-kind".to_string(), vec!["local".to_string()]),
            ("source-ref".to_string(), vec!["refs/heads/dev".to_string()]),
        ]);

        assert!(decode_stored_fields(&fields).is_err());
    }
}
