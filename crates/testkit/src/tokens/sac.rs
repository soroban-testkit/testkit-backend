use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use soroban_sdk::testutils::{MockAuth, MockAuthInvoke};
use soroban_sdk::token::{StellarAssetClient, TokenClient};
use soroban_sdk::{Address, Env, IntoVal};

use crate::core::{TestEnv, TestkitError};

/// The decimal precision every Stellar Asset Contract uses. This is fixed
/// by the protocol (the classic Stellar asset precision), not something a
/// deployer can choose — verified against soroban-sdk 27.0.6:
/// `register_stellar_asset_contract_v2` takes no decimals parameter, and a
/// freshly deployed SAC's own `decimals()` always returns 7.
const SAC_DECIMALS: u32 = 7;

/// A deployed Stellar Asset Contract test double, with one-line mint/balance
/// helpers so tests don't hand-roll SAC setup boilerplate.
///
/// # Administrative authorization
///
/// The double deploys the SAC with a fresh test address as its admin and
/// mocks that admin's authorization for [`TestToken::mint`] /
/// [`TestToken::mint_raw`] only — minting therefore works with no global auth
/// mocking enabled. Every other operation (transfers, burns, approvals) still
/// needs the test to authorize the caller with `mock_all_auths` or a per-call
/// `mock_auths`; see [`crate::tokens`] for the same rule stated module-wide.
///
/// # Ownership and Clone semantics
///
/// `TestToken` represents a stateful test double handle that owns local supply- and
/// holder-tracking metadata ([`TestToken::total_tracked`], [`TestToken::tracked_holders`])
/// alongside the contract address and administrative identity.
///
/// `TestToken` intentionally does **not** implement [`Clone`]:
/// - Cloning the handle would duplicate internal tracking cells ([`std::cell::Cell`] and
///   [`std::cell::RefCell`]) independently, causing supply accounting and tracked holder
///   records to diverge while interacting with the exact same underlying contract on the host.
/// - Ledger state (balances, allowances, authorized mints) lives on the host [`Env`]
///   and is shared by all callers using [`TestToken::address`].
/// - Test fixtures, assertions, and helper functions should borrow `&TestToken`.
///   When distinct tokens are needed, deploy them via [`TestEnv::token`] or [`TestEnv::token_set`].
pub struct TestToken {
    env: Env,
    address: Address,
    admin: Address,
    total_tracked: Cell<i128>,
    holders: RefCell<Vec<Address>>,
    metadata: RefCell<TokenMetadata>,
}

impl TestToken {
    fn new(env: &Env, admin: Address) -> Self {
        let sac = env.register_stellar_asset_contract_v2(admin.clone());
        Self {
            env: env.clone(),
            address: sac.address(),
            admin,
            total_tracked: Cell::new(0),
            holders: RefCell::new(Vec::new()),
            metadata: RefCell::new(TokenMetadata::default()),
        }
    }

    /// The token contract's address.
    pub fn address(&self) -> Address {
        self.address.clone()
    }

    /// The token's decimal precision. Always `7`, fixed by the protocol.
    pub fn decimals(&self) -> u32 {
        SAC_DECIMALS
    }

    /// Mint `amount` **whole units** to `to` (converted internally to base
    /// units, i.e. `amount * 10^decimals`).
    ///
    /// The Stellar Asset Contract admin's authorization is mocked for this
    /// call, so it succeeds with no global auth mocking enabled.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::Misuse`] if the conversion to base
    /// units would overflow `i128`; use [`TestToken::mint_raw`] for amounts
    /// already in base units.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    ///
    /// token.mint(&alice, 100);
    /// token.assert_balance(&alice, 1_000_000_000); // 100 * 10^7
    /// ```
    /// Convert `amount` whole units into base units (stroops) at this token's decimal precision.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// assert_eq!(token.to_base_units(5).unwrap(), 50_000_000);
    /// ```
    pub fn to_base_units(&self, whole: i128) -> Result<i128, TestkitError> {
        whole_to_base_units(whole, self.decimals())
    }

    /// Convert `base` units into whole units at this token's decimal precision.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// assert_eq!(token.to_whole_units(50_000_000).unwrap(), 5);
    /// ```
    pub fn to_whole_units(&self, base: i128) -> Result<i128, TestkitError> {
        base_to_whole_units(base, self.decimals())
    }

    /// Mint `amount` **whole units** to `to` (converted internally to base
    /// units, i.e. `amount * 10^decimals`).
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::Misuse`] if the conversion to base
    /// units would overflow `i128`, or if the contract invocation fails or traps.
    /// Use [`TestToken::mint_raw`] for amounts already in base units, or
    /// [`TestToken::try_mint`] for fallible invocations.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    ///
    /// token.mint(&alice, 100);
    /// token.assert_balance(&alice, 1_000_000_000); // 100 * 10^7
    /// ```
    pub fn mint(&self, to: &Address, amount: i128) {
        let raw = self.to_base_units(amount).unwrap_or_else(|err| {
            panic!(
                "{}",
                TestkitError::Misuse(format!(
                    "mint({amount}) overflows i128 once converted to base units at {} \
                     decimals; use mint_raw for amounts already in base units: {err}",
                    self.decimals()
                ))
            )
        });
        self.mint_raw(to, raw);
    }

    /// Attempt to mint `amount` **whole units** to `to`, returning
    /// `Err(TestkitError)` if conversion overflows or if the mint traps.
    ///
    /// If the mint traps, `total_tracked` remains unchanged and consistent.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    /// assert!(token.try_mint(&alice, 10).is_ok());
    /// ```
    pub fn try_mint(&self, to: &Address, amount: i128) -> Result<(), TestkitError> {
        let raw = self.to_base_units(amount)?;
        self.try_mint_raw(to, raw)
    }

    /// Mint `amount` **base units** (stroops) to `to`, with no unit
    /// conversion.
    ///
    /// The Stellar Asset Contract admin's authorization is mocked for this
    /// call, so it succeeds with no global auth mocking enabled.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError`] if the mint traps or fails.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    ///
    /// token.mint_raw(&alice, 42);
    /// token.assert_balance(&alice, 42);
    /// ```
    pub fn mint_raw(&self, to: &Address, amount: i128) {
        if let Err(err) = self.try_mint_raw(to, amount) {
            panic!("{err}");
        }
    }

