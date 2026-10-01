// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0022 group-admin role enforcement — property tests for the
//! `Group::process_incoming_verifying_roles` apply-path gate.
//!
//! Each test builds a 3-member role-enabled group (owner = alice, admin = bob,
//! member = charlie), has one member CRAFT a commit, and asserts a DIFFERENT
//! member's verifying apply path either merges it (authorised) or REFUSES it
//! (unauthorised — fail-closed, never merged). The committer is authenticated
//! by MLS, so the gate's `sender()`-derived role check is what enforces the
//! policy; an offender's refused commit forks them out of the honest state.

use lakko_mls::{group, key_package, Group, GroupKind, Identity, Incoming, RoleTable};
use openmls::extensions::{
    Extension, ExtensionType, Extensions, RequiredCapabilitiesExtension, UnknownExtension,
};
use openmls::prelude::LeafNodeIndex;

/// The `0xFF02` role-table extension type, restated locally — the constant is
/// `pub(crate)` and this suite hand-builds an extension set a peer must judge.
const ROLE_EXTENSION_TYPE: u16 = 0xFF02;
/// The ADR-0062 conversation-kind extension type.
const KIND_EXTENSION_TYPE: u16 = 0xFF03;

/// The extension set `set_group_roles` builds, restated so a test can commit
/// one that `set_group_roles` itself now refuses to build — the adversary seam
/// a peer-gate test needs (mirrors `group_ownership_transfer.rs`).
fn role_extensions(table: &RoleTable) -> Extensions<openmls::group::GroupContext> {
    Extensions::from_vec(vec![
        Extension::RequiredCapabilities(RequiredCapabilitiesExtension::new(
            &[
                ExtensionType::Unknown(ROLE_EXTENSION_TYPE),
                ExtensionType::Unknown(KIND_EXTENSION_TYPE),
            ],
            &[],
            &[],
        )),
        Extension::Unknown(ROLE_EXTENSION_TYPE, UnknownExtension(table.encode())),
        Extension::Unknown(
            KIND_EXTENSION_TYPE,
            UnknownExtension(GroupKind::Group.encode().to_vec()),
        ),
    ])
    .expect("build role extensions")
}

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

struct Scene {
    alice: Identity,
    bob: Identity,
    charlie: Identity,
    alice_g: Group,
    bob_g: Group,
    charlie_g: Group,
}

