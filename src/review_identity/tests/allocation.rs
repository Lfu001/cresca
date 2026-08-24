use super::*;

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
