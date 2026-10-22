# soroban-testkit

**The testing crate you would have written yourself, on the third contract.**

Testing infrastructure for Soroban contracts: property-test generators for
money math, an event-assertion API, ledger time and TTL-expiry control,
token test doubles, and a coverage-aware CLI.

`soroban-sdk` ships `testutils`, which is adequate for basic unit tests. It
is not adequate for testing contracts that hold money. Every serious
Soroban contract repo independently rebuilds some subset of:

- Advancing ledger time correctly, including the interaction between
  `timestamp` and `sequence`
- Asserting on emitted events without hand-decoding `Val` topics
- Generating adversarial `i128` values that actually find overflow bugs
- Deploying and funding a Stellar Asset Contract token double for transfer
  tests
- Proving that every privileged entry point rejects unauthorized callers
- Simulating TTL expiry

`soroban-testkit` is that shared layer, as a dev-dependency crate plus a
small CLI.

## Example

A small escrow contract, tested end to end: mint a token, release it to
the seller, and check the event, the balances, and that no value was
created or destroyed along the way.

```
use soroban_sdk::{contract, contractimpl, symbol_short, Address, Env};
use soroban_testkit::prelude::*;

#[contract]
struct Escrow;

#[contractimpl]
impl Escrow {
    pub fn release(env: Env, token: Address, from: Address, to: Address, amount: i128) {
        from.require_auth();
        soroban_sdk::token::TokenClient::new(&env, &token)
            .transfer(&from, soroban_sdk::MuxedAddress::from(to), &amount);
        #[allow(deprecated)]
        env.events().publish((symbol_short!("release"),), amount);
    }
}

# fn main() {
let env = TestEnv::new();
let token = env.token();
let buyer = env.address();
let seller = env.address();
token.mint(&buyer, 100);

let escrow_id = env.env().register(Escrow, ());
let client = EscrowClient::new(env.env(), &escrow_id);

env.env().mock_all_auths();
let (_, events) = env.events_during(|| {
    client.release(&token.address(), &buyer, &seller, &1_000_000_000);
});
events
    .from(&escrow_id)
    .assert_emitted(symbol_short!("release"));

// Each of these balance queries is itself a top-level call, so check
// them after reading events — see EventLog's note on scope.
token.assert_balance(&buyer, 0);
token.assert_balance(&seller, 1_000_000_000);

Conservation {
    deposited: 1_000_000_000,
    withdrawn: 1_000_000_000,
    refunded: 0,
    remaining: 0,
}
.assert_holds();
# }
```

## CLI

A companion binary, `soroban-testkit-cli` (installs as `soroban-testkit`),
for things that don't belong in a dev-dependency:

```sh
# Coverage for the WASM target (wraps cargo-llvm-cov, which needs no
# Soroban-specific flags since contract tests run natively).
soroban-testkit coverage --format html --open

# Machine-readable coverage report
soroban-testkit coverage --format json --output coverage.json

# Empirically find how many recipients a batch operation can handle
# before it exceeds mainnet resource limits. Build successfully first so
# a failed rebuild cannot leave limits measuring an older WASM artifact.
cargo build -p my_contract --target wasm32v1-none --release &&
soroban-testkit limits --contract target/wasm32v1-none/release/my_contract.wasm \
  --fn batch_payout --ramp recipients

# Static checks: missing require_auth, unchecked i128 arithmetic,
# storage reads with no TTL bump, ignored token-transfer results, and
# signed amount parameters with no positive-value validation.
soroban-testkit audit ./src --strict

# Stable machine-readable reports for automation and GitHub code scanning.
soroban-testkit audit ./src --format json
soroban-testkit audit ./src --format sarif > audit.sarif
```

`limits` ramps numeric and `Bytes` parameters directly, or ramps a `Vec<T>`
by element count. `--ramp` must exactly match an input name in the selected
function's contract ABI. An unknown name is rejected before probes run or
reports are written, with the available inputs listed for recovery.
Use repeated `--arg NAME=VALUE` options for non-ramped scalar values;
`Bytes` overrides use `0x`-prefixed hex. The command refines the first
failing ramp value with binary search.
Every ramp attempt runs in its own subprocess: a real `.wasm` contract
that exceeds resource limits can abort the process outright rather than
return an error, and isolating each attempt is the only safe way to probe
past that boundary. Failed probes are classified with a stable category
(`exit-failure`, `timed-out`, `abort`, or `spawn-failure`), and the same
summary renders both the terminal output and the `--export` JSON, so the
two formats cannot disagree about the discovered boundary. Ledger
read/write counts and transaction size aren't reported (they come from a
network-side simulated footprint this crate doesn't produce); instructions
and memory, measured locally, are.

