// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Staged direct commits ([ADR-0081]) — property tests for an invariant-#3
//! change to the MLS state machine.
//!
//! A direct commit (add, remove, role change) used to merge N -> N+1 inside
//! the create call. ADR-0081 sends every new epoch to the Delivery Service
//! together with its commit row, and the server may refuse it (`409`, another
//! commit took the epoch). A commit merged before that answer cannot be taken
//! back without a whole-provider restore, and a commit recorded only after
//! the answer leaves the device below its own commit when it crashes in
//! between. The staged twins (`stage_add_members`, `stage_remove_member`,
//! `stage_set_group_roles`) build the same commit and hold it pending at N,
//! reusing the lifecycle the ADR-0045 sweep already has.
//!
//! What is pinned here:
//!
//! 1. a staged direct commit, once merged, reaches the state the eager path
//!    reaches: the same epoch, the same roster or role table, a commit every
//!    peer applies, a Welcome the added member joins from, and one shared
//!    exporter secret across all of them; and `staged_export_secret` before
//!    the merge equals `export_secret` after it;
//! 2. abandoning leaves the group at N untouched (epoch, roster, role table,
//!    exporter secret, and it can still send to its peers) and leaves a
//!    sibling group of the same identity untouched;
//! 3. a staged commit survives a storage snapshot and a restore into a fresh
//!    identity handle, and can then be merged (or abandoned) there;
//! 4. while one commit is staged, a second stage of any kind, a sweep stage
//!    and an eager direct commit are refused, and the typed refusal is
//!    `SweepAlreadyStaged`.
//!
//! [ADR-0081]: ../../../docs/adr/0081-devicegroup-commits-cannot-strand.md

use lakko_mls::{key_package, Group, GroupKind, Identity, LeafNodeIndex, RefusalReason, RoleTable};
use proptest::prelude::*;

const LABEL: &str = "lakko/test/staged-direct/v1";

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
    LeafNodeIndex::new(
        g.members()
            .iter()
            .find(|m| m.signature_public_key == pubkey)
            .expect("member present")
            .leaf_index,
    )
}

fn roster(g: &Group) -> Vec<Vec<u8>> {
    let mut keys: Vec<Vec<u8>> = g
        .members()
        .into_iter()
        .map(|m| m.signature_public_key)
        .collect();
    keys.sort();
    keys
}

fn secret(g: &Group, id: &Identity) -> Vec<u8> {
    g.export_secret(id, LABEL, b"ctx", 32)
        .expect("export secret")
        .to_vec()
}

/// owner = alice, member = bob and charlie (in a group of kind `Group`, which
/// carries a role table); dave holds a KeyPackage and is not in the group.
struct Scene {
    alice: Identity,
    bob: Identity,
    charlie: Identity,
    dave: Identity,
    alice_g: Group,
    bob_g: Group,
    charlie_g: Group,
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
    let add_c = alice_g
        .add_member_from_wire(&alice, &kp_wire(&charlie))
        .expect("add charlie");
    bob_g
        .process_incoming_verifying_roles(&bob, &wire(add_c.commit))
        .expect("bob applies add charlie");
    let charlie_g =
        Group::join_from_welcome(&wire(add_c.welcome), &charlie).expect("charlie joins");
    Scene {
        alice,
        bob,
        charlie,
        dave,
        alice_g,
        bob_g,
        charlie_g,
    }
}

/// The three direct commits a client still sends to a Delivery Service.
#[derive(Clone, Copy, Debug)]
enum Op {
    AddDave,
    RemoveCharlie,
    PromoteBob,
}

/// What the staged call returned: the commit, and the Welcome of an add.
struct Staged {
    commit: Vec<u8>,
    welcome: Option<Vec<u8>>,
}

fn stage(s: &mut Scene, op: Op) -> Staged {
    match op {
        Op::AddDave => {
            let kp = kp_wire(&s.dave);
            let added = s
                .alice_g
                .stage_add_members_from_wire(&s.alice, &[kp.as_slice()])
                .expect("stage add");
            Staged {
                commit: wire(added.commit),
                welcome: Some(wire(added.welcome)),
            }
        },
        Op::RemoveCharlie => {
            let leaf = leaf_of(&s.alice_g, s.charlie.signature_public_key());
            Staged {
                commit: wire(
                    s.alice_g
                        .stage_remove_member(&s.alice, leaf)
                        .expect("stage remove"),
                ),
                welcome: None,
            }
        },
        Op::PromoteBob => {
            let roles = RoleTable::new(pk32(&s.alice), vec![pk32(&s.bob)]);
            Staged {
                commit: s
                    .alice_g
                    .stage_set_group_roles(&s.alice, &roles)
                    .expect("stage roles"),
                welcome: None,
            }
        },
    }
}

