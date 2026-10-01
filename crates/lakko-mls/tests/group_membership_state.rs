// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `Group::is_active` — property tests for the membership-state accessor
//! ([ADR-0045] §6.2 of `docs/ux/proposal-based-membership.md`).
//!
//! **What is deliberately NOT tested here.** "`is_active` is false after a
//! commit that removes your own leaf" is a property of OpenMLS, not of Lakko —
//! `merge_staged_commit` sets `MlsGroupState::Inactive` on
//! `staged_commit.self_removed()` and this crate does not participate in that
//! decision. Asserting it would pin a dependency's behaviour and pass
//! vacuously the day we stopped calling the accessor at all.
//!
//! **What IS tested is our claim.** Every client can already infer its own
//! membership from the roster — *"no `MemberRecord` whose
//! `signature_public_key` is mine"* — and that inference is sound today. The
//! accessor exists so three clients do not each hand-roll it. The property
//! that earns its place is therefore that **the accessor and the roster
//! inference return the same answer on every path that reaches an eviction**,
//! asserted on both sides of the eviction and from every vantage point in the
//! group, so:
//!
//! * a client that replaces the inference with the accessor cannot regress; and
//! * the day the ADR-0022 Phase-2b per-device-leaf work makes a leaf signature
//!   key stop being the account key — the one genuine hole in the inference,
//!   which fails silently and in the dangerous direction — CI says so instead
//!   of a user.
//!
//! The two eviction paths that exist are both covered: the ADR-0045 proposal
//! path (an admin proposes, the owner sweeps) and the direct-commit path that
//! survives on a group with no role table.
//!
//! [ADR-0045]: ../../../docs/adr/0045-remote-commit-epoch-fork.md

use lakko_mls::{key_package, Group, GroupKind, Identity, LeafNodeIndex, RoleTable};

fn wire(msg: openmls::framing::MlsMessageOut) -> Vec<u8> {
    lakko_mls::group::encode_message(&msg).expect("encode message")
}

fn kp_wire(id: &Identity) -> Vec<u8> {
    key_package::serialize(&id.create_key_package().expect("key package")).expect("serialize kp")
}

fn pk32(id: &Identity) -> [u8; 32] {
    id.signature_public_key()
        .try_into()
        .expect("Ed25519 signature key is 32 bytes")
}

fn leaf_of(g: &Group, pubkey: &[u8]) -> LeafNodeIndex {
    let idx = g
        .members()
        .iter()
        .find(|m| m.signature_public_key == pubkey)
        .expect("member present")
        .leaf_index;
    LeafNodeIndex::new(idx)
}

/// The inference every client can already write: is my own signature key in
/// the roster this handle reports?
fn roster_says_member(g: &Group, id: &Identity) -> bool {
    g.members()
        .iter()
        .any(|m| m.signature_public_key == id.signature_public_key())
}

/// **The property.** Assert the accessor and the inference agree, and return
/// the (single, now-unambiguous) answer so the caller can pin its value too.
///
/// `where_` names the vantage point so a failure says which handle disagreed
/// rather than only that one did.
#[track_caller]
fn agree(g: &Group, id: &Identity, where_: &str) -> bool {
    let accessor = g.is_active();
    let inference = roster_says_member(g, id);
    assert_eq!(
        accessor, inference,
        "{where_}: is_active() said {accessor} but the roster inference said \
         {inference} — the accessor and the inference must never diverge, \
         because clients treat them as the same question",
    );
    accessor
}

struct Scene {
    alice: Identity,
    bob: Identity,
    charlie: Identity,
    alice_g: Group,
    bob_g: Group,
    charlie_g: Group,
}

/// owner = alice, admin = bob, member = charlie — the same shape the ADR-0022
/// and ADR-0045 suites use, so the three can be read side by side.
fn scene(tag: u8) -> Scene {
    let alice = Identity::from_seed(&[0xA0 ^ tag; 64]).expect("alice");
    let bob = Identity::from_seed(&[0xB0 ^ tag; 64]).expect("bob");
    let charlie = Identity::from_seed(&[0xC0 ^ tag; 64]).expect("charlie");

    let mut alice_g = Group::create(&alice, GroupKind::Group).expect("alice creates group");

    let add_bob = alice_g
        .add_member_from_wire(&alice, &kp_wire(&bob))
        .expect("alice adds bob");
    let mut bob_g = Group::join_from_welcome(&wire(add_bob.welcome), &bob).expect("bob joins");

    let add_c = alice_g
        .add_member_from_wire(&alice, &kp_wire(&charlie))
        .expect("alice adds charlie");
    bob_g
        .process_incoming_verifying_roles(&bob, &wire(add_c.commit))
        .expect("bob applies owner's add-charlie");
    let mut charlie_g =
        Group::join_from_welcome(&wire(add_c.welcome), &charlie).expect("charlie joins");

    let roles = RoleTable::new(pk32(&alice), vec![pk32(&bob)]);
    let promote = alice_g
        .set_group_roles(&alice, &roles)
        .expect("alice promotes bob");
    bob_g
        .process_incoming_verifying_roles(&bob, &promote)
        .expect("bob applies promote");
    charlie_g
        .process_incoming_verifying_roles(&charlie, &promote)
        .expect("charlie applies promote");

    Scene {
        alice,
        bob,
        charlie,
        alice_g,
        bob_g,
        charlie_g,
    }
}

