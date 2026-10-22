//! Regression coverage for the per-storage-kind TTL rules documented in
//! `soroban_testkit::ttl` (temporary entries are deleted on expiry and cannot
//! be recovered; persistent and instance entries are archived and restored;
//! instance storage has a single entry per contract; `expire` ignores the
//! kind and advances past `max_entry_ttl`), plus what `ttl_of` reports for
//! missing keys and unsupported targets, and what `expire`/`simulate_expiry`
//! preserve.

use soroban_sdk::testutils::Ledger as _;
use soroban_sdk::{contract, contractimpl, symbol_short, Address, Env, Symbol};
use soroban_testkit::core::TestEnv;
use soroban_testkit::ttl::StorageKind;

#[contract]
struct Store;

#[contractimpl]
impl Store {
    pub fn set_persistent(env: Env, key: Symbol, value: i128) {
        env.storage().persistent().set(&key, &value);
    }

    pub fn read_persistent(env: Env, key: Symbol) -> Option<i128> {
        env.storage().persistent().get(&key)
    }

    pub fn set_temporary(env: Env, key: Symbol, value: i128) {
        env.storage().temporary().set(&key, &value);
    }

    pub fn read_temporary(env: Env, key: Symbol) -> Option<i128> {
        env.storage().temporary().get(&key)
    }

    pub fn set_instance(env: Env, value: i128) {
        env.storage().instance().set(&symbol_short!("v"), &value);
    }
}

#[contract]
struct SampleStorageContract;

#[contractimpl]
impl SampleStorageContract {
    pub fn write_persistent(env: Env, key: Symbol, value: i128) {
        env.storage().persistent().set(&key, &value);
    }

    pub fn write_temporary(env: Env, key: Symbol, value: i128) {
        env.storage().temporary().set(&key, &value);
    }
}

#[test]
fn temporary_and_persistent_entries_get_their_own_minimum_ttl() {
    let env = TestEnv::new();
    let id = env.env().register(Store, ());
    let client = StoreClient::new(env.env(), &id);
    client.set_persistent(&symbol_short!("p"), &1);
    client.set_temporary(&symbol_short!("t"), &1);

    let info = env.env().ledger().get();
    let persistent = env.ttl_of(&id, StorageKind::Persistent, symbol_short!("p"));
    let temporary = env.ttl_of(&id, StorageKind::Temporary, symbol_short!("t"));

    // Each kind is granted its own minimum on write; the reported remaining
    // TTL is one ledger lower because the writing call consumed a ledger.
    assert_eq!(
        persistent + 1,
        info.min_persistent_entry_ttl.min(info.max_entry_ttl),
        "persistent writes are granted min_persistent_entry_ttl"
    );
    assert_eq!(
        temporary + 1,
        info.min_temp_entry_ttl.min(info.max_entry_ttl),
        "temporary writes are granted min_temp_entry_ttl"
    );
}

#[test]
fn a_temporary_entry_is_absent_after_expiry() {
    let env = TestEnv::new();
    let id = env.env().register(Store, ());
    let client = StoreClient::new(env.env(), &id);
    client.set_temporary(&symbol_short!("t"), &2);

    // Temporary storage is deleted, not archived: after expiry the contract
    // must observe the entry as absent.
    env.assert_temporary_expires(
        &id,
        symbol_short!("t"),
        || client.read_temporary(&symbol_short!("t")),
        None,
    );
}

#[test]
fn a_persistent_entry_recovers_after_expiry() {
    let env = TestEnv::new();
    let id = env.env().register(Store, ());
    let client = StoreClient::new(env.env(), &id);
    client.set_persistent(&symbol_short!("p"), &7);

    // Persistent storage is archived, not deleted: reading it after expiry
    // restores it, so the contract still sees the value.
    env.assert_recovers_after_expiry(
        &id,
        symbol_short!("p"),
        || client.read_persistent(&symbol_short!("p")).unwrap_or(0),
        7,
    );
}

#[test]
fn ttl_of_ignores_the_key_for_instance_storage() {
    let env = TestEnv::new();
    let id = env.env().register(Store, ());
    let client = StoreClient::new(env.env(), &id);
    client.set_instance(&5);

    // There is one instance entry per contract, so every key resolves to the
    // same live TTL.
    let with_symbol = env.ttl_of(&id, StorageKind::Instance, symbol_short!("a"));
    let with_other_symbol = env.ttl_of(&id, StorageKind::Instance, symbol_short!("b"));
    let with_number = env.ttl_of(&id, StorageKind::Instance, 12_345u32);

    assert!(with_symbol > 0, "the instance entry should be live");
    assert_eq!(with_symbol, with_other_symbol);
    assert_eq!(with_symbol, with_number);
}

