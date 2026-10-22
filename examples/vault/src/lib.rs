#![no_std]

use soroban_sdk::{contract, contracterror, contractimpl, contracttype, Address, Env, Vec};

#[contracttype]
pub enum DataKey {
    Owner,
    Balance,
    /// A persistent record whose TTL handling is deliberately buggy in
    /// `touch_record` and fixed in `touch_record_checked` (see below).
    Record,
    /// A temporary record used to demonstrate expiry handling.
    Temp,
    /// The trusted Treasury contract address used for cross-contract
    /// payouts via `delegate_payout`. Set once by the owner.
    Treasury,
}

/// Errors this contract returns from its `try_*` entry points.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum VaultError {
    NotInitialized = 1,
    InsufficientBalance = 2,
    Unauthorized = 3,
    TreasuryNotSet = 4,
}

#[contract]
pub struct Vault;

#[contractimpl]
impl Vault {
    pub fn initialize(env: Env, owner: Address) {
        env.storage().instance().set(&DataKey::Owner, &owner);
        env.storage().instance().set(&DataKey::Balance, &0i128);
    }

    pub fn owner(env: Env) -> Result<Address, VaultError> {
        env.storage()
            .instance()
            .get(&DataKey::Owner)
            .ok_or(VaultError::NotInitialized)
    }

    pub fn balance(env: Env) -> Result<i128, VaultError> {
        env.storage()
            .instance()
            .get(&DataKey::Balance)
            .ok_or(VaultError::NotInitialized)
    }

    pub fn deposit(env: Env, amount: i128) -> Result<(), VaultError> {
        let balance: i128 = env
            .storage()
            .instance()
            .get(&DataKey::Balance)
            .ok_or(VaultError::NotInitialized)?;
        env.storage()
            .instance()
            .set(&DataKey::Balance, &(balance + amount));
        Ok(())
    }

    /// Deliberately missing the owner auth check: any caller can drain the
    /// vault. Kept alongside `withdraw_checked` so soroban-testkit's own
    /// tests can demonstrate `AuthMatrix::assert_enforced` catching a real
    /// missing-auth vulnerability, and passing once it's fixed.
    pub fn withdraw(env: Env, amount: i128) -> Result<(), VaultError> {
        let balance: i128 = env
            .storage()
            .instance()
            .get(&DataKey::Balance)
            .ok_or(VaultError::NotInitialized)?;
        if balance < amount {
            return Err(VaultError::InsufficientBalance);
        }
        env.storage()
            .instance()
            .set(&DataKey::Balance, &(balance - amount));
        Ok(())
    }

    /// The fixed version of `withdraw`: only the owner may withdraw.
    pub fn withdraw_checked(env: Env, amount: i128) -> Result<(), VaultError> {
        let owner: Address = env
            .storage()
            .instance()
            .get(&DataKey::Owner)
            .ok_or(VaultError::NotInitialized)?;
        owner.require_auth();
        Self::withdraw(env, amount)
    }

    pub fn set_record(env: Env, value: i128) {
        env.storage().persistent().set(&DataKey::Record, &value);
    }

