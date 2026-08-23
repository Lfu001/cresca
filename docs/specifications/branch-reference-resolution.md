# Branch Reference Resolution Specification

## Status

Accepted specification for a future Cresca release. The behavior described here is
not available in v0.5.0.

## Purpose

`cresca review <target> <source>` must work for branches that exist only locally as
well as branches published to remotes. It must also recognize that a local branch and
its configured upstream usually represent the same review.

This specification defines what branch inputs mean, when Cresca continues an existing
review, and when it refuses to guess.

## General rule

Cresca resolves or continues a review automatically only when there is one safe,
unique result.

If multiple branches or review histories could match, a required remote cannot be
checked, or Cresca cannot identify one safe review base, it stops with an error.
Branch ambiguity errors show how to select a local or remote branch explicitly.
Remote errors identify the required remote and operation. Review-history errors list
the conflicting histories and suggest explicit syntax only when it can resolve the
conflict.

Once Cresca has identified one review and one review base, it preserves only approvals
whose corresponding changes can be identified unambiguously. A change without a safe
correspondence becomes unreviewed; an individual unmappable approval does not by
itself make the command fail.

A safe review base is the single result from the equivalent of
`git merge-base --all <resolved-target-tip> <range-endpoint>`. The range endpoint is
the resolved source tip, or the commit selected by `--stop-at` when that option is
present. `--skip-to` changes which earlier changes begin as approved, but does not
change the range endpoint or merge-base calculation. If Git finds no best common
ancestor or more than one, Cresca stops. Once there is exactly one, failures to map
individual approvals only make those changes unreviewed.

## Supported branch inputs

Cresca accepts three forms for both `<target>` and `<source>`.

### Plain branch name

```sh
cresca review main dev
```

A plain name lets Cresca use the local branch, its configured remote upstream, or a
unique same-named remote branch according to the rules below.

### Explicit local branch

```sh
cresca review refs/heads/main refs/heads/dev
```

`refs/heads/<name>` always means the local branch.

- Cresca does not inspect its upstream.
- Cresca does not look for a remote replacement.
- The command fails if the local branch does not exist.

This is also how to select a local branch whose name looks like a remote branch, such
as `refs/heads/origin/dev`.

### Explicit remote branch

```sh
cresca review origin/main origin/dev
cresca review refs/remotes/origin/main refs/remotes/origin/dev
```

Both forms explicitly select a branch from a configured remote.

- Cresca verifies that the branch still exists on that remote.
- Cresca fetches the selected branch before preparing the review.
- A stale remote-tracking branch is not used after the remote branch has been deleted.
- Cresca does not fall back to a local branch.
- Cresca does not need to contact unrelated remotes, but the selected remote must be
  reachable.

Remote names containing `/` are supported. Cresca uses the longest configured remote
name that matches the input.

## Inputs that are not branches

Target and source must be branches. Cresca rejects:

- commit IDs;
- tags that do not also name a branch;
- `HEAD` and other pseudorefs;
- revision expressions such as `HEAD~2`;
- invalid Git ref names.

When a tag and branch have the same short name, Cresca selects the branch. Use
`--skip-to` and `--stop-at` to limit a review to particular commits.

## Resolving a plain branch name

### Branch with a valid remote upstream

If a local branch has a valid remote upstream, Cresca uses that exact upstream.

Local and remote branch names do not need to match. For example, this configuration:

```ini
branch.dev.remote = origin
branch.dev.merge = refs/heads/feature/alice
```

makes plain `dev` refer to `origin/feature/alice`.

Because the upstream is an explicit Git configuration choice, Cresca does not search
other remotes for competing branches in this case.

Plain `dev` reviews the fetched upstream tip, not the local `dev` tip. Unpushed local
commits are therefore excluded, including when the local and upstream histories have
diverged. Use `refs/heads/dev` to review the local tip explicitly.

### Branch without a remote upstream

When there is no valid remote upstream, Cresca checks every configured remote for the
same branch name.

| Local branch | Matching remote branches | Result |
|---|---:|---|
| exists | none | use the local branch |
| exists | one or more | stop: local and remote are ambiguous |
| absent | exactly one | use that remote branch |
| absent | two or more | stop: the remotes are ambiguous |
| absent | none | stop: branch not found |

For this scan, Cresca must obtain a conclusive existence result from every configured
remote. If any result is unknown, it stops and identifies that remote. Users can avoid
checks of unrelated remotes by specifying `refs/heads/<name>` or
`<remote>/<name>` explicitly. Explicit remote syntax still requires the selected
remote to be reachable.

A Git local upstream (`branch.<name>.remote = .`) is not treated as a publishing
relationship. Cresca applies the table above instead.

### Invalid or unavailable configured upstream

Cresca distinguishes a missing upstream from one that is configured but invalid or
unavailable. It stops instead of silently using the local branch when:

- only the upstream remote or merge branch is configured;
- either setting has multiple values;
- the configured remote no longer exists;
- the configured upstream branch was deleted;
- the remote cannot be queried or fetched.