// ============================================================================
// The positive case — a healthy group, from every vantage point
// ============================================================================

/// The baseline the eviction tests are measured against: every member of a
/// healthy group reads active AND finds itself in its own roster. Without this,
/// an accessor hard-wired to `false` would satisfy every eviction assertion.
#[test]
fn every_member_of_a_healthy_group_is_active_and_in_its_own_roster() {
    let s = scene(0x01);

    assert!(agree(&s.alice_g, &s.alice, "owner"));
    assert!(agree(&s.bob_g, &s.bob, "admin"));
    assert!(agree(&s.charlie_g, &s.charlie, "member"));
}

// ============================================================================
// Eviction path 1 — the ADR-0045 proposal path
// ============================================================================

/// An admin proposes the removal, the owner sweeps it into one commit, and the
/// evicted device applies that commit off the journal like any other.
///
/// This is the path a role-enabled group actually takes, and it is the one the
/// clients will gate their composer on.
#[test]
fn the_accessor_and_the_inference_agree_across_a_proposal_path_eviction() {
    let mut s = scene(0x02);

    // Before: everyone agrees, and everyone is in.
    assert!(agree(&s.charlie_g, &s.charlie, "charlie before"));
    assert!(agree(&s.alice_g, &s.alice, "alice before"));
    assert!(agree(&s.bob_g, &s.bob, "bob before"));

    // bob (admin) proposes charlie's removal.
    let charlie_leaf = leaf_of(&s.alice_g, s.charlie.signature_public_key());
    let proposal = s
        .bob_g
        .propose_remove_member(&s.bob, charlie_leaf)
        .expect("an admin may propose a removal");
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &proposal)
        .expect("alice stores bob's proposal");
    s.charlie_g
        .process_incoming_verifying_roles(&s.charlie, &proposal)
        .expect("charlie stores the proposal that names its own leaf");

    // alice (owner) sweeps it into exactly one commit.
    let sweep = s
        .alice_g
        .commit_pending_proposals(&s.alice)
        .expect("sweep")
        .expect("the sweep must produce a commit");
    let commit = wire(sweep.commit);

    // The evicted device applies its own removal off the journal — this does
    // not error, it merges and the group goes terminal.
    s.charlie_g
        .process_incoming_verifying_roles(&s.charlie, &commit)
        .expect("the removing commit applies on the evicted device");
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &commit)
        .expect("bob applies the removal");

    // After: the evicted device agrees with itself, and the answer is "out".
    assert!(
        !agree(&s.charlie_g, &s.charlie, "charlie after its own eviction"),
        "the evicted device must read inactive",
    );
    // ...and every survivor agrees with itself, and is still in.
    assert!(
        agree(&s.alice_g, &s.alice, "alice after charlie's eviction"),
        "the committer stays a member",
    );
    assert!(
        agree(&s.bob_g, &s.bob, "bob after charlie's eviction"),
        "an unaffected member stays a member",
    );
}

// ============================================================================
// Eviction path 2 — the direct-commit path (a group with no role table)
// ============================================================================

/// A group created without a role table has no proposal path at all
/// (`propose_*` refuses), so `remove_member` is the only route and it is still
/// live. The accessor must track it identically.
#[test]
fn the_accessor_and_the_inference_agree_across_a_direct_commit_eviction() {
    let alice = Identity::from_seed(&[0x91u8; 64]).expect("alice");
    let bob = Identity::from_seed(&[0x92u8; 64]).expect("bob");

    // The `create_with_id` shape: no role table, hence no proposal path.
    let mut alice_g = Group::create_with_id(&alice, &[0x5Au8; 32]).expect("open group");
    assert!(
        alice_g.group_roles().is_none(),
        "this scene must be the no-role-table one",
    );

    let add_bob = alice_g
        .add_member_from_wire(&alice, &kp_wire(&bob))
        .expect("alice adds bob");
    let mut bob_g = Group::join_from_welcome(&wire(add_bob.welcome), &bob).expect("bob joins");

    assert!(agree(&alice_g, &alice, "alice before"));
    assert!(agree(&bob_g, &bob, "bob before"));

    let bob_leaf = leaf_of(&alice_g, bob.signature_public_key());
    let removal = alice_g
        .remove_member(&alice, bob_leaf)
        .expect("a direct remove still works on an open group");
    bob_g
        .process_incoming_verifying_roles(&bob, &wire(removal))
        .expect("bob applies its own removal");

    assert!(
        !agree(&bob_g, &bob, "bob after its own direct-commit eviction"),
        "the evicted device must read inactive on the direct path too",
    );
    assert!(
        agree(&alice_g, &alice, "alice after removing bob"),
        "the remover stays a member",
    );
}

