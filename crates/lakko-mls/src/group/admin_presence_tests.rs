// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0022 amendment of 2026-11-22 (C-100), the adversarial half: a committer
//! that removes an admin's last leaf without the prune, or writes some other
//! table under the prune's name, is refused by every peer. The honest paths
//! are pinned through the public API in `tests/admin_presence.rs`.

use super::*;
use crate::key_package;

fn wire(m: MlsMessageOut) -> Vec<u8> {
    encode_message(&m).expect("encode")
}

fn kp_wire(id: &Identity) -> Vec<u8> {
    key_package::serialize(&id.create_key_package().expect("kp")).expect("serialize kp")
}

fn pk32(id: &Identity) -> [u8; 32] {
    id.signature_public_key().try_into().expect("32-byte key")
}

fn leaf_of(g: &Group, id: &Identity) -> LeafNodeIndex {
    LeafNodeIndex::new(
        g.members()
            .iter()
            .find(|m| m.signature_public_key == id.signature_public_key())
            .expect("member present")
            .leaf_index,
    )
}

/// owner = alice; admins = bob, charlie, dave. Everyone applied everything.
struct Scene {
    alice: Identity,
    bob: Identity,
    charlie: Identity,
    dave: Identity,
    alice_g: Group,
    bob_g: Group,
}

fn scene(tag: u8) -> Scene {
    let alice = Identity::from_seed(&[0x10 ^ tag; 64]).expect("alice");
    let bob = Identity::from_seed(&[0x20 ^ tag; 64]).expect("bob");
    let charlie = Identity::from_seed(&[0x30 ^ tag; 64]).expect("charlie");
    let dave = Identity::from_seed(&[0x40 ^ tag; 64]).expect("dave");

    let mut alice_g = Group::create(&alice, GroupKind::Group).expect("create");
    let add_bob = alice_g
        .add_member_from_wire(&alice, &kp_wire(&bob))
        .expect("add bob");
    let mut bob_g = Group::join_from_welcome(&wire(add_bob.welcome), &bob).expect("bob joins");
    for other in [&charlie, &dave] {
        let add = alice_g
            .add_member_from_wire(&alice, &kp_wire(other))
            .expect("add");
        bob_g
            .process_incoming_verifying_roles(&bob, &wire(add.commit))
            .expect("bob applies");
    }
    let promote = alice_g
        .set_group_roles(
            &alice,
            &RoleTable::new(pk32(&alice), vec![pk32(&bob), pk32(&charlie), pk32(&dave)]),
        )
        .expect("promote");
    bob_g
        .process_incoming_verifying_roles(&bob, &promote)
        .expect("bob applies promote");
    Scene {
        alice,
        bob,
        charlie,
        dave,
        alice_g,
        bob_g,
    }
}

/// **Adversary.** `committer` removes `leaf` and, when given, carries `gce` by
/// value — with none of this crate's own checks — and merges it locally.
fn crafted_commit(
    g: &mut Group,
    committer: &Identity,
    leaf: LeafNodeIndex,
    gce: Option<Extensions<GroupContext>>,
) -> Vec<u8> {
    let mut builder = g
        .inner
        .commit_builder()
        .consume_proposal_store(false)
        .propose_removals([leaf]);
    if let Some(gce) = gce {
        builder = builder
            .propose_group_context_extensions(gce)
            .expect("add the role rewrite");
    }
    let bundle = builder
        .load_psks(committer.provider.storage())
        .expect("psks")
        .build(
            committer.provider.rand(),
            committer.provider.crypto(),
            &committer.signature_keys,
            |_| true,
        )
        .expect("build")
        .stage_commit(&committer.provider)
        .expect("stage");
    g.inner
        .merge_pending_commit(&committer.provider)
        .expect("merge locally");
    wire(bundle.into_contents().0)
}

fn refusal_of(alice_g: &mut Group, alice: &Identity, commit: &[u8]) -> Option<RefusalReason> {
    let before = alice_g.epoch();
    let err = alice_g
        .process_incoming_verifying_roles(alice, commit)
        .expect_err("the owner refuses it");
    assert_eq!(alice_g.epoch(), before, "a refused commit is not merged");
    err.refusal_reason()
}

