# Changelog

All notable user-facing changes to the `soroban-testkit` workspace are
documented here. Both workspace crates — `soroban-testkit` (library) and
`soroban-testkit-cli` (binary) — share one version number and one entry per
release (see [`RELEASING.md`](RELEASING.md)).

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
as interpreted for a `0.x` crate in [`API_STABILITY.md`](API_STABILITY.md).

## How to add an entry

Every PR that changes user-facing behavior adds to the `## [Unreleased]`
section under the matching type. Entries are short, imperative, and aimed
at a contract author, not at the internal implementation:

- **Added** — new public API or CLI feature.
- **Changed** — existing behavior changed (non-breaking).
- **Fixed** — a bug fix that restores documented behavior.
- **Removed** — public API or flags removed (always a breaking change;
  note that under the `API_STABILITY.md` `0.x` policy).

Breaking changes are called out as a `> **Breaking:**` note in the entry.
The [`docs` workflow](.github/workflows/docs.yml) requires the current
workspace version to have either a released section or an `[Unreleased]`
section, so a merge can never land without a place to record the change.

## [Unreleased]

### Added

- Prebuilt `soroban-testkit` CLI archives (with `.sha256` checksums) for
  Linux x86_64, macOS x86_64 and arm64, and Windows x86_64, attached to each
  GitHub release.
- Core test environment: `TestEnv` with deterministic seeding
  (`with_seed`), random address generation, and a `TestkitError` type that
  every assertion helper panics with.
- Ledger clock control: `advance`, `advance_ledgers`, `warp_to`, `now`,
  `sequence`, and `at`, so tests stop hand-rolling the timestamp/sequence
  relationship.
- Adversarial money-math generators (`adversarial_amounts`, `amounts_in`,
  `overflow_edge_triples`, `bps_values`) and value-conservation assertions
  (`Conservation::assert_holds`, `assert_conserved_over`).
- Event capture and assertion API (`EventLog`, chainable filters) whose
  failure messages print the full decoded log.
- One-line Stellar Asset Contract token doubles (`TestToken`) with a
  documented whole-units vs base-units mint split and supply tracking.
- `AuthMatrix` to prove every privileged entry point rejects unauthorized
  callers, reporting a pass/fail grid per caller.
- TTL inspection and expiry simulation (`ttl_of`, `expire`,
  `assert_bumps_ttl`, `assert_survives_expiry`).
- TTL assertion and snapshot helpers (`assert_ttl_at_least`,
  `assert_ttl_delta`, `ttl_snapshot` with `TtlSnapshot::diff`).
- Temporary-storage expiry recipes: `assert_temporary_expires` (the entry
  is deleted and the contract treats it as absent instead of trapping),
  `assert_temporary_expiry_boundary` (pins the last live ledger and the
  first expired one), and `assert_temporary_extension_defers_expiry`
  (an extension actually keeps the entry alive past its original expiry).
- `soroban-testkit` CLI with `coverage`, `limits`, and `audit`
  subcommands (published as `soroban-testkit-cli`).
- Project and release documentation: `API_STABILITY.md`,
  `COMPATIBILITY.md`, `RELEASING.md`, this changelog, and the automated
  `docs` and `release` workflows.
- CI quality gates, each guarded by a regression test: `README.md`
  examples run as an explicit doc-test step, rustdoc warnings are denied
  workflow-wide, the test suite runs with network access removed
  (`.github/scripts/test-no-network.sh`), and the crates.io `repository`
  metadata is checked against the canonical repository URL.
- Ledger checkpoints: `TestEnv::checkpoint` captures the full ledger state as
  a restorable `LedgerCheckpoint` value, and `TestEnv::restore_checkpoint`
  returns the environment to it (unlike `warp_to`, it may move the clock
  backwards).
- Sequence-oriented clock control: `advance_to_sequence` moves the ledger to
  an absolute sequence number, advancing the timestamp in proportion via the
  close interval; `try_advance_to_sequence` is its checked counterpart.
- `TestEnv::seed` exposes the seed a deterministic environment was built
  with, so a test can rebuild an identical environment from it.
- `TestkitError::kind` returns the variant name, and `TestkitError::chain`
  walks the error's source chain, for match-free diagnostics and logging.