    /// Attempt to mint `amount` **base units** (stroops) to `to`.
    ///
    /// If the contract invocation traps or rejects the mint, `total_tracked`
    /// remains consistent (unchanged) and a [`TestkitError`] is returned with
    /// actionable context.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    /// assert!(token.try_mint_raw(&alice, 42).is_ok());
    /// ```
    pub fn try_mint_raw(&self, to: &Address, amount: i128) -> Result<(), TestkitError> {
        if amount < 0 {
            return Err(TestkitError::Misuse(format!(
                "cannot mint negative amount {amount} to {to:?}"
            )));
        }

        let new_total = self
            .total_tracked
            .get()
            .checked_add(amount)
            .ok_or_else(|| {
                TestkitError::Misuse(format!(
                    "minting {amount} base units would overflow tracked supply (current total: {})",
                    self.total_tracked.get()
                ))
            })?;

        let invoke_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            StellarAssetClient::new(&self.env, &self.address)
                .mock_auths(&[MockAuth {
                    address: &self.admin,
                    invoke: &MockAuthInvoke {
                        contract: &self.address,
                        fn_name: "mint",
                        args: (to.clone(), amount).into_val(&self.env),
                        sub_invokes: &[],
                    },
                }])
                .try_mint(to, &amount)
        }));

        match invoke_res {
            Ok(Ok(Ok(()))) => {
                self.total_tracked.set(new_total);
                self.track_holder(to);
                Ok(())
            }
            Ok(Err(err)) => Err(TestkitError::AssertionFailed(format!(
                "mint of {amount} base units to {to:?} failed or trapped: {err:?}"
            ))),
            Ok(Ok(Err(err))) => Err(TestkitError::DecodeFailed(format!(
                "mint response decoding failed: {err:?}"
            ))),
            Err(_) => Err(TestkitError::AssertionFailed(format!(
                "mint of {amount} base units to {to:?} trapped with panic"
            ))),
        }
    }

    /// The balance of `of`, in **base units**.
    pub fn balance(&self, of: &Address) -> i128 {
        TokenClient::new(&self.env, &self.address).balance(of)
    }

    /// Assert that `of`'s balance equals `expected`, in **base units**.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::AssertionFailed`] showing the expected
    /// and actual balances and the token address if they differ.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    ///
    /// token.mint_raw(&alice, 10);
    /// token.assert_balance(&alice, 10);
    /// ```
    pub fn assert_balance(&self, of: &Address, expected: i128) {
        let actual = self.balance(of);
        if actual != expected {
            let token_addr = self.address();
            panic!(
                "{}",
                TestkitError::AssertionFailed(format!(
                    "expected balance of {of:?} for token {token_addr:?} to be {expected}, found {actual}"
                ))
            );
        }
    }

    /// The whole-unit balance of `of`, **truncated**: a fractional part is
    /// silently dropped, so a balance of `1.5` units reads as `1`.
    ///
    /// Use [`TestToken::balance_whole_exact`] when a fractional balance should
    /// be an error instead of being rounded away.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    /// token.mint(&alice, 10);
    /// assert_eq!(token.balance_whole(&alice), 10);
    /// ```
    pub fn balance_whole(&self, of: &Address) -> i128 {
        let raw = self.balance(of);
        self.to_whole_units(raw).unwrap_or_else(|err| {
            panic!("{err}");
        })
    }

    /// Assert that `of`'s whole-unit balance equals `expected_whole`.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::Misuse`] if scaling `expected_whole`
    /// to base units overflows `i128`.
    /// Panics with a [`TestkitError::AssertionFailed`] if balances differ.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    /// token.mint(&alice, 10);
    /// token.assert_balance_whole(&alice, 10);
    /// ```
    pub fn assert_balance_whole(&self, of: &Address, expected_whole: i128) {
        let raw_expected = self.to_base_units(expected_whole).unwrap_or_else(|err| {
            panic!("{err}");
        });
        self.assert_balance(of, raw_expected);
    }

    /// Convert `base` units into whole units at this token's decimal
    /// precision, refusing to drop a fractional part.
    ///
    /// Unlike [`TestToken::to_whole_units`], which truncates, this returns an
    /// error when `base` is not an exact multiple of `10^decimals`.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// assert_eq!(token.to_whole_units_exact(50_000_000).unwrap(), 5);
    /// assert!(token.to_whole_units_exact(50_000_001).is_err());
    /// ```
    pub fn to_whole_units_exact(&self, base: i128) -> Result<i128, TestkitError> {
        base_to_whole_units_exact(base, self.decimals())
    }

    /// The whole-unit balance of `of`, or an error if the balance is not an
    /// exact number of whole units.
    ///
    /// The error says which account was queried, shows the balance both
    /// formatted and in base units, and states the fractional remainder, so a
    /// test that expected a round balance points straight at the stray
    /// base units.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    ///
    /// token.mint(&alice, 10);
    /// assert_eq!(token.try_balance_whole_exact(&alice).unwrap(), 10);
    ///
    /// token.mint_raw(&alice, 1); // one stray base unit
    /// assert!(token.try_balance_whole_exact(&alice).is_err());
    /// ```
    pub fn try_balance_whole_exact(&self, of: &Address) -> Result<i128, TestkitError> {
        let raw = self.balance(of);
        self.to_whole_units_exact(raw)
            .map_err(|err| TestkitError::Context {
                context: format!(
                    "balance of {of:?} is {} ({raw} base units)",
                    self.format_amount(raw)
                ),
                source: Box::new(err),
            })
    }

    /// The whole-unit balance of `of`, panicking if the balance is not an
    /// exact number of whole units.
    ///
    /// This is the checked counterpart of [`TestToken::balance_whole`], which
    /// truncates.
    ///
    /// # Panics
    ///
    /// Panics with the [`TestkitError`] from
    /// [`TestToken::try_balance_whole_exact`] if the balance has a fractional
    /// part.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    ///
    /// token.mint(&alice, 25);
    /// assert_eq!(token.balance_whole_exact(&alice), 25);
    /// ```
    pub fn balance_whole_exact(&self, of: &Address) -> i128 {
        self.try_balance_whole_exact(of).unwrap_or_else(|err| {
            panic!("{err}");
        })
    }

    /// Total **base units** minted through this `TestToken` (via
    /// [`TestToken::mint`] or [`TestToken::mint_raw`]) across every
    /// recipient — for supply-conservation assertions.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// token.mint_raw(&env.address(), 10);
    /// token.mint_raw(&env.address(), 5);
    /// assert_eq!(token.total_tracked(), 15);
    /// ```
    pub fn total_tracked(&self) -> i128 {
        self.total_tracked.get()
    }

    fn track_holder(&self, who: &Address) {
        let mut holders = self.holders.borrow_mut();
        if !holders.contains(who) {
            holders.push(who.clone());
        }
    }

    /// Every address this `TestToken` has successfully minted to, in the
    /// order each was first minted to, without duplicates.
    ///
    /// A failed or rejected mint does not add its recipient. Addresses that
    /// only ever *received a transfer* are not tracked: the list is the set of
    /// holders the testkit funded, the same population [`TestToken::total_tracked`]
    /// sums over.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    /// let bob = env.address();
    ///
    /// token.mint_raw(&alice, 10);
    /// token.mint_raw(&bob, 5);
    /// token.mint_raw(&alice, 1); // already tracked, listed once
    /// assert_eq!(token.tracked_holders(), vec![alice, bob]);
    /// ```
    pub fn tracked_holders(&self) -> Vec<Address> {
        self.holders.borrow().clone()
    }

    /// How many distinct addresses [`TestToken::tracked_holders`] lists.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// assert_eq!(token.tracked_holder_count(), 0);
    ///
    /// token.mint_raw(&env.address(), 10);
    /// assert_eq!(token.tracked_holder_count(), 1);
    /// ```
    pub fn tracked_holder_count(&self) -> usize {
        self.holders.borrow().len()
    }

    /// Whether `who` has been minted to through this `TestToken`.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    /// let bob = env.address();
    ///
    /// token.mint_raw(&alice, 10);
    /// assert!(token.is_tracked_holder(&alice));
    /// assert!(!token.is_tracked_holder(&bob));
    /// ```
    pub fn is_tracked_holder(&self, who: &Address) -> bool {
        self.holders.borrow().contains(who)
    }

    /// The current balance, in **base units**, of every tracked holder, in the
    /// same order as [`TestToken::tracked_holders`].
    ///
    /// Balances are read from the contract now, so they reflect transfers and
    /// burns made since the mint.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    /// let bob = env.address();
    ///
    /// token.mint_raw(&alice, 10);
    /// token.mint_raw(&bob, 5);
    /// assert_eq!(token.tracked_balances(), vec![(alice, 10), (bob, 5)]);
    /// ```
    pub fn tracked_balances(&self) -> Vec<(Address, i128)> {
        self.tracked_holders()
            .into_iter()
            .map(|holder| {
                let balance = self.balance(&holder);
                (holder, balance)
            })
            .collect()
    }

    /// A [`TokenSnapshot`] of every tracked holder's balance plus the tracked
    /// supply, for diffing a whole fixture before and after an operation.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    /// let bob = env.address();
    /// token.mint_raw(&alice, 10);
    /// token.mint_raw(&bob, 5);
    ///
    /// let before = token.snapshot_tracked();
    /// token.mint_raw(&bob, 7);
    /// let diff = before.diff(&token.snapshot_tracked());
    ///
    /// assert_eq!(diff.balance_delta_of(&alice), Some(0));
    /// assert_eq!(diff.balance_delta_of(&bob), Some(7));
    /// assert_eq!(diff.supply_delta(), 7);
    /// ```
    pub fn snapshot_tracked(&self) -> TokenSnapshot {
        let holders = self.tracked_holders();
        self.snapshot_accounts(holders.iter())
    }

    /// This token's display metadata: the name and symbol the testkit
    /// attaches to it. See [`TokenMetadata`] for what that does and does not
    /// affect.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token_with_metadata("US Dollar Coin", "USDC");
    /// assert_eq!(token.metadata().symbol(), "USDC");
    /// ```
    pub fn metadata(&self) -> TokenMetadata {
        self.metadata.borrow().clone()
    }

    /// This token's display name. `"Test Token"` until configured.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// assert_eq!(env.token().name(), "Test Token");
    /// ```
    pub fn name(&self) -> String {
        self.metadata.borrow().name().to_string()
    }

    /// This token's display symbol. `"TEST"` until configured.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// assert_eq!(env.token().symbol(), "TEST");
    /// ```
    pub fn symbol(&self) -> String {
        self.metadata.borrow().symbol().to_string()
    }

    /// Set this token's display name and symbol.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::Misuse`] if `name` is blank or `symbol`
    /// is not 1 to 12 ASCII letters or digits. The previous metadata is kept.
    /// Use [`TestToken::try_set_metadata`] to handle the error instead.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// token.set_metadata("Euro Coin", "EURC");
    /// assert_eq!(token.name(), "Euro Coin");
    /// assert_eq!(token.symbol(), "EURC");
    /// ```
    pub fn set_metadata(&self, name: &str, symbol: &str) {
        if let Err(err) = self.try_set_metadata(name, symbol) {
            panic!("{err}");
        }
    }

    /// Set this token's display name and symbol, returning an error for an
    /// invalid name or symbol and leaving the previous metadata untouched.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// assert!(token.try_set_metadata("Euro Coin", "EURC").is_ok());
    /// assert!(token.try_set_metadata("Euro Coin", "not valid").is_err());
    /// assert_eq!(token.symbol(), "EURC");
    /// ```
    pub fn try_set_metadata(&self, name: &str, symbol: &str) -> Result<(), TestkitError> {
        let metadata = TokenMetadata::new(name, symbol)?;
        *self.metadata.borrow_mut() = metadata;
        Ok(())
    }

    /// Format an amount in **base units** as a human-readable string with this
    /// token's symbol, for failure messages and logs.
    ///
    /// The whole part and the fractional part are computed with integer
    /// arithmetic, so no precision is lost, and trailing fractional zeros are
    /// dropped. A negative amount keeps its sign.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token_with_metadata("US Dollar Coin", "USDC");
    /// assert_eq!(token.format_amount(15_000_000), "1.5 USDC");
    /// assert_eq!(token.format_amount(20_000_000), "2 USDC");
    /// assert_eq!(token.format_amount(1), "0.0000001 USDC");
    /// assert_eq!(token.format_amount(-5_000_000), "-0.5 USDC");
    /// ```
    pub fn format_amount(&self, base: i128) -> String {
        let decimals = self.decimals();
        let scale = 10u128.pow(decimals);
        let magnitude = base.unsigned_abs();
        let whole = magnitude / scale;
        let fraction = magnitude % scale;

        let mut out = String::new();
        if base < 0 {
            out.push('-');
        }
        out.push_str(&whole.to_string());
        if fraction != 0 {
            let digits = format!("{fraction:0width$}", width = decimals as usize);
            out.push('.');
            out.push_str(digits.trim_end_matches('0'));
        }
        format!("{out} {}", self.symbol())
    }

    /// Mint multiple (recipient, amount) allocations in **whole units** for table-driven fixtures.
    ///
    /// Amounts are converted internally to base units (stroops). The Stellar Asset
    /// Contract admin's authorization is mocked for all mints, so this succeeds
    /// with no global auth mocking enabled.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::Misuse`] if any allocation's amount is negative
    /// or if conversion to base units overflows `i128`.
    /// Panics with a [`TestkitError::AssertionFailed`] if any contract invocation traps.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    /// let bob = env.address();
    ///
    /// token.mint_many([(&alice, 10), (&bob, 20)]);
    /// token.assert_balance_whole(&alice, 10);
    /// token.assert_balance_whole(&bob, 20);
    /// ```
    pub fn mint_many<I>(&self, allocations: I)
    where
        I: IntoIterator,
        I::Item: MintAllocation,
    {
        if let Err(err) = self.try_mint_many(allocations) {
            panic!("{err}");
        }
    }

    /// Attempt to mint multiple (recipient, amount) allocations in **whole units**.
    ///
    /// Returns `Err(TestkitError)` if any conversion overflows or if any mint traps.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    ///
    /// assert!(token.try_mint_many([(&alice, 10)]).is_ok());
    /// ```
    pub fn try_mint_many<I>(&self, allocations: I) -> Result<(), TestkitError>
    where
        I: IntoIterator,
        I::Item: MintAllocation,
    {
        let items: Vec<_> = allocations.into_iter().collect();
        let mut raw_items = Vec::with_capacity(items.len());
        for item in &items {
            let amount = item.amount();
            let to = item.recipient();
            if amount < 0 {
                return Err(TestkitError::Misuse(format!(
                    "cannot mint negative amount {amount} to {to:?}"
                )));
            }
            let raw = self.to_base_units(amount).map_err(|err| {
                TestkitError::Misuse(format!(
                    "mint_many({amount}) overflows i128 once converted to base units: {err}"
                ))
            })?;
            raw_items.push((to, raw));
        }
        for (to, raw) in raw_items {
            self.try_mint_raw(to, raw)?;
        }
        Ok(())
    }

    /// Mint multiple (recipient, amount) allocations in **base units** (stroops) with no unit conversion.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError`] if any amount is negative, overflows tracked supply,
    /// or if any invocation traps.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    /// let bob = env.address();
    ///
    /// token.mint_many_raw([(&alice, 100), (&bob, 200)]);
    /// token.assert_balance(&alice, 100);
    /// token.assert_balance(&bob, 200);
    /// ```
    pub fn mint_many_raw<I>(&self, allocations: I)
    where
        I: IntoIterator,
        I::Item: MintAllocation,
    {
        if let Err(err) = self.try_mint_many_raw(allocations) {
            panic!("{err}");
        }
    }

    /// Attempt to mint multiple (recipient, amount) allocations in **base units** (stroops).
    pub fn try_mint_many_raw<I>(&self, allocations: I) -> Result<(), TestkitError>
    where
        I: IntoIterator,
        I::Item: MintAllocation,
    {
        let items: Vec<_> = allocations.into_iter().collect();
        for item in &items {
            let amount = item.amount();
            let to = item.recipient();
            if amount < 0 {
                return Err(TestkitError::Misuse(format!(
                    "cannot mint negative amount {amount} to {to:?}"
                )));
            }
        }
        for item in items {
            self.try_mint_raw(item.recipient(), item.amount())?;
        }
        Ok(())
    }

    /// Assert that running `f` changes the tracked token supply by exactly
    /// `expected_delta` **base units** (stroops) (after minus before).
    ///
    /// The delta is signed: minting increases tracked supply (positive delta).
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::AssertionFailed`] showing the before
    /// and after supplies and the actual delta if it differs from `expected_delta`.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    ///
    /// token.assert_supply_delta(100, || {
    ///     token.mint_raw(&alice, 100);
    /// });
    /// ```
    pub fn assert_supply_delta(&self, expected_delta: i128, f: impl FnOnce()) {
        let before = self.total_tracked();
        f();
        let after = self.total_tracked();
        let delta = after.checked_sub(before).unwrap_or_else(|| {
            panic!(
                "{}",
                TestkitError::AssertionFailed(format!(
                    "tracked supply delta overflowed i128: before={before}, after={after}"
                ))
            );
        });
        if delta != expected_delta {
            panic!(
                "{}",
                TestkitError::AssertionFailed(format!(
                    "expected the call to change tracked token supply by {expected_delta} base units, \
                     but it went from {before} to {after} (delta={delta})"
                ))
            );
        }
    }

    /// Assert that running `f` changes the tracked token supply by exactly
    /// `expected_delta_whole` **whole units** (after minus before).
    ///
    /// Converted internally to base units at this token's decimal precision.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::Misuse`] if scaling `expected_delta_whole`
    /// overflows `i128`.
    /// Panics with a [`TestkitError::AssertionFailed`] if the actual change
    /// differs from `expected_delta_whole`.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    ///
    /// token.assert_supply_delta_whole(5, || {
    ///     token.mint(&alice, 5);
    /// });
    /// ```
    pub fn assert_supply_delta_whole(&self, expected_delta_whole: i128, f: impl FnOnce()) {
        let expected_raw = self
            .to_base_units(expected_delta_whole)
            .unwrap_or_else(|err| {
                panic!("{err}");
            });
        let before = self.total_tracked();
        f();
        let after = self.total_tracked();
        let delta = after.checked_sub(before).unwrap_or_else(|| {
            panic!(
                "{}",
                TestkitError::AssertionFailed(format!(
                    "tracked supply delta overflowed i128: before={before}, after={after}"
                ))
            );
        });
        if delta != expected_raw {
            panic!(
                "{}",
                TestkitError::AssertionFailed(format!(
                    "expected the call to change tracked token supply by {expected_delta_whole} whole units \
                     ({expected_raw} base units), but it went from {before} to {after} (delta={delta})"
                ))
            );
        }
    }

    /// Assert that running `f` changes the balance of `of` by exactly
    /// `expected_delta` **base units** (stroops) (after minus before).
    ///
    /// The delta is signed: receiving tokens yields a positive delta, while
    /// sending, burning, or spending tokens yields a negative delta.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::AssertionFailed`] showing the before
    /// and after balances and the actual delta if it differs from `expected_delta`.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    ///
    /// token.assert_balance_delta(&alice, 50, || {
    ///     token.mint_raw(&alice, 50);
    /// });
    /// ```
    pub fn assert_balance_delta(&self, of: &Address, expected_delta: i128, f: impl FnOnce()) {
        let before = self.balance(of);
        f();
        let after = self.balance(of);
        let delta = after.checked_sub(before).unwrap_or_else(|| {
            panic!(
                "{}",
                TestkitError::AssertionFailed(format!(
                    "balance delta for {of:?} overflowed i128: before={before}, after={after}"
                ))
            );
        });
        if delta != expected_delta {
            let token_addr = self.address();
            panic!(
                "{}",
                TestkitError::AssertionFailed(format!(
                    "expected the call to change balance of {of:?} for token {token_addr:?} by {expected_delta} base units, \
                     but it went from {before} to {after} (delta={delta})"
                ))
            );
        }
    }

    /// Assert that running `f` changes the balance of `of` by exactly
    /// `expected_delta_whole` **whole units** (after minus before).
    ///
    /// Converted internally to base units at this token's decimal precision.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::Misuse`] if scaling `expected_delta_whole`
    /// overflows `i128`.
    /// Panics with a [`TestkitError::AssertionFailed`] if the actual change
    /// differs from `expected_delta_whole`.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    ///
    /// token.assert_balance_delta_whole(&alice, 10, || {
    ///     token.mint(&alice, 10);
    /// });
    /// ```
    pub fn assert_balance_delta_whole(
        &self,
        of: &Address,
        expected_delta_whole: i128,
        f: impl FnOnce(),
    ) {
        let expected_raw = self
            .to_base_units(expected_delta_whole)
            .unwrap_or_else(|err| {
                panic!("{err}");
            });
        let before = self.balance(of);
        f();
        let after = self.balance(of);
        let delta = after.checked_sub(before).unwrap_or_else(|| {
            panic!(
                "{}",
                TestkitError::AssertionFailed(format!(
                    "balance delta for {of:?} overflowed i128: before={before}, after={after}"
                ))
            );
        });
        if delta != expected_raw {
            panic!(
                "{}",
                TestkitError::AssertionFailed(format!(
                    "expected the call to change balance of {of:?} by {expected_delta_whole} whole units \
                     ({expected_raw} base units), but it went from {before} to {after} (delta={delta})"
                ))
            );
        }
    }

    /// Assert that running `f` changes the balances of multiple accounts by their
    /// respective `expected_delta` in **base units** (stroops).
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    /// let bob = env.address();
    ///
    /// token.assert_balances_delta([(&alice, 100), (&bob, 200)], || {
    ///     token.mint_raw(&alice, 100);
    ///     token.mint_raw(&bob, 200);
    /// });
    /// ```
    pub fn assert_balances_delta<I>(&self, expected_deltas: I, f: impl FnOnce())
    where
        I: IntoIterator,
        I::Item: BalanceDeltaAllocation,
    {
        let deltas: Vec<(Address, i128)> = expected_deltas
            .into_iter()
            .map(|item| (item.account().clone(), item.expected_delta()))
            .collect();
        let before: Vec<(Address, i128, i128)> = deltas
            .iter()
            .map(|(addr, expected)| (addr.clone(), self.balance(addr), *expected))
            .collect();
        f();
        for (addr, before_bal, expected_delta) in before {
            let after_bal = self.balance(&addr);
            let delta = after_bal.checked_sub(before_bal).unwrap_or_else(|| {
                panic!(
                    "{}",
                    TestkitError::AssertionFailed(format!(
                        "balance delta for {addr:?} overflowed i128: before={before_bal}, after={after_bal}"
                    ))
                );
            });
            if delta != expected_delta {
                let token_addr = self.address();
                panic!(
                    "{}",
                    TestkitError::AssertionFailed(format!(
                        "expected the call to change balance of {addr:?} for token {token_addr:?} by {expected_delta} base units, \
                         but it went from {before_bal} to {after_bal} (delta={delta})"
                    ))
                );
            }
        }
    }

    /// Assert that running `f` changes the balances of multiple accounts by their
    /// respective `expected_delta_whole` in **whole units**.
    pub fn assert_balances_delta_whole<I>(&self, expected_deltas: I, f: impl FnOnce())
    where
        I: IntoIterator,
        I::Item: BalanceDeltaAllocation,
    {
        let deltas: Vec<(Address, i128)> = expected_deltas
            .into_iter()
            .map(|item| {
                let raw = self
                    .to_base_units(item.expected_delta())
                    .unwrap_or_else(|err| {
                        panic!("{err}");
                    });
                (item.account().clone(), raw)
            })
            .collect();
        self.assert_balances_delta(deltas, f);
    }

    /// Capture a point-in-time [`TokenSnapshot`] for `account`, recording its
    /// balance and the current tracked token supply.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    ///
    /// let snap = token.snapshot(&alice);
    /// assert_eq!(snap.balance(), 0);
    /// assert_eq!(snap.tracked_supply(), 0);
    /// ```
    #[allow(clippy::mutable_key_type)]
    pub fn snapshot(&self, account: &Address) -> TokenSnapshot {
        let bal = self.balance(account);
        let mut balances = BTreeMap::new();
        balances.insert(account.clone(), bal);
        TokenSnapshot {
            token: self.address(),
            account: Some(account.clone()),
            balance: Some(bal),
            tracked_supply: self.total_tracked(),
            balances,
        }
    }

    /// Capture a point-in-time [`TokenSnapshot`] of the token's tracked supply
    /// without a primary account.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    ///
    /// let snap = token.snapshot_supply();
    /// assert_eq!(snap.tracked_supply(), 0);
    /// ```
    pub fn snapshot_supply(&self) -> TokenSnapshot {
        TokenSnapshot {
            token: self.address(),
            account: None,
            balance: None,
            tracked_supply: self.total_tracked(),
            balances: BTreeMap::new(),
        }
    }

    /// Capture a point-in-time [`TokenSnapshot`] across multiple accounts.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    /// let bob = env.address();
    ///
    /// let snap = token.snapshot_accounts([&alice, &bob]);
    /// assert_eq!(snap.balance_of(&alice), Some(0));
    /// assert_eq!(snap.balance_of(&bob), Some(0));
    /// ```
    #[allow(clippy::mutable_key_type)]
    pub fn snapshot_accounts<'a, I>(&self, accounts: I) -> TokenSnapshot
    where
        I: IntoIterator<Item = &'a Address>,
    {
        let mut balances = BTreeMap::new();
        for addr in accounts {
            balances.insert(addr.clone(), self.balance(addr));
        }
        TokenSnapshot {
            token: self.address(),
            account: None,
            balance: None,
            tracked_supply: self.total_tracked(),
            balances,
        }
    }

    /// Assert that `of`'s balance equals `expected` **base units**, naming the
    /// assertion explicitly as a raw-balance check.
    ///
    /// This is a named alias for [`TestToken::assert_balance`] that makes it
    /// unambiguous at the call site that `expected` is in base units (stroops),
    /// not whole units. Use it when the distinction matters for readability —
    /// for example, right next to a [`TestToken::assert_balance_whole`] call.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::AssertionFailed`] showing the expected
    /// and actual balances and the token address if they differ.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    ///
    /// token.mint_raw(&alice, 42);
    /// token.assert_balance_raw(&alice, 42);
    /// ```
    pub fn assert_balance_raw(&self, of: &Address, expected: i128) {
        self.assert_balance(of, expected);
    }

    /// Transfer `amount` **base units** from `from` to `to`, requiring the
    /// caller to have set up authorization (via `mock_all_auths` or
    /// `mock_auths`) before invoking this helper.
    ///
    /// This is a convenience wrapper around the [`TokenClient`] transfer call
    /// that keeps the test body free of client boilerplate. The tracked supply
    /// is not modified by a transfer — only the on-chain balances change.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::AssertionFailed`] if the transfer traps
    /// or is rejected. Use [`TestToken::try_transfer`] for a fallible variant.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    /// let bob = env.address();
    ///
    /// token.mint_raw(&alice, 100);
    /// env.env().mock_all_auths_allowing_non_root_auth();
    /// token.transfer(&alice, &bob, 40);
    /// token.assert_balance(&alice, 60);
    /// token.assert_balance(&bob, 40);
    /// ```
    pub fn transfer(&self, from: &Address, to: &Address, amount: i128) {
        if let Err(err) = self.try_transfer(from, to, amount) {
            panic!("{err}");
        }
    }

    /// Attempt to transfer `amount` **base units** from `from` to `to`.
    ///
    /// Returns `Err(TestkitError)` if the transfer traps or is rejected by
    /// the contract (for example because authorization is missing or the
    /// sender's balance is insufficient).
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    /// let bob = env.address();
    ///
    /// token.mint_raw(&alice, 100);
    /// env.env().mock_all_auths_allowing_non_root_auth();
    /// assert!(token.try_transfer(&alice, &bob, 50).is_ok());
    /// ```
    pub fn try_transfer(
        &self,
        from: &Address,
        to: &Address,
        amount: i128,
    ) -> Result<(), TestkitError> {
        let invoke_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            TokenClient::new(&self.env, &self.address).try_transfer(
                from,
                &soroban_sdk::MuxedAddress::from(to.clone()),
                &amount,
            )
        }));

        match invoke_res {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Err(err)) => Err(TestkitError::AssertionFailed(format!(
                "transfer of {amount} base units from {from:?} to {to:?} failed or trapped: {err:?}"
            ))),
            Ok(Ok(Err(err))) => Err(TestkitError::DecodeFailed(format!(
                "transfer response decoding failed: {err:?}"
            ))),
            Err(_) => Err(TestkitError::AssertionFailed(format!(
                "transfer of {amount} base units from {from:?} to {to:?} trapped with panic"
            ))),
        }
    }

    /// Burn `amount` **base units** from `from`, decreasing that account's
    /// on-chain balance.
    ///
    /// The caller must have set up authorization for the burn (via
    /// `mock_all_auths` or `mock_auths`) before invoking this helper. The
    /// tracked supply managed by this `TestToken` is **not** decremented —
    /// it only records minted supply. If you need to track net supply after
    /// burns, compare [`TestToken::total_tracked`] against the sum of live
    /// balances.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::AssertionFailed`] if the burn traps or
    /// is rejected. Use [`TestToken::try_burn`] for a fallible variant.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    ///
    /// token.mint_raw(&alice, 100);
    /// env.env().mock_all_auths_allowing_non_root_auth();
    /// token.burn(&alice, 30);
    /// token.assert_balance(&alice, 70);
    /// ```
    pub fn burn(&self, from: &Address, amount: i128) {
        if let Err(err) = self.try_burn(from, amount) {
            panic!("{err}");
        }
    }

    /// Attempt to burn `amount` **base units** from `from`.
    ///
    /// Returns `Err(TestkitError)` if the burn traps or is rejected by the
    /// contract (for example because authorization is missing or the balance
    /// is insufficient).
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    ///
    /// token.mint_raw(&alice, 100);
    /// env.env().mock_all_auths_allowing_non_root_auth();
    /// assert!(token.try_burn(&alice, 50).is_ok());
    /// token.assert_balance(&alice, 50);
    /// ```
    pub fn try_burn(&self, from: &Address, amount: i128) -> Result<(), TestkitError> {
        let invoke_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            TokenClient::new(&self.env, &self.address).try_burn(from, &amount)
        }));

        match invoke_res {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Err(err)) => Err(TestkitError::AssertionFailed(format!(
                "burn of {amount} base units from {from:?} failed or trapped: {err:?}"
            ))),
            Ok(Ok(Err(err))) => Err(TestkitError::DecodeFailed(format!(
                "burn response decoding failed: {err:?}"
            ))),
            Err(_) => Err(TestkitError::AssertionFailed(format!(
                "burn of {amount} base units from {from:?} trapped with panic"
            ))),
        }
    }

    /// The current allowance that `spender` may draw from `from`'s balance,
    /// in **base units**.
    ///
    /// Returns `0` when no allowance has been set or when the approval has
    /// expired.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    /// let bob = env.address();
    ///
    /// assert_eq!(token.allowance(&alice, &bob), 0);
    /// ```
    pub fn allowance(&self, from: &Address, spender: &Address) -> i128 {
        TokenClient::new(&self.env, &self.address).allowance(from, spender)
    }

    /// Approve `spender` to draw up to `amount` **base units** from `from`'s
    /// balance, with the approval expiring at ledger sequence `live_until_ledger`.
    ///
    /// The caller must have set up authorization for `from` before invoking
    /// this helper (via `mock_all_auths` or `mock_auths`).
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::AssertionFailed`] if the approval traps
    /// or is rejected. Use [`TestToken::try_approve`] for a fallible variant.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    /// let bob = env.address();
    ///
    /// token.mint_raw(&alice, 200);
    /// env.env().mock_all_auths_allowing_non_root_auth();
    /// token.approve(&alice, &bob, 100, 1_000);
    /// assert_eq!(token.allowance(&alice, &bob), 100);
    /// ```
    pub fn approve(&self, from: &Address, spender: &Address, amount: i128, live_until_ledger: u32) {
        if let Err(err) = self.try_approve(from, spender, amount, live_until_ledger) {
            panic!("{err}");
        }
    }

    /// Attempt to approve `spender` to draw up to `amount` **base units** from
    /// `from`'s balance with an expiry at `live_until_ledger`.
    ///
    /// Returns `Err(TestkitError)` if the approval traps or is rejected.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    /// let bob = env.address();
    ///
    /// token.mint_raw(&alice, 200);
    /// env.env().mock_all_auths_allowing_non_root_auth();
    /// assert!(token.try_approve(&alice, &bob, 50, 1_000).is_ok());
    /// assert_eq!(token.allowance(&alice, &bob), 50);
    /// ```
    pub fn try_approve(
        &self,
        from: &Address,
        spender: &Address,
        amount: i128,
        live_until_ledger: u32,
    ) -> Result<(), TestkitError> {
        let invoke_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            TokenClient::new(&self.env, &self.address).try_approve(
                from,
                spender,
                &amount,
                &live_until_ledger,
            )
        }));

        match invoke_res {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Err(err)) => Err(TestkitError::AssertionFailed(format!(
                "approve({amount} base units from {from:?} to {spender:?}, live_until={live_until_ledger}) failed or trapped: {err:?}"
            ))),
            Ok(Ok(Err(err))) => Err(TestkitError::DecodeFailed(format!(
                "approve response decoding failed: {err:?}"
            ))),
            Err(_) => Err(TestkitError::AssertionFailed(format!(
                "approve({amount} base units from {from:?} to {spender:?}) trapped with panic"
            ))),
        }
    }

    /// Assert that `spender`'s allowance from `from` equals `expected` **base
    /// units**.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::AssertionFailed`] showing the spender,
    /// owner, expected amount, and actual amount if they differ.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// let alice = env.address();
    /// let bob = env.address();
    ///
    /// token.mint_raw(&alice, 200);
    /// env.env().mock_all_auths_allowing_non_root_auth();
    /// token.approve(&alice, &bob, 75, 1_000);
    /// token.assert_allowance(&alice, &bob, 75);
    /// ```
    pub fn assert_allowance(&self, from: &Address, spender: &Address, expected: i128) {
        let actual = self.allowance(from, spender);
        if actual != expected {
            let token_addr = self.address();
            panic!(
                "{}",
                TestkitError::AssertionFailed(format!(
                    "expected allowance of {spender:?} from {from:?} for token {token_addr:?} \
                     to be {expected} base units, found {actual}"
                ))
            );
        }
    }
}