/// An admin that removes another admin and keeps them in the table: the role
/// would survive the person, so every peer refuses the commit.
#[test]
fn removing_an_admin_without_the_prune_is_refused() {
    let mut s = scene(0x01);
    let charlie_leaf = leaf_of(&s.bob_g, &s.charlie);
    let commit = crafted_commit(&mut s.bob_g, &s.bob, charlie_leaf, None);
    assert_eq!(
        refusal_of(&mut s.alice_g, &s.alice, &commit),
        Some(RefusalReason::AdminNotAMember),
    );
}

/// The control: the same commit carrying the exact prune is accepted from an
/// admin, so the refusal above is about the missing prune and nothing else.
#[test]
fn control_the_exact_prune_from_an_admin_is_accepted() {
    let mut s = scene(0x02);
    let charlie_leaf = leaf_of(&s.bob_g, &s.charlie);
    let table = s.bob_g.group_roles().expect("table");
    let prune = s
        .bob_g
        .role_rewrite_extensions(&pruned_table(&table, &[pk32(&s.charlie)]))
        .expect("prune");
    let commit = crafted_commit(&mut s.bob_g, &s.bob, charlie_leaf, Some(prune));
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &commit)
        .expect("the owner accepts the exact prune from an admin");
    let after = s.alice_g.group_roles().expect("table");
    assert!(!after.can_administer(&pk32(&s.charlie)));
    assert!(after.can_administer(&pk32(&s.dave)), "dave untouched");
}

/// An admin cannot use the prune to demote a present admin: dropping dave
/// along with the departing charlie is a role change, which is owner-only.
#[test]
fn a_prune_that_also_drops_a_present_admin_is_refused() {
    let mut s = scene(0x03);
    let charlie_leaf = leaf_of(&s.bob_g, &s.charlie);
    let table = s.bob_g.group_roles().expect("table");
    let wider = pruned_table(&table, &[pk32(&s.charlie), pk32(&s.dave)]);
    let gce = s.bob_g.role_rewrite_extensions(&wider).expect("gce");
    let commit = crafted_commit(&mut s.bob_g, &s.bob, charlie_leaf, Some(gce));
    assert_eq!(
        refusal_of(&mut s.alice_g, &s.alice, &commit),
        Some(RefusalReason::NotOwner),
    );
}

/// Nor to take the owner slot, or name somebody outside the group: anything
/// but the table every peer computes is judged as an ordinary role change, or
/// as a table naming an absent admin, and refused.
#[test]
fn a_prune_that_writes_any_other_table_is_refused() {
    let erin = Identity::from_seed(&[0xEEu8; 64]).expect("erin");
    for (tag, expected) in [
        (0x04u8, RefusalReason::NotOwner),
        (0x05u8, RefusalReason::AdminNotAMember),
    ] {
        let mut s = scene(tag);
        let forged = if expected == RefusalReason::NotOwner {
            // charlie dropped, but bob takes the owner slot.
            RoleTable::new(pk32(&s.bob), vec![pk32(&s.dave), pk32(&s.alice)])
        } else {
            // charlie dropped, but a stranger is named admin.
            RoleTable::new(
                pk32(&s.alice),
                vec![pk32(&s.bob), pk32(&s.dave), pk32(&erin)],
            )
        };
        let charlie_leaf = leaf_of(&s.bob_g, &s.charlie);
        let gce = s.bob_g.role_rewrite_extensions(&forged).expect("gce");
        let commit = crafted_commit(&mut s.bob_g, &s.bob, charlie_leaf, Some(gce));
        assert_eq!(
            refusal_of(&mut s.alice_g, &s.alice, &commit),
            Some(expected)
        );
    }
}

/// The removal of an admin's last leaf alongside a by-value table rewrite that
/// drops the admin is the owner's ordinary role change when the owner commits
/// it, and the peers accept it.
#[test]
fn the_owner_may_fold_any_table_that_keeps_its_admins_present() {
    let mut s = scene(0x06);
    let charlie_leaf = leaf_of(&s.alice_g, &s.charlie);
    let table = RoleTable::new(pk32(&s.alice), vec![pk32(&s.bob)]);
    let gce = s.alice_g.role_rewrite_extensions(&table).expect("gce");
    let commit = crafted_commit(&mut s.alice_g, &s.alice, charlie_leaf, Some(gce));
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &commit)
        .expect("an owner's table naming only present admins is accepted");
    let after = s.bob_g.group_roles().expect("table");
    assert!(
        !after.can_administer(&pk32(&s.dave)),
        "the owner demoted dave"
    );
    assert!(!after.can_administer(&pk32(&s.charlie)));
}
