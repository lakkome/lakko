// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0060 — a conversation's kind is set at creation, and a one-to-one keeps
//! its two people.
//!
//! Drives the public API only. The peer-commit-gate half (a commit built
//! without the local checks) needs the crate's test-only adversary paths and
//! lives in `src/group.rs` (`one_to_one_gate_tests`).

use lakko_mls::{
    group, key_package, Group, GroupKind, Identity, Incoming, LeafNodeIndex, OutgoingOwner,
    RefusalReason, RoleTable,
};

fn wire(msg: openmls::framing::MlsMessageOut) -> Vec<u8> {
    group::encode_message(&msg).expect("encode message")
}

fn kp_wire(id: &Identity) -> Vec<u8> {
    key_package::serialize(&id.create_key_package().expect("key package")).expect("serialize kp")
}

fn account(tag: u8) -> Identity {
    Identity::from_seed(&[tag; 64]).expect("account identity")
}

/// A device identity bound to `owner`'s account (ADR-0050).
fn device(owner: &Identity, tag: u8) -> Identity {
    Identity::from_seed_bound(&[tag; 32], &owner.ed25519_seed_bytes(), 1_700_000_000)
        .expect("bound device identity")
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

fn reason<T>(r: lakko_mls::Result<T>, what: &str) -> RefusalReason {
    let Err(e) = r else {
        panic!("{what}: expected a refusal, got success");
    };
    e.refusal_reason()
        .unwrap_or_else(|| panic!("{what}: expected a typed refusal, got {e}"))
}

fn accounts(g: &Group) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = g.members().into_iter().map(|m| m.account_key).collect();
    out.sort();
    out.dedup();
    out
}

/// alice's first device `a1` founds a one-to-one with bob's device `b1`.
struct Scene {
    a: Identity,
    a1: Identity,
    b: Identity,
    b1: Identity,
    a1_g: Group,
    b1_g: Group,
}

fn one_to_one(tag: u8) -> Scene {
    let a = account(0xA0 ^ tag);
    let a1 = device(&a, 0xA1 ^ tag);
    let b = account(0xB0 ^ tag);
    let b1 = device(&b, 0xB1 ^ tag);
    let mut a1_g = Group::create(&a1, GroupKind::OneToOne).expect("a1 founds");
    let add = a1_g
        .add_members_from_wire(&a1, &[&kp_wire(&b1)])
        .expect("the founding commit admits bob");
    let b1_g = Group::join_from_welcome(&wire(add.welcome), &b1).expect("b1 joins");
    Scene {
        a,
        a1,
        b,
        b1,
        a1_g,
        b1_g,
    }
}

#[test]
fn the_kind_is_read_the_same_on_every_member() {
    let s = one_to_one(0x01);
    assert_eq!(s.a1_g.group_kind(), Some(GroupKind::OneToOne));
    assert_eq!(s.b1_g.group_kind(), Some(GroupKind::OneToOne));

    // A two-person GROUP reads as a group on both, which is the live defect
    // this ADR exists for.
    let c = account(0x11);
    let d = account(0x12);
    let mut g = Group::create(&c, GroupKind::Group).expect("group");
    let add = g.add_member_from_wire(&c, &kp_wire(&d)).expect("add d");
    let d_g = Group::join_from_welcome(&wire(add.welcome), &d).expect("d joins");
    assert_eq!(g.group_kind(), Some(GroupKind::Group));
    assert_eq!(d_g.group_kind(), Some(GroupKind::Group));
    assert_eq!(g.members().len(), 2);
}

#[test]
fn the_founding_commit_admits_the_other_person_with_several_devices_and_own_devices() {
    let a = account(0x21);
    let a1 = device(&a, 0x22);
    let a2 = device(&a, 0x23);
    let b = account(0x24);
    let b1 = device(&b, 0x25);
    let b2 = device(&b, 0x26);
    let mut g = Group::create(&a1, GroupKind::OneToOne).expect("found");
    let add = g
        .add_members_from_wire(&a1, &[&kp_wire(&b1), &kp_wire(&b2), &kp_wire(&a2)])
        .expect("one other person with two devices, plus my own other device");
    assert_eq!(g.members().len(), 4);
    assert_eq!(accounts(&g).len(), 2);
    for joiner in [&b1, &b2, &a2] {
        let j = Group::join_from_welcome(&wire(add.welcome.clone()), joiner).expect("joins");
        assert_eq!(j.group_kind(), Some(GroupKind::OneToOne));
    }
    let _ = (b, a);
}

#[test]
fn a_founding_commit_naming_two_other_accounts_is_refused_locally() {
    let a = account(0x31);
    let b = account(0x32);
    let c = account(0x33);
    let mut g = Group::create(&a, GroupKind::OneToOne).expect("found");
    let epoch = g.epoch();
    assert_eq!(
        reason(
            g.add_members_from_wire(&a, &[&kp_wire(&b), &kp_wire(&c)]),
            "two others",
        ),
        RefusalReason::OneToOneMembershipFixed,
    );
    assert_eq!(g.epoch(), epoch, "nothing was committed");
    assert_eq!(g.members().len(), 1);
}