The error distinguishes invalid configuration, a branch confirmed absent on a
reachable remote, and a remote whose state could not be determined. A temporarily
unavailable remote is not reported as a deleted branch.

## Review identity

A review is identified by its resolved target branch and resolved source branch, in
that order. A local identity is the full local ref, such as `refs/heads/dev`. A remote
identity is the configured remote name together with the full branch name on that
remote, such as remote `origin` and branch `refs/heads/dev`. The current commit IDs
and the spelling used on the command line do not define identity.

When local `main` tracks `origin/main` and local `dev` tracks `origin/dev`, these
commands continue the same review:

```sh
cresca review main dev
cresca review main origin/dev
cresca review origin/main dev
cresca review origin/main origin/dev
```

Different configured remote names remain different identities even when their URLs or
current commits are equal.

An explicit local branch is deliberately distinct from its remote branch. For
example, `refs/heads/dev` and `origin/dev` may have separate reviews.

Target and source order matters. Reversing them creates a different review. If target
and source resolve to the same branch, Cresca stops because there is no meaningful
branch comparison to review. Different branch identities may point to the same
commit; in that case the review is valid but currently contains no changes.

## Choosing an existing review

After resolving target and source, Cresca finds reviews that match either:

- the exact resolved target and source; or
- for a plain input only, a uniquely related local branch whose upstream relationship
  has changed.

On every successful plain-name review, Cresca remembers the local branch through
which each endpoint was resolved. A transition candidate exists only when the same
remembered local branch represented the old identity and now resolves to the new
identity. The old-to-new relationship must be unique across all remembered and current
relationships. Commit IDs and reflogs are not evidence of continuity.

The result follows one rule:

| Matching reviews | Result |
|---:|---|
| none | create a new review |
| exactly one | continue that review |
| two or more | stop and list the conflicting review branches |

For a plain input, both exact matches and eligible local/upstream transitions are
candidates. An exact match does not silently override a separate related review. When
plain `dev` could continue both a local and a remote review, Cresca stops.

Explicit local or remote syntax matches only that exact resolved identity for the
corresponding endpoint; it never follows an identity transition. The user can
therefore choose `refs/heads/dev` or `origin/dev` explicitly to resolve an ambiguity.

Cresca never combines approvals from independently created review branches.

## Branch lifecycle

The following rules apply independently and symmetrically to the target and source.
Examples use the source branch `dev` only for brevity.

### Publishing a local branch

The normal local-to-remote workflow continues the existing review:

1. Review a local-only `dev` branch using the plain name, for example
   `cresca review main dev`.
2. Approve some or all of its changes.
3. Run `git push -u origin dev`.
4. Add and push more changes.
5. Run `cresca review main dev` again.

Cresca reuses the previous review branch and retains every approval that can be mapped
safely. Only new or no-longer-corresponding changes return to the unreviewed state.

The same behavior applies when the upstream branch has a different name.

This continuity depends on the earlier plain-name review having remembered the local
branch. A review used only through explicit local or remote syntax remains tied to
that exact identity and does not gain transition history later.

### Changing or removing an upstream

Cresca continues the existing review when one local branch remembered by the previous
successful review uniquely connects the old and new identities. This can cover:

- changing `origin/dev` to `upstream/team-dev`;
- removing the upstream after the former remote branch is no longer a competing
  same-named candidate;
- renaming a remote branch while keeping the same local branch relationship.

If several local branches or review histories provide conflicting relationships,
Cresca stops instead of choosing one.

After an upstream is removed, a plain name follows the no-upstream resolution table.
If the former same-named remote branch still exists, local and remote are ambiguous;
Cresca stops and requires explicit syntax.

### Renaming branches

Cresca does not detect renames. A review can nevertheless continue when one stable,
unique relationship proves continuity:

- a renamed local branch can continue through an unchanged remote upstream;
- a renamed remote branch can continue through an unchanged local branch.

Cresca does not infer a rename from matching commits or reflogs. A renamed local-only
branch, a renamed remote-only branch, or simultaneous local and remote renames with no
stable relationship therefore start a new review. The previous review branch remains
available but is not combined with the new review.

Publishing, upstream-change, and rename continuity through a local branch applies
only when an earlier successful plain-name review remembered that branch. A review
used only with explicit syntax never follows these transitions.

### Deleting and recreating a branch

Reusing the same resolved branch identity—whether after deletion and recreation or a
force-push—makes the previous review a candidate. Cresca then applies the same history
rewrite and approval-mapping rules in both cases.

### Normal pushes and force-pushes

Moving or rewriting a branch does not by itself create a new review. Cresca preserves
approvals that still have a safe correspondence, returns changes without a safe
correspondence to the unreviewed state, and stops only when it cannot identify one
prior review or one safe review base.

The existing `--skip-to` and `--stop-at` behavior remains unchanged.

## Review branch names and naming hooks

Review identity is independent of the local review branch name.

