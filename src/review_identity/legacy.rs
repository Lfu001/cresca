use super::model::{
    LegacyReviewIdentity, ReviewIdentity, ReviewRequest, ReviewSelectionError, ReviewSide,
};
use crate::branch_ref::{
    parse_branch_request, BranchRequest, BranchResolutionError, CanonicalBranch, ResolvedBranch,
};
use crate::git::run_git_command;
use std::collections::BTreeSet;

pub(super) fn configured_remotes_for_legacy_filter(
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

pub(super) fn next_v1_identity<F>(
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