#[test]
fn expire_ignores_the_kind_and_advances_past_max_entry_ttl() {
    let env = TestEnv::new();
    let max_entry_ttl = env.env().ledger().get().max_entry_ttl;
    let start = env.env().ledger().sequence();

    env.expire(StorageKind::Temporary);

    // The `kind` argument documents intent only; the advance is the same for
    // every kind and guaranteed to clear the longest possible TTL.
    assert_eq!(env.env().ledger().sequence() - start, max_entry_ttl + 1);
}

#[test]
fn distinguish_missing_key_from_unsupported_storage_inspection() {
    let env = TestEnv::new();
    let contract_id = env.env().register(SampleStorageContract, ());
    let client = SampleStorageContractClient::new(env.env(), &contract_id);
    let present_key = symbol_short!("present");
    let missing_key = symbol_short!("missing");

    client.write_persistent(&present_key, &100);
    client.write_temporary(&present_key, &200);

    // 1. Present keys report positive TTL
    assert!(env.ttl_of(&contract_id, StorageKind::Persistent, &present_key) > 0);
    assert!(env.ttl_of(&contract_id, StorageKind::Temporary, &present_key) > 0);

    // 2. Missing key on a valid contract returns AssertionFailed distinguishing missing key
    let err_missing = env
        .try_ttl_of(&contract_id, StorageKind::Persistent, &missing_key)
        .expect_err("missing key must return error");
    assert!(
        err_missing.is_assertion_failed(),
        "missing key should be an AssertionFailed, got: {err_missing}"
    );
    assert!(
        err_missing.message().contains("missing key"),
        "error message should indicate missing key: {err_missing}"
    );

    // Panicking variant for missing key
    let panic_missing = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.ttl_of(&contract_id, StorageKind::Persistent, &missing_key);
    }));
    assert!(panic_missing.is_err());

    // 3. Unsupported storage inspection (e.g. unregistered address) returns Misuse
    let unregistered: Address = env.address();
    let err_unsupported = env
        .try_ttl_of(&unregistered, StorageKind::Persistent, &present_key)
        .expect_err("unsupported target must return error");
    assert!(
        err_unsupported.is_misuse(),
        "unsupported storage inspection should be Misuse, got: {err_unsupported}"
    );
    assert!(
        err_unsupported
            .message()
            .contains("unsupported storage inspection"),
        "error message should indicate unsupported storage inspection: {err_unsupported}"
    );

    // Panicking variant for unsupported storage inspection
    let panic_unsupported = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.ttl_of(&unregistered, StorageKind::Persistent, &present_key);
    }));
    assert!(panic_unsupported.is_err());
}

#[test]
fn expiry_simulation_preserves_unrelated_ledger_settings() {
    let env = TestEnv::new();

    // Configure distinct ledger settings
    let test_protocol = env.env().ledger().get().protocol_version;
    env.env().ledger().set_protocol_version(test_protocol);
    env.env().ledger().set_base_reserve(500);
    env.env().ledger().set_min_persistent_entry_ttl(1_234);
    env.env().ledger().set_max_entry_ttl(456_790);

    let initial = env.env().ledger().get();
    assert_eq!(initial.protocol_version, test_protocol);
    assert_eq!(initial.base_reserve, 500);
    assert_eq!(initial.min_persistent_entry_ttl, 1_234);

    // Call expire() which advances sequence and timestamp
    env.expire(StorageKind::Persistent);

    let after_expire = env.env().ledger().get();
    assert_eq!(
        after_expire.protocol_version, test_protocol,
        "expire() must preserve protocol_version"
    );
    assert_eq!(
        after_expire.base_reserve, 500,
        "expire() must preserve base_reserve"
    );
    assert_eq!(
        after_expire.min_persistent_entry_ttl, 1_234,
        "expire() must preserve min_persistent_entry_ttl"
    );
    assert_eq!(
        after_expire.max_entry_ttl, initial.max_entry_ttl,
        "expire() must preserve max_entry_ttl"
    );

    // Call simulate_expiry() which restores clock and parameters
    let seq_before = env.sequence();
    let time_before = env.now();

    env.simulate_expiry(StorageKind::Persistent, || {
        assert!(env.sequence() > seq_before);
    });

    assert_eq!(env.sequence(), seq_before);
    assert_eq!(env.now(), time_before);

    let final_ledger = env.env().ledger().get();
    assert_eq!(final_ledger.protocol_version, test_protocol);
    assert_eq!(final_ledger.base_reserve, 500);
}

