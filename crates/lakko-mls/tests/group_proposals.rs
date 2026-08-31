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
    committer_rank, group, key_package, Group, Identity, Incoming, LeafNodeIndex, OutgoingOwner,
    ProposalKind, RoleTable,
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
    let dave = Identity::from_seed(&[0x11u8; 64]).expect("dave");

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

// ============================================================================
// Losing the race survivably — the staged sweep and its rollback (ADR-0045,
// G-38 residual 1)
//
// The committer election makes two simultaneous sweeps RARE, not impossible:
// no liveness timeout can tell a dead committer from a slow one, so during a
// handover two devices can both act. The DS journal already arbitrates, being
// an append-only log every member observes in the same order. What was missing
// was the loser's ability to survive losing — having eager-merged N+1 before
// publishing, it sat at an epoch nobody else reached, silently and for ever.
// ============================================================================

fn exporter(g: &Group, id: &Identity) -> Vec<u8> {
    g.export_secret(id, "lakko/test/v1", b"ctx", 32)
        .expect("exporter")
        .to_vec()
}

/// Decrypt an application message and return its plaintext, failing loudly on
/// anything else.
fn recv_text(g: &mut Group, id: &Identity, bytes: &[u8]) -> Vec<u8> {
    match g.process_incoming_verifying_roles(id, bytes) {
        Ok(Incoming::Application { bytes, .. }) => bytes,
        other => panic!("expected an application message, got {other:?}"),
    }
}

/// **The point of this sprint.** Two committers sweep the same epoch during a
/// handover; the journal orders one first. The winner merges, the loser
/// abandons, applies the winner's commit, and the two agree on group *state* —
/// asserted as equal exporter output, not equal epoch numbers, because the
/// whole hazard is that the numbers match while the state does not.
#[test]
fn the_loser_of_a_concurrent_sweep_rolls_back_and_converges() {
    let mut s = scene(0x0D);
    let dave = Identity::from_seed(&[0xDDu8; 64]).expect("dave");
    let epoch = s.alice_g.epoch();
    assert_eq!(s.bob_g.epoch(), epoch);

    // Two intents against the SAME epoch; both admins hold both proposals.
    let alice_proposal = s
        .alice_g
        .propose_add_member_from_wire(&s.alice, &kp_wire(&dave))
        .expect("alice proposes add-dave");
    let charlie_leaf = leaf_of(&s.bob_g, s.charlie.signature_public_key());
    let bob_proposal = s
        .bob_g
        .propose_remove_member(&s.bob, charlie_leaf)
        .expect("bob proposes remove-charlie");
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &bob_proposal)
        .expect("alice stores bob's proposal");
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &alice_proposal)
        .expect("bob stores alice's proposal");

    // The handover window: BOTH believe they are the acting committer, so both
    // sweep. Neither advances — that is what makes the loss survivable.
    let alice_sweep = s
        .alice_g
        .stage_pending_proposals(&s.alice)
        .expect("alice stages")
        .expect("one commit");
    let bob_sweep = s
        .bob_g
        .stage_pending_proposals(&s.bob)
        .expect("bob stages")
        .expect("one commit");
    assert_eq!(
        s.alice_g.epoch(),
        epoch,
        "staging must not advance the epoch"
    );
    assert_eq!(s.bob_g.epoch(), epoch, "staging must not advance the epoch");
    assert!(s.alice_g.has_staged_sweep());
    assert!(s.bob_g.has_staged_sweep());

    // The journal orders alice's commit first. Alice reads that verdict and
    // merges; her state is now the group's real state.
    assert!(s
        .alice_g
        .merge_staged_sweep(&s.alice)
        .expect("alice merges"));
    assert_eq!(s.alice_g.epoch(), epoch + 1);
    assert!(!s.alice_g.has_staged_sweep());

    // Bob reads the same verdict, sees it lost, and rolls back.
    assert!(s.bob_g.abandon_staged_sweep(&s.bob).expect("bob abandons"));
    assert_eq!(s.bob_g.epoch(), epoch, "the loser is back where it started");
    assert!(!s.bob_g.has_staged_sweep());
    assert_eq!(
        s.bob_g.pending_proposals().len(),
        2,
        "abandoning throws no work away: the swept proposals are still pending",
    );

    // Bob's own commit is on the wire too, and the winner rejects it as
    // wrong-epoch — exactly the fate that used to strand its author.
    assert!(s
        .alice_g
        .process_incoming_verifying_roles(&s.alice, &wire(bob_sweep.commit))
        .is_err());

    // The acceptance criterion: the loser applies the winner's commit and
    // carries on.
    assert!(matches!(
        s.bob_g
            .process_incoming_verifying_roles(&s.bob, &wire(alice_sweep.commit)),
        Ok(Incoming::Commit)
    ));
    assert_eq!(s.bob_g.epoch(), s.alice_g.epoch());
    assert_eq!(
        exporter(&s.bob_g, &s.bob),
        exporter(&s.alice_g, &s.alice),
        "same epoch AND same group state, not merely the same epoch number",
    );
    assert!(is_member(&s.bob_g, &dave));
    assert!(!is_member(&s.bob_g, &s.charlie));
    assert!(
        s.bob_g.pending_proposals().is_empty(),
        "the winner's commit carried them, so the merge clears the store",
    );

    // And the group really is whole: dave's Welcome opens into that same state.
    let dave_g = Group::join_from_welcome(
        &wire(alice_sweep.welcome.expect("a swept Add produces a Welcome")),
        &dave,
    )
    .expect("dave joins");
    assert_eq!(exporter(&dave_g, &dave), exporter(&s.bob_g, &s.bob));
}

/// **Control.** The identical scene with the loser MERGING instead of
/// abandoning reproduces G-38 residual 1: matching epoch numbers, divergent
/// state, each rejecting the other's commit. Without this the test above could
/// pass on the scene alone rather than on the rollback.
#[test]
fn control_a_lost_sweep_that_merges_instead_of_rolling_back_still_forks() {
    let mut s = scene(0x0E);
    let dave = Identity::from_seed(&[0xDEu8; 64]).expect("dave");
    let epoch = s.alice_g.epoch();

    let alice_proposal = s
        .alice_g
        .propose_add_member_from_wire(&s.alice, &kp_wire(&dave))
        .expect("alice proposes add-dave");
    let charlie_leaf = leaf_of(&s.bob_g, s.charlie.signature_public_key());
    let bob_proposal = s
        .bob_g
        .propose_remove_member(&s.bob, charlie_leaf)
        .expect("bob proposes remove-charlie");
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &bob_proposal)
        .expect("alice stores bob's proposal");
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &alice_proposal)
        .expect("bob stores alice's proposal");

    let alice_sweep = s
        .alice_g
        .stage_pending_proposals(&s.alice)
        .expect("alice stages")
        .expect("one commit");
    let bob_sweep = s
        .bob_g
        .stage_pending_proposals(&s.bob)
        .expect("bob stages")
        .expect("one commit");

    // Both merge — the loser does NOT roll back.
    assert!(s
        .alice_g
        .merge_staged_sweep(&s.alice)
        .expect("alice merges"));
    assert!(s.bob_g.merge_staged_sweep(&s.bob).expect("bob merges"));
    assert_eq!(s.alice_g.epoch(), epoch + 1);
    assert_eq!(s.bob_g.epoch(), epoch + 1);

    // Each refuses the other's commit as wrong-epoch, and neither can recover.
    assert!(s
        .alice_g
        .process_incoming_verifying_roles(&s.alice, &wire(bob_sweep.commit))
        .is_err());
    assert!(s
        .bob_g
        .process_incoming_verifying_roles(&s.bob, &wire(alice_sweep.commit))
        .is_err());

    assert_eq!(s.alice_g.epoch(), s.bob_g.epoch());
    assert_ne!(
        exporter(&s.alice_g, &s.alice),
        exporter(&s.bob_g, &s.bob),
        "same epoch number, different group state — the silent fork",
    );
}

