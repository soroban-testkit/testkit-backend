//! Focused coverage for the definition of a "known actor" in `AuthMatrix`
//! reports, plus auth-state reset behaviour across actor cells.

use soroban_sdk::testutils::{MockAuth, MockAuthInvoke};
use soroban_sdk::{contract, contractimpl, Address, Env, IntoVal};
use soroban_testkit::auth::AuthMatrix;
use soroban_testkit::core::TestEnv;

#[contract]
struct SecureBank;

#[contractimpl]
impl SecureBank {
    pub fn privileged(env: Env, caller: Address) {
        caller.require_auth();
        // Dummy operation
        let _ = env;
    }
}

#[test]
fn known_actors_is_the_deduped_union_in_first_seen_order() {
    let env = TestEnv::new();
    let alice = env.address();
    let bob = env.address();
    let carol = env.address();

    let matrix = AuthMatrix::new(&env)
        .entry_point("a", &[alice.clone(), bob.clone()], |_| Ok(()))
        .entry_point("b", &[bob.clone(), carol.clone()], |_| Ok(()));

    // `bob` is named on both entry points but appears once.
    assert_eq!(matrix.known_actors(), vec![alice, bob, carol]);
}

#[test]
fn addresses_never_listed_are_not_known_actors() {
    let env = TestEnv::new();
    let listed = env.address();
    let never_listed = env.address();

    let matrix =
        AuthMatrix::new(&env).entry_point("only", std::slice::from_ref(&listed), |_| Ok(()));

    let known = matrix.known_actors();
    assert_eq!(known, vec![listed]);
    assert!(!known.contains(&never_listed));
}

#[test]
fn report_states_the_known_actor_definition_and_names_the_offender() {
    let env = TestEnv::new();
    let owner = env.address();
    let attacker = env.address();

    // `open` allows both callers, so both are known actors; `guarded` is
    // buggy and lets everyone through, so the known actor `attacker` wrongly
    // succeeds there and must be flagged.
    let matrix = AuthMatrix::new(&env)
        .entry_point("open", &[owner.clone(), attacker.clone()], |_| Ok(()))
        .entry_point("guarded", std::slice::from_ref(&owner), |_| Ok(()));

    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| matrix.assert_enforced()))
        .expect_err("guarded allows everyone, so the attacker cell must be caught");
    let message = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();

    assert!(
        message.contains("known actors = union of all entry points' allowed lists"),
        "the report must state the known-actor definition: {message}"
    );
    assert!(
        message.contains(&format!("{attacker:?}")),
        "the report must name the offending known actor: {message}"
    );
    assert!(
        message.contains("guarded"),
        "the report must name the entry point that let the actor through: {message}"
    );
}

#[test]
fn auth_state_is_reset_between_independent_actor_cases() {
    let env = TestEnv::new();
    let contract_id = env.env().register(SecureBank, ());
    let alice = env.address();
    let _bob = env.address();
    let client = SecureBankClient::new(env.env(), &contract_id);

    // Build an AuthMatrix where only alice is allowed to call `privileged`.
    // In the closure, we mock auth for whatever caller is passed, BUT if caller is not alice,
    // we do not mock auth (or contract rejects).
    // Specifically, if alice runs first and mocks auth, bob's case runs with clean auth state.
    let matrix = AuthMatrix::new(&env).entry_point(
        "privileged",
        std::slice::from_ref(&alice),
        |caller: &Address| {
            if *caller == alice {
                match client
                    .mock_auths(&[MockAuth {
                        address: caller,
                        invoke: &MockAuthInvoke {
                            contract: &contract_id,
                            fn_name: "privileged",
                            args: (caller.clone(),).into_val(env.env()),
                            sub_invokes: &[],
                        },
                    }])
                    .try_privileged(caller)
                {
                    Ok(Ok(())) => Ok(()),
                    _ => Err(soroban_sdk::Error::from_contract_error(1)),
                }
            } else {
                match client.try_privileged(caller) {
                    Ok(Ok(())) => Ok(()),
                    _ => Err(soroban_sdk::Error::from_contract_error(1)),
                }
            }
        },
    );

    // Before and after each actor cell, auth is automatically reset.
    // If auth leaked from alice to bob, bob would unexpectedly pass!
    // Since bob is rejected and alice succeeds, assert_enforced passes.
    matrix.assert_enforced();
}