    /// Deliberately buggy: reads the record without extending its TTL, so
    /// a long-lived record can silently expire even while still "in use".
    pub fn touch_record(env: Env) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::Record)
            .unwrap_or(0)
    }

    /// The fixed version: extends the TTL on every read, keeping the
    /// record alive for as long as it's actually accessed.
    pub fn touch_record_checked(env: Env) -> i128 {
        let value = env
            .storage()
            .persistent()
            .get(&DataKey::Record)
            .unwrap_or(0);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Record, 5_000, 10_000);
        value
    }

    pub fn set_temp(env: Env, value: i128) {
        env.storage().temporary().set(&DataKey::Temp, &value);
    }

    /// Deliberately buggy: panics if the temporary entry has expired,
    /// instead of handling its absence gracefully.
    pub fn read_temp_unchecked(env: Env) -> i128 {
        env.storage().temporary().get(&DataKey::Temp).unwrap()
    }

    /// The fixed version: treats an expired (missing) entry as `0`
    /// instead of trapping.
    pub fn read_temp_checked(env: Env) -> i128 {
        env.storage().temporary().get(&DataKey::Temp).unwrap_or(0)
    }

    /// Regression fixture for the limits CLI: rejects a count above three
    /// before resource budgets are exhausted.
    pub fn reject_after_three(count: u32) -> Result<u32, VaultError> {
        if count > 3 {
            Err(VaultError::InsufficientBalance)
        } else {
            Ok(count)
        }
    }

    /// Pays `amount` to every address in `recipients`. Used by
    /// `soroban-testkit limits` to demonstrate ramping a batch operation's
    /// recipient count until it exceeds mainnet resource limits.
    pub fn batch_payout(env: Env, recipients: Vec<Address>, amount: i128) -> u32 {
        for recipient in recipients.iter() {
            env.storage().persistent().set(&recipient, &amount);
        }
        recipients.len()
    }

    /// Set the trusted Treasury contract address used by `delegate_payout`.
    /// Only the owner may set it; this is the authorization root for the
    /// cross-contract call chain below.
    pub fn set_treasury(env: Env, treasury: Address) -> Result<(), VaultError> {
        let owner: Address = env
            .storage()
            .instance()
            .get(&DataKey::Owner)
            .ok_or(VaultError::NotInitialized)?;
        owner.require_auth();
        env.storage().instance().set(&DataKey::Treasury, &treasury);
        Ok(())
    }

    /// Return the currently configured Treasury contract address.
    pub fn treasury(env: Env) -> Result<Address, VaultError> {
        env.storage()
            .instance()
            .get(&DataKey::Treasury)
            .ok_or(VaultError::TreasuryNotSet)
    }

    /// Cross-contract payout: the caller authorizes the Vault to spend
    /// `amount` on their behalf, and the Vault forwards the payout
    /// request to its trusted Treasury contract.
    ///
    /// This demonstrates cross-contract authorization propagation:
    /// 1. `caller.require_auth()` — the external caller authorizes the
    ///    Vault to act on their behalf.
    /// 2. The Vault invokes `Treasury::payout`, which itself calls
    ///    `vault.require_auth()` — the Treasury requires the *calling*
    ///    contract (the Vault) to authenticate itself.
    ///
    /// The two `require_auth` calls form a cross-contract auth chain:
    /// user -> Vault -> Treasury. Soroban's auth framework verifies
    /// the whole chain in one transaction, which is the pattern this
    /// example exists to demonstrate.
    pub fn delegate_payout(
        env: Env,
        caller: Address,
        recipient: Address,
        amount: i128,
    ) -> Result<(), VaultError> {
        // The external caller must authorize the Vault to act for them.
        caller.require_auth();

        let treasury_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::Treasury)
            .ok_or(VaultError::TreasuryNotSet)?;

        // Forward the payout to the Treasury contract. The Treasury's
        // own `payout` entry point will call `vault.require_auth()`,
        // forming the second link in the auth chain.
        let treasury_client = TreasuryClient::new(&env, &treasury_addr);
        treasury_client.payout(&env.current_contract_address(), &recipient, &amount);
        Ok(())
    }
}

/// A second contract in the same example crate, used to demonstrate
/// cross-contract authorization.
///
/// `Treasury` is a minimal contract that holds no balance of its own — it
/// is a stand-in for any contract that a Vault might call as part of a
/// payout flow. Its single entry point, `payout`, requires the *calling*
/// contract to authenticate itself via `require_auth`, so a Vault that
/// calls it must be an authorized caller.
#[contract]
pub struct Treasury;

#[contractimpl]
impl Treasury {
    /// Pay `amount` to `recipient` on behalf of `from`.
    ///
    /// `from` is expected to be a contract address (the Vault). The
    /// `from.require_auth()` call is the second link in the
    /// cross-contract auth chain: it proves the calling contract
    /// authorized this invocation, not just that *some* address signed.
    pub fn payout(env: Env, from: Address, recipient: Address, amount: i128) {
        from.require_auth();
        // In a real contract this would transfer a token; here we just
        // record the payout so tests can observe it.
        env.storage()
            .persistent()
            .set(&(from, recipient), &amount);
    }
}
