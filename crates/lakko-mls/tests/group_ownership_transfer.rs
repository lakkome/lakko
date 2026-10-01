// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Ownership transfer — property tests for ADR-0022's deferred follow-up.
//!
//! The scene is the same 3-member role-enabled group the ADR-0022 and ADR-0045
//! suites use (owner = alice, admin = bob, member = charlie), so all three read
//! side by side.
//!
//! The suite's centre is [`transfer_then_the_former_owner_leaves_and_the_group_works`],
//! which is the whole reason the feature exists, and its control
//! [`control_the_owner_cannot_leave_before_transferring`], which shows the
//! departure is impossible in the same scene one step earlier — so the headline
//! test is provably measuring the transfer and not the scene.
//!
//! Everything else guards the one invariant transfer could destroy: **the group
//! is never left with an owner who is not in it.** An owner outside the group is
//! permanent, because a role change is owner-only and the owner's leaf is
//! unremovable, so there is no state to recover from.

use lakko_mls::{
    group, key_package, Group, GroupKind, Identity, Incoming, LeafNodeIndex, OutgoingOwner,
    ProposalKind, Role, RoleTable,
};
use openmls::extensions::{
    Extension, ExtensionType, Extensions, RequiredCapabilitiesExtension, UnknownExtension,
};

fn wire(msg: openmls::framing::MlsMessageOut) -> Vec<u8> {
    group::encode_message(&msg).expect("encode message")
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

fn is_member(g: &Group, id: &Identity) -> bool {
    g.members()
        .iter()
        .any(|m| m.signature_public_key == id.signature_public_key())
}

/// The `0xFF02` role-table extension type, restated locally — the constant is
/// `pub(crate)` and these tests hand-build extension sets a peer must judge.
const ROLE_EXTENSION_TYPE: u16 = 0xFF02;
/// The ADR-0060 conversation-kind extension type.
const KIND_EXTENSION_TYPE: u16 = 0xFF03;

/// The extension set `set_group_roles` builds, restated so a test can build a
/// *malformed* one (a hand-rolled table, or none at all) the way a modified
/// client would.
fn role_extensions(table: Option<&RoleTable>) -> Extensions<openmls::group::GroupContext> {
    let mut exts = vec![Extension::RequiredCapabilities(
        RequiredCapabilitiesExtension::new(
            &[
                ExtensionType::Unknown(ROLE_EXTENSION_TYPE),
                ExtensionType::Unknown(KIND_EXTENSION_TYPE),
            ],
            &[],
            &[],
        ),
    )];
    exts.push(Extension::Unknown(
        KIND_EXTENSION_TYPE,
        UnknownExtension(GroupKind::Group.encode().to_vec()),
    ));
    if let Some(t) = table {
        exts.push(Extension::Unknown(
            ROLE_EXTENSION_TYPE,
            UnknownExtension(t.encode()),
        ));
    }
    Extensions::from_vec(exts).expect("build role extensions")
}

struct Scene {
    alice: Identity,
    bob: Identity,
    charlie: Identity,
    alice_g: Group,
    bob_g: Group,
    charlie_g: Group,
}

/// owner = alice, admin = bob, member = charlie.
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

/// Every device applies `commit`, so the whole group stays on one epoch.
fn broadcast(s: &mut Scene, commit: &[u8], skip_alice: bool) {
    if !skip_alice {
        s.alice_g
            .process_incoming_verifying_roles(&s.alice, commit)
            .expect("alice applies");
    }
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, commit)
        .expect("bob applies");
    s.charlie_g
        .process_incoming_verifying_roles(&s.charlie, commit)
        .expect("charlie applies");
}

// ============================================================================
// Who may transfer
// ============================================================================