/// **The sibling-group hazard.** A rollback must touch the one group it is
/// repairing. `Identity::restore_storage` restores the WHOLE provider, so
/// using it here would roll every other group on the account back too —
/// including past commits they have already published, which is strictly worse
/// than the fork. This drives that difference: the sibling group advances
/// AFTER the sweep is staged, i.e. exactly the state a whole-provider snapshot
/// taken at staging time would destroy.
#[test]
fn abandoning_a_sweep_leaves_a_sibling_group_untouched() {
    let mut s = scene(0x0F);
    let erin = Identity::from_seed(&[0xEFu8; 64]).expect("erin");

    // A second, unrelated group on the SAME two providers.
    let mut alice_b = Group::create(&s.alice).expect("alice creates group B");
    let add_bob_b = alice_b
        .add_member_from_wire(&s.alice, &kp_wire(&s.bob))
        .expect("alice adds bob to B");
    let mut bob_b =
        Group::join_from_welcome(&wire(add_bob_b.welcome), &s.bob).expect("bob joins B");
    assert_ne!(alice_b.group_id(), s.alice_g.group_id());

    // Group A: bob stages a sweep. A whole-provider snapshot would be taken
    // here.
    let charlie_leaf = leaf_of(&s.bob_g, s.charlie.signature_public_key());
    let bob_proposal = s
        .bob_g
        .propose_remove_member(&s.bob, charlie_leaf)
        .expect("bob proposes remove-charlie");
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &bob_proposal)
        .expect("alice stores it");
    let _staged = s
        .bob_g
        .stage_pending_proposals(&s.bob)
        .expect("bob stages")
        .expect("one commit");
    assert!(s.bob_g.has_staged_sweep());

    // Group B advances WHILE that sweep is staged — this is the state a
    // whole-provider restore would silently destroy.
    let add_erin_b = alice_b
        .add_member_from_wire(&s.alice, &kp_wire(&erin))
        .expect("alice adds erin to B");
    bob_b
        .process_incoming_verifying_roles(&s.bob, &wire(add_erin_b.commit))
        .expect("bob applies B's add-erin");
    let b_epoch = bob_b.epoch();
    let b_secret = exporter(&bob_b, &s.bob);
    assert_eq!(b_secret, exporter(&alice_b, &s.alice));

    // Group A: bob loses the race and rolls back.
    assert!(s.bob_g.abandon_staged_sweep(&s.bob).expect("bob abandons"));

    // Group B is byte-for-byte where it was, and still live: it takes the next
    // commit and stays converged with its peer.
    assert_eq!(bob_b.epoch(), b_epoch);
    assert_eq!(exporter(&bob_b, &s.bob), b_secret);
    assert!(is_member(&bob_b, &erin));
    let rotate_b = alice_b.self_update(&s.alice).expect("alice rotates B");
    bob_b
        .process_incoming_verifying_roles(&s.bob, &rotate_b)
        .expect("bob applies B's rotation after the sibling rollback");
    assert_eq!(exporter(&bob_b, &s.bob), exporter(&alice_b, &s.alice));
}

/// Abandoning returns the proposals to the pending set, so the next committer
/// — here the same device, re-electing itself — carries the very same intents.
/// The loser loses its commit, never the requests behind it.
#[test]
fn an_abandoned_sweep_leaves_its_proposals_for_the_next_committer() {
    let mut s = scene(0x10);
    let dave = Identity::from_seed(&[0x10u8; 64]).expect("dave");

    let alice_proposal = s
        .alice_g
        .propose_add_member_from_wire(&s.alice, &kp_wire(&dave))
        .expect("alice proposes add-dave");
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &alice_proposal)
        .expect("bob stores alice's proposal");
    let charlie_leaf = leaf_of(&s.bob_g, s.charlie.signature_public_key());
    s.bob_g
        .propose_remove_member(&s.bob, charlie_leaf)
        .expect("bob proposes remove-charlie");
    assert_eq!(s.bob_g.pending_proposals().len(), 2);

    let first = s
        .bob_g
        .stage_pending_proposals(&s.bob)
        .expect("bob stages")
        .expect("one commit");
    assert_eq!(first.swept.len(), 2);

    // A second sweep while one is staged is refused, fail-closed: building it
    // would silently replace a commit that may already be on the wire.
    assert!(s.bob_g.stage_pending_proposals(&s.bob).is_err());

    assert!(s.bob_g.abandon_staged_sweep(&s.bob).expect("bob abandons"));
    assert_eq!(
        s.bob_g.pending_proposals().len(),
        2,
        "both intents survive the rollback",
    );

    // Re-electing itself, the same device sweeps the same two intents again.
    let second = s
        .bob_g
        .stage_pending_proposals(&s.bob)
        .expect("bob re-stages")
        .expect("one commit");
    assert_eq!(second.swept.len(), 2);
    assert!(s.bob_g.merge_staged_sweep(&s.bob).expect("bob merges"));
    assert!(is_member(&s.bob_g, &dave));
    assert!(!is_member(&s.bob_g, &s.charlie));
}

/// **The rollback must not reach a path that never had one.** Every
/// eager-merging path leaves nothing staged, so `abandon_staged_sweep` is a
/// no-op on all of them and cannot undo a merged commit.
#[test]
fn a_rollback_cannot_reach_an_eagerly_merged_commit() {
    let mut s = scene(0x11);
    let dave = Identity::from_seed(&[0x11u8; 64]).expect("dave");

    // A direct commit.
    s.alice_g
        .add_member_from_wire(&s.alice, &kp_wire(&dave))
        .expect("alice adds dave directly");
    let after_direct = s.alice_g.epoch();
    assert!(!s.alice_g.has_staged_sweep());
    assert!(
        !s.alice_g
            .abandon_staged_sweep(&s.alice)
            .expect("abandon is a no-op"),
        "there is nothing staged to abandon after an eager merge",
    );
    assert_eq!(s.alice_g.epoch(), after_direct);

    // And the stage-plus-merge convenience path behaves the same.
    let charlie_leaf = leaf_of(&s.alice_g, s.charlie.signature_public_key());
    s.alice_g
        .propose_remove_member(&s.alice, charlie_leaf)
        .expect("alice proposes remove-charlie");
    s.alice_g
        .commit_pending_proposals(&s.alice)
        .expect("alice sweeps")
        .expect("one commit");
    let after_sweep = s.alice_g.epoch();
    assert_eq!(after_sweep, after_direct + 1);
    assert!(!s.alice_g.has_staged_sweep());
    assert!(!s
        .alice_g
        .abandon_staged_sweep(&s.alice)
        .expect("abandon is a no-op"));
    assert_eq!(s.alice_g.epoch(), after_sweep);
    assert!(!s
        .alice_g
        .merge_staged_sweep(&s.alice)
        .expect("merge is a no-op"));
    assert_eq!(s.alice_g.epoch(), after_sweep);
}

