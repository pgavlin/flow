# Contributing

Use the Rust toolchain pinned in `rust-toolchain.toml`. Keep changes focused and
include regression tests for behavior changes.

## Releasing

`flow_cache`, `flow_document`, and `flow_publication` share a version and are
published together.

1. Update `workspace.package.version` and both internal dependency versions in
   the root `Cargo.toml`. Keep the local `path` entries.
2. Refresh the workspace and reader lockfiles (`Cargo.lock` and
   `examples/reader/Cargo.lock`). Commit the changes, merge to `main`, and wait
   for CI to pass.
3. From a clean checkout of the release commit, run:

   ```sh
   cargo lint --all
   cargo test --locked --workspace --all-features
   cargo publish --locked --workspace --all-features --dry-run
   ```

4. Create and push a version tag matching the manifests:

   ```sh
   git tag -a v0.1.1 -m 'flow 0.1.1'
   git push origin v0.1.1
   ```

5. Approve the **`crates-io`** environment deployment in GitHub Actions.

The release workflow validates the tag and dependency versions, runs CI, and
obtains a temporary crates.io token through trusted publishing. Cargo publishes
in dependency order: `flow_cache`, `flow_document`, then `flow_publication`.
After publication, the workflow creates a GitHub release with generated notes.
No crates.io token needs to be stored in GitHub secrets.

## Retrying a release

Rerun the failed workflow, or dispatch it against the same tag:

```sh
gh workflow run release.yml --ref v0.1.1
```

Already-published versions are skipped only when their packaged Git metadata
matches the tagged commit and records a clean checkout. Keep release tags
immutable; if the code needs to change, publish a new version.