fn eager(s: &mut Scene, op: Op) -> Staged {
    match op {
        Op::AddDave => {
            let added = s
                .alice_g
                .add_member_from_wire(&s.alice, &kp_wire(&s.dave))
                .expect("eager add");
            Staged {
                commit: wire(added.commit),
                welcome: Some(wire(added.welcome)),
            }
        },
        Op::RemoveCharlie => {
            let leaf = leaf_of(&s.alice_g, s.charlie.signature_public_key());
            Staged {
                commit: wire(
                    s.alice_g
                        .remove_member(&s.alice, leaf)
                        .expect("eager remove"),
                ),
                welcome: None,
            }
        },
        Op::PromoteBob => {
            let roles = RoleTable::new(pk32(&s.alice), vec![pk32(&s.bob)]);
            Staged {
                commit: s
                    .alice_g
                    .set_group_roles(&s.alice, &roles)
                    .expect("eager roles"),
                welcome: None,
            }
        },
    }
}

/// The observable state a commit leads to, for comparing the two paths.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Shape {
    epoch: u64,
    roster: Vec<Vec<u8>>,
    roles: Option<RoleTable>,
}

fn shape(g: &Group) -> Shape {
    Shape {
        epoch: g.epoch(),
        roster: roster(g),
        roles: g.group_roles(),
    }
}

/// Bob applies the commit, and (for an add) dave joins from the Welcome;
/// every remaining member then derives alice's exporter secret.
fn peers_agree(s: &mut Scene, staged: &Staged, op: Op) {
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &staged.commit)
        .expect("bob applies the commit");
    assert_eq!(
        s.bob_g.epoch(),
        s.alice_g.epoch(),
        "bob reaches alice's epoch"
    );
    assert_eq!(
        roster(&s.bob_g),
        roster(&s.alice_g),
        "bob's roster is alice's"
    );
    assert_eq!(s.bob_g.group_roles(), s.alice_g.group_roles());
    let want = secret(&s.alice_g, &s.alice);
    assert_eq!(secret(&s.bob_g, &s.bob), want, "bob derives alice's secret");
    if let Some(welcome) = &staged.welcome {
        let dave_g = Group::join_from_welcome(welcome, &s.dave).expect("dave joins");
        assert_eq!(dave_g.epoch(), s.alice_g.epoch());
        assert_eq!(
            secret(&dave_g, &s.dave),
            want,
            "dave derives alice's secret"
        );
    }
    if !matches!(op, Op::RemoveCharlie) {
        s.charlie_g
            .process_incoming_verifying_roles(&s.charlie, &staged.commit)
            .expect("charlie applies the commit");
        assert_eq!(secret(&s.charlie_g, &s.charlie), want);
    }
}

// ============================================================================
// 1. Merge reaches the eager path's state
// ============================================================================

fn staged_merge_matches_eager(op: Op, tag: u8) {
    let mut eager_scene = scene(tag);
    let eager_commit = eager(&mut eager_scene, op);
    let eager_shape = shape(&eager_scene.alice_g);
    peers_agree(&mut eager_scene, &eager_commit, op);

    let mut s = scene(tag);
    let before = shape(&s.alice_g);
    let staged = stage(&mut s, op);
    assert!(s.alice_g.has_staged_sweep(), "{op:?}: the commit is staged");
    assert_eq!(shape(&s.alice_g), before, "{op:?}: staging moves nothing");

    let next_key = s
        .alice_g
        .staged_export_secret(&s.alice, LABEL, b"ctx", 32)
        .expect("staged export")
        .expect("a commit is staged")
        .to_vec();
    assert!(s.alice_g.merge_staged_sweep(&s.alice).expect("merge"));
    assert!(!s.alice_g.has_staged_sweep());
    assert_eq!(
        secret(&s.alice_g, &s.alice),
        next_key,
        "{op:?}: the staged N+1 key is the merged epoch's key"
    );
    assert_eq!(
        shape(&s.alice_g),
        eager_shape,
        "{op:?}: a merged staged commit reaches the eager path's state"
    );
    peers_agree(&mut s, &staged, op);
}

#[test]
fn a_merged_staged_add_matches_the_eager_add() {
    staged_merge_matches_eager(Op::AddDave, 0x01);
}

#[test]
fn a_merged_staged_remove_matches_the_eager_remove() {
    staged_merge_matches_eager(Op::RemoveCharlie, 0x02);
}

#[test]
fn a_merged_staged_role_change_matches_the_eager_role_change() {
    staged_merge_matches_eager(Op::PromoteBob, 0x03);
}

// ============================================================================
// 2. Abandon leaves N untouched, and siblings untouched
// ============================================================================

