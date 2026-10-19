//! Runtime capability discovery for SDK TTL test support.
//!
//! Different protocol versions and SDK configurations expose different TTL
//! behaviours. Instead of hard-coding assumptions about what the current
//! host enforces, call [`TestEnv::ttl_capabilities`] once at the start of a
//! test and branch on what you actually have.
//!
//! # Why this matters
//!
//! The Soroban SDK's state-archival rules have changed across protocol
//! versions:
//!
//! | Version | Persistent on expiry | Temporary on expiry |
//! |---|---|---|
//! | ≥ [`ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH`] | Archived → restored on read | Deleted forever |
//!
//! A test that *assumes* archival is available will mis-fire on a host or
//! protocol that does not support it. [`SdkTtlCapabilities`] lets the test
//! discover the truth at runtime and either skip, adapt, or assert
//! accordingly.
//!
//! [`ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH`]: crate::ttl::ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH
//!
//! # Example — skipping archival work when it isn't available
//!
//! ```
//! use soroban_testkit::core::TestEnv;
//!
//! let env = TestEnv::new();
//! let caps = env.ttl_capabilities();
//!
//! if caps.persistent_archival_supported() {
//!     // test archival / restore behaviour
//! }
//! ```

use std::fmt;

use soroban_sdk::testutils::Ledger as _;

use crate::core::TestEnv;
use crate::ttl::{
    ARCHIVAL_PROTOCOL_CONTRACT_LIFECYCLE, ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH,
    ARCHIVAL_PROTOCOL_STATE_ARCHIVAL,
};

// ─────────────────────────────────────────────────────────────────────────────
// SdkTtlCapabilities
// ─────────────────────────────────────────────────────────────────────────────

/// A snapshot of which TTL and state-archival capabilities are available in
/// a [`TestEnv`] at the time of the probe.
///
/// Obtain one with [`TestEnv::ttl_capabilities`]. The snapshot is taken
/// from the live ledger info so it always agrees with what the host actually
/// enforces — it is never hardcoded.
///
/// # Capabilities reported
///
/// | Method | True when |
/// |---|---|
/// | [`ttl_inspection_supported`] | The SDK can report a live TTL for storage entries |
/// | [`ttl_extension_supported`] | Contracts may call `extend_ttl` |
/// | [`persistent_archival_supported`] | Expired persistent entries are archived rather than deleted |
/// | [`temporary_deletion_supported`] | Expired temporary entries are deleted (no restore) |
/// | [`contract_lifecycle_supported`] | Per-contract instance and code lifecycle operations available |
/// | [`state_archival_settings_supported`] | State-archival network parameters are available |
/// | [`bulk_ttl_bump_supported`] | The multi-key TTL-bump recipe is supported |
///
/// [`ttl_inspection_supported`]: SdkTtlCapabilities::ttl_inspection_supported
/// [`ttl_extension_supported`]: SdkTtlCapabilities::ttl_extension_supported
/// [`persistent_archival_supported`]: SdkTtlCapabilities::persistent_archival_supported
/// [`temporary_deletion_supported`]: SdkTtlCapabilities::temporary_deletion_supported
/// [`contract_lifecycle_supported`]: SdkTtlCapabilities::contract_lifecycle_supported
/// [`state_archival_settings_supported`]: SdkTtlCapabilities::state_archival_settings_supported
/// [`bulk_ttl_bump_supported`]: SdkTtlCapabilities::bulk_ttl_bump_supported
///
/// # Example
///
/// ```
/// use soroban_testkit::core::TestEnv;
///
/// let env = TestEnv::new();
/// let caps = env.ttl_capabilities();
///
/// // TTL inspection is always supported by the pinned SDK.
/// assert!(caps.ttl_inspection_supported());
/// // The protocol version driving the report is accessible.
/// assert!(caps.protocol_version() > 0);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdkTtlCapabilities {
    /// Protocol version the environment reports.
    protocol_version: u32,
    /// Minimum TTL (ledgers) for a new persistent entry.
    min_persistent_entry_ttl: u32,
    /// Minimum TTL (ledgers) for a new temporary entry.
    min_temp_entry_ttl: u32,
    /// Maximum TTL (ledgers) any entry may be extended to.
    max_entry_ttl: u32,
}