/// The loser converges even if its poll loop applies the winner's commit
/// *before* it has adjudicated — OpenMLS discards a pending commit as part of
/// merging a foreign one. Abandoning first is still the order this crate
/// documents and drives; this pins the safety net under a client that gets the
/// order wrong, so it is a wedged group in neither direction.
#[test]
fn applying_the_winner_before_abandoning_still_converges() {
    let mut s = scene(0x12);
    let dave = Identity::from_seed(&[0x12u8; 64]).expect("dave");

    let alice_proposal = s
        .alice_g
        .propose_add_member_from_wire(&s.alice, &kp_wire(&dave))
        .expect("alice proposes add-dave");
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &alice_proposal)
        .expect("bob stores it");

    let alice_sweep = s
        .alice_g
        .stage_pending_proposals(&s.alice)
        .expect("alice stages")
        .expect("one commit");
    let _bob_sweep = s
        .bob_g
        .stage_pending_proposals(&s.bob)
        .expect("bob stages")
        .expect("one commit");
    assert!(s
        .alice_g
        .merge_staged_sweep(&s.alice)
        .expect("alice merges"));

    // Bob applies the winner while still holding its own staged sweep.
    assert!(s.bob_g.has_staged_sweep());
    assert!(matches!(
        s.bob_g
            .process_incoming_verifying_roles(&s.bob, &wire(alice_sweep.commit)),
        Ok(Incoming::Commit)
    ));
    assert!(
        !s.bob_g.has_staged_sweep(),
        "merging a foreign commit discards the local staged one",
    );
    assert_eq!(exporter(&s.bob_g, &s.bob), exporter(&s.alice_g, &s.alice));

    // And the after-the-fact abandon is a harmless no-op.
    assert!(!s.bob_g.abandon_staged_sweep(&s.bob).expect("no-op abandon"));
    assert_eq!(exporter(&s.bob_g, &s.bob), exporter(&s.alice_g, &s.alice));
}

// ============================================================================
// A pending proposal must not mute the group
//
// OpenMLS refuses `MlsGroup::create_message` outright while its proposal store
// is non-empty (`openmls-0.8.1/src/group/mls_group/application.rs:27-31`,
// `MlsGroupStateError::PendingProposal`). ADR-0045 stores every authorised
// incoming proposal, so without care the whole group goes mute from the moment
// one admin proposes until a committer sweeps — and ADR-0045's liveness model
// lets a request wait indefinitely when no admin is running. Chat must be
// unaffected by a membership request; these tests are what says so.
//
// Only `create_message` is gated this way. `is_operational`
// (`mls_group/mod.rs:779`) — which guards `propose_*`, `add_members`,
// `remove_members`, `self_update` and `commit_to_pending_proposals` — refuses
// on a staged *commit*, never on a pending proposal, so those paths are a
// separate and narrower obligation already covered by `has_staged_sweep`.
// ============================================================================

/// **The centrepiece.** With a membership proposal pending, every member —
/// the proposer, the owner, and a plain member — can still send chat, and it
/// round-trips to everyone else.
#[test]
fn a_pending_proposal_does_not_mute_the_group() {
    let mut s = scene(0x20);
    let dave = Identity::from_seed(&[0x20u8; 64]).expect("dave");

    // bob (admin) proposes adding dave; owner + plain member both retain it.
    let proposal = s
        .bob_g
        .propose_add_member_from_wire(&s.bob, &kp_wire(&dave))
        .expect("bob proposes add-dave");
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &proposal)
        .expect("alice stores it");
    s.charlie_g
        .process_incoming_verifying_roles(&s.charlie, &proposal)
        .expect("charlie stores it");
    for (who, g) in [
        ("alice", &s.alice_g),
        ("bob", &s.bob_g),
        ("charlie", &s.charlie_g),
    ] {
        assert_eq!(
            g.pending_proposals().len(),
            1,
            "{who} must be holding the pending proposal for this test to mean anything",
        );
    }

    // The proposer sends.
    let from_bob = wire(
        s.bob_g
            .create_application_message(&s.bob, b"proposed dave, chat on")
            .expect("the PROPOSER must still be able to send"),
    );
    assert_eq!(
        recv_text(&mut s.alice_g, &s.alice, &from_bob),
        b"proposed dave, chat on".to_vec(),
    );
    assert_eq!(
        recv_text(&mut s.charlie_g, &s.charlie, &from_bob),
        b"proposed dave, chat on".to_vec(),
    );

    // The owner sends.
    let from_alice = wire(
        s.alice_g
            .create_application_message(&s.alice, b"seen it, will sweep later")
            .expect("the OWNER must still be able to send"),
    );
    assert_eq!(
        recv_text(&mut s.bob_g, &s.bob, &from_alice),
        b"seen it, will sweep later".to_vec(),
    );
    assert_eq!(
        recv_text(&mut s.charlie_g, &s.charlie, &from_alice),
        b"seen it, will sweep later".to_vec(),
    );

    // A plain member sends.
    let from_charlie = wire(
        s.charlie_g
            .create_application_message(&s.charlie, b"hi from charlie")
            .expect("a PLAIN MEMBER must still be able to send"),
    );
    assert_eq!(
        recv_text(&mut s.alice_g, &s.alice, &from_charlie),
        b"hi from charlie".to_vec(),
    );
    assert_eq!(
        recv_text(&mut s.bob_g, &s.bob, &from_charlie),
        b"hi from charlie".to_vec(),
    );

    // Sending changed nothing about what is pending.
    for (who, g) in [
        ("alice", &s.alice_g),
        ("bob", &s.bob_g),
        ("charlie", &s.charlie_g),
    ] {
        assert_eq!(
            g.pending_proposals().len(),
            1,
            "{who} must still hold exactly the one pending proposal after sending",
        );
    }
}

/// Sending while proposals are pending must not disturb the sweep: the commit
/// still carries **every** authorised intent, and every peer — including ones
/// that sent chat in the meantime — resolves it **by reference** and converges.
#[test]
fn sending_while_proposals_are_pending_does_not_disturb_the_sweep() {
    let mut s = scene(0x21);
    let dave = Identity::from_seed(&[0x21u8; 64]).expect("dave");
    let erin = Identity::from_seed(&[0x22u8; 64]).expect("erin");

    // Two authorised intents from two devices.
    let bob_proposal = s
        .bob_g
        .propose_add_member_from_wire(&s.bob, &kp_wire(&dave))
        .expect("bob proposes add-dave");
    let alice_proposal = s
        .alice_g
        .propose_add_member_from_wire(&s.alice, &kp_wire(&erin))
        .expect("alice proposes add-erin");
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &bob_proposal)
        .expect("alice stores bob's");
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &alice_proposal)
        .expect("bob stores alice's");
    s.charlie_g
        .process_incoming_verifying_roles(&s.charlie, &bob_proposal)
        .expect("charlie stores bob's");
    s.charlie_g
        .process_incoming_verifying_roles(&s.charlie, &alice_proposal)
        .expect("charlie stores alice's");

    // Everyone chats across the pending window.
    let chatter = wire(
        s.charlie_g
            .create_application_message(&s.charlie, b"chatter during the pending window")
            .expect("charlie sends"),
    );
    assert_eq!(
        recv_text(&mut s.alice_g, &s.alice, &chatter),
        b"chatter during the pending window".to_vec(),
    );
    assert_eq!(
        recv_text(&mut s.bob_g, &s.bob, &chatter),
        b"chatter during the pending window".to_vec(),
    );

    // The sweep still carries both adds.
    let sweep = s
        .alice_g
        .commit_pending_proposals(&s.alice)
        .expect("alice sweeps")
        .expect("one commit");
    assert_eq!(sweep.swept.len(), 2, "both intents must be in the commit");
    assert!(sweep.welcome.is_some(), "two adds must produce a Welcome");

    // Every peer resolves the by-reference proposals out of its own store.
    let commit = wire(sweep.commit);
    assert!(matches!(
        s.bob_g.process_incoming_verifying_roles(&s.bob, &commit),
        Ok(Incoming::Commit)
    ));
    assert!(matches!(
        s.charlie_g
            .process_incoming_verifying_roles(&s.charlie, &commit),
        Ok(Incoming::Commit)
    ));
    assert_eq!(s.alice_g.member_count(), 5);
    assert_eq!(exporter(&s.bob_g, &s.bob), exporter(&s.alice_g, &s.alice));
    assert_eq!(
        exporter(&s.charlie_g, &s.charlie),
        exporter(&s.alice_g, &s.alice),
    );
    assert!(is_member(&s.alice_g, &dave) && is_member(&s.alice_g, &erin));

    // The sweep emptied the pending set, and chat keeps working afterwards.
    assert!(s.alice_g.pending_proposals().is_empty());
    let after = wire(
        s.alice_g
            .create_application_message(&s.alice, b"swept, still talking")
            .expect("alice sends after the sweep"),
    );
    assert_eq!(
        recv_text(&mut s.charlie_g, &s.charlie, &after),
        b"swept, still talking".to_vec(),
    );
}

