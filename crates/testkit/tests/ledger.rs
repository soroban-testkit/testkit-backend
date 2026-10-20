//! Integration coverage for the scoped ledger clock (`TestEnv::at`), ledger
//! checkpoints (`TestEnv::checkpoint` / `TestEnv::restore_checkpoint`), and
//! protocol-version fixtures (`LedgerDefaults::protocol_version`).
//!
//! `at` runs a closure at a chosen ledger time and then restores the
//! environment. These tests pin the boundary that a scoped evaluation must
//! not be built from — or leave behind — ledger state newer than the moment
//! it claims to represent: inside the scope the sequence number belongs to
//! the scoped timestamp, and the newer pre-scope clock comes back intact on
//! exit.
//!
//! Checkpoints are the explicit form of the same idea: a saved ledger
//! position that a test can return to, even backwards. Protocol-version
//! fixtures must survive every path that rebuilds or rewinds the ledger.

use std::panic::{self, AssertUnwindSafe};
use std::time::Duration;

use soroban_sdk::testutils::Ledger as _;
use soroban_testkit::core::{LedgerDefaults, TestEnv};
use soroban_testkit::ledger::LEDGER_CLOSE_TIME_SECS;

fn clock(env: &TestEnv) -> (u64, u32) {
    (env.now(), env.sequence())
}

#[test]
fn scoped_at_reports_the_sequence_that_belongs_to_its_timestamp() {
    let env = TestEnv::new();
    env.advance(Duration::from_secs(100)); // (100, 20)

    let inside = env.at(40, || clock(&env));

    assert_eq!(
        inside,
        (40, 8),
        "a scope at timestamp 40 must not report the newer pre-scope sequence"
    );
    assert_eq!(clock(&env), (100, 20));
}

#[test]
fn advance_inside_a_scoped_at_moves_from_the_scoped_position() {
    let env = TestEnv::new();
    env.advance(Duration::from_secs(100)); // (100, 20)

    let inside = env.at(40, || {
        env.advance(Duration::from_secs(10));
        clock(&env)
    });

    assert_eq!(inside, (50, 10));
    assert_eq!(
        clock(&env),
        (100, 20),
        "the newer pre-scope clock must come back intact"
    );
}

#[test]
fn scoped_at_restores_the_newer_pre_scope_state_when_the_closure_panics() {
    let env = TestEnv::new();
    env.advance(Duration::from_secs(100));
    let before = clock(&env);

    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        env.at(0, || {
            assert_eq!(clock(&env), (0, 0));
            panic!("boom");
        });
    }));

    assert!(result.is_err());
    assert_eq!(clock(&env), before);
}

#[test]
fn scoped_at_still_rolls_back_ledger_fields_changed_inside() {
    let env = TestEnv::new();
    let before = env.env().ledger().get();

    env.at(env.now() + 25, || {
        env.env().ledger().set(soroban_sdk::testutils::LedgerInfo {
            base_reserve: before.base_reserve + 1,
            ..env.env().ledger().get()
        });
    });

    let after = env.env().ledger().get();
    assert_eq!(after.base_reserve, before.base_reserve);
    assert_eq!(after.protocol_version, before.protocol_version);
    assert_eq!(after.network_id, before.network_id);
}

#[test]
fn scoped_at_preserves_a_seeded_timestamp_sequence_offset() {
    let env = TestEnv::with_ledger_defaults(
        LedgerDefaults::new()
            .timestamp(1_700_000_000)
            .sequence_number(1_000),
    );

    let inside = env.at(1_700_000_000 - 100, || clock(&env));

    assert_eq!(inside, (1_700_000_000 - 100, 980));
    assert_eq!(clock(&env), (1_700_000_000, 1_000));
}

#[test]
fn scoped_at_clamps_the_sequence_at_the_ends_of_its_range() {
    let past = TestEnv::with_ledger_defaults(
        LedgerDefaults::new()
            .timestamp(1_000_000)
            .sequence_number(1),
    );
    assert_eq!(past.at(0, || past.sequence()), 0);
    assert_eq!(clock(&past), (1_000_000, 1));

    let future = TestEnv::new();
    assert_eq!(future.at(u64::MAX, || future.sequence()), u32::MAX);
    assert_eq!(clock(&future), (0, 0));
}

#[test]
fn scoped_at_uses_the_environments_close_interval() {
    let env = TestEnv::new().with_ledger_close_interval(2);
    env.advance(Duration::from_secs(20)); // (20, 10)

    // Two seconds per ledger: timestamp 6 is three ledgers.
    assert_eq!(env.at(6, || env.sequence()), 3);

    let default = TestEnv::new();
    assert_eq!(
        default.ledger_close_interval(),
        LEDGER_CLOSE_TIME_SECS,
        "the default interval stays documented"
    );
}

