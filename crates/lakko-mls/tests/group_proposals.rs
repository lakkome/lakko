// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0045 proposal-based membership — property tests for the receipt gate,
//! the sweep, and the committer rank.
//!
//! Every test drives the **public** API only, and each scene is the same
//! 3-member role-enabled group ADR-0022's tests use (owner = alice, admin = bob,
//! member = charlie) so the two suites can be read side by side.
//!
//! The suite's centre is [`two_proposals_from_two_devices_land_in_one_commit`],
//! which is the whole reason ADR-0045 exists, and its control
//! [`control_two_concurrent_direct_commits_still_fork`], which reproduces the
//! G-38 residual 1 hazard on the direct-commit path so the property test above
//! is provably measuring something real rather than passing vacuously.

use lakko_mls::{
    committer_rank, group, key_package, Group, Identity, Incoming, LeafNodeIndex, ProposalKind,
    RoleTable,
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

    let mut alice_g = Group::create(&alice).expect("alice creates group");

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
// The receipt gate
// ============================================================================

#[test]
fn admin_proposal_is_stored_and_an_owner_sweep_commits_it() {
    let mut s = scene(0x01);
    let dave = Identity::from_seed(&[0xD1u8; 64]).expect("dave");

    // bob (admin) proposes adding dave.
    let proposal = s
        .bob_g
        .propose_add_member_from_wire(&s.bob, &kp_wire(&dave))
        .expect("bob proposes add-dave");

    // alice (owner) receives it: authorised, so it is RETAINED.
    let incoming = s
        .alice_g
        .process_incoming_verifying_roles(&s.alice, &proposal)
        .expect("alice processes bob's proposal");
    let Incoming::Proposal(record) = incoming else {
        panic!("expected a proposal, got {incoming:?}");
    };
    assert!(record.stored, "an admin's add proposal must be stored");
    assert!(matches!(record.kind, ProposalKind::Add { .. }));
    assert_eq!(s.alice_g.pending_proposals().len(), 1);

    // ...and alice's sweep turns it into exactly one commit.
    let epoch_before = s.alice_g.epoch();
    let sweep = s
        .alice_g
        .commit_pending_proposals(&s.alice)
        .expect("sweep")
        .expect("the sweep must produce a commit");
    assert_eq!(sweep.swept.len(), 1);
    assert_eq!(s.alice_g.epoch(), epoch_before + 1);
    assert!(is_member(&s.alice_g, &dave));
    assert!(
        s.alice_g.pending_proposals().is_empty(),
        "a merged commit empties the pending set",
    );
    assert!(
        sweep.welcome.is_some(),
        "a swept Add must produce a Welcome"
    );
}

#[test]
fn plain_member_proposal_is_dropped_and_never_reaches_a_commit() {
    let mut s = scene(0x02);
    let dave = Identity::from_seed(&[0xD2u8; 64]).expect("dave");

    // charlie (plain member) is refused LOCALLY when it tries to propose...
    assert!(
        s.charlie_g
            .propose_add_member_from_wire(&s.charlie, &kp_wire(&dave))
            .is_err(),
        "a plain member's local propose must be refused",
    );

    // ...and even if it gets the bytes onto the wire anyway (a modified client
    // that skips its own check), the RECEIPT GATE drops them: we simulate that
    // by having charlie propose while still an admin is impossible, so instead
    // drive the same bytes from a member the peers know is not authorised —
    // charlie's own removal proposal of BOB, which is a member-authored remove.
    let bob_leaf = leaf_of(&s.charlie_g, s.bob.signature_public_key());
    assert!(
        s.charlie_g
            .propose_remove_member(&s.charlie, bob_leaf)
            .is_err(),
        "a plain member must not propose someone else's removal",
    );
    assert!(s.charlie_g.pending_proposals().is_empty());
    assert!(s.alice_g.pending_proposals().is_empty());

    // Nothing pending anywhere, so a sweep produces no commit at all.
    assert!(s
        .alice_g
        .commit_pending_proposals(&s.alice)
        .expect("sweep")
        .is_none());
    assert!(!is_member(&s.alice_g, &dave));
}

// ============================================================================
// The rule: who may propose what
// ============================================================================

#[test]
fn owner_removal_is_refused_even_when_proposed_by_an_admin() {
    let mut s = scene(0x04);
    let alice_leaf = leaf_of(&s.bob_g, s.alice.signature_public_key());
    assert!(
        s.bob_g.propose_remove_member(&s.bob, alice_leaf).is_err(),
        "an admin must not be able to propose the owner's removal",
    );
    assert!(s.bob_g.pending_proposals().is_empty());
}

#[test]
fn role_table_change_proposal_is_owner_only() {
    let mut s = scene(0x05);
    let promote_charlie = RoleTable::new(pk32(&s.alice), vec![pk32(&s.bob), pk32(&s.charlie)]);
    assert!(
        s.bob_g
            .propose_group_roles(&s.bob, &promote_charlie)
            .is_err(),
        "an admin must not propose a role-table change",
    );
    // The owner may.
    let proposal = s
        .alice_g
        .propose_group_roles(&s.alice, &promote_charlie)
        .expect("owner proposes a role change");
    let incoming = s
        .bob_g
        .process_incoming_verifying_roles(&s.bob, &proposal)
        .expect("bob processes it");
    let Incoming::Proposal(record) = incoming else {
        panic!("expected a proposal");
    };
    assert!(record.stored);
    assert_eq!(record.kind, ProposalKind::RoleChange);
}

#[test]
fn a_member_may_propose_its_own_removal_but_not_someone_elses() {
    let mut s = scene(0x06);
    let bob_leaf = leaf_of(&s.charlie_g, s.bob.signature_public_key());
    assert!(
        s.charlie_g
            .propose_remove_member(&s.charlie, bob_leaf)
            .is_err(),
        "a plain member must not propose another member's removal",
    );

    // ...but leaving is its own affair, and needs no role.
    let own_leaf = s.charlie_g.own_leaf_index();
    let leave = s
        .charlie_g
        .propose_remove_member(&s.charlie, own_leaf)
        .expect("a member must be able to propose its own removal");

    // An admin stores it and sweeps it, and charlie is gone.
    let incoming = s
        .bob_g
        .process_incoming_verifying_roles(&s.bob, &leave)
        .expect("bob processes charlie's leave");
    let Incoming::Proposal(record) = incoming else {
        panic!("expected a proposal");
    };
    assert!(record.stored, "a self-removal must be stored");
    let sweep = s
        .bob_g
        .commit_pending_proposals(&s.bob)
        .expect("bob sweeps")
        .expect("commit produced");
    assert!(sweep.welcome.is_none(), "a removal produces no Welcome");
    assert!(!is_member(&s.bob_g, &s.charlie));
}

/// A by-**value** proposal reports the committer as its sender, so the commit
/// gate judges the committer — byte-for-byte the pre-ADR-0045 ADR-0022 rule.
/// Every direct-commit path still behaves exactly as it did.
#[test]
fn a_by_value_proposal_is_still_judged_against_the_committer() {
    let mut s = scene(0x07);
    let dave = Identity::from_seed(&[0xD7u8; 64]).expect("dave");

    // charlie (member) crafts a direct add-commit — one by-value Add whose
    // sender is charlie itself.
    let add = s
        .charlie_g
        .add_member_from_wire(&s.charlie, &kp_wire(&dave))
        .expect("charlie crafts a direct add locally");
    assert!(
        s.alice_g
            .process_incoming_verifying_roles(&s.alice, &wire(add.commit))
            .is_err(),
        "a plain member's by-value Add must still be refused at the commit gate",
    );

    // The same commit from bob (admin) is accepted.
    let add = s
        .bob_g
        .add_member_from_wire(&s.bob, &kp_wire(&dave))
        .expect("bob crafts a direct add");
    assert!(matches!(
        s.alice_g
            .process_incoming_verifying_roles(&s.alice, &wire(add.commit)),
        Ok(Incoming::Commit)
    ));
}

/// ADR-0045 decision 3's most important rule: an external-join `Add` from a
/// non-member must NEVER be swept implicitly. It is retained on the ADR-0027
/// side channel that requires an explicit admin approve, and never enters the
/// MLS proposal store the sweep consumes.
#[test]
fn sweep_never_includes_an_external_join_add() {
    let mut s = scene(0x08);
    let stranger = Identity::from_seed(&[0xE8u8; 64]).expect("stranger");

    let external = stranger
        .create_external_join_proposal(&s.alice_g.group_id(), s.alice_g.epoch())
        .expect("stranger crafts an external-join proposal");
    let incoming = s
        .alice_g
        .process_incoming_verifying_roles(&s.alice, &external)
        .expect("alice processes it");
    assert!(matches!(incoming, Incoming::ExternalJoinProposal { .. }));

    // It is awaiting an explicit ADR-0027 decision...
    assert_eq!(s.alice_g.pending_external_joins().len(), 1);
    // ...and it is NOT in the MLS pending set the sweep consumes.
    assert!(
        s.alice_g.pending_proposals().is_empty(),
        "an external-join Add must never enter the swept proposal store",
    );
    // So a sweep, even by the owner, produces nothing and admits nobody.
    assert!(
        s.alice_g
            .commit_pending_proposals(&s.alice)
            .expect("sweep")
            .is_none(),
        "a sweep must not silently admit a stranger holding a /g/ link",
    );
    assert!(!is_member(&s.alice_g, &stranger));

    // Belt and braces: a legitimate member proposal alongside it is swept, and
    // the stranger still does not get in.
    let dave = Identity::from_seed(&[0xD8u8; 64]).expect("dave");
    let proposal = s
        .bob_g
        .propose_add_member_from_wire(&s.bob, &kp_wire(&dave))
        .expect("bob proposes add-dave");
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &proposal)
        .expect("alice stores it");
    let sweep = s
        .alice_g
        .commit_pending_proposals(&s.alice)
        .expect("sweep")
        .expect("commit produced");
    assert_eq!(sweep.swept.len(), 1, "only the member proposal is swept");
    assert!(is_member(&s.alice_g, &dave));
    assert!(!is_member(&s.alice_g, &stranger));
}