fn abandon_leaves_n(op: Op, tag: u8) {
    let mut s = scene(tag);
    // A sibling group in alice's provider, with bob in it.
    let mut sib = Group::create(&s.alice, GroupKind::Group).expect("sibling");
    let sib_add = sib
        .add_member_from_wire(&s.alice, &kp_wire(&s.bob))
        .expect("sibling add bob");
    let mut bob_sib = Group::join_from_welcome(&wire(sib_add.welcome), &s.bob).expect("bob sib");
    let sib_before = (shape(&sib), secret(&sib, &s.alice));

    let before = (shape(&s.alice_g), secret(&s.alice_g, &s.alice));
    let staged = stage(&mut s, op);
    assert!(s.alice_g.abandon_staged_sweep(&s.alice).expect("abandon"));
    assert!(!s.alice_g.has_staged_sweep());
    assert_eq!(
        (shape(&s.alice_g), secret(&s.alice_g, &s.alice)),
        before,
        "{op:?}: abandoning leaves the group exactly at N"
    );
    assert_eq!(
        (shape(&sib), secret(&sib, &s.alice)),
        sib_before,
        "{op:?}: abandoning touches no sibling group"
    );

    // N still works: a message reaches bob, in both groups.
    let msg = wire(
        s.alice_g
            .create_application_message(&s.alice, b"still at N")
            .expect("send at N"),
    );
    match s
        .bob_g
        .process_incoming(&s.bob, &msg)
        .expect("bob decrypts")
    {
        lakko_mls::group::Incoming::Application { bytes, .. } => assert_eq!(bytes, b"still at N"),
        other => panic!("{op:?}: expected an application message, got {other:?}"),
    }
    let sib_msg = wire(
        sib.create_application_message(&s.alice, b"sibling")
            .expect("send in sibling"),
    );
    assert!(matches!(
        bob_sib
            .process_incoming(&s.bob, &sib_msg)
            .expect("bob decrypts sibling"),
        lakko_mls::group::Incoming::Application { .. }
    ));

    // The abandoned commit never reaches a peer as state: bob, who never
    // applied it, is still at alice's epoch, and alice can commit again.
    assert_eq!(s.bob_g.epoch(), s.alice_g.epoch());
    let again = stage(&mut s, op);
    assert_ne!(
        again.commit, staged.commit,
        "{op:?}: a fresh commit, not a replay"
    );
    assert!(s
        .alice_g
        .merge_staged_sweep(&s.alice)
        .expect("merge the second"));
    peers_agree(&mut s, &again, op);
}

#[test]
fn an_abandoned_staged_add_leaves_the_group_and_its_siblings_at_n() {
    abandon_leaves_n(Op::AddDave, 0x11);
}

#[test]
fn an_abandoned_staged_remove_leaves_the_group_and_its_siblings_at_n() {
    abandon_leaves_n(Op::RemoveCharlie, 0x12);
}

#[test]
fn an_abandoned_staged_role_change_leaves_the_group_and_its_siblings_at_n() {
    abandon_leaves_n(Op::PromoteBob, 0x13);
}

// ============================================================================
// 3. A staged commit survives a snapshot and a restore
// ============================================================================

/// alice's seed for a given scene tag (see [`scene`]).
fn alice_seed(tag: u8) -> [u8; 64] {
    [0x10 ^ tag; 64]
}

fn survives_restore(op: Op, tag: u8, merge: bool) {
    let mut s = scene(tag);
    let before = (shape(&s.alice_g), secret(&s.alice_g, &s.alice));
    let staged = stage(&mut s, op);
    let snapshot = s.alice.snapshot_storage().expect("snapshot");
    let group_id = s.alice_g.group_id();

    // A fresh handle on the same account, as after a restart.
    let alice2 = Identity::from_seed(&alice_seed(tag)).expect("alice again");
    alice2.restore_storage(&snapshot).expect("restore");
    let mut restored = Group::load(&alice2, &group_id)
        .expect("load")
        .expect("the group is in the snapshot");
    assert!(
        restored.has_staged_sweep(),
        "{op:?}: the staged commit survived"
    );
    assert_eq!(
        (shape(&restored), secret(&restored, &alice2)),
        before,
        "{op:?}: the restored group is still at N"
    );
    let staged_key = restored
        .staged_export_secret(&alice2, LABEL, b"ctx", 32)
        .expect("staged export")
        .expect("staged")
        .to_vec();
    assert_eq!(
        Some(staged_key.clone()),
        s.alice_g
            .staged_export_secret(&s.alice, LABEL, b"ctx", 32)
            .expect("staged export")
            .map(|k| k.to_vec()),
        "{op:?}: the restored staged commit is the same commit"
    );
    if merge {
        assert!(restored
            .merge_staged_sweep(&alice2)
            .expect("merge after restore"));
        assert_eq!(secret(&restored, &alice2), staged_key);
        s.alice_g = restored;
        s.alice = alice2;
        peers_agree(&mut s, &staged, op);
    } else {
        assert!(restored
            .abandon_staged_sweep(&alice2)
            .expect("abandon after restore"));
        assert_eq!((shape(&restored), secret(&restored, &alice2)), before);
    }
}