// ──────────────────────────────────────────────────────────────────────────────
// Issue #405 — capability discovery for SDK TTL test support
// ──────────────────────────────────────────────────────────────────────────────

use soroban_testkit::ttl::{
    ARCHIVAL_PROTOCOL_CONTRACT_LIFECYCLE, ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH,
    ARCHIVAL_PROTOCOL_STATE_ARCHIVAL,
};

// ── Positive: capabilities match runtime ledger info ─────────────────────────

#[test]
fn ttl_capabilities_accessors_match_runtime_ledger_info() {
    use soroban_sdk::testutils::Ledger as _;

    let env = TestEnv::new();
    let info = env.env().ledger().get();
    let caps = env.ttl_capabilities();

    assert_eq!(caps.protocol_version(), info.protocol_version);
    assert_eq!(
        caps.min_persistent_entry_ttl(),
        info.min_persistent_entry_ttl
    );
    assert_eq!(caps.min_temp_entry_ttl(), info.min_temp_entry_ttl);
    assert_eq!(caps.max_entry_ttl(), info.max_entry_ttl);
}

#[test]
fn ttl_capabilities_max_ttl_bounds_are_consistent() {
    let caps = TestEnv::new().ttl_capabilities();
    assert!(
        caps.max_entry_ttl() >= caps.min_persistent_entry_ttl(),
        "max_entry_ttl must be >= min_persistent_entry_ttl"
    );
    assert!(
        caps.max_entry_ttl() >= caps.min_temp_entry_ttl(),
        "max_entry_ttl must be >= min_temp_entry_ttl"
    );
}

// ── Positive: per-protocol capability predicates ──────────────────────────────

#[test]
fn at_soroban_launch_core_capabilities_are_available() {
    let env = TestEnv::with_protocol_version(ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH);
    let caps = env.ttl_capabilities();
    assert!(
        caps.ttl_inspection_supported(),
        "ttl_inspection must be supported at launch"
    );
    assert!(
        caps.ttl_extension_supported(),
        "ttl_extension must be supported at launch"
    );
    assert!(
        caps.persistent_archival_supported(),
        "persistent_archival must be supported at launch"
    );
    assert!(
        caps.temporary_deletion_supported(),
        "temporary_deletion must be supported at launch"
    );
    assert!(
        caps.bulk_ttl_bump_supported(),
        "bulk_ttl_bump must be supported at launch"
    );
}

#[test]
fn at_soroban_launch_later_capabilities_are_absent() {
    let env = TestEnv::with_protocol_version(ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH);
    let caps = env.ttl_capabilities();
    assert!(
        !caps.contract_lifecycle_supported(),
        "contract_lifecycle must NOT be supported at the Soroban launch protocol"
    );
    assert!(
        !caps.state_archival_settings_supported(),
        "state_archival_settings must NOT be supported at the Soroban launch protocol"
    );
}

#[test]
fn at_contract_lifecycle_protocol_contract_lifecycle_is_available() {
    let env = TestEnv::with_protocol_version(ARCHIVAL_PROTOCOL_CONTRACT_LIFECYCLE);
    let caps = env.ttl_capabilities();
    assert!(caps.contract_lifecycle_supported());
    // Earlier capabilities must still be present.
    assert!(caps.ttl_inspection_supported());
    assert!(caps.persistent_archival_supported());
    // state_archival_settings is not yet available.
    assert!(!caps.state_archival_settings_supported());
}

#[test]
fn at_state_archival_protocol_all_capabilities_are_available() {
    let env = TestEnv::with_protocol_version(ARCHIVAL_PROTOCOL_STATE_ARCHIVAL);
    let caps = env.ttl_capabilities();
    assert!(caps.ttl_inspection_supported());
    assert!(caps.ttl_extension_supported());
    assert!(caps.persistent_archival_supported());
    assert!(caps.temporary_deletion_supported());
    assert!(caps.contract_lifecycle_supported());
    assert!(caps.state_archival_settings_supported());
    assert!(caps.bulk_ttl_bump_supported());
}

// ── Positive: combination predicates ─────────────────────────────────────────