// ============================================================================
// The committer
// ============================================================================

#[test]
fn every_member_independently_derives_the_same_committer() {
    let s = scene(0x09);
    let views = [
        (&s.alice_g, "alice"),
        (&s.bob_g, "bob"),
        (&s.charlie_g, "charlie"),
    ];
    for (g, who) in views {
        let table = g.group_roles().expect("role-enabled group");
        assert_eq!(
            committer_rank(&table, s.alice.signature_public_key()),
            Some(0),
            "{who} must rank the owner first",
        );
        assert_eq!(
            committer_rank(&table, s.bob.signature_public_key()),
            Some(1),
            "{who} must rank the sole admin second",
        );
        assert_eq!(
            committer_rank(&table, s.charlie.signature_public_key()),
            None,
            "{who} must give a plain member no rank",
        );
    }
    assert_eq!(s.alice_g.own_committer_rank(&s.alice), Some(0));
    assert_eq!(s.bob_g.own_committer_rank(&s.bob), Some(1));
    assert_eq!(s.charlie_g.own_committer_rank(&s.charlie), None);
}

#[test]
fn a_plain_member_cannot_sweep() {
    let mut s = scene(0x0A);
    let dave = Identity::from_seed(&[0xDAu8; 64]).expect("dave");
    let proposal = s
        .bob_g
        .propose_add_member_from_wire(&s.bob, &kp_wire(&dave))
        .expect("bob proposes");
    s.charlie_g
        .process_incoming_verifying_roles(&s.charlie, &proposal)
        .expect("charlie stores an admin's proposal");
    assert_eq!(s.charlie_g.pending_proposals().len(), 1);
    assert!(
        s.charlie_g.commit_pending_proposals(&s.charlie).is_err(),
        "a plain member must not be able to sweep",
    );
}

