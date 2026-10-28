# Advanced Usage

## Branch Reference Resolution

`cresca review <target> <source>` accepts three branch forms for both the target and source:

| Form | Example | Meaning |
|------|---------|---------|
| Plain branch name | `dev` | Use a configured remote upstream, or discover one safe local or same-named remote branch. |
| Explicit local branch | `refs/heads/dev` | Use exactly the local branch without querying remotes. |
| Explicit remote branch | `origin/dev` | Verify and fetch exactly that branch from the selected remote. |

`refs/remotes/origin/dev` is also accepted as an explicit remote spelling. A tracked plain branch uses its fetched upstream tip, so unpushed local commits are excluded. Use `refs/heads/dev` when you intentionally want the local tip.

When a local branch has no valid remote upstream, Cresca checks every configured remote for a same-named branch and applies this table:

| Local branch | Matching remote branches | Result |
|--------------|--------------------------|--------|
| exists | none | Use the local branch. |
| exists | one or more | Stop because local and remote are ambiguous. |
| absent | exactly one | Use that remote branch. |
| absent | two or more | Stop because the remotes are ambiguous. |
| absent | none | Stop because the branch was not found. |

If local `main` tracks `origin/main` and local `dev` tracks `origin/dev`, all four of these commands continue the same review:

```sh
cresca review main dev
cresca review main origin/dev
cresca review origin/main dev
cresca review origin/main origin/dev
```

This continuity also covers the usual local-to-remote workflow. You can review a local-only plain branch, approve changes, publish it with `git push -u`, and run the same plain-name review again. Cresca reuses the review and preserves approvals that still correspond safely; newly added or rewritten changes remain unreviewed.

Plain names fail closed when more than one branch is possible. For example:

```text
error: Branch `dev` is ambiguous. Candidates: `refs/heads/dev`, `origin/dev`.
Select one explicitly (local: `refs/heads/dev`; remote: `origin/dev`).
```

Use the displayed spelling in the corresponding target or source position, such as `cresca review main refs/heads/dev` or `cresca review main origin/dev`.

Review naming hooks receive the raw source and target spellings in standard-input JSON, even though Cresca resolves branch identity independently.

## Reviewing by PR Identifier

Configure a user-owned resolver to start or resume a review by PR number, URL, or another identifier:

```sh
cresca review --pr 123
cresca review --pr https://example.com/project/pull/123
cresca review --pr 123 --skip-to abc1234 --stop-at def5678
```

Supply either `--pr <identifier>` or both positional branches. An empty identifier, an incomplete positional pair, or mixing the two modes is an argument error. A branch literally named `pr` remains valid in positional mode. Identifiers are opaque strings: Cresca does not parse numbers or detect providers. Use `--pr=-identifier` for a value beginning with a hyphen.

Add the resolver to the same user-wide configuration file used by the naming hook:

```toml
[review_branch.pr_hook]
program = "/Users/me/bin/resolve-pr"
args = []
```

Cresca runs the program directly, without a shell, at the repository root. Only the configured arguments are passed on the command line. It writes one UTF-8 JSON object to standard input and closes the input:

```json
{"request":{"pr":"123"}}
```

The resolver must exit successfully and return one UTF-8 JSON object on standard output:

```json
{
  "branches": {
    "target": "origin/main",
    "source": "origin/feature/login"
  },
  "metadata": {
    "ticket": "ABC-456",
    "number": 123
  }
}
```