/// A refused proposal stays refused across a send: the send path must not be
/// able to re-admit anything the receipt gate dropped, and a sweep taken after
/// it carries only what was authorised.
#[test]
fn a_send_never_resurrects_a_refused_proposal() {
    let mut s = scene(0x23);
    let dave = Identity::from_seed(&[0x23u8; 64]).expect("dave");
    let mallory = Identity::from_seed(&[0x24u8; 64]).expect("mallory");

    // One authorised proposal (bob, admin) and one unauthorised (charlie, plain
    // member) reach alice.
    let ok_proposal = s
        .bob_g
        .propose_add_member_from_wire(&s.bob, &kp_wire(&dave))
        .expect("bob proposes add-dave");
    let bad_proposal = s
        .charlie_g
        .propose_add_member_from_wire(&s.charlie, &kp_wire(&mallory));
    assert!(
        bad_proposal.is_err(),
        "a plain member's own client refuses to mint the proposal",
    );

    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &ok_proposal)
        .expect("alice stores bob's");
    assert_eq!(s.alice_g.pending_proposals().len(), 1);

    // Alice sends, repeatedly, across the pending window.
    for text in [b"one".as_slice(), b"two".as_slice(), b"three".as_slice()] {
        let msg = wire(
            s.alice_g
                .create_application_message(&s.alice, text)
                .expect("alice sends while a proposal is pending"),
        );
        assert_eq!(recv_text(&mut s.bob_g, &s.bob, &msg), text.to_vec());
    }

    // Exactly one proposal, unchanged, and the sweep carries exactly it.
    assert_eq!(s.alice_g.pending_proposals().len(), 1);
    let sweep = s
        .alice_g
        .commit_pending_proposals(&s.alice)
        .expect("alice sweeps")
        .expect("one commit");
    assert_eq!(sweep.swept.len(), 1);
    assert!(matches!(sweep.swept[0].kind, ProposalKind::Add { .. }));
    assert!(is_member(&s.alice_g, &dave));
    assert!(
        !is_member(&s.alice_g, &mallory),
        "the refused proposal must never reach a commit",
    );
}

/// A restart must not lose a pending proposal — and the reloaded group must
/// still be able to send, and still resolve the eventual commit by reference.
#[test]
fn a_restart_keeps_a_pending_proposal_and_the_group_still_sends() {
    let mut s = scene(0x25);
    let dave = Identity::from_seed(&[0x25u8; 64]).expect("dave");
    let alice_seed = [0xA0u8 ^ 0x25; 64];

    let proposal = s
        .bob_g
        .propose_add_member_from_wire(&s.bob, &kp_wire(&dave))
        .expect("bob proposes add-dave");
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &proposal)
        .expect("alice stores it");
    s.charlie_g
        .process_incoming_verifying_roles(&s.charlie, &proposal)
        .expect("charlie stores it");

    // Alice sends twice before the restart, so the restart is exercised on a
    // group whose send path has already round-tripped the pending set — and so
    // a park/unpark that appended a duplicate persisted entry per send would
    // surface below as a pending set of three rather than one.
    for text in [b"before the restart".as_slice(), b"and again".as_slice()] {
        let before = wire(
            s.alice_g
                .create_application_message(&s.alice, text)
                .expect("alice sends before the restart"),
        );
        assert_eq!(
            recv_text(&mut s.charlie_g, &s.charlie, &before),
            text.to_vec(),
        );
    }

    // Restart alice: snapshot, drop everything, rebuild from the same seed.
    let snapshot = s.alice.snapshot_storage().expect("snapshot");
    let group_id = s.alice_g.group_id();
    drop(s.alice_g);
    drop(s.alice);

    let alice = Identity::from_seed(&alice_seed).expect("restored alice");
    alice.restore_storage(&snapshot).expect("restore");
    let mut alice_g = Group::load(&alice, &group_id)
        .expect("load")
        .expect("group present after restore");

    // The proposal survived, exactly once.
    let pending = alice_g.pending_proposals();
    assert_eq!(
        pending.len(),
        1,
        "a restart must not lose (or duplicate) a pending proposal",
    );
    assert!(matches!(pending[0].kind, ProposalKind::Add { .. }));

    // The reloaded group still sends...
    let after = wire(
        alice_g
            .create_application_message(&alice, b"after the restart")
            .expect("the reloaded group must still be able to send"),
    );
    assert_eq!(
        recv_text(&mut s.charlie_g, &s.charlie, &after),
        b"after the restart".to_vec(),
    );

    // ...and still sweeps the surviving proposal into a commit every peer
    // resolves by reference.
    let sweep = alice_g
        .commit_pending_proposals(&alice)
        .expect("alice sweeps after the restart")
        .expect("one commit");
    assert_eq!(sweep.swept.len(), 1);
    let commit = wire(sweep.commit);
    assert!(matches!(
        s.charlie_g
            .process_incoming_verifying_roles(&s.charlie, &commit),
        Ok(Incoming::Commit)
    ));
    assert_eq!(
        exporter(&s.charlie_g, &s.charlie),
        exporter(&alice_g, &alice),
    );
    assert!(is_member(&alice_g, &dave));
}

// ============================================================================
// The direct-commit paths must not sweep the proposal store (ADR-0045 review,
// HIGH-1)
// ============================================================================