#[test]
fn a_third_account_is_refused_at_the_proposer_and_on_the_direct_path() {
    let mut s = one_to_one(0x02);
    let c = account(0x41);
    assert_eq!(
        reason(
            s.a1_g.propose_add_member_from_wire(&s.a1, &kp_wire(&c)),
            "propose a third person",
        ),
        RefusalReason::OneToOneMembershipFixed,
    );
    assert_eq!(
        reason(
            s.b1_g.propose_add_member_from_wire(&s.b1, &kp_wire(&c)),
            "the other person proposes a third person",
        ),
        RefusalReason::OneToOneMembershipFixed,
    );
    let epoch = s.a1_g.epoch();
    assert_eq!(
        reason(
            s.a1_g.add_member_from_wire(&s.a1, &kp_wire(&c)),
            "direct add of a third person",
        ),
        RefusalReason::OneToOneMembershipFixed,
    );
    assert_eq!(s.a1_g.epoch(), epoch);
    assert!(s.a1_g.pending_proposals().is_empty());
}

#[test]
fn own_device_admit_and_retire_and_re_admission_still_work() {
    let mut s = one_to_one(0x03);
    let b2 = device(&s.b, 0xB2 ^ 0x03);

    // bob admits his own new device: allowed, and the owner's sweep lands it.
    let proposal = s
        .b1_g
        .propose_add_member_from_wire(&s.b1, &kp_wire(&b2))
        .expect("own device");
    let Incoming::Proposal(record) = s
        .a1_g
        .process_incoming_verifying_roles(&s.a1, &proposal)
        .expect("a1 receives")
    else {
        panic!("expected a proposal");
    };
    assert!(record.stored, "the receipt gate keeps an own-device add");
    let sweep = s
        .a1_g
        .commit_pending_proposals(&s.a1)
        .expect("sweep")
        .expect("a commit");
    s.b1_g
        .process_incoming_verifying_roles(&s.b1, &wire(sweep.commit))
        .expect("b1 applies");
    assert_eq!(s.a1_g.members().len(), 3);
    assert_eq!(accounts(&s.a1_g).len(), 2);

    // bob retires the new device: allowed, since his account keeps a leaf.
    let b2_leaf = leaf_of(&s.b1_g, &b2);
    let retire = s
        .b1_g
        .propose_remove_member(&s.b1, b2_leaf)
        .expect("retire own device");
    s.a1_g
        .process_incoming_verifying_roles(&s.a1, &retire)
        .expect("a1 receives");
    let sweep = s
        .a1_g
        .commit_pending_proposals(&s.a1)
        .expect("sweep")
        .expect("a commit");
    s.b1_g
        .process_incoming_verifying_roles(&s.b1, &wire(sweep.commit))
        .expect("b1 applies");
    assert_eq!(s.a1_g.members().len(), 2);

    // Re-admission: alice proposes bob's new device. Bob's account is already
    // in the conversation, so this is the ADR-0050 arm, not a third person.
    let b3 = device(&s.b, 0xB3 ^ 0x03);
    let readmit = s
        .a1_g
        .propose_add_member_from_wire(&s.a1, &kp_wire(&b3))
        .expect("re-admission of the other person's new device");
    s.b1_g
        .process_incoming_verifying_roles(&s.b1, &readmit)
        .expect("b1 stores the re-admission");
    let sweep = s
        .a1_g
        .commit_pending_proposals(&s.a1)
        .expect("sweep")
        .expect("a commit");
    s.b1_g
        .process_incoming_verifying_roles(&s.b1, &wire(sweep.commit))
        .expect("b1 applies the re-admission");
    assert_eq!(accounts(&s.a1_g).len(), 2);
    let _ = &s.a;
}

#[test]
fn removing_or_leaving_with_an_accounts_last_leaf_is_refused() {
    let mut s = one_to_one(0x04);
    let b1_leaf = leaf_of(&s.a1_g, &s.b1);
    assert_eq!(
        reason(
            s.a1_g.propose_remove_member(&s.a1, b1_leaf),
            "owner removes the other person",
        ),
        RefusalReason::OneToOneMembershipFixed,
    );
    assert_eq!(
        reason(
            s.a1_g.remove_member(&s.a1, b1_leaf),
            "direct removal of the other person",
        ),
        RefusalReason::OneToOneMembershipFixed,
    );
    let own = leaf_of(&s.b1_g, &s.b1);
    assert_eq!(
        reason(
            s.b1_g.propose_remove_member(&s.b1, own),
            "the other person leaves",
        ),
        RefusalReason::OneToOneMembershipFixed,
    );
    assert_eq!(s.a1_g.members().len(), 2);
}

