//! Pre-built actor fixtures for common Soroban role matrices.
//!
//! Building a correct [`AuthMatrix`] requires enumerating every participant
//! that should be checked against every entry point.  For the handful of
//! role shapes that appear in nearly every contract — owner-only, a two-role
//! admin/user split, and the classic three-role admin/operator/viewer
//! hierarchy — the boilerplate is always the same.  These fixtures provide
//! ready-made actor sets so a test author can state the *shape* of access
//! control rather than rediscovering it each time.
//!
//! # Usage pattern
//!
//! 1. Pick the fixture that matches your contract's role shape.
//! 2. Register the fixture's actors as the `allowed` list on each entry
//!    point, then pass the fixture's full actor set to any entry point that
//!    should reject everyone (so the matrix cross-checks the right addresses).
//! 3. Hand the populated [`AuthMatrix`] to [`AuthMatrix::assert_enforced`].
//!
//! # Example — owner-only contract
//!
//! ```
//! use soroban_testkit::auth::{AuthMatrix, OwnerActors};
//! use soroban_testkit::core::TestEnv;
//!
//! let env = TestEnv::new();
//! let fixture = OwnerActors::new(&env);
//!
//! // Only the owner is allowed; everyone else (here the attacker) must be
//! // rejected.
//! let owner = fixture.owner.clone();
//! let allowed = fixture.allowed().to_vec();
//! AuthMatrix::new(&env)
//!     .entry_point(
//!         "privileged_op",
//!         allowed.as_slice(),
//!         move |caller| {
//!             if *caller == owner {
//!                 Ok(())
//!             } else {
//!                 Err(soroban_sdk::Error::from_contract_error(1))
//!             }
//!         },
//!     )
//!     .assert_enforced();
//! ```

use soroban_sdk::Address;

use crate::core::TestEnv;

// ──────────────────────────────────────────────────────────────────────────────
// OwnerActors — single privileged owner + one unprivileged challenger
// ──────────────────────────────────────────────────────────────────────────────

/// A two-actor fixture for contracts with a single privileged owner.
///
/// Provides the minimum set needed to prove that exactly one address may call
/// a protected entry point: the `owner` that should succeed, and an
/// `attacker` that should be rejected.  Both addresses are added to the
/// matrix as *known actors*, so the cross-check grid covers both sides
/// without requiring the test author to enumerate them.
///
/// # Example
///
/// ```
/// use soroban_testkit::auth::{AuthMatrix, OwnerActors};
/// use soroban_testkit::core::TestEnv;
///
/// let env = TestEnv::new();
/// let fixture = OwnerActors::new(&env);
///
/// let owner = fixture.owner.clone();
/// let allowed = fixture.allowed().to_vec();
/// AuthMatrix::new(&env)
///     .entry_point(
///         "admin_only",
///         allowed.as_slice(),
///         move |caller| {
///             if *caller == owner {
///                 Ok(())
///             } else {
///                 Err(soroban_sdk::Error::from_contract_error(1))
///             }
///         },
///     )
///     .assert_enforced();
/// ```
#[derive(Debug, Clone)]
pub struct OwnerActors {
    /// The privileged address; the only actor expected to pass.
    pub owner: Address,
    /// An unprivileged challenger; expected to be rejected on every
    /// owner-only entry point.
    pub attacker: Address,
}

impl OwnerActors {
    /// Generate a fresh `OwnerActors` fixture from `env`.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::auth::OwnerActors;
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let fixture = OwnerActors::new(&env);
    /// assert_ne!(fixture.owner, fixture.attacker);
    /// ```
    pub fn new(env: &TestEnv) -> Self {
        Self {
            owner: env.address(),
            attacker: env.address(),
        }
    }