`branches.target` and `branches.source` are required, nonempty strings using the [supported branch forms](#branch-reference-resolution). `metadata` is an optional object whose contents are defined by the hook; omitting it produces an empty object. Cresca passes it to the naming hook without interpretation. Unknown or duplicate fields in the output envelope or `branches` object are errors. The resolver cannot return `request`. Surrounding JSON whitespace and a trailing newline are accepted; extra output is rejected. Send diagnostics to standard error.

Your resolver chooses how to obtain PR information. Document and install any provider CLI dependencies and authentication it requires separately. It must fail if it cannot reliably determine the target and source, including unsupported PRs or forks. Cresca does not add remotes or provide automatic support for unconfigured forks; Git authentication continues to use your existing Git configuration.

Every PR-mode invocation runs the resolver again, including review resumption. The resolved branches then follow the same validation, range selection, review identity, and approval reconstruction rules as positional input. Different identifiers that resolve to the same identities can resume the same review. An unchanged identifier does not join reviews whose resolved identities differ. Request information and hook metadata are transient and are not stored with review identity or approvals.

PR mode requires a configured resolver. Positional review neither runs it nor requires its program or provider authentication to be available. Both hooks inherit the normal process environment; Cresca adds no dedicated context environment variables, timeout, login flow, or fallback. `args` defaults to an empty list, and `program` must contain a non-whitespace character.

Cresca checks for a clean worktree before running the resolver and checks again before its own review preparation. Hook programs are trusted user code: they can change files or Git state. Cresca does not sandbox them or undo their external side effects. Hook configuration is user-wide only; Cresca does not load it from the repository.

If a hook cannot launch, Cresca reports the program and operating system error. If it terminates unsuccessfully, Cresca reports the program, exit status when available, and standard error. Successful output still must pass validation. Valid output containing unavailable or invalid branch references uses the normal branch-resolution diagnostics. Standard error from a successful hook is displayed only with `--verbose`.

## Customizing Review Branch Names

See the [v0.8.0 migration guide](migrations/v0.8.0.md) for existing naming hooks from v0.5.0 through v0.7.1.

By default, Cresca creates review branches such as `review-main-develop`. To choose names with your own script, create the user-wide configuration file for your platform:

| Platform | Configuration file |
|----------|--------------------|
| Linux and macOS | `$HOME/.cresca/config.toml` |
| Windows | `<user profile>\.cresca\config.toml` (for example, `C:\Users\me\.cresca\config.toml`) |

Configure the program that produces the branch name:

```toml
[review_branch.naming_hook]
program = "/Users/me/bin/cresca-review-name"
args = []
```

Cresca runs the program directly, without a shell, at the repository root:

```text
<program> <configured args...>
```

The hook reads one UTF-8 JSON object from standard input, followed by EOF. Positional review provides the original target and source spellings:

```json
{
  "request": {},
  "branches": {"target": "main", "source": "feature/login"},
  "metadata": {}
}
```

PR mode provides the original identifier, resolver-returned branch spellings, and resolver metadata:

```json
{
  "request": {"pr": "123"},
  "branches": {"target": "origin/main", "source": "origin/feature/login"},
  "metadata": {"ticket": "ABC-456", "number": 123}
}
```

The hook must exit successfully and print exactly one nonempty UTF-8 line containing a valid Git branch name. For example, this executable shell script requires `jq` and removes a `feature/` prefix to produce names such as `login-into-main`:

```sh
#!/bin/sh
set -eu
jq -r '.branches | "\(.source | sub("^feature/"; ""))-into-\(.target)"'
```

To produce names such as `review-pr-123` when the resolver supplies `metadata.number`, a naming hook can instead use `jq -r '"review-pr-\(.metadata.number)"'`. The original `request.pr` may be a URL or another arbitrary string, so use resolver metadata when a name needs an extracted PR number. Every generated name must satisfy Git's branch-name rules.

On Windows, register PowerShell explicitly rather than relying on script-file associations:

```toml
[review_branch.naming_hook]
program = "pwsh"
args = [
  "-NoProfile",
  "-File",
  "C:\\Users\\me\\bin\\cresca-review-name.ps1",
]
```

```powershell
$InputData = [Console]::In.ReadToEnd() | ConvertFrom-Json
$Source = $InputData.branches.source -replace '^feature/', ''
$Target = $InputData.branches.target
Write-Output "$Source-into-$Target"
```

When the hook is not configured, the default naming rule remains unchanged. Existing review branches are found by their Cresca metadata and reused without running the naming hook, even if their names do not start with `review-`. Request information and metadata do not rename an existing review branch.

The naming hook should only compute and print a name. As with the resolver, Cresca cannot undo files or other side effects created by user hook code.

## Reviewing a Specific Range of Commits

When dealing with large PRs, you can limit the review scope using `--skip-to` and `--stop-at` options:

```text
merge-base ---- A ---- B ---- C ---- D ---- develop
                       ^             ^
               --skip-to=B     --stop-at=C
```

| Option | Description |
|--------|-------------|
| `--skip-to <hash>` | Auto-approve commits before this hash |
| `--stop-at <hash>` | Exclude commits after this hash from review |

Examples:

```sh
# Review only commits B, C (auto-approve A, exclude D)
cresca review main develop --skip-to=B --stop-at=C

# Review from B to develop (auto-approve A)
cresca review main develop --skip-to=B

# Review from merge-base to C (exclude D)
cresca review main develop --stop-at=C
```

Use `git log --oneline main..develop` to see available commits.

## Checking Review Progress

`status` shows the changes that remain unapproved in the range prepared by the most recent successful `review` command:

```sh
cresca status
```

Renames appear as `R<score> old-path -> new-path`; a pure rename is `R100`. The summary includes edits made alongside a rename. To inspect those ordinary edit hunks, compare the review commit with the source branch in Git or your editor.

If Cresca cannot determine a single safe base for the review, it stops with an error instead of guessing which approvals to retain. Resolve the branch history and start the review again once a safe base is available.
