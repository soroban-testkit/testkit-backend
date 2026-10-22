use std::panic::{self, AssertUnwindSafe};
use std::time::Duration;

use soroban_sdk::testutils::Ledger as _;

use crate::core::{TestEnv, TestkitError};

/// A saved ledger position that can be restored later.
///
/// Captures every field of the ledger that `soroban-sdk` exposes for a test
/// environment — timestamp, sequence number, protocol version, network ID,
/// base reserve, and the entry time-to-live parameters — so that
/// [`TestEnv::restore_checkpoint`] can return the environment to exactly
/// this point.
///
/// A checkpoint is a plain value, not a live handle: it is independent of
/// the environment it was taken from, can be restored more than once, and
/// (unlike [`TestEnv::warp_to`]) restoring one may move the clock backwards.
/// It records ledger state only — contract storage, captured events, and
/// registered tokens are not part of it.
///
/// # Example
///
/// ```
/// use soroban_testkit::core::TestEnv;
/// use soroban_testkit::ledger::LedgerCheckpoint;
/// use std::time::Duration;
///
/// let env = TestEnv::new();
/// let checkpoint = env.checkpoint();
/// env.advance(Duration::from_secs(100));
/// env.restore_checkpoint(checkpoint);
/// // Clock is back where it started.
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerCheckpoint {
    info: soroban_sdk::testutils::LedgerInfo,
}

impl LedgerCheckpoint {
    /// The ledger sequence number at the time of the checkpoint.
    pub fn sequence(&self) -> u32 {
        self.info.sequence_number
    }

    /// The ledger timestamp (unix seconds) at the time of the checkpoint.
    pub fn timestamp(&self) -> u64 {
        self.info.timestamp
    }
}

/// Stellar's current observed average ledger close time, in seconds.
///
/// This is **not** a protocol-guaranteed constant — it is a network
/// characteristic that has moved before (historically as high as ~5-6s,
/// with the Stellar Development Foundation targeting 2.5s in a future
/// release) and could move again. It is the default close interval every
/// [`TestEnv`] starts with; [`TestEnv::advance`] and
/// [`TestEnv::advance_ledgers`] use the interval only to keep `timestamp`
/// and `sequence` moving in proportion to each other, and no assertion in
/// this crate depends on its exact value. If the network average changes
/// meaningfully, update this constant — or, for a single test or fixture,
/// override it with [`TestEnv::with_ledger_close_interval`] — rather than
/// working around it at call sites.
///
/// # Source of the value
///
/// The number is chosen and maintained by this crate; it is **not** read
/// from `soroban-sdk` or from a network at runtime. The SDK's test
/// environment models a ledger as a `timestamp` and a `sequence_number`
/// (both start at `0`) and exposes no close interval, so the relationship
/// between the two — one ledger per this many seconds — is supplied here.
/// It approximates the average spacing of the `closeTime` values in
/// consecutive ledger headers (see the [ledger header
/// documentation](https://developers.stellar.org/docs/learn/encyclopedia/network-configuration/ledger-headers#close-time)).
/// It was last checked against `soroban-sdk` 27.0.6; re-check it whenever
/// the SDK is upgraded, since a future SDK may start exposing the interval.
///
/// The constant must be non-zero: [`TestEnv::with_ledger_close_interval`]
/// rejects a zero override, and the default has to satisfy the same rule.
///
/// # Example
///
/// ```
/// use soroban_testkit::core::TestEnv;
/// use soroban_testkit::ledger::LEDGER_CLOSE_TIME_SECS;
///
/// // With no override, this constant is the interval in effect.
/// let env = TestEnv::new();
/// assert_eq!(env.ledger_close_interval(), LEDGER_CLOSE_TIME_SECS);
/// ```
pub const LEDGER_CLOSE_TIME_SECS: u64 = 5;

const SECS_PER_MINUTE: u64 = 60;
const SECS_PER_HOUR: u64 = 60 * SECS_PER_MINUTE;
const SECS_PER_DAY: u64 = 24 * SECS_PER_HOUR;

impl TestEnv {
    /// Configure how many seconds each ledger takes to close in this
    /// environment, returning the environment so fixtures can chain it onto
    /// construction.
    ///
    /// The default is [`LEDGER_CLOSE_TIME_SECS`]. The interval sets how
    /// `timestamp` and `sequence` move together: [`TestEnv::advance`] (and
    /// [`TestEnv::warp_to`] and the calendar helpers built on it) add
    /// `ceil(seconds / interval)` ledgers, and [`TestEnv::advance_ledgers`]
    /// adds `n * interval` seconds. The setting is per environment — other
    /// `TestEnv`s are unaffected — and configuring it does not move the
    /// clock.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::Misuse`] if `secs` is zero: a zero
    /// interval cannot map elapsed time onto ledger sequence numbers.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// // A network that closes a ledger every 2 seconds.
    /// let env = TestEnv::new().with_ledger_close_interval(2);
    /// let (now, sequence) = (env.now(), env.sequence());
    /// env.advance_ledgers(10);
    /// assert_eq!(env.now(), now + 20);
    /// assert_eq!(env.sequence(), sequence + 10);
    /// ```
    pub fn with_ledger_close_interval(mut self, secs: u64) -> Self {
        if secs == 0 {
            panic!(
                "{}",
                TestkitError::Misuse(
                    "ledger close interval must be at least 1 second; a zero interval \
                     cannot map elapsed time onto ledger sequence numbers"
                        .into()
                )
            );
        }
        self.set_close_interval_override(secs);
        self
    }

    /// The number of seconds each ledger takes to close in this
    /// environment: [`LEDGER_CLOSE_TIME_SECS`] unless overridden with
    /// [`TestEnv::with_ledger_close_interval`].
    ///
    /// The value is resolved on every call, in this order: the environment's
    /// own override, then the crate-wide [`LEDGER_CLOSE_TIME_SECS`]. Neither
    /// comes from `soroban-sdk` or the network (see the constant's docs for
    /// why). An override survives [`TestEnv::clone_config`] and
    /// [`TestEnv::reset`]; an environment with no override keeps following
    /// the crate default.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    /// use soroban_testkit::ledger::LEDGER_CLOSE_TIME_SECS;
    ///
    /// assert_eq!(TestEnv::new().ledger_close_interval(), LEDGER_CLOSE_TIME_SECS);
    /// assert_eq!(
    ///     TestEnv::new().with_ledger_close_interval(2).ledger_close_interval(),
    ///     2
    /// );
    /// ```
    pub fn ledger_close_interval(&self) -> u64 {
        self.close_interval_override()
            .unwrap_or(LEDGER_CLOSE_TIME_SECS)
    }

    /// Capture the current ledger state as a checkpoint.
    ///
    /// The checkpoint records the complete ledger state (timestamp, sequence,
    /// protocol version, network ID, base reserve, etc.) so it can be
    /// perfectly restored later with [`TestEnv::restore_checkpoint`].
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    /// use std::time::Duration;
    ///
    /// let env = TestEnv::new();
    /// let cp = env.checkpoint();
    /// env.advance(Duration::from_secs(100));
    /// env.restore_checkpoint(cp);
    /// ```
    pub fn checkpoint(&self) -> LedgerCheckpoint {
        LedgerCheckpoint {
            info: self.env().ledger().get(),
        }
    }

    /// Restore the ledger to a previously captured checkpoint.
    ///
    /// Every field the checkpoint records (timestamp, sequence number,
    /// protocol version, network ID, base reserve, and the entry
    /// time-to-live parameters) is written back exactly as it was when
    /// [`TestEnv::checkpoint`] was called.
    ///
    /// Unlike [`TestEnv::warp_to`], this may move the clock **backwards**:
    /// that is the whole point of a checkpoint. Restoring is total — it
    /// never panics and cannot fail, because a [`LedgerCheckpoint`] can only
    /// be produced by [`TestEnv::checkpoint`] and therefore always holds
    /// values the ledger accepts.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    /// use std::time::Duration;
    ///
    /// let env = TestEnv::new();
    /// let cp = env.checkpoint();
    /// env.advance(Duration::from_secs(100));
    /// env.restore_checkpoint(cp.clone());
    /// assert_eq!(env.now(), cp.timestamp());
    /// assert_eq!(env.sequence(), cp.sequence());
    /// ```
    pub fn restore_checkpoint(&self, checkpoint: LedgerCheckpoint) {
        self.env().ledger().set(checkpoint.info);
    }