/// A helper trait for table-driven mint fixtures that converts various tuple shapes into an address reference and amount.
pub trait MintAllocation {
    /// The recipient address.
    fn recipient(&self) -> &Address;
    /// The mint amount.
    fn amount(&self) -> i128;
}

impl MintAllocation for (&Address, i128) {
    fn recipient(&self) -> &Address {
        self.0
    }
    fn amount(&self) -> i128 {
        self.1
    }
}

impl MintAllocation for &(&Address, i128) {
    fn recipient(&self) -> &Address {
        self.0
    }
    fn amount(&self) -> i128 {
        self.1
    }
}

impl MintAllocation for (&Address, &i128) {
    fn recipient(&self) -> &Address {
        self.0
    }
    fn amount(&self) -> i128 {
        *self.1
    }
}

impl MintAllocation for &(&Address, &i128) {
    fn recipient(&self) -> &Address {
        self.0
    }
    fn amount(&self) -> i128 {
        *self.1
    }
}

impl MintAllocation for (Address, i128) {
    fn recipient(&self) -> &Address {
        &self.0
    }
    fn amount(&self) -> i128 {
        self.1
    }
}

impl MintAllocation for &(Address, i128) {
    fn recipient(&self) -> &Address {
        &self.0
    }
    fn amount(&self) -> i128 {
        self.1
    }
}

impl MintAllocation for (Address, &i128) {
    fn recipient(&self) -> &Address {
        &self.0
    }
    fn amount(&self) -> i128 {
        *self.1
    }
}

impl MintAllocation for &(Address, &i128) {
    fn recipient(&self) -> &Address {
        &self.0
    }
    fn amount(&self) -> i128 {
        *self.1
    }
}

/// A helper trait for table-driven balance delta assertions.
pub trait BalanceDeltaAllocation {
    /// The account address.
    fn account(&self) -> &Address;
    /// The expected delta.
    fn expected_delta(&self) -> i128;
}

impl BalanceDeltaAllocation for (&Address, i128) {
    fn account(&self) -> &Address {
        self.0
    }
    fn expected_delta(&self) -> i128 {
        self.1
    }
}

impl BalanceDeltaAllocation for &(&Address, i128) {
    fn account(&self) -> &Address {
        self.0
    }
    fn expected_delta(&self) -> i128 {
        self.1
    }
}

impl BalanceDeltaAllocation for (&Address, &i128) {
    fn account(&self) -> &Address {
        self.0
    }
    fn expected_delta(&self) -> i128 {
        *self.1
    }
}

impl BalanceDeltaAllocation for &(&Address, &i128) {
    fn account(&self) -> &Address {
        self.0
    }
    fn expected_delta(&self) -> i128 {
        *self.1
    }
}

impl BalanceDeltaAllocation for (Address, i128) {
    fn account(&self) -> &Address {
        &self.0
    }
    fn expected_delta(&self) -> i128 {
        self.1
    }
}

impl BalanceDeltaAllocation for &(Address, i128) {
    fn account(&self) -> &Address {
        &self.0
    }
    fn expected_delta(&self) -> i128 {
        self.1
    }
}

impl BalanceDeltaAllocation for (Address, &i128) {
    fn account(&self) -> &Address {
        &self.0
    }
    fn expected_delta(&self) -> i128 {
        *self.1
    }
}

impl BalanceDeltaAllocation for &(Address, &i128) {
    fn account(&self) -> &Address {
        &self.0
    }
    fn expected_delta(&self) -> i128 {
        *self.1
    }
}

/// A point-in-time snapshot of token balances and supply, taken with
/// [`TestToken::snapshot`] and compared with [`TokenSnapshot::diff`].
///
/// # Example
///
/// ```
/// use soroban_testkit::core::TestEnv;
///
/// let env = TestEnv::new();
/// let token = env.token();
/// let alice = env.address();
///
/// let before = token.snapshot(&alice);
/// token.mint_raw(&alice, 100);
/// let after = token.snapshot(&alice);
///
/// let diff = before.diff(&after);
/// assert_eq!(diff.balance_delta(), 100);
/// assert_eq!(diff.supply_delta(), 100);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenSnapshot {
    token: Address,
    account: Option<Address>,
    balance: Option<i128>,
    tracked_supply: i128,
    balances: BTreeMap<Address, i128>,
}

impl TokenSnapshot {
    /// The token contract's address.
    pub fn token(&self) -> &Address {
        &self.token
    }

    /// The primary account this snapshot was taken for, if any.
    pub fn account(&self) -> Option<&Address> {
        self.account.as_ref()
    }