/// Only the owner. Transfer is a role change and a role change is owner-only;
/// anything weaker is a coup, since an admin able to transfer could name itself.
#[test]
fn only_the_owner_can_transfer() {
    let mut s = scene(0x01);

    // An admin is refused locally on both paths...
    assert!(
        s.bob_g
            .transfer_ownership(&s.bob, s.bob.signature_public_key(), OutgoingOwner::Admin)
            .is_err(),
        "an admin must not be able to transfer ownership to itself",
    );
    assert!(s
        .bob_g
        .propose_ownership_transfer(&s.bob, s.bob.signature_public_key(), OutgoingOwner::Admin)
        .is_err());
    // ...and so is a plain member.
    assert!(s
        .charlie_g
        .transfer_ownership(
            &s.charlie,
            s.charlie.signature_public_key(),
            OutgoingOwner::Admin
        )
        .is_err());

    // And a modified client that skips its own check and rewrites the table by
    // hand is refused by every PEER — the enforcing copy. Built through the raw
    // GroupContextExtensions primitive, because `set_group_roles` now refuses a
    // non-owner locally too (ADR-0045 review, LOW-1) and this assertion is
    // about the peer gate.
    let coup = RoleTable::new(pk32(&s.bob), vec![pk32(&s.alice)]);
    let commit = wire(
        s.bob_g
            .set_group_context_extensions(&s.bob, role_extensions(Some(&coup)))
            .expect("bob crafts a self-transfer locally"),
    );
    assert!(
        s.alice_g
            .process_incoming_verifying_roles(&s.alice, &commit)
            .is_err(),
        "the owner must refuse an admin's self-transfer",
    );
    assert!(
        s.charlie_g
            .process_incoming_verifying_roles(&s.charlie, &commit)
            .is_err(),
        "a plain member must refuse an admin's self-transfer too",
    );
    assert_eq!(s.charlie_g.own_role(&s.alice), Some(Role::Owner));
}

// ============================================================================
// What the transfer does
// ============================================================================

#[test]
fn after_transfer_the_new_owner_holds_owner_powers_and_the_old_one_does_not() {
    let mut s = scene(0x02);
    let commit = s
        .alice_g
        .transfer_ownership(&s.alice, s.bob.signature_public_key(), OutgoingOwner::Admin)
        .expect("alice transfers to bob");
    broadcast(&mut s, &commit, true);

    // Every view agrees on the new authority structure.
    for (g, who) in [
        (&s.alice_g, "alice"),
        (&s.bob_g, "bob"),
        (&s.charlie_g, "charlie"),
    ] {
        let t = g.group_roles().expect("role-enabled");
        assert_eq!(t.owner(), &pk32(&s.bob), "{who} must see bob as owner");
        assert_eq!(
            t.role_of(s.alice.signature_public_key()),
            Role::Admin,
            "{who} must see the outgoing owner as admin",
        );
    }
    assert_eq!(s.bob_g.own_committer_rank(&s.bob), Some(0));

    // The NEW owner can do owner-only things: promote charlie.
    let promote = s
        .bob_g
        .set_group_roles(
            &s.bob,
            &RoleTable::new(pk32(&s.bob), vec![pk32(&s.alice), pk32(&s.charlie)]),
        )
        .expect("bob rewrites the role table");
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &promote)
        .expect("the former owner accepts the new owner's role change");
    s.charlie_g
        .process_incoming_verifying_roles(&s.charlie, &promote)
        .expect("charlie accepts it");
    assert_eq!(s.charlie_g.own_role(&s.charlie), Some(Role::Admin));

    // The OLD owner cannot: refused locally on the proposal path...
    assert!(
        s.alice_g
            .propose_group_roles(&s.alice, &RoleTable::owner_only(pk32(&s.alice)))
            .is_err(),
        "the former owner must not be able to propose a role change",
    );
    assert!(s
        .alice_g
        .transfer_ownership(
            &s.alice,
            s.charlie.signature_public_key(),
            OutgoingOwner::Admin
        )
        .is_err());
    // ...and refused by peers on the direct-commit path. Raw primitive again:
    // alice is no longer the owner, so `set_group_roles` refuses her locally.
    let take_back_table = RoleTable::new(pk32(&s.alice), vec![pk32(&s.bob)]);
    let take_back = wire(
        s.alice_g
            .set_group_context_extensions(&s.alice, role_extensions(Some(&take_back_table)))
            .expect("alice crafts a take-back locally"),
    );
    assert!(
        s.bob_g
            .process_incoming_verifying_roles(&s.bob, &take_back)
            .is_err(),
        "a transfer cannot be undone by its author",
    );
    assert!(s
        .charlie_g
        .process_incoming_verifying_roles(&s.charlie, &take_back)
        .is_err());
}

#[test]
fn the_outgoing_owner_can_be_dropped_to_a_plain_member() {
    let mut s = scene(0x03);
    let commit = s
        .alice_g
        .transfer_ownership(
            &s.alice,
            s.bob.signature_public_key(),
            OutgoingOwner::Member,
        )
        .expect("alice transfers to bob, keeping nothing");
    broadcast(&mut s, &commit, true);

    assert_eq!(s.charlie_g.own_role(&s.alice), Some(Role::Member));
    assert_eq!(s.alice_g.own_committer_rank(&s.alice), None);
    // A plain member cannot add, so the demotion is real and not cosmetic.
    let dave = Identity::from_seed(&[0x33u8; 64]).expect("dave");
    assert!(s
        .alice_g
        .propose_add_member_from_wire(&s.alice, &kp_wire(&dave))
        .is_err());
}