#[test]
fn full_archival_recipe_supported_at_launch_and_later() {
    for protocol in [
        ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH,
        ARCHIVAL_PROTOCOL_CONTRACT_LIFECYCLE,
        ARCHIVAL_PROTOCOL_STATE_ARCHIVAL,
    ] {
        let caps = TestEnv::with_protocol_version(protocol).ttl_capabilities();
        assert!(
            caps.full_archival_recipe_supported(),
            "full_archival_recipe must be supported at protocol {protocol}"
        );
    }
}

#[test]
fn temporary_boundary_recipe_supported_at_all_modern_protocols() {
    for protocol in [
        ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH,
        ARCHIVAL_PROTOCOL_CONTRACT_LIFECYCLE,
        ARCHIVAL_PROTOCOL_STATE_ARCHIVAL,
    ] {
        let caps = TestEnv::with_protocol_version(protocol).ttl_capabilities();
        assert!(
            caps.temporary_boundary_recipe_supported(),
            "temporary_boundary_recipe must be supported at protocol {protocol}"
        );
    }
}

// ── Positive: supported / unsupported lists are complementary ────────────────

#[test]
fn supported_and_unsupported_lists_partition_all_known_capabilities() {
    const ALL_CAPS: &[&str] = &[
        "ttl_inspection",
        "ttl_extension",
        "persistent_archival",
        "temporary_deletion",
        "contract_lifecycle",
        "state_archival_settings",
        "bulk_ttl_bump",
    ];
    for protocol in [
        ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH,
        ARCHIVAL_PROTOCOL_CONTRACT_LIFECYCLE,
        ARCHIVAL_PROTOCOL_STATE_ARCHIVAL,
    ] {
        let caps = TestEnv::with_protocol_version(protocol).ttl_capabilities();
        let supported = caps.supported_capabilities();
        let unsupported = caps.unsupported_capabilities();

        for name in ALL_CAPS {
            let in_s = supported.contains(name);
            let in_u = unsupported.contains(name);
            assert!(
                in_s ^ in_u,
                "capability {name:?} must be in exactly one list at protocol {protocol}"
            );
        }
        assert_eq!(
            supported.len() + unsupported.len(),
            ALL_CAPS.len(),
            "lists must cover exactly {len} capabilities at protocol {protocol}",
            len = ALL_CAPS.len()
        );
    }
}

#[test]
fn at_state_archival_unsupported_list_is_empty() {
    let caps = TestEnv::with_protocol_version(ARCHIVAL_PROTOCOL_STATE_ARCHIVAL).ttl_capabilities();
    let missing = caps.unsupported_capabilities();
    assert!(
        missing.is_empty(),
        "unexpected unsupported capabilities: {missing:?}"
    );
}

// ── Positive: assert_full_ttl_support passes at state-archival protocol ───────

#[test]
fn assert_full_ttl_support_passes_at_state_archival_protocol() {
    TestEnv::with_protocol_version(ARCHIVAL_PROTOCOL_STATE_ARCHIVAL).assert_full_ttl_support();
}

// ── Positive: assert_ttl_capabilities for individual names ───────────────────

#[test]
fn assert_ttl_capabilities_passes_for_capabilities_present_at_launch() {
    let env = TestEnv::with_protocol_version(ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH);
    env.assert_ttl_capabilities(&[
        "ttl_inspection",
        "ttl_extension",
        "persistent_archival",
        "temporary_deletion",
        "bulk_ttl_bump",
    ]);
}

#[test]
fn assert_ttl_capabilities_with_empty_slice_always_passes() {
    TestEnv::with_protocol_version(ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH).assert_ttl_capabilities(&[]);
}

// ── Positive: Display and Clone ───────────────────────────────────────────────

#[test]
fn sdk_ttl_capabilities_display_contains_protocol_and_flags() {
    let caps = TestEnv::new().ttl_capabilities();
    let s = caps.to_string();
    assert!(
        s.contains("protocol="),
        "Display must include protocol: {s}"
    );
    assert!(
        s.contains("ttl_inspection="),
        "Display must include ttl_inspection: {s}"
    );
    assert!(
        s.contains("persistent_archival="),
        "Display must include persistent_archival: {s}"
    );
    assert!(
        s.contains("temporary_deletion="),
        "Display must include temporary_deletion: {s}"
    );
    assert!(
        s.contains("bulk_ttl_bump="),
        "Display must include bulk_ttl_bump: {s}"
    );
}

#[test]
fn sdk_ttl_capabilities_clone_equals_original() {
    let caps = TestEnv::new().ttl_capabilities();
    assert_eq!(caps.clone(), caps);
}

