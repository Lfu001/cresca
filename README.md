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

Renames appear as `R100 old-path -> new-path`, and the summary includes edits made alongside a rename. To inspect those ordinary edit hunks, compare the review commit with the source branch in Git or your editor.

If Cresca cannot determine a single safe base for the review, it stops with an error instead of guessing which approvals to retain. Resolve the branch history and start the review again once a safe base is available.

## License

[MIT](https://github.com/Lfu001/cresca/blob/main/LICENSE)
