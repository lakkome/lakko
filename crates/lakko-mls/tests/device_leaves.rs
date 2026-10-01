// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0050 per-device conversation leaves — property tests.
//!
//! Every conversation leaf is a per-device identity bound to its account by a
//! signed binding in the leaf credential; every gate resolves a leaf to its
//! account before asking who may do what. These tests drive the public API
//! only and pin: the resolver, the two self-service arms, the owner's last
//! leaf, the per-account cap, the acting-leaf committer rank, the self-service
//! sweep, and per-account authorship.

use lakko_mls::{
    group, key_package, DeviceBinding, Group, GroupKind, Identity, Incoming, LeafNodeIndex,
    RefusalReason, Role,
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

/// A device identity bound to `owner`'s account.
fn device(owner: &Identity, tag: u8) -> Identity {
    Identity::from_seed_bound(&[tag; 32], &owner.ed25519_seed_bytes(), 1_700_000_000)
        .expect("bound device identity")
}

fn account_key(id: &Identity) -> Vec<u8> {
    id.account_public_key().expect("32-byte key").to_vec()
}

fn leaf_of(g: &Group, id: &Identity) -> LeafNodeIndex {
    let idx = g
        .members()
        .iter()
        .find(|m| m.signature_public_key == id.signature_public_key())
        .expect("member present")
        .leaf_index;
    LeafNodeIndex::new(idx)
}

fn reason(err: &lakko_mls::Error) -> RefusalReason {
    err.refusal_reason().expect("a typed refusal")
}

/// Deliver `bytes` to every group in `peers`.
fn deliver(bytes: &[u8], peers: &mut [(&Identity, &mut Group)]) {
    for (id, g) in peers.iter_mut() {
        g.process_incoming_verifying_roles(id, bytes)
            .expect("peer applies");
    }
}

/// alice's account `a`, her first device `a1` (creator), bob's account `b`
/// (an unbound Phase 1 leaf), charlie's device `c1` bound to charlie's
/// account `c`. Returns the identities and each member's group handle.
struct Scene {
    a: Identity,
    a1: Identity,
    b: Identity,
    c: Identity,
    c1: Identity,
    a1_g: Group,
    b_g: Group,
    c1_g: Group,
}

fn scene(tag: u8) -> Scene {
    let a = account(0xA0 ^ tag);
    let a1 = device(&a, 0xA1 ^ tag);
    let b = account(0xB0 ^ tag);
    let c = account(0xC0 ^ tag);
    let c1 = device(&c, 0xC1 ^ tag);

    let mut a1_g = Group::create(&a1, GroupKind::Group).expect("a1 creates");
    let add_b = a1_g
        .add_member_from_wire(&a1, &kp_wire(&b))
        .expect("a1 adds bob");
    let mut b_g = Group::join_from_welcome(&wire(add_b.welcome), &b).expect("bob joins");
    let add_c = a1_g
        .add_member_from_wire(&a1, &kp_wire(&c1))
        .expect("a1 adds c1");
    b_g.process_incoming_verifying_roles(&b, &wire(add_c.commit))
        .expect("bob applies");
    let c1_g = Group::join_from_welcome(&wire(add_c.welcome), &c1).expect("c1 joins");
    Scene {
        a,
        a1,
        b,
        c,
        c1,
        a1_g,
        b_g,
        c1_g,
    }
}

// ============================================================================
// The resolver
// ============================================================================

#[test]
fn a_bound_leaf_resolves_to_its_account_and_a_phase_one_leaf_to_itself() {
    let s = scene(0x01);
    let members = s.b_g.members();
    let of = |id: &Identity| {
        members
            .iter()
            .find(|m| m.signature_public_key == id.signature_public_key())
            .expect("present")
            .account_key
            .clone()
    };
    assert_eq!(
        of(&s.a1),
        account_key(&s.a),
        "a1 resolves to alice's account"
    );
    assert_ne!(
        s.a1.signature_public_key(),
        account_key(&s.a).as_slice(),
        "and its leaf key is not the account key"
    );
    assert_eq!(
        of(&s.b),
        s.b.signature_public_key(),
        "an unbound leaf is its own account"
    );
    assert_eq!(of(&s.c1), account_key(&s.c));

    // The role table names alice's ACCOUNT, so a1 is the owner.
    let table = s.a1_g.group_roles().expect("role table");
    assert_eq!(table.owner().as_slice(), account_key(&s.a));
    assert_eq!(s.a1_g.own_role(&s.a1), Some(Role::Owner));
    assert_eq!(s.b_g.own_role(&s.b), Some(Role::Member));
}

#[test]
fn a_forged_binding_is_refused_by_the_direct_add_the_proposal_and_the_welcome() {
    let mut s = scene(0x02);
    // A binding can only be installed on the device it names: the core
    // refuses a mismatched or corrupt one on the device itself...
    let mallory = account(0x77);
    let forged = device(&mallory, 0x78);
    let mut other = Identity::from_seed(&[0x79; 32]).expect("other device");
    assert!(other
        .set_device_binding(forged.device_binding().expect("bound"))
        .is_err());
    // ...so a leaf carrying one can only come from a non-Lakko client. Every
    // adder path refuses it, and so does a joiner that finds one in its roster.
    let bad = forged_credential_key_package();
    let Err(err) = s.a1_g.add_member_from_wire(&s.a1, &bad) else {
        panic!("direct add must be refused");
    };
    assert_eq!(reason(&err), RefusalReason::DeviceBindingInvalid);
    let err = s
        .a1_g
        .propose_add_member_from_wire(&s.a1, &bad)
        .expect_err("proposal refused");
    assert_eq!(reason(&err), RefusalReason::DeviceBindingInvalid);
    // And the receipt gate on a peer drops it even if the proposer's own
    // check were bypassed: bob proposing it is refused locally too.
    let err = s
        .b_g
        .propose_add_member_from_wire(&s.b, &bad)
        .expect_err("refused for a member as well");
    assert_eq!(reason(&err), RefusalReason::DeviceBindingInvalid);
}

/// A KeyPackage whose leaf credential is a binding that names the leaf's key
/// but whose signature is corrupt — the shape a tampering client produces.
fn forged_credential_key_package() -> Vec<u8> {
    let m = account(0x66);
    let y = Identity::from_seed(&[0x67; 32]).expect("y");
    let y_key: [u8; 32] = y.signature_public_key().try_into().expect("32");
    let mut bytes = DeviceBinding::sign(&m.ed25519_seed_bytes(), y_key, 1);
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    assert!(Identity::from_seed_with_binding(&[0x67; 32], &bytes).is_err());
    key_package::serialize(
        &y.adversary_key_package_with_credential(bytes.to_vec())
            .expect("adversary kp"),
    )
    .expect("serialize")
}

// ============================================================================
// The self-service arms
// ============================================================================

#[test]
fn a_member_adds_its_own_device_without_a_role_and_a_stranger_only_with_one() {
    let mut s = scene(0x03);
    // charlie (plain member) proposes adding charlie's second device.
    let c2 = device(&s.c, 0xC2);
    let proposal = s
        .c1_g
        .propose_add_member_from_wire(&s.c1, &kp_wire(&c2))
        .expect("own device");
    let got = s
        .b_g
        .process_incoming_verifying_roles(&s.b, &proposal)
        .expect("bob processes");
    let Incoming::Proposal(record) = got else {
        panic!("expected a proposal");
    };
    assert!(record.stored, "a member's own-device Add is retained");

    // charlie proposes adding a stranger: refused locally and by peers.
    let stranger = account(0x55);
    let err = s
        .c1_g
        .propose_add_member_from_wire(&s.c1, &kp_wire(&stranger))
        .expect_err("stranger refused");
    assert_eq!(reason(&err), RefusalReason::AddNotOwnerOrAdmin);
}

#[test]
fn a_member_re_admits_a_bound_device_of_an_account_already_in_the_group() {
    let mut s = scene(0x04);
    // bob (plain, unbound) proposes adding alice's NEW device a2 — alice's
    // account is already a member, so bob may.
    let a2 = device(&s.a, 0xA2);
    let proposal = s
        .b_g
        .propose_add_member_from_wire(&s.b, &kp_wire(&a2))
        .expect("re-admission by a plain member");
    let Incoming::Proposal(record) = s
        .c1_g
        .process_incoming_verifying_roles(&s.c1, &proposal)
        .expect("c1 processes")
    else {
        panic!("expected a proposal");
    };
    assert!(record.stored);
    // The owner's device sweeps it and a2 lands as a second owner leaf.
    s.a1_g
        .process_incoming_verifying_roles(&s.a1, &proposal)
        .expect("a1 processes");
    let sweep = s
        .a1_g
        .commit_pending_proposals(&s.a1)
        .expect("sweep")
        .expect("carried");
    deliver(
        &wire(sweep.commit),
        &mut [(&s.b, &mut s.b_g), (&s.c1, &mut s.c1_g)],
    );
    let a2_g =
        Group::join_from_welcome(&wire(sweep.welcome.expect("welcome")), &a2).expect("a2 joins");
    assert_eq!(a2_g.own_role(&a2), Some(Role::Owner), "a2 is the owner too");
    assert_eq!(a2_g.member_count(), 4);
}

#[test]
fn a_member_removes_its_own_device_without_a_role() {
    let mut s = scene(0x05);
    let c2 = device(&s.c, 0xC2);
    // Admit c2 via the owner.
    let p = s
        .c1_g
        .propose_add_member_from_wire(&s.c1, &kp_wire(&c2))
        .expect("own device");
    deliver(&p, &mut [(&s.a1, &mut s.a1_g), (&s.b, &mut s.b_g)]);
    let sweep = s
        .a1_g
        .commit_pending_proposals(&s.a1)
        .expect("sweep")
        .expect("carried");
    deliver(
        &wire(sweep.commit),
        &mut [(&s.b, &mut s.b_g), (&s.c1, &mut s.c1_g)],
    );
    let mut c2_g =
        Group::join_from_welcome(&wire(sweep.welcome.expect("welcome")), &c2).expect("c2 joins");

    // c2 removes c1 (its own account's other leaf): no role needed.
    let leaf = leaf_of(&c2_g, &s.c1);
    let p = c2_g
        .propose_remove_member(&c2, leaf)
        .expect("remove own device");
    let Incoming::Proposal(record) = s
        .b_g
        .process_incoming_verifying_roles(&s.b, &p)
        .expect("bob processes")
    else {
        panic!("proposal");
    };
    assert!(
        record.stored,
        "removing one's own account's leaf needs no role"
    );

    // c2 removing BOB is refused.
    let leaf = leaf_of(&c2_g, &s.b);
    let err = c2_g
        .propose_remove_member(&c2, leaf)
        .expect_err("not an admin");
    assert_eq!(reason(&err), RefusalReason::RemoveNotOwnerOrAdmin);
}

#[test]
fn the_owner_may_retire_a_device_but_never_its_last_leaf() {
    let mut s = scene(0x06);
    // Owner's last leaf: refused.
    let own = s.a1_g.own_leaf_index();
    let err = s
        .a1_g
        .propose_remove_member(&s.a1, own)
        .expect_err("last leaf");
    assert_eq!(reason(&err), RefusalReason::OwnerUnremovable);

    // Add a2, then a1 may retire itself.
    let a2 = device(&s.a, 0xA2);
    let p = s
        .a1_g
        .propose_add_member_from_wire(&s.a1, &kp_wire(&a2))
        .expect("own device");
    deliver(&p, &mut [(&s.b, &mut s.b_g), (&s.c1, &mut s.c1_g)]);
    let sweep = s
        .a1_g
        .commit_pending_proposals(&s.a1)
        .expect("sweep")
        .expect("carried");
    deliver(
        &wire(sweep.commit),
        &mut [(&s.b, &mut s.b_g), (&s.c1, &mut s.c1_g)],
    );
    let _a2_g =
        Group::join_from_welcome(&wire(sweep.welcome.expect("welcome")), &a2).expect("a2 joins");
    let own = s.a1_g.own_leaf_index();
    let p = s
        .a1_g
        .propose_remove_member(&s.a1, own)
        .expect("owner retires one of two devices");
    let Incoming::Proposal(record) = s
        .b_g
        .process_incoming_verifying_roles(&s.b, &p)
        .expect("bob processes")
    else {
        panic!("proposal");
    };
    assert!(record.stored);
}

// ============================================================================
// The cap
// ============================================================================

#[test]
fn an_account_cannot_exceed_the_per_group_device_cap() {
    let mut s = scene(0x07);
    // charlie already holds 1 leaf; propose 7 more distinct devices (total 8)
    // — all retained — then the 9th is refused.
    for i in 0..7u8 {
        let d = device(&s.c, 0xD0 + i);
        let p = s
            .c1_g
            .propose_add_member_from_wire(&s.c1, &kp_wire(&d))
            .unwrap_or_else(|e| panic!("device #{i}: {e}"));
        let Incoming::Proposal(record) = s
            .b_g
            .process_incoming_verifying_roles(&s.b, &p)
            .expect("bob processes")
        else {
            panic!("proposal");
        };
        assert!(record.stored, "device #{i} within the cap is retained");
    }
    let ninth = device(&s.c, 0xDF);
    let err = s
        .c1_g
        .propose_add_member_from_wire(&s.c1, &kp_wire(&ninth))
        .expect_err("ninth device");
    assert_eq!(reason(&err), RefusalReason::DeviceLimitReached);
}

// ============================================================================
// Committer rank and the self-service sweep
// ============================================================================

#[test]
fn only_the_lowest_leaf_of_a_ranked_account_acts_and_peers_agree() {
    let mut s = scene(0x08);
    let a2 = device(&s.a, 0xA2);
    let p = s
        .a1_g
        .propose_add_member_from_wire(&s.a1, &kp_wire(&a2))
        .expect("own device");
    deliver(&p, &mut [(&s.b, &mut s.b_g), (&s.c1, &mut s.c1_g)]);
    let sweep = s
        .a1_g
        .commit_pending_proposals(&s.a1)
        .expect("sweep")
        .expect("carried");
    deliver(
        &wire(sweep.commit),
        &mut [(&s.b, &mut s.b_g), (&s.c1, &mut s.c1_g)],
    );
    let a2_g =
        Group::join_from_welcome(&wire(sweep.welcome.expect("welcome")), &a2).expect("a2 joins");

    let r1 = s.a1_g.own_committer_rank(&s.a1);
    let r2 = a2_g.own_committer_rank(&a2);
    assert!(
        matches!((r1, r2), (Some(0), None) | (None, Some(0))),
        "exactly one of the owner's devices acts, got {r1:?} / {r2:?}"
    );
    let acting = if r1.is_some() { &s.a1 } else { &a2 };
    let idle = if r1.is_some() { &a2 } else { &s.a1 };
    assert!(
        acting.signature_public_key() < idle.signature_public_key(),
        "the acting device is the lowest leaf key"
    );
    // Every peer ranks the same device. Since ADR-0050 section 2.5a a plain
    // member account is ranked after the admins (here: none), in bytewise
    // account-key order among the plain accounts {bob, charlie}.
    let bob_rank = if account_key(&s.b) < account_key(&s.c) {
        1
    } else {
        2
    };
    for g in [&s.b_g, &s.c1_g] {
        assert_eq!(g.committer_rank_of(acting.signature_public_key()), Some(0));
        assert_eq!(g.committer_rank_of(idle.signature_public_key()), None);
        assert_eq!(
            g.committer_rank_of(s.b.signature_public_key()),
            Some(bob_rank)
        );
    }
}

#[test]
fn a_self_service_only_pending_set_is_sweepable_by_its_own_account() {
    let mut s = scene(0x09);
    // charlie is a plain member: no rank while the set is empty.
    assert_eq!(s.c1_g.own_committer_rank(&s.c1), None);
    let c2 = device(&s.c, 0xC2);
    let p = s
        .c1_g
        .propose_add_member_from_wire(&s.c1, &kp_wire(&c2))
        .expect("own device");
    deliver(&p, &mut [(&s.a1, &mut s.a1_g), (&s.b, &mut s.b_g)]);
    // Now charlie holds its account rank (ADR-0050 section 2.5a): after the
    // (zero) admins, in bytewise account-key order among {bob, charlie}.
    let charlie_rank = if account_key(&s.c) < account_key(&s.b) {
        1
    } else {
        2
    };
    assert_eq!(s.c1_g.own_committer_rank(&s.c1), Some(charlie_rank));
    // And charlie's sweep lands on every peer.
    let sweep = s
        .c1_g
        .commit_pending_proposals(&s.c1)
        .expect("charlie sweeps its own device")
        .expect("carried");
    deliver(
        &wire(sweep.commit),
        &mut [(&s.a1, &mut s.a1_g), (&s.b, &mut s.b_g)],
    );
    let c2_g =
        Group::join_from_welcome(&wire(sweep.welcome.expect("welcome")), &c2).expect("c2 joins");
    assert_eq!(c2_g.member_count(), 4);
    // Once swept, charlie is a plain member again.
    assert_eq!(s.c1_g.own_committer_rank(&s.c1), None);

    // A set that mixes in someone else's request: since ADR-0050 section 2.5a
    // charlie keeps its rank (it still holds its own device's Add), but its
    // sweep carries only that Add and leaves the owner's add of a stranger
    // out. (Before 2.5a the mixed set gave charlie no rank at all.)
    let d = account(0x99);
    let p = s
        .a1_g
        .propose_add_member_from_wire(&s.a1, &kp_wire(&d))
        .expect("owner proposes a stranger");
    let c3 = device(&s.c, 0xC3);
    let _p2 = s
        .c1_g
        .propose_add_member_from_wire(&s.c1, &kp_wire(&c3))
        .expect("own device");
    s.c1_g
        .process_incoming_verifying_roles(&s.c1, &p)
        .expect("c1 stores the owner's add");
    assert_eq!(s.c1_g.own_committer_rank(&s.c1), Some(charlie_rank));
    let sweep = s
        .c1_g
        .commit_pending_proposals(&s.c1)
        .expect("charlie sweeps")
        .expect("carried");
    assert_eq!(sweep.swept.len(), 1, "only charlie's own device");
    assert!(s.c1_g.pending_proposals().is_empty());
    assert!(!s
        .c1_g
        .members()
        .iter()
        .any(|m| m.signature_public_key == d.signature_public_key()));
}

// ============================================================================
// Authorship
// ============================================================================

#[test]
fn sender_account_names_the_person_from_any_of_their_devices() {
    let mut s = scene(0x0A);
    let a2 = device(&s.a, 0xA2);
    let p = s
        .a1_g
        .propose_add_member_from_wire(&s.a1, &kp_wire(&a2))
        .expect("own device");
    deliver(&p, &mut [(&s.b, &mut s.b_g), (&s.c1, &mut s.c1_g)]);
    let sweep = s
        .a1_g
        .commit_pending_proposals(&s.a1)
        .expect("sweep")
        .expect("carried");
    deliver(
        &wire(sweep.commit),
        &mut [(&s.b, &mut s.b_g), (&s.c1, &mut s.c1_g)],
    );
    let mut a2_g =
        Group::join_from_welcome(&wire(sweep.welcome.expect("welcome")), &a2).expect("a2 joins");

    let m1 = s
        .a1_g
        .create_application_message(&s.a1, b"from device one")
        .expect("send");
    let m2 = a2_g
        .create_application_message(&a2, b"from device two")
        .expect("send");
    for m in [m1, m2] {
        let got = s
            .b_g
            .process_incoming_verifying_roles(&s.b, &wire(m))
            .expect("bob receives");
        let Incoming::Application {
            sender_leaf,
            sender_account,
            ..
        } = got
        else {
            panic!("application");
        };
        assert!(sender_leaf.is_some());
        assert_eq!(
            sender_account.as_deref(),
            Some(account_key(&s.a).as_slice()),
            "both devices are the same author"
        );
    }
}

// ============================================================================
// A person's devices share one commit
// ============================================================================

/// Regression for the two-device 1x1 chat on the test stack: a founder that
/// added a person's two devices with one direct commit each — dropping the
/// commits as "nobody to broadcast to" — left the first device holding a
/// Welcome for epoch 1 while the group stood at epoch 2, with the epoch-2
/// commit on no journal. `add_members` puts every leaf in ONE commit: one
/// Welcome, one epoch, and a message from the founder opens on both devices.
#[test]
fn a_persons_devices_added_in_one_commit_land_at_one_epoch_and_both_read_the_founder() {
    let a = account(0x71);
    let a1 = device(&a, 0x72);
    let a2 = device(&a, 0x73);
    let b = account(0x74);
    let b1 = device(&b, 0x75);

    let mut b1_g = Group::create(&b1, GroupKind::Group).expect("b1 creates");
    let added = b1_g
        .add_members_from_wire(&b1, &[&kp_wire(&a1), &kp_wire(&a2)])
        .expect("b1 adds both of alice's devices in one commit");
    let welcome = wire(added.welcome);

    // The same Welcome bytes reach each device's own mailbox.
    let mut a1_g = Group::join_from_welcome(&welcome, &a1).expect("a1 joins");
    let mut a2_g = Group::join_from_welcome(&welcome, &a2).expect("a2 joins");
    assert_eq!(b1_g.epoch(), 1, "one commit, one epoch advance");
    assert_eq!(a1_g.epoch(), b1_g.epoch());
    assert_eq!(a2_g.epoch(), b1_g.epoch());
    assert_eq!(b1_g.members().len(), 3);

    let msg = wire(
        b1_g.create_application_message(&b1, b"hi from bob")
            .expect("bob seals"),
    );
    for (id, g) in [(&a1, &mut a1_g), (&a2, &mut a2_g)] {
        match g
            .process_incoming_verifying_roles(id, &msg)
            .expect("device opens")
        {
            Incoming::Application {
                bytes,
                sender_account,
                ..
            } => {
                assert_eq!(bytes, b"hi from bob");
                assert_eq!(sender_account, Some(account_key(&b)));
            },
            other => panic!("expected an application message, got {other:?}"),
        }
    }

    // The empty list is refused rather than committing nothing.
    assert!(b1_g.add_members_from_wire(&b1, &[]).is_err());
}

/// ADR-0050 section 2.6 — ONE published KeyPackage opens every Welcome sealed
/// to it (a last-resort package): the pairing fan-out and a contact's
/// re-admission fetch a device's one package and admit it to N groups.
#[test]
fn one_published_key_package_opens_a_welcome_per_group() {
    let a = account(0x81);
    let a1 = device(&a, 0x82);
    let a2 = device(&a, 0x83);
    let b = account(0x84);
    let b1 = device(&b, 0x85);
    let kp = kp_wire(&a2);

    let mut g1 = Group::create(&a1, GroupKind::Group).expect("a1 founds group 1");
    let mut g2 = Group::create(&b1, GroupKind::Group).expect("b1 founds group 2");
    let w1 = wire(
        g1.add_member_from_wire(&a1, &kp)
            .expect("group 1 admits a2")
            .welcome,
    );
    let w2 = wire(
        g2.add_member_from_wire(&b1, &kp)
            .expect("group 2 admits a2")
            .welcome,
    );

    let j1 = Group::join_from_welcome(&w1, &a2).expect("a2 joins group 1");
    let j2 = Group::join_from_welcome(&w2, &a2)
        .expect("a2 joins group 2 from the SAME package after the first join");
    assert_eq!(j1.epoch(), g1.epoch());
    assert_eq!(j2.epoch(), g2.epoch());
}

// ============================================================================
// ADR-0053 walk finding: an admin's pending join request read "Member" even
// for a contact, because the request is keyed on the joiner's device leaf and
// contacts on the account. The retained KeyPackage carries the verified
// device binding; the accessor names the account behind it.
// ============================================================================

#[test]
fn a_pending_join_request_names_the_joiners_account() {
    let owner_account = account(0x61);
    let owner = device(&owner_account, 0x62);
    let mut g = Group::create(&owner, GroupKind::Group).expect("group");

    let joiner_account = account(0x63);
    let joiner_device = device(&joiner_account, 0x64);
    let proposal = joiner_device
        .create_external_join_proposal(&g.group_id(), g.epoch())
        .expect("a bound device crafts an external-join proposal");
    assert!(matches!(
        g.process_incoming_verifying_roles(&owner, &proposal),
        Ok(Incoming::ExternalJoinProposal { .. })
    ));
    let leaf_key = joiner_device.signature_public_key().to_vec();
    assert_eq!(g.pending_external_joins(), vec![leaf_key.clone()]);

    // The leaf key names the phone; the accessor names the person.
    assert_ne!(leaf_key, account_key(&joiner_account));
    assert_eq!(
        g.pending_external_join_account_key(&leaf_key)
            .map(|k| k.to_vec()),
        Some(account_key(&joiner_account)),
    );

    // An unbound Phase 1 joiner is its own account.
    let phase1 = account(0x65);
    let proposal = phase1
        .create_external_join_proposal(&g.group_id(), g.epoch())
        .expect("phase 1 proposal");
    g.process_incoming_verifying_roles(&owner, &proposal)
        .expect("stored");
    assert_eq!(
        g.pending_external_join_account_key(phase1.signature_public_key())
            .map(|k| k.to_vec()),
        Some(phase1.signature_public_key().to_vec()),
    );

    // No such request: nothing.
    assert!(g.pending_external_join_account_key(&[0u8; 32]).is_none());
}