    /// The balance of the primary account in **base units** (stroops).
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::Misuse`] if this snapshot was not taken
    /// for a single primary account. Use [`TokenSnapshot::balance_of`] to query
    /// arbitrary accounts.
    pub fn balance(&self) -> i128 {
        self.balance.unwrap_or_else(|| {
            panic!(
                "{}",
                TestkitError::Misuse(
                    "snapshot has no primary account; use balance_of(&address) instead".into()
                )
            );
        })
    }

    /// The whole-unit balance of the primary account (truncated).
    ///
    /// # Errors
    ///
    /// Returns `Err(TestkitError)` if scaling overflows.
    pub fn balance_whole(&self) -> Result<i128, TestkitError> {
        base_to_whole_units(self.balance(), SAC_DECIMALS)
    }

    /// The balance of `account` in **base units** (stroops) recorded in this snapshot.
    pub fn balance_of(&self, account: &Address) -> Option<i128> {
        self.balances.get(account).copied().or_else(|| {
            if self.account.as_ref() == Some(account) {
                self.balance
            } else {
                None
            }
        })
    }

    /// The whole-unit balance of `account` recorded in this snapshot.
    ///
    /// # Errors
    ///
    /// Returns `Err(TestkitError)` if scaling overflows.
    pub fn balance_of_whole(&self, account: &Address) -> Result<Option<i128>, TestkitError> {
        match self.balance_of(account) {
            Some(raw) => base_to_whole_units(raw, SAC_DECIMALS).map(Some),
            None => Ok(None),
        }
    }

    /// Total **base units** minted through the `TestToken` at the time of the snapshot.
    pub fn tracked_supply(&self) -> i128 {
        self.tracked_supply
    }

    /// Total **whole units** minted through the `TestToken` at the time of the snapshot.
    ///
    /// # Errors
    ///
    /// Returns `Err(TestkitError)` if scaling overflows.
    pub fn tracked_supply_whole(&self) -> Result<i128, TestkitError> {
        base_to_whole_units(self.tracked_supply, SAC_DECIMALS)
    }

    /// All account balances captured in this snapshot.
    #[allow(clippy::mutable_key_type)]
    pub fn balances(&self) -> &BTreeMap<Address, i128> {
        &self.balances
    }

    /// Compare this snapshot to a `later` snapshot of the same token, computing the
    /// changes in balances and tracked supply.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::Misuse`] if `later` is from a different token contract
    /// or was taken for a different primary account.
    #[allow(clippy::mutable_key_type)]
    pub fn diff(&self, later: &TokenSnapshot) -> TokenDiff {
        if self.token != later.token {
            panic!(
                "{}",
                TestkitError::Misuse(format!(
                    "cannot diff snapshots from different tokens: {:?} vs {:?}",
                    self.token, later.token
                ))
            );
        }
        if self.account != later.account {
            panic!(
                "{}",
                TestkitError::Misuse(format!(
                    "cannot diff snapshots for different primary accounts: {:?} vs {:?}",
                    self.account, later.account
                ))
            );
        }

        let balance_delta = match (self.balance, later.balance) {
            (Some(b1), Some(b2)) => Some(b2.checked_sub(b1).unwrap_or_else(|| {
                panic!(
                    "{}",
                    TestkitError::AssertionFailed(format!(
                        "primary account balance delta overflowed i128: before={b1}, after={b2}"
                    ))
                );
            })),
            _ => None,
        };

        let supply_delta = later
            .tracked_supply
            .checked_sub(self.tracked_supply)
            .unwrap_or_else(|| {
                panic!(
                    "{}",
                    TestkitError::AssertionFailed(format!(
                        "tracked supply delta overflowed i128: before={}, after={}",
                        self.tracked_supply, later.tracked_supply
                    ))
                );
            });

        let mut balance_deltas = BTreeMap::new();
        for (addr, b1) in &self.balances {
            if let Some(b2) = later.balances.get(addr) {
                balance_deltas.insert(addr.clone(), b2 - b1);
            }
        }
        for (addr, b2) in &later.balances {
            if !self.balances.contains_key(addr) {
                if let Some(b1) = self.balances.get(addr) {
                    balance_deltas.insert(addr.clone(), b2 - b1);
                }
            }
        }

        TokenDiff {
            token: self.token.clone(),
            account: self.account.clone(),
            balance_delta,
            supply_delta,
            balance_deltas,
        }
    }
}

/// The difference between two [`TokenSnapshot`] readings of the same token contract.
///
/// # Example
///
/// ```
/// use soroban_testkit::core::TestEnv;
///
/// let env = TestEnv::new();
/// let token = env.token();
/// let alice = env.address();
///
/// let before = token.snapshot(&alice);
/// token.mint_raw(&alice, 100);
/// let after = token.snapshot(&alice);
///
/// let diff = before.diff(&after);
/// diff.assert_balance_delta(100);
/// diff.assert_supply_delta(100);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenDiff {
    token: Address,
    account: Option<Address>,
    balance_delta: Option<i128>,
    supply_delta: i128,
    balance_deltas: BTreeMap<Address, i128>,
}

impl TokenDiff {
    /// The token contract address.
    pub fn token(&self) -> &Address {
        &self.token
    }

    /// The primary account this diff was computed for, if any.
    pub fn account(&self) -> Option<&Address> {
        self.account.as_ref()
    }

    /// The signed change in balance of the primary account in **base units** (stroops) (after minus before).
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::Misuse`] if the snapshots were not taken for a primary account.
    pub fn balance_delta(&self) -> i128 {
        self.balance_delta.unwrap_or_else(|| {
            panic!(
                "{}",
                TestkitError::Misuse("diff has no primary account balance delta".into())
            );
        })
    }

    /// The signed change in balance of the primary account in **whole units**.
    ///
    /// # Errors
    ///
    /// Returns `Err(TestkitError)` if scaling overflows.
    pub fn balance_delta_whole(&self) -> Result<i128, TestkitError> {
        base_to_whole_units(self.balance_delta(), SAC_DECIMALS)
    }

    /// The signed change in balance of `account` in **base units** (after minus before).
    pub fn balance_delta_of(&self, account: &Address) -> Option<i128> {
        self.balance_deltas.get(account).copied().or_else(|| {
            if self.account.as_ref() == Some(account) {
                self.balance_delta
            } else {
                None
            }
        })
    }

    /// The signed change in balance of `account` in **whole units**.
    ///
    /// # Errors
    ///
    /// Returns `Err(TestkitError)` if scaling overflows.
    pub fn balance_delta_of_whole(&self, account: &Address) -> Result<Option<i128>, TestkitError> {
        match self.balance_delta_of(account) {
            Some(raw) => base_to_whole_units(raw, SAC_DECIMALS).map(Some),
            None => Ok(None),
        }
    }

    /// All individual account balance deltas.
    #[allow(clippy::mutable_key_type)]
    pub fn balance_deltas(&self) -> &BTreeMap<Address, i128> {
        &self.balance_deltas
    }

    /// The signed change in tracked supply in **base units** (after minus before).
    pub fn supply_delta(&self) -> i128 {
        self.supply_delta
    }

    /// The signed change in tracked supply in **whole units**.
    ///
    /// # Errors
    ///
    /// Returns `Err(TestkitError)` if scaling overflows.
    pub fn supply_delta_whole(&self) -> Result<i128, TestkitError> {
        base_to_whole_units(self.supply_delta, SAC_DECIMALS)
    }

    /// Assert that the primary account's balance delta equals `expected_delta` base units.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::AssertionFailed`] if actual delta differs from `expected_delta`.
    pub fn assert_balance_delta(&self, expected_delta: i128) {
        let actual = self.balance_delta();
        if actual != expected_delta {
            panic!(
                "{}",
                TestkitError::AssertionFailed(format!(
                    "expected snapshot balance delta of {:?} to be {expected_delta} base units, found {actual}",
                    self.account
                ))
            );
        }
    }

    /// Assert that the primary account's balance delta equals `expected_delta_whole` whole units.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::Misuse`] if scaling overflows, or [`TestkitError::AssertionFailed`]
    /// if actual delta differs.
    pub fn assert_balance_delta_whole(&self, expected_delta_whole: i128) {
        let expected_raw =
            whole_to_base_units(expected_delta_whole, SAC_DECIMALS).unwrap_or_else(|err| {
                panic!("{err}");
            });
        self.assert_balance_delta(expected_raw);
    }

    /// Assert that the supply delta equals `expected_delta` base units.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::AssertionFailed`] if actual supply delta differs.
    pub fn assert_supply_delta(&self, expected_delta: i128) {
        if self.supply_delta != expected_delta {
            panic!(
                "{}",
                TestkitError::AssertionFailed(format!(
                    "expected snapshot supply delta of {:?} to be {expected_delta} base units, found {}",
                    self.token, self.supply_delta
                ))
            );
        }
    }

    /// Assert that the supply delta equals `expected_delta_whole` whole units.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::Misuse`] if scaling overflows, or [`TestkitError::AssertionFailed`]
    /// if actual supply delta differs.
    pub fn assert_supply_delta_whole(&self, expected_delta_whole: i128) {
        let expected_raw =
            whole_to_base_units(expected_delta_whole, SAC_DECIMALS).unwrap_or_else(|err| {
                panic!("{err}");
            });
        self.assert_supply_delta(expected_raw);
    }
}

/// Convert a whole-unit amount into base units at the given decimal precision without overflowing.
///
/// Returns `Err(TestkitError::Misuse)` with actionable context if `10^decimals` or the
/// multiplication overflows `i128`.
///
/// # Example
///
/// ```
/// use soroban_testkit::tokens::whole_to_base_units;
///
/// let raw = whole_to_base_units(10, 7).unwrap();
/// assert_eq!(raw, 100_000_000);
/// ```
pub fn whole_to_base_units(whole: i128, decimals: u32) -> Result<i128, TestkitError> {
    let scale = 10i128.checked_pow(decimals).ok_or_else(|| {
        TestkitError::Misuse(format!(
            "decimal precision {decimals} is too large: 10^{decimals} overflows i128"
        ))
    })?;
    whole.checked_mul(scale).ok_or_else(|| {
        TestkitError::Misuse(format!(
            "converting {whole} whole units at {decimals} decimals overflows i128 (scale factor: {scale})"
        ))
    })
}

/// Convert a base-unit amount into whole units at the given decimal precision.
///
/// Returns `Err(TestkitError::Misuse)` with actionable context if `10^decimals` overflows `i128`.
///
/// # Example
///
/// ```
/// use soroban_testkit::tokens::base_to_whole_units;
///
/// let whole = base_to_whole_units(100_000_000, 7).unwrap();
/// assert_eq!(whole, 10);
/// ```
pub fn base_to_whole_units(base: i128, decimals: u32) -> Result<i128, TestkitError> {
    let scale = 10i128.checked_pow(decimals).ok_or_else(|| {
        TestkitError::Misuse(format!(
            "decimal precision {decimals} is too large: 10^{decimals} overflows i128"
        ))
    })?;
    Ok(base / scale)
}

/// Convert a base-unit amount into whole units at the given decimal
/// precision, refusing to drop a fractional part.
///
/// [`base_to_whole_units`] truncates; this returns
/// `Err(TestkitError::Misuse)` when `base` is not an exact multiple of
/// `10^decimals`, naming the whole units and the base units left over. It also
/// returns an error if `10^decimals` overflows `i128`.
///
/// # Example
///
/// ```
/// use soroban_testkit::tokens::base_to_whole_units_exact;
///
/// assert_eq!(base_to_whole_units_exact(100_000_000, 7).unwrap(), 10);
/// assert!(base_to_whole_units_exact(100_000_001, 7).is_err());
/// ```
pub fn base_to_whole_units_exact(base: i128, decimals: u32) -> Result<i128, TestkitError> {
    let scale = 10i128.checked_pow(decimals).ok_or_else(|| {
        TestkitError::Misuse(format!(
            "decimal precision {decimals} is too large: 10^{decimals} overflows i128"
        ))
    })?;
    let remainder = base % scale;
    if remainder != 0 {
        return Err(TestkitError::Misuse(format!(
            "{base} base units is not a whole number of units at {decimals} decimals \
             ({} whole units and {remainder} base units left over); use \
             base_to_whole_units to truncate, or keep the amount in base units",
            base / scale
        )));
    }
    Ok(base / scale)
}

/// The longest symbol a Stellar asset code allows.
const MAX_SYMBOL_LEN: usize = 12;

/// Display metadata for a [`TestToken`]: a human-readable name and a
/// ticker-style symbol.
///
/// These are labels the test double carries so failure messages and fixtures
/// can say `1.5 USDC` instead of `15000000`. They are **not** written to the
/// token contract. A Stellar Asset Contract takes its on-chain `name()` and
/// `symbol()` from its underlying asset, which this double does not
/// configure, so a contract under test that reads them through a token client
/// will not see these values.
///
/// The symbol follows the Stellar asset-code rule — 1 to 12 ASCII letters or
/// digits — so any symbol accepted here is one a real asset could carry. The
/// name only has to be non-blank; surrounding whitespace is trimmed.
///
/// Until configured, a token's metadata is the name `"Test Token"` and the
/// symbol `"TEST"`.
///
/// # Example
///
/// ```
/// use soroban_testkit::tokens::TokenMetadata;
///
/// let metadata = TokenMetadata::new("US Dollar Coin", "USDC").unwrap();
/// assert_eq!(metadata.name(), "US Dollar Coin");
/// assert_eq!(metadata.symbol(), "USDC");
///
/// // A symbol must be a valid asset code.
/// assert!(TokenMetadata::new("US Dollar Coin", "US-DOLLAR").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenMetadata {
    name: String,
    symbol: String,
}

impl TokenMetadata {
    /// Validate and build metadata.
    ///
    /// # Errors
    ///
    /// Returns `Err(TestkitError::Misuse)` if `name` is empty or only
    /// whitespace, or if `symbol` is empty, longer than 12 characters, or
    /// contains anything other than ASCII letters and digits.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::tokens::TokenMetadata;
    ///
    /// assert!(TokenMetadata::new("  Euro Coin  ", "EURC").is_ok());
    /// assert!(TokenMetadata::new("   ", "EURC").is_err());
    /// assert!(TokenMetadata::new("Euro Coin", "").is_err());
    /// assert!(TokenMetadata::new("Euro Coin", "ABCDEFGHIJKLM").is_err()); // 13 characters
    /// ```
    pub fn new(name: impl Into<String>, symbol: impl Into<String>) -> Result<Self, TestkitError> {
        let name: String = name.into();
        let name = name.trim().to_string();
        let symbol: String = symbol.into();
        if name.is_empty() {
            return Err(TestkitError::Misuse(
                "token name must not be empty or only whitespace".to_string(),
            ));
        }
        if symbol.is_empty()
            || symbol.len() > MAX_SYMBOL_LEN
            || !symbol.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return Err(TestkitError::Misuse(format!(
                "token symbol {symbol:?} is not a valid Stellar asset code: use 1 to \
                 {MAX_SYMBOL_LEN} ASCII letters or digits"
            )));
        }
        Ok(Self { name, symbol })
    }

    /// The display name.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::tokens::TokenMetadata;
    ///
    /// assert_eq!(TokenMetadata::new("Euro Coin", "EURC").unwrap().name(), "Euro Coin");
    /// ```
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The display symbol.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::tokens::TokenMetadata;
    ///
    /// assert_eq!(TokenMetadata::new("Euro Coin", "EURC").unwrap().symbol(), "EURC");
    /// ```
    pub fn symbol(&self) -> &str {
        &self.symbol
    }
}

