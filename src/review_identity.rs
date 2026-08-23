use crate::branch_ref::{
    parse_branch_request, BranchRequest, BranchResolutionError, CanonicalBranch, ResolutionMode,
    ResolvedBranch,
};
use crate::git::{
    add_review_config_value, replace_review_config_value, review_config_values, run_git_command,
    run_git_command_machine_output, unset_review_config_values, GitCommandError,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

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

#[derive(Clone, Debug)]
pub struct ReviewRequest {
    pub target: ResolvedBranch,
    pub source: ResolvedBranch,
}

impl ReviewRequest {
    pub fn identity(&self) -> ReviewIdentity {
        fn side(resolved: &ResolvedBranch) -> ReviewSide {
            let local_anchors = if resolved.mode == ResolutionMode::Plain {
                resolved.local_anchor.iter().cloned().collect()
            } else {
                BTreeSet::new()
            };
            ReviewSide {
                canonical: resolved.canonical.clone(),
                local_anchors,
            }
        }

        ReviewIdentity {
            target: side(&self.target),
            source: side(&self.source),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExistingReview {
    pub branch: String,
    pub stored: StoredReviewIdentity,
    pub next_identity: ReviewIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewSelection {
    New { identity: ReviewIdentity },
    Existing(ExistingReview),
}

#[derive(Clone, Debug)]
pub struct StoredReview {
    pub branch: String,
    pub identity: StoredReviewIdentity,
}

#[derive(Debug)]
pub enum ReviewSelectionError {
    Git(GitCommandError),
    Conflict(String),
    RelevantReviewInvalid { branch: String, reason: String },
}

impl fmt::Display for ReviewSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Git(error) => formatter.write_str(&error.description),
            Self::Conflict(message) => formatter.write_str(message),
            Self::RelevantReviewInvalid { branch, reason } => write!(
                formatter,
                "Review branch `{branch}` has invalid relevant metadata: {reason}"
            ),
        }
    }
}

impl From<GitCommandError> for ReviewSelectionError {
    fn from(error: GitCommandError) -> Self {
        Self::Git(error)
    }
}

impl From<BranchResolutionError> for ReviewSelectionError {
    fn from(error: BranchResolutionError) -> Self {
        match error {
            BranchResolutionError::Git(error) => Self::Git(error),
            BranchResolutionError::Message(message) => Self::Conflict(message),
        }
    }
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

fn load_candidates_from_branches<I, F>(
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

fn configured_remotes_for_legacy_filter(
    verbose: bool,
) -> Result<Vec<String>, ReviewSelectionError> {
    let output = run_git_command(
        "list configured remotes for legacy review filtering",
        &["remote"],
        &[],
        verbose,
    )?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|remote| !remote.is_empty())
        .map(str::to_owned)
        .collect())
}

fn legacy_plain_name(
    raw: &str,
    remotes: &[String],
) -> Result<Option<String>, ReviewSelectionError> {
    match parse_branch_request(raw, remotes) {
        Ok(BranchRequest::Plain { name }) => Ok(Some(name)),
        Ok(BranchRequest::ExplicitLocal { .. } | BranchRequest::ExplicitRemote { .. })
        | Err(BranchResolutionError::Message(_)) => Ok(None),
        Err(BranchResolutionError::Git(error)) => Err(error.into()),
    }
}

fn legacy_local_branch_name(raw: &str) -> Option<&str> {
    match raw.strip_prefix("refs/heads/") {
        Some(name) => (!name.is_empty()).then_some(name),
        None => (!raw.is_empty()).then_some(raw),
    }
}

fn legacy_local_branch_exists(name: &str, verbose: bool) -> Result<bool, ReviewSelectionError> {
    Ok(run_git_command(
        &format!("check legacy local branch `{name}`"),
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{name}"),
        ],
        &[1],
        verbose,
    )?
    .status
    .success())
}

fn legacy_config_values(key: &str, verbose: bool) -> Result<Vec<String>, ReviewSelectionError> {
    let output = run_git_command(
        &format!("read legacy branch relationship `{key}`"),
        &["config", "--local", "--get-all", key],
        &[1],
        verbose,
    )?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect())
}

fn legacy_plain_local_connects(
    name: &str,
    requested: &ResolvedBranch,
    verbose: bool,
) -> Result<bool, ReviewSelectionError> {
    if !legacy_local_branch_exists(name, verbose)? {
        return Ok(false);
    }
    let local_ref = format!("refs/heads/{name}");
    match &requested.canonical {
        CanonicalBranch::Local { reference } => Ok(reference == &local_ref),
        CanonicalBranch::Remote { remote, reference } => {
            let remotes = legacy_config_values(&format!("branch.{name}.remote"), verbose)?;
            let merges = legacy_config_values(&format!("branch.{name}.merge"), verbose)?;
            Ok(
                remotes.as_slice() == [remote.as_str()]
                    && merges.as_slice() == [reference.as_str()],
            )
        }
    }
}

fn explicit_spelling_matches(raw: &str, canonical: &CanonicalBranch) -> bool {
    match canonical {
        CanonicalBranch::Local { reference } => raw == reference,
        CanonicalBranch::Remote { remote, reference } => {
            let branch = reference.strip_prefix("refs/heads/").unwrap_or(reference);
            raw == format!("{remote}/{branch}") || raw == format!("refs/remotes/{remote}/{branch}")
        }
    }
}