impl SdkTtlCapabilities {
    /// The protocol version this capability snapshot was taken from.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// assert!(env.ttl_capabilities().protocol_version() > 0);
    /// ```
    pub fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    /// The minimum TTL (in ledgers) granted to a new persistent or instance
    /// entry, as reported by the live ledger info.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// assert!(env.ttl_capabilities().min_persistent_entry_ttl() > 0);
    /// ```
    pub fn min_persistent_entry_ttl(&self) -> u32 {
        self.min_persistent_entry_ttl
    }

    /// The minimum TTL (in ledgers) granted to a new temporary entry, as
    /// reported by the live ledger info.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// assert!(env.ttl_capabilities().min_temp_entry_ttl() > 0);
    /// ```
    pub fn min_temp_entry_ttl(&self) -> u32 {
        self.min_temp_entry_ttl
    }

    /// The maximum TTL (in ledgers) any entry may be extended to, as reported
    /// by the live ledger info.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let caps = env.ttl_capabilities();
    /// assert!(caps.max_entry_ttl() >= caps.min_persistent_entry_ttl());
    /// ```
    pub fn max_entry_ttl(&self) -> u32 {
        self.max_entry_ttl
    }

    // ── Capability predicates ─────────────────────────────────────────────────

    /// Whether the SDK can inspect the TTL of a storage entry at runtime.
    ///
    /// This capability has been present since Soroban launched; it is always
    /// `true` against the pinned `soroban-sdk` this crate targets. It is
    /// exposed so that code written against future SDK versions can check
    /// gracefully rather than crashing.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// assert!(TestEnv::new().ttl_capabilities().ttl_inspection_supported());
    /// ```
    pub fn ttl_inspection_supported(&self) -> bool {
        // TTL inspection via `get_ttl` has been available since the initial
        // Soroban launch protocol.
        self.protocol_version >= ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH
    }

    /// Whether contracts may call `extend_ttl` to push an entry's expiry
    /// further into the future.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// assert!(TestEnv::new().ttl_capabilities().ttl_extension_supported());
    /// ```
    pub fn ttl_extension_supported(&self) -> bool {
        self.protocol_version >= ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH
    }

    /// Whether expired **persistent** entries are archived (and restored on
    /// their next read) rather than permanently deleted.
    ///
    /// When `false`, reading an expired persistent entry is equivalent to
    /// reading a never-written one. Tests that exercise
    /// [`TestEnv::assert_recovers_after_expiry`] should guard behind this
    /// flag.
    ///
    /// [`TestEnv::assert_recovers_after_expiry`]: crate::core::TestEnv::assert_recovers_after_expiry
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// // The pinned SDK always archives persistent entries.
    /// assert!(TestEnv::new().ttl_capabilities().persistent_archival_supported());
    /// ```
    pub fn persistent_archival_supported(&self) -> bool {
        self.protocol_version >= ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH
    }

    /// Whether expired **temporary** entries are immediately and permanently
    /// deleted (as opposed to being archived like persistent entries).
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// // Temporary entries are always deleted on expiry in the pinned SDK.
    /// assert!(TestEnv::new().ttl_capabilities().temporary_deletion_supported());
    /// ```
    pub fn temporary_deletion_supported(&self) -> bool {
        self.protocol_version >= ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH
    }

    /// Whether per-contract instance and code lifecycle operations (instance
    /// TTL extension, code TTL extension) are available.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let _ = env.ttl_capabilities().contract_lifecycle_supported();
    /// ```
    pub fn contract_lifecycle_supported(&self) -> bool {
        self.protocol_version >= ARCHIVAL_PROTOCOL_CONTRACT_LIFECYCLE
    }

