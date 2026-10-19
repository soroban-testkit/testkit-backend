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

## Pinning compatible versions

Pick the row in the matrix above that matches the Stellar network you deploy
to, then pin both crates in your own `Cargo.toml` so a `cargo update` cannot
move one without the other:

```toml
[dev-dependencies]
soroban-testkit = "=0.1.0"
soroban-sdk = "=27.0.6"   # keep your existing features, e.g. "testutils"
```

The `=` exact pin matters because `soroban-testkit` is `0.x`: a minor bump
may break your tests (see [`API_STABILITY.md`](API_STABILITY.md)). To upgrade,
change both pins in the same commit and read the matching section of the
[API migration guide](docs/api-migration.md) and the
[SDK migration guide](docs/sdk-migration.md).

To install the CLI at the same version, once it is published:

```sh
cargo install soroban-testkit-cli --version =0.1.0 --locked
```

### Checking the resolved versions

Run `cargo tree -i soroban-sdk` in your project. It must list exactly one
`soroban-sdk` version, and it must be the one in the matrix row you chose.

If it lists two versions, another dependency pins a different `soroban-sdk`
major. Cargo then builds two copies of the SDK, and types from one copy do not
unify with the other. The symptom is a compile error such as
`expected struct ..., found struct ...` that names the same type twice. Fix it
by moving the other dependency to the same SDK major, or by pinning the
`soroban-sdk` version the row lists so Cargo unifies them.

Every release keeps the CLI's pinned `soroban-spec` equal to the workspace
`soroban-sdk` version; the [`docs` workflow](.github/workflows/docs.yml)
fails the PR if they differ.

## Historical rows

A row per released version is kept here, oldest last, so users of an older
release can look up the SDK/protocol it was built for. The first row lands
with the `0.1.0` release.