#[test]
fn two_snapshots_from_the_same_env_are_equal() {
    let env = TestEnv::new();
    assert_eq!(env.ttl_capabilities(), env.ttl_capabilities());
}

// ── Negative: assert_full_ttl_support fails at launch protocol ───────────────

#[test]
#[should_panic(expected = "does not support the following TTL capabilities")]
fn assert_full_ttl_support_panics_at_soroban_launch_protocol() {
    TestEnv::with_protocol_version(ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH).assert_full_ttl_support();
}

// ── Negative: assert_ttl_capabilities fails for missing capabilities ──────────

#[test]
#[should_panic(expected = "missing required TTL capabilities")]
fn assert_ttl_capabilities_panics_when_contract_lifecycle_is_requested_at_launch() {
    TestEnv::with_protocol_version(ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH)
        .assert_ttl_capabilities(&["contract_lifecycle"]);
}

#[test]
#[should_panic(expected = "missing required TTL capabilities")]
fn assert_ttl_capabilities_panics_when_state_archival_settings_requested_at_launch() {
    TestEnv::with_protocol_version(ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH)
        .assert_ttl_capabilities(&["state_archival_settings"]);
}

// ── Negative: unknown capability name produces Misuse error ──────────────────

#[test]
#[should_panic(expected = "unknown TTL capability name")]
fn assert_ttl_capabilities_panics_on_unknown_name() {
    TestEnv::new().assert_ttl_capabilities(&["not_a_real_capability"]);
}

// ── Edge case: capabilities are stable across ledger advance ──────────────────

#[test]
fn ttl_capabilities_are_stable_across_ledger_advance() {
    let env = TestEnv::new();
    let before = env.ttl_capabilities();
    env.advance_ledgers(1_000);
    let after = env.ttl_capabilities();

    assert_eq!(before.protocol_version(), after.protocol_version());
    assert_eq!(
        before.min_persistent_entry_ttl(),
        after.min_persistent_entry_ttl()
    );
    assert_eq!(before.max_entry_ttl(), after.max_entry_ttl());
    assert_eq!(
        before.ttl_inspection_supported(),
        after.ttl_inspection_supported()
    );
    assert_eq!(
        before.persistent_archival_supported(),
        after.persistent_archival_supported()
    );
}

// ── Edge case: capability checks compose with existing TTL assertions ─────────

#[test]
fn capability_guard_composes_with_assert_bumps_ttl() {
    let env = TestEnv::new();
    let caps = env.ttl_capabilities();

    // Only exercise assert_bumps_ttl when the host supports TTL extension.
    // In practice this is always true against the pinned SDK, but the
    // guard documents the intent and makes the test forward-compatible.
    if !caps.ttl_extension_supported() {
        return; // capability absent — skip, not fail
    }

    let id = env.env().register(Store, ());
    let client = StoreClient::new(env.env(), &id);
    client.set_persistent(&symbol_short!("k"), &1);

    env.assert_bumps_ttl(&id, StorageKind::Persistent, symbol_short!("k"), || {
        // Extend the TTL directly through the storage API.
        env.env().as_contract(&id, || {
            env.env()
                .storage()
                .persistent()
                .extend_ttl(&symbol_short!("k"), 5_000, 10_000);
        });
    });
}

#[test]
fn capability_guard_composes_with_assert_recovers_after_expiry() {
    let env = TestEnv::new();
    let caps = env.ttl_capabilities();

    if !caps.full_archival_recipe_supported() {
        return; // archival not available — skip
    }

    let id = env.env().register(Store, ());
    let client = StoreClient::new(env.env(), &id);
    client.set_persistent(&symbol_short!("p"), &77);

    env.assert_recovers_after_expiry(
        &id,
        symbol_short!("p"),
        || client.read_persistent(&symbol_short!("p")).unwrap_or(0),
        77,
    );
}

#[test]
fn capability_guard_composes_with_assert_temporary_expires() {
    let env = TestEnv::new();
    let caps = env.ttl_capabilities();

    if !caps.temporary_deletion_supported() {
        return; // deletion semantics not available — skip
    }

    let id = env.env().register(Store, ());
    let client = StoreClient::new(env.env(), &id);
    client.set_temporary(&symbol_short!("t"), &42);

    env.assert_temporary_expires(
        &id,
        symbol_short!("t"),
        || client.read_temporary(&symbol_short!("t")),
        None,
    );
}