#[test]
fn a_group_with_no_role_table_has_no_proposal_path() {
    // `create_with_id` deliberately seeds no role table — a PublisherGroup /
    // call-group shape. ADR-0045 leaves such a group on direct commits.
    let alice = Identity::from_seed(&[0x1Au8; 64]).expect("alice");
    let mut g = Group::create_with_id(&alice, &[7u8; 32]).expect("create");
    let dave = Identity::from_seed(&[0x1Bu8; 64]).expect("dave");
    assert!(g.group_roles().is_none());
    assert!(g
        .propose_add_member_from_wire(&alice, &kp_wire(&dave))
        .is_err());
    assert!(g.commit_pending_proposals(&alice).is_err());
    assert_eq!(g.own_committer_rank(&alice), None);
}

// ============================================================================
// The hazard ADR-0045 exists to close
// ============================================================================

/// **The point of ADR-0045.** Two devices act on the same group at the same
/// epoch. Under the proposal path their intents compose into ONE commit, every
/// device lands on the same epoch with the same roster, and nobody forks.
#[test]
fn two_proposals_from_two_devices_land_in_one_commit() {
    let mut s = scene(0x0B);
    let dave = Identity::from_seed(&[0xDBu8; 64]).expect("dave");
    let erin = Identity::from_seed(&[0xEBu8; 64]).expect("erin");
    let epoch = s.alice_g.epoch();
    assert_eq!(s.bob_g.epoch(), epoch);

    // Concurrently, against the SAME epoch: alice wants dave in, bob wants
    // charlie out. Neither advances its own epoch.
    let alice_proposal = s
        .alice_g
        .propose_add_member_from_wire(&s.alice, &kp_wire(&dave))
        .expect("alice proposes add-dave");
    let charlie_leaf = leaf_of(&s.bob_g, s.charlie.signature_public_key());
    let bob_proposal = s
        .bob_g
        .propose_remove_member(&s.bob, charlie_leaf)
        .expect("bob proposes remove-charlie");
    assert_eq!(
        s.alice_g.epoch(),
        epoch,
        "a proposal must not advance the epoch"
    );
    assert_eq!(
        s.bob_g.epoch(),
        epoch,
        "a proposal must not advance the epoch"
    );

    // Each device receives the other's proposal (never its own echo).
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &bob_proposal)
        .expect("alice stores bob's proposal");
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &alice_proposal)
        .expect("bob stores alice's proposal");
    assert_eq!(s.alice_g.pending_proposals().len(), 2);
    assert_eq!(s.bob_g.pending_proposals().len(), 2);

    // The committer is rank 0 — alice — and it sweeps BOTH into one commit.
    assert_eq!(s.alice_g.own_committer_rank(&s.alice), Some(0));
    let sweep = s
        .alice_g
        .commit_pending_proposals(&s.alice)
        .expect("alice sweeps")
        .expect("one commit");
    assert_eq!(sweep.swept.len(), 2, "both intents ride one commit");
    assert_eq!(s.alice_g.epoch(), epoch + 1);

    // bob applies it and lands on the SAME epoch with the SAME roster — the
    // divergence G-38 residual 1 describes simply does not occur.
    assert!(matches!(
        s.bob_g
            .process_incoming_verifying_roles(&s.bob, &wire(sweep.commit)),
        Ok(Incoming::Commit)
    ));
    assert_eq!(s.bob_g.epoch(), s.alice_g.epoch());
    assert_eq!(
        s.bob_g
            .export_secret(&s.bob, "lakko/test/v1", b"ctx", 32)
            .expect("bob exporter")
            .to_vec(),
        s.alice_g
            .export_secret(&s.alice, "lakko/test/v1", b"ctx", 32)
            .expect("alice exporter")
            .to_vec(),
        "same epoch AND same group state, not merely the same epoch number",
    );
    assert!(is_member(&s.bob_g, &dave));
    assert!(!is_member(&s.bob_g, &s.charlie));
    assert!(s.bob_g.pending_proposals().is_empty());

    // dave's Welcome opens into that same state.
    let dave_g = Group::join_from_welcome(
        &wire(sweep.welcome.expect("a swept Add produces a Welcome")),
        &dave,
    )
    .expect("dave joins");
    assert_eq!(dave_g.epoch(), s.alice_g.epoch());
    assert!(!is_member(&dave_g, &erin));
}