// ============================================================================
// The survivor — a group whose *other* member left stays live
// ============================================================================

/// The mirror of the eviction case, and the one a client would break first if
/// it gated the composer on "the roster shrank" rather than on "am I in it":
/// a 1x1 chat whose peer is removed is still a group this device may send to.
#[test]
fn a_group_whose_other_member_was_removed_stays_active_for_the_survivor() {
    let mut s = scene(0x03);

    let charlie_leaf = leaf_of(&s.alice_g, s.charlie.signature_public_key());
    let proposal = s
        .bob_g
        .propose_remove_member(&s.bob, charlie_leaf)
        .expect("an admin may propose a removal");
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &proposal)
        .expect("alice stores the proposal");
    let sweep = s
        .alice_g
        .commit_pending_proposals(&s.alice)
        .expect("sweep")
        .expect("the sweep must produce a commit");
    let commit = wire(sweep.commit);
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &commit)
        .expect("bob applies the removal");

    // The survivors are active, agree with their own rosters, and can still
    // send — which is the operational meaning of "active", asserted rather
    // than assumed, so a false positive from the accessor cannot pass here.
    assert!(agree(&s.alice_g, &s.alice, "alice survives"));
    assert!(agree(&s.bob_g, &s.bob, "bob survives"));
    assert_eq!(s.alice_g.member_count(), 2);

    let msg = s
        .alice_g
        .create_application_message(&s.alice, b"still live")
        .expect("an active group still sends");
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &wire(msg))
        .expect("and a surviving member still receives");
}

// ============================================================================
// The accessor survives a restart, and so does the agreement
// ============================================================================

/// OpenMLS persists `MlsGroupState::Inactive` before it merges, so a snapshot
/// taken after an eviction restores an inactive group. The roster is persisted
/// too, so the two must still agree on the far side of a restart — which is
/// what lets a client read the state at launch rather than only in the poll
/// batch that carried the removal.
#[test]
fn the_agreement_survives_a_snapshot_restore_on_both_sides_of_an_eviction() {
    let mut s = scene(0x04);
    let charlie_seed = [0xC0u8 ^ 0x04; 64];
    let alice_seed = [0xA0u8 ^ 0x04; 64];

    let charlie_leaf = leaf_of(&s.alice_g, s.charlie.signature_public_key());
    let proposal = s
        .bob_g
        .propose_remove_member(&s.bob, charlie_leaf)
        .expect("an admin may propose a removal");
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &proposal)
        .expect("alice stores the proposal");
    s.charlie_g
        .process_incoming_verifying_roles(&s.charlie, &proposal)
        .expect("charlie stores the proposal");
    let sweep = s
        .alice_g
        .commit_pending_proposals(&s.alice)
        .expect("sweep")
        .expect("the sweep must produce a commit");
    let commit = wire(sweep.commit);
    s.charlie_g
        .process_incoming_verifying_roles(&s.charlie, &commit)
        .expect("charlie applies its own removal");

    let group_id = s.alice_g.group_id();

    // The evicted device: snapshot after the merge, restore, reload.
    let charlie_snapshot = s.charlie.snapshot_storage().expect("charlie snapshots");
    let charlie_restored = Identity::from_seed(&charlie_seed).expect("charlie restored identity");
    charlie_restored
        .restore_storage(&charlie_snapshot)
        .expect("charlie restores");
    let charlie_reloaded = Group::load(&charlie_restored, &group_id)
        .expect("load")
        .expect("charlie's group is in the snapshot");
    assert!(
        !agree(&charlie_reloaded, &charlie_restored, "charlie reloaded"),
        "the terminal state must survive a restart, and the roster with it",
    );

    // The survivor: same round trip, opposite answer.
    let alice_snapshot = s.alice.snapshot_storage().expect("alice snapshots");
    let alice_restored = Identity::from_seed(&alice_seed).expect("alice restored identity");
    alice_restored
        .restore_storage(&alice_snapshot)
        .expect("alice restores");
    let alice_reloaded = Group::load(&alice_restored, &group_id)
        .expect("load")
        .expect("alice's group is in the snapshot");
    assert!(
        agree(&alice_reloaded, &alice_restored, "alice reloaded"),
        "a survivor's group must reload active",
    );
}