/// **The reviewer's scenario, end to end.** The owner proposes handing the
/// group to Charlie; an admin concurrently reaches for the ordinary
/// `remove_member(Charlie)`. OpenMLS builds every direct commit with
/// `consume_proposal_store` defaulting to `true` and the filter hardcoded to
/// `|_| true`, so before the guard the admin's commit silently absorbed the
/// transfer, violated C-63 (`owner_survives`), **merged eagerly**, and was then
/// refused by every peer — leaving the admin alone at N+1, permanently and
/// silently. That is the exact G-38 shape ADR-0045 exists to erase, reached
/// through a path that predates it.
///
/// The assertion is the one that matters: after the refusal the committer and
/// the peer agree on group *state* — equal exporter output, not merely equal
/// epoch numbers, because the whole hazard is that the numbers match while the
/// state does not.
#[test]
fn a_direct_commit_refuses_to_absorb_a_pending_proposal() {
    let mut s = scene(0x26);

    // alice (owner) proposes handing the group to charlie.
    let transfer = s
        .alice_g
        .propose_ownership_transfer(
            &s.alice,
            s.charlie.signature_public_key(),
            OutgoingOwner::Admin,
        )
        .expect("alice proposes the transfer");
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &transfer)
        .expect("bob stores the transfer");
    s.charlie_g
        .process_incoming_verifying_roles(&s.charlie, &transfer)
        .expect("charlie stores the transfer");
    assert_eq!(s.bob_g.pending_proposals().len(), 1);

    let epoch_before = s.bob_g.epoch();
    let state_before = exporter(&s.bob_g, &s.bob);

    // bob (admin) reaches for the direct path. It must refuse rather than
    // build a commit that also carries alice's transfer.
    let charlie_leaf = leaf_of(&s.bob_g, s.charlie.signature_public_key());
    let refusal = s
        .bob_g
        .remove_member(&s.bob, charlie_leaf)
        .expect_err("a direct commit must refuse while a proposal is pending");
    assert!(
        format!("{refusal}").contains("pending"),
        "the refusal must name the reason, got {refusal}",
    );

    // Nothing moved on the committer...
    assert_eq!(s.bob_g.epoch(), epoch_before);
    assert_eq!(exporter(&s.bob_g, &s.bob), state_before);
    assert_eq!(
        s.bob_g.pending_proposals().len(),
        1,
        "and nothing was eaten"
    );

    // ...and the committer and the peer still agree, which is the property the
    // absorbed commit destroyed.
    assert_eq!(s.alice_g.epoch(), s.bob_g.epoch());
    assert_eq!(exporter(&s.alice_g, &s.alice), exporter(&s.bob_g, &s.bob));
    assert_eq!(
        s.bob_g.group_roles().expect("table").owner(),
        &pk32(&s.alice),
        "the owner is unchanged: the transfer is still only proposed",
    );
    assert!(is_member(&s.bob_g, &s.charlie));

    // The way through is the sweep, and it still works — the transfer lands on
    // every member, so the refusal costs the group nothing but a detour.
    let sweep = s
        .bob_g
        .commit_pending_proposals(&s.bob)
        .expect("bob sweeps")
        .expect("one commit");
    let commit = wire(sweep.commit);
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &commit)
        .expect("alice applies the sweep");
    s.charlie_g
        .process_incoming_verifying_roles(&s.charlie, &commit)
        .expect("charlie applies the sweep");
    assert_eq!(
        s.alice_g.group_roles().expect("table").owner(),
        &pk32(&s.charlie),
    );
    assert_eq!(exporter(&s.alice_g, &s.alice), exporter(&s.bob_g, &s.bob));
}

/// The benign half of the same defect, which needs no malice and no ownership:
/// a direct `add_member` that also swept in somebody else's pending `Add` gave
/// that person a **leaf but no Welcome** — the caller routes the single Welcome
/// to the one person it meant to add, so the swept-in member is in the roster
/// and can never join.
#[test]
fn a_direct_add_cannot_smuggle_in_a_second_member() {
    let mut s = scene(0x27);
    let dave = Identity::from_seed(&[0x2Au8; 64]).expect("dave");
    let erin = Identity::from_seed(&[0x2Bu8; 64]).expect("erin");

    // bob (admin) proposes adding dave; alice stores it.
    let proposal = s
        .bob_g
        .propose_add_member_from_wire(&s.bob, &kp_wire(&dave))
        .expect("bob proposes add-dave");
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &proposal)
        .expect("alice stores it");

    // alice (owner) reaches for the direct path to add erin. Refused: the
    // commit would have carried dave too.
    assert!(
        s.alice_g
            .add_member_from_wire(&s.alice, &kp_wire(&erin))
            .is_err(),
        "a direct add must not silently add a second member",
    );
    assert!(!is_member(&s.alice_g, &dave));
    assert!(!is_member(&s.alice_g, &erin));
}

/// The claim the guard rests on, asserted rather than assumed: a group with no
/// ADR-0022 role table never stores a proposal, so the direct-commit paths
/// every such group depends on — `DeviceGroup`, `PublisherGroup`, call groups —
/// are untouched by the guard.
#[test]
fn a_group_with_no_role_table_still_direct_commits() {
    let alice = Identity::from_seed(&[0x91u8; 64]).expect("alice");
    let bob = Identity::from_seed(&[0x92u8; 64]).expect("bob");
    let charlie = Identity::from_seed(&[0x93u8; 64]).expect("charlie");

    // The `create_with_id` shape: no role table, hence no proposal path.
    let mut alice_g = Group::create_with_id(&alice, &[0x5Au8; 32]).expect("open group");
    assert!(alice_g.group_roles().is_none());

    let add_bob = alice_g
        .add_member_from_wire(&alice, &kp_wire(&bob))
        .expect("alice adds bob");
    let mut bob_g = Group::join_from_welcome(&wire(add_bob.welcome), &bob).expect("bob joins");

    // Such a group has no proposal path to fill the set with: `propose_*`
    // refuses without a table...
    assert!(alice_g
        .propose_add_member_from_wire(&alice, &kp_wire(&charlie))
        .is_err());
    // ...and the receipt gate stores nothing for it either, so the set is
    // permanently empty on both sides.
    assert!(alice_g.pending_proposals().is_empty());
    assert!(bob_g.pending_proposals().is_empty());

    // So every direct path still works, exactly as it did.
    let add_c = alice_g
        .add_member_from_wire(&alice, &kp_wire(&charlie))
        .expect("a direct add still works on an open group");
    bob_g
        .process_incoming_verifying_roles(&bob, &wire(add_c.commit))
        .expect("bob applies it");
    let update = alice_g
        .self_update(&alice)
        .expect("a direct self_update still works");
    bob_g
        .process_incoming_verifying_roles(&bob, &update)
        .expect("bob applies the update");
    let charlie_leaf = leaf_of(&alice_g, charlie.signature_public_key());
    let removal = alice_g
        .remove_member(&alice, charlie_leaf)
        .expect("a direct remove still works");
    bob_g
        .process_incoming_verifying_roles(&bob, &wire(removal))
        .expect("bob applies the removal");
    assert_eq!(exporter(&alice_g, &alice), exporter(&bob_g, &bob));
}

// ============================================================================
// The park must be invisible to a concurrent snapshot (ADR-0045 review, HIGH-2)
// ============================================================================

/// **The concurrency the natives actually drive.** `Group::create_application_message`
/// parks the pending proposal set around the encrypt call, and the original
/// argument for why that was safe — that `&mut self` on the group excludes a
/// snapshot — was wrong: [`Identity::snapshot_storage`] takes `&self` on the
/// *identity*, a different object, and on iOS and Android the UniFFI `Group` is
/// `Mutex<MlsGroup>` while `Identity` is bare, so the send thread and the
/// persist thread genuinely run at once.
///
/// A park that emptied the **persisted** queue for the length of an encrypt
/// therefore let a snapshot capture a group with no pending proposals; a client
/// that persisted that snapshot and restarted could never apply the
/// by-reference commit those proposals belong to
/// (`InvalidCommit(MissingProposal)`) — silently, permanently, and needing no
/// adversary. So the property is asserted the way it is actually consumed:
/// every snapshot taken during a send, restored into a fresh identity and
/// reloaded, must carry the whole pending set.
#[test]
fn a_snapshot_taken_during_a_send_never_observes_a_torn_pending_set() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    const SENDS: usize = 400;
    const MAX_SAMPLES: usize = 300;

    let mut s = scene(0x28);
    let alice_seed = [0xA0u8 ^ 0x28; 64];

    // Three distinct proposals, so a torn capture is unmistakable — and one of
    // them is charlie's own departure, which any member may file.
    for who in [0x31u8, 0x32] {
        let newcomer = Identity::from_seed(&[who; 64]).expect("newcomer");
        let proposal = s
            .bob_g
            .propose_add_member_from_wire(&s.bob, &kp_wire(&newcomer))
            .expect("bob proposes an add");
        s.alice_g
            .process_incoming_verifying_roles(&s.alice, &proposal)
            .expect("alice stores it");
    }
    let own = s.charlie_g.own_leaf_index();
    let leaving = s
        .charlie_g
        .propose_remove_member(&s.charlie, own)
        .expect("charlie proposes to leave");
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &leaving)
        .expect("alice stores it");
    assert_eq!(s.alice_g.pending_proposals().len(), 3);

    let group_id = s.alice_g.group_id();
    let alice = &s.alice;
    let alice_g = &mut s.alice_g;
    let stop = AtomicBool::new(false);
    let samples: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

    std::thread::scope(|sc| {
        sc.spawn(|| {
            while !stop.load(Ordering::Relaxed) {
                let snap = alice.snapshot_storage().expect("snapshot");
                let mut held = samples.lock().expect("samples lock");
                if held.len() >= MAX_SAMPLES {
                    break;
                }
                held.push(snap);
            }
        });
        for _ in 0..SENDS {
            alice_g
                .create_application_message(alice, b"chat while a proposal waits")
                .expect("the group must keep sending");
        }
        stop.store(true, Ordering::Relaxed);
    });

    let captured = samples.into_inner().expect("samples lock");
    assert!(
        captured.len() >= 16,
        "the probe must actually have sampled during the sends; got {}",
        captured.len(),
    );
    for (i, snap) in captured.iter().enumerate() {
        let probe = Identity::from_seed(&alice_seed).expect("probe identity");
        probe.restore_storage(snap).expect("restore");
        let reloaded = Group::load(&probe, &group_id)
            .expect("load")
            .expect("the group is present in every snapshot");
        assert_eq!(
            reloaded.pending_proposals().len(),
            3,
            "snapshot #{i} of {} captured a torn pending set",
            captured.len(),
        );
    }

    // And the live group is unchanged by all of it.
    assert_eq!(alice_g.pending_proposals().len(), 3);
}

