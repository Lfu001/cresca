mod allocation;
mod legacy;
mod metadata;
mod model;
mod selection;

pub use allocation::allocate_new_review_branch;
pub use metadata::{load_review_candidates, read_stored_review_identity, write_review_identity_v2};
pub use model::{
    ReviewIdentity, ReviewIdentityReadError, ReviewRequest, ReviewSelection, ReviewSelectionError,
    ReviewSide, StoredReviewIdentity,
};
pub use selection::select_review;

#[cfg(test)]
use allocation::{allocate_new_review_branch_with, suffixed_review_branch};
#[cfg(test)]
use metadata::{
    decode_stored_fields, decode_v2_fields, load_candidates_from_branches, REVIEW_METADATA_V2,
};
#[cfg(test)]
use model::{LegacyReviewIdentity, StoredReview};

#[cfg(test)]
mod tests;