Probe execution uses the already-built `soroban-testkit` executable directly,
not `cargo run` or a throwaway Cargo project. Each probe child is killed and
reaped on timeout; an early ordinary error/return also invokes a cleanup
guard. This guard cannot run if the operating system forcibly kills the
parent process. Contract paths are passed as single OS arguments; copyable
reproduction instructions shell-quote paths containing spaces or quotes.

`audit` accepts three scan roots: a crate directory (walked recursively for
`.rs` files), a single `.rs` file, or `-` to read Rust source from stdin.
While walking a directory it skips `target/`, `vendor/`, `node_modules/`,
and hidden directories such as `.git/` and `.github/`, keeping build output
and dependency trees out of recursive scans. A missing path is an actionable
error rather than an empty "no findings" run. An explicitly selected excluded
directory or single file is still scanned. Empty, whitespace-only, and
comment-only Rust files are valid inputs with no findings. Raw identifiers
such as `r#amount` are checked the same way as their ordinary spellings.

Findings use paths relative to the Cargo workspace root (or the repository
root or crate root when there is no workspace). Every path the command
compares or prints is normalized first — symlinks and `.`/`..` components
resolved — so one file always reports under a single spelling however its
scan root was spelled on the command line. Findings are then reported in
one deterministic order — file, then line, then rule, then message — so a
report diff always reflects a real change rather than scan order. The
nearest `.soroban-testkit.toml` or `soroban-testkit.toml` between the
scanned source directory and that root supplies rule configuration; the
dotted name takes precedence in the same directory. Single-file scans start
config discovery at the file's parent, and stdin scans start at the current
directory.
`--baseline baseline.json` resolves the baseline file from the current
directory, while its `file` entries are workspace-relative, so one baseline
works from member crates too. Absolute entries are also accepted.

Unreadable files, directory traversal failures, and Rust parse errors are
reported separately on stderr with the affected path (and line/column for
syntax errors). These are *parse warnings*, not findings: a warning means
the input was skipped rather than scanned, so it never appears in the
report, never counts toward `finding_count`, and never triggers `--strict`.
The scan continues through other files and writes their findings in the
requested format, then exits non-zero to signal an incomplete audit even
without `--strict`. Fix these warnings before treating an empty report as a
clean scan. JSON reports escape control characters in file names and
diagnostic text.

**Audit limitation:** the linter inspects written Rust source only. It does
not expand procedural macros (including Soroban contract macros) or
`macro_rules!`, so generated entry points and generated bodies may go
unexamined. A clean report does not certify generated code. The limitation
is included in text, JSON (`summary.limitations`) and SARIF
(`runs[].properties.limitations`) results. Inspect expanded source
separately when generated code matters.

## Status

This crate is under active development. See `BUILD_SPEC.md` for the build
plan and module boundaries, and [`ARCHITECTURE.md`](ARCHITECTURE.md) for a
contributor-facing map of the workspace and how its modules fit together.

CI enforces the guarantees that keep this documentation honest: every Rust
example above is compiled and run as a doctest against the public API,
rustdoc warnings fail the build, the test suite runs with network access
removed, and the crates.io `repository` metadata is checked against the
canonical repository URL.

## Documentation

- [`CHANGELOG.md`](CHANGELOG.md) — user-facing release notes for each
  version of both workspace crates.
- [`API_STABILITY.md`](API_STABILITY.md) — the semver/API stability policy
  for the `0.x` releases: what a version bump means and what you may rely
  on.
- [`COMPATIBILITY.md`](COMPATIBILITY.md) — which `soroban-sdk` and Stellar
  protocol versions each release targets.
- [`MIGRATION.md`](MIGRATION.md) — upgrade notes for each minor release,
  with before/after code for every breaking change.
- [`RELEASING.md`](RELEASING.md) — the release checklist for
  `soroban-testkit` and `soroban-testkit-cli`.
- [`BUILD_SPEC.md`](BUILD_SPEC.md) — the build plan and module boundaries.
- [`ARCHITECTURE.md`](ARCHITECTURE.md) — a contributor-facing map of the
  workspace.
- [`docs/TROUBLESHOOTING.md`](docs/TROUBLESHOOTING.md) — what the common
  `soroban-testkit` failure codes mean and how to fix them.

## Prior art

[`soroban-fork`](https://crates.io/crates/soroban-fork) does lazy
mainnet/testnet forking for tests. That is a different problem;
`soroban-testkit` does not attempt it.

## License

Apache-2.0
add validation coverage for the recurring contract
add validation coverage for the batch payout auth model

add a line-count and coverage comparison to validation

add benchmark tracking for TestEnv construction