    /// Advance the ledger to an exact sequence number.
    ///
    /// Sets the sequence number to `target_sequence` and moves the timestamp
    /// forward by the number of ledgers crossed, using the environment's
    /// [`close interval`](TestEnv::ledger_close_interval):
    ///
    /// ```text
    /// timestamp += (target_sequence - current_sequence) * close_interval
    /// ```
    ///
    /// This is the sequence-oriented counterpart to
    /// [`TestEnv::advance_ledgers`], for tests that think in absolute ledger
    /// numbers (for example, "the ledger after the vesting cliff") rather
    /// than deltas.
    ///
    /// If `target_sequence` is less than or equal to the current sequence
    /// number, this is a no-op: outside [`TestEnv::restore_checkpoint`], the
    /// ledger clock does not run backwards.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::Misuse`] if the timestamp that
    /// `target_sequence` maps to does not fit in a `u64`. Use
    /// [`TestEnv::try_advance_to_sequence`] to handle that case without
    /// panicking. Both new values are computed before either is written, so
    /// the clock is left unchanged.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// env.advance_to_sequence(100);
    /// assert_eq!(env.sequence(), 100);
    /// ```
    pub fn advance_to_sequence(&self, target_sequence: u32) {
        if let Err(e) = self.try_advance_to_sequence(target_sequence) {
            panic!("{}", e);
        }
    }

    /// Attempt to advance the ledger to an exact sequence number.
    ///
    /// This is the checked counterpart to [`TestEnv::advance_to_sequence`]:
    /// identical mapping, but an advance whose timestamp does not fit in a
    /// `u64` is returned as [`TestkitError::Misuse`] instead of panicking.
    /// The clock is left unchanged on error.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// env.try_advance_to_sequence(100).expect("advance should succeed");
    /// assert_eq!(env.sequence(), 100);
    /// ```
    pub fn try_advance_to_sequence(&self, target_sequence: u32) -> Result<(), TestkitError> {
        let info = self.env().ledger().get();
        let current = info.sequence_number;

        // Never move the clock backwards: a target at or behind the current
        // sequence is accepted as a no-op, exactly like a zero-length
        // `advance`.
        if target_sequence <= current {
            return Ok(());
        }

        let delta = target_sequence - current;
        let interval = self.ledger_close_interval();
        let overflow = |part: &str| {
            TestkitError::Misuse(format!(
                "advancing to sequence {target_sequence} overflows ledger arithmetic: \
                 {part} for {delta} ledgers at {interval}s per ledger does not fit \
                 (current sequence {current})"
            ))
        };

        let timestamp_delta = u64::from(delta)
            .checked_mul(interval)
            .ok_or_else(|| overflow("the timestamp delta"))?;
        let timestamp = info
            .timestamp
            .checked_add(timestamp_delta)
            .ok_or_else(|| overflow("the resulting timestamp"))?;

        self.env().ledger().set_sequence_number(target_sequence);
        self.env().ledger().set_timestamp(timestamp);
        Ok(())
    }

    /// Advance the ledger clock by a wall-clock duration, advancing the
    /// ledger sequence proportionally (per [`TestEnv::ledger_close_interval`]).
    ///
    /// Only whole seconds are applied; any sub-second part of `duration` is
    /// ignored, because ledger timestamps are whole unix seconds.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::Misuse`] if the advance would overflow
    /// ledger arithmetic — the matching ledger count does not fit the
    /// `u32` sequence number, or the new sequence number or timestamp would
    /// pass its type's maximum. The clock is left unchanged.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    /// use std::time::Duration;
    ///
    /// let env = TestEnv::new();
    /// let before = env.now();
    /// env.advance(Duration::from_secs(60));
    /// assert_eq!(env.now(), before + 60);
    /// ```
    pub fn advance(&self, duration: Duration) {
        if let Err(e) = self.try_advance(duration) {
            panic!("{}", e);
        }
    }

    /// Attempt to advance the ledger clock by a wall-clock duration,
    /// advancing the ledger sequence proportionally (per
    /// [`TestEnv::ledger_close_interval`]).
    ///
    /// Only whole seconds are applied; any sub-second part of `duration` is
    /// ignored, because ledger timestamps are whole unix seconds.
    ///
    /// Returns `Err(TestkitError::Misuse)` if the advance would overflow
    /// ledger arithmetic — the matching ledger count does not fit the
    /// `u32` sequence number, or the new sequence number or timestamp would
    /// pass its type's maximum. The clock is left unchanged on error.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    /// use std::time::Duration;
    ///
    /// let env = TestEnv::new();
    /// let before = env.now();
    /// env.try_advance(Duration::from_secs(60)).expect("advance should succeed");
    /// assert_eq!(env.now(), before + 60);
    /// ```
    pub fn try_advance(&self, duration: Duration) -> Result<(), TestkitError> {
        self.try_advance_secs(duration.as_secs())
    }

    /// Advance the ledger clock by `minutes` minutes (60 seconds each).
    ///
    /// Minutes are fixed-length: this is plain unix-time arithmetic with no
    /// leap seconds or time zones, matching how ledger timestamps work. See
    /// [`TestEnv::advance`] for how the sequence follows.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::Misuse`] if `minutes` overflows a
    /// `u64` number of seconds, or if the advance would overflow ledger
    /// arithmetic (see [`TestEnv::advance`]). The clock is left unchanged.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let before = env.now();
    /// env.advance_minutes(5);
    /// assert_eq!(env.now(), before + 300);
    /// ```
    pub fn advance_minutes(&self, minutes: u64) {
        if let Err(e) = self.try_advance_minutes(minutes) {
            panic!("{}", e);
        }
    }

    /// Attempt to advance the ledger clock by `minutes` minutes (60 seconds
    /// each).
    ///
    /// Minutes are fixed-length: this is plain unix-time arithmetic with no
    /// leap seconds or time zones, matching how ledger timestamps work. See
    /// [`TestEnv::try_advance`] for how the sequence follows.
    ///
    /// Returns `Err(TestkitError::Misuse)` if `minutes` overflows a `u64`
    /// number of seconds, or if the advance would overflow ledger arithmetic
    /// (see [`TestEnv::try_advance`]). The clock is left unchanged on error.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let before = env.now();
    /// env.try_advance_minutes(5).expect("advance should succeed");
    /// assert_eq!(env.now(), before + 300);
    /// ```
    pub fn try_advance_minutes(&self, minutes: u64) -> Result<(), TestkitError> {
        let secs = calendar_secs("try_advance_minutes", minutes, SECS_PER_MINUTE)?;
        self.try_advance_secs(secs)
    }

    /// Advance the ledger clock by `hours` hours (3,600 seconds each).
    ///
    /// Hours are fixed-length: this is plain unix-time arithmetic with no
    /// leap seconds or time zones, matching how ledger timestamps work. See
    /// [`TestEnv::advance`] for how the sequence follows.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::Misuse`] if `hours` overflows a `u64`
    /// number of seconds, or if the advance would overflow ledger arithmetic
    /// (see [`TestEnv::advance`]). The clock is left unchanged.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let before = env.now();
    /// env.advance_hours(2);
    /// assert_eq!(env.now(), before + 7_200);
    /// ```
    pub fn advance_hours(&self, hours: u64) {
        if let Err(e) = self.try_advance_hours(hours) {
            panic!("{}", e);
        }
    }

    /// Attempt to advance the ledger clock by `hours` hours (3,600 seconds
    /// each).
    ///
    /// Hours are fixed-length: this is plain unix-time arithmetic with no
    /// leap seconds or time zones, matching how ledger timestamps work. See
    /// [`TestEnv::try_advance`] for how the sequence follows.
    ///
    /// Returns `Err(TestkitError::Misuse)` if `hours` overflows a `u64`
    /// number of seconds, or if the advance would overflow ledger arithmetic
    /// (see [`TestEnv::try_advance`]). The clock is left unchanged on error.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let before = env.now();
    /// env.try_advance_hours(2).expect("advance should succeed");
    /// assert_eq!(env.now(), before + 7_200);
    /// ```
    pub fn try_advance_hours(&self, hours: u64) -> Result<(), TestkitError> {
        let secs = calendar_secs("try_advance_hours", hours, SECS_PER_HOUR)?;
        self.try_advance_secs(secs)
    }

    /// Advance the ledger clock by `days` days (86,400 seconds each).
    ///
    /// Days are fixed-length: this is plain unix-time arithmetic with no
    /// leap seconds, daylight-saving shifts, or time zones, matching how
    /// ledger timestamps work. See [`TestEnv::advance`] for how the sequence
    /// follows.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::Misuse`] if `days` overflows a `u64`
    /// number of seconds, or if the advance would overflow ledger arithmetic
    /// (see [`TestEnv::advance`]). The clock is left unchanged.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let before = env.now();
    /// env.advance_days(30);
    /// assert_eq!(env.now(), before + 30 * 86_400);
    /// ```
    pub fn advance_days(&self, days: u64) {
        if let Err(e) = self.try_advance_days(days) {
            panic!("{}", e);
        }
    }

    /// Attempt to advance the ledger clock by `days` days (86,400 seconds
    /// each).
    ///
    /// Days are fixed-length: this is plain unix-time arithmetic with no
    /// leap seconds, daylight-saving shifts, or time zones, matching how
    /// ledger timestamps work. See [`TestEnv::try_advance`] for how the
    /// sequence follows.
    ///
    /// Returns `Err(TestkitError::Misuse)` if `days` overflows a `u64`
    /// number of seconds, or if the advance would overflow ledger arithmetic
    /// (see [`TestEnv::try_advance`]). The clock is left unchanged on error.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let before = env.now();
    /// env.try_advance_days(30).expect("advance should succeed");
    /// assert_eq!(env.now(), before + 30 * 86_400);
    /// ```
    pub fn try_advance_days(&self, days: u64) -> Result<(), TestkitError> {
        let secs = calendar_secs("try_advance_days", days, SECS_PER_DAY)?;
        self.try_advance_secs(secs)
    }