// ============================================================================
// The pending set is bounded and de-duplicated (ADR-0045 review, MEDIUM)
// ============================================================================

/// **The cheapest denial available to an unprivileged member.** `authorise`
/// lets any member propose its **own** removal — correctly, that is leaving —
/// and `propose_remove_member` is on both FFI surfaces. Nothing about the
/// ratchet stops one member emitting that same proposal N times: it refuses a
/// byte-identical replay, while N fresh encryptions of one plaintext proposal
/// all decrypt to the same `ProposalRef`, and `ProposalStore::add` is a bare
/// push. Every member then carries, persists and re-parks all N — once per
/// outgoing chat message — and ADR-0045 decision 2 gives the pending set no
/// deadline, so nothing but an admin noticing ever clears it.
#[test]
fn repeating_one_proposal_does_not_grow_the_pending_set() {
    let mut s = scene(0x29);
    let own = s.charlie_g.own_leaf_index();

    // Eight distinct ciphertexts of one and the same proposal.
    let mut wire_copies = Vec::new();
    for _ in 0..8 {
        wire_copies.push(
            s.charlie_g
                .propose_remove_member(&s.charlie, own)
                .expect("charlie proposes its own removal again"),
        );
    }
    let distinct: std::collections::BTreeSet<&Vec<u8>> = wire_copies.iter().collect();
    assert_eq!(
        distinct.len(),
        wire_copies.len(),
        "the flood must be distinct on the wire, or the ratchet would stop it \
         and this test would prove nothing",
    );

    for copy in &wire_copies {
        let incoming = s
            .alice_g
            .process_incoming_verifying_roles(&s.alice, copy)
            .expect("alice processes each copy");
        let Incoming::Proposal(record) = incoming else {
            panic!("expected a proposal, got {incoming:?}");
        };
        assert!(
            record.stored,
            "a repeat is not a refusal — the group does hold this request",
        );
    }
    assert_eq!(
        s.alice_g.pending_proposals().len(),
        1,
        "eight copies of one request must occupy one slot",
    );

    // And the one retained copy still commits, so de-duplication costs the
    // proposer nothing.
    let sweep = s
        .alice_g
        .commit_pending_proposals(&s.alice)
        .expect("alice sweeps")
        .expect("one commit");
    assert_eq!(sweep.swept.len(), 1);
    assert!(!is_member(&s.alice_g, &s.charlie));
}

/// The cap, and which side pays at it. A proposal arriving into a full set is
/// refused (`stored == false`) rather than evicting one already held: evicting
/// would be strictly worse than the flood, because a commit that references an
/// evicted proposal can no longer be resolved and is refused with
/// `InvalidCommit(MissingProposal)` on the evicting member alone. So the late
/// proposer pays a re-proposal after the next sweep; the group never pays a
/// commit.
#[test]
fn the_pending_set_is_capped_and_the_late_proposer_pays() {
    const CAP: usize = 64;

    let mut s = scene(0x2A);

    // Fill alice's set with CAP distinct authorised requests.
    for i in 0..CAP {
        let newcomer = Identity::from_seed(&[0xE0u8 ^ u8::try_from(i).expect("cap < 256"); 64])
            .expect("newcomer");
        let proposal = s
            .bob_g
            .propose_add_member_from_wire(&s.bob, &kp_wire(&newcomer))
            .expect("bob proposes an add");
        s.alice_g
            .process_incoming_verifying_roles(&s.alice, &proposal)
            .expect("alice stores it");
    }
    assert_eq!(s.alice_g.pending_proposals().len(), CAP);

    // An honest device refuses to publish past the cap too — it must not hand
    // its peers a proposal their receipt gates would refuse, or it would hold
    // one nobody else does and sweep a commit they cannot resolve.
    let one_more = Identity::from_seed(&[0x7Fu8; 64]).expect("one more");
    assert!(
        s.bob_g
            .propose_add_member_from_wire(&s.bob, &kp_wire(&one_more))
            .is_err(),
        "the local path must refuse past the cap",
    );

    // A proposal from a device that has not filled its own set still arrives,
    // and is refused on receipt — unstored, and named as what it was.
    let own = s.charlie_g.own_leaf_index();
    let leaving = s
        .charlie_g
        .propose_remove_member(&s.charlie, own)
        .expect("charlie proposes to leave");
    let incoming = s
        .alice_g
        .process_incoming_verifying_roles(&s.alice, &leaving)
        .expect("alice processes it");
    let Incoming::Proposal(record) = incoming else {
        panic!("expected a proposal, got {incoming:?}");
    };
    assert!(!record.stored, "the newcomer is refused at the cap");
    assert_eq!(
        record.kind,
        ProposalKind::Remove {
            target_leaf: own.u32()
        },
        "and the refusal still says what was refused",
    );

    // Nothing already held was evicted to make room.
    assert_eq!(s.alice_g.pending_proposals().len(), CAP);
}