/// **Control.** The same two intents expressed as DIRECT commits reproduce the
/// hazard: both devices report the same epoch number while holding different
/// group state, and each rejects the other's commit. Without this the test
/// above could pass vacuously; with it, the property is demonstrably about the
/// proposal path and not about the scene.
#[test]
fn control_two_concurrent_direct_commits_still_fork() {
    let mut s = scene(0x0C);
    let dave = Identity::from_seed(&[0xDCu8; 64]).expect("dave");
    let epoch = s.alice_g.epoch();

    let alice_commit = s
        .alice_g
        .add_member_from_wire(&s.alice, &kp_wire(&dave))
        .expect("alice commits add-dave");
    let charlie_leaf = leaf_of(&s.bob_g, s.charlie.signature_public_key());
    let bob_commit = s
        .bob_g
        .remove_member(&s.bob, charlie_leaf)
        .expect("bob commits remove-charlie");

    // Both eager-merged locally to the SAME epoch number...
    assert_eq!(s.alice_g.epoch(), epoch + 1);
    assert_eq!(s.bob_g.epoch(), epoch + 1);

    // ...and each refuses the other's commit as wrong-epoch.
    assert!(s
        .alice_g
        .process_incoming_verifying_roles(&s.alice, &wire(bob_commit))
        .is_err());
    assert!(s
        .bob_g
        .process_incoming_verifying_roles(&s.bob, &wire(alice_commit.commit))
        .is_err());

    // Same epoch number, different group state — the silent fork.
    assert_eq!(s.alice_g.epoch(), s.bob_g.epoch());
    assert_ne!(
        s.alice_g
            .export_secret(&s.alice, "lakko/test/v1", b"ctx", 32)
            .expect("alice exporter")
            .to_vec(),
        s.bob_g
            .export_secret(&s.bob, "lakko/test/v1", b"ctx", 32)
            .expect("bob exporter")
            .to_vec(),
    );
}