/// owner = alice, admin = bob, member = charlie — a 3-member role-enabled group
/// (every conversation group is role-enabled from creation, ADR-0022 option A).
fn scene() -> Scene {
    let alice = Identity::from_seed(&[0xA0u8; 64]).expect("alice");
    let bob = Identity::from_seed(&[0xB0u8; 64]).expect("bob");
    let charlie = Identity::from_seed(&[0xC0u8; 64]).expect("charlie");

    let mut alice_g = Group::create(&alice, GroupKind::Group).expect("alice creates group");

    // alice (owner) adds bob; bob joins from the Welcome (carries the role table).
    let add_bob = alice_g
        .add_member_from_wire(&alice, &kp_wire(&bob))
        .expect("alice adds bob");
    let mut bob_g = Group::join_from_welcome(&wire(add_bob.welcome), &bob).expect("bob joins");

    // alice (owner) adds charlie; bob applies via the gate (owner add -> ok).
    let add_c = alice_g
        .add_member_from_wire(&alice, &kp_wire(&charlie))
        .expect("alice adds charlie");
    bob_g
        .process_incoming_verifying_roles(&bob, &wire(add_c.commit))
        .expect("bob applies owner's add-charlie");
    let mut charlie_g =
        Group::join_from_welcome(&wire(add_c.welcome), &charlie).expect("charlie joins");

    // alice (owner) promotes bob to admin; bob + charlie apply via the gate.
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

#[test]
fn owner_can_add() {
    let mut s = scene();
    let dave = Identity::from_seed(&[0xD0u8; 64]).expect("dave");
    let add = s
        .alice_g
        .add_member_from_wire(&s.alice, &kp_wire(&dave))
        .expect("alice adds dave");
    // bob (admin) applies alice's (owner) add via the gate -> allowed.
    assert!(matches!(
        s.bob_g
            .process_incoming_verifying_roles(&s.bob, &wire(add.commit)),
        Ok(Incoming::Commit)
    ));
}

#[test]
fn admin_can_add() {
    let mut s = scene();
    let dave = Identity::from_seed(&[0xD1u8; 64]).expect("dave");
    let add = s
        .bob_g
        .add_member_from_wire(&s.bob, &kp_wire(&dave))
        .expect("bob adds dave");
    // alice (owner) applies bob's (admin) add via the gate -> allowed.
    assert!(matches!(
        s.alice_g
            .process_incoming_verifying_roles(&s.alice, &wire(add.commit)),
        Ok(Incoming::Commit)
    ));
}

#[test]
fn member_cannot_add() {
    let mut s = scene();
    let dave = Identity::from_seed(&[0xD2u8; 64]).expect("dave");
    let add = s
        .charlie_g
        .add_member_from_wire(&s.charlie, &kp_wire(&dave))
        .expect("charlie crafts an add locally");
    // alice (owner) REFUSES charlie's (member) add.
    assert!(s
        .alice_g
        .process_incoming_verifying_roles(&s.alice, &wire(add.commit))
        .is_err());
}

#[test]
fn member_cannot_remove() {
    let mut s = scene();
    let bob_leaf = leaf_of(&s.charlie_g, s.bob.signature_public_key());
    let commit = s
        .charlie_g
        .remove_member(&s.charlie, bob_leaf)
        .expect("charlie crafts a remove locally");
    // alice (owner) REFUSES charlie's (member) removal of bob.
    assert!(s
        .alice_g
        .process_incoming_verifying_roles(&s.alice, &wire(commit))
        .is_err());
}

#[test]
fn owner_cannot_be_removed() {
    let mut s = scene();
    let alice_leaf = leaf_of(&s.bob_g, s.alice.signature_public_key());
    // bob (admin) tries to remove alice (the owner).
    let commit = s
        .bob_g
        .remove_member(&s.bob, alice_leaf)
        .expect("bob crafts an owner-removal locally");
    // charlie (member) REFUSES it — the owner is unremovable.
    assert!(s
        .charlie_g
        .process_incoming_verifying_roles(&s.charlie, &wire(commit))
        .is_err());
}

#[test]
fn admin_can_remove_member() {
    let mut s = scene();
    let charlie_leaf = leaf_of(&s.bob_g, s.charlie.signature_public_key());
    // bob (admin) removes charlie (member).
    let commit = s
        .bob_g
        .remove_member(&s.bob, charlie_leaf)
        .expect("bob removes charlie");
    // alice (owner) applies it via the gate -> allowed.
    assert!(matches!(
        s.alice_g
            .process_incoming_verifying_roles(&s.alice, &wire(commit)),
        Ok(Incoming::Commit)
    ));
}

#[test]
fn role_change_is_owner_only_so_admin_self_promote_is_refused() {
    let mut s = scene();
    // bob (admin) tries to rewrite the role table (add charlie as a second
    // admin) — a GroupContextExtensions commit, which is owner-only.
    let roles = RoleTable::new(pk32(&s.alice), vec![pk32(&s.bob), pk32(&s.charlie)]);

    // Built through the raw GroupContextExtensions primitive, not through
    // `set_group_roles`, which now refuses a non-owner locally (ADR-0045
    // review, LOW-1). This test is about the PEER gate, so it needs a
    // committer that skips the local one — the same adversary shape
    // `group_ownership_transfer.rs` uses for its table-erasing commit.
    let commit = s
        .bob_g
        .set_group_context_extensions(&s.bob, role_extensions(&roles))
        .expect("bob crafts a role-table change locally");
    // alice (owner) REFUSES bob's (admin) role change.
    assert!(s
        .alice_g
        .process_incoming_verifying_roles(&s.alice, &wire(commit))
        .is_err());
}

/// The local half of the same rule (ADR-0045 review, LOW-1): `set_group_roles`
/// refuses a table its own peers would refuse, **before** touching MLS state,
/// rather than merging eagerly and self-forking. Both refusals are already
/// enforced peer-side; what is new is that the caller is told.
#[test]
fn set_group_roles_refuses_locally_what_every_peer_would_refuse() {
    let mut s = scene();
    let epoch_before = s.alice_g.epoch();

    // A non-owner may not rewrite the table at all.
    assert!(
        s.bob_g
            .set_group_roles(
                &s.bob,
                &RoleTable::new(pk32(&s.alice), vec![pk32(&s.bob), pk32(&s.charlie)]),
            )
            .is_err(),
        "an admin's role change must be refused locally, not merged and then refused by peers",
    );

    // The owner may, but not to an owner who is not in the group — the
    // permanently un-administrable table `owner_survives` refuses on every peer.
    let outsider = Identity::from_seed(&[0x5Eu8; 64]).expect("outsider");
    assert!(
        s.alice_g
            .set_group_roles(&s.alice, &RoleTable::owner_only(pk32(&outsider)))
            .is_err(),
        "a table naming a non-member owner must be refused locally",
    );

    // Neither refusal touched the group.
    assert_eq!(s.alice_g.epoch(), epoch_before);
    assert_eq!(s.bob_g.epoch(), epoch_before);
    assert_eq!(
        s.alice_g.group_roles().expect("table").owner(),
        &pk32(&s.alice),
    );

    // And the legitimate rewrite still works.
    assert!(s
        .alice_g
        .set_group_roles(&s.alice, &RoleTable::new(pk32(&s.alice), vec![]))
        .is_ok());
}