    /// Whether the state-archival network parameters (grace period for
    /// read-only entries, etc.) are available and enforced.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let _ = env.ttl_capabilities().state_archival_settings_supported();
    /// ```
    pub fn state_archival_settings_supported(&self) -> bool {
        self.protocol_version >= ARCHIVAL_PROTOCOL_STATE_ARCHIVAL
    }

    /// Whether the multi-key TTL-bump recipe
    /// ([`TestEnv::assert_bumps_ttl_multi`]) is supported.
    ///
    /// This mirrors [`ttl_extension_supported`]: both depend on the same
    /// protocol capability. The separate predicate exists so tests can name
    /// their intent precisely.
    ///
    /// [`TestEnv::assert_bumps_ttl_multi`]: crate::core::TestEnv::assert_bumps_ttl_multi
    /// [`ttl_extension_supported`]: SdkTtlCapabilities::ttl_extension_supported
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// assert!(TestEnv::new().ttl_capabilities().bulk_ttl_bump_supported());
    /// ```
    pub fn bulk_ttl_bump_supported(&self) -> bool {
        self.ttl_extension_supported()
    }

    // ── Convenience combinators ───────────────────────────────────────────────

    /// Whether the full archival recipe — write, expire, read-back, verify
    /// restored — is supported end-to-end.
    ///
    /// Equivalent to `persistent_archival_supported() && ttl_inspection_supported()`.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// assert!(TestEnv::new().ttl_capabilities().full_archival_recipe_supported());
    /// ```
    pub fn full_archival_recipe_supported(&self) -> bool {
        self.persistent_archival_supported() && self.ttl_inspection_supported()
    }

    /// Whether the temporary-expiry boundary recipe — checking both sides
    /// of an entry's last live ledger — is supported.
    ///
    /// Equivalent to `temporary_deletion_supported() && ttl_inspection_supported()`.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// assert!(TestEnv::new().ttl_capabilities().temporary_boundary_recipe_supported());
    /// ```
    pub fn temporary_boundary_recipe_supported(&self) -> bool {
        self.temporary_deletion_supported() && self.ttl_inspection_supported()
    }

    /// Collect every capability name that is currently **not** supported into
    /// a list of human-readable strings.
    ///
    /// Useful for diagnostic output when a test suite needs to document which
    /// capabilities are absent, without asserting on any specific one.
    ///
    /// Returns an empty `Vec` when all capabilities are supported.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let caps = TestEnv::new().ttl_capabilities();
    /// // The pinned SDK supports everything; the unsupported list is empty.
    /// let missing = caps.unsupported_capabilities();
    /// assert!(
    ///     missing.is_empty(),
    ///     "unexpected missing capabilities: {missing:?}"
    /// );
    /// ```
    pub fn unsupported_capabilities(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if !self.ttl_inspection_supported() {
            out.push("ttl_inspection");
        }
        if !self.ttl_extension_supported() {
            out.push("ttl_extension");
        }
        if !self.persistent_archival_supported() {
            out.push("persistent_archival");
        }
        if !self.temporary_deletion_supported() {
            out.push("temporary_deletion");
        }
        if !self.contract_lifecycle_supported() {
            out.push("contract_lifecycle");
        }
        if !self.state_archival_settings_supported() {
            out.push("state_archival_settings");
        }
        if !self.bulk_ttl_bump_supported() {
            out.push("bulk_ttl_bump");
        }
        out
    }

    /// Collect every capability name that is currently supported into a list
    /// of human-readable strings.
    ///
    /// Useful for audit logging or CI diagnostics.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let caps = TestEnv::new().ttl_capabilities();
    /// let supported = caps.supported_capabilities();
    /// assert!(!supported.is_empty());
    /// ```
    pub fn supported_capabilities(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.ttl_inspection_supported() {
            out.push("ttl_inspection");
        }
        if self.ttl_extension_supported() {
            out.push("ttl_extension");
        }
        if self.persistent_archival_supported() {
            out.push("persistent_archival");
        }
        if self.temporary_deletion_supported() {
            out.push("temporary_deletion");
        }
        if self.contract_lifecycle_supported() {
            out.push("contract_lifecycle");
        }
        if self.state_archival_settings_supported() {
            out.push("state_archival_settings");
        }
        if self.bulk_ttl_bump_supported() {
            out.push("bulk_ttl_bump");
        }
        out
    }
}