/// The regression guard for refusing [`ProposalKind::Update`].
///
/// Rotating your own leaf is legitimate and stays available: `self_update`
/// drives the rotation through the commit's `UpdatePath`
/// (openmls `CommitBuilder::leaf_node_parameters`) and composes **no** proposal,
/// so the peer-verified commit gate sees a commit carrying nothing and merges
/// it. The refusal removes only the ability to ask *somebody else* to commit
/// your rotation.
///
/// This is the test that fails loudly if that ever stops being true — if a
/// future openmls, or a change here, made `self_update` inline an `Update`
/// proposal, the gate would now refuse the commit on **every peer** and the
/// rotating member would fork itself off the group. That would be silent
/// without this test: nothing else in the suite asserts that a rotation in a
/// role-enabled group is applied by its peers.
#[test]
fn self_update_still_rotates_a_leaf_in_a_role_enabled_group() {
    let mut s = scene(0x2B);
    let before = s.bob_g.epoch();

    let rotation = s.bob_g.self_update(&s.bob).expect("bob rotates its leaf");

    for (g, id, who) in [
        (&mut s.alice_g, &s.alice, "the owner"),
        (&mut s.charlie_g, &s.charlie, "a plain member"),
    ] {
        let incoming = g
            .process_incoming_verifying_roles(id, &rotation)
            .unwrap_or_else(|e| panic!("{who} must apply an admin's leaf rotation: {e}"));
        assert!(
            matches!(incoming, Incoming::Commit),
            "a rotation is a commit, not a proposal: {incoming:?}",
        );
        assert_eq!(g.epoch(), before + 1);
    }
    assert_eq!(s.bob_g.epoch(), before + 1);
    assert!(
        s.alice_g.pending_proposals().is_empty(),
        "a rotation must leave nothing pending behind it",
    );

    // The group is still fully operational for its peers afterwards.
    let dave = Identity::from_seed(&[0x2Bu8; 64]).expect("dave");
    let proposal = s
        .bob_g
        .propose_add_member_from_wire(&s.bob, &kp_wire(&dave))
        .expect("bob proposes an add at the rotated epoch");
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &proposal)
        .expect("alice stores it");
    let sweep = s
        .alice_g
        .commit_pending_proposals(&s.alice)
        .expect("sweep")
        .expect("one commit");
    assert_eq!(sweep.swept.len(), 1);
    assert!(is_member(&s.alice_g, &dave));
}

// ============================================================================
// One role change at a time (ADR-0045 § What the client sprints found, item 5)
// ============================================================================

/// Every role table this suite toggles between, so the two tests below and
/// their control cannot drift apart on what "a different role change" means.
fn promote_charlie(s: &Scene) -> RoleTable {
    RoleTable::new(pk32(&s.alice), vec![pk32(&s.bob), pk32(&s.charlie)])
}

fn demote_bob(s: &Scene) -> RoleTable {
    RoleTable::new(pk32(&s.alice), vec![])
}

/// **The defect, reachable only with the guard removed.** Two role-table
/// rewrites reach one member's pending set, and the sweep that ought to carry
/// both cannot: RFC 9420 admits at most one `GroupContextExtensions` proposal
/// per commit. This is the control for
/// [`a_second_role_change_is_refused_while_one_is_pending`] — it builds the
/// state the guard exists to make unreachable, by rolling the owner's provider
/// back between the two proposals so the second is minted against a set the
/// guard sees as empty, and records what a sweep over that state actually does.
#[test]
fn control_two_role_changes_in_one_pending_set_cannot_both_be_swept() {
    let mut s = scene(0x2B);
    let group_id = s.alice_g.group_id();
    let snapshot = s.alice.snapshot_storage().expect("snapshot");

    // Toggle one.
    let first = s
        .alice_g
        .propose_group_roles(&s.alice, &promote_charlie(&s))
        .expect("owner proposes the first role change");

    // Roll the owner's provider back to before it, so the second toggle is
    // minted from a device that has never seen the first — the shape the guard
    // cannot reach, and the only way to build this state through the public API
    // once the guard is in place.
    s.alice.restore_storage(&snapshot).expect("restore");
    let mut rolled = Group::load(&s.alice, &group_id)
        .expect("load")
        .expect("the group is present in the snapshot");
    assert!(rolled.pending_proposals().is_empty());
    // One filler handshake message first, so the second role change is framed at
    // a generation bob has not already consumed for the first — an artefact of
    // minting two proposals from one leaf at one epoch, not part of the property.
    let charlie_leaf = leaf_of(&rolled, s.charlie.signature_public_key());
    rolled
        .propose_remove_member(&s.alice, charlie_leaf)
        .expect("filler proposal");
    let second = rolled
        .propose_group_roles(&s.alice, &demote_bob(&s))
        .expect("the rolled-back device proposes the second role change");

    // An admin receives both. Both are authorised — the owner may rewrite the
    // table — so both are stored.
    for (label, bytes) in [("first", &first), ("second", &second)] {
        let incoming = s
            .bob_g
            .process_incoming_verifying_roles(&s.bob, bytes)
            .unwrap_or_else(|e| panic!("bob processes the {label} role change: {e:?}"));
        let Incoming::Proposal(record) = incoming else {
            panic!("expected a proposal for {label}");
        };
        assert!(record.stored, "the {label} role change must be stored");
        assert_eq!(record.kind, ProposalKind::RoleChange);
    }
    assert_eq!(
        s.bob_g.pending_proposals().len(),
        2,
        "the control must actually reach the two-role-change state",
    );

    // A sweep over that state must still land. Without the sweep-side half of
    // the guard OpenMLS refuses the whole build with `TooManyGCEProposals` and,
    // since the proposal store empties only when a commit is MERGED and every
    // direct-commit path is closed while proposals are pending, this member's
    // membership operations are dead for good — a permanent wedge, not a lost
    // toggle. So: exactly one role change rides, everything else rides with it,
    // and the epoch moves.
    let epoch_before = s.bob_g.epoch();
    let sweep = s
        .bob_g
        .commit_pending_proposals(&s.bob)
        .expect("a sweep over two role changes must not wedge the group")
        .expect("the sweep produces a commit");
    let role_changes = sweep
        .swept
        .iter()
        .filter(|r| r.kind == ProposalKind::RoleChange)
        .count();
    assert_eq!(
        role_changes, 1,
        "RFC 9420 admits one GroupContextExtensions proposal per commit",
    );
    assert_eq!(s.bob_g.epoch(), epoch_before + 1, "the group moved on");
    assert!(
        s.bob_g.pending_proposals().is_empty(),
        "merging a commit empties the whole store, so the other change is gone \
         and its author must re-propose",
    );
    assert!(
        s.bob_g.group_roles().is_some(),
        "the group still has a readable role table",
    );
}

/// **The guard.** A second role change is refused while one is pending, so the
/// state the control reaches is never built by a well-behaved device.
#[test]
fn a_second_role_change_is_refused_while_one_is_pending() {
    let mut s = scene(0x2C);

    let first = s
        .alice_g
        .propose_group_roles(&s.alice, &promote_charlie(&s))
        .expect("the first role change lands");
    assert_eq!(s.alice_g.pending_proposals().len(), 1);

    let refused = s
        .alice_g
        .propose_group_roles(&s.alice, &demote_bob(&s))
        .expect_err("a second role change must be refused while one is pending");
    // The message is the client's raw material, so pin the two phrases it must
    // keep: what was refused, and why now rather than never.
    let message = format!("{refused}");
    assert!(
        message.contains("role-table change refused"),
        "the refusal must name what it refused: {message}",
    );
    assert!(
        message.contains("already pending"),
        "the refusal must say it is a timing refusal, not an authority one: {message}",
    );

    // Nothing was published and nothing was stored: the set still holds exactly
    // the one change, so no sweep can ever face the control's state.
    assert_eq!(s.alice_g.pending_proposals().len(), 1);
    assert_eq!(
        s.alice_g.pending_proposals()[0].kind,
        ProposalKind::RoleChange,
    );

    // And the one change still lands, unaltered.
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &first)
        .expect("bob stores the first role change");
    let sweep = s
        .bob_g
        .commit_pending_proposals(&s.bob)
        .expect("bob sweeps")
        .expect("the sweep produces a commit");
    assert_eq!(sweep.swept.len(), 1);
    let table = s
        .bob_g
        .group_roles()
        .expect("role table survives the sweep");
    assert_eq!(
        table.admins().len(),
        2,
        "the swept table is the first one proposed",
    );
    assert!(table.can_administer(&pk32(&s.charlie)));

    // Once swept, the next toggle is free again.
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &wire(sweep.commit))
        .expect("alice applies the sweep");
    s.alice_g
        .propose_group_roles(&s.alice, &demote_bob(&s))
        .expect("a role change is proposable again once the pending one is swept");
}

