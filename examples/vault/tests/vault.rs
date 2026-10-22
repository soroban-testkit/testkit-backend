#![cfg(feature = "testutils")]

use soroban_sdk::{testutils::Address as _, Address, Env};
use vault::{Vault, VaultClient, VaultError};

#[test]
fn vault_initialization_and_deposit() {
    let env = Env::default();
    let contract_id = env.register(Vault, ());
    let client = VaultClient::new(&env, &contract_id);

    let owner = Address::generate(&env);
    client.initialize(&owner);

    assert_eq!(client.owner(), owner);
    assert_eq!(client.balance(), 0);

    client.deposit(&1_000);
    assert_eq!(client.balance(), 1_000);
}

#[test]
fn vault_withdraw_and_checked_withdraw() {
    let env = Env::default();
    let contract_id = env.register(Vault, ());
    let client = VaultClient::new(&env, &contract_id);

    let owner = Address::generate(&env);
    client.initialize(&owner);
    client.deposit(&500);

    // withdraw without auth works (demonstrating deliberately buggy behavior)
    client.withdraw(&200);
    assert_eq!(client.balance(), 300);

    // withdraw_checked with mock_all_auths works
    env.mock_all_auths();
    client.withdraw_checked(&100);
    assert_eq!(client.balance(), 200);

    // withdraw more than balance returns InsufficientBalance
    let res = client.try_withdraw(&500);
    assert_eq!(res, Err(Ok(VaultError::InsufficientBalance)));
}

#[test]
fn vault_record_and_temp_storage() {
    let env = Env::default();
    let contract_id = env.register(Vault, ());
    let client = VaultClient::new(&env, &contract_id);

    client.set_record(&42);
    assert_eq!(client.touch_record(), 42);
    assert_eq!(client.touch_record_checked(), 42);

    client.set_temp(&99);
    assert_eq!(client.read_temp_unchecked(), 99);
    assert_eq!(client.read_temp_checked(), 99);
}

#[test]
fn vault_batch_payout() {
    let env = Env::default();
    let contract_id = env.register(Vault, ());
    let client = VaultClient::new(&env, &contract_id);

    let addr1 = Address::generate(&env);
    let addr2 = Address::generate(&env);
    let recipients = soroban_sdk::vec![&env, addr1, addr2];

    let count = client.batch_payout(&recipients, &100);
    assert_eq!(count, 2);
}

// ── Cross-contract authorization (#415) ─────────────────────────────────────
//
// These tests exercise the multi-contract auth chain:
//   user -> Vault::delegate_payout -> Treasury::payout
// where each link calls `require_auth`, and Soroban's auth framework
// verifies the whole chain in one transaction.

#[test]
fn delegate_payout_succeeds_when_both_auth_links_are_mocked() {
    let env = Env::default();

    // Register both contracts in the same env so their addresses are valid
    // against each other.
    let vault_id = env.register(Vault, ());
    let treasury_id = env.register(Treasury, ());
    let vault_client = VaultClient::new(&env, &vault_id);

    let owner = Address::generate(&env);
    let caller = Address::generate(&env);
    let recipient = Address::generate(&env);

    vault_client.initialize(&owner);
    vault_client.set_treasury(&treasury_id);

    // mock_all_auths covers both require_auth calls in the chain:
    // caller.require_auth() in delegate_payout and
    // vault.require_auth() in Treasury::payout.
    env.mock_all_auths();
    vault_client.delegate_payout(&caller, &recipient, &500);
}

#[test]
fn set_treasury_requires_owner_auth() {
    let env = Env::default();
    let vault_id = env.register(Vault, ());
    let vault_client = VaultClient::new(&env, &vault_id);

    let owner = Address::generate(&env);
    let treasury_id = env.register(Treasury, ());

    vault_client.initialize(&owner);

    // Without mocking auth, set_treasury must fail.
    let result = vault_client.try_set_treasury(&treasury_id);
    assert!(result.is_err() || matches!(result, Ok(Err(_))));
}

#[test]
fn delegate_payout_fails_without_treasury_set() {
    let env = Env::default();
    let vault_id = env.register(Vault, ());
    let vault_client = VaultClient::new(&env, &vault_id);

    let owner = Address::generate(&env);
    let caller = Address::generate(&env);
    let recipient = Address::generate(&env);

    vault_client.initialize(&owner);
    // No set_treasury call: TreasuryNotSet is expected.
    env.mock_all_auths();
    let result = vault_client.try_delegate_payout(&caller, &recipient, &100);
    assert!(result.is_err() || matches!(result, Ok(Err(_))));
}
