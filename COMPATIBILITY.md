# Compatibility matrix

Which `soroban-testkit` release works with which `soroban-sdk` and Stellar
protocol version. The workspace pins a single `soroban-sdk` in the root
[`Cargo.toml`](Cargo.toml); a testkit release and a `soroban-sdk` version
travel together, so a bump to either must update this matrix in the same
commit (enforced by the [`docs` workflow](.github/workflows/docs.yml)).

## Current matrix

| `soroban-testkit` / `soroban-testkit-cli` | `soroban-sdk` (pinned) | Stellar protocol | Rust toolchain |
|---|---|---|---|
| 0.1.0 (unreleased) | 27.0.6 | 27 | stable per [`rust-toolchain.toml`](rust-toolchain.toml); `soroban-sdk` 27 requires Rust ≥ 1.91 |

The `soroban-sdk` major version tracks the Stellar network protocol major
version (SDK `27.0.x` ↔ protocol 27). Protocol reference:
[Stellar network protocol history](https://stellar.expert/explorer/public/protocol-history).

## Pinning a compatible version

A testkit release is built against exactly one `soroban-sdk`. Your contract
crate must resolve to that same `soroban-sdk`, or two copies of the SDK end up
in the build and their types (`Env`, `Address`) stop matching. Pin both to the
row in the matrix above that matches the release you use:

```toml
[dev-dependencies]
soroban-testkit = "=0.1.0"   # once 0.1.0 is published
soroban-sdk = { version = "=27.0.6", features = ["testutils"] }
```

The SDK major version is also the Stellar protocol version the release
targets. Before choosing a row, check the protocol version of the network you
deploy to: a contract built against a different protocol may not run there.

Confirm that only one copy of the SDK is in the dependency graph:

```sh
cargo tree -i soroban-sdk
```

### When pinning fails

- `failed to select a version for the requirement soroban-sdk`: two crates in
  your workspace require different SDK versions. Make every crate use the
  version from the matrix, or run `cargo update -p soroban-sdk --precise 27.0.6`.
- `two different versions of crate soroban_sdk are being used`: `cargo tree -i
  soroban-sdk` lists more than one version. Align them as above, then rebuild.
- The pinned row no longer builds on your toolchain: check the Rust version in
  the matrix's last column, and open an issue if the row itself is broken.

## Policy

- Keep this matrix accurate in the same commit that bumps
  `[workspace.dependencies] soroban-sdk` or the workspace version.
- CI enforces that the pinned `soroban-sdk` string appears in this file, so
  a pin can never drift from the documentation.
- The weekly `scheduled-sdk-check` job in
  [`.github/workflows/ci.yml`](.github/workflows/ci.yml) additionally tests
  the workspace against whatever `soroban-sdk` is currently latest on
  crates.io, independent of this pin. When a newer `soroban-sdk` major is a
  clean drop-in, bump the pin and add the new row here; when it is not, the
  check surfaces the breakage as an issue before a contributor hits it.
- `soroban-sdk` upstream supports only its two most recent major releases
  with security fixes; a pinned major that is older is a risk worth
  recording in this matrix.

## Historical rows

A row per released version is kept here, oldest last, so users of an older
release can look up the SDK/protocol it was built for. The first row lands
with the `0.1.0` release.