#[test]
fn transfer_composes_with_the_proposal_path_and_any_admin_may_sweep_it() {
    let mut s = scene(0x04);

    // The owner PROPOSES the transfer; it is an ordinary owner-only RoleChange,
    // so it needs no new rule at the receipt gate.
    let proposal = s
        .alice_g
        .propose_ownership_transfer(
            &s.alice,
            s.charlie.signature_public_key(),
            OutgoingOwner::Admin,
        )
        .expect("alice proposes handing the group to charlie");
    let incoming = s
        .bob_g
        .process_incoming_verifying_roles(&s.bob, &proposal)
        .expect("bob processes it");
    let Incoming::Proposal(record) = incoming else {
        panic!("expected a proposal");
    };
    assert!(
        record.stored,
        "the owner's transfer proposal must be stored"
    );
    assert_eq!(record.kind, ProposalKind::RoleChange);

    // ...and an ADMIN sweeps it, which is the ADR-0045 shape: the committer is
    // whoever holds the duty, not necessarily the author.
    let sweep = s
        .bob_g
        .commit_pending_proposals(&s.bob)
        .expect("bob sweeps")
        .expect("a commit is produced");
    assert_eq!(sweep.swept.len(), 1);
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &wire(sweep.commit))
        .expect("alice applies the sweep");
    assert_eq!(
        s.alice_g.group_roles().expect("table").owner(),
        &pk32(&s.charlie)
    );
    assert_eq!(
        s.bob_g.group_roles().expect("table").owner(),
        &pk32(&s.charlie)
    );
}

// ============================================================================
// The group is never left ownerless
// ============================================================================

#[test]
fn transfer_to_a_non_member_is_refused_locally() {
    let mut s = scene(0x05);
    let stranger = Identity::from_seed(&[0x55u8; 64]).expect("stranger");
    assert!(!is_member(&s.alice_g, &stranger));
    assert!(
        s.alice_g
            .transfer_ownership(
                &s.alice,
                stranger.signature_public_key(),
                OutgoingOwner::Admin
            )
            .is_err(),
        "an owner outside the group can never be replaced or removed",
    );
    assert!(s
        .alice_g
        .propose_ownership_transfer(
            &s.alice,
            stranger.signature_public_key(),
            OutgoingOwner::Admin
        )
        .is_err());
    // A short / long key is not a member either.
    assert!(s
        .alice_g
        .transfer_ownership(&s.alice, &[7u8; 16], OutgoingOwner::Admin)
        .is_err());
    // Nothing was touched.
    assert_eq!(
        s.alice_g.group_roles().expect("table").owner(),
        &pk32(&s.alice)
    );
    assert!(s.alice_g.pending_proposals().is_empty());
}

/// **The enforcing copy.** A modified client that skips the local check and
/// hand-builds a role table naming a non-member is refused by every peer. This
/// commit is reachable through the public API today (`set_group_roles` takes any
/// table), and before this change every peer merged it — after which nobody in
/// the group could ever change the role table again.
#[test]
fn a_peer_refuses_a_commit_that_hands_the_group_to_a_non_member() {
    let mut s = scene(0x06);
    let stranger = Identity::from_seed(&[0x66u8; 64]).expect("stranger");

    let orphan = RoleTable::new(pk32(&stranger), vec![pk32(&s.alice), pk32(&s.bob)]);
    let commit = wire(
        s.alice_g
            .set_group_context_extensions(&s.alice, role_extensions(Some(&orphan)))
            .expect("alice crafts an orphaning table locally"),
    );

    assert!(
        s.bob_g
            .process_incoming_verifying_roles(&s.bob, &commit)
            .is_err(),
        "an admin must refuse a commit that would leave the group unowned",
    );
    assert!(
        s.charlie_g
            .process_incoming_verifying_roles(&s.charlie, &commit)
            .is_err(),
        "a plain member must refuse it too",
    );
    // Un-merged: the honest peers keep the old owner and stay administrable.
    assert_eq!(
        s.bob_g.group_roles().expect("table").owner(),
        &pk32(&s.alice)
    );
    let dave = Identity::from_seed(&[0x67u8; 64]).expect("dave");
    assert!(s
        .bob_g
        .propose_add_member_from_wire(&s.bob, &kp_wire(&dave))
        .is_ok());
}