- `AuthMatrix::known_actors` exposes the de-duplicated union of every entry
  point's `allowed` addresses, making explicit which callers the matrix
  cross-checks.

### Changed

- `AuthMatrix` failure reports now state the definition of a "known actor",
  and event-capture failure diagnostics render the captured topics and data
  in decoded form so they line up with the raw captured values. The signed
  `i128` amount convention is now documented across the money generators, as
  is the scope of captured events at nested contract-invocation boundaries.
- The `limits` subcommand now mocks authorization on each isolated probe
  and defaults numeric ramp parameters to a non-zero value, making it
  usable against real auth-gated payout contracts out of the box.
- `AuthMatrix` docs now state that repeated callers are de-duplicated and
  that entry-point names may be reused to check one function under several
  setups, each producing its own reported section.
- `soroban_testkit::tokens` docs now name the unit (whole units vs base
  units/stroops) on every method and state that the SAC admin's authorization
  is mocked for `mint`/`mint_raw` only, not for other operations.
- `soroban_testkit::ttl` docs now spell out per-storage-kind behavior against
  the pinned SDK: temporary entries are deleted on expiry, while persistent
  and instance entries are archived and restored on next read.

### Fixed

- Exclude `vendor/` alongside `target/` from recursive `audit` scans while
  honoring explicitly selected scan roots (#427).
- Apply audit rules consistently to raw Rust identifiers, including types,
  bindings, and method names (#434).
- Guard empty-source audit behavior across file, directory, and stdin scans
  in every output format (#435).
- Explain how to recover from a `limits --ramp` name missing from the selected
  function's ABI, including functions with no inputs; guard rejection before
  probes or report writes (#441).
- Validate the `limits` success/failure bracket before binary refinement and
  report unstable probe results with recovery guidance (#442).
- Clamp doubled ramp values to the search cap without integer overflow (#443).
- Rebuild the limits test fixture before use and reject failed rebuilds even
  when an older WASM artifact exists (#449).
- Explain how to recover from a missing or unreadable `--contract` file (#450).
- Keep `audit` findings and baseline paths relative to the workspace, and
  discover rule configuration consistently for directories, files, and stdin
  (#428). Continue scanning readable files after source or traversal errors,
  while returning a non-zero status for incomplete scans (#425). Distinguish
  Rust, TOML, and JSON parse diagnostics from file-access errors and include
  the affected path and available source location (#426). Guard JSON escaping
  of control characters in paths and diagnostic fields with regression
  coverage (#424).
- `TestEnv::at` now enters its scope with a coherent ledger position: the
  sequence number is derived from the scoped timestamp and the environment's
  ledger close interval (saturating at the ends of the range) instead of
  leaving the newer pre-scope sequence in place. The complete pre-scope
  snapshot is still restored on exit, so the newer clock comes back intact
  and nothing the scope touched leaks out.
- `coverage` command now provides a specific, actionable error message when
  `cargo-llvm-cov` is not installed or not found in PATH, distinguishing
  between the tool being missing and other subprocess errors.
- `limits` command termination is now deterministic: search stops at a
  built-in upper bound rather than when a non-deterministic condition is met,
  and the output notes whether the ceiling was discovered or the upper bound
  was reached.
- `limits` diagnostics now include the first failed ramp value, helping users
  understand where their contract hits resource limits.
- `limits` and `coverage` commands now handle paths with spaces correctly in
  baseline, export, and output operations.
- `limits` never mocked authorization: every ramp attempt failed on a
  `require_auth` call before resource limits were ever reached. Each probe
  now uses `mock_all_auths_allowing_non_root_auth()` on its own
  single-use `Env`.
- `limits` numeric defaults were `0`, which contracts validating positive
  amounts rejected outright; the default is now `1`.
- `soroban_testkit::prelude` re-exports `Actor` and `AddressIter` again, and
  `core` imports `TestkitError` where it uses it; the library did not compile
  without these.

## [0.1.0]

First planned release (not yet published). The complete user-facing surface
is tracked in the `## [Unreleased]` section above; on release, that content
moves into a dated `0.1.0` section here per the [`RELEASING.md`](RELEASING.md)
checklist.

[Unreleased]: https://github.com/soroban-testkit/soroban-testkit/compare/main...HEAD
[0.1.0]: https://github.com/soroban-testkit/soroban-testkit/releases
