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
}

/// Errors this contract returns from its `try_*` entry points.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum VaultError {
    NotInitialized = 1,
    InsufficientBalance = 2,
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
}