#[test]
fn reset_auth_helpers_work_on_matrix_and_env() {
    let env = TestEnv::new();
    let matrix = AuthMatrix::new(&env);

    // Call reset_auth on both instances without error
    matrix.reset_auth();
    env.reset_auth();
}

#[test]
fn assert_requires_auth_resets_auth_state() {
    let env = TestEnv::new();
    let contract_id = env.env().register(SecureBank, ());
    let alice = env.address();
    let client = SecureBankClient::new(env.env(), &contract_id);

    // Without mocked auth, calling privileged should fail
    env.assert_requires_auth(|| {
        client.privileged(&alice);
    });

    // After assert_requires_auth, environment auth state remains clean
    env.reset_auth();
}

// ──────────────────────────────────────────────────────────────────────────────
// Issue #403 — named actor fixtures for common role matrices
// ──────────────────────────────────────────────────────────────────────────────

use soroban_testkit::auth::{AdminUserActors, OwnerActors, PeerActors, RbacActors};

// ── OwnerActors ───────────────────────────────────────────────────────────────

#[test]
fn owner_actors_enforced_owner_only_matrix_passes() {
    let env = TestEnv::new();
    let fx = OwnerActors::new(&env);

    // Both owner and attacker are registered as allowed on the "open" entry
    // point so that both become known actors; only owner is allowed on the
    // guarded one.  assert_enforced must pass because the guarded closure
    // correctly rejects the attacker.
    let owner = fx.owner.clone();
    let attacker = fx.attacker.clone();
    AuthMatrix::new(&env)
        .entry_point("open", fx.all().as_slice(), |_| Ok(()))
        .entry_point("guarded", fx.allowed(), move |caller| {
            if *caller == owner {
                Ok(())
            } else {
                Err(soroban_sdk::Error::from_contract_error(1))
            }
        })
        .assert_enforced();
    let _ = attacker; // consumed only to suppress the unused-variable lint
}

#[test]
fn owner_actors_enforced_catches_missing_auth_on_guarded_entry_point() {
    let env = TestEnv::new();
    let fx = OwnerActors::new(&env);

    let matrix = AuthMatrix::new(&env)
        // Both actors become known from this entry point.
        .entry_point("open", fx.all().as_slice(), |_| Ok(()))
        // Bug: guarded lets everyone through.
        .entry_point("guarded", fx.allowed(), |_| Ok(()));

    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| matrix.assert_enforced()))
        .expect_err("guarded lets the attacker through — must fail");
    let msg = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();

    assert!(
        msg.contains(&format!("{:?}", fx.attacker)),
        "failure must name the attacker: {msg}"
    );
    assert!(
        msg.contains("guarded"),
        "failure must name the entry point: {msg}"
    );
}

#[test]
fn owner_actors_allowed_slice_is_only_owner() {
    let env = TestEnv::new();
    let fx = OwnerActors::new(&env);
    assert_eq!(fx.allowed(), &[fx.owner.clone()]);
    assert!(!fx.allowed().contains(&fx.attacker));
}

// ── AdminUserActors ───────────────────────────────────────────────────────────

#[test]
fn admin_user_actors_enforced_two_tier_matrix_passes() {
    let env = TestEnv::new();
    let fx = AdminUserActors::new(&env);

    let admin = fx.admin.clone();
    let user_addr = fx.user.clone();
    let admin2 = fx.admin.clone();
    AuthMatrix::new(&env)
        .entry_point("admin_op", fx.admin_only(), move |caller| {
            if *caller == admin {
                Ok(())
            } else {
                Err(soroban_sdk::Error::from_contract_error(1))
            }
        })
        .entry_point("user_op", fx.admin_and_user().as_slice(), move |caller| {
            if *caller == admin2 || *caller == user_addr {
                Ok(())
            } else {
                Err(soroban_sdk::Error::from_contract_error(1))
            }
        })
        .assert_enforced();
}