#[test]
fn checkpoint_restore_rewinds_the_clock_to_the_saved_position() {
    let env = TestEnv::new();
    env.advance(Duration::from_secs(100)); // (100, 20)
    let checkpoint = env.checkpoint();
    env.advance(Duration::from_secs(50));
    assert_eq!(clock(&env), (150, 30));

    env.restore_checkpoint(checkpoint.clone());

    assert_eq!(clock(&env), (100, 20));
    assert_eq!((checkpoint.timestamp(), checkpoint.sequence()), (100, 20));
}

#[test]
fn a_checkpoint_can_move_the_clock_backwards_and_be_restored_repeatedly() {
    let env = TestEnv::new();
    env.advance(Duration::from_secs(10)); // (10, 2)
    let checkpoint = env.checkpoint();

    env.advance(Duration::from_secs(10));
    env.restore_checkpoint(checkpoint.clone());
    assert_eq!(clock(&env), (10, 2));

    env.advance(Duration::from_secs(20));
    assert_eq!(clock(&env), (30, 6));

    env.restore_checkpoint(checkpoint);
    assert_eq!(
        clock(&env),
        (10, 2),
        "a checkpoint is a plain value and must be restorable more than once"
    );
}

#[test]
fn restore_checkpoint_rolls_back_ledger_fields_changed_directly() {
    let env = TestEnv::new();
    let checkpoint = env.checkpoint();
    let before = env.env().ledger().get();

    env.env().ledger().set(soroban_sdk::testutils::LedgerInfo {
        base_reserve: before.base_reserve + 1,
        ..env.env().ledger().get()
    });
    env.restore_checkpoint(checkpoint);

    assert_eq!(env.env().ledger().get().base_reserve, before.base_reserve);
}

#[test]
fn a_checkpoint_taken_inside_a_scope_restores_the_scoped_position() {
    let env = TestEnv::new();
    env.advance(Duration::from_secs(100)); // (100, 20)

    let inside = env.at(40, || env.checkpoint());
    assert_eq!(clock(&env), (100, 20), "the scope must restore the pre-scope clock");

    env.restore_checkpoint(inside);
    assert_eq!(clock(&env), (40, 8));
}

#[test]
fn a_checkpoint_of_a_seeded_environment_restores_the_seeded_position() {
    let env = TestEnv::with_ledger_defaults(
        LedgerDefaults::new()
            .timestamp(1_700_000_000)
            .sequence_number(1_000),
    );
    let checkpoint = env.checkpoint();

    env.advance(Duration::from_secs(60));
    env.restore_checkpoint(checkpoint);

    assert_eq!(clock(&env), (1_700_000_000, 1_000));
}

#[test]
fn protocol_version_is_the_sdk_default_when_no_fixture_sets_it() {
    let sdk = soroban_sdk::Env::default().ledger().get().protocol_version;

    let env = TestEnv::new();

    assert_eq!(env.env().ledger().get().protocol_version, sdk);
    assert_eq!(env.metadata().protocol_version, sdk);
}

#[test]
fn protocol_version_from_defaults_reaches_the_ledger_and_metadata() {
    let current = soroban_sdk::Env::default().ledger().get().protocol_version;
    let defaults = LedgerDefaults::new().protocol_version(current);
    assert_eq!(defaults.protocol_version_override(), Some(current));

    let env = TestEnv::with_ledger_defaults(defaults);

    assert_eq!(env.env().ledger().get().protocol_version, current);
    assert_eq!(env.metadata().protocol_version, current);
}

#[test]
fn protocol_version_fixture_survives_scopes_checkpoints_and_reset() {
    let current = soroban_sdk::Env::default().ledger().get().protocol_version;
    let mut env = TestEnv::with_ledger_defaults(
        LedgerDefaults::new()
            .timestamp(1_000)
            .protocol_version(current),
    );
    let checkpoint = env.checkpoint();

    env.at(500, || {
        assert_eq!(env.env().ledger().get().protocol_version, current);
    });
    env.advance(Duration::from_secs(30));
    env.restore_checkpoint(checkpoint);
    assert_eq!(env.env().ledger().get().protocol_version, current);

    assert_eq!(
        env.clone_config().env().ledger().get().protocol_version,
        current,
        "clone_config must carry the protocol-version fixture"
    );

    env.reset();
    assert_eq!(env.env().ledger().get().protocol_version, current);
}
