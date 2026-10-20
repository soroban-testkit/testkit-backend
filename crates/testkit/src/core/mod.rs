//! The entry point every other module builds on: [`TestEnv`], a wrapper
//! around [`soroban_sdk::Env`], and [`TestkitError`], the error type
//! assertion helpers panic with.
//!
//! # Thread Safety and Environment Sharing
//!
//! - **One environment per test.** [`TestEnv`] is neither `Send` nor `Sync`:
//!   it holds the `soroban_sdk` host behind an `Rc`. It cannot be moved into
//!   `std::thread::spawn`, and it cannot be shared by reference across
//!   threads either. Build a fresh `TestEnv` inside each test (or each
//!   thread that needs one).
//! - **Parallel tests are isolated.** Cargo runs each `#[test]` on its own
//!   thread with its own `TestEnv`. Ledger time, storage, captured events,
//!   address labels, and checkpoints belong to one environment, so one test
//!   cannot observe another test's `advance`, `warp_to`, or `at` calls.
//! - **Cloning configuration is not sharing state.** [`TestEnv::clone_config`]
//!   and [`TestEnv::reset`] start from the same configuration with a fresh
//!   ledger. Neither hands the original host to another environment.
//! - **Raw handles alias the same host.** A reference from
//!   [`TestEnv::env`] points at the environment's own host, not a copy.
//!   Mutations made through it are visible through the `TestEnv`, and
//!   `env().clone()` produces a second handle to that same host.
//! - **Seeds are unique, not reproducible.** [`TestEnv::new`] derives its
//!   seed from the wall clock and a process-wide counter, so two environments
//!   never share a seed within one process, but the seeds depend on creation
//!   order. Use [`TestEnv::with_seed`] when a test must produce the same
//!   values on every run.
//!
//! # `no_std` Compatibility Assessment
//!
//! **Current status: `std`-only.**
//!
//! This module depends on several items from `std` that have no stable
//! `core`/`alloc` replacements at the time this assessment was written
//! (soroban-sdk 27.0.6, Rust 1.80+):
//!
//! | Dependency | Source | Reason `no_std` is blocked |
//! |---|---|---|
//! | `std::sync::atomic::AtomicU64` | `env.rs` — `SEED_COUNTER` | Available in `core::sync::atomic` on most targets; not a blocker. |
//! | `std::time::SystemTime` | `env.rs` — `random_seed()` | **No `core` equivalent.** `SystemTime` is used as an entropy source for non-reproducible seeds. A `no_std` port would need a caller-supplied entropy hook or a compile-time feature that removes `TestEnv::new()` in favour of `TestEnv::with_seed(…)`. |
//! | `std::panic::{catch_unwind, resume_unwind}` | `ledger/clock.rs` — `TestEnv::at` | **No `core` equivalent.** `at()` catches a panic to guarantee clock restoration even when the closure panics. Without `std`, the restore-on-unwind guarantee cannot be upheld. |
//! | `thiserror` (derives `std::error::Error`) | `error.rs` — `TestkitError` | `thiserror` supports `no_std` since 2.0 via `#[error]` on enums with `#![no_std]`; not a blocker once the others are resolved. |
//! | `soroban_sdk` with `testutils` feature | `env.rs` | The `soroban-sdk` test environment itself requires `std` (it uses threads and OS resources). A `no_std` port of `soroban-testkit` is therefore **blocked upstream** until `soroban-sdk`'s test utilities support `no_std`. |
//!
//! ## Verdict
//!
//! `soroban-testkit` cannot be made `no_std` today because its most
//! fundamental dependency — `soroban_sdk` with `features = ["testutils"]` —
//! requires `std`. This is a deliberate design choice in the Soroban SDK:
//! the test host is a full in-process simulation that exercises OS-level
//! primitives (threads, time, file I/O for snapshots). There is no plan to
//! change that.
//!
//! Even if `soroban-sdk/testutils` were made `no_std`, `soroban-testkit`
//! would still require changes to:
//!
//! 1. Replace `SystemTime` in `random_seed()` with an injected entropy source
//!    (or remove the non-deterministic `TestEnv::new()` constructor behind a
//!    feature flag, keeping only `TestEnv::with_seed`).
//! 2. Provide a `no_std`-safe alternative to `catch_unwind` / `resume_unwind`
//!    in `TestEnv::at()`, either by dropping the panic-safety guarantee or by
//!    gating `at()` behind a `std` feature flag.
//!
//! ## Recommendation
//!
//! Track this as a future enhancement once `soroban-sdk/testutils` publishes
//! a `no_std` target. Until then, the crate correctly uses `std` throughout
//! and no `no_std` shims are added.

mod env;
mod error;

pub use env::{
    assert_independent, check_independent, Actor, AddressIter, EnvMetadata, FixtureBuilder,
    LedgerDefaults, TestEnv, TestFixture, MAX_ADDRESS_BATCH_SIZE,
};
pub use error::TestkitError;

#[cfg(test)]
mod no_std_assessment_tests {
    /// Documents that the `core` module is explicitly `std`-only and
    /// tracks the reasons why, so that CI will fail if someone accidentally
    /// compiles it without `std` and gets confusing errors instead of a
    /// clear explanation.
    ///
    /// This test is a compile-time marker: if this module builds, `std` is
    /// available. There is no assertion because the guarantee is that the
    /// build *succeeds* with `std` present — a `no_std` build will fail at
    /// the import of `std::time::SystemTime` long before reaching this test.
    #[test]
    fn core_requires_std_and_that_is_expected() {
        // `std::time::SystemTime` is used in `random_seed()`.
        let _ = std::time::SystemTime::now();
        // `std::panic::catch_unwind` is used in `TestEnv::at()`.
        let _ = std::panic::catch_unwind(|| ());
    }
}