#[test]
fn admin_user_actors_enforced_catches_user_escaping_to_admin_op() {
    let env = TestEnv::new();
    let fx = AdminUserActors::new(&env);

    // Bug: admin_op lets both admin and user through.
    let matrix = AuthMatrix::new(&env)
        .entry_point("admin_op", fx.admin_only(), |_| Ok(()))
        .entry_point("user_op", fx.admin_and_user().as_slice(), |_| Ok(()));

    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| matrix.assert_enforced()))
        .expect_err("user must not access admin_op");
    let msg = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    assert!(
        msg.contains("admin_op"),
        "failure must name the entry point: {msg}"
    );
}

#[test]
fn admin_user_actors_all_contains_three_distinct_addresses() {
    let env = TestEnv::new();
    let fx = AdminUserActors::new(&env);
    let all = fx.all();
    assert_eq!(all.len(), 3);
    assert_ne!(all[0], all[1]);
    assert_ne!(all[1], all[2]);
    assert_ne!(all[0], all[2]);
}

// ── RbacActors ────────────────────────────────────────────────────────────────

#[test]
fn rbac_actors_enforced_three_tier_matrix_passes() {
    let env = TestEnv::new();
    let fx = RbacActors::new(&env);

    let admin = fx.admin.clone();
    let admin2 = fx.admin.clone();
    let operator = fx.operator.clone();
    let admin3 = fx.admin.clone();
    let operator2 = fx.operator.clone();
    let viewer = fx.viewer.clone();

    AuthMatrix::new(&env)
        .entry_point("configure", fx.admin_only(), move |caller| {
            if *caller == admin {
                Ok(())
            } else {
                Err(soroban_sdk::Error::from_contract_error(1))
            }
        })
        .entry_point(
            "operate",
            fx.admin_and_operator().as_slice(),
            move |caller| {
                if *caller == admin2 || *caller == operator {
                    Ok(())
                } else {
                    Err(soroban_sdk::Error::from_contract_error(1))
                }
            },
        )
        .entry_point("read", fx.all_roles().as_slice(), move |caller| {
            if *caller == admin3 || *caller == operator2 || *caller == viewer {
                Ok(())
            } else {
                Err(soroban_sdk::Error::from_contract_error(1))
            }
        })
        .assert_enforced();
}

#[test]
fn rbac_actors_enforced_catches_viewer_escalating_to_operator() {
    let env = TestEnv::new();
    let fx = RbacActors::new(&env);

    // Bug: operate lets all three roles through (viewer should not be allowed).
    let matrix = AuthMatrix::new(&env)
        .entry_point("configure", fx.admin_only(), |_| {
            Err(soroban_sdk::Error::from_contract_error(1))
        })
        .entry_point("operate", fx.all_roles().as_slice(), |_| Ok(()));

    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| matrix.assert_enforced()))
        .expect_err("viewer must not pass operate");
    let msg = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    assert!(
        msg.contains("operate"),
        "failure must name the entry point: {msg}"
    );
}

#[test]
fn rbac_actors_outsider_is_not_in_any_allowed_list() {
    let env = TestEnv::new();
    let fx = RbacActors::new(&env);
    assert!(!fx.admin_only().contains(&fx.outsider));
    assert!(!fx.admin_and_operator().contains(&fx.outsider));
    assert!(!fx.all_roles().contains(&fx.outsider));
}

// ── PeerActors ────────────────────────────────────────────────────────────────

#[test]
fn peer_actors_enforced_symmetric_access_passes() {
    let env = TestEnv::new();
    let fx = PeerActors::new(&env, 3);
    let peers_clone: Vec<_> = fx.peers().to_vec();

    AuthMatrix::new(&env)
        .entry_point("vote", fx.peers(), move |caller| {
            if peers_clone.contains(caller) {
                Ok(())
            } else {
                Err(soroban_sdk::Error::from_contract_error(1))
            }
        })
        .assert_enforced();
}