#[test]
fn a_staged_add_survives_a_restore_and_merges() {
    survives_restore(Op::AddDave, 0x21, true);
}

#[test]
fn a_staged_remove_survives_a_restore_and_merges() {
    survives_restore(Op::RemoveCharlie, 0x22, true);
}

#[test]
fn a_staged_role_change_survives_a_restore_and_merges() {
    survives_restore(Op::PromoteBob, 0x23, true);
}

#[test]
fn a_staged_commit_survives_a_restore_and_abandons() {
    survives_restore(Op::AddDave, 0x24, false);
}

// ============================================================================
// 4. A second commit is refused while one is staged
// ============================================================================

#[track_caller]
fn refused_as_staged<T>(r: lakko_mls::Result<T>, what: &str) {
    let Err(err) = r else {
        panic!("{what}: was accepted while a commit is staged");
    };
    assert_eq!(
        err.refusal_reason(),
        Some(RefusalReason::SweepAlreadyStaged),
        "{what}: {err}"
    );
}

#[test]
fn every_second_commit_is_refused_while_one_is_staged() {
    for (i, first) in [Op::AddDave, Op::RemoveCharlie, Op::PromoteBob]
        .into_iter()
        .enumerate()
    {
        let mut s = scene(0x31 + u8::try_from(i).expect("small"));
        let staged = stage(&mut s, first);
        let kp = kp_wire(&s.dave);
        let leaf = leaf_of(&s.alice_g, s.bob.signature_public_key());
        let roles = RoleTable::new(pk32(&s.alice), vec![pk32(&s.charlie)]);

        refused_as_staged(
            s.alice_g
                .stage_add_members_from_wire(&s.alice, &[kp.as_slice()]),
            "a second staged add",
        );
        refused_as_staged(
            s.alice_g.stage_remove_member(&s.alice, leaf),
            "a second staged remove",
        );
        refused_as_staged(
            s.alice_g.stage_set_group_roles(&s.alice, &roles),
            "a second staged role change",
        );
        refused_as_staged(
            s.alice_g.add_member_from_wire(&s.alice, &kp),
            "an eager add",
        );
        refused_as_staged(s.alice_g.remove_member(&s.alice, leaf), "an eager remove");
        refused_as_staged(
            s.alice_g.set_group_roles(&s.alice, &roles),
            "an eager role change",
        );
        refused_as_staged(s.alice_g.stage_pending_proposals(&s.alice), "a sweep stage");

        // The first commit is intact and still the one that merges.
        assert!(s.alice_g.has_staged_sweep());
        assert!(s
            .alice_g
            .merge_staged_sweep(&s.alice)
            .expect("merge the first"));
        peers_agree(&mut s, &staged, first);
    }
}

// ============================================================================
// Property: any sequence of staged commits, each merged or abandoned, with or
// without a restore in between, keeps alice and her peers in one state.
// ============================================================================

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        Just(Op::AddDave),
        Just(Op::RemoveCharlie),
        Just(Op::PromoteBob)
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 12, .. ProptestConfig::default() })]

    #[test]
    fn staged_commits_keep_the_group_consistent(
        op in op_strategy(),
        first_abandoned in any::<bool>(),
        restore in any::<bool>(),
        tag in 0x40u8..0x60,
    ) {
        let mut s = scene(tag);
        let at_n = (shape(&s.alice_g), secret(&s.alice_g, &s.alice));
        if first_abandoned {
            let _ = stage(&mut s, op);
            prop_assert!(s.alice_g.abandon_staged_sweep(&s.alice).expect("abandon"));
            prop_assert_eq!((shape(&s.alice_g), secret(&s.alice_g, &s.alice)), at_n.clone());
        }
        let staged = stage(&mut s, op);
        prop_assert_eq!((shape(&s.alice_g), secret(&s.alice_g, &s.alice)), at_n);
        if restore {
            let snapshot = s.alice.snapshot_storage().expect("snapshot");
            let id = s.alice_g.group_id();
            let alice2 = Identity::from_seed(&alice_seed(tag)).expect("alice again");
            alice2.restore_storage(&snapshot).expect("restore");
            s.alice_g = Group::load(&alice2, &id).expect("load").expect("present");
            s.alice = alice2;
            prop_assert!(s.alice_g.has_staged_sweep());
        }
        prop_assert!(s.alice_g.merge_staged_sweep(&s.alice).expect("merge"));
        peers_agree(&mut s, &staged, op);
    }
}