impl fmt::Display for SdkTtlCapabilities {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SdkTtlCapabilities {{ protocol={}, ttl_inspection={}, ttl_extension={}, \
             persistent_archival={}, temporary_deletion={}, contract_lifecycle={}, \
             state_archival_settings={}, bulk_ttl_bump={} }}",
            self.protocol_version,
            self.ttl_inspection_supported(),
            self.ttl_extension_supported(),
            self.persistent_archival_supported(),
            self.temporary_deletion_supported(),
            self.contract_lifecycle_supported(),
            self.state_archival_settings_supported(),
            self.bulk_ttl_bump_supported(),
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// TestEnv methods
// ─────────────────────────────────────────────────────────────────────────────

impl TestEnv {
    /// Probe the current SDK environment and return a snapshot of which TTL
    /// capabilities are available.
    ///
    /// The snapshot is derived from the live ledger info — it is never
    /// hardcoded — so it accurately reflects the protocol version and network
    /// parameters the environment currently enforces. Call this once at the
    /// start of a test (or test module) and branch on the returned
    /// [`SdkTtlCapabilities`] to decide which TTL assertions to apply.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let caps = env.ttl_capabilities();
    ///
    /// assert!(caps.ttl_inspection_supported());
    /// assert!(caps.ttl_extension_supported());
    /// assert!(caps.persistent_archival_supported());
    /// assert!(caps.temporary_deletion_supported());
    /// ```
    pub fn ttl_capabilities(&self) -> SdkTtlCapabilities {
        let info = self.env().ledger().get();
        SdkTtlCapabilities {
            protocol_version: info.protocol_version,
            min_persistent_entry_ttl: info.min_persistent_entry_ttl,
            min_temp_entry_ttl: info.min_temp_entry_ttl,
            max_entry_ttl: info.max_entry_ttl,
        }
    }

    /// Assert that the environment supports every TTL capability this crate
    /// exposes, panicking with an actionable message that lists any gaps.
    ///
    /// This is the most convenient way to guard an entire test file that
    /// relies on all TTL features: place it at the top of each test or in
    /// a shared setup function.
    ///
    /// # Panics
    ///
    /// Panics with a [`crate::core::TestkitError::AssertionFailed`] listing
    /// every missing capability if any are absent.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// // The pinned SDK supports every capability; this never panics.
    /// TestEnv::new().assert_full_ttl_support();
    /// ```
    pub fn assert_full_ttl_support(&self) {
        use crate::core::TestkitError;
        let caps = self.ttl_capabilities();
        let missing = caps.unsupported_capabilities();
        if !missing.is_empty() {
            panic!(
                "{}",
                TestkitError::AssertionFailed(format!(
                    "environment (protocol {}) does not support the following TTL \
                     capabilities required by this test: [{}]. \
                     Use TestEnv::with_protocol_version to pin a supported version, \
                     or guard the test behind individual capability checks.",
                    caps.protocol_version(),
                    missing.join(", ")
                ))
            );
        }
    }