impl Default for TokenMetadata {
    fn default() -> Self {
        Self {
            name: "Test Token".to_string(),
            symbol: "TEST".to_string(),
        }
    }
}

/// Several [`TestToken`]s deployed into one test environment and addressed by
/// label, for contracts that handle more than one asset (a DEX pair, a
/// multi-collateral vault, a payment router).
///
/// Build one with [`TestEnv::token_set`]. Each token is independent — its own
/// contract, its own tracked supply and holders — and its name and symbol are
/// set from its label, so amounts in failure messages read `5 USDC` rather
/// than `5 TEST`.
///
/// # Example
///
/// ```
/// use soroban_testkit::core::TestEnv;
///
/// let env = TestEnv::new();
/// let tokens = env.token_set(["USDC", "EURC"]);
/// let alice = env.address();
///
/// tokens.mint_each(&alice, 100);
/// tokens.token("USDC").mint(&alice, 5);
///
/// assert_eq!(tokens.token("USDC").balance_whole(&alice), 105);
/// assert_eq!(tokens.token("EURC").balance_whole(&alice), 100);
/// ```
pub struct TokenSet {
    tokens: Vec<(String, TestToken)>,
}

impl TokenSet {
    /// How many tokens the set holds.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// assert_eq!(env.token_set(["USDC", "EURC"]).len(), 2);
    /// ```
    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    /// Whether the set holds no tokens. A set built by
    /// [`TestEnv::token_set`] never does.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// assert!(!env.token_set(["USDC"]).is_empty());
    /// ```
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// The labels, in the order the set was built.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let tokens = env.token_set(["USDC", "EURC"]);
    /// assert_eq!(tokens.labels(), vec!["USDC", "EURC"]);
    /// ```
    pub fn labels(&self) -> Vec<&str> {
        self.tokens
            .iter()
            .map(|(label, _)| label.as_str())
            .collect()
    }

    /// The token with `label`, if the set has one.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let tokens = env.token_set(["USDC"]);
    /// assert!(tokens.get("USDC").is_some());
    /// assert!(tokens.get("EURC").is_none());
    /// ```
    pub fn get(&self, label: &str) -> Option<&TestToken> {
        self.tokens
            .iter()
            .find(|(candidate, _)| candidate == label)
            .map(|(_, token)| token)
    }

    /// The token with `label`.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::Misuse`] listing the labels the set does
    /// have if `label` is not one of them. Use [`TokenSet::get`] to probe.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let tokens = env.token_set(["USDC", "EURC"]);
    /// assert_eq!(tokens.token("EURC").symbol(), "EURC");
    /// ```
    pub fn token(&self, label: &str) -> &TestToken {
        self.get(label).unwrap_or_else(|| {
            panic!(
                "{}",
                TestkitError::Misuse(format!(
                    "no token labelled {label:?} in this set; known labels: {:?}",
                    self.labels()
                ))
            )
        })
    }

    /// Every `(label, token)` pair, in the order the set was built.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let tokens = env.token_set(["USDC", "EURC"]);
    /// let symbols: Vec<String> = tokens.iter().map(|(_, token)| token.symbol()).collect();
    /// assert_eq!(symbols, vec!["USDC", "EURC"]);
    /// ```
    pub fn iter(&self) -> impl Iterator<Item = (&str, &TestToken)> {
        self.tokens
            .iter()
            .map(|(label, token)| (label.as_str(), token))
    }

    /// Mint `amount` **whole units** of every token in the set to `to`.
    ///
    /// # Panics
    ///
    /// Panics under the same conditions as [`TestToken::mint`].
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let tokens = env.token_set(["USDC", "EURC"]);
    /// let alice = env.address();
    ///
    /// tokens.mint_each(&alice, 10);
    /// assert_eq!(tokens.token("USDC").balance(&alice), 100_000_000);
    /// assert_eq!(tokens.token("EURC").balance(&alice), 100_000_000);
    /// ```
    pub fn mint_each(&self, to: &Address, amount: i128) {
        for (_, token) in &self.tokens {
            token.mint(to, amount);
        }
    }

    /// Mint `amount` **base units** of every token in the set to `to`.
    ///
    /// # Panics
    ///
    /// Panics under the same conditions as [`TestToken::mint_raw`].
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let tokens = env.token_set(["USDC", "EURC"]);
    /// let alice = env.address();
    ///
    /// tokens.mint_each_raw(&alice, 42);
    /// assert_eq!(tokens.token("USDC").balance(&alice), 42);
    /// ```
    pub fn mint_each_raw(&self, to: &Address, amount: i128) {
        for (_, token) in &self.tokens {
            token.mint_raw(to, amount);
        }
    }

    /// `of`'s balance of every token, in **base units**, as `(label, balance)`
    /// pairs in the order the set was built.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let tokens = env.token_set(["USDC", "EURC"]);
    /// let alice = env.address();
    /// tokens.token("USDC").mint_raw(&alice, 7);
    ///
    /// assert_eq!(
    ///     tokens.balances_of(&alice),
    ///     vec![("USDC".to_string(), 7), ("EURC".to_string(), 0)]
    /// );
    /// ```
    pub fn balances_of(&self, of: &Address) -> Vec<(String, i128)> {
        self.tokens
            .iter()
            .map(|(label, token)| (label.clone(), token.balance(of)))
            .collect()
    }

    /// Assert `of`'s balance of each listed token, with each `expected` amount
    /// in **base units**. Tokens not listed are not checked.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::AssertionFailed`] naming the token and
    /// showing the expected and actual balance, formatted and in base units,
    /// for the first mismatch. Panics with a [`TestkitError::Misuse`] if a
    /// label is not in the set.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let tokens = env.token_set(["USDC", "EURC"]);
    /// let alice = env.address();
    /// tokens.token("USDC").mint_raw(&alice, 7);
    ///
    /// tokens.assert_balances(&alice, [("USDC", 7), ("EURC", 0)]);
    /// ```
    pub fn assert_balances<'a, I>(&self, of: &Address, expected: I)
    where
        I: IntoIterator<Item = (&'a str, i128)>,
    {
        for (label, expected_base) in expected {
            let token = self.token(label);
            let actual = token.balance(of);
            if actual != expected_base {
                panic!(
                    "{}",
                    TestkitError::AssertionFailed(format!(
                        "expected {label} balance of {of:?} to be {} ({expected_base} base \
                         units), found {} ({actual} base units)",
                        token.format_amount(expected_base),
                        token.format_amount(actual)
                    ))
                );
            }
        }
    }
}

impl TestEnv {
    /// Deploy a Stellar Asset Contract with the standard 7 decimals, using a
    /// fresh test address as its admin.
    ///
    /// The admin's authorization is mocked for minting, so [`TestToken::mint`]
    /// / [`TestToken::mint_raw`] need no global auth mocking; other operations
    /// still do — see [`crate::tokens`].
    ///
    /// The token admin is an implementation detail of the double and does **not**
    /// advance the `TestEnv` address stream. Calling `token()` before, after, or
    /// between [`TestEnv::address`] / [`TestEnv::addresses`] calls therefore has
    /// no effect on subsequent address generation: the seeded sequence remains
    /// stable regardless of call order.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token();
    /// assert_eq!(token.decimals(), 7);
    /// ```
    pub fn token(&self) -> TestToken {
        // Use the raw SDK generator for the admin address so that calling
        // token() does not advance the TestEnv's own address counter.  The
        // admin is an internal implementation detail of the double; tests
        // should never need to refer to it.  Keeping it off the TestEnv
        // stream means the seeded address sequence stays stable regardless
        // of how many tokens are created and in what order.
        use soroban_sdk::testutils::Address as _;
        let admin = Address::generate(self.env());
        TestToken::new(self.env(), admin)
    }

    /// Deploy a Stellar Asset Contract, asserting it uses `decimals`.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::Misuse`] if `decimals != 7`. Stellar
    /// Asset Contracts fix their decimal precision at 7 (the classic
    /// Stellar asset precision) — verified against soroban-sdk 27.0.6,
    /// where `register_stellar_asset_contract_v2` takes no decimals
    /// parameter. This method exists so a test that expects a specific
    /// precision fails loudly if that ever changes, rather than silently
    /// assuming it.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token_with_decimals(7);
    /// assert_eq!(token.decimals(), 7);
    /// ```
    pub fn token_with_decimals(&self, decimals: u32) -> TestToken {
        if decimals != SAC_DECIMALS {
            panic!(
                "{}",
                TestkitError::Misuse(format!(
                    "token_with_decimals({decimals}): Stellar Asset Contracts fix decimals at \
                     {SAC_DECIMALS} and cannot be deployed with a different precision; use \
                     token() for the standard {SAC_DECIMALS}-decimal token"
                ))
            );
        }
        self.token()
    }

    /// Deploy a token carrying display metadata (see [`TokenMetadata`]).
    ///
    /// The metadata is validated before anything is deployed.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::Misuse`] if `name` is blank or `symbol`
    /// is not 1 to 12 ASCII letters or digits. Use
    /// [`TestEnv::try_token_with_metadata`] to handle the error.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let token = env.token_with_metadata("US Dollar Coin", "USDC");
    /// assert_eq!(token.name(), "US Dollar Coin");
    /// assert_eq!(token.symbol(), "USDC");
    /// ```
    pub fn token_with_metadata(&self, name: &str, symbol: &str) -> TestToken {
        match self.try_token_with_metadata(name, symbol) {
            Ok(token) => token,
            Err(err) => panic!("{err}"),
        }
    }

    /// Deploy a token carrying display metadata, returning an error for an
    /// invalid name or symbol instead of deploying anything.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// assert!(env.try_token_with_metadata("Euro Coin", "EURC").is_ok());
    /// assert!(env.try_token_with_metadata("Euro Coin", "no spaces").is_err());
    /// ```
    pub fn try_token_with_metadata(
        &self,
        name: &str,
        symbol: &str,
    ) -> Result<TestToken, TestkitError> {
        let metadata = TokenMetadata::new(name, symbol)?;
        let token = self.token();
        *token.metadata.borrow_mut() = metadata;
        Ok(token)
    }

    /// Deploy `count` independent tokens with default metadata.
    ///
    /// Use [`TestEnv::token_set`] when the tokens should be addressed by
    /// label. Like [`TestEnv::token`], this does not advance the address
    /// stream.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let tokens = env.tokens(3);
    /// assert_eq!(tokens.len(), 3);
    /// assert_ne!(tokens[0].address(), tokens[1].address());
    /// ```
    pub fn tokens(&self, count: usize) -> Vec<TestToken> {
        (0..count).map(|_| self.token()).collect()
    }

    /// Deploy one token per label and return them as a [`TokenSet`].
    ///
    /// Each label becomes that token's name and symbol, so it must be a valid
    /// asset code: 1 to 12 ASCII letters or digits.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError`] if `labels` is empty, repeats a label,
    /// or contains a label that is not a valid asset code. Nothing is
    /// deployed in that case. Use [`TestEnv::try_token_set`] to handle the
    /// error.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let tokens = env.token_set(["USDC", "EURC"]);
    /// assert_eq!(tokens.token("USDC").symbol(), "USDC");
    /// ```
    pub fn token_set<I, S>(&self, labels: I) -> TokenSet
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        match self.try_token_set(labels) {
            Ok(set) => set,
            Err(err) => panic!("{err}"),
        }
    }

    /// Deploy one token per label, returning an error for an empty,
    /// duplicated or invalid label before anything is deployed.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// assert!(env.try_token_set(["USDC", "EURC"]).is_ok());
    /// assert!(env.try_token_set(["USDC", "USDC"]).is_err());
    /// assert!(env.try_token_set(Vec::<&str>::new()).is_err());
    /// ```
    pub fn try_token_set<I, S>(&self, labels: I) -> Result<TokenSet, TestkitError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let labels: Vec<String> = labels
            .into_iter()
            .map(|label| label.as_ref().to_string())
            .collect();
        if labels.is_empty() {
            return Err(TestkitError::Misuse(
                "a token set needs at least one label".to_string(),
            ));
        }

        let mut metadata = Vec::with_capacity(labels.len());
        for (index, label) in labels.iter().enumerate() {
            if labels[..index].contains(label) {
                return Err(TestkitError::Misuse(format!(
                    "duplicate token label {label:?}: every token in a set needs its own label"
                )));
            }
            let entry = TokenMetadata::new(label.as_str(), label.as_str()).map_err(|err| {
                TestkitError::Context {
                    context: format!("token label {label:?} cannot be used as a name and symbol"),
                    source: Box::new(err),
                }
            })?;
            metadata.push(entry);
        }

        let tokens = labels
            .into_iter()
            .zip(metadata)
            .map(|(label, entry)| {
                let token = self.token();
                *token.metadata.borrow_mut() = entry;
                (label, token)
            })
            .collect();
        Ok(TokenSet { tokens })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{contract, contractimpl};

    #[test]
    fn token_is_immediately_usable_for_transfers() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();
        token.mint_raw(&alice, 100);

        TokenClient::new(env.env(), &token.address())
            .mock_auths(&[MockAuth {
                address: &alice,
                invoke: &MockAuthInvoke {
                    contract: &token.address(),
                    fn_name: "transfer",
                    args: (alice.clone(), bob.clone(), 30_i128).into_val(env.env()),
                    sub_invokes: &[],
                },
            }])
            .transfer(&alice, soroban_sdk::MuxedAddress::from(bob.clone()), &30);

