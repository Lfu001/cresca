# cresca

A tool to partially review the pull requests.

By marking the reviewed changes as commit instead of "viewed" checkbox in GitHub, there is no confusion about which changes are already reviewed and which are not.

## Installation

```sh
cargo install cresca
```

Also You need to have `git` installed.

## Getting Started

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

## Documentation

See [Advanced Usage](docs/advanced-usage.md) for review range options, review progress, and custom review branch naming.

## License

[MIT](https://github.com/Lfu001/cresca/blob/main/LICENSE)