/// The same gate refuses a commit that would *delete* the role table rather than
/// change it — which would silently revert the group to the open/legacy shape
/// where any member may add or remove.
#[test]
fn a_peer_refuses_a_commit_that_erases_the_role_table() {
    let mut s = scene(0x07);
    let commit = s
        .alice_g
        .set_group_context_extensions(&s.alice, role_extensions(None))
        .expect("alice crafts a table-erasing commit locally");
    assert!(s
        .bob_g
        .process_incoming_verifying_roles(&s.bob, &wire(commit))
        .is_err());
    assert!(s.bob_g.group_roles().is_some(), "the table survives");
}

/// The composition no per-proposal rule can see: "hand the group to Charlie"
/// and "Charlie leaves" are each individually authorised, and together they
/// would leave the group unowned. The sweep EXCLUDES the role change rather than
/// refusing the whole sweep, so the unrelated intent still lands.
#[test]
fn a_sweep_drops_a_transfer_to_someone_the_same_commit_removes() {
    let mut s = scene(0x08);

    let transfer = s
        .alice_g
        .propose_ownership_transfer(
            &s.alice,
            s.charlie.signature_public_key(),
            OutgoingOwner::Admin,
        )
        .expect("alice proposes handing the group to charlie");
    let own_leaf = s.charlie_g.own_leaf_index();
    let leaving = s
        .charlie_g
        .propose_remove_member(&s.charlie, own_leaf)
        .expect("charlie proposes to leave");

    // Both reach alice, both are authorised, both are stored. Bob sees the
    // departure too, so he can resolve the by-reference proposal in the sweep.
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &leaving)
        .expect("alice stores charlie's leave");
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &leaving)
        .expect("bob stores charlie's leave");
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &transfer)
        .expect("bob stores alice's transfer");
    assert_eq!(s.alice_g.pending_proposals().len(), 2);
    assert_eq!(s.bob_g.pending_proposals().len(), 2);

    let sweep = s
        .alice_g
        .commit_pending_proposals(&s.alice)
        .expect("alice sweeps")
        .expect("a commit is produced");
    assert_eq!(
        sweep.swept.len(),
        1,
        "only the departure rides the commit; the orphaning transfer is dropped",
    );
    assert!(matches!(
        sweep.swept[0].kind,
        ProposalKind::Remove { .. } | ProposalKind::Leave
    ));
    assert!(!is_member(&s.alice_g, &s.charlie));
    assert_eq!(
        s.alice_g.group_roles().expect("table").owner(),
        &pk32(&s.alice),
        "the group keeps an owner that is in it",
    );

    // bob applies the same commit and lands on the same state, so nothing forked.
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &wire(sweep.commit))
        .expect("bob applies the sweep");
    assert_eq!(s.bob_g.epoch(), s.alice_g.epoch());
    assert_eq!(
        s.bob_g.group_roles().expect("table").owner(),
        &pk32(&s.alice)
    );

    // And the transfer is simply re-proposable once charlie is not involved.
    assert!(s
        .alice_g
        .propose_ownership_transfer(&s.alice, s.bob.signature_public_key(), OutgoingOwner::Admin)
        .is_ok());
}

#[test]
fn transfer_to_yourself_is_refused_rather_than_burning_an_epoch() {
    let mut s = scene(0x09);
    let epoch = s.alice_g.epoch();
    assert!(s
        .alice_g
        .transfer_ownership(
            &s.alice,
            s.alice.signature_public_key(),
            OutgoingOwner::Admin
        )
        .is_err());
    assert_eq!(s.alice_g.epoch(), epoch, "a refusal touches no MLS state");
}

#[test]
fn a_group_with_no_role_table_has_no_ownership_to_transfer() {
    let alice = Identity::from_seed(&[0x0Au8; 64]).expect("alice");
    let mut g = Group::create_with_id(&alice, &[9u8; 32]).expect("create");
    assert!(g.group_roles().is_none());
    assert!(g
        .transfer_ownership(&alice, alice.signature_public_key(), OutgoingOwner::Admin)
        .is_err());
}

// ============================================================================
// The point of the feature
// ============================================================================