/// Two `Remove`s of one account's two leaves are each harmless alone and
/// together remove a person; the commit-wide rule keeps the sweep from
/// carrying both.
#[test]
fn two_removals_of_one_persons_two_leaves_never_land_together() {
    let mut s = one_to_one(0x05);
    let b2 = device(&s.b, 0xB2 ^ 0x05);
    let readmit = s
        .a1_g
        .propose_add_member_from_wire(&s.a1, &kp_wire(&b2))
        .expect("re-admit");
    s.b1_g
        .process_incoming_verifying_roles(&s.b1, &readmit)
        .expect("b1 stores it");
    let sweep = s
        .a1_g
        .commit_pending_proposals(&s.a1)
        .expect("sweep")
        .expect("commit");
    s.b1_g
        .process_incoming_verifying_roles(&s.b1, &wire(sweep.commit))
        .expect("b1 applies");

    let l1 = leaf_of(&s.a1_g, &s.b1);
    let l2 = leaf_of(&s.a1_g, &b2);
    s.a1_g
        .propose_remove_member(&s.a1, l1)
        .expect("first removal is harmless alone");
    s.a1_g
        .propose_remove_member(&s.a1, l2)
        .expect("second removal is harmless alone too");
    let swept = s
        .a1_g
        .commit_pending_proposals(&s.a1)
        .expect("sweep")
        .expect("one of them commits");
    assert_eq!(swept.swept.len(), 1, "only one removal is carried");
    assert_eq!(
        accounts(&s.a1_g).len(),
        2,
        "bob is still in the conversation"
    );
}

#[test]
fn a_role_change_and_an_ownership_transfer_are_refused() {
    let mut s = one_to_one(0x06);
    let a_key: [u8; 32] = s.a.account_public_key().expect("key");
    let b_key: [u8; 32] = s.b.account_public_key().expect("key");
    let promote = RoleTable::new(a_key, vec![b_key]);
    for (what, got) in [
        ("set_group_roles", s.a1_g.set_group_roles(&s.a1, &promote)),
        (
            "propose_group_roles",
            s.a1_g.propose_group_roles(&s.a1, &promote),
        ),
        (
            "transfer_ownership",
            s.a1_g
                .transfer_ownership(&s.a1, &b_key, OutgoingOwner::Admin),
        ),
        (
            "propose_ownership_transfer",
            s.a1_g
                .propose_ownership_transfer(&s.a1, &b_key, OutgoingOwner::Admin),
        ),
    ] {
        assert_eq!(
            reason(got, what),
            RefusalReason::OneToOneHasNoRoles,
            "{what}"
        );
    }
}

#[test]
fn an_external_join_is_never_retained_and_never_approved() {
    let mut s = one_to_one(0x07);
    let stranger = account(0x71);
    let knock = stranger
        .create_external_join_proposal(&s.a1_g.group_id(), s.a1_g.epoch())
        .expect("a stranger can always ask");
    let incoming = s
        .a1_g
        .process_incoming_verifying_roles(&s.a1, &knock)
        .expect("the knock is consumed");
    let Incoming::ExternalJoinProposal {
        joiner_signature_key,
        retained,
    } = incoming
    else {
        panic!("expected an external-join proposal, got {incoming:?}");
    };
    assert!(!retained, "a one-to-one keeps nothing to approve");
    assert!(s.a1_g.pending_external_joins().is_empty());
    assert_eq!(
        reason(
            s.a1_g.approve_external_join(&s.a1, &joiner_signature_key),
            "approve",
        ),
        RefusalReason::OneToOneMembershipFixed,
    );
}

#[test]
fn a_role_change_in_a_group_keeps_the_kind_byte_identical() {
    let a = account(0x81);
    let b = account(0x82);
    let mut g = Group::create(&a, GroupKind::Group).expect("group");
    let add = g.add_member_from_wire(&a, &kp_wire(&b)).expect("add");
    let mut b_g = Group::join_from_welcome(&wire(add.welcome), &b).expect("join");
    let before = g.group_context_extension(0xFF03).expect("kind present");

    let a_key: [u8; 32] = a.account_public_key().expect("key");
    let b_key: [u8; 32] = b.account_public_key().expect("key");
    let promote = g
        .set_group_roles(&a, &RoleTable::new(a_key, vec![b_key]))
        .expect("promote");
    b_g.process_incoming_verifying_roles(&b, &promote)
        .expect("the peer accepts a role change that keeps the kind");
    assert_eq!(g.group_context_extension(0xFF03).as_ref(), Some(&before));
    assert_eq!(b_g.group_context_extension(0xFF03).as_ref(), Some(&before));

    let transfer = g
        .propose_ownership_transfer(&a, &b_key, OutgoingOwner::Admin)
        .expect("propose transfer");
    b_g.process_incoming_verifying_roles(&b, &transfer)
        .expect("b stores it");
    let sweep = b_g
        .commit_pending_proposals(&b)
        .expect("admin sweeps")
        .expect("commit");
    g.process_incoming_verifying_roles(&a, &wire(sweep.commit))
        .expect("the transfer lands");
    assert_eq!(g.group_roles().expect("table").owner(), &b_key);
    assert_eq!(g.group_context_extension(0xFF03).as_ref(), Some(&before));
    assert_eq!(b_g.group_context_extension(0xFF03).as_ref(), Some(&before));
    assert_eq!(g.group_kind(), Some(GroupKind::Group));
}
