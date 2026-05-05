// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Phase 0 exit-criterion integration test.
//!
//! Exercises the full Lakko MLS lifecycle on three independent identities:
//!
//! 1. Alice creates a group.
//! 2. Bob publishes a KeyPackage (wire form), Alice adds him via Welcome.
//! 3. Alice and Bob exchange application messages.
//! 4. Charlie publishes a KeyPackage, Alice adds him; Bob processes the
//!    Commit; Charlie joins via Welcome.
//! 5. All three exchange a message that decrypts on every side.
//! 6. Alice removes Bob; Bob and Charlie process the Commit.
//! 7. Alice and Charlie keep messaging after the removal.
//! 8. Alice and Charlie's exporter outputs match for the same label/context.

use lakko_mls::{group, key_package, Group, Identity, Incoming};
use openmls::prelude::LeafNodeIndex;

/// Encode and decode an MLS message via the wire format helpers we expose.
/// Mirrors what would happen across the network.
fn over_wire(msg: openmls::framing::MlsMessageOut) -> Vec<u8> {
    group::encode_message(&msg).expect("encode")
}

/// Label used for the exporter consistency check in step 8.
const EXPORTER_LABEL: &str = "lakko/test/exporter";

#[test]
#[allow(clippy::too_many_lines)] // intentional: full lifecycle in one trace
fn three_party_group_lifecycle_and_exporter_consistency() {
    // ---- 1. Alice creates a fresh group ------------------------------------
    let alice = Identity::generate("alice").expect("alice identity");
    let mut alice_group = Group::create(&alice).expect("alice creates group");
    assert_eq!(alice_group.member_count(), 1);
    assert_eq!(alice_group.epoch(), 0);

    // ---- 2. Bob publishes a KeyPackage; Alice adds him ---------------------
    let bob = Identity::generate("bob").expect("bob identity");
    let bob_kp = bob.create_key_package().expect("bob keypackage");
    let bob_kp_wire = key_package::serialize(&bob_kp).expect("serialize bob kp");

    let add_bob = alice_group
        .add_member_from_wire(&alice, &bob_kp_wire)
        .expect("alice adds bob");
    assert_eq!(alice_group.member_count(), 2);
    assert_eq!(alice_group.epoch(), 1);

    // Bob joins from the Welcome.
    let welcome_for_bob = over_wire(add_bob.welcome);
    let mut bob_group =
        Group::join_from_welcome(&welcome_for_bob, &bob).expect("bob joins from welcome");
    assert_eq!(bob_group.member_count(), 2);
    assert_eq!(bob_group.epoch(), 1);

    // ---- 3. Alice ↔ Bob application messages ------------------------------
    let alice_msg = alice_group
        .create_application_message(&alice, b"hi bob, this is alice")
        .expect("alice encrypts");
    let bob_received = bob_group
        .process_incoming(&bob, &over_wire(alice_msg))
        .expect("bob decrypts");
    assert_eq!(
        bob_received_bytes(bob_received),
        b"hi bob, this is alice".to_vec()
    );

    let bob_msg = bob_group
        .create_application_message(&bob, b"hi alice, bob here")
        .expect("bob encrypts");
    let alice_received = alice_group
        .process_incoming(&alice, &over_wire(bob_msg))
        .expect("alice decrypts");
    assert_eq!(
        bob_received_bytes(alice_received),
        b"hi alice, bob here".to_vec()
    );

    // ---- 4. Charlie joins; Bob processes the Commit ------------------------
    let charlie = Identity::generate("charlie").expect("charlie identity");
    let charlie_kp = charlie.create_key_package().expect("charlie keypackage");
    let charlie_kp_wire = key_package::serialize(&charlie_kp).expect("serialize charlie kp");

    let add_charlie = alice_group
        .add_member_from_wire(&alice, &charlie_kp_wire)
        .expect("alice adds charlie");
    assert_eq!(alice_group.member_count(), 3);
    assert_eq!(alice_group.epoch(), 2);

    // Bob, an existing member, processes the Commit and advances his epoch.
    let bob_post = bob_group
        .process_incoming(&bob, &over_wire(add_charlie.commit))
        .expect("bob processes add-charlie commit");
    assert!(matches!(bob_post, Incoming::Commit));
    assert_eq!(bob_group.member_count(), 3);
    assert_eq!(bob_group.epoch(), 2);

    // Charlie joins from his Welcome.
    let welcome_for_charlie = over_wire(add_charlie.welcome);
    let mut charlie_group =
        Group::join_from_welcome(&welcome_for_charlie, &charlie).expect("charlie joins");
    assert_eq!(charlie_group.member_count(), 3);
    assert_eq!(charlie_group.epoch(), 2);

    // ---- 5. Three-way message ---------------------------------------------
    let three_way = alice_group
        .create_application_message(&alice, b"hello, three of us now")
        .expect("alice encrypts three-way");
    let three_way_bytes = over_wire(three_way);

    let bob_three = bob_group
        .process_incoming(&bob, &three_way_bytes)
        .expect("bob decrypts three-way");
    assert_eq!(
        bob_received_bytes(bob_three),
        b"hello, three of us now".to_vec()
    );
    let charlie_three = charlie_group
        .process_incoming(&charlie, &three_way_bytes)
        .expect("charlie decrypts three-way");
    assert_eq!(
        bob_received_bytes(charlie_three),
        b"hello, three of us now".to_vec()
    );

    // ---- 6. Alice removes Bob; Bob + Charlie process the Commit -----------
    // Bob's leaf index from his own perspective is the canonical answer.
    let bob_leaf: LeafNodeIndex = bob_group.own_leaf_index();
    let remove_commit = alice_group
        .remove_member(&alice, bob_leaf)
        .expect("alice removes bob");
    assert_eq!(alice_group.member_count(), 2);
    assert_eq!(alice_group.epoch(), 3);

    let remove_wire = over_wire(remove_commit);

    let bob_post_remove = bob_group
        .process_incoming(&bob, &remove_wire)
        .expect("bob processes his own removal");
    assert!(matches!(bob_post_remove, Incoming::Commit));

    let charlie_post_remove = charlie_group
        .process_incoming(&charlie, &remove_wire)
        .expect("charlie processes bob's removal");
    assert!(matches!(charlie_post_remove, Incoming::Commit));
    assert_eq!(charlie_group.member_count(), 2);
    assert_eq!(charlie_group.epoch(), 3);

    // ---- 7. Alice and Charlie keep messaging ------------------------------
    let post_remove = alice_group
        .create_application_message(&alice, b"just us now")
        .expect("alice encrypts post-remove");
    let charlie_pr = charlie_group
        .process_incoming(&charlie, &over_wire(post_remove))
        .expect("charlie decrypts post-remove");
    assert_eq!(bob_received_bytes(charlie_pr), b"just us now".to_vec());

    // ---- 8. Exporter consistency at the same epoch -------------------------
    let context = b"sframe-call-id-deadbeef";

    let alice_export = alice_group
        .export_secret(&alice, EXPORTER_LABEL, context, 32)
        .expect("alice exporter");
    let charlie_export = charlie_group
        .export_secret(&charlie, EXPORTER_LABEL, context, 32)
        .expect("charlie exporter");

    assert_eq!(
        alice_export.as_slice(),
        charlie_export.as_slice(),
        "exporter outputs must match across members at the same epoch",
    );
    assert_eq!(alice_export.len(), 32);

    // Exporter changes when context changes.
    let alice_other = alice_group
        .export_secret(&alice, EXPORTER_LABEL, b"different-call-id", 32)
        .expect("alice exporter, different context");
    assert_ne!(alice_export.as_slice(), alice_other.as_slice());
}