#[test]
fn peer_actors_enforced_catches_outsider_slipping_through() {
    let env = TestEnv::new();
    let fx = PeerActors::new(&env, 2);

    // Bug: vote lets everyone through.
    // Both peers AND the outsider become known actors once both are on any
    // entry point — here we seed them all via a second "open" entry point
    // so the matrix has all three in its grid.
    let all = fx.all();
    let matrix = AuthMatrix::new(&env)
        .entry_point("open", all.as_slice(), |_| Ok(()))
        .entry_point("vote", fx.peers(), |_| Ok(()));

    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| matrix.assert_enforced()))
        .expect_err("outsider must not pass vote");
    let msg = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    assert!(
        msg.contains(&format!("{:?}", fx.outsider)),
        "failure must name the outsider: {msg}"
    );
}

#[test]
fn peer_actors_minimum_and_maximum_peer_counts_are_accepted() {
    let env = TestEnv::new();
    let min = PeerActors::new(&env, PeerActors::MIN_PEERS);
    assert_eq!(min.peers().len(), PeerActors::MIN_PEERS);
    let max = PeerActors::new(&env, PeerActors::MAX_PEERS);
    assert_eq!(max.peers().len(), PeerActors::MAX_PEERS);
}

#[test]
#[should_panic(expected = "PeerActors requires at least 2 peers")]
fn peer_actors_rejects_single_peer_with_actionable_message() {
    let env = TestEnv::new();
    let _ = PeerActors::new(&env, 1);
}

#[test]
#[should_panic(expected = "PeerActors supports at most 16 peers")]
fn peer_actors_rejects_excessive_peer_count_with_actionable_message() {
    let env = TestEnv::new();
    let _ = PeerActors::new(&env, 17);
}

// ── Fixture reproducibility (edge cases) ─────────────────────────────────────

#[test]
fn all_fixtures_are_reproducible_from_the_same_seed() {
    let owner_a = OwnerActors::new(&TestEnv::with_seed(99));
    let owner_b = OwnerActors::new(&TestEnv::with_seed(99));
    assert_eq!(owner_a.owner, owner_b.owner);
    assert_eq!(owner_a.attacker, owner_b.attacker);

    let au_a = AdminUserActors::new(&TestEnv::with_seed(99));
    let au_b = AdminUserActors::new(&TestEnv::with_seed(99));
    assert_eq!(au_a.admin, au_b.admin);
    assert_eq!(au_a.user, au_b.user);
    assert_eq!(au_a.outsider, au_b.outsider);

    let rb_a = RbacActors::new(&TestEnv::with_seed(99));
    let rb_b = RbacActors::new(&TestEnv::with_seed(99));
    assert_eq!(rb_a.admin, rb_b.admin);
    assert_eq!(rb_a.operator, rb_b.operator);
    assert_eq!(rb_a.viewer, rb_b.viewer);
    assert_eq!(rb_a.outsider, rb_b.outsider);

    let peer_a = PeerActors::new(&TestEnv::with_seed(99), 3);
    let peer_b = PeerActors::new(&TestEnv::with_seed(99), 3);
    assert_eq!(peer_a.peers(), peer_b.peers());
    assert_eq!(peer_a.outsider, peer_b.outsider);
}

#[test]
fn fixtures_from_the_same_env_generate_non_overlapping_addresses() {
    let env = TestEnv::new();
    let owner_fx = OwnerActors::new(&env);
    let au_fx = AdminUserActors::new(&env);

    // All six addresses drawn from the same env must be distinct.
    let combined: Vec<_> = owner_fx.all().into_iter().chain(au_fx.all()).collect();
    for i in 0..combined.len() {
        for j in (i + 1)..combined.len() {
            assert_ne!(combined[i], combined[j], "address collision at {i} vs {j}");
        }
    }
}
