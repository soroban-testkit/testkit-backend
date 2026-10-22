# Architecture

A map of the workspace for contributors: what each crate and module is
responsible for, how they depend on each other, and the conventions that
hold the codebase together. Read `BUILD_SPEC.md` first for the project's
scope and non-negotiable rules; this document is about *where things live*
and *why the boundaries are drawn where they are*.

## Workspace layout

```
soroban-testkit/
├── crates/
│   ├── testkit/     the library: soroban-testkit
│   └── cli/         the binary: soroban-testkit-cli (coverage, limits, audit)
├── examples/
│   └── vault/       a real contract used as a fixture by the CLI's
│                     integration tests (compiled to wasm32v1-none)
└── deny.toml, .github/workflows/ci.yml, BUILD_SPEC.md, CONTRIBUTING.md
```

Two workspace members, `crates/*` and `examples/vault`, resolved with
`resolver = "2"` (see the root `Cargo.toml`). `examples/vault` is not a
toy — the CLI's `limits` command builds it to a real `.wasm` and asserts
against it, so it doubles as a fixture and as living documentation of what
a testkit-tested contract looks like.

## `crates/testkit` — the library

Everything here is a `pub mod`, declared in `lib.rs`, each owning one
concern. A module never reaches into another module's private internals;
cross-module composition happens through `prelude.rs` or through public
types passed as arguments (e.g. `TestEnv` is threaded through nearly every
other module's public functions).

| Module | Owns | Depends on |
|---|---|---|
| `core` | [`TestEnv`] (wraps `soroban_sdk::Env`) and [`TestkitError`], the error type every assertion helper panics with | — (the foundation) |
| `auth` | [`AuthMatrix`] — systematically proving every privileged entry point rejects unauthorized callers | `core` |
| `events` | Asserting on emitted events without hand-decoding `Val` topics | `core` |
| `ledger` | Advancing/warping ledger time and reasoning about the `timestamp`/`sequence` relationship | `core` |
| `money` | Adversarial `i128` generation and conservation-of-value assertions | `core` |
| `tokens` | One-line Stellar Asset Contract test doubles | `core`, `ledger` |
| `ttl` | Ledger-entry TTL inspection and expiry simulation | `core`, `ledger` |
| `prelude` | Re-exports the common subset of the above for a single `use soroban_testkit::prelude::*;` | all of the above |

If you're adding a new capability, ask which row it belongs in before
writing code. A new top-level module is warranted only when the capability
doesn't fit an existing one *and* is itself in scope per `BUILD_SPEC.md`
§3 — most new assertion helpers extend an existing module (e.g. a new
event-shape assertion goes in `events`, not a new module).

### The error contract

Assertion helpers **panic** — that's how a failing test surfaces in
`cargo test` output — but every panic carries a [`TestkitError`], never a
bare string. This is what makes failure messages a first-class feature
(per `CONTRIBUTING.md`): a `TestkitError::Misuse("events were never
captured".into())` tells the next contributor exactly what to fix, where a
bare `panic!("bad state")` doesn't. When you add an assertion helper, add
its failure variant to `TestkitError` (or reuse an existing one) rather
than reaching for `panic!` directly.

## `crates/cli` — the binary

A thin layer over the library, structured as one file per subcommand under
`src/commands/`:

| Command | Purpose |
|---|---|
| `coverage` | Runs and reports test coverage for a contract crate |
| `limits` | Builds a contract to `wasm32v1-none` and checks it against Soroban resource limits (this is what depends on `examples/vault`) |
| `audit` | Runs the auth/event/TTL assertion suite against a contract as a single pass |

`main.rs` wires subcommands to `clap`'s derive API and nothing else — CLI
argument parsing and output formatting live in `commands/`, and any logic
that isn't specific to being a CLI (e.g. "how do I build a contract to
wasm") belongs in the `testkit` library so it stays usable outside the
CLI.

## Data flow: a typical test

1. A contract test constructs a [`TestEnv`] (`core`), which wraps
   `soroban_sdk::Env` and is the handle every other module's functions
   take as their first argument.
2. The test drives the contract under test as normal (`soroban_sdk`
   client calls), then reaches for `soroban_testkit::prelude::*` helpers
   to assert on the result: `assert_conserved_over` (`money`), an events
   assertion (`events`), a TTL check (`ttl`), or `AuthMatrix` (`auth`) to
   prove unauthorized callers are rejected.
3. On failure, the helper panics with a `TestkitError`-derived message
   that names the specific expectation and the actual value observed —
   never a generic "assertion failed".

## The in-process testing boundary

Every test that uses `soroban-testkit` runs **entirely in the same OS
process as `cargo test`**.  There is no network, no Stellar node, no
Docker container, and no external process.  `soroban-sdk`'s test
environment (`soroban_sdk::Env`) is an in-process simulator; `TestEnv`
wraps it and adds the helpers described above.

### What this means in practice

- **Tests are fast and deterministic.**  A full test suite for a
  moderately complex contract typically finishes in under a second.
  There is nothing to boot, authenticate against, or tear down between
  runs.

- **Everything is mocked by design.**  Ledger time, token balances,
  events, storage TTLs — all are controlled by the test through
  `TestEnv`.  You do not need a real network to advance the clock by a
  year or simulate a persistent-storage expiry.

- **The boundary is the WASM ABI, not a network.**  Your contract
  functions are called through the same generated client types you would
  use in production code (`VaultClient`, etc.), but they execute against
  the in-process simulator rather than a live ledger.

### Worked example: testing a time-locked vault

The following example exercises three concerns at once — auth, ledger
time, and storage TTL — without leaving the process:

```rust
// In your contract crate's dev-dependency tree:
// soroban-testkit = { version = "0.1", features = ["testutils"] }

use soroban_sdk::{testutils::Address as _, Address, Env};
use soroban_testkit::prelude::*;
use vault::{Vault, VaultClient};

#[test]
fn owner_cannot_withdraw_before_lock_expires() {
    // 1. Construct an isolated in-process environment.  No network.
    let env = TestEnv::new();
    let contract_id = env.env().register(Vault, ());
    let client = VaultClient::new(env.env(), &contract_id);

    let owner = Address::generate(env.env());
    client.initialize(&owner);
    client.deposit(&1_000);

    // 2. Advance ledger time without touching the clock on your machine.
    env.advance_ledgers(10);  // fast-forward ten ledgers (~50 seconds)

    // 3. Assert that auth is enforced: an unauthorized caller must fail.
    env.assert_requires_auth(|| {
        client.withdraw(&500);  // no mock_all_auths → must panic
    });

    // 4. Now mock the owner's signature and withdraw successfully.
    env.env().mock_all_auths();
    client.withdraw(&500);

    // 5. Conservation: nothing was created or destroyed.
    Conservation {
        deposited: 1_000,
        withdrawn: 500,
        refunded: 0,
        remaining: 500,
    }
    .assert_holds();
}
```

Key observations:

1. `TestEnv::new()` is the only setup required.  No `before_each`, no
   fixture files, no environment variables.
2. `env.advance_ledgers(10)` moves the in-process clock forward.  The
   real clock on the CI machine is irrelevant.
3. `env.assert_requires_auth(|| ...)` wraps a call that should panic
   when auth is absent.  If the contract forgets `require_auth`, this
   assertion fails and the test output names the missing check.
4. `Conservation::assert_holds()` verifies that no value leaked or
   appeared from nowhere — the most common class of bug in streaming or
   vesting contracts.

### What falls outside this boundary

The in-process simulator does not model network conditions, fee markets,
sequence-number races, or multi-transaction interactions.  For those
scenarios, refer to `soroban-fork` (lazy mainnet/testnet forking) or the
Stellar testnet.  The `soroban-testkit` CLI's `limits` command bridges
one gap: it builds your contract to WASM and measures real resource
consumption, answering "how many recipients can my batch payment handle
before it exceeds mainnet limits?" without a live network.

## CI's shape mirrors this structure

`.github/workflows/ci.yml` jobs map directly onto the guarantees this
document describes: `lint`/`test`/`doc` enforce the code conventions in
`CONTRIBUTING.md`, `audit`/`deny` enforce the supply-chain policy, and
`coverage`/`scheduled-sdk-check` guard the two properties above the module
table can't: how much of the library's own behavior is exercised by its
own test suite, and whether it still builds against whatever `soroban-sdk`
release is current upstream.

[`TestEnv`]: crates/testkit/src/core/env.rs
[`TestkitError`]: crates/testkit/src/core/error.rs
[`AuthMatrix`]: crates/testkit/src/auth/matrix.rs
