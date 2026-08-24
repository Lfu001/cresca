use super::*;

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

    let fields = encode_v2_fields(&identity);
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
#[test]
fn malformed_metadata_is_not_loaded_as_a_candidate() {
    let candidates = load_candidates_from_branches(vec!["broken-review".to_string()], |_| {
        Err(super::super::ReviewIdentityReadError::Invalid(
            "missing source-ref".to_string(),
        ))
    })
    .unwrap();

    assert!(candidates.is_empty());
}
