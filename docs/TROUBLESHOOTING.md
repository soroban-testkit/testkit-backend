# Troubleshooting Soroban test failures

This guide maps common `soroban-testkit` failures to their cause and fix. Start
with the error code: every runtime failure the crate raises is a
`TestkitError` with a stable `code()`, and the message after it says what
went wrong.

| Code | Meaning | Section |
|---|---|---|
| `TESTKIT_ASSERTION_FAILED` | A contract behaved differently than the test expected | [Assertion failures](#assertion-failures-testkit_assertion_failed) |
| `TESTKIT_DECODE_FAILED` | A captured value could not be decoded into the requested type | [Decode failures](#decode-failures-testkit_decode_failed) |
| `TESTKIT_MISUSE` | The testkit API was called in a way it does not support | [Misuse](#misuse-testkit_misuse) |

Compile errors and non-reproducible runs have their own sections below.

## Assertion failures (`TESTKIT_ASSERTION_FAILED`)

Assertion helpers panic with this code when a contract does not meet the
expectation. The message names the expectation that failed.

- **An expected event was never emitted.** Check that the call under test
  actually ran, and that it was made against the same environment whose events
  you are reading. Events belong to one `TestEnv`; a helper on a different
  environment will not see them.
- **A conservation invariant does not hold.** Check the starting state the
  test set up, and whether an earlier call in the test changed the balances
  the invariant reads.
- **A time-dependent value is off by one ledger.** Ledger timestamps and
  sequence numbers move together, in proportion to the close interval. If the
  expected value was computed by hand, compare it with
  `env.sequence()` and `env.now()` after the same `advance` call. See
  [`TestEnv::ledger_close_interval`](../crates/testkit/src/ledger/clock.rs) for
  the interval in effect.

To see the full state at the point of failure, print `env.metadata()` or the
checkpoint you took before the failing step.

## Decode failures (`TESTKIT_DECODE_FAILED`)

A decode helper could not convert a captured value into the type you asked
for. The usual cause is a type mismatch: the contract returned or emitted a
value with a different shape than the Rust type in the test.

- Check the type the test requests against the contract's declared return or
  event type, including the field order of structs and enums.
- If the value came from an event, confirm you are decoding the event that
  matches the topic you filtered on. Two events with the same payload shape
  but different topics will both decode, so the wrong one can pass silently.
- The error carries the original decode error as its source. Follow
  `std::error::Error::source` when the top-level message is not specific
  enough.

## Misuse (`TESTKIT_MISUSE`)

Misuse means the testkit rejected an argument or sequence of calls before it
touched the contract. The message says which rule was broken. Common cases:

- **Clock moved backwards.** `warp_to` and `advance_to_sequence` never move the
  ledger back. To return to an earlier position, take a checkpoint before the
  change with `env.checkpoint()` and restore it with
  `env.restore_checkpoint(...)`. See
  [`LedgerCheckpoint`](../crates/testkit/src/ledger/clock.rs).
- **Zero ledger close interval.** `with_ledger_close_interval(0)` is rejected,
  because a zero interval cannot map elapsed time onto ledger sequence numbers.
  Use at least one second.
- **Clock overflow.** Advancing far enough that the timestamp or sequence no
  longer fits in its integer type is a misuse. Use the `try_` variant, such as
  `try_advance` or `try_warp_to`, to get the error as a value instead of a
  panic. The clock is left unchanged on error.
- **Duplicate address label.** `address_with_label` rejects a label that is
  already in use in the same environment. Pick a distinct label, or reuse the
  address you already have.
- **Batch too large.** Address batches are capped at `MAX_ADDRESS_BATCH_SIZE`.
  Split the request into smaller batches.

If a `try_` method exists for the call that failed, prefer it in tests that
check error handling, and keep the panicking form for setup code where a
failure should stop the test.

## Compile errors

These come from the Rust compiler, not from a runtime check. They protect the
environment's ownership rules; see the "Thread Safety and Environment Sharing"
section of [`crates/testkit/src/core/mod.rs`](../crates/testkit/src/core/mod.rs).

- **`Rc<...> cannot be sent between threads safely`.** `TestEnv` is not `Send`.
  Create the environment inside the thread or test that uses it. Do not move
  it into `std::thread::spawn`.
- **`cannot borrow ... as mutable because it is also borrowed as immutable`.**
  A reference from `env.env()` is still alive. `reset` takes `&mut self`, so
  drop the reference before resetting the environment.
- **A reference from `env.env()` outlives the `TestEnv`.** Keep the `TestEnv`
  in scope for as long as any reference to its environment is used.

## Non-reproducible runs

If a test passes on some runs and fails on others, check where its inputs come
from:

- `TestEnv::new()` derives its seed from the wall clock and a process-wide
  counter. Two environments in the same process get different seeds, and the
  seeds depend on creation order. Use `TestEnv::with_seed(...)` for any test
  whose randomized values must be the same on every run.
- Tests that depend on the number of other environments created before them
  are sensitive to test ordering. Build the environment inside the test itself.

## Protocol version errors

A protocol version set with `LedgerDefaults::protocol_version` must be one the
`soroban-sdk` host accepts. A value that is too old is rejected by the host.
To avoid guessing a number, read the baseline from the SDK:

```rust
let current = soroban_sdk::Env::default().ledger().get().protocol_version;
let env = TestEnv::with_ledger_defaults(LedgerDefaults::new().protocol_version(current));
```

If you need to pin an older protocol on purpose, confirm the value against
[`COMPATIBILITY.md`](../COMPATIBILITY.md) for the `soroban-sdk` version in use.

## Still stuck?

- Re-run the failing test alone, with the test name filter, to rule out
  ordering effects.
- Take a checkpoint before the failing step and print `env.metadata()` at
  the failure point to see the ledger state.
- Open an issue with the error code, the full message, and the `soroban-sdk`
  version from [`COMPATIBILITY.md`](../COMPATIBILITY.md).