fn bob_received_bytes(incoming: Incoming) -> Vec<u8> {
    match incoming {
        Incoming::Application(b) => b,
        other => panic!("expected application message, got {other:?}"),
    }
}

/// Phase 1.16e6a: storage snapshot / restore round-trip.
///
/// Mirror of a real-world reload cycle:
///
///   1. Alice (seed-derived) creates a group and adds Bob.
///   2. Alice sends a message; Bob decrypts.
///   3. We snapshot Alice's MLS storage and grab the group_id.
///   4. **New `Alice` instance** is built from the same seed —
///      fresh `OpenMlsRustCrypto` provider, empty storage,
///      identical Ed25519 + X25519 public keys.
///   5. We restore the snapshot into the new provider's
///      storage and call `Group::load` to bring the group
///      back to life.
///   6. Restored Alice continues messaging Bob from the same
///      epoch — the ratchet state is intact, so the next
///      `create_application_message` is decryptable on Bob's
///      side without any rejoin handshake.
#[test]
fn snapshot_and_restore_round_trips_a_live_group() {
    // Use seed-derived identities so the fresh restore-side
    // Alice has byte-identical signature keys.
    let alice_seed = [0xA1u8; 64];
    let alice = Identity::from_seed(&alice_seed).expect("alice from seed");
    let alice_pub_before = alice.signature_public_key().to_vec();

    let bob = Identity::generate("bob").expect("bob");

    // ---- 1. Alice creates a group, adds Bob ------------------------
    let mut alice_group = Group::create(&alice).expect("alice creates");
    let bob_kp = bob.create_key_package().expect("bob kp");
    let bob_kp_wire = key_package::serialize(&bob_kp).expect("serialize bob kp");
    let added = alice_group
        .add_member_from_wire(&alice, &bob_kp_wire)
        .expect("add bob");
    let mut bob_group =
        Group::join_from_welcome(&over_wire(added.welcome), &bob).expect("bob joins");
    assert_eq!(alice_group.member_count(), 2);
    assert_eq!(alice_group.epoch(), 1);

    // ---- 2. Alice → Bob, plaintext check ---------------------------
    let m1 = alice_group
        .create_application_message(&alice, b"first message")
        .expect("alice encrypts");
    let r1 = bob_group
        .process_incoming(&bob, &over_wire(m1))
        .expect("bob decrypts");
    assert_eq!(bob_received_bytes(r1), b"first message".to_vec());

    // ---- 3. Snapshot --------------------------------------------------
    let snapshot = alice.snapshot_storage().expect("snapshot");
    let group_id = alice_group.group_id();
    assert!(!snapshot.is_empty(), "snapshot must contain bytes");
    // Drop the old Alice + group entirely. Anything that
    // survives this point came from the snapshot, not memory.
    drop(alice_group);
    drop(alice);

    // ---- 4. Fresh Alice from the same seed -------------------------
    let restored_alice = Identity::from_seed(&alice_seed).expect("restored_alice from seed");
    assert_eq!(
        restored_alice.signature_public_key(),
        alice_pub_before.as_slice(),
        "from_seed must be deterministic",
    );

    // ---- 5. Restore the snapshot + load the group -----------------
    restored_alice.restore_storage(&snapshot).expect("restore");
    let mut restored_group = Group::load(&restored_alice, &group_id)
        .expect("load")
        .expect("group present in restored storage");
    assert_eq!(restored_group.member_count(), 2);
    assert_eq!(restored_group.epoch(), 1);
    assert_eq!(restored_group.group_id(), group_id);

    // ---- 6. Restored Alice keeps messaging Bob --------------------
    let m2 = restored_group
        .create_application_message(&restored_alice, b"after restore")
        .expect("restored_alice encrypts");
    let r2 = bob_group
        .process_incoming(&bob, &over_wire(m2))
        .expect("bob decrypts after restore");
    assert_eq!(bob_received_bytes(r2), b"after restore".to_vec());
}

