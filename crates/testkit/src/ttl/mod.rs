//! Ledger-entry TTL inspection and expiry simulation.
//!
//! Contracts that fail to bump TTL break in production through silent data
//! loss, and the failure mode is close to untestable by hand — this module
//! makes it a normal assertion.
//!
//! Beyond the raw [`StorageKind`] and [`crate::core::TestEnv::ttl_of`]
//! primitives this module ships the tools a production contract actually
//! needs: scoped execution on either side of the expiry boundary
//! ([`crate::core::TestEnv::assert_runs_before_expiry`] /
//! [`crate::core::TestEnv::assert_runs_after_expiry`]), a full [`TtlTimeline`]
//! report per entry, persistent-storage recovery recipes
//! ([`crate::core::TestEnv::assert_recovers_after_expiry`]),
//! temporary-storage expiry recipes
//! ([`crate::core::TestEnv::assert_temporary_expires`],
//! [`crate::core::TestEnv::assert_temporary_expiry_boundary`] and
//! [`crate::core::TestEnv::assert_temporary_extension_defers_expiry`]), and
//! protocol-version fixtures for archival behavior
//! ([`crate::core::TestEnv::with_protocol_version`] and
//! [`ArchivalParameters`]).
//!
//! # Per-storage-kind behavior (pinned SDK)
//!
//! Soroban has three storage kinds, and their TTL semantics differ in the one
//! way tests care about most: what happens once an entry runs out of rent.
//! The rules below are what the pinned `soroban-sdk` (27.0.6) host enforces.
//! Because they have changed across protocol versions, pin a version with
//! [`crate::core::TestEnv::with_protocol_version`] and snapshot
//! [`ArchivalParameters`] rather than hardcoding the values.
//!
//! | Kind | Minimum TTL on write | On expiry |
//! | ---- | -------------------- | --------- |
//! | [`StorageKind::Temporary`] | `min_temp_entry_ttl` | **Deleted** — gone for good; a later read reports the entry absent. |
//! | [`StorageKind::Persistent`] | `min_persistent_entry_ttl` | **Archived** — a later read restores it and reports its value again. |
//! | [`StorageKind::Instance`] | `min_persistent_entry_ttl` | **Archived** — restored on next use, like persistent. |
//!
//! What that means for tests:
//!
//! - A **temporary** entry cannot be recovered, so the only correct contract
//!   behavior after expiry is to treat it as absent. Exercise that with
//!   [`crate::core::TestEnv::assert_temporary_expires`] or
//!   [`crate::core::TestEnv::assert_temporary_expiry_boundary`].
//! - A **persistent** entry *is* recoverable: reading it after expiry restores
//!   it with a fresh TTL. Exercise that with
//!   [`crate::core::TestEnv::assert_recovers_after_expiry`]. Since a read is
//!   itself a restore, use a [`TtlTimeline`] snapshot (not a re-read) to ask
//!   "has it expired yet?".
//! - **Instance** storage holds exactly one entry per contract (its own data
//!   plus its code), so [`crate::core::TestEnv::ttl_of`] ignores the `key`
//!   argument for this kind.
//!
//! Headroom also differs by kind: a temporary entry is granted
//! `min_temp_entry_ttl` on write, while persistent and instance entries are
//! granted `min_persistent_entry_ttl`, and no kind may exceed `max_entry_ttl`.
//! [`crate::core::TestEnv::expire`] avoids per-entry differences entirely by
//! advancing one ledger past `max_entry_ttl`, which expires every kind at once.

mod capabilities;
mod expiry;

pub use capabilities::SdkTtlCapabilities;
pub use expiry::{
    ArchivalParameters, StorageKind, TtlSnapshot, TtlTimeline,
    ARCHIVAL_PROTOCOL_CONTRACT_LIFECYCLE, ARCHIVAL_PROTOCOL_SOROBAN_LAUNCH,
    ARCHIVAL_PROTOCOL_STATE_ARCHIVAL,
};