        token.assert_balance(&alice, 70);
        token.assert_balance(&bob, 30);
    }

    #[test]
    fn mint_works_with_no_global_auth_mocking() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();

        // No mock_all_auths/mock_auths anywhere: the double mocks the SAC
        // admin's authorization for mint itself.
        token.mint(&alice, 2);
        token.mint_raw(&alice, 42);

        token.assert_balance(&alice, 2 * 10i128.pow(7) + 42);
    }

    #[test]
    fn transfers_still_require_authorization() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();
        token.mint_raw(&alice, 100);

        // Only minting is mocked; an un-authorized transfer must still fail.
        env.assert_requires_auth(|| {
            TokenClient::new(env.env(), &token.address()).transfer(
                &alice,
                soroban_sdk::MuxedAddress::from(bob.clone()),
                &10,
            );
        });
    }

    #[test]
    fn mint_converts_whole_units_at_seven_decimals() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        token.mint(&alice, 100);
        token.assert_balance(&alice, 1_000_000_000);
    }

    #[test]
    fn mint_and_mint_raw_disagree_by_exactly_ten_pow_decimals() {
        let env = TestEnv::new();
        let token = env.token();
        let a = env.address();
        let b = env.address();

        token.mint(&a, 1);
        token.mint_raw(&b, 1);

        assert_eq!(token.balance(&a), token.balance(&b) * 10i128.pow(7));
    }

    #[test]
    #[should_panic(expected = "token_with_decimals(6)")]
    fn token_with_decimals_rejects_non_seven() {
        let env = TestEnv::new();
        let _ = env.token_with_decimals(6);
    }

    #[test]
    fn total_tracked_sums_every_mint() {
        let env = TestEnv::new();
        let token = env.token();
        token.mint_raw(&env.address(), 10);
        token.mint(&env.address(), 1);
        assert_eq!(token.total_tracked(), 10 + 10i128.pow(7));
    }

    #[contract]
    struct Wallet;

    #[contractimpl]
    impl Wallet {
        pub fn send(env: Env, token: Address, from: Address, to: Address, amount: i128) {
            TokenClient::new(&env, &token).transfer(
                &from,
                soroban_sdk::MuxedAddress::from(to),
                &amount,
            );
        }

        pub fn spend(env: Env, token: Address, from: Address, amount: i128) {
            TokenClient::new(&env, &token).burn(&from, &amount);
        }

        pub fn allow(
            env: Env,
            token: Address,
            from: Address,
            spender: Address,
            amount: i128,
            live_until: u32,
        ) {
            TokenClient::new(&env, &token).approve(&from, &spender, &amount, &live_until);
        }
    }

    #[test]
    fn works_with_a_contract_that_transfers_burns_and_approves() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();
        let spender = env.address();
        token.mint_raw(&alice, 1_000);

        env.env().mock_all_auths_allowing_non_root_auth();
        let wallet_id = env.env().register(Wallet, ());
        let wallet = WalletClient::new(env.env(), &wallet_id);

        wallet.send(&token.address(), &alice, &bob, &300);
        token.assert_balance(&alice, 700);
        token.assert_balance(&bob, 300);

        wallet.spend(&token.address(), &alice, &200);
        token.assert_balance(&alice, 500);
        assert_eq!(
            token.balance(&alice) + token.balance(&bob),
            800,
            "burn should remove the burned amount from the funded accounts' combined balance"
        );

        wallet.allow(&token.address(), &alice, &spender, &50, &1_000);
        let allowance = TokenClient::new(env.env(), &token.address()).allowance(&alice, &spender);
        assert_eq!(allowance, 50);
    }

    // ── Bug #462 regression: token() must not advance the TestEnv address stream ──

    #[test]
    fn token_does_not_consume_a_slot_in_the_address_stream() {
        // Without the fix, creating a token before generating addresses would
        // shift every subsequent address by one slot.
        let with_token_first = TestEnv::with_seed(42);
        let without_token = TestEnv::with_seed(42);

        let _token = with_token_first.token();
        let a1 = with_token_first.address();

        let a2 = without_token.address();

        assert_eq!(
            a1, a2,
            "token() shifted the address stream; the admin must not draw from TestEnv::address()"
        );
    }

    #[test]
    fn address_sequence_is_stable_regardless_of_token_call_position() {
        // token() called before, between, and after address() calls must
        // produce the same sequence a plain env with no token calls would.
        let env = TestEnv::with_seed(99);
        let reference = TestEnv::with_seed(99);

        let _t0 = env.token(); // token before first address
        let a1 = env.address();
        let _t1 = env.token(); // token between addresses
        let a2 = env.address();
        let _t2 = env.token(); // token after second address

        let r1 = reference.address();
        let r2 = reference.address();

        assert_eq!(a1, r1, "first address shifted by leading token() call");
        assert_eq!(a2, r2, "second address shifted by interleaved token() call");
    }

    #[test]
    fn multiple_tokens_do_not_shift_seeded_address_stream() {
        let env = TestEnv::with_seed(7);
        let reference = TestEnv::with_seed(7);

        // Create several tokens then generate a batch.
        let _t1 = env.token();
        let _t2 = env.token();
        let _t3 = env.token();
        let batch = env.addresses(4);

        let ref_batch = reference.addresses(4);

        assert_eq!(batch, ref_batch, "batch shifted by preceding token() calls");
    }

    // ── Table-driven fixture mint_many tests ──

    #[test]
    fn mint_many_funds_multiple_accounts_from_various_fixture_shapes() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();
        let carol = env.address();

        // 1. Array of references [(&Address, i128)]
        token.mint_many([(&alice, 10), (&bob, 20)]);
        token.assert_balance_whole(&alice, 10);
        token.assert_balance_whole(&bob, 20);

        // 2. Slice of references &[(&Address, i128)]
        token.mint_many(&[(&alice, 5), (&carol, 15)]);
        token.assert_balance_whole(&alice, 15);
        token.assert_balance_whole(&carol, 15);

        // 3. Vector of references vec![(&Address, i128)]
        token.mint_many(vec![(&bob, 5), (&carol, 5)]);
        token.assert_balance_whole(&bob, 25);
        token.assert_balance_whole(&carol, 20);

        // 4. Owned addresses [(Address, i128)]
        token.mint_many([(alice.clone(), 1), (bob.clone(), 2)]);
        token.assert_balance_whole(&alice, 16);
        token.assert_balance_whole(&bob, 27);

        // 5. Empty fixture is a no-op
        let total_before = token.total_tracked();
        token.mint_many(&[(&alice, 0)][..0]);
        assert_eq!(token.total_tracked(), total_before);
    }

    #[test]
    fn mint_many_raw_funds_in_base_units() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();

        token.mint_many_raw([(&alice, 100), (&bob, 200)]);
        token.assert_balance(&alice, 100);
        token.assert_balance(&bob, 200);
        assert_eq!(token.total_tracked(), 300);
    }

    #[test]
    fn try_mint_many_rejects_negative_amounts_cleanly() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();

        let err = token
            .try_mint_many([(&alice, 10), (&bob, -5)])
            .expect_err("negative amount should fail");
        assert!(err.is_misuse());
        assert!(err.message().contains("cannot mint negative amount -5"));
        assert_eq!(token.total_tracked(), 0);
    }

    #[test]
    #[should_panic(expected = "cannot mint negative amount -10")]
    fn mint_many_panics_on_negative_amount() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        token.mint_many([(&alice, -10)]);
    }

    #[test]
    #[should_panic(expected = "overflows i128")]
    fn mint_many_panics_on_overflow_amount() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        token.mint_many([(&alice, i128::MAX)]);
    }

    // ── Supply delta assertion tests ──

    #[test]
    fn assert_supply_delta_passes_on_matching_delta() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();

        // Positive delta on mint
        token.assert_supply_delta(100, || {
            token.mint_raw(&alice, 100);
        });

        // Zero delta when no mint occurs
        token.assert_supply_delta(0, || {
            // No minting
        });

        // Multiple mints combined
        token.assert_supply_delta(150, || {
            token.mint_raw(&alice, 50);
            token.mint_raw(&alice, 100);
        });
    }

    #[test]
    fn assert_supply_delta_whole_passes_on_matching_delta() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();

        token.assert_supply_delta_whole(5, || {
            token.mint(&alice, 5);
        });
    }

    #[test]
    #[should_panic(
        expected = "expected the call to change tracked token supply by 50 base units, but it went from 0 to 100 (delta=100)"
    )]
    fn assert_supply_delta_fails_when_delta_differs() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();

        token.assert_supply_delta(50, || {
            token.mint_raw(&alice, 100);
        });
    }

    #[test]
    #[should_panic(
        expected = "expected the call to change tracked token supply by 2 whole units (20000000 base units), but it went from 0 to 10000000 (delta=10000000)"
    )]
    fn assert_supply_delta_whole_fails_when_delta_differs() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();

        token.assert_supply_delta_whole(2, || {
            token.mint(&alice, 1);
        });
    }

    // ── Balance delta assertion tests ──

    #[test]
    fn assert_balance_delta_passes_on_positive_and_negative_changes() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();

        // Positive delta (mint)
        token.assert_balance_delta(&alice, 1_000, || {
            token.mint_raw(&alice, 1_000);
        });

        // Transfer: Alice decreases, Bob increases
        env.env().mock_all_auths_allowing_non_root_auth();
        let wallet_id = env.env().register(Wallet, ());
        let wallet = WalletClient::new(env.env(), &wallet_id);

        token.assert_balance_delta(&alice, -300, || {
            wallet.send(&token.address(), &alice, &bob, &300);
        });
        token.assert_balance(&alice, 700);
        token.assert_balance(&bob, 300);

        // Burn: Alice decreases
        token.assert_balance_delta(&alice, -200, || {
            wallet.spend(&token.address(), &alice, &200);
        });
        token.assert_balance(&alice, 500);

        // Zero delta
        token.assert_balance_delta(&alice, 0, || {
            // no-op
        });
    }

    #[test]
    fn assert_balance_delta_whole_passes() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();

        token.assert_balance_delta_whole(&alice, 10, || {
            token.mint(&alice, 10);
        });
        token.assert_balance_whole(&alice, 10);
    }

    #[test]
    #[should_panic(expected = "expected the call to change balance of")]
    fn assert_balance_delta_fails_on_mismatch() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();

        token.assert_balance_delta(&alice, 50, || {
            token.mint_raw(&alice, 30);
        });
    }

    #[test]
    #[should_panic(expected = "expected the call to change balance of")]
    fn assert_balance_delta_whole_fails_on_mismatch() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();

        token.assert_balance_delta_whole(&alice, 5, || {
            token.mint(&alice, 2);
        });
    }

    #[test]
    fn assert_balances_delta_checks_multiple_accounts() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();
        token.mint_raw(&alice, 500);

        env.env().mock_all_auths_allowing_non_root_auth();
        let wallet_id = env.env().register(Wallet, ());
        let wallet = WalletClient::new(env.env(), &wallet_id);

        // Simultaneous sender decrease and recipient increase
        token.assert_balances_delta([(&alice, -200), (&bob, 200)], || {
            wallet.send(&token.address(), &alice, &bob, &200);
        });
        token.assert_balance(&alice, 300);
        token.assert_balance(&bob, 200);

        // Whole unit multi-balance delta assertion
        token.assert_balances_delta_whole([(&alice, 5), (&bob, 10)], || {
            token.mint(&alice, 5);
            token.mint(&bob, 10);
        });
    }

    #[test]
    #[should_panic(expected = "expected the call to change balance of")]
    fn assert_balances_delta_fails_when_any_account_mismatches() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();

        token.assert_balances_delta([(&alice, 100), (&bob, 200)], || {
            token.mint_raw(&alice, 100);
            token.mint_raw(&bob, 150); // Mismatch!
        });
    }

    // ── Token snapshot and diff tests ──

    #[test]
    fn token_snapshot_and_diff_reports_deltas_and_asserts() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();

        let before = token.snapshot(&alice);
        assert_eq!(before.balance(), 0);
        assert_eq!(before.balance_whole().unwrap(), 0);
        assert_eq!(before.tracked_supply(), 0);
        assert_eq!(before.account(), Some(&alice));
        assert_eq!(before.token(), &token.address());

        token.mint(&alice, 10); // 100_000_000 stroops
        token.mint_raw(&bob, 500);

        let after = token.snapshot(&alice);
        assert_eq!(after.balance(), 100_000_000);
        assert_eq!(after.balance_whole().unwrap(), 10);
        assert_eq!(after.tracked_supply(), 100_000_500);

        let diff = before.diff(&after);
        assert_eq!(diff.token(), &token.address());
        assert_eq!(diff.account(), Some(&alice));
        assert_eq!(diff.balance_delta(), 100_000_000);
        assert_eq!(diff.balance_delta_whole().unwrap(), 10);
        assert_eq!(diff.supply_delta(), 100_000_500);

        diff.assert_balance_delta(100_000_000);
        diff.assert_balance_delta_whole(10);
        diff.assert_supply_delta(100_000_500);
    }

    #[test]
    fn snapshot_supply_and_snapshot_accounts_work() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();

        let snap_supply_before = token.snapshot_supply();
        assert_eq!(snap_supply_before.tracked_supply(), 0);
        assert_eq!(snap_supply_before.account(), None);

        token.mint_many([(&alice, 5), (&bob, 10)]);

        let snap_supply_after = token.snapshot_supply();
        assert_eq!(snap_supply_after.tracked_supply_whole().unwrap(), 15);

        let diff_supply = snap_supply_before.diff(&snap_supply_after);
        diff_supply.assert_supply_delta_whole(15);

        let accounts_snap = token.snapshot_accounts([&alice, &bob]);
        assert_eq!(accounts_snap.balance_of(&alice), Some(50_000_000));
        assert_eq!(accounts_snap.balance_of_whole(&alice).unwrap(), Some(5));
        assert_eq!(accounts_snap.balance_of(&bob), Some(100_000_000));
        assert_eq!(accounts_snap.balance_of_whole(&bob).unwrap(), Some(10));
    }

    #[test]
    #[should_panic(expected = "cannot diff snapshots from different tokens")]
    fn diff_rejects_snapshots_from_different_tokens() {
        let env = TestEnv::new();
        let t1 = env.token();
        let t2 = env.token();
        let alice = env.address();

        let s1 = t1.snapshot(&alice);
        let s2 = t2.snapshot(&alice);
        let _ = s1.diff(&s2);
    }

    #[test]
    #[should_panic(expected = "cannot diff snapshots for different primary accounts")]
    fn diff_rejects_snapshots_from_different_accounts() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();

        let s1 = token.snapshot(&alice);
        let s2 = token.snapshot(&bob);
        let _ = s1.diff(&s2);
    }

    #[test]
    #[should_panic(expected = "snapshot has no primary account")]
    fn balance_panics_on_supply_only_snapshot() {
        let env = TestEnv::new();
        let token = env.token();
        let snap = token.snapshot_supply();
        let _ = snap.balance();
    }

    #[test]
    #[should_panic(expected = "diff has no primary account balance delta")]
    fn balance_delta_panics_on_supply_only_diff() {
        let env = TestEnv::new();
        let token = env.token();
        let s1 = token.snapshot_supply();
        let s2 = token.snapshot_supply();
        let diff = s1.diff(&s2);
        let _ = diff.balance_delta();
    }

    #[test]
    #[should_panic(expected = "expected snapshot balance delta of")]
    fn diff_assert_balance_delta_panics_on_mismatch() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();

        let s1 = token.snapshot(&alice);
        token.mint_raw(&alice, 100);
        let s2 = token.snapshot(&alice);

        s1.diff(&s2).assert_balance_delta(50);
    }

    #[test]
    #[should_panic(expected = "expected snapshot supply delta of")]
    fn diff_assert_supply_delta_panics_on_mismatch() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();

        let s1 = token.snapshot(&alice);
        token.mint_raw(&alice, 100);
        let s2 = token.snapshot(&alice);

        s1.diff(&s2).assert_supply_delta(50);
    }

    // ── Exact whole-unit balances (#138) ──

    #[test]
    fn exact_conversion_accepts_whole_multiples() {
        assert_eq!(base_to_whole_units_exact(0, 7).unwrap(), 0);
        assert_eq!(base_to_whole_units_exact(100_000_000, 7).unwrap(), 10);
        assert_eq!(base_to_whole_units_exact(-20_000_000, 7).unwrap(), -2);
        assert_eq!(base_to_whole_units_exact(5, 0).unwrap(), 5);
    }

    #[test]
    fn exact_conversion_rejects_a_fractional_part_and_says_what_is_left_over() {
        let message = base_to_whole_units_exact(15_000_000, 7)
            .unwrap_err()
            .to_string();
        assert!(message.contains("not a whole number of units"), "{message}");
        assert!(
            message.contains("1 whole units and 5000000 base units left over"),
            "{message}"
        );
    }

    #[test]
    fn exact_conversion_rejects_a_negative_fractional_amount() {
        assert!(base_to_whole_units_exact(-15_000_000, 7).is_err());
        assert!(base_to_whole_units_exact(1, 7).is_err());
    }

    #[test]
    fn exact_conversion_reports_an_overflowing_precision() {
        let message = base_to_whole_units_exact(1, 39).unwrap_err().to_string();
        assert!(message.contains("too large"), "{message}");
    }

    #[test]
    fn token_to_whole_units_exact_matches_the_free_function() {
        let env = TestEnv::new();
        let token = env.token();
        assert_eq!(token.to_whole_units_exact(50_000_000).unwrap(), 5);
        assert!(token.to_whole_units_exact(50_000_001).is_err());
    }

    #[test]
    fn balance_whole_exact_reads_round_balances() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();

        token.mint(&alice, 25);

        assert_eq!(token.balance_whole_exact(&alice), 25);
        assert_eq!(token.try_balance_whole_exact(&alice).unwrap(), 25);
        // An account that never held the token has a round balance of zero.
        assert_eq!(token.balance_whole_exact(&bob), 0);
    }

    #[test]
    fn balance_whole_still_truncates_where_the_exact_query_refuses() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();

        token.mint(&alice, 1);
        token.mint_raw(&alice, 5_000_000); // 1.5 units

        assert_eq!(token.balance_whole(&alice), 1);
        assert!(token.try_balance_whole_exact(&alice).is_err());
    }

    #[test]
    fn exact_balance_error_names_the_balance_and_the_remainder() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        token.mint_raw(&alice, 15_000_000);

        let message = token
            .try_balance_whole_exact(&alice)
            .unwrap_err()
            .to_string();

        assert!(message.contains("1.5 TEST"), "{message}");
        assert!(message.contains("15000000 base units"), "{message}");
        assert!(message.contains("left over"), "{message}");
    }

    #[test]
    #[should_panic(expected = "not a whole number of units")]
    fn balance_whole_exact_panics_on_a_fractional_balance() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        token.mint_raw(&alice, 1);

        let _ = token.balance_whole_exact(&alice);
    }

    // ── Token metadata (#139) ──

    #[test]
    fn metadata_defaults_to_test_token() {
        let env = TestEnv::new();
        let token = env.token();
        assert_eq!(token.name(), "Test Token");
        assert_eq!(token.symbol(), "TEST");
        assert_eq!(token.metadata(), TokenMetadata::default());
    }

    #[test]
    fn token_with_metadata_sets_name_and_symbol() {
        let env = TestEnv::new();
        let token = env.token_with_metadata("US Dollar Coin", "USDC");
        assert_eq!(token.name(), "US Dollar Coin");
        assert_eq!(token.symbol(), "USDC");
        assert_eq!(token.metadata().name(), "US Dollar Coin");
        assert_eq!(token.metadata().symbol(), "USDC");
    }

    #[test]
    fn set_metadata_replaces_the_previous_values() {
        let env = TestEnv::new();
        let token = env.token_with_metadata("US Dollar Coin", "USDC");
        token.set_metadata("Euro Coin", "EURC");
        assert_eq!(token.name(), "Euro Coin");
        assert_eq!(token.symbol(), "EURC");
    }

    #[test]
    fn invalid_metadata_is_rejected_and_keeps_the_previous_values() {
        let env = TestEnv::new();
        let token = env.token_with_metadata("US Dollar Coin", "USDC");

        for (name, symbol) in [
            ("   ", "USDC"),
            ("", "USDC"),
            ("US Dollar Coin", ""),
            ("US Dollar Coin", "ABCDEFGHIJKLM"),
            ("US Dollar Coin", "US-DC"),
            ("US Dollar Coin", "US DC"),
            ("US Dollar Coin", "USDÉ"),
        ] {
            assert!(
                token.try_set_metadata(name, symbol).is_err(),
                "({name:?}, {symbol:?}) should be rejected"
            );
        }

        assert_eq!(token.name(), "US Dollar Coin");
        assert_eq!(token.symbol(), "USDC");
    }

    #[test]
    fn symbol_accepts_the_stellar_asset_code_limits() {
        assert!(TokenMetadata::new("One", "A").is_ok());
        assert!(TokenMetadata::new("Twelve", "ABCDEFGHIJKL").is_ok());
        assert!(TokenMetadata::new("Mixed", "Usd2024").is_ok());
    }

    #[test]
    fn metadata_name_is_trimmed() {
        let metadata = TokenMetadata::new("  Euro Coin  ", "EURC").unwrap();
        assert_eq!(metadata.name(), "Euro Coin");
    }

    #[test]
    fn try_token_with_metadata_rejects_bad_input() {
        let env = TestEnv::new();
        assert!(env.try_token_with_metadata("Euro Coin", "EURC").is_ok());
        assert!(env
            .try_token_with_metadata("Euro Coin", "no spaces")
            .is_err());
        assert!(env.try_token_with_metadata("", "EURC").is_err());
    }

    #[test]
    #[should_panic(expected = "not a valid Stellar asset code")]
    fn token_with_metadata_panics_on_a_bad_symbol() {
        let env = TestEnv::new();
        let _ = env.token_with_metadata("Euro Coin", "EU-RC");
    }

    #[test]
    #[should_panic(expected = "token name must not be empty")]
    fn set_metadata_panics_on_a_blank_name() {
        let env = TestEnv::new();
        env.token().set_metadata("  ", "EURC");
    }

    #[test]
    fn format_amount_prints_whole_and_fractional_parts() {
        let env = TestEnv::new();
        let token = env.token_with_metadata("US Dollar Coin", "USDC");

        assert_eq!(token.format_amount(0), "0 USDC");
        assert_eq!(token.format_amount(10_000_000), "1 USDC");
        assert_eq!(token.format_amount(15_000_000), "1.5 USDC");
        assert_eq!(token.format_amount(12_345_678), "1.2345678 USDC");
        assert_eq!(token.format_amount(1), "0.0000001 USDC");
        assert_eq!(token.format_amount(1_000_000), "0.1 USDC");
        assert_eq!(token.format_amount(-5_000_000), "-0.5 USDC");
        assert_eq!(token.format_amount(-30_000_000), "-3 USDC");
    }

    #[test]
    fn format_amount_uses_the_default_symbol_until_configured() {
        let env = TestEnv::new();
        let token = env.token();
        assert_eq!(token.format_amount(15_000_000), "1.5 TEST");
    }

    #[test]
    fn format_amount_handles_the_extremes_without_overflow() {
        let env = TestEnv::new();
        let token = env.token();
        assert_eq!(
            token.format_amount(i128::MAX),
            "17014118346046923173168730371588.4105727 TEST"
        );
        assert_eq!(
            token.format_amount(i128::MIN),
            "-17014118346046923173168730371588.4105728 TEST"
        );
    }

    #[test]
    fn metadata_does_not_change_balances_or_holder_tracking() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        token.mint_raw(&alice, 10);

        token.set_metadata("Euro Coin", "EURC");

        token.assert_balance(&alice, 10);
        assert_eq!(token.total_tracked(), 10);
        assert_eq!(token.tracked_holders(), vec![alice]);
    }

    // ── Tracked-holder enumeration (#141) ──

    #[test]
    fn tracked_holders_starts_empty() {
        let env = TestEnv::new();
        let token = env.token();
        assert!(token.tracked_holders().is_empty());
        assert_eq!(token.tracked_holder_count(), 0);
        assert!(token.tracked_balances().is_empty());
    }

    #[test]
    fn tracked_holders_lists_minted_addresses_once_in_first_mint_order() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();
        let carol = env.address();

        token.mint_raw(&carol, 1);
        token.mint_raw(&alice, 2);
        token.mint_raw(&carol, 3); // already tracked
        token.mint(&bob, 4);

        assert_eq!(token.tracked_holders(), vec![carol, alice, bob]);
        assert_eq!(token.tracked_holder_count(), 3);
    }

    #[test]
    fn mint_many_tracks_every_recipient() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();

        token.mint_many([(&alice, 1), (&bob, 2)]);

        assert_eq!(token.tracked_holders(), vec![alice, bob]);
    }

    #[test]
    fn a_rejected_mint_does_not_track_its_recipient() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();

        assert!(token.try_mint_raw(&alice, -1).is_err());

        assert!(!token.is_tracked_holder(&alice));
        assert_eq!(token.tracked_holder_count(), 0);
    }

    #[test]
    fn is_tracked_holder_agrees_with_the_list() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();

        token.mint_raw(&alice, 10);

        assert!(token.is_tracked_holder(&alice));
        assert!(!token.is_tracked_holder(&bob));
    }

    #[test]
    fn transfer_recipients_are_not_tracked_holders() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();
        token.mint_raw(&alice, 100);

        TokenClient::new(env.env(), &token.address())
            .mock_auths(&[MockAuth {
                address: &alice,
                invoke: &MockAuthInvoke {
                    contract: &token.address(),
                    fn_name: "transfer",
                    args: (alice.clone(), bob.clone(), 30_i128).into_val(env.env()),
                    sub_invokes: &[],
                },
            }])
            .transfer(&alice, soroban_sdk::MuxedAddress::from(bob.clone()), &30);

        assert!(!token.is_tracked_holder(&bob));
        // Tracked balances are read live, so they reflect the transfer.
        assert_eq!(token.tracked_balances(), vec![(alice, 70)]);
    }

    #[test]
    fn tracked_balances_follow_the_holder_order() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();

        token.mint_raw(&bob, 5);
        token.mint_raw(&alice, 10);

        assert_eq!(token.tracked_balances(), vec![(bob, 5), (alice, 10)]);
    }

    #[test]
    fn snapshot_tracked_covers_every_holder_and_diffs() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();
        token.mint_raw(&alice, 10);
        token.mint_raw(&bob, 5);

        let before = token.snapshot_tracked();
        assert_eq!(before.balance_of(&alice), Some(10));
        assert_eq!(before.balance_of(&bob), Some(5));
        assert_eq!(before.tracked_supply(), 15);

        token.mint_raw(&bob, 7);
        let diff = before.diff(&token.snapshot_tracked());

        assert_eq!(diff.balance_delta_of(&alice), Some(0));
        assert_eq!(diff.balance_delta_of(&bob), Some(7));
        assert_eq!(diff.supply_delta(), 7);
    }

    #[test]
    fn holders_are_tracked_per_token() {
        let env = TestEnv::new();
        let first = env.token();
        let second = env.token();
        let alice = env.address();

        first.mint_raw(&alice, 1);

        assert!(first.is_tracked_holder(&alice));
        assert!(!second.is_tracked_holder(&alice));
    }

    // ── Multiple token assets (#140) ──

    #[test]
    fn tokens_deploys_independent_tokens() {
        let env = TestEnv::new();
        let tokens = env.tokens(3);
        let alice = env.address();

        assert_eq!(tokens.len(), 3);
        assert_ne!(tokens[0].address(), tokens[1].address());
        assert_ne!(tokens[1].address(), tokens[2].address());

        tokens[0].mint_raw(&alice, 9);
        assert_eq!(tokens[0].total_tracked(), 9);
        assert_eq!(tokens[1].total_tracked(), 0);
        tokens[1].assert_balance(&alice, 0);
    }

    #[test]
    fn tokens_zero_is_empty() {
        let env = TestEnv::new();
        assert!(env.tokens(0).is_empty());
    }

    #[test]
    fn tokens_and_token_sets_do_not_shift_the_address_stream() {
        let env = TestEnv::with_seed(5);
        let reference = TestEnv::with_seed(5);

        let _tokens = env.tokens(2);
        let _set = env.token_set(["USDC", "EURC"]);
        let batch = env.addresses(3);

        assert_eq!(batch, reference.addresses(3));
    }

    #[test]
    fn token_set_labels_each_token_and_names_it_after_the_label() {
        let env = TestEnv::new();
        let tokens = env.token_set(["USDC", "EURC"]);

        assert_eq!(tokens.len(), 2);
        assert!(!tokens.is_empty());
        assert_eq!(tokens.labels(), vec!["USDC", "EURC"]);
        assert_eq!(tokens.token("USDC").symbol(), "USDC");
        assert_eq!(tokens.token("USDC").name(), "USDC");
        assert_eq!(tokens.token("EURC").symbol(), "EURC");
        assert_ne!(
            tokens.token("USDC").address(),
            tokens.token("EURC").address()
        );
    }

    #[test]
    fn token_set_get_returns_none_for_an_unknown_label() {
        let env = TestEnv::new();
        let tokens = env.token_set(["USDC"]);

        assert!(tokens.get("USDC").is_some());
        assert!(tokens.get("EURC").is_none());
    }

    #[test]
    #[should_panic(expected = "no token labelled \"DAI\" in this set")]
    fn token_set_token_panics_for_an_unknown_label() {
        let env = TestEnv::new();
        let tokens = env.token_set(["USDC"]);

        let _ = tokens.token("DAI");
    }

    #[test]
    fn token_set_unknown_label_message_lists_the_known_labels() {
        let env = TestEnv::new();
        let tokens = env.token_set(["USDC", "EURC"]);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = tokens.token("DAI");
        }));

        let payload = result.expect_err("an unknown label must panic");
        let message = payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|m| m.to_string()))
            .unwrap_or_default();
        assert!(message.contains("known labels"), "{message}");
        assert!(
            message.contains("USDC") && message.contains("EURC"),
            "{message}"
        );
    }

    #[test]
    fn token_set_rejects_empty_duplicate_and_invalid_labels() {
        let env = TestEnv::new();

        let empty = env
            .try_token_set(Vec::<&str>::new())
            .err()
            .expect("an empty label list is rejected");
        assert!(empty.to_string().contains("at least one label"));

        let duplicate = env
            .try_token_set(["USDC", "EURC", "USDC"])
            .err()
            .expect("a repeated label is rejected");
        assert!(duplicate.to_string().contains("duplicate token label"));

        let invalid = env
            .try_token_set(["USDC", "not a code"])
            .err()
            .expect("a label that is not an asset code is rejected");
        assert!(invalid
            .to_string()
            .contains("cannot be used as a name and symbol"));
    }

    #[test]
    #[should_panic(expected = "duplicate token label")]
    fn token_set_panics_on_a_duplicate_label() {
        let env = TestEnv::new();
        let _ = env.token_set(["USDC", "USDC"]);
    }

    #[test]
    fn token_set_mint_each_funds_every_token() {
        let env = TestEnv::new();
        let tokens = env.token_set(["USDC", "EURC"]);
        let alice = env.address();

        tokens.mint_each(&alice, 10);
        tokens.mint_each_raw(&alice, 3);

        for (_, token) in tokens.iter() {
            token.assert_balance(&alice, 100_000_003);
            assert!(token.is_tracked_holder(&alice));
        }
    }

    #[test]
    fn token_set_tokens_are_independent() {
        let env = TestEnv::new();
        let tokens = env.token_set(["USDC", "EURC"]);
        let alice = env.address();

        tokens.token("USDC").mint_raw(&alice, 50);

        assert_eq!(tokens.token("USDC").total_tracked(), 50);
        assert_eq!(tokens.token("EURC").total_tracked(), 0);
        assert!(tokens.token("EURC").tracked_holders().is_empty());
    }

    #[test]
    fn token_set_balances_of_lists_every_token_in_build_order() {
        let env = TestEnv::new();
        let tokens = env.token_set(["USDC", "EURC"]);
        let alice = env.address();
        tokens.token("EURC").mint_raw(&alice, 8);

        assert_eq!(
            tokens.balances_of(&alice),
            vec![("USDC".to_string(), 0), ("EURC".to_string(), 8)]
        );
    }

    #[test]
    fn token_set_iter_keeps_build_order() {
        let env = TestEnv::new();
        let tokens = env.token_set(["B", "A", "C"]);

        let labels: Vec<&str> = tokens.iter().map(|(label, _)| label).collect();

        assert_eq!(labels, vec!["B", "A", "C"]);
    }

    #[test]
    fn assert_balances_passes_when_every_listed_balance_matches() {
        let env = TestEnv::new();
        let tokens = env.token_set(["USDC", "EURC", "GBPC"]);
        let alice = env.address();
        tokens.token("USDC").mint_raw(&alice, 7);

        // GBPC is not listed, so it is not checked.
        tokens.assert_balances(&alice, [("USDC", 7), ("EURC", 0)]);
    }

    #[test]
    #[should_panic(expected = "expected EURC balance of")]
    fn assert_balances_fails_naming_the_mismatched_token() {
        let env = TestEnv::new();
        let tokens = env.token_set(["USDC", "EURC"]);
        let alice = env.address();
        tokens.token("USDC").mint_raw(&alice, 5);

        tokens.assert_balances(&alice, [("USDC", 5), ("EURC", 9)]);
    }

    #[test]
    #[should_panic(expected = "no token labelled \"DAI\" in this set")]
    fn assert_balances_fails_for_a_label_outside_the_set() {
        let env = TestEnv::new();
        let tokens = env.token_set(["USDC"]);
        let alice = env.address();

        tokens.assert_balances(&alice, [("DAI", 0)]);
    }

 fix/tokens-sac-improvements
    // ── Issue #133: include token address in balance assertion failures ──

    #[test]
    fn assert_balance_failure_message_includes_token_address() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let token_addr = token.address();

        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            token.assert_balance(&alice, 500);
        }));

        let err = res.expect_err("assert_balance should panic on mismatch");
        let msg = if let Some(s) = err.downcast_ref::<String>() {
            s.clone()
        } else if let Some(s) = err.downcast_ref::<&str>() {
            s.to_string()
        } else {
            panic!("unexpected panic payload type");
        };

        assert!(msg.contains(&format!("{alice:?}")));
        assert!(msg.contains(&format!("{token_addr:?}")));
        assert!(msg.contains("expected balance of"));
        assert!(msg.contains("500"));
        assert!(msg.contains("found 0"));
    }

    #[test]
    fn assert_balance_whole_failure_message_includes_token_address() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let token_addr = token.address();

        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            token.assert_balance_whole(&alice, 10);
        }));

        let err = res.expect_err("assert_balance_whole should panic on mismatch");
        let msg = if let Some(s) = err.downcast_ref::<String>() {
            s.clone()
        } else if let Some(s) = err.downcast_ref::<&str>() {
            s.to_string()
        } else {
            panic!("unexpected panic payload type");
        };

        assert!(msg.contains(&format!("{alice:?}")));
        assert!(msg.contains(&format!("{token_addr:?}")));
        assert!(msg.contains("expected balance of"));
    }

    #[test]
    fn assert_balance_delta_failure_message_includes_token_address() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let token_addr = token.address();

        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            token.assert_balance_delta(&alice, 100, || {});
        }));

        let err = res.expect_err("assert_balance_delta should panic on mismatch");
        let msg = if let Some(s) = err.downcast_ref::<String>() {
            s.clone()
        } else if let Some(s) = err.downcast_ref::<&str>() {
            s.to_string()
        } else {
            panic!("unexpected panic payload type");
        };

        assert!(msg.contains(&format!("{alice:?}")));
        assert!(msg.contains(&format!("{token_addr:?}")));
    }

    // ── Issue #132: TestToken clone and ownership semantics ──

    #[test]
    fn test_token_ownership_and_borrowing_semantics() {

    // ── Issue #137: raw-balance assertion naming explicitly ──

    #[test]
    fn assert_balance_raw_passes_on_matching_base_units() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();

        token.mint_raw(&alice, 42);
        token.assert_balance_raw(&alice, 42);
    }

    #[test]
    #[should_panic(expected = "expected balance of")]
    fn assert_balance_raw_fails_on_mismatch() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();

        token.mint_raw(&alice, 10);
        token.assert_balance_raw(&alice, 99);
    }

    #[test]
    fn assert_balance_raw_and_assert_balance_are_equivalent() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();

        token.mint_raw(&alice, 777);
        token.assert_balance(&alice, 777);
        token.assert_balance_raw(&alice, 777);
    }

    // ── Issue #134: tracked transfer helper ──

    #[test]
    fn transfer_moves_balance_between_accounts() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();

        token.mint_raw(&alice, 100);
        env.env().mock_all_auths_allowing_non_root_auth();

        token.transfer(&alice, &bob, 40);
        token.assert_balance(&alice, 60);
        token.assert_balance(&bob, 40);
    }

    #[test]
    fn transfer_does_not_change_tracked_supply() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();

        token.mint_raw(&alice, 100);
        let supply_before = token.total_tracked();

        env.env().mock_all_auths_allowing_non_root_auth();
        token.transfer(&alice, &bob, 50);

        assert_eq!(token.total_tracked(), supply_before);
    }

    #[test]
    fn try_transfer_returns_ok_on_success() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();

        token.mint_raw(&alice, 200);
        env.env().mock_all_auths_allowing_non_root_auth();

        assert!(token.try_transfer(&alice, &bob, 100).is_ok());
        token.assert_balance(&alice, 100);
        token.assert_balance(&bob, 100);
    }

    #[test]
    fn try_transfer_returns_err_when_authorization_missing() {
 main
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();

 fix/tokens-sac-improvements
        // Pass token double by reference across helpers
        fn helper_mint(t: &TestToken, to: &Address, whole: i128) {
            t.mint(to, whole);
        }

        helper_mint(&token, &alice, 10);
        helper_mint(&token, &bob, 20);

        assert_eq!(token.tracked_holder_count(), 2);
        assert_eq!(token.total_tracked(), 300_000_000);
        token.assert_balance_whole(&alice, 10);
        token.assert_balance_whole(&bob, 20);
    }

    // ── Issue #130: test maximum supported decimal precision ──

    #[test]
    fn maximum_supported_decimal_precision_for_stellar_asset_contract() {
        let env = TestEnv::new();
        let token = env.token();
        assert_eq!(token.decimals(), 7);

        let explicit_token = env.token_with_decimals(7);
        assert_eq!(explicit_token.decimals(), 7);
    }

    #[test]
    #[should_panic(expected = "Stellar Asset Contracts fix decimals at 7")]
    fn token_with_decimals_rejects_greater_than_seven() {
        let env = TestEnv::new();
        let _ = env.token_with_decimals(8);
    }

    #[test]
    #[should_panic(expected = "Stellar Asset Contracts fix decimals at 7")]
    fn token_with_decimals_rejects_u32_max() {
        let env = TestEnv::new();
        let _ = env.token_with_decimals(u32::MAX);
    }

    #[test]
    fn maximum_supported_decimal_precision_for_unit_conversions() {
        // 10^38 fits within i128 (i128::MAX is ~1.7e38), 10^39 overflows i128.
        let max_precision = 38;
        let scale_38 = whole_to_base_units(1, max_precision).expect("38 decimals fits in i128");
        assert_eq!(scale_38, 10i128.pow(max_precision));

        let whole = base_to_whole_units(scale_38, max_precision).expect("round trip base to whole");
        assert_eq!(whole, 1);

        let exact = base_to_whole_units_exact(scale_38, max_precision).expect("exact base to whole");
        assert_eq!(exact, 1);

        // 39 decimals overflows i128
        let err_whole = whole_to_base_units(1, 39).expect_err("39 decimals overflows i128");
        assert!(err_whole.is_misuse());
        assert!(err_whole.message().contains("decimal precision 39 is too large"));

        let err_base = base_to_whole_units(1, 39).expect_err("39 decimals overflows i128");
        assert!(err_base.is_misuse());

        let err_exact = base_to_whole_units_exact(1, 39).expect_err("39 decimals overflows i128");
        assert!(err_exact.is_misuse());

        // u32::MAX does not panic and reports misuse cleanly
        let err_u32_max = whole_to_base_units(1, u32::MAX).expect_err("u32::MAX overflows");
        assert!(err_u32_max.is_misuse());
    }

    // ── Issue #129: test zero and negative mint behavior explicitly ──

    #[test]
    fn test_zero_mint_behavior_explicit() {

        token.mint_raw(&alice, 100);

        let result = token.try_transfer(&alice, &bob, 50);
        assert!(result.is_err());
    }

    #[test]
    #[should_panic(expected = "transfer of")]
    fn transfer_panics_when_balance_insufficient() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();

        token.mint_raw(&alice, 10);
        env.env().mock_all_auths_allowing_non_root_auth();
        token.transfer(&alice, &bob, 999);
    }

    // ── Issue #135: tracked burn helper ──

    #[test]
    fn burn_reduces_balance_of_account() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();

        token.mint_raw(&alice, 100);
        env.env().mock_all_auths_allowing_non_root_auth();

        token.burn(&alice, 30);
        token.assert_balance(&alice, 70);
    }

    #[test]
    fn burn_does_not_decrement_tracked_supply() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();

        token.mint_raw(&alice, 100);
        let supply_before = token.total_tracked();

        env.env().mock_all_auths_allowing_non_root_auth();
        token.burn(&alice, 40);

        assert_eq!(token.total_tracked(), supply_before);
    }

    #[test]
    fn try_burn_returns_ok_on_success() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();

        token.mint_raw(&alice, 100);
        env.env().mock_all_auths_allowing_non_root_auth();

        assert!(token.try_burn(&alice, 50).is_ok());
        token.assert_balance(&alice, 50);
    }

    #[test]
    fn try_burn_returns_err_when_authorization_missing() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();

        token.mint_raw(&alice, 100);

        let result = token.try_burn(&alice, 50);
        assert!(result.is_err());
    }

    #[test]
    #[should_panic(expected = "burn of")]
    fn burn_panics_when_balance_insufficient() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();

        token.mint_raw(&alice, 10);
        env.env().mock_all_auths_allowing_non_root_auth();
        token.burn(&alice, 999);
    }

    // ── Issue #136: allowance setup and assertion helpers ──

    #[test]
    fn allowance_returns_zero_before_any_approval() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();

        assert_eq!(token.allowance(&alice, &bob), 0);
    }

    #[test]
    fn approve_sets_allowance_for_spender() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();

        token.mint_raw(&alice, 200);
        env.env().mock_all_auths_allowing_non_root_auth();

        token.approve(&alice, &bob, 100, 1_000);
        assert_eq!(token.allowance(&alice, &bob), 100);
    }

    #[test]
    fn try_approve_returns_ok_on_success() {
 main
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();
 fix/tokens-sac-improvements
        let carol = env.address();
        let dave = env.address();

        // Minting 0 whole units
        token.mint(&alice, 0);
        token.assert_balance(&alice, 0);
        assert_eq!(token.total_tracked(), 0);
        assert!(token.is_tracked_holder(&alice));

        // Minting 0 base units
        token.mint_raw(&bob, 0);
        token.assert_balance(&bob, 0);
        assert_eq!(token.total_tracked(), 0);
        assert!(token.is_tracked_holder(&bob));

        // Fallible try_mint variants succeed for 0
        assert!(token.try_mint(&carol, 0).is_ok());
        token.assert_balance(&carol, 0);
        assert!(token.is_tracked_holder(&carol));

        assert!(token.try_mint_raw(&dave, 0).is_ok());
        token.assert_balance(&dave, 0);
        assert!(token.is_tracked_holder(&dave));
    }

    #[test]
    fn test_negative_mint_rejected_without_modifying_state() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();

        let err_raw = token
            .try_mint_raw(&alice, -1)
            .expect_err("negative mint_raw must fail");
        assert!(err_raw.is_misuse());
        assert!(err_raw.message().contains("cannot mint negative amount -1"));

        let err_whole = token
            .try_mint(&alice, -5)
            .expect_err("negative try_mint must fail");
        assert!(err_whole.is_misuse());
        assert!(err_whole.message().contains("cannot mint negative amount"));

        // Tracking state remains unchanged
        assert_eq!(token.total_tracked(), 0);
        assert!(!token.is_tracked_holder(&alice));
        assert_eq!(token.tracked_holder_count(), 0);
    }

    #[test]
    #[should_panic(expected = "cannot mint negative amount -1")]
    fn mint_raw_panics_on_negative_amount() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        token.mint_raw(&alice, -1);
    }

    #[test]
    #[should_panic(expected = "cannot mint negative amount")]
    fn mint_panics_on_negative_amount() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        token.mint(&alice, -10);


        token.mint_raw(&alice, 200);
        env.env().mock_all_auths_allowing_non_root_auth();

        assert!(token.try_approve(&alice, &bob, 50, 1_000).is_ok());
        assert_eq!(token.allowance(&alice, &bob), 50);
    }

    #[test]
    fn try_approve_returns_err_when_authorization_missing() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();

        token.mint_raw(&alice, 200);

        let result = token.try_approve(&alice, &bob, 50, 1_000);
        assert!(result.is_err());
    }

    #[test]
    fn assert_allowance_passes_when_allowance_matches() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();

        token.mint_raw(&alice, 200);
        env.env().mock_all_auths_allowing_non_root_auth();
        token.approve(&alice, &bob, 75, 1_000);

        token.assert_allowance(&alice, &bob, 75);
    }

    #[test]
    #[should_panic(expected = "expected allowance of")]
    fn assert_allowance_fails_when_allowance_differs() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();

        token.mint_raw(&alice, 200);
        env.env().mock_all_auths_allowing_non_root_auth();
        token.approve(&alice, &bob, 75, 1_000);

        token.assert_allowance(&alice, &bob, 50);
    }

    #[test]
    fn assert_allowance_failure_message_names_spender_owner_and_token() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();
        let token_addr = token.address();

        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            token.assert_allowance(&alice, &bob, 999);
        }));

        let err = res.expect_err("assert_allowance should panic on mismatch");
        let msg = if let Some(s) = err.downcast_ref::<String>() {
            s.clone()
        } else if let Some(s) = err.downcast_ref::<&str>() {
            s.to_string()
        } else {
            panic!("unexpected panic payload type");
        };

        assert!(msg.contains(&format!("{bob:?}")), "{msg}");
        assert!(msg.contains(&format!("{alice:?}")), "{msg}");
        assert!(msg.contains(&format!("{token_addr:?}")), "{msg}");
        assert!(msg.contains("999"), "{msg}");
        assert!(msg.contains("found 0"), "{msg}");
    }

    #[test]
    fn allowance_is_independent_per_spender() {
        let env = TestEnv::new();
        let token = env.token();
        let alice = env.address();
        let bob = env.address();
        let carol = env.address();

        token.mint_raw(&alice, 500);
        env.env().mock_all_auths_allowing_non_root_auth();

        token.approve(&alice, &bob, 100, 1_000);
        token.approve(&alice, &carol, 200, 1_000);

        token.assert_allowance(&alice, &bob, 100);
        token.assert_allowance(&alice, &carol, 200);
        assert_eq!(token.allowance(&alice, &bob), 100);
        assert_eq!(token.allowance(&alice, &carol), 200);
 main
    }
}