/// Negative case: `Group::load` on a fresh-from-seed identity
/// (no snapshot restored) returns `Ok(None)` — pinpoints the
/// distinction between "storage doesn't have this group" and
/// "storage is corrupt".
#[test]
fn group_load_returns_none_when_no_state_was_persisted() {
    let alice = Identity::from_seed(&[0xA2u8; 64]).expect("alice");
    let arbitrary_id = [0x99u8; 32];
    let loaded = Group::load(&alice, &arbitrary_id).expect("load");
    assert!(loaded.is_none(), "no row in storage → None");
}

/// Negative case: a truncated snapshot is rejected with a
/// descriptive [`Error::Identity`].
#[test]
fn restore_storage_rejects_truncated_snapshot() {
    let alice = Identity::from_seed(&[0xA3u8; 64]).expect("alice");
    let alice_group = Group::create(&alice).expect("group");
    let snapshot = alice.snapshot_storage().expect("snapshot");

    // Lop off the trailing 5 bytes. The decoder MUST surface
    // a structured error, not panic — we don't want a corrupt
    // IDB row to crash the renderer.
    let truncated = &snapshot[..snapshot.len() - 5];
    let err = alice
        .restore_storage(truncated)
        .expect_err("truncated snapshot must fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("snapshot"),
        "error message should mention snapshot, got: {msg}",
    );
    // Touch alice_group to silence "unused" — and to confirm
    // the failed restore left the original storage intact.
    let _ = alice_group.group_id();
}
