# soroban-testkit — build specification

**Testing infrastructure for Soroban contracts.** Property-test generators for money math, an event-assertion API, ledger time and TTL-expiry control, token test doubles, and a coverage-aware CLI — so that contract authors stop hand-rolling the same test scaffolding in every repo.

> **This document is a build specification, not a README.** It is written to be handed to a coding agent module by module. Do not build the whole thing in one pass. Build order is given in [§4](#4-build-order) and is not optional — later modules depend on earlier ones existing and being tested.
>
> **The single most important instruction in this document:** every module below lists its exact file path, its public API signatures, and its acceptance criteria. Implement *only* what the module specifies. Do not invent additional abstractions, do not add a trait layer "for extensibility", do not create a `utils.rs` catch-all. If something seems missing, it is either in a later module or deliberately out of scope — check [§3](#3-scope-boundaries) before adding it.

---

## Contents

1. [Problem and positioning](#1-problem-and-positioning)
2. [Repository layout](#2-repository-layout)
3. [Scope boundaries](#3-scope-boundaries)
4. [Build order](#4-build-order)
5. [Module 0 — repo skeleton](#module-0--repo-skeleton)
6. [Module 1 — `core`](#module-1--core)
7. [Module 2 — `ledger`](#module-2--ledger)
8. [Module 3 — `money`](#module-3--money)
9. [Module 4 — `events`](#module-4--events)
10. [Module 5 — `tokens`](#module-5--tokens)
11. [Module 6 — `auth`](#module-6--auth)
12. [Module 7 — `ttl`](#module-7--ttl)
13. [Module 8 — `prelude` and crate assembly](#module-8--prelude-and-crate-assembly)
14. [Module 9 — CLI](#module-9--cli)
15. [Module 10 — validation against a real contract suite](#module-10--validation-against-a-real-contract-suite)
16. [Conventions](#11-conventions)
17. [Verify before you build](#12-verify-before-you-build)
18. [Issue seeding for Drips Wave](#13-issue-seeding-for-drips-wave)

---

## 1. Problem and positioning

`soroban-sdk` ships `testutils`, which is adequate for basic unit tests. It is not adequate for testing contracts that hold money. Every serious Soroban contract repo independently rebuilds some subset of:

- Advancing ledger time correctly, including the interaction between `timestamp` and `sequence`
- Asserting on emitted events without hand-decoding `Val` topics
- Generating adversarial `i128` values that actually find overflow bugs
- Deploying and funding a Stellar Asset Contract token double for transfer tests
- Proving that every privileged entry point rejects unauthorized callers — the most common Soroban vulnerability class
- Simulating TTL expiry, which is nearly impossible to test by hand and silently breaks contracts in production

`soroban-testkit` is that shared layer. It is a dev-dependency crate plus a small CLI.

**Positioning statement for the README:** "The testing crate you would have written yourself, on the third contract."

**Prior art to check, not duplicate:** `soroban-fork` does lazy mainnet/testnet forking for tests. That is a genuinely different problem and this crate should not attempt it. If a user needs forking, the docs should point at `soroban-fork`.

---

## 2. Repository layout

Single repository, single published crate, with the CLI as a second binary crate in the same workspace.

```
soroban-testkit/
├── Cargo.toml                     # workspace root
├── rust-toolchain.toml
├── Makefile
├── README.md
├── LICENSE                        # Apache-2.0
├── CONTRIBUTING.md
├── SECURITY.md
├── FUNDING.json
├── .editorconfig
├── .github/
│   ├── workflows/ci.yml
│   ├── ISSUE_TEMPLATE/
│   └── pull_request_template.md
├── crates/
│   ├── testkit/                   # the library — published as soroban-testkit
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── prelude.rs
│   │       ├── core/
│   │       │   ├── mod.rs
│   │       │   ├── env.rs
│   │       │   └── error.rs
│   │       ├── ledger/
│   │       │   ├── mod.rs
│   │       │   └── clock.rs
│   │       ├── money/
│   │       │   ├── mod.rs
│   │       │   ├── generators.rs
│   │       │   └── invariants.rs
│   │       ├── events/
│   │       │   ├── mod.rs
│   │       │   ├── capture.rs
│   │       │   └── assertions.rs
│   │       ├── tokens/
│   │       │   ├── mod.rs
│   │       │   └── sac.rs
│   │       ├── auth/
│   │       │   ├── mod.rs
│   │       │   └── matrix.rs
│   │       └── ttl/
│   │           ├── mod.rs
│   │           └── expiry.rs
│   └── cli/                       # published as soroban-testkit-cli
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs
│           └── commands/
│               ├── mod.rs
│               ├── coverage.rs
│               ├── limits.rs
│               └── audit.rs
├── examples/
│   └── vault/                     # a deliberately buggy contract used in docs + tests
└── tests/
    └── integration/
```

**Why one repo and not five:** unlike SoroRail, every module here is consumed through a single `use soroban_testkit::prelude::*`. Splitting them across repos would force version-lockstep between crates that are always used together. The CLI is separate only because it should not be a dependency of anyone's test build.

---

## 3. Scope boundaries

**In scope:** anything that helps you test a Soroban contract in-process with `soroban-sdk`'s test environment.

**Explicitly out of scope.** Decline issues and PRs proposing these, and say why:

- Mainnet/testnet forking → point at `soroban-fork`
- Deployment tooling, migration scripts, upgrade helpers
- Frontend or JS/TS testing of any kind
- A test *runner* — this works with `cargo test` and `cargo nextest`, it does not replace them
- Benchmarking framework — resource measurement is in scope via the CLI, a full criterion-style harness is not
- Anything that requires a network connection at test time

The crate must have **zero non-dev network dependencies** and must compile for the WASM target where applicable.

---

## 4. Build order

Strict. Do not start a module until the previous one compiles, is tested, and CI is green.

| # | Module | Depends on | Rough size |
|---|---|---|---|
| 0 | repo skeleton | — | small |
| 1 | `core` | 0 | small |
| 2 | `ledger` | 1 | small |
| 3 | `money` | 1 | medium |
| 4 | `events` | 1 | medium |
| 5 | `tokens` | 1, 2 | medium |
| 6 | `auth` | 1, 4 | medium |
| 7 | `ttl` | 1, 2 | large — hardest module |
| 8 | `prelude` + assembly | 1–7 | small |
| 9 | CLI | 8 | large |
| 10 | validation | 8 | medium |

**Prompting instruction:** open one agent session per module. Paste that module's section, plus [§11 Conventions](#11-conventions) and [§12 Verify before you build](#12-verify-before-you-build). Do not paste the whole document — that is what produces shallow work across everything instead of one finished module.

---

## Module 0 — repo skeleton

**Goal:** a workspace that builds and has green CI with zero functionality.

**Files to create:** everything in [§2](#2-repository-layout) except the contents of `crates/testkit/src/` (create `lib.rs` with only module declarations, commented out) and `crates/cli/src/` (create `main.rs` printing nothing but version).

**`Cargo.toml` (workspace root):**

```toml
[workspace]
resolver = "2"
members = ["crates/*", "examples/vault"]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "Apache-2.0"
repository = "<ACTUAL REPO URL — verify this, do not guess>"

[workspace.dependencies]
soroban-sdk = "<VERIFY — see §12>"
```

**Acceptance criteria:**
- `cargo build --workspace` succeeds
- `cargo fmt --check` and `cargo clippy -- -D warnings` pass
- CI workflow runs fmt, clippy, build, test on push and PR
- `FUNDING.json` present on default branch

**Note:** set `repository` correctly the first time. Getting this wrong ships broken links to crates.io.

---

## Module 1 — `core`

**Files:** `src/core/mod.rs`, `src/core/env.rs`, `src/core/error.rs`

**Purpose:** the entry point every other module builds on. A wrapper around `soroban_sdk::Env` that carries testkit state (captured events, clock position, registered tokens) alongside the raw env.

**Public API:**

```rust
pub struct TestEnv { /* private */ }

impl TestEnv {
    /// Fresh environment with a deterministic starting ledger.
    pub fn new() -> Self;

    /// Deterministic RNG seed for reproducible property tests.
    pub fn with_seed(seed: u64) -> Self;

    /// Escape hatch to the underlying SDK env.
    pub fn env(&self) -> &soroban_sdk::Env;

    /// Generate a fresh random address.
    pub fn address(&self) -> soroban_sdk::Address;

    /// Generate `n` fresh addresses.
    pub fn addresses(&self, n: usize) -> Vec<soroban_sdk::Address>;
}

impl Default for TestEnv { /* calls new() */ }
```

**`error.rs`:** a `TestkitError` enum with `thiserror`. Variants for assertion failures, decode failures, and misuse (e.g. asserting on events before capture was enabled). Every assertion helper in later modules returns or panics with one of these, never a bare `panic!("...")`.

**Design constraint:** `TestEnv` must be cheap to construct — tests create hundreds of them. No I/O, no allocation beyond what the SDK env requires.

**Acceptance criteria:**
- `TestEnv::new()` twice in the same test produces independent environments
- `with_seed(42)` produces identical address sequences across runs — assert this in a test
- Doc comment on every public item, with a runnable `# Example` block

---

## Module 2 — `ledger`

**Files:** `src/ledger/mod.rs`, `src/ledger/clock.rs`

**Purpose:** control ledger time without the caller reasoning about the `timestamp`/`sequence` relationship.

**Public API:**

```rust
impl TestEnv {
    /// Advance by wall-clock duration. Also advances sequence proportionally.
    pub fn advance(&self, duration: Duration);

    /// Advance by exactly n ledgers (~5s each — VERIFY the constant).
    pub fn advance_ledgers(&self, n: u32);

    /// Jump to an absolute unix timestamp. Panics if in the past.
    pub fn warp_to(&self, timestamp: u64);

    pub fn now(&self) -> u64;
    pub fn sequence(&self) -> u32;

    /// Run a closure at a given time, then restore the clock.
    pub fn at<T>(&self, timestamp: u64, f: impl FnOnce() -> T) -> T;
}
```

**Why `at()` matters:** testing a vesting schedule means evaluating `vested_amount` at ten points in time. Without this, every test manually saves and restores the clock, and half of them get it wrong.

**Acceptance criteria:**
- `advance(Duration::from_secs(60))` moves timestamp by exactly 60
- Sequence advances consistently with the documented ledger interval
- `warp_to` a past timestamp panics with a clear `TestkitError`
- `at()` restores the clock even if `f` panics — test with `catch_unwind`

---

## Module 3 — `money`

**Files:** `src/money/mod.rs`, `src/money/generators.rs`, `src/money/invariants.rs`

**Purpose:** find the arithmetic bugs that hand-written tests miss.

**`generators.rs` — adversarial value generation:**

```rust
/// Values chosen to break money math: 0, 1, -1, i128::MAX, i128::MIN,
/// i128::MAX - 1, stroop boundaries (10^7), and values near common
/// overflow thresholds for multiply-then-divide patterns.
pub fn adversarial_amounts() -> Vec<i128>;

/// Random amounts in a plausible range, seeded from TestEnv.
pub fn amounts_in(env: &TestEnv, min: i128, max: i128, n: usize) -> Vec<i128>;

/// Amount/duration/rate triples where rate * duration is near overflow.
pub fn overflow_edge_triples() -> Vec<(i128, u64, i128)>;

/// Basis-point values including 0, 1, 9999, 10000, and out-of-range.
pub fn bps_values() -> Vec<u32>;
```

**`invariants.rs` — conservation checking.** This is the highest-value piece in the crate. Contracts that stream or vest must never leak or mint value through rounding.

```rust
pub struct Conservation {
    pub deposited: i128,
    pub withdrawn: i128,
    pub refunded: i128,
    pub remaining: i128,
}

impl Conservation {
    /// Panics with a detailed diff if deposited != withdrawn + refunded + remaining.
    pub fn assert_holds(&self);

    /// Allows a documented tolerance. Use only where rounding is
    /// specified behavior, and state why at the call site.
    pub fn assert_within(&self, tolerance: i128);
}

/// Drive a full lifecycle across n random withdrawal points and assert
/// conservation at every step.
pub fn assert_conserved_over<F>(env: &TestEnv, steps: usize, f: F)
where F: Fn(&TestEnv, usize) -> Conservation;
```

**Acceptance criteria:**
- `adversarial_amounts()` includes every listed boundary — assert length and contents
- `assert_holds` failure message shows all four values and the delta, not just "assertion failed"
- A test that deliberately constructs a leaking `Conservation` and confirms it panics
- Documented in the README with a before/after example

---

## Module 4 — `events`

**Files:** `src/events/mod.rs`, `src/events/capture.rs`, `src/events/assertions.rs`

**Purpose:** assert on contract events without hand-decoding `Val`.

**Public API:**

```rust
pub struct CapturedEvent {
    pub contract: Address,
    pub topics: Vec<Val>,
    pub data: Val,
}

pub struct EventLog { /* private */ }

impl TestEnv {
    /// All events emitted so far.
    pub fn events(&self) -> EventLog;

    /// Events emitted during the closure only.
    pub fn events_during<T>(&self, f: impl FnOnce() -> T) -> (T, EventLog);
}

impl EventLog {
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;

    pub fn from(&self, contract: &Address) -> EventLog;
    pub fn with_topic<T: IntoVal<Env, Val>>(&self, topic: T) -> EventLog;

    /// Panics with the full log if no matching event exists.
    pub fn assert_emitted<T: IntoVal<Env, Val>>(&self, topic: T);
    pub fn assert_emitted_times<T: IntoVal<Env, Val>>(&self, topic: T, n: usize);
    pub fn assert_none<T: IntoVal<Env, Val>>(&self, topic: T);

    /// Decode the data payload of the single matching event.
    pub fn decode_one<D: TryFromVal<Env, Val>>(&self) -> D;
}
```

**Design constraint:** filters are chainable and return new `EventLog`s. `env.events().from(&c).with_topic(symbol_short!("transfer")).assert_emitted_times(..., 3)` must read naturally.

**Failure messages are the product here.** When `assert_emitted` fails it must print every captured event with decoded topics, because the usual cause is a typo'd topic symbol and the user needs to see what *was* emitted.

**Acceptance criteria:**
- Chained filters compose correctly — test with three contracts emitting overlapping topics
- `events_during` excludes events from before the closure
- Failure output includes the full decoded log — snapshot-test the message

---

## Module 5 — `tokens`

**Files:** `src/tokens/mod.rs`, `src/tokens/sac.rs`

**Purpose:** one-line token setup. Currently every repo writes 30 lines of SAC boilerplate per test file.

**Public API:**

```rust
pub struct TestToken { /* private */ }

impl TestEnv {
    /// Deploy a Stellar Asset Contract with 7 decimals.
    pub fn token(&self) -> TestToken;

    /// Deploy with explicit decimals.
    pub fn token_with_decimals(&self, decimals: u32) -> TestToken;
}

impl TestToken {
    pub fn address(&self) -> Address;
    pub fn decimals(&self) -> u32;

    /// Mint to an address. Amount is in whole units, converted internally.
    pub fn mint(&self, to: &Address, amount: i128);

    /// Mint in raw stroops/base units.
    pub fn mint_raw(&self, to: &Address, amount: i128);

    pub fn balance(&self, of: &Address) -> i128;
    pub fn assert_balance(&self, of: &Address, expected: i128);

    /// Total across all addresses the testkit has minted to —
    /// for supply-conservation assertions.
    pub fn total_tracked(&self) -> i128;
}
```

**The decimals trap must be handled explicitly.** `mint` takes whole units, `mint_raw` takes base units, and both are documented with the conversion spelled out. Do not provide a single ambiguous `mint`.

**Acceptance criteria:**
- `token()` deploys and is immediately usable for transfers
- `mint(addr, 100)` with 7 decimals results in `balance_raw == 1_000_000_000`
- A test asserting `mint` and `mint_raw` disagree by exactly `10^decimals`
- Works with a contract that calls `transfer`, `burn`, and `approve`

---

## Module 6 — `auth`

**Files:** `src/auth/mod.rs`, `src/auth/matrix.rs`

**Purpose:** systematically prove that privileged entry points reject unauthorized callers. Missing auth checks are the single most common Soroban vulnerability class, and testing them by hand is tedious enough that people skip it.

**Public API:**

```rust
/// Declarative table of who may call what.
pub struct AuthMatrix<'a> { /* private */ }

impl<'a> AuthMatrix<'a> {
    pub fn new(env: &'a TestEnv) -> Self;

    /// Register an entry point, the addresses permitted to call it,
    /// and a closure that invokes it as a given caller.
    pub fn entry_point(
        self,
        name: &str,
        allowed: &[Address],
        invoke: impl Fn(&Address) -> Result<(), soroban_sdk::Error> + 'a,
    ) -> Self;

    /// For every registered entry point, assert every allowed address
    /// succeeds and every other known address fails.
    pub fn assert_enforced(self);
}

impl TestEnv {
    /// Assert a single call fails when auth is absent.
    pub fn assert_requires_auth(&self, f: impl FnOnce());
}
```

**Why a matrix rather than individual assertions:** a contract with six entry points and four actors is 24 cases. Written by hand, people test four of them. The matrix makes the full grid the default and the omission visible.

**Acceptance criteria:**
- Against the `examples/vault` contract with a deliberately missing auth check, `assert_enforced` fails and names the specific entry point and caller
- Against the fixed version, it passes
- Failure message lists the full grid with pass/fail per cell

---

## Module 7 — `ttl`

**Files:** `src/ttl/mod.rs`, `src/ttl/expiry.rs`

**This is the hardest module. Budget accordingly and do not start it before modules 1–6 are done.**

**Purpose:** Soroban storage entries expire. Contracts that fail to bump TTL break in production in ways that are effectively untestable today, and the failure mode is silent data loss.

**Public API:**

```rust
pub enum StorageKind { Temporary, Persistent, Instance }

impl TestEnv {
    /// Current TTL of an entry, in ledgers.
    pub fn ttl_of<K: IntoVal<Env, Val>>(&self, contract: &Address, kind: StorageKind, key: K) -> u32;

    /// Advance far enough that entries of this kind expire.
    pub fn expire(&self, kind: StorageKind);

    /// Assert the contract bumped TTL during the closure.
    pub fn assert_bumps_ttl<K: IntoVal<Env, Val>>(
        &self, contract: &Address, kind: StorageKind, key: K, f: impl FnOnce(),
    );

    /// Assert the contract behaves correctly when an entry has expired —
    /// takes the expected error.
    pub fn assert_survives_expiry(
        &self, kind: StorageKind, f: impl FnOnce(),
    );
}
```

**Implementation warning:** TTL semantics differ by storage kind, and the ledger-entry lifetime constants are network parameters that have changed across protocol versions. Do not hardcode them from this document or from memory. Read them from the test env's ledger info at runtime, and if the SDK does not expose them, make that a documented limitation rather than guessing a number.

**Acceptance criteria:**
- `ttl_of` returns a value that decreases as ledgers advance
- `assert_bumps_ttl` fails on a contract that reads without bumping
- Behavior documented per `StorageKind`, with the differences stated explicitly
- If any part cannot be implemented against the current SDK, ship the rest and open an issue describing the gap — do not fake it

---

## Module 8 — `prelude` and crate assembly

**Files:** `src/prelude.rs`, `src/lib.rs`

```rust
// prelude.rs
pub use crate::core::{TestEnv, TestkitError};
pub use crate::events::{CapturedEvent, EventLog};
pub use crate::money::{adversarial_amounts, amounts_in, bps_values, Conservation};
pub use crate::auth::AuthMatrix;
pub use crate::tokens::TestToken;
pub use crate::ttl::StorageKind;
```

**`lib.rs` requirements:**
- `#![doc = include_str!("../../../README.md")]` so doc tests validate README examples
- `#![deny(missing_docs)]`
- Crate-level docs with a complete worked example

**Acceptance criteria:**
- `use soroban_testkit::prelude::*;` gives access to every primary type
- `cargo test --doc` passes — every README example compiles and runs
- `cargo doc` produces no warnings

---

## Module 9 — CLI

**Crate:** `crates/cli`, binary name `soroban-testkit`.

Use `clap` with derive. Three subcommands, built in this order.

### `soroban-testkit coverage`

Wraps coverage collection for the WASM target, which does not work out of the box.

```
soroban-testkit coverage [--format text|lcov|html|json] [--fail-under <pct>] [--open] [--exclude-generated] [--ignore-filename-regex <regex> ...] [--no-clean]
```

Shells out to the underlying coverage tool. Its job is knowing the right flags for Soroban's target, not reimplementing coverage.
JSON mode writes machine-readable cargo-llvm-cov output to `coverage.json` by default, or to `--output <path>`. When only `--output-dir` is provided, it writes `<dir>/coverage.json`. `--open` is HTML-only.

`--exclude-generated` leaves out files under `generated/` or `fixtures/` directories and files named `*_generated.rs`. `--ignore-filename-regex` leaves out any other source paths matching a regex. Both become a single `--ignore-filename-regex` argument to cargo-llvm-cov. `--no-clean` passes `--no-clean` so profile data from earlier runs is kept for incremental collection.

When cargo-llvm-cov exits non-zero, nothing is deleted. The error lists any output files still on disk and says they were not validated, since they may be partial or left over from an earlier run.

### `soroban-testkit limits`

Empirically discovers resource ceilings rather than guessing them.

```
soroban-testkit limits --contract <path-to-wasm> --fn <name> --ramp <param> [--arg <name=value> ...] [--jobs <n>]
```

`--jobs <n>` (1 to 8, default 1) is opt-in. It probes up to `n` consecutive ramp values concurrently while doubling, and reads results back in ramp order, stopping at the first failure. The reported boundary matches a sequential run. Probes past the first failure still run to completion but are discarded. Binary refinement always runs one probe at a time.

The contract path must point to an existing compiled WASM file; `limits` does not
build contracts. Chain your build and limits invocation with `&&` so a failed
rebuild cannot lead to measuring an older artifact. Relative paths are resolved
from the current directory.

Binary refinement assumes success is monotonic up to a resource ceiling. Both
bracket endpoints are re-probed first; contradictory results stop the command
with an error. This check cannot establish monotonicity at unprobed values.

Invokes the function with an increasing parameter (e.g. recipient count) until it exceeds resource limits, refines the boundary with binary search, then reports the last successful value plus instruction count and memory at that point. `Bytes` ramp parameters vary by length and contain zero bytes. Use repeatable `--arg NAME=VALUE` options to set non-ramped scalar parameters; values are parsed according to the Soroban type, and `Bytes` values use `0x`-prefixed hexadecimal. These overrides are forwarded to each isolated probe. Unsupported aggregate types produce an actionable error.

**This directly answers the `batch_payout` maximum-recipients question** and is the CLI's most compelling demo. Feature it in the README.

### `soroban-testkit audit`

Static checks over a contract crate. Start with three, each independently useful and each a natural issue for a contributor:

1. Entry points that take an `Address` parameter but never call `require_auth` on it
2. Arithmetic on `i128` outside a checked/trapping context
3. Storage reads with no corresponding TTL bump on the same key

Output: file, line, rule, severity, and a one-line explanation. Exit non-zero on findings when `--strict`.

**This is not a security product.** It is a linter with three heuristics. Say so in the output footer and in the README. Overclaiming here would be worse than not shipping it.

**Acceptance criteria (all commands):**
- `--help` text is complete and accurate
- Every command has an integration test against `examples/vault`
- No command requires network access
- Errors print actionable messages, never a raw panic or backtrace

---

## Module 10 — validation against a real contract suite

**Goal:** prove the crate works on something that is not a toy, and generate the README's evidence.

**Steps:**

1. Add `soroban-testkit` as a dev-dependency to a real deployed contract suite — use `sororail-contracts` if available, or any public Soroban repo with a permissive license.
2. Rewrite its existing tests using the testkit. Record the line-count delta.
3. Run `AuthMatrix::assert_enforced` across every contract. Record any findings.
4. Run `assert_conserved_over` on any streaming or vesting contract.
5. Run `soroban-testkit limits` on any batch operation.

**Deliverable:** a `VALIDATION.md` in the repo recording what was found, including nothing-found results. If the auth matrix finds a real bug in a real contract, that becomes the README's headline and the strongest possible argument for the crate.

**Do not skip this module.** A testing library nobody has used on real contracts is not credible, and this is the cheapest credibility you will ever buy.

---

## 11. Conventions

**License:** Apache-2.0, present before the first external contribution.

**Commits:** Conventional Commits, enforced in CI.

**Branching:** trunk-based, short-lived branches, squash merge, linear history.

**CI on every PR:** `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test --workspace`, `cargo doc` with no warnings, `cargo audit`. Red CI blocks merge with no maintainer exception.

**Error handling:** no `unwrap()` or `expect()` in library code outside tests. Assertion helpers panic deliberately — that is their contract — but with `TestkitError` context, never a bare message.

**Documentation:** `#![deny(missing_docs)]` is enforced from Module 8 onward. Every public item needs a doc comment with a runnable example. A PR changing public behavior without touching docs is incomplete.

**Failure messages are a first-class feature.** This is a testing crate; its output *is* its user interface. Any assertion helper whose failure message does not tell the user what went wrong and what to look at is not done.

**Testing the tests:** every assertion helper needs two tests — one where it passes, one where it correctly fails. Use `#[should_panic(expected = "...")]` to pin the message.

---

## 12. Verify before you build

The Soroban toolchain moves quickly and this document is a snapshot. Confirm the following against current sources before writing code. **Where current documentation contradicts this spec, current documentation wins** — note the discrepancy in the PR description so the spec can be corrected.

1. **Current `soroban-sdk` version.** Check crates.io for the latest *stable* release; do not pin a prerelease. Confirm the pinned Rust toolchain satisfies it.
2. **`soroban_sdk::testutils` current surface.** Specifically what exists for ledger manipulation, event access, auth mocking, and TTL inspection. Several modules here assume capabilities that may have been added, renamed, or removed. Modules 2, 4, 6, and 7 all depend on this.
3. **The ledger close interval constant** used in `advance_ledgers`. Do not hardcode 5 seconds from this document.
4. **TTL / state archival parameters** and whether the SDK exposes them at runtime. This determines how much of Module 7 is buildable.
5. **Stellar Asset Contract interface** for the token calls in Module 5.
6. **Current coverage tooling for the WASM target** — this changes often and Module 9 depends on it.
7. **`stellar` CLI syntax** if the CLI shells out to it. It was renamed from `soroban` and command shapes have changed across releases; older tutorials are wrong.

---

## 13. Issue seeding for Drips Wave

Once Module 8 is done, the repo can be applied to a Wave Program. Target 12–15 open issues before a Wave opens; an approved repo with an empty board is worse than not applying.

Every issue must state: the problem, affected file paths, expected behavior, acceptance criteria, and how to test locally. An issue that does not meet that bar is not ready for a `good-first-issue` label.

**Natural issue seams in this design:**

- One generator function in Module 3 per issue (`bps_values`, `overflow_edge_triples`, …)
- One `EventLog` filter or assertion method per issue
- One `audit` rule per issue — indefinitely extensible, each self-contained
- One `TestToken` method per issue
- Doc examples for each public item
- Failure-message improvements, each independently reviewable
- Each `VALIDATION.md` target repo as its own issue

Label taxonomy: `good-first-issue`, `help-wanted`, `module/core`, `module/money`, `module/events`, `module/auth`, `module/ttl`, `module/cli`, `docs`, `size/s`, `size/m`, `size/l`.

**Difficulty guidance:** Modules 3, 4, 5, and the `audit` rules are good first issues. Module 7 is not — do not label TTL work as beginner-friendly.

## Coverage tool support

The `coverage` command delegates collection to `cargo-llvm-cov`, which is the supported coverage tool for the Soroban WASM target. Keep the tool installed from its current stable release and verify compatibility with the repository's pinned Rust toolchain before running coverage locally or in CI.

Use the same tool and format flags in local and CI workflows so text, LCOV, HTML, and JSON reports describe the same instrumented target. If `cargo-llvm-cov` is unavailable, the command should report that prerequisite clearly and include the installation or version-check guidance instead of failing with an opaque shell error.

Coverage collection is separate from the normal test runner: run the workspace tests first, then invoke the coverage command for the report format required by the workflow. This keeps tool availability, target setup, and report generation failures distinguishable.