For a new review, Cresca passes the two unnormalized command-line inputs to the
existing v0.5.0 naming behavior. A configured naming hook receives argument 1 = raw
source and argument 2 = raw target, preserving the v0.5.0 interface.

When Cresca continues an existing review or follows a safe branch transition:

- it keeps the existing review branch name;
- it does not run the naming hook again;
- it does not rename the review branch.

Consequently, the first spelling used may affect the review branch's display name,
but later equivalent spellings do not create separate reviews.

## Existing reviews from v0.5.0

Existing review branches remain usable with `approve` and `status` without contacting
remotes.

On a later `review`, Cresca upgrades a related existing review only when its saved
target and source can be resolved uniquely under this specification. If multiple old
reviews become equivalent, a required remote is unavailable, or an old value is
ambiguous, Cresca stops and names the affected review branches. A failed upgrade does
not change their metadata or approvals.

Before contacting remotes for an old review, Cresca applies a name-only filter to both
saved endpoints. An endpoint passes when its saved value:

- exactly equals the corresponding current command-line value;
- is an explicit spelling of the currently resolved identity;
- equals the current request's plain local branch name or its full local ref;
- names a current local branch configured for the currently resolved remote branch;
  or
- is a plain name with the same branch name as the currently resolved endpoint.

Only old reviews for which both endpoints pass this filter are considered. For this
one-time upgrade, a saved plain name may serve as the legacy local relationship when a
single current local branch of that name exists; explicit saved values do not grant
transition permission. A relevant old endpoint is then resolved using the same rules
as a new request; ambiguity or remote failure stops
the command and names that review branch. After both endpoints resolve, the old review
is retained as a candidate only when the resulting pair exactly equals the requested
resolved pair or the saved plain name uniquely connects its current local branch to
the requested identity. A pair that resolves uniquely to different identities is not
a candidate. Invalid old reviews outside the filtered set are ignored. One candidate
is upgraded atomically and keeps its branch name without rerunning the naming hook;
multiple candidates cause an ambiguity error. A successful upgrade preserves
approvals under the same history-mapping rules as any other continued review.

Unrelated stale review branches do not block new reviews.

## `approve` and `status`

`approve` and `status` use the current review branch's saved review range.

- They do not query or fetch remotes.
- They work with existing v0.5.0 reviews and reviews created under this specification.
- They do not discover later local or remote branch movement and do not upgrade review
  identity. Run `cresca review ...` first to refresh the saved range.
- They continue to work when the original target or source branch was deleted,
  provided every commit required by the saved range is still available. If a required
  commit has been pruned, the command fails without changing approvals.
- `status` displays the target and source identities saved by the last successful
  `cresca review`, whether that command created, continued, transitioned, or upgraded
  the review. It does not verify that those branches still exist or reflect later
  upstream changes.
- For an unupgraded v0.5.0 review, `status` displays its saved target and source
  verbatim and identifies them as unresolved legacy values.

## Errors and repository safety

Ambiguity errors include:

- the requested branch name;
- every local or remote candidate found;
- every conflicting review branch, when applicable;
- explicit alternatives such as `refs/heads/dev` and `origin/dev`.

A failed command leaves the current branch, local branches, remote-tracking branches,
Cresca review branches and metadata, local configuration, index, worktree, and
`FETCH_HEAD` unchanged. Remote checks may download otherwise unreachable Git objects;
those objects do not change any named ref or working state. External side effects of
a user-provided naming hook are outside this guarantee.

## Acceptance scenarios

The implementation must demonstrate all of the following:

- local-only target and source branches can be reviewed;
- explicit local and explicit remote syntax never fall back to each other;
- plain local/remote and multi-remote ambiguity is rejected;
- plain tracked names review the upstream tip and exclude unpushed local commits;
- a failure on any required remote is reported, while explicit syntax bypasses
  unrelated remotes;
- absence of an upstream follows the no-upstream table, while incomplete
  configuration, a deleted upstream branch, and an unavailable upstream produce
  distinct errors;
- differently named upstream branches resolve correctly;
- remote names containing `/` and Git local upstreams (`remote = .`) behave as
  specified;
- a branch wins over a same-named tag;
- the four equivalent invocation forms reuse one review branch;
- publishing a branch previously reviewed through its plain local name preserves its
  approved baseline;
- unique upstream changes and removals preserve the review when a prior plain-name
  review remembered the local relationship, while removal with a same-named remote
  still present is rejected as ambiguous;
- conflicting local relationships and review histories are rejected;
- explicit local and remote reviews remain separate;
- target-side and source-side transitions follow the same rules;
- renames without a stable relationship start a new review and do not combine review
  histories;
- deletion and recreation follows the same rewrite rules as a force-push;
- safe normal-push and force-push reconstruction remains unchanged;
- naming hooks retain their v0.5.0 arguments and are not rerun during reuse;
- existing reviews remain usable, upgrade only when uniquely resolvable, and preserve
  approvals when upgraded;
- `approve` and `status` remain offline and use only the saved review range;
- every rejected operation leaves repository state unchanged.