    /// Advance by exactly `n` ledgers, moving the timestamp forward by
    /// `n * `[`TestEnv::ledger_close_interval`].
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::Misuse`] if the advance would overflow
    /// ledger arithmetic — the sequence number or timestamp would pass their
    /// type's maximum. The clock is left unchanged.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let before = env.sequence();
    /// env.advance_ledgers(10);
    /// assert_eq!(env.sequence(), before + 10);
    /// ```
    pub fn advance_ledgers(&self, n: u32) {
        if let Err(e) = self.try_advance_ledgers(n) {
            panic!("{}", e);
        }
    }

    /// Attempt to advance by exactly `n` ledgers, moving the timestamp
    /// forward by `n * `[`TestEnv::ledger_close_interval`].
    ///
    /// Returns `Err(TestkitError::Misuse)` if the advance would overflow
    /// ledger arithmetic — the sequence number or timestamp would pass their
    /// type's maximum. The clock is left unchanged on error.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let before = env.sequence();
    /// env.try_advance_ledgers(10).expect("advance should succeed");
    /// assert_eq!(env.sequence(), before + 10);
    /// ```
    pub fn try_advance_ledgers(&self, n: u32) -> Result<(), TestkitError> {
        let info = self.env().ledger().get();
        let interval = self.ledger_close_interval();
        let time_delta = (n as u64).checked_mul(interval).ok_or_else(|| {
            TestkitError::Misuse(format!(
                "advancing {n} ledgers at {interval}s per ledger overflows ledger arithmetic"
            ))
        })?;
        let target_sequence = info.sequence_number.checked_add(n).ok_or_else(|| {
            TestkitError::Misuse(format!(
                "advancing by {n} ledgers would overflow the sequence number (currently at {})",
                info.sequence_number
            ))
        })?;
        let target_timestamp = info.timestamp.checked_add(time_delta).ok_or_else(|| {
            TestkitError::Misuse(format!(
                "advancing by {n} ledgers would overflow the timestamp (currently at {})",
                info.timestamp
            ))
        })?;

        self.env().ledger().set_sequence_number(target_sequence);
        self.env().ledger().set_timestamp(target_timestamp);
        Ok(())
    }

    /// Advance the ledger clock to the next close time.
    ///
    /// Computes the remaining time until the next ledger close boundary and
    /// advances by exactly that amount, landing on the next close. If the
    /// clock is already at a close boundary, advances by one full close
    /// interval (i.e., the next close is one interval later, not the current
    /// instant).
    ///
    /// The returned value is the number of seconds actually advanced.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let before = env.now();
    /// let advanced = env.advance_to_next_close();
    /// assert!(advanced > 0);
    /// assert_eq!(env.now(), before + advanced);
    /// ```
    pub fn advance_to_next_close(&self) -> u64 {
        self.try_advance_to_next_close().unwrap_or_else(|e| panic!("{}", e))
    }

    /// Attempt to advance the ledger clock to the next close time.
    ///
    /// Returns the number of seconds advanced, or a [`TestkitError::Misuse`]
    /// if the advance would overflow ledger arithmetic. The clock is left
    /// unchanged on error.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let before = env.now();
    /// let advanced = env.try_advance_to_next_close().expect("advance should succeed");
    /// assert_eq!(env.now(), before + advanced);
    /// ```
    pub fn try_advance_to_next_close(&self) -> Result<u64, TestkitError> {
        let interval = self.ledger_close_interval();
        let elapsed = self.now() % interval;
        let remaining = if elapsed == 0 {
            interval
        } else {
            interval - elapsed
        };
        self.try_advance_secs(remaining)?;
        Ok(remaining)
    }

    /// Jump to an absolute unix timestamp, advancing the sequence in
    /// proportion to the elapsed time.
    ///
    /// # Panics
    ///
    /// Panics with a [`TestkitError::Misuse`] if `timestamp` is before the
    /// environment's current time — the ledger clock cannot run backwards —
    /// or if the jump would overflow ledger arithmetic (see
    /// [`TestEnv::advance`]).
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// env.warp_to(env.now() + 3600);
    /// assert_eq!(env.now(), 3600);
    /// ```
    pub fn warp_to(&self, timestamp: u64) {
        if let Err(e) = self.try_warp_to(timestamp) {
            panic!("{}", e);
        }
    }

    /// Attempt to jump to an absolute unix timestamp, advancing the sequence
    /// in proportion to the elapsed time.
    ///
    /// Returns `Err(TestkitError::Misuse)` if `timestamp` is before the
    /// environment's current time — the ledger clock cannot run backwards —
    /// or if the jump would overflow ledger arithmetic (see
    /// [`TestEnv::try_advance`]). The clock is left unchanged on error.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// env.try_warp_to(env.now() + 3600).expect("warp should succeed");
    /// assert_eq!(env.now(), 3600);
    /// ```
    pub fn try_warp_to(&self, timestamp: u64) -> Result<(), TestkitError> {
        let now = self.now();
        if timestamp < now {
            return Err(TestkitError::Misuse(format!(
                "warp_to({timestamp}) is before the current ledger time ({now}); \
                 the ledger clock cannot run backwards"
            )));
        }
        self.try_advance(Duration::from_secs(timestamp - now))
    }

    /// The current ledger timestamp (unix seconds).
    pub fn now(&self) -> u64 {
        self.env().ledger().timestamp()
    }

    /// The current ledger sequence number.
    pub fn sequence(&self) -> u32 {
        self.env().ledger().sequence()
    }

    /// Run `f` with the ledger clock set to `timestamp`, then restore the
    /// clock to whatever it was before — even if `f` panics.
    ///
    /// This is for evaluating time-dependent contract state (e.g. a vesting
    /// schedule) at several points in time without each call site having to
    /// save and restore the clock by hand.
    ///
    /// Inside the scope the ledger reports a **coherent** position: the
    /// timestamp is `timestamp`, and the sequence number is the one the
    /// clock would hold at that time — the entry sequence shifted by
    /// `ceil((timestamp - entry_timestamp) / ` [`close
    /// interval`](TestEnv::ledger_close_interval) `)`, saturating at `0`
    /// and `u32::MAX` when the target falls outside the sequence number's
    /// range. The scope never leaves the newer (or older) pre-scope
    /// sequence number sitting next to the scoped timestamp, so an
    /// [`TestEnv::advance`] inside the scope moves the clock from the
    /// scoped position rather than from state the scope replaced.
    ///
    /// On exit — normal return or panic — the complete entry snapshot
    /// (timestamp, sequence number, and every other recorded ledger field)
    /// is written back, so the newer pre-scope position is restored
    /// intact and nothing the scope touched leaks out.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let before = env.now();
    /// let observed = env.at(before + 1_000, || env.now());
    /// assert_eq!(observed, before + 1_000);
    /// assert_eq!(env.now(), before);
    /// ```
    pub fn at<T>(&self, timestamp: u64, f: impl FnOnce() -> T) -> T {
        let original = self.env().ledger().get();
        self.env().ledger().set(soroban_sdk::testutils::LedgerInfo {
            timestamp,
            sequence_number: scoped_sequence(&original, timestamp, self.ledger_close_interval()),
            ..original.clone()
        });
        let result = panic::catch_unwind(AssertUnwindSafe(f));
        self.env().ledger().set(original);
        match result {
            Ok(value) => value,
            Err(payload) => panic::resume_unwind(payload),
        }
    }

    /// Shared implementation of every seconds-based `advance*` helper: move
    /// the timestamp forward by `secs` and the sequence by the matching
    /// ledger count for this environment's close interval.
    ///
    /// Both new values are computed with overflow checks before either is
    /// written, so a rejected advance leaves the clock untouched.
    fn try_advance_secs(&self, secs: u64) -> Result<(), TestkitError> {
        let info = self.env().ledger().get();
        let interval = self.ledger_close_interval();
        let ledgers = secs.div_ceil(interval);
        let sequence = u32::try_from(ledgers)
            .ok()
            .and_then(|ledgers| info.sequence_number.checked_add(ledgers))
            .ok_or_else(|| {
                TestkitError::Misuse(format!(
                    "advancing the ledger clock by {secs}s overflows ledger arithmetic: \
                 at {interval}s per ledger that is {ledgers} ledgers, but the clock is \
                 at sequence {} (a u32); use a shorter duration",
                    info.sequence_number
                ))
            })?;
        let timestamp = info.timestamp.checked_add(secs).ok_or_else(|| {
            TestkitError::Misuse(format!(
                "advancing the ledger clock by {secs}s overflows ledger arithmetic: \
                 at {interval}s per ledger that is {ledgers} ledgers, but the clock is \
                 at timestamp {} (a u64); use a shorter duration",
                info.timestamp
            ))
        })?;

        self.env().ledger().set_timestamp(timestamp);
        self.env().ledger().set_sequence_number(sequence);
        Ok(())
    }
}