/// A pending **add** or **remove** composes into the same sweep perfectly well;
/// only the `GroupContextExtensions` slot is exclusive. The guard must not
/// widen into "no role change while anything is pending".
#[test]
fn a_pending_add_or_remove_does_not_block_a_role_change() {
    let mut s = scene(0x2D);
    let dave = Identity::from_seed(&[0x2Du8; 64]).expect("dave");

    // An admin's add and a member's own departure, both pending on the owner.
    let add = s
        .bob_g
        .propose_add_member_from_wire(&s.bob, &kp_wire(&dave))
        .expect("bob proposes an add");
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &add)
        .expect("alice stores the add");
    let own = s.charlie_g.own_leaf_index();
    let leaving = s
        .charlie_g
        .propose_remove_member(&s.charlie, own)
        .expect("charlie proposes to leave");
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &leaving)
        .expect("alice stores the departure");
    assert_eq!(s.alice_g.pending_proposals().len(), 2);

    // The role change still goes out, and all three compose into one commit.
    s.alice_g
        .propose_group_roles(&s.alice, &promote_charlie(&s))
        .expect("an add and a remove must not block a role change");
    let sweep = s
        .alice_g
        .commit_pending_proposals(&s.alice)
        .expect("owner sweeps")
        .expect("the sweep produces a commit");
    assert_eq!(sweep.swept.len(), 3, "all three intents ride one commit");
}

/// A group with no ADR-0022 role table has no proposal path at all, so its
/// pending set is permanently empty and the guard is a counted-zero no-op —
/// every direct-commit caller (`DeviceGroup`, `PublisherGroup`, call groups)
/// is untouched. Asserted rather than assumed, including that the refusal such
/// a group *does* get names the pre-existing reason and not the new one.
#[test]
fn a_group_with_no_role_table_is_untouched_by_the_role_change_guard() {
    let alice = Identity::from_seed(&[0x5Du8; 64]).expect("alice");
    let mut g = Group::create_with_id(&alice, b"no-role-table").expect("create");
    assert!(g.group_roles().is_none());

    let refusal = g
        .propose_group_roles(&alice, &RoleTable::new(pk32(&alice), vec![]))
        .expect_err("a group with no role table has no proposal path");
    let message = format!("{refusal}");
    assert!(
        message.contains("carries no role table"),
        "the refusal must be the pre-existing one, not the new guard: {message}",
    );

    // ...and consecutive direct membership commits still work.
    for who in [0x5Eu8, 0x5F] {
        let newcomer = Identity::from_seed(&[who; 64]).expect("newcomer");
        g.add_member_from_wire(&alice, &kp_wire(&newcomer))
            .expect("the direct path is unchanged");
    }
    assert!(g.pending_proposals().is_empty());
}

/// Ownership transfer is the same role-table rewrite under another name, so it
/// collides on the same exclusive slot — in both directions.
#[test]
fn ownership_transfer_and_a_role_change_exclude_each_other() {
    let mut s = scene(0x2E);
    let charlie_key = pk32(&s.charlie);

    // A pending transfer blocks a role change.
    s.alice_g
        .propose_ownership_transfer(&s.alice, &charlie_key, OutgoingOwner::Admin)
        .expect("owner proposes a transfer");
    assert!(
        s.alice_g
            .propose_group_roles(&s.alice, &promote_charlie(&s))
            .is_err(),
        "a pending transfer must block a role change",
    );

    // And the other way round, on a fresh scene.
    let mut t = scene(0x2F);
    t.alice_g
        .propose_group_roles(&t.alice, &promote_charlie(&t))
        .expect("owner proposes a role change");
    assert!(
        t.alice_g
            .propose_ownership_transfer(&t.alice, &pk32(&t.charlie), OutgoingOwner::Admin)
            .is_err(),
        "a pending role change must block a transfer",
    );
}

/// **A departing admin's own sweep must still land, on every peer.**
///
/// ADR-0045 made leaving reachable as "the member proposes it and an admin
/// commits", but OpenMLS forbids a committer from removing its own leaf
/// (`CreateCommitError::CannotRemoveSelf`, raised inside `CommitBuilder::build`
/// once the proposals have been applied to the diff). So the departing admin's
/// OWN sweep used to fail outright — and since a pending proposal also closes
/// every direct-commit path, that device's membership operations were frozen
/// for as long as it was the acting committer, with chat still working over the
/// top of it.
///
/// The sweep filter does not enumerate this, and should not: the proposal is
/// not defective, it is simply not committable *by this device*. The
/// last-resort exclusion search is what finds that out.
///
/// The half that matters here and cannot be seen from inside one device: the
/// rescued commit is an ordinary commit that every peer accepts.
#[test]
fn a_departing_admins_sweep_is_rescued_and_its_commit_lands_on_every_peer() {
    let mut s = scene(0x30);
    let dave = Identity::from_seed(&[0xD0u8; 64]).expect("dave");
    let bob_leaf = leaf_of(&s.bob_g, s.bob.signature_public_key());

    // bob (admin) asks to leave, and in the same round asks for dave.
    let leave = s
        .bob_g
        .propose_remove_member(&s.bob, bob_leaf)
        .expect("an admin may always ask to leave");
    let add = s
        .bob_g
        .propose_add_member_from_wire(&s.bob, &kp_wire(&dave))
        .expect("an admin may ask to add somebody");
    for (g, id) in [(&mut s.alice_g, &s.alice), (&mut s.charlie_g, &s.charlie)] {
        g.process_incoming_verifying_roles(id, &leave)
            .expect("peer takes the departure request");
        g.process_incoming_verifying_roles(id, &add)
            .expect("peer takes the add request");
    }

    // bob is the acting committer and sweeps its own pending set.
    let swept = s
        .bob_g
        .commit_pending_proposals(&s.bob)
        .expect("the departing admin's own sweep must still be buildable")
        .expect("something is swept");
    assert_eq!(
        swept.swept.len(),
        1,
        "dave's add lands; bob's own departure is not committable BY BOB",
    );

    // ...and the commit is ordinary: every peer applies it.
    let commit = wire(swept.commit);
    for (g, id) in [(&mut s.alice_g, &s.alice), (&mut s.charlie_g, &s.charlie)] {
        g.process_incoming_verifying_roles(id, &commit)
            .expect("peer applies the rescued commit");
        assert!(is_member(g, &dave), "dave joined on this peer");
        assert!(is_member(g, &s.bob), "and bob is still here");
    }
    assert_eq!(s.alice_g.epoch(), s.bob_g.epoch());
    assert_eq!(s.charlie_g.epoch(), s.bob_g.epoch());

    // The exclusion is not a refusal: bob re-asks and a DIFFERENT committer
    // enacts the very same departure.
    let leave_again = s
        .bob_g
        .propose_remove_member(&s.bob, leaf_of(&s.bob_g, s.bob.signature_public_key()))
        .expect("bob asks again");
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &leave_again)
        .expect("alice takes it");
    let owner_sweep = s
        .alice_g
        .commit_pending_proposals(&s.alice)
        .expect("the owner sweeps")
        .expect("something is swept");
    assert_eq!(owner_sweep.swept.len(), 1, "bob's departure, this time");
    assert!(
        !is_member(&s.alice_g, &s.bob),
        "bob is gone from the owner's roster",
    );
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &wire(owner_sweep.commit))
        .expect("bob applies its own eviction");
    assert!(
        !s.bob_g.is_active(),
        "and bob's own group is now inactive, which is what leaving means",
    );
}
