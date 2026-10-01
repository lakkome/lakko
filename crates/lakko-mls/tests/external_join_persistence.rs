// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0027 — the retained external-join queue survives the owner's restart.
//!
//! The v1 queue was a `Vec` on the `Group`; an owner whose app restarted
//! while a `/g/` request was pending forgot it, and the joiner — whose
//! landing has no deadline (ADR-0045 decision 2) — waited for a decision
//! nobody could make any more. The queue now rides the MLS storage snapshot,
//! and these tests pin the property from the public API only: what
//! `snapshot_storage` carries, `Group::load` gives back, and every decision
//! taken after the restart behaves exactly as it would have before it.

use lakko_mls::group::encode_message;
use lakko_mls::{Group, GroupKind, Identity};

const ALICE_SEED: [u8; 64] = [0xA5u8; 64];
const DAVE_SEED: [u8; 64] = [0xD5u8; 64];

fn external_join_proposal(joiner: &Identity, group_id: &[u8], epoch: u64) -> Vec<u8> {
    joiner
        .create_external_join_proposal(group_id, epoch)
        .expect("join proposal")
}

fn pk(id: &Identity) -> Vec<u8> {
    id.signature_public_key().to_vec()
}

/// Retain on one process, snapshot, restore into a fresh `Identity` from the
/// same seed, load — and the request is still there, approvable, sweepable,
/// and the joiner lands from the Welcome the post-restart sweep produced.
#[test]
fn a_pending_request_survives_the_owners_restart_and_is_still_approvable() {
    let alice = Identity::from_seed(&ALICE_SEED).expect("alice");
    let mut alice_g = Group::create(&alice, GroupKind::Group).expect("create");
    let dave = Identity::from_seed(&DAVE_SEED).expect("dave");
    let dave_pk = pk(&dave);
    let group_id = alice_g.group_id();

    let prop = external_join_proposal(&dave, &group_id, alice_g.epoch());
    alice_g.process_incoming(&alice, &prop).expect("process");
    assert_eq!(alice_g.pending_external_joins(), vec![dave_pk.clone()]);

    // ---- restart -----------------------------------------------------
    let snapshot = alice.snapshot_storage().expect("snapshot");
    drop(alice_g);
    drop(alice);
    let alice = Identity::from_seed(&ALICE_SEED).expect("alice again");
    alice.restore_storage(&snapshot).expect("restore");
    let mut alice_g = Group::load(&alice, &group_id)
        .expect("load")
        .expect("group present");

    assert_eq!(
        alice_g.pending_external_joins(),
        vec![dave_pk.clone()],
        "the request must come back from the snapshot",
    );

    // ---- the decision after the restart ------------------------------
    let proposal = alice_g
        .approve_external_join(&alice, &dave_pk)
        .expect("approve after restart");
    assert!(!proposal.is_empty());
    assert!(alice_g.pending_external_joins().is_empty());

    let sweep = alice_g
        .commit_pending_proposals(&alice)
        .expect("sweep")
        .expect("a commit was produced");
    let welcome = sweep.welcome.expect("the sweep carries dave's Welcome");
    let dave_g =
        Group::join_from_welcome(&encode_message(&welcome).expect("encode"), &dave).expect("join");
    assert_eq!(dave_g.group_id(), group_id);
    assert!(
        alice_g
            .members()
            .iter()
            .any(|m| m.signature_public_key == dave_pk),
        "dave is a member after the post-restart sweep",
    );

    // ---- and the settled state survives a second restart -------------
    let snapshot = alice.snapshot_storage().expect("snapshot 2");
    let alice = Identity::from_seed(&ALICE_SEED).expect("alice a third time");
    alice.restore_storage(&snapshot).expect("restore 2");
    let alice_g = Group::load(&alice, &group_id)
        .expect("load 2")
        .expect("group present 2");
    assert!(
        alice_g.pending_external_joins().is_empty(),
        "an approved request must not be resurrected",
    );
}

/// A rejection is a decision too: it must not be undone by a restart, or
/// the admin would be asked the same question again after every relaunch.
#[test]
fn a_rejected_request_stays_rejected_across_a_restart() {
    let alice = Identity::from_seed(&ALICE_SEED).expect("alice");
    let mut alice_g = Group::create(&alice, GroupKind::Group).expect("create");
    let dave = Identity::from_seed(&DAVE_SEED).expect("dave");
    let dave_pk = pk(&dave);
    let group_id = alice_g.group_id();

    let prop = external_join_proposal(&dave, &group_id, alice_g.epoch());
    alice_g.process_incoming(&alice, &prop).expect("process");
    alice_g
        .reject_external_join(&alice, &dave_pk)
        .expect("reject");
    assert!(alice_g.pending_external_joins().is_empty());

    let snapshot = alice.snapshot_storage().expect("snapshot");
    let alice = Identity::from_seed(&ALICE_SEED).expect("alice again");
    alice.restore_storage(&snapshot).expect("restore");
    let alice_g = Group::load(&alice, &group_id)
        .expect("load")
        .expect("group present");
    assert!(
        alice_g.pending_external_joins().is_empty(),
        "a rejected request must not come back",
    );
    assert!(
        !alice_g
            .members()
            .iter()
            .any(|m| m.signature_public_key == dave_pk),
        "and dave was never added",
    );
}

/// Two requests retained across a restart keep their order and both handles
/// stay valid, so the admin's pending list is the one they saw before.
#[test]
fn several_pending_requests_survive_in_order() {
    let alice = Identity::from_seed(&ALICE_SEED).expect("alice");
    let mut alice_g = Group::create(&alice, GroupKind::Group).expect("create");
    let group_id = alice_g.group_id();
    let joiners: Vec<Identity> = (0u8..3)
        .map(|i| Identity::from_seed(&[0xE0 + i; 64]).expect("joiner"))
        .collect();
    for j in &joiners {
        let prop = external_join_proposal(j, &group_id, alice_g.epoch());
        alice_g.process_incoming(&alice, &prop).expect("process");
    }
    let before = alice_g.pending_external_joins();
    assert_eq!(before.len(), 3);

    let snapshot = alice.snapshot_storage().expect("snapshot");
    let alice = Identity::from_seed(&ALICE_SEED).expect("alice again");
    alice.restore_storage(&snapshot).expect("restore");
    let mut alice_g = Group::load(&alice, &group_id)
        .expect("load")
        .expect("group present");
    assert_eq!(alice_g.pending_external_joins(), before);

    // The middle one is rejected after the restart; the other two stay.
    alice_g
        .reject_external_join(&alice, &before[1])
        .expect("reject");
    assert_eq!(
        alice_g.pending_external_joins(),
        vec![before[0].clone(), before[2].clone()]
    );
}
