# Releasing

This is the release checklist for both workspace crates:

- `soroban-testkit` — the library, published to crates.io as
  `soroban-testkit`
- `soroban-testkit-cli` — the binary, published to crates.io as
  `soroban-testkit-cli` (installs as `soroban-testkit`)

Both crates share a single [`version`](Cargo.toml) (`[workspace.package]
version`), so they are version-locked and released **together from one
git tag**, in dependency order: the library first, then the CLI.

Before picking a version number, read the [`API_STABILITY.md`](API_STABILITY.md)
policy. For what changed in a given release, see [`CHANGELOG.md`](CHANGELOG.md);
for the supported `soroban-sdk` / Stellar protocol versions, see
[`COMPATIBILITY.md`](COMPATIBILITY.md).

## Release checklist

Everything below must be true before a release is tagged. The
[`Release` workflow](.github/workflows/release.yml) re-checks the
release-critical items automatically on every `v*` tag and refuses to
publish if any of them fail.

### Pre-release (on `main`)

1. **CI is green.** The full suite in
   [`.github/workflows/ci.yml`](.github/workflows/ci.yml) passes on
   `main`: fmt, clippy, build + test, doc, audit, deny, coverage.
2. **Changelog is up to date.** [`CHANGELOG.md`](CHANGELOG.md) has a
   `## [<version>] - <date>` heading as its first released section, with
   user-facing entries. Merge the current `## [Unreleased]` content into
   it. Add a `[<version>]` comparison link at the bottom if it is not
   already there.
3. **Version is bumped.** In the workspace root `Cargo.toml`,
   `[workspace.package] version` equals `<version>`. The version bump and
   the changelog move land in the **same commit**, so the tag references a
   tree where tag, version, and changelog all agree.
4. **API policy is honored.** If the bump is a breaking change (see
   `API_STABILITY.md` — for a `0.x` crate that means a minor bump), the
   breaking changes are listed in the changelog. Add a `## 0.<minor>`
   section to [`docs/api-migration.md`](docs/api-migration.md) for the new
   minor release, with the steps a downstream test suite needs.
5. **Compatibility matrix is current.** [`COMPATIBILITY.md`](COMPATIBILITY.md)
   documents the `soroban-sdk` and Stellar protocol version this release
   targets and passes CI's docs check. If the pinned `soroban-sdk` moved,
   update the matrix in the same commit.
6. **Commit and push.** Conventional Commits, e.g. `chore(release): v<version>`.

### Tag and publish

7. **Push the tag**

   ```sh
   git tag v<version>
   git push origin v<version>
   ```

8. The `Release` workflow runs the checklist, then publishes both crates to
   crates.io in order (`soroban-testkit` first, then `soroban-testkit-cli`)
   and creates a GitHub release from the changelog section. It also builds
   the `soroban-testkit` CLI for each supported target and attaches the
   archives and their `.sha256` checksums to the release:
   `x86_64-unknown-linux-gnu`, `x86_64-apple-darwin`,
   `aarch64-apple-darwin`, and `x86_64-pc-windows-msvc`. A new target is added
   to the matrix in [`release.yml`](.github/workflows/release.yml) and the
   allowlist in [`build-cli-binaries.sh`](.github/scripts/build-cli-binaries.sh)
   together.

### Post-release

9. **Verify on crates.io** that both `soroban-testkit` and
   `soroban-testkit-cli` list the new version and that docs.rs has built
   each successfully.
10. **If the pinned `soroban-sdk` changed in this release**, make sure the
    weekly `scheduled-sdk-check` job is green against the new pin (it runs
    against whatever is latest on crates.io, independent of the pin).

## Manual publish (fallback)

If the automated workflow cannot be used (no `CRATES_IO_TOKEN` configured,
no runner access, temporary GitHub outage), publish manually — the order
matters only because the CLI may eventually depend on the library:

```sh
# Verify first, before touching the registry.
cargo publish --dry-run -p soroban-testkit
cargo publish --dry-run -p soroban-testkit-cli

# Then publish for real, library first.
cargo publish -p soroban-testkit
cargo publish -p soroban-testkit-cli

# Finally, create the GitHub release with the notes from CHANGELOG.md.
gh release create v<version> --title "v<version>" --notes-file <(sed -n '/^## \[<version>\]/,/^## /p' CHANGELOG.md)
```

## What the automated checks enforce

The `Release` workflow enforces these release-critical invariants:

- The git tag (`v<version>`) matches `[workspace.package] version` and both published crate versions (`crates/testkit` and `crates/cli`), verified by `.github/scripts/verify-release.sh`.
- `CHANGELOG.md` has a section for `<version>` (so a release can never ship
  without release notes).
- `COMPATIBILITY.md` documents the pinned `soroban-sdk` version.

The [`docs` workflow](.github/workflows/docs.yml) enforces the same
invariants on every PR, plus the presence of `API_STABILITY.md`,
`COMPATIBILITY.md`, and `RELEASING.md`, so a release can never reach the
tag step missing a required document.