/// Convert `count` calendar units of `secs_per_unit` seconds into seconds,
/// returning a [`TestkitError::Misuse`] naming `helper` if the product
/// does not fit in a `u64`.
fn calendar_secs(helper: &str, count: u64, secs_per_unit: u64) -> Result<u64, TestkitError> {
    count.checked_mul(secs_per_unit).ok_or_else(|| {
        TestkitError::Misuse(format!(
            "{helper}({count}) overflows ledger arithmetic: \
             {count} * {secs_per_unit}s does not fit in a u64 number of seconds"
        ))
    })
}

/// The sequence number the ledger reports at `timestamp`, given the position
/// recorded in `info` and the environment's `interval` seconds per ledger.
///
/// The mapping mirrors [`TestEnv::advance`]: elapsed (or rewound) seconds
/// map to `ceil(seconds / interval)` ledgers, applied to `info`'s own
/// timestamp/sequence pair so an environment seeded with a non-zero offset
/// keeps that offset at the target time. The result saturates at `0` and
/// `u32::MAX` — a scoped evaluation is total, so a target outside the
/// sequence number's range clamps rather than panicking or wrapping.
fn scoped_sequence(
    info: &soroban_sdk::testutils::LedgerInfo,
    timestamp: u64,
    interval: u64,
) -> u32 {
    let interval = i128::from(interval);
    let delta = i128::from(timestamp) - i128::from(info.timestamp);
    // Ceiling division, so a rewind mirrors `advance`'s round-up in the
    // forward direction. `interval` is never zero (see
    // `with_ledger_close_interval`), so this cannot divide by zero.
    let whole = delta.div_euclid(interval);
    let ledgers = whole + if delta.rem_euclid(interval) > 0 { 1 } else { 0 };
    let scoped = i128::from(info.sequence_number) + ledgers;
    scoped.clamp(0, i128::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_moves_timestamp_by_exact_duration() {
        let env = TestEnv::new();
        let before = env.now();
        env.advance(Duration::from_secs(60));
        assert_eq!(env.now(), before + 60);
    }

    // A zero-duration advance must be an accepted no-op, not treated as an
    // edge case that trips the overflow/rounding arithmetic in
    // `advance_secs` (e.g. `0.div_ceil(interval)` must stay `0`).
    #[test]
    fn advance_by_zero_duration_moves_nothing() {
        let env = TestEnv::new();
        let (now, sequence) = (env.now(), env.sequence());
        env.advance(Duration::from_secs(0));
        assert_eq!(env.now(), now);
        assert_eq!(env.sequence(), sequence);
    }

    #[test]
    fn advance_moves_sequence_consistently_with_close_time() {
        let env = TestEnv::new();
        let before = env.sequence();
        env.advance(Duration::from_secs(LEDGER_CLOSE_TIME_SECS * 10));
        assert_eq!(env.sequence(), before + 10);
    }

    #[test]
    fn advance_ledgers_moves_sequence_by_exactly_n() {
        let env = TestEnv::new();
        let before = env.sequence();
        env.advance_ledgers(7);
        assert_eq!(env.sequence(), before + 7);
    }

    #[test]
    #[should_panic(expected = "warp_to")]
    fn warp_to_past_timestamp_panics() {
        let env = TestEnv::new();
        env.advance(Duration::from_secs(100));
        env.warp_to(0);
    }

    #[test]
    fn warp_to_future_timestamp_sets_now() {
        let env = TestEnv::new();
        env.warp_to(500);
        assert_eq!(env.now(), 500);
    }

    // `warp_to` the exact current timestamp is not "into the past", so it
    // must be accepted as a no-op rather than panicking.
    #[test]
    fn warp_to_the_current_timestamp_is_a_no_op() {
        let env = TestEnv::new();
        env.advance(Duration::from_secs(123));
        let (now, sequence) = (env.now(), env.sequence());

        env.warp_to(now);

        assert_eq!(env.now(), now);
        assert_eq!(env.sequence(), sequence);
    }

    #[test]
    fn at_restores_clock_after_returning() {
        let env = TestEnv::new();
        let before = env.now();
        let seen = env.at(before + 42, || env.now());
        assert_eq!(seen, before + 42);
        assert_eq!(env.now(), before);
    }

    #[test]
    fn at_restores_clock_even_if_closure_panics() {
        let env = TestEnv::new();
        let before = env.now();
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            env.at(before + 999, || panic!("boom"));
        }));
        assert!(result.is_err());
        assert_eq!(env.now(), before);
    }

    // --- advance overflow rejection and checked variants ----------------

    // Regression: a panic inside a *nested* `at` must unwind through both
    // levels and leave each one's saved clock restored — the inner `at`
    // restoring its own snapshot before resuming the unwind, and the outer
    // `at` then restoring its snapshot as the panic passes through it.
    #[test]
    fn at_restores_clock_at_every_level_when_a_nested_closure_panics() {
        let env = TestEnv::new();
        let outer_before = env.now();

        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            env.at(outer_before + 100, || {
                let inner_before = env.now();
                assert_eq!(inner_before, outer_before + 100);
                env.at(outer_before + 200, || {
                    panic!("boom from the innermost closure");
                });
            });
        }));

        assert!(result.is_err());
        assert_eq!(
            env.now(),
            outer_before,
            "outer `at` must restore its snapshot even though the panic \
             originated two levels deeper"
        );
    }

    // Regression: `at` used to restore only the timestamp, so fields such as
    // `protocol_version` or `base_reserve` that a closure changed (directly,
    // or via a nested `at`/`advance`) leaked out of the call instead of
    // being rolled back with the timestamp.
    #[test]
    fn at_restores_non_clock_ledger_fields() {
        let env = TestEnv::new();
        let before = env.env().ledger().get();
        let mutated_reserve = before.base_reserve + 1;

        env.at(env.now() + 10, || {
            env.env().ledger().set(soroban_sdk::testutils::LedgerInfo {
                base_reserve: mutated_reserve,
                ..env.env().ledger().get()
            });
            assert_eq!(env.env().ledger().get().base_reserve, mutated_reserve);
        });

        let after = env.env().ledger().get();
        assert_eq!(
            after.base_reserve, before.base_reserve,
            "base_reserve leaked out of `at` instead of being restored"
        );
        assert_eq!(after.timestamp, before.timestamp);
        assert_eq!(after.protocol_version, before.protocol_version);
        assert_eq!(after.network_id, before.network_id);
    }

    // --- scoped clock coherence (#465) ----------------------------------

    // Regression: `at` used to write only the timestamp, so inside the
    // scope the ledger reported the *newer* pre-scope sequence next to the
    // scoped timestamp. A scoped evaluation of a past moment then saw a
    // future ledger number, and any `advance` inside the scope computed
    // from that newer state.
    #[test]
    fn at_sets_a_coherent_sequence_for_the_scoped_timestamp() {
        let env = TestEnv::new();
        env.advance(Duration::from_secs(100)); // (timestamp 100, sequence 20)

        let seen = env.at(40, || (env.now(), env.sequence()));

        assert_eq!(
            seen,
            (40, 8),
            "the scope must report the sequence that belongs to its timestamp"
        );
        assert_eq!(clock(&env), (100, 20));
    }

    #[test]
    fn at_rounds_the_scoped_sequence_up_like_advance() {
        let env = TestEnv::new();
        env.advance(Duration::from_secs(100)); // (100, 20)

        // Rewinding 58s is 11.6 ledgers; rounding up (as `advance` does in
        // the forward direction) lands 11 ledgers back.
        assert_eq!(env.at(42, || env.sequence()), 9);
    }

    #[test]
    fn at_preserves_a_seeded_timestamp_sequence_offset() {
        let env = TestEnv::with_ledger_defaults(
            crate::core::LedgerDefaults::new()
                .timestamp(1_700_000_000)
                .sequence_number(1_000),
        );

        let seen = env.at(1_700_000_000 - 100, || (env.now(), env.sequence()));

        assert_eq!(seen, (1_700_000_000 - 100, 980));
        assert_eq!(clock(&env), (1_700_000_000, 1_000));
    }

    // The scope's restore must return the newer pre-scope clock intact —
    // it must not be left at, or overwritten by, the scoped position.
    #[test]
    fn at_restores_the_newer_pre_scope_sequence_on_exit() {
        let env = TestEnv::new();
        env.advance(Duration::from_secs(100)); // (100, 20) — newer than the scope
        let before = clock(&env);

        env.at(0, || {
            assert_eq!(
                clock(&env),
                (0, 0),
                "the scope must not report the newer pre-scope sequence"
            );
        });

        assert_eq!(
            clock(&env),
            before,
            "the newer pre-scope state must be restored intact"
        );
    }

    #[test]
    fn at_clamps_the_scoped_sequence_at_zero_for_a_target_before_the_origin() {
        let env = TestEnv::with_ledger_defaults(
            crate::core::LedgerDefaults::new()
                .timestamp(1_000_000)
                .sequence_number(1),
        );

        let seen = env.at(0, || env.sequence());

        assert_eq!(seen, 0, "a far-past target must clamp at sequence 0");
        assert_eq!(clock(&env), (1_000_000, 1));
    }

    #[test]
    fn at_saturates_the_scoped_sequence_at_u32_max_for_a_far_future_target() {
        let env = TestEnv::new();
        let before = clock(&env);

        let seen = env.at(u64::MAX, || env.sequence());

        assert_eq!(seen, u32::MAX);
        assert_eq!(clock(&env), before);
    }

    #[test]
    fn advance_inside_at_computes_from_the_scoped_sequence() {
        let env = TestEnv::new();
        env.advance(Duration::from_secs(100)); // (100, 20)

        env.at(40, || {
            // Scoped at (40, 8); ten more seconds adds two ledgers.
            env.advance(Duration::from_secs(10));
            assert_eq!(clock(&env), (50, 10));
        });

        assert_eq!(clock(&env), (100, 20));
    }

    #[test]
    fn nested_at_scopes_chain_their_sequences() {
        let env = TestEnv::new();
        env.advance(Duration::from_secs(100)); // (100, 20)

        let seen = env.at(50, || {
            // Outer scope: (50, 10).
            env.at(70, || (env.now(), env.sequence())) // inner: (70, 14)
        });

        assert_eq!(seen, (70, 14));
        assert_eq!(clock(&env), (100, 20));
    }

    // --- advance overflow rejection -------------------------------------

    // Regression: `ledgers as u32` used to truncate, so a duration worth
    // exactly 2^32 ledgers advanced the timestamp by ~680 years while
    // leaving the sequence number where it was.
    #[test]
    #[should_panic(expected = "overflows ledger arithmetic")]
    fn advance_rejects_a_ledger_count_that_wraps_u32_to_zero() {
        let env = TestEnv::new();
        let wrapping_secs = (u64::from(u32::MAX) + 1) * LEDGER_CLOSE_TIME_SECS;
        env.advance(Duration::from_secs(wrapping_secs));
    }

    #[test]
    fn try_advance_rejects_a_ledger_count_that_wraps_u32_to_zero() {
        let env = TestEnv::new();
        let wrapping_secs = (u64::from(u32::MAX) + 1) * LEDGER_CLOSE_TIME_SECS;
        let result = env.try_advance(Duration::from_secs(wrapping_secs));
        assert!(result.is_err());
    }

    #[test]
    #[should_panic(expected = "overflows ledger arithmetic")]
    fn advance_rejects_the_maximum_duration() {
        let env = TestEnv::new();
        env.advance(Duration::from_secs(u64::MAX));
    }

    #[test]
    fn try_advance_rejects_the_maximum_duration() {
        let env = TestEnv::new();
        let result = env.try_advance(Duration::from_secs(u64::MAX));
        assert!(result.is_err());
    }

    #[test]
    #[should_panic(expected = "overflows ledger arithmetic")]
    fn advance_rejects_sequence_overflow_instead_of_saturating() {
        let env = TestEnv::new();
        env.advance_ledgers(u32::MAX);
        env.advance(Duration::from_secs(LEDGER_CLOSE_TIME_SECS));
    }

    #[test]
    fn try_advance_rejects_sequence_overflow() {
        let env = TestEnv::new();
        env.advance_ledgers(u32::MAX);
        let result = env.try_advance(Duration::from_secs(LEDGER_CLOSE_TIME_SECS));
        assert!(result.is_err());
    }

    #[test]
    #[should_panic(expected = "overflows ledger arithmetic")]
    fn advance_rejects_timestamp_overflow_instead_of_saturating() {
        let env = TestEnv::new();
        soroban_sdk::testutils::Ledger::set_timestamp(&env.env().ledger(), u64::MAX - 10);
        env.advance(Duration::from_secs(60));
    }

    #[test]
    fn try_advance_rejects_timestamp_overflow() {
        let env = TestEnv::new();
        soroban_sdk::testutils::Ledger::set_timestamp(&env.env().ledger(), u64::MAX - 10);
        let result = env.try_advance(Duration::from_secs(60));
        assert!(result.is_err());
    }

    #[test]
    fn advance_accepts_a_duration_landing_exactly_on_the_sequence_limit() {
        let env = TestEnv::new();
        let room = u64::from(u32::MAX - env.sequence());
        env.advance(Duration::from_secs(room * LEDGER_CLOSE_TIME_SECS));
        assert_eq!(env.sequence(), u32::MAX);
    }

    #[test]
    #[should_panic(expected = "overflows ledger arithmetic")]
    fn advance_rejects_a_duration_one_second_past_the_sequence_limit() {
        let env = TestEnv::new();
        let room = u64::from(u32::MAX - env.sequence());
        env.advance(Duration::from_secs(room * LEDGER_CLOSE_TIME_SECS + 1));
    }

    #[test]
    fn rejected_advance_leaves_the_clock_untouched() {
        let env = TestEnv::new();
        env.advance(Duration::from_secs(100));
        let (now, sequence) = (env.now(), env.sequence());
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            env.advance(Duration::from_secs(u64::MAX));
        }));
        assert!(result.is_err());
        assert_eq!(env.now(), now);
        assert_eq!(env.sequence(), sequence);
    }

    #[test]
    #[should_panic(expected = "would overflow the sequence number")]
    fn advance_ledgers_prevents_sequence_overflow() {
        let env = TestEnv::new();
        env.advance_ledgers(u32::MAX);
        env.advance_ledgers(1);
    }

    #[test]
    fn try_advance_ledgers_prevents_sequence_overflow() {
        let env = TestEnv::new();
        env.advance_ledgers(u32::MAX);
        let result = env.try_advance_ledgers(1);
        assert!(result.is_err());
    }

    #[test]
    fn try_advance_ledgers_succeeds_when_valid() {
        let env = TestEnv::new();
        let before = env.sequence();
        let result = env.try_advance_ledgers(10);
        assert!(result.is_ok());
        assert_eq!(env.sequence(), before + 10);
    }

    #[test]
    fn try_warp_to_rejects_past_timestamp() {
        let env = TestEnv::new();
        env.advance(Duration::from_secs(100));
        let result = env.try_warp_to(50);
        assert!(result.is_err());
    }

    #[test]
    fn try_warp_to_succeeds_for_future_timestamp() {
        let env = TestEnv::new();
        let result = env.try_warp_to(500);
        assert!(result.is_ok());
        assert_eq!(env.now(), 500);
    }

    #[test]
    fn overflow_message_names_the_offending_duration() {
        let env = TestEnv::new();
        let payload = panic::catch_unwind(AssertUnwindSafe(|| {
            env.advance(Duration::from_secs(u64::MAX));
        }))
        .unwrap_err();
        let message = payload
            .downcast_ref::<String>()
            .expect("panic payload should be a formatted String");
        assert!(message.contains("misuse of testkit API"), "{message}");
        assert!(message.contains(&u64::MAX.to_string()), "{message}");
    }

    #[test]
    #[should_panic(expected = "overflows ledger arithmetic")]
    fn warp_to_rejects_a_jump_that_overflows_ledger_arithmetic() {
        let env = TestEnv::new();
        env.warp_to(u64::MAX);
    }

    // --- configurable close interval ------------------------------------

    #[test]
    fn default_close_interval_is_the_crate_constant() {
        assert_eq!(
            TestEnv::new().ledger_close_interval(),
            LEDGER_CLOSE_TIME_SECS
        );
    }

    // The zero check lives in `with_ledger_close_interval`, so nothing else
    // stops the default from being edited to 0 — which would make every
    // `advance` divide by zero.
    #[test]
    fn default_close_interval_is_non_zero() {
        assert_ne!(TestEnv::new().ledger_close_interval(), 0);
    }

    #[test]
    fn interval_resolves_from_the_override_then_the_crate_default() {
        let plain = TestEnv::new();
        assert_eq!(plain.close_interval_override(), None);
        assert_eq!(plain.ledger_close_interval(), LEDGER_CLOSE_TIME_SECS);

        let configured = TestEnv::new().with_ledger_close_interval(9);
        assert_eq!(configured.close_interval_override(), Some(9));
        assert_eq!(configured.ledger_close_interval(), 9);
    }

    #[test]
    fn custom_interval_changes_how_advance_maps_time_to_ledgers() {
        let env = TestEnv::new().with_ledger_close_interval(2);
        let (now, sequence) = (env.now(), env.sequence());
        env.advance(Duration::from_secs(20));
        assert_eq!(env.now(), now + 20);
        assert_eq!(env.sequence(), sequence + 10);
    }

    #[test]
    fn custom_interval_changes_how_advance_ledgers_maps_ledgers_to_time() {
        let env = TestEnv::new().with_ledger_close_interval(2);
        let (now, sequence) = (env.now(), env.sequence());
        env.advance_ledgers(10);
        assert_eq!(env.sequence(), sequence + 10);
        assert_eq!(env.now(), now + 20);
    }

    #[test]
    fn custom_interval_applies_to_warp_to() {
        let env = TestEnv::new().with_ledger_close_interval(10);
        let sequence = env.sequence();
        env.warp_to(100);
        assert_eq!(env.now(), 100);
        assert_eq!(env.sequence(), sequence + 10);
    }

    #[test]
    fn advance_rounds_up_to_a_whole_ledger_for_a_custom_interval() {
        let env = TestEnv::new().with_ledger_close_interval(3);
        let sequence = env.sequence();
        env.advance(Duration::from_secs(7));
        assert_eq!(env.sequence(), sequence + 3);
    }

    #[test]
    fn advance_by_zero_moves_nothing_for_any_interval() {
        let env = TestEnv::new().with_ledger_close_interval(1);
        let (now, sequence) = (env.now(), env.sequence());
        env.advance(Duration::ZERO);
        assert_eq!((env.now(), env.sequence()), (now, sequence));
    }

    #[test]
    fn a_one_second_interval_maps_seconds_to_ledgers_one_to_one() {
        let env = TestEnv::new().with_ledger_close_interval(1);
        let sequence = env.sequence();
        env.advance(Duration::from_secs(42));
        assert_eq!(env.sequence(), sequence + 42);
    }

    #[test]
    fn a_huge_interval_still_advances_at_least_one_ledger() {
        let env = TestEnv::new().with_ledger_close_interval(u64::MAX);
        let sequence = env.sequence();
        env.advance(Duration::from_secs(60));
        assert_eq!(env.sequence(), sequence + 1);
    }

    #[test]
    fn close_interval_is_per_environment() {
        let fast = TestEnv::new().with_ledger_close_interval(1);
        let default = TestEnv::new();
        assert_eq!(fast.ledger_close_interval(), 1);
        assert_eq!(default.ledger_close_interval(), LEDGER_CLOSE_TIME_SECS);
    }

    #[test]
    fn configuring_the_interval_does_not_move_the_clock_or_change_the_seed() {
        let plain = TestEnv::with_seed(7);
        let (now, sequence) = (plain.now(), plain.sequence());
        let configured = TestEnv::with_seed(7).with_ledger_close_interval(2);
        assert_eq!((configured.now(), configured.sequence()), (now, sequence));
        assert_eq!(configured.seed(), 7);
    }

    #[test]
    #[should_panic(expected = "ledger close interval must be at least 1 second")]
    fn zero_close_interval_is_rejected() {
        let _ = TestEnv::new().with_ledger_close_interval(0);
    }

    // --- calendar helpers -----------------------------------------------

    #[test]
    fn advance_minutes_moves_timestamp_by_sixty_seconds_each() {
        let env = TestEnv::new();
        let before = env.now();
        env.advance_minutes(3);
        assert_eq!(env.now(), before + 180);
    }

    #[test]
    fn advance_hours_moves_timestamp_by_3600_seconds_each() {
        let env = TestEnv::new();
        let before = env.now();
        env.advance_hours(2);
        assert_eq!(env.now(), before + 7_200);
    }

    #[test]
    fn advance_days_moves_timestamp_and_sequence_together() {
        let env = TestEnv::new();
        let (now, sequence) = (env.now(), env.sequence());
        env.advance_days(1);
        assert_eq!(env.now(), now + 86_400);
        assert_eq!(
            env.sequence(),
            sequence + (86_400 / LEDGER_CLOSE_TIME_SECS) as u32
        );
    }

    #[test]
    fn calendar_helpers_agree_with_each_other_and_with_advance() {
        let by_minutes = TestEnv::new();
        let by_hours = TestEnv::new();
        let by_days = TestEnv::new();
        let by_duration = TestEnv::new();
        by_minutes.advance_minutes(24 * 60);
        by_hours.advance_hours(24);
        by_days.advance_days(1);
        by_duration.advance(Duration::from_secs(86_400));
        for env in [&by_minutes, &by_hours, &by_days] {
            assert_eq!(env.now(), by_duration.now());
            assert_eq!(env.sequence(), by_duration.sequence());
        }
    }

    #[test]
    fn calendar_helpers_accept_zero_and_move_nothing() {
        let env = TestEnv::new();
        let (now, sequence) = (env.now(), env.sequence());
        env.advance_minutes(0);
        env.advance_hours(0);
        env.advance_days(0);
        assert_eq!((env.now(), env.sequence()), (now, sequence));
    }

    #[test]
    fn calendar_helpers_respect_a_custom_close_interval() {
        let env = TestEnv::new().with_ledger_close_interval(60);
        let sequence = env.sequence();
        env.advance_hours(1);
        assert_eq!(env.sequence(), sequence + 60);
    }

    #[test]
    fn calendar_helpers_accumulate() {
        let env = TestEnv::new();
        let before = env.now();
        env.advance_days(1);
        env.advance_hours(1);
        env.advance_minutes(1);
        assert_eq!(env.now(), before + 86_400 + 3_600 + 60);
    }

    #[test]
    #[should_panic(expected = "advance_minutes(18446744073709551615) overflows ledger arithmetic")]
    fn advance_minutes_rejects_a_count_that_overflows_seconds() {
        TestEnv::new().advance_minutes(u64::MAX);
    }

    #[test]
    fn try_advance_minutes_rejects_a_count_that_overflows_seconds() {
        let result = TestEnv::new().try_advance_minutes(u64::MAX);
        assert!(result.is_err());
    }

    #[test]
    #[should_panic(expected = "advance_hours(18446744073709551615) overflows ledger arithmetic")]
    fn advance_hours_rejects_a_count_that_overflows_seconds() {
        TestEnv::new().advance_hours(u64::MAX);
    }

    #[test]
    fn try_advance_hours_rejects_a_count_that_overflows_seconds() {
        let result = TestEnv::new().try_advance_hours(u64::MAX);
        assert!(result.is_err());
    }

    #[test]
    #[should_panic(expected = "advance_days(18446744073709551615) overflows ledger arithmetic")]
    fn advance_days_rejects_a_count_that_overflows_seconds() {
        TestEnv::new().advance_days(u64::MAX);
    }

    #[test]
    fn try_advance_days_rejects_a_count_that_overflows_seconds() {
        let result = TestEnv::new().try_advance_days(u64::MAX);
        assert!(result.is_err());
    }

    #[test]
    #[should_panic(expected = "overflows ledger arithmetic")]
    fn advance_days_rejects_seconds_that_fit_u64_but_not_the_sequence() {
        TestEnv::new().advance_days(u64::MAX / SECS_PER_DAY);
    }

    #[test]
    fn rejected_calendar_advance_leaves_the_clock_untouched() {
        let env = TestEnv::new();
        env.advance_minutes(5);
        let (now, sequence) = (env.now(), env.sequence());
        let result = panic::catch_unwind(AssertUnwindSafe(|| env.advance_days(u64::MAX)));
        assert!(result.is_err());
        assert_eq!((env.now(), env.sequence()), (now, sequence));
    }

    // --- property tests: timestamp vs. sequence ---------------------------
    //
    // These check relationships that must hold for *every* input rather than
    // hand-picked ones. `proptest` is not a dependency of this crate, so each
    // property runs `CASES` inputs from a small deterministic generator
    // instead. Every failure message names its case and inputs, and the
    // generator is seeded from the case number alone, so a failure reproduces
    // on every run.
    //
    // Notation in the failure messages: `Δt` is the change in `now()` and
    // `Δs` the change in `sequence()`.

    const CASES: u64 = 64;

    /// SplitMix64: tiny, dependency-free, and well distributed even for
    /// consecutive seeds.
    struct Rng(u64);

    impl Rng {
        /// The generator for `case` of the property identified by `salt`.
        fn for_case(salt: u64, case: u64) -> Self {
            Rng(salt.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ case)
        }

        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        /// A value in `lo..=hi` (`hi` must be well below `u64::MAX`).
        fn range(&mut self, lo: u64, hi: u64) -> u64 {
            lo + self.next_u64() % (hi - lo + 1)
        }
    }

    /// `CASES` numbered generators for one property; `salt` keeps different
    /// properties from drawing the same numbers.
    fn cases(salt: u64) -> impl Iterator<Item = (u64, Rng)> {
        (0..CASES).map(move |case| (case, Rng::for_case(salt, case)))
    }

    /// A random close interval, in seconds.
    fn random_interval(rng: &mut Rng) -> u64 {
        rng.range(1, 120)
    }

    fn clock(env: &TestEnv) -> (u64, u32) {
        (env.now(), env.sequence())
    }

    fn env_with(interval: u64) -> TestEnv {
        TestEnv::new().with_ledger_close_interval(interval)
    }

    #[test]
    fn property_advance_moves_time_exactly_and_sequence_by_the_ceiling() {
        for (case, mut rng) in cases(1) {
            let interval = random_interval(&mut rng);
            let secs = rng.range(0, 1_000_000);
            let env = env_with(interval);
            let (now, sequence) = clock(&env);

            env.advance(Duration::from_secs(secs));

            let ctx = format!("case {case}: interval={interval}s, advance={secs}s");
            assert_eq!(env.now() - now, secs, "Δt: {ctx}");
            assert_eq!(
                u64::from(env.sequence() - sequence),
                secs.div_ceil(interval),
                "Δs: {ctx}"
            );
        }
    }

    #[test]
    fn property_advance_ledgers_moves_sequence_exactly_and_time_by_n_intervals() {
        for (case, mut rng) in cases(2) {
            let interval = random_interval(&mut rng);
            let n = rng.range(0, 100_000) as u32;
            let env = env_with(interval);
            let (now, sequence) = clock(&env);

            env.advance_ledgers(n);

            let ctx = format!("case {case}: interval={interval}s, ledgers={n}");
            assert_eq!(env.sequence() - sequence, n, "Δs: {ctx}");
            assert_eq!(env.now() - now, u64::from(n) * interval, "Δt: {ctx}");
        }
    }

    #[test]
    fn property_a_whole_number_of_intervals_is_the_same_as_that_many_ledgers() {
        for (case, mut rng) in cases(3) {
            let interval = random_interval(&mut rng);
            let n = rng.range(0, 100_000);
            let by_time = env_with(interval);
            let by_ledgers = env_with(interval);

            by_time.advance(Duration::from_secs(n * interval));
            by_ledgers.advance_ledgers(n as u32);

            assert_eq!(
                clock(&by_time),
                clock(&by_ledgers),
                "case {case}: interval={interval}s, ledgers={n}"
            );
        }
    }

    #[test]
    fn property_splitting_an_advance_keeps_time_and_adds_at_most_one_ledger() {
        for (case, mut rng) in cases(4) {
            let interval = random_interval(&mut rng);
            let (a, b) = (rng.range(0, 500_000), rng.range(0, 500_000));
            let split = env_with(interval);
            let whole = env_with(interval);

            split.advance(Duration::from_secs(a));
            split.advance(Duration::from_secs(b));
            whole.advance(Duration::from_secs(a + b));

            // Each part rounds up on its own, so the split can only be ahead.
            let ctx = format!("case {case}: interval={interval}s, parts={a}s+{b}s");
            assert_eq!(split.now(), whole.now(), "timestamp: {ctx}");
            assert!(split.sequence() >= whole.sequence(), "split behind: {ctx}");
            assert!(
                split.sequence() - whole.sequence() <= 1,
                "split more than one ledger ahead: {ctx}"
            );
        }
    }

    #[test]
    fn property_splitting_an_advance_in_whole_intervals_loses_nothing() {
        for (case, mut rng) in cases(5) {
            let interval = random_interval(&mut rng);
            let (a, b) = (rng.range(0, 50_000), rng.range(0, 50_000));
            let split = env_with(interval);
            let whole = env_with(interval);

            split.advance(Duration::from_secs(a * interval));
            split.advance(Duration::from_secs(b * interval));
            whole.advance(Duration::from_secs((a + b) * interval));

            assert_eq!(
                clock(&split),
                clock(&whole),
                "case {case}: interval={interval}s, parts={a}+{b} ledgers"
            );
        }
    }

    #[test]
    fn property_warp_to_lands_on_the_target_and_advances_the_sequence_to_match() {
        for (case, mut rng) in cases(6) {
            let interval = random_interval(&mut rng);
            let env = env_with(interval);
            env.advance(Duration::from_secs(rng.range(0, 100_000)));
            let (now, sequence) = clock(&env);
            let delta = rng.range(0, 1_000_000);

            env.warp_to(now + delta);

            let ctx = format!("case {case}: interval={interval}s, from={now}, delta={delta}s");
            assert_eq!(env.now(), now + delta, "timestamp: {ctx}");
            assert_eq!(
                u64::from(env.sequence() - sequence),
                delta.div_ceil(interval),
                "Δs: {ctx}"
            );

            // Warping to the time it already is must change nothing.
            let settled = clock(&env);
            env.warp_to(env.now());
            assert_eq!(clock(&env), settled, "warp to now: {ctx}");
        }
    }

    #[test]
    fn property_warp_to_any_past_timestamp_is_rejected_and_changes_nothing() {
        for (case, mut rng) in cases(7) {
            let interval = random_interval(&mut rng);
            let env = env_with(interval);
            env.advance(Duration::from_secs(rng.range(1, 100_000)));
            let (now, sequence) = clock(&env);
            let past = rng.range(0, now - 1);

            let result = panic::catch_unwind(AssertUnwindSafe(|| env.warp_to(past)));

            let ctx = format!("case {case}: interval={interval}s, now={now}, target={past}");
            assert!(result.is_err(), "warp into the past was accepted: {ctx}");
            assert_eq!(clock(&env), (now, sequence), "clock moved: {ctx}");
        }
    }

    #[test]
    fn property_at_restores_time_and_sequence_whatever_the_target_or_closure_does() {
        for (case, mut rng) in cases(8) {
            let interval = random_interval(&mut rng);
            let env = env_with(interval);
            env.advance(Duration::from_secs(rng.range(0, 100_000)));
            let before = clock(&env);
            let target = rng.range(0, 10_000_000);
            let skipped = rng.range(0, 100_000);

            let observed = env.at(target, || env.now());
            let ctx = format!("case {case}: interval={interval}s, target={target}");
            assert_eq!(observed, target, "timestamp inside at: {ctx}");
            assert_eq!(clock(&env), before, "clock after at: {ctx}");

            // Even a closure that moves the clock itself is rolled back.
            env.at(target, || env.advance(Duration::from_secs(skipped)));
            assert_eq!(
                clock(&env),
                before,
                "clock after at with an advance of {skipped}s inside: {ctx}"
            );
        }
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn property_calendar_helpers_match_advance_by_the_same_number_of_seconds() {
        type Helper = (&'static str, u64, fn(&TestEnv, u64));
        let helpers: [Helper; 3] = [
            ("advance_minutes", SECS_PER_MINUTE, TestEnv::advance_minutes),
            ("advance_hours", SECS_PER_HOUR, TestEnv::advance_hours),
            ("advance_days", SECS_PER_DAY, TestEnv::advance_days),
        ];

        for (case, mut rng) in cases(9) {
            let interval = random_interval(&mut rng);
            let count = rng.range(0, 10_000);
            for (name, unit, helper) in helpers {
                let by_helper = env_with(interval);
                let by_advance = env_with(interval);

                helper(&by_helper, count);
                by_advance.advance(Duration::from_secs(count * unit));

                assert_eq!(
                    clock(&by_helper),
                    clock(&by_advance),
                    "case {case}: {name}({count}) at {interval}s per ledger"
                );
            }
        }
    }

    #[test]
    fn property_any_run_of_clock_moves_stays_monotonic_and_proportional() {
        for (case, mut rng) in cases(10) {
            let interval = random_interval(&mut rng);
            let env = env_with(interval);
            let (start_now, start_sequence) = clock(&env);
            let (mut prev_now, mut prev_sequence) = (start_now, start_sequence);
            // Moves that round a duration up to whole ledgers, each of which
            // can leave the sequence up to `interval - 1` seconds ahead.
            let mut rounding_moves = 0;

            for step in 0..rng.range(1, 12) {
                let op = rng.range(0, 3);
                match op {
                    0 => env.advance(Duration::from_secs(rng.range(0, 100_000))),
                    1 => env.advance_ledgers(rng.range(0, 10_000) as u32),
                    2 => env.warp_to(env.now() + rng.range(0, 100_000)),
                    _ => env.advance_minutes(rng.range(0, 1_000)),
                }
                if op != 1 {
                    rounding_moves += 1;
                }

                let (now, sequence) = clock(&env);
                let ctx = format!("case {case}: interval={interval}s, step={step}, op={op}");
                assert!(now >= prev_now, "timestamp went backwards: {ctx}");
                assert!(sequence >= prev_sequence, "sequence went backwards: {ctx}");
                // Ledgers never lag the elapsed time, and only run ahead of it
                // by the rounding each duration-based move can add.
                let elapsed = now - start_now;
                let ledger_secs = u64::from(sequence - start_sequence) * interval;
                assert!(ledger_secs >= elapsed, "sequence lags time: {ctx}");
                assert!(
                    ledger_secs <= elapsed + rounding_moves * (interval - 1),
                    "sequence ahead of time by more than rounding allows: {ctx}"
                );
                (prev_now, prev_sequence) = (now, sequence);
            }
        }
    }

    // --- advance_to_sequence (#60) --------------------------------------

    #[test]
    fn advance_to_sequence_lands_on_the_target_and_moves_time_proportionally() {
        let env = TestEnv::new();
        let (now, sequence) = clock(&env);

        env.advance_to_sequence(sequence + 100);

        assert_eq!(env.sequence(), sequence + 100);
        assert_eq!(env.now(), now + 100 * LEDGER_CLOSE_TIME_SECS);
    }

    #[test]
    fn advance_to_sequence_uses_the_environments_close_interval() {
        let env = env_with(3);
        let now = env.now();

        env.advance_to_sequence(7);

        assert_eq!(env.sequence(), 7);
        assert_eq!(env.now(), now + 21);
    }

    #[test]
    fn advance_to_sequence_starts_from_a_non_zero_sequence() {
        let env = TestEnv::with_ledger_defaults(
            crate::core::LedgerDefaults::new()
                .timestamp(1_700_000_000)
                .sequence_number(1_000),
        );

        env.advance_to_sequence(1_010);

        assert_eq!(env.sequence(), 1_010);
        assert_eq!(env.now(), 1_700_000_000 + 10 * LEDGER_CLOSE_TIME_SECS);
    }

    // A target at or behind the current sequence is a no-op: outside
    // `restore_checkpoint` the clock never runs backwards, so a test cannot
    // rewind by asking for an older ledger.
    #[test]
    fn advance_to_sequence_to_the_current_sequence_is_a_no_op() {
        let env = TestEnv::new();
        env.advance_ledgers(9);
        let before = clock(&env);

        env.advance_to_sequence(env.sequence());

        assert_eq!(clock(&env), before);
    }

    #[test]
    fn advance_to_sequence_into_the_past_is_a_no_op() {
        let env = TestEnv::new();
        env.advance_to_sequence(500);
        let before = clock(&env);

        env.advance_to_sequence(1);

        assert_eq!(clock(&env), before);
    }

    #[test]
    fn try_advance_to_sequence_reports_overflow_without_moving_the_clock() {
        let env = TestEnv::new();
        env.env().ledger().set_timestamp(u64::MAX - 10);
        let before = clock(&env);

        let error = env
            .try_advance_to_sequence(env.sequence() + 10)
            .expect_err("a mapped timestamp past u64::MAX must be rejected");

        assert_eq!(error.kind(), "Misuse");
        assert!(
            error.message().contains("overflows ledger arithmetic"),
            "{error}"
        );
        assert_eq!(clock(&env), before, "clock moved on a rejected advance");
    }

    #[test]
    #[should_panic(expected = "overflows ledger arithmetic")]
    fn advance_to_sequence_rejects_a_mapped_timestamp_that_does_not_fit() {
        let env = TestEnv::new();
        env.env().ledger().set_timestamp(u64::MAX - 10);
        env.advance_to_sequence(env.sequence() + 10);
    }

    #[test]
    fn advance_to_sequence_leaves_the_clock_untouched_when_it_panics() {
        let env = TestEnv::new();
        env.env().ledger().set_timestamp(u64::MAX - 10);
        let before = clock(&env);

        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            env.advance_to_sequence(env.sequence() + 10);
        }));

        assert!(result.is_err(), "advance should have panicked");
        assert_eq!(clock(&env), before);
    }

    #[test]
    fn property_advance_to_sequence_matches_advance_ledgers() {
        for (case, mut rng) in cases(11) {
            let interval = random_interval(&mut rng);
            let n = rng.range(0, 100_000) as u32;

            let by_sequence = env_with(interval);
            let by_ledgers = env_with(interval);
            let target = by_sequence.sequence() + n;

            by_sequence.advance_to_sequence(target);
            by_ledgers.advance_ledgers(n);

            assert_eq!(
                clock(&by_sequence),
                clock(&by_ledgers),
                "case {case}: interval={interval}s, target={target}"
            );
        }
    }

    // --- LedgerCheckpoint (#61) -----------------------------------------

    #[test]
    fn checkpoint_restores_the_exact_clock_position() {
        let env = TestEnv::new();
        let checkpoint = env.checkpoint();

        env.advance(Duration::from_secs(600));
        assert_ne!(clock(&env), (checkpoint.timestamp(), checkpoint.sequence()));

        env.restore_checkpoint(checkpoint.clone());

        assert_eq!(clock(&env), (checkpoint.timestamp(), checkpoint.sequence()));
    }

    #[test]
    fn restore_checkpoint_returns_every_recorded_ledger_field() {
        let env = TestEnv::new();
        let original = env.env().ledger().get();
        let checkpoint = env.checkpoint();

        env.env().ledger().with_mut(|info| {
            info.timestamp = info.timestamp.wrapping_add(9_999);
            info.sequence_number = info.sequence_number.wrapping_add(42);
            info.protocol_version = info.protocol_version.wrapping_add(1);
            info.network_id = [7; 32];
            info.base_reserve = info.base_reserve.wrapping_add(1);
            info.min_temp_entry_ttl = info.min_temp_entry_ttl.wrapping_add(1);
            info.min_persistent_entry_ttl = info.min_persistent_entry_ttl.wrapping_add(1);
            info.max_entry_ttl = info.max_entry_ttl.wrapping_add(1);
        });
        assert_ne!(
            env.env().ledger().get(),
            original,
            "the fixture failed to disturb the ledger"
        );

        env.restore_checkpoint(checkpoint);

        assert_eq!(env.env().ledger().get(), original);
    }

    #[test]
    fn a_checkpoint_is_a_snapshot_not_a_live_view() {
        let env = TestEnv::new();
        let checkpoint = env.checkpoint();
        let taken = (checkpoint.timestamp(), checkpoint.sequence());

        env.advance(Duration::from_secs(300));

        assert_eq!(
            (checkpoint.timestamp(), checkpoint.sequence()),
            taken,
            "moving the clock must not change an existing checkpoint"
        );
    }

    #[test]
    fn a_checkpoint_can_be_restored_more_than_once() {
        let env = TestEnv::new();
        let checkpoint = env.checkpoint();

        for restore_number in 0..3 {
            env.advance(Duration::from_secs(60));
            env.restore_checkpoint(checkpoint.clone());
            assert_eq!(
                clock(&env),
                (checkpoint.timestamp(), checkpoint.sequence()),
                "restore {restore_number}"
            );
        }
    }

    // A checkpoint is the documented way to move the ledger clock backwards,
    // so a rewind must be accepted rather than treated as misuse.
    #[test]
    fn restore_checkpoint_may_rewind_the_clock() {
        let env = TestEnv::new();
        let checkpoint = env.checkpoint();

        env.advance(Duration::from_secs(600));
        let before = env.sequence();

        env.restore_checkpoint(checkpoint);

        assert_eq!(env.sequence(), 0);
        assert!(env.sequence() < before);
    }

    // A checkpoint is a value, not a handle into the environment it came
    // from, so it can be applied to a different environment.
    #[test]
    fn a_checkpoint_can_be_applied_to_another_environment() {
        let source = TestEnv::with_ledger_defaults(
            crate::core::LedgerDefaults::new()
                .timestamp(1_700_000_000)
                .sequence_number(77),
        );
        let checkpoint = source.checkpoint();

        let target = TestEnv::new();
        assert_ne!(target.sequence(), checkpoint.sequence());

        target.restore_checkpoint(checkpoint.clone());

        assert_eq!(target.now(), checkpoint.timestamp());
        assert_eq!(target.sequence(), checkpoint.sequence());
    }

    #[test]
    fn checkpoints_carry_value_equality() {
        let env = TestEnv::new();
        assert_eq!(env.checkpoint(), env.checkpoint());

        let before = env.checkpoint();
        env.advance(Duration::from_secs(1));
        assert_ne!(env.checkpoint(), before);
    }

    // --- advance_to_next_close (#399) -----------------------------------

    #[test]
    fn advance_to_next_close_lands_on_a_close_boundary() {
        let env = TestEnv::new().with_ledger_close_interval(5);
        env.advance(Duration::from_secs(3)); // not on a boundary
        let before = env.now();

        env.advance_to_next_close();

        assert_eq!(env.now() % 5, 0, "must land on a close boundary");
        assert!(env.now() > before);
    }

    #[test]
    fn advance_to_next_close_from_a_boundary_advances_one_full_interval() {
        let env = TestEnv::new().with_ledger_close_interval(5);
        // Start at t=0 (a boundary), advance to next close
        let before = env.now();
        let advanced = env.advance_to_next_close();

        assert_eq!(advanced, 5, "from a boundary, advance one full interval");
        assert_eq!(env.now(), before + 5);
    }

    #[test]
    fn advance_to_next_close_advances_by_the_remainder() {
        let env = TestEnv::new().with_ledger_close_interval(10);
        env.advance(Duration::from_secs(7)); // 7 % 10 = 7, remainder = 3
        let before = env.now();

        let advanced = env.advance_to_next_close();

        assert_eq!(advanced, 3);
        assert_eq!(env.now(), before + 3);
    }

    #[test]
    fn advance_to_next_close_respects_custom_interval() {
        let env = TestEnv::new().with_ledger_close_interval(3);
        env.advance(Duration::from_secs(1)); // 1 % 3 = 1, remainder = 2
        let before = env.now();

        env.advance_to_next_close();

        assert_eq!(env.now(), before + 2);
        assert_eq!(env.now() % 3, 0);
    }

    #[test]
    fn try_advance_to_next_close_succeeds() {
        let env = TestEnv::new();
        let result = env.try_advance_to_next_close();
        assert!(result.is_ok());
        assert!(result.unwrap() > 0);
    }
}