    /// The slice of addresses that are *allowed* to call the entry point —
    /// just `[owner]`.
    ///
    /// Pass this to [`AuthMatrix::entry_point`] as the `allowed` argument.
    /// The fixture's full actor set (owner + attacker) is automatically
    /// covered as known actors once the matrix runs.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::auth::{AuthMatrix, OwnerActors};
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let fixture = OwnerActors::new(&env);
    /// assert_eq!(fixture.allowed(), &[fixture.owner.clone()]);
    /// ```
    pub fn allowed(&self) -> &[Address] {
        std::slice::from_ref(&self.owner)
    }

    /// Every address this fixture generated, in a fixed order.
    ///
    /// Useful when you need to register the full set with a second entry
    /// point so the matrix's known-actor set includes everyone.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::auth::OwnerActors;
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let fixture = OwnerActors::new(&env);
    /// assert_eq!(fixture.all().len(), 2);
    /// ```
    pub fn all(&self) -> Vec<Address> {
        vec![self.owner.clone(), self.attacker.clone()]
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// AdminUserActors — two privileged roles with different access levels
// ──────────────────────────────────────────────────────────────────────────────

/// A three-actor fixture for contracts with an admin and a regular user role.
///
/// This is the most common two-tier pattern: an `admin` may call everything, a
/// `user` may call a subset, and an `outsider` is rejected everywhere.
/// Registering both the admin-only and the admin+user entry points on the
/// same matrix ensures the grid cross-checks all three actors against all
/// entry points.
///
/// # Example
///
/// ```
/// use soroban_testkit::auth::{AdminUserActors, AuthMatrix};
/// use soroban_testkit::core::TestEnv;
///
/// let env = TestEnv::new();
/// let fx = AdminUserActors::new(&env);
///
/// // Pre-clone the fields the closures need so we don't hold a borrow of
/// // `fx` while also passing slices from it to `entry_point`.
/// let admin1 = fx.admin.clone();
/// let admin2 = fx.admin.clone();
/// let user = fx.user.clone();
/// let admin_only = fx.admin_only().to_vec();
/// let admin_and_user = fx.admin_and_user();
///
/// AuthMatrix::new(&env)
///     // admin-only operation
///     .entry_point(
///         "configure",
///         admin_only.as_slice(),
///         move |caller| {
///             if *caller == admin1 {
///                 Ok(())
///             } else {
///                 Err(soroban_sdk::Error::from_contract_error(1))
///             }
///         },
///     )
///     // operation open to both admin and user
///     .entry_point(
///         "deposit",
///         admin_and_user.as_slice(),
///         move |caller| {
///             if *caller == admin2 || *caller == user {
///                 Ok(())
///             } else {
///                 Err(soroban_sdk::Error::from_contract_error(1))
///             }
///         },
///     )
///     .assert_enforced();
/// ```
#[derive(Debug, Clone)]
pub struct AdminUserActors {
    /// The privileged administrator; allowed on all entry points.
    pub admin: Address,
    /// The regular user; allowed on user-accessible entry points only.
    pub user: Address,
    /// An outsider with no role; expected to be rejected everywhere.
    pub outsider: Address,
}

impl AdminUserActors {
    /// Generate a fresh `AdminUserActors` fixture from `env`.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::auth::AdminUserActors;
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let fx = AdminUserActors::new(&env);
    /// // All three addresses are distinct.
    /// assert_ne!(fx.admin, fx.user);
    /// assert_ne!(fx.admin, fx.outsider);
    /// assert_ne!(fx.user, fx.outsider);
    /// ```
    pub fn new(env: &TestEnv) -> Self {
        Self {
            admin: env.address(),
            user: env.address(),
            outsider: env.address(),
        }
    }

    /// Allowed list for admin-only entry points: `[admin]`.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::auth::AdminUserActors;
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let fx = AdminUserActors::new(&env);
    /// assert_eq!(fx.admin_only(), &[fx.admin.clone()]);
    /// ```
    pub fn admin_only(&self) -> &[Address] {
        std::slice::from_ref(&self.admin)
    }

    /// Allowed list for entry points accessible to both admin and user:
    /// `[admin, user]`.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::auth::AdminUserActors;
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let fx = AdminUserActors::new(&env);
    /// assert_eq!(fx.admin_and_user(), &[fx.admin.clone(), fx.user.clone()]);
    /// ```
    pub fn admin_and_user(&self) -> Vec<Address> {
        vec![self.admin.clone(), self.user.clone()]
    }

    /// Every address this fixture generated, in a fixed order.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::auth::AdminUserActors;
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let fx = AdminUserActors::new(&env);
    /// assert_eq!(fx.all().len(), 3);
    /// ```
    pub fn all(&self) -> Vec<Address> {
        vec![self.admin.clone(), self.user.clone(), self.outsider.clone()]
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// RbacActors — three-tier role-based access control
// ──────────────────────────────────────────────────────────────────────────────

/// A four-actor fixture for contracts with three distinct roles: admin,
/// operator, and viewer.
///
/// This covers the most common three-tier RBAC shape:
///
/// | Role | Typical access |
/// |---|---|
/// | `admin` | All entry points, including configuration and destruction |
/// | `operator` | Operational entry points (write, but not configure/destroy) |
/// | `viewer` | Read-only entry points only |
/// | `outsider` | No access — always rejected |
///
/// Registering the appropriate allowed lists per entry point and calling
/// [`AuthMatrix::assert_enforced`] proves the full 4 × N grid with one call.
///
/// # Example
///
/// ```
/// use soroban_testkit::auth::{AuthMatrix, RbacActors};
/// use soroban_testkit::core::TestEnv;
///
/// let env = TestEnv::new();
/// let fx = RbacActors::new(&env);
///
/// // Pre-clone fields captured by closures to avoid borrow conflicts.
/// let admin1 = fx.admin.clone();
/// let admin2 = fx.admin.clone();
/// let operator = fx.operator.clone();
/// let admin3 = fx.admin.clone();
/// let operator2 = fx.operator.clone();
/// let viewer = fx.viewer.clone();
/// let admin_only = fx.admin_only().to_vec();
/// let admin_and_op = fx.admin_and_operator();
/// let all_roles = fx.all_roles();
///
/// AuthMatrix::new(&env)
///     .entry_point("configure", admin_only.as_slice(), move |caller| {
///         if *caller == admin1 {
///             Ok(())
///         } else {
///             Err(soroban_sdk::Error::from_contract_error(1))
///         }
///     })
///     .entry_point("operate", admin_and_op.as_slice(), move |caller| {
///         if *caller == admin2 || *caller == operator {
///             Ok(())
///         } else {
///             Err(soroban_sdk::Error::from_contract_error(1))
///         }
///     })
///     .entry_point("read", all_roles.as_slice(), move |caller| {
///         if *caller == admin3 || *caller == operator2 || *caller == viewer {
///             Ok(())
///         } else {
///             Err(soroban_sdk::Error::from_contract_error(1))
///         }
///     })
///     .assert_enforced();
/// ```
#[derive(Debug, Clone)]
pub struct RbacActors {
    /// Full administrative access.
    pub admin: Address,
    /// Operational (write) access; cannot configure or destroy.
    pub operator: Address,
    /// Read-only access.
    pub viewer: Address,
    /// No role — always rejected.
    pub outsider: Address,
}

impl RbacActors {
    /// Generate a fresh `RbacActors` fixture from `env`.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::auth::RbacActors;
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let fx = RbacActors::new(&env);
    /// // All four addresses are distinct.
    /// assert_ne!(fx.admin, fx.operator);
    /// assert_ne!(fx.operator, fx.viewer);
    /// assert_ne!(fx.viewer, fx.outsider);
    /// ```
    pub fn new(env: &TestEnv) -> Self {
        Self {
            admin: env.address(),
            operator: env.address(),
            viewer: env.address(),
            outsider: env.address(),
        }
    }

    /// Allowed list for admin-only entry points: `[admin]`.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::auth::RbacActors;
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let fx = RbacActors::new(&env);
    /// assert_eq!(fx.admin_only(), &[fx.admin.clone()]);
    /// ```
    pub fn admin_only(&self) -> &[Address] {
        std::slice::from_ref(&self.admin)
    }

    /// Allowed list for entry points accessible to admin and operator:
    /// `[admin, operator]`.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::auth::RbacActors;
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let fx = RbacActors::new(&env);
    /// let allowed = fx.admin_and_operator();
    /// assert!(allowed.contains(&fx.admin));
    /// assert!(allowed.contains(&fx.operator));
    /// assert!(!allowed.contains(&fx.viewer));
    /// ```
    pub fn admin_and_operator(&self) -> Vec<Address> {
        vec![self.admin.clone(), self.operator.clone()]
    }

    /// Allowed list for entry points accessible to all three named roles:
    /// `[admin, operator, viewer]`.
    ///
    /// This is typically used for read-only or low-privilege entry points.
    /// The `outsider` is intentionally excluded; registering this on an entry
    /// point still causes the matrix to reject the outsider there.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::auth::RbacActors;
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let fx = RbacActors::new(&env);
    /// let roles = fx.all_roles();
    /// assert_eq!(roles.len(), 3);
    /// assert!(!roles.contains(&fx.outsider));
    /// ```
    pub fn all_roles(&self) -> Vec<Address> {
        vec![
            self.admin.clone(),
            self.operator.clone(),
            self.viewer.clone(),
        ]
    }

    /// Every address this fixture generated, including the outsider.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::auth::RbacActors;
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let fx = RbacActors::new(&env);
    /// assert_eq!(fx.all().len(), 4);
    /// assert!(fx.all().contains(&fx.outsider));
    /// ```
    pub fn all(&self) -> Vec<Address> {
        vec![
            self.admin.clone(),
            self.operator.clone(),
            self.viewer.clone(),
            self.outsider.clone(),
        ]
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// PeerActors — equal-privilege multi-party fixture
// ──────────────────────────────────────────────────────────────────────────────

/// A fixture for contracts where two or more equal peers each have symmetric
/// access — multi-signature schemes, shared liquidity pools, or co-owned
/// vaults where every participant may call any permitted entry point.
///
/// The fixture generates `n` peer addresses (all with identical access) plus
/// one outsider who must always be rejected.  `n` must be at least 2 and at
/// most 16; outside that range the constructor panics with a clear message.
///
/// # Example
///
/// ```
/// use soroban_testkit::auth::{AuthMatrix, PeerActors};
/// use soroban_testkit::core::TestEnv;
///
/// let env = TestEnv::new();
/// let fx = PeerActors::new(&env, 3);
///
/// // Every peer may call the operation; the outsider must be rejected.
/// AuthMatrix::new(&env)
///     .entry_point("vote", fx.peers(), |caller| {
///         if fx.is_peer(caller) {
///             Ok(())
///         } else {
///             Err(soroban_sdk::Error::from_contract_error(1))
///         }
///     })
///     .assert_enforced();
/// ```
#[derive(Debug, Clone)]
pub struct PeerActors {
    /// The equal-privilege peer addresses.
    peers: Vec<Address>,
    /// An outsider with no role; expected to be rejected everywhere.
    pub outsider: Address,
}

impl PeerActors {
    /// The minimum number of peers that may be requested.
    pub const MIN_PEERS: usize = 2;

    /// The maximum number of peers that may be requested.
    pub const MAX_PEERS: usize = 16;

    /// Generate a fresh `PeerActors` fixture with `n` equal-privilege peers
    /// and one outsider.
    ///
    /// # Panics
    ///
    /// Panics with a clear message if `n < 2` or `n > 16`.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::auth::PeerActors;
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let fx = PeerActors::new(&env, 4);
    /// assert_eq!(fx.peers().len(), 4);
    /// assert_ne!(fx.peers()[0], fx.outsider);
    /// ```
    pub fn new(env: &TestEnv, n: usize) -> Self {
        assert!(
            n >= Self::MIN_PEERS,
            "PeerActors requires at least {} peers, got {}",
            Self::MIN_PEERS,
            n
        );
        assert!(
            n <= Self::MAX_PEERS,
            "PeerActors supports at most {} peers, got {}",
            Self::MAX_PEERS,
            n
        );
        let peers = (0..n).map(|_| env.address()).collect();
        let outsider = env.address();
        Self { peers, outsider }
    }

    /// The peer addresses (all equal-privilege).
    ///
    /// Pass this to [`crate::auth::AuthMatrix::entry_point`] as the `allowed`
    /// argument for operations that every peer may call.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::auth::PeerActors;
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let fx = PeerActors::new(&env, 2);
    /// assert_eq!(fx.peers().len(), 2);
    /// ```
    pub fn peers(&self) -> &[Address] {
        &self.peers
    }

    /// Returns `true` if `addr` is one of the peer addresses.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::auth::PeerActors;
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let fx = PeerActors::new(&env, 2);
    /// assert!(fx.is_peer(&fx.peers()[0]));
    /// assert!(!fx.is_peer(&fx.outsider));
    /// ```
    pub fn is_peer(&self, addr: &Address) -> bool {
        self.peers.contains(addr)
    }

    /// Every address this fixture generated, including the outsider.
    ///
    /// # Example
    ///
    /// ```
    /// use soroban_testkit::auth::PeerActors;
    /// use soroban_testkit::core::TestEnv;
    ///
    /// let env = TestEnv::new();
    /// let fx = PeerActors::new(&env, 3);
    /// assert_eq!(fx.all().len(), 4); // 3 peers + 1 outsider
    /// ```
    pub fn all(&self) -> Vec<Address> {
        let mut all = self.peers.clone();
        all.push(self.outsider.clone());
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── OwnerActors ──────────────────────────────────────────────────────────

    #[test]
    fn owner_actors_generates_two_distinct_addresses() {
        let env = TestEnv::new();
        let fx = OwnerActors::new(&env);
        assert_ne!(fx.owner, fx.attacker);
    }

    #[test]
    fn owner_actors_allowed_contains_only_owner() {
        let env = TestEnv::new();
        let fx = OwnerActors::new(&env);
        let allowed = fx.allowed();
        assert_eq!(allowed.len(), 1);
        assert_eq!(allowed[0], fx.owner);
        assert!(!allowed.contains(&fx.attacker));
    }

    #[test]
    fn owner_actors_all_contains_both_addresses() {
        let env = TestEnv::new();
        let fx = OwnerActors::new(&env);
        let all = fx.all();
        assert_eq!(all.len(), 2);
        assert!(all.contains(&fx.owner));
        assert!(all.contains(&fx.attacker));
    }

    #[test]
    fn owner_actors_is_reproducible_from_the_same_seed() {
        let a = OwnerActors::new(&TestEnv::with_seed(7));
        let b = OwnerActors::new(&TestEnv::with_seed(7));
        assert_eq!(a.owner, b.owner);
        assert_eq!(a.attacker, b.attacker);
    }

    // ── AdminUserActors ──────────────────────────────────────────────────────

    #[test]
    fn admin_user_actors_generates_three_distinct_addresses() {
        let env = TestEnv::new();
        let fx = AdminUserActors::new(&env);
        assert_ne!(fx.admin, fx.user);
        assert_ne!(fx.admin, fx.outsider);
        assert_ne!(fx.user, fx.outsider);
    }

    #[test]
    fn admin_only_list_contains_only_admin() {
        let env = TestEnv::new();
        let fx = AdminUserActors::new(&env);
        let list = fx.admin_only();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0], fx.admin);
    }

    #[test]
    fn admin_and_user_list_contains_admin_and_user_not_outsider() {
        let env = TestEnv::new();
        let fx = AdminUserActors::new(&env);
        let list = fx.admin_and_user();
        assert_eq!(list.len(), 2);
        assert!(list.contains(&fx.admin));
        assert!(list.contains(&fx.user));
        assert!(!list.contains(&fx.outsider));
    }

    #[test]
    fn admin_user_actors_all_contains_all_three() {
        let env = TestEnv::new();
        let fx = AdminUserActors::new(&env);
        let all = fx.all();
        assert_eq!(all.len(), 3);
        assert!(all.contains(&fx.admin));
        assert!(all.contains(&fx.user));
        assert!(all.contains(&fx.outsider));
    }

    // ── RbacActors ───────────────────────────────────────────────────────────

    #[test]
    fn rbac_actors_generates_four_distinct_addresses() {
        let env = TestEnv::new();
        let fx = RbacActors::new(&env);
        let all = fx.all();
        // All four must be unique.
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(all[i], all[j], "addresses at {i} and {j} collided");
            }
        }
    }

    #[test]
    fn rbac_admin_only_excludes_operator_viewer_outsider() {
        let env = TestEnv::new();
        let fx = RbacActors::new(&env);
        let list = fx.admin_only();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0], fx.admin);
        assert!(!list.contains(&fx.operator));
        assert!(!list.contains(&fx.viewer));
        assert!(!list.contains(&fx.outsider));
    }

    #[test]
    fn rbac_admin_and_operator_excludes_viewer_and_outsider() {
        let env = TestEnv::new();
        let fx = RbacActors::new(&env);
        let list = fx.admin_and_operator();
        assert_eq!(list.len(), 2);
        assert!(list.contains(&fx.admin));
        assert!(list.contains(&fx.operator));
        assert!(!list.contains(&fx.viewer));
        assert!(!list.contains(&fx.outsider));
    }

    #[test]
    fn rbac_all_roles_excludes_outsider() {
        let env = TestEnv::new();
        let fx = RbacActors::new(&env);
        let roles = fx.all_roles();
        assert_eq!(roles.len(), 3);
        assert!(roles.contains(&fx.admin));
        assert!(roles.contains(&fx.operator));
        assert!(roles.contains(&fx.viewer));
        assert!(!roles.contains(&fx.outsider));
    }

    #[test]
    fn rbac_all_includes_outsider() {
        let env = TestEnv::new();
        let fx = RbacActors::new(&env);
        let all = fx.all();
        assert_eq!(all.len(), 4);
        assert!(all.contains(&fx.outsider));
    }

    // ── PeerActors ───────────────────────────────────────────────────────────

    #[test]
    fn peer_actors_generates_n_distinct_peers_and_one_outsider() {
        let env = TestEnv::new();
        let fx = PeerActors::new(&env, 3);
        assert_eq!(fx.peers().len(), 3);
        let all = fx.all();
        assert_eq!(all.len(), 4);
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(all[i], all[j], "addresses at {i} and {j} collided");
            }
        }
    }

    #[test]
    fn peer_actors_is_peer_returns_true_for_peers_only() {
        let env = TestEnv::new();
        let fx = PeerActors::new(&env, 3);
        for peer in fx.peers() {
            assert!(fx.is_peer(peer), "expected {peer:?} to be a peer");
        }
        assert!(!fx.is_peer(&fx.outsider));
    }

    #[test]
    #[should_panic(expected = "PeerActors requires at least 2 peers, got 1")]
    fn peer_actors_rejects_fewer_than_two_peers() {
        let env = TestEnv::new();
        let _ = PeerActors::new(&env, 1);
    }

    #[test]
    #[should_panic(expected = "PeerActors supports at most 16 peers, got 17")]
    fn peer_actors_rejects_more_than_sixteen_peers() {
        let env = TestEnv::new();
        let _ = PeerActors::new(&env, 17);
    }

    #[test]
    fn peer_actors_boundary_values_are_accepted() {
        let env = TestEnv::new();
        let min = PeerActors::new(&env, PeerActors::MIN_PEERS);
        assert_eq!(min.peers().len(), PeerActors::MIN_PEERS);
        let max = PeerActors::new(&env, PeerActors::MAX_PEERS);
        assert_eq!(max.peers().len(), PeerActors::MAX_PEERS);
    }
}