fn canonical_branch_name(canonical: &CanonicalBranch) -> &str {
    match canonical {
        CanonicalBranch::Local { reference } | CanonicalBranch::Remote { reference, .. } => {
            reference.strip_prefix("refs/heads/").unwrap_or(reference)
        }
    }
}

fn legacy_side_prefilter(
    raw: &str,
    requested: &ResolvedBranch,
    remotes: &[String],
    verbose: bool,
) -> Result<bool, ReviewSelectionError> {
    if raw == requested.requested
        || explicit_spelling_matches(raw, &requested.canonical)
        || requested.local_anchor.as_deref() == Some(raw)
    {
        return Ok(true);
    }
    let plain_name = legacy_plain_name(raw, remotes)?;
    let local_name = legacy_local_branch_name(raw);
    Ok(plain_name
        .as_deref()
        .is_some_and(|name| name == canonical_branch_name(&requested.canonical))
        || match local_name {
            Some(name) => legacy_plain_local_connects(name, requested, verbose)?,
            None => false,
        })
}

fn next_legacy_side(
    raw: &str,
    resolved: &ResolvedBranch,
    requested: &ResolvedBranch,
    remotes: &[String],
    verbose: bool,
) -> Result<Option<ReviewSide>, ReviewSelectionError> {
    let plain_name = legacy_plain_name(raw, remotes)?;
    let anchor_connects = match plain_name.as_deref() {
        Some(name) => legacy_plain_local_connects(name, requested, verbose)?,
        None => false,
    };
    if resolved.canonical != requested.canonical && !anchor_connects {
        return Ok(None);
    }
    let mut local_anchors: BTreeSet<String> = requested.local_anchor.iter().cloned().collect();
    if anchor_connects {
        local_anchors.insert(format!(
            "refs/heads/{}",
            plain_name.expect("a connecting legacy anchor must be plain")
        ));
    }
    Ok(Some(ReviewSide {
        canonical: requested.canonical.clone(),
        local_anchors,
    }))
}

fn next_v1_identity<F>(
    branch: &str,
    legacy: &LegacyReviewIdentity,
    request: &ReviewRequest,
    remotes: &[String],
    mut resolve_legacy: F,
    verbose: bool,
) -> Result<Option<ReviewIdentity>, ReviewSelectionError>
where
    F: FnMut(&str) -> Result<ResolvedBranch, BranchResolutionError>,
{
    let target_relevant = legacy_side_prefilter(&legacy.target, &request.target, remotes, verbose)?;
    let source_relevant = legacy_side_prefilter(&legacy.source, &request.source, remotes, verbose)?;
    if !target_relevant || !source_relevant {
        return Ok(None);
    }
    let resolve = |raw: &str, result: Result<ResolvedBranch, BranchResolutionError>| {
        result.map_err(|error| match error {
            BranchResolutionError::Git(mut error) => {
                error.description = format!(
                    "resolve saved legacy endpoint `{raw}` for review branch `{branch}`: {}",
                    error.description
                );
                ReviewSelectionError::Git(error)
            }
            BranchResolutionError::Message(message) => {
                ReviewSelectionError::RelevantReviewInvalid {
                    branch: branch.to_string(),
                    reason: format!("cannot resolve saved legacy endpoint `{raw}`: {message}"),
                }
            }
        })
    };
    let resolved_target = resolve(&legacy.target, resolve_legacy(&legacy.target));
    let resolved_source = resolve(&legacy.source, resolve_legacy(&legacy.source));
    let resolved_target = resolved_target?;
    let resolved_source = resolved_source?;
    let target = next_legacy_side(
        &legacy.target,
        &resolved_target,
        &request.target,
        remotes,
        verbose,
    )?;
    let source = next_legacy_side(
        &legacy.source,
        &resolved_source,
        &request.source,
        remotes,
        verbose,
    )?;
    match (target, source) {
        (Some(target), Some(source)) => Ok(Some(ReviewIdentity { target, source })),
        _ => Ok(None),
    }
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

fn decode_v2_fields(fields: &BTreeMap<String, Vec<String>>) -> Result<ReviewIdentity, String> {
    Ok(ReviewIdentity {
        target: decode_side(fields, "target")?,
        source: decode_side(fields, "source")?,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        allocate_new_review_branch_with, decode_stored_fields, decode_v2_fields,
        load_candidates_from_branches, load_review_candidates, select_review,
        suffixed_review_branch, write_review_identity_v2, LegacyReviewIdentity, ReviewIdentity,
        ReviewRequest, ReviewSelection, ReviewSide, StoredReview, StoredReviewIdentity,
        REVIEW_METADATA_V2,
    };
    use crate::branch_ref::{
        BranchResolutionError, CanonicalBranch, ResolutionMode, ResolvedBranch,
    };
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::{Mutex, OnceLock};
    use tempfile::TempDir;

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
    ) -> Result<ReviewSelection, super::ReviewSelectionError>
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

    #[test]
    fn malformed_metadata_is_not_loaded_as_a_candidate() {
        let candidates = load_candidates_from_branches(vec!["broken-review".to_string()], |_| {
            Err(super::ReviewIdentityReadError::Invalid(
                "missing source-ref".to_string(),
            ))
        })
        .unwrap();

        assert!(candidates.is_empty());
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