/// **Control.** In the same scene one step earlier, the owner cannot leave. This
/// is the dead end ADR-0045 made permanent and the reason transfer was built; it
/// is asserted here so the headline test below cannot pass vacuously.
#[test]
fn control_the_owner_cannot_leave_before_transferring() {
    let mut s = scene(0x0B);
    let own_leaf = s.alice_g.own_leaf_index();
    assert!(
        s.alice_g.propose_remove_member(&s.alice, own_leaf).is_err(),
        "an owner must not be able to propose its own removal",
    );
    // Nobody else can evict them either, so there is genuinely no way out.
    let alice_leaf = leaf_of(&s.bob_g, s.alice.signature_public_key());
    assert!(s.bob_g.propose_remove_member(&s.bob, alice_leaf).is_err());
    assert!(s.alice_g.pending_proposals().is_empty());
}

/// **The whole reason the feature exists.** Transfer, then the former owner
/// leaves, and the group keeps working — new members can be added, messages
/// still decrypt, and it still has an owner who is in it.
#[test]
fn transfer_then_the_former_owner_leaves_and_the_group_works() {
    let mut s = scene(0x0C);
    let own_leaf = s.alice_g.own_leaf_index();
    assert!(
        s.alice_g.propose_remove_member(&s.alice, own_leaf).is_err(),
        "precondition: while owner, alice is bound to the group",
    );

    // 1. Hand over. Alice keeps nothing — she is leaving.
    let transfer = s
        .alice_g
        .propose_ownership_transfer(
            &s.alice,
            s.bob.signature_public_key(),
            OutgoingOwner::Member,
        )
        .expect("alice proposes handing the group to bob");
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &transfer)
        .expect("bob stores it");
    s.charlie_g
        .process_incoming_verifying_roles(&s.charlie, &transfer)
        .expect("charlie stores it");
    let sweep = s
        .alice_g
        .commit_pending_proposals(&s.alice)
        .expect("alice sweeps her own transfer")
        .expect("a commit is produced");
    let transfer_commit = wire(sweep.commit);
    broadcast(&mut s, &transfer_commit, true);
    assert_eq!(s.bob_g.group_roles().expect("table").owner(), &pk32(&s.bob));
    assert_eq!(s.alice_g.own_role(&s.alice), Some(Role::Member));

    // 2. Now she can leave. Same call, same leaf, different answer — and the
    //    only thing that changed is who the table calls owner.
    let own_leaf = s.alice_g.own_leaf_index();
    let leaving = s
        .alice_g
        .propose_remove_member(&s.alice, own_leaf)
        .expect("the former owner may leave");
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &leaving)
        .expect("bob stores it");
    s.charlie_g
        .process_incoming_verifying_roles(&s.charlie, &leaving)
        .expect("charlie stores it");
    let sweep = s
        .bob_g
        .commit_pending_proposals(&s.bob)
        .expect("the new owner sweeps")
        .expect("a commit is produced");
    let leave_commit = wire(sweep.commit);
    s.charlie_g
        .process_incoming_verifying_roles(&s.charlie, &leave_commit)
        .expect("charlie applies the departure");
    assert!(!is_member(&s.bob_g, &s.alice));
    assert!(!is_member(&s.charlie_g, &s.alice));

    // 3. The group still works. It has an owner who is in it...
    let table = s.charlie_g.group_roles().expect("table");
    assert_eq!(table.owner(), &pk32(&s.bob));
    assert!(s
        .charlie_g
        .members()
        .iter()
        .any(|m| m.signature_public_key == pk32(&s.bob)));

    // ...membership still changes...
    let dave = Identity::from_seed(&[0x1Cu8; 64]).expect("dave");
    let add = s
        .bob_g
        .add_member_from_wire(&s.bob, &kp_wire(&dave))
        .expect("the new owner adds dave");
    s.charlie_g
        .process_incoming_verifying_roles(&s.charlie, &wire(add.commit))
        .expect("charlie applies the add");
    let dave_g = Group::join_from_welcome(&wire(add.welcome), &dave).expect("dave joins");
    assert_eq!(dave_g.group_roles().expect("table").owner(), &pk32(&s.bob));

    // ...and messages still flow, on one shared epoch.
    let msg = s
        .bob_g
        .create_application_message(&s.bob, b"still here")
        .expect("bob sends");
    let got = s
        .charlie_g
        .process_incoming_verifying_roles(&s.charlie, &wire(msg))
        .expect("charlie receives");
    let Incoming::Application { bytes, .. } = got else {
        panic!("expected an application message");
    };
    assert_eq!(bytes, b"still here");
    assert_eq!(s.charlie_g.epoch(), s.bob_g.epoch());
    assert_eq!(dave_g.epoch(), s.bob_g.epoch());
}