    /// Assert that the environment supports at least the given minimum set of
    /// TTL capabilities, accepting a slice of capability names (as returned by
    /// [`SdkTtlCapabilities::supported_capabilities`] /
    /// [`SdkTtlCapabilities::unsupported_capabilities`]).
    ///
    /// Valid names: `"ttl_inspection"`, `"ttl_extension"`,
    /// `"persistent_archival"`, `"temporary_deletion"`,
    /// `"contract_lifecycle"`, `"state_archival_settings"`,
    /// `"bulk_ttl_bump"`.
    ///
    /// # Panics
    ///
    /// Panics with a [`crate::core::TestkitError::AssertionFailed`] naming
    /// every requested capability that is missing, or with
    /// [`crate::core::TestkitError::Misuse`] if an unknown capability name
    /// is requested.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::core::TestEnv;
    ///
    /// TestEnv::new().assert_ttl_capabilities(&[
    ///     "ttl_inspection",
    ///     "persistent_archival",
    /// ]);
    /// ```
    pub fn assert_ttl_capabilities(&self, required: &[&str]) {
        use crate::core::TestkitError;

        // Validate that every requested name is a known capability.
        const KNOWN: &[&str] = &[
            "ttl_inspection",
            "ttl_extension",
            "persistent_archival",
            "temporary_deletion",
            "contract_lifecycle",
            "state_archival_settings",
            "bulk_ttl_bump",
        ];
        let unknown: Vec<&&str> = required.iter().filter(|n| !KNOWN.contains(*n)).collect();
        if !unknown.is_empty() {
            panic!(
                "{}",
                TestkitError::Misuse(format!(
                    "unknown TTL capability name(s): [{}]. Valid names are: [{}]",
                    unknown
                        .iter()
                        .map(|n| n.to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                    KNOWN.join(", ")
                ))
            );
        }

        let caps = self.ttl_capabilities();
        let missing: Vec<&&str> = required
            .iter()
            .filter(|&&name| !match name {
                "ttl_inspection" => caps.ttl_inspection_supported(),
                "ttl_extension" => caps.ttl_extension_supported(),
                "persistent_archival" => caps.persistent_archival_supported(),
                "temporary_deletion" => caps.temporary_deletion_supported(),
                "contract_lifecycle" => caps.contract_lifecycle_supported(),
                "state_archival_settings" => caps.state_archival_settings_supported(),
                "bulk_ttl_bump" => caps.bulk_ttl_bump_supported(),
                _ => unreachable!("already validated above"),
            })
            .collect();

        if !missing.is_empty() {
            panic!(
                "{}",
                TestkitError::AssertionFailed(format!(
                    "environment (protocol {}) is missing required TTL capabilities: [{}]",
                    caps.protocol_version(),
                    missing
                        .iter()
                        .map(|n| n.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Unit tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ttl::{
        ARCHIVAL_PROTOCOL_CONTRACT_LIFECYCLE, ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH,
        ARCHIVAL_PROTOCOL_STATE_ARCHIVAL,
    };

    fn caps_at(protocol: u32) -> SdkTtlCapabilities {
        let env = TestEnv::with_protocol_version(protocol);
        env.ttl_capabilities()
    }

    // ── SdkTtlCapabilities accessors ─────────────────────────────────────────

    #[test]
    fn capabilities_reflect_live_ledger_info() {
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
    fn max_ttl_is_at_least_as_large_as_min_persistent_ttl() {
        let caps = TestEnv::new().ttl_capabilities();
        assert!(
            caps.max_entry_ttl() >= caps.min_persistent_entry_ttl(),
            "max_entry_ttl ({}) must be >= min_persistent_entry_ttl ({})",
            caps.max_entry_ttl(),
            caps.min_persistent_entry_ttl()
        );
    }

    #[test]
    fn max_ttl_is_at_least_as_large_as_min_temp_ttl() {
        let caps = TestEnv::new().ttl_capabilities();
        assert!(
            caps.max_entry_ttl() >= caps.min_temp_entry_ttl(),
            "max_entry_ttl ({}) must be >= min_temp_entry_ttl ({})",
            caps.max_entry_ttl(),
            caps.min_temp_entry_ttl()
        );
    }

    // ── Capability predicates at the Soroban launch protocol ─────────────────

    #[test]
    fn at_soroban_launch_ttl_inspection_and_extension_are_supported() {
        let caps = caps_at(ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH);
        assert!(caps.ttl_inspection_supported());
        assert!(caps.ttl_extension_supported());
    }

    #[test]
    fn at_soroban_launch_archival_semantics_are_supported() {
        let caps = caps_at(ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH);
        assert!(caps.persistent_archival_supported());
        assert!(caps.temporary_deletion_supported());
    }

    #[test]
    fn at_soroban_launch_contract_lifecycle_is_not_yet_supported() {
        let caps = caps_at(ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH);
        assert!(!caps.contract_lifecycle_supported());
    }

    #[test]
    fn at_soroban_launch_state_archival_settings_are_not_yet_supported() {
        let caps = caps_at(ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH);
        assert!(!caps.state_archival_settings_supported());
    }

    #[test]
    fn at_soroban_launch_bulk_ttl_bump_is_supported() {
        let caps = caps_at(ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH);
        assert!(caps.bulk_ttl_bump_supported());
    }

    // ── Capability predicates at the contract lifecycle protocol ─────────────

    #[test]
    fn at_contract_lifecycle_contract_lifecycle_is_supported() {
        let caps = caps_at(ARCHIVAL_PROTOCOL_CONTRACT_LIFECYCLE);
        assert!(caps.contract_lifecycle_supported());
    }

    #[test]
    fn at_contract_lifecycle_earlier_capabilities_are_still_supported() {
        let caps = caps_at(ARCHIVAL_PROTOCOL_CONTRACT_LIFECYCLE);
        assert!(caps.ttl_inspection_supported());
        assert!(caps.ttl_extension_supported());
        assert!(caps.persistent_archival_supported());
        assert!(caps.temporary_deletion_supported());
        assert!(caps.bulk_ttl_bump_supported());
    }

    #[test]
    fn at_contract_lifecycle_state_archival_settings_are_not_yet_supported() {
        let caps = caps_at(ARCHIVAL_PROTOCOL_CONTRACT_LIFECYCLE);
        assert!(!caps.state_archival_settings_supported());
    }

    // ── Capability predicates at the state archival protocol ─────────────────

    #[test]
    fn at_state_archival_all_capabilities_are_supported() {
        let caps = caps_at(ARCHIVAL_PROTOCOL_STATE_ARCHIVAL);
        assert!(caps.ttl_inspection_supported());
        assert!(caps.ttl_extension_supported());
        assert!(caps.persistent_archival_supported());
        assert!(caps.temporary_deletion_supported());
        assert!(caps.contract_lifecycle_supported());
        assert!(caps.state_archival_settings_supported());
        assert!(caps.bulk_ttl_bump_supported());
    }

    // ── Combination predicates ────────────────────────────────────────────────

    #[test]
    fn full_archival_recipe_requires_both_archival_and_inspection() {
        let caps = caps_at(ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH);
        assert!(caps.full_archival_recipe_supported());
    }

    #[test]
    fn temporary_boundary_recipe_requires_deletion_and_inspection() {
        let caps = caps_at(ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH);
        assert!(caps.temporary_boundary_recipe_supported());
    }

    // ── Supported / unsupported lists ────────────────────────────────────────

    #[test]
    fn at_state_archival_unsupported_list_is_empty() {
        let caps = caps_at(ARCHIVAL_PROTOCOL_STATE_ARCHIVAL);
        assert!(
            caps.unsupported_capabilities().is_empty(),
            "unexpected unsupported: {:?}",
            caps.unsupported_capabilities()
        );
    }

    #[test]
    fn at_soroban_launch_unsupported_list_contains_contract_lifecycle_and_state_archival_settings()
    {
        let caps = caps_at(ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH);
        let missing = caps.unsupported_capabilities();
        assert!(
            missing.contains(&"contract_lifecycle"),
            "expected contract_lifecycle in unsupported: {missing:?}"
        );
        assert!(
            missing.contains(&"state_archival_settings"),
            "expected state_archival_settings in unsupported: {missing:?}"
        );
    }

    #[test]
    fn supported_and_unsupported_lists_are_complementary() {
        const ALL: &[&str] = &[
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
            let caps = caps_at(protocol);
            let supported = caps.supported_capabilities();
            let unsupported = caps.unsupported_capabilities();
            // Every known capability appears in exactly one list.
            for name in ALL {
                let in_supported = supported.contains(name);
                let in_unsupported = unsupported.contains(name);
                assert!(
                    in_supported ^ in_unsupported,
                    "capability {name:?} must be in exactly one list at protocol {protocol}"
                );
            }
            // Together they cover all known capabilities.
            assert_eq!(
                supported.len() + unsupported.len(),
                ALL.len(),
                "supported + unsupported must cover all {len} capabilities at protocol {protocol}",
                len = ALL.len()
            );
        }
    }

    // ── Display ───────────────────────────────────────────────────────────────

    #[test]
    fn display_includes_protocol_version_and_all_capability_flags() {
        let caps = TestEnv::new().ttl_capabilities();
        let s = caps.to_string();
        assert!(
            s.contains(&caps.protocol_version().to_string()),
            "Display must include protocol version: {s}"
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
    }

    // ── TestEnv::ttl_capabilities round-trips ────────────────────────────────

    #[test]
    fn two_calls_to_ttl_capabilities_on_the_same_env_return_equal_snapshots() {
        let env = TestEnv::new();
        assert_eq!(env.ttl_capabilities(), env.ttl_capabilities());
    }

    #[test]
    fn capabilities_snapshot_is_independent_of_ledger_advance() {
        let env = TestEnv::new();
        let before = env.ttl_capabilities();
        env.advance_ledgers(100);
        let after = env.ttl_capabilities();
        // Advancing the sequence does not change protocol version or TTL params.
        assert_eq!(before.protocol_version(), after.protocol_version());
        assert_eq!(
            before.min_persistent_entry_ttl(),
            after.min_persistent_entry_ttl()
        );
        assert_eq!(before.max_entry_ttl(), after.max_entry_ttl());
    }

    // ── TestEnv::assert_full_ttl_support ─────────────────────────────────────

    #[test]
    fn assert_full_ttl_support_passes_at_state_archival_protocol() {
        TestEnv::with_protocol_version(ARCHIVAL_PROTOCOL_STATE_ARCHIVAL).assert_full_ttl_support();
    }

    #[test]
    #[should_panic(expected = "does not support the following TTL capabilities")]
    fn assert_full_ttl_support_panics_at_soroban_launch_protocol() {
        // At the launch protocol, contract_lifecycle and state_archival_settings
        // are missing, so assert_full_ttl_support must panic.
        TestEnv::with_protocol_version(ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH).assert_full_ttl_support();
    }

    // ── TestEnv::assert_ttl_capabilities ─────────────────────────────────────

    #[test]
    fn assert_ttl_capabilities_passes_when_all_requested_are_present() {
        let env = TestEnv::new();
        env.assert_ttl_capabilities(&["ttl_inspection", "ttl_extension", "persistent_archival"]);
    }

    #[test]
    #[should_panic(expected = "missing required TTL capabilities")]
    fn assert_ttl_capabilities_panics_when_a_requested_capability_is_missing() {
        // At the launch protocol, state_archival_settings is absent.
        TestEnv::with_protocol_version(ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH)
            .assert_ttl_capabilities(&["ttl_inspection", "state_archival_settings"]);
    }

    #[test]
    #[should_panic(expected = "unknown TTL capability name")]
    fn assert_ttl_capabilities_panics_on_unknown_capability_name() {
        TestEnv::new().assert_ttl_capabilities(&["nonexistent_capability"]);
    }

    #[test]
    fn assert_ttl_capabilities_with_empty_slice_always_passes() {
        TestEnv::with_protocol_version(ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH)
            .assert_ttl_capabilities(&[]);
    }

    // ── PartialEq / Clone ─────────────────────────────────────────────────────

    #[test]
    fn capabilities_from_same_seed_env_are_equal() {
        let a = TestEnv::with_seed(42).ttl_capabilities();
        let b = TestEnv::with_seed(99).ttl_capabilities();
        // Different seeds, same starting ledger — capabilities should match.
        assert_eq!(a, b);
    }

    #[test]
    fn capabilities_clone_is_equal_to_original() {
        let caps = TestEnv::new().ttl_capabilities();
        assert_eq!(caps.clone(), caps);
    }
}
