# cresca

A tool to partially review the pull requests.

By marking the reviewed changes as commit instead of "viewed" checkbox in GitHub, there is no confusion about which changes are already reviewed and which are not.

## Installation

```sh
cargo install cresca
```

Also You need to have `git` installed.

## Usage

1. Start a review by specifying the branches. Following example will prepare a review branch (typically named `review-main-develop`) for the PR that `develop` is to be merged into `main`.

    ```sh
    cresca review main develop
    ```

2. Review the changes and stage them. You don't have to stage all the changes (e.g. if there are 20 lines of changes in hello.txt, you can stage only 10 lines of it). Stage only the changes you have reviewed. "Stage Selected Ranges" in VSCode is useful for this.

3. Approve the reviewed changes.

    ```sh
    cresca approve
    ```

4. If either branch changes after the PR is reviewed, go back to step 1. Cresca safely retains approvals it can reconstruct and leaves uncertain changes unreviewed.

5. After the PR is merged, you can just delete the review branch.

## Advanced Usage

### Customizing Review Branch Names

By default, Cresca creates review branches such as `review-main-develop`. To choose names with your own script, create `~/.cresca/config.toml`:

```toml
[review_branch.naming_hook]
program = "/Users/me/bin/cresca-review-name"
args = []
```

Cresca runs the configured program directly, without a shell. It appends the exact source and target values from `cresca review <target> <source>` after the configured arguments:

```text
<program> <configured args...> <source> <target>
```

The hook must exit successfully and print exactly one non-empty UTF-8 line containing a valid Git branch name. For example, this executable shell script removes a `feature/` prefix and produces names such as `login-into-main`:

```sh
#!/bin/sh
set -eu

source=${1#feature/}
target=$2
printf '%s-into-%s\n' "$source" "$target"
```

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
param($Source, $Target)
$Source = $Source -replace '^feature/', ''
Write-Output "$Source-into-$Target"
```

When the hook is not configured, the default naming rule remains unchanged. Existing review branches are found by their Cresca metadata and reused without running the hook, even if their names do not start with `review-`.

The hook runs from the repository root and should only compute and print a name. Cresca cannot undo files or other side effects created by user hook code. Hook configuration is user-wide only; Cresca intentionally does not execute hooks from repository-controlled configuration.

### Reviewing a Specific Range of Commits

When dealing with large PRs, you can limit the review scope using `--skip-to` and `--stop-at` options:

```text
merge-base ---- A ---- B ---- C ---- D ---- develop
                       ^             ^
               --skip-to=B     --stop-at=C
```

| Option              | Description                                  |
|---------------------|----------------------------------------------|
| `--skip-to <hash>`  | Auto-approve commits before this hash        |
| `--stop-at <hash>`  | Exclude commits after this hash from review  |

**Examples:**

```sh
# Review only commits B, C (auto-approve A, exclude D)
cresca review main develop --skip-to=B --stop-at=C

# Review from B to develop (auto-approve A)
cresca review main develop --skip-to=B

# Review from merge-base to C (exclude D)
cresca review main develop --stop-at=C
```

Use `git log --oneline main..develop` to see available commits.

### Checking Review Progress

`status` shows the changes that remain unapproved in the range prepared by the most recent successful `review` command:

```sh
cresca status
```

Renames appear as `R<score> old-path -> new-path`; a pure rename is `R100`. The summary includes edits made alongside a rename. To inspect those ordinary edit hunks, compare the review commit with the source branch in Git or your editor.

If Cresca cannot determine a single safe base for the review, it stops with an error instead of guessing which approvals to retain. Resolve the branch history and start the review again once a safe base is available.

## License

[MIT](https://github.com/Lfu001/cresca/blob/main/LICENSE)
