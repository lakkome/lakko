// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A person removed from a group and later added back must be in it again.
//!
//! One [`Identity`] holds every group of a device in one OpenMLS provider, and
//! a Welcome names the same group id the evicted state is stored under. So
//! what these tests pin is not "a Welcome joins": it is that the evicted
//! group's state, still sitting under that id, does not stand in the way.
//!
//! Each scene walks the real path: an admin proposes, the owner sweeps, every
//! member applies the commit, the evicted device sees `is_active() == false`,
//! the admin proposes the same person from a fresh KeyPackage, the owner
//! sweeps, and the person joins with the SAME identity.
//!
//! The other half is C-88's: a Welcome replaces an ENDED group and nothing else.
//! A Welcome proves only that some group of that id invited this device, and
//! anyone who knows the id can build one, so an active group is never touched
//! and a refused join leaves the storage exactly as it found it.
//!
//! The one way an active group goes is `delete_after_ended_elsewhere`: the
//! account's own devices said it ended, and the epoch of that end keeps the
//! word off a group the person has since come back to.
//!
//! [`a_join_lands_once_the_evicted_rows_are_gone`] is a control: the same scene
//! with the group's rows stripped from the provider first. It separates "the
//! protocol cannot re-admit this person" from "the stored group is in the way".
//!
//! The forged Welcome that passes every admission check needs a conversation
//! built under a chosen id, which the public API does not offer, so it is a
//! unit test of `group.rs`.

use std::collections::BTreeMap;

use lakko_mls::{
    group, key_package, Error, Group, GroupKind, Identity, Incoming, LeafNodeIndex, RefusalReason,
    RoleTable,
};
use proptest::prelude::*;

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

fn leaf_of(g: &Group, id: &Identity) -> LeafNodeIndex {
    let idx = g
        .members()
        .iter()
        .find(|m| m.signature_public_key == id.signature_public_key())
        .expect("member present")
        .leaf_index;
    LeafNodeIndex::new(idx)
}

/// The provider's rows, as `snapshot_storage` writes them. A map, so two reads
/// compare whole whatever order the provider's `HashMap` iterates in.
fn rows(id: &Identity) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let snapshot = id.snapshot_storage().expect("snapshot");
    let mut cursor = &snapshot[..];
    let take = |cur: &mut &[u8], n: usize| -> Vec<u8> {
        let (head, rest) = cur.split_at(n);
        *cur = rest;
        head.to_vec()
    };
    let take_u32 = |cur: &mut &[u8]| -> u32 {
        let (head, rest) = cur.split_at(4);
        *cur = rest;
        u32::from_be_bytes(head.try_into().expect("4 bytes"))
    };

    let count = take_u32(&mut cursor);
    let mut out = BTreeMap::new();
    for _ in 0..count {
        let klen = take_u32(&mut cursor) as usize;
        let key = take(&mut cursor, klen);
        let vlen = take_u32(&mut cursor) as usize;
        out.insert(key, take(&mut cursor, vlen));
    }
    out
}

/// Whether a provider row belongs to `group_id`.
///
/// `MemoryStorage` keys are a label, then the JSON of the key: a group id is
/// written as a JSON byte array, so the array text finds every row of it.
fn names_group(key: &[u8], group_id: &[u8]) -> bool {
    let needle = format!(
        "[{}]",
        group_id
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(",")
    )
    .into_bytes();
    key.windows(needle.len()).any(|w| w == needle)
}

/// Every provider row keyed on `group_id`.
fn rows_of(id: &Identity, group_id: &[u8]) -> Vec<Vec<u8>> {
    rows(id)
        .into_keys()
        .filter(|key| names_group(key, group_id))
        .collect()
}

/// Removes every provider row keyed on `group_id`, leaving the rest of the
/// identity's state (signature key, KeyPackage bundles, other groups) in
/// place: what a delete of that one group would leave behind.
fn strip_group_rows(id: &Identity, group_id: &[u8]) -> usize {
    let all = rows(id);
    let kept: Vec<_> = all
        .iter()
        .filter(|(key, _)| !names_group(key, group_id))
        .collect();
    let dropped = all.len() - kept.len();

    let mut out = u32::try_from(kept.len())
        .expect("row count fits")
        .to_be_bytes()
        .to_vec();
    for (key, value) in kept {
        out.extend_from_slice(&u32::try_from(key.len()).expect("len").to_be_bytes());
        out.extend_from_slice(key);
        out.extend_from_slice(&u32::try_from(value.len()).expect("len").to_be_bytes());
        out.extend_from_slice(value);
    }
    id.restore_storage(&out).expect("restore");
    dropped
}

/// owner = alice, admin = bob, plus `person` as a plain member.
struct Scene {
    alice: Identity,
    bob: Identity,
    alice_g: Group,
    bob_g: Group,
    person_g: Group,
}

fn scene(tag: u8, person: &Identity) -> Scene {
    let alice = Identity::from_seed(&[0xA0 ^ tag; 64]).expect("alice");
    let bob = Identity::from_seed(&[0xB0 ^ tag; 64]).expect("bob");

    let mut alice_g = Group::create(&alice, GroupKind::Group).expect("alice creates group");
    let add_bob = alice_g
        .add_member_from_wire(&alice, &kp_wire(&bob))
        .expect("alice adds bob");
    let mut bob_g = Group::join_from_welcome(&wire(add_bob.welcome), &bob).expect("bob joins");

    let add_person = alice_g
        .add_member_from_wire(&alice, &kp_wire(person))
        .expect("alice adds the person");
    bob_g
        .process_incoming_verifying_roles(&bob, &wire(add_person.commit))
        .expect("bob applies the add");
    let mut person_g =
        Group::join_from_welcome(&wire(add_person.welcome), person).expect("the person joins");

    let roles = RoleTable::new(pk32(&alice), vec![pk32(&bob)]);
    let promote = alice_g
        .set_group_roles(&alice, &roles)
        .expect("alice promotes bob");
    bob_g
        .process_incoming_verifying_roles(&bob, &promote)
        .expect("bob applies the promote");
    person_g
        .process_incoming_verifying_roles(person, &promote)
        .expect("the person applies the promote");

    Scene {
        alice,
        bob,
        alice_g,
        bob_g,
        person_g,
    }
}

/// bob proposes the person's removal, alice sweeps. Returns the commit, which
/// bob has applied and the person has not.
fn remove_person(s: &mut Scene, person: &Identity) -> Vec<u8> {
    let target = leaf_of(&s.alice_g, person);
    let proposal = s
        .bob_g
        .propose_remove_member(&s.bob, target)
        .expect("bob proposes the removal");
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &proposal)
        .expect("alice stores the proposal");
    s.person_g
        .process_incoming_verifying_roles(person, &proposal)
        .expect("the person stores the proposal");
    let sweep = s
        .alice_g
        .commit_pending_proposals(&s.alice)
        .expect("sweep")
        .expect("the sweep carried the removal");
    let commit = wire(sweep.commit);
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &commit)
        .expect("bob applies the removal");
    commit
}

/// bob proposes the person again from a fresh KeyPackage, alice sweeps.
/// Returns `(commit, welcome)`; bob has applied the commit.
fn add_person_back(s: &mut Scene, person: &Identity) -> (Vec<u8>, Vec<u8>) {
    let proposal = s
        .bob_g
        .propose_add_member_from_wire(&s.bob, &kp_wire(person))
        .expect("bob proposes the person again");
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &proposal)
        .expect("alice stores the proposal");
    let sweep = s
        .alice_g
        .commit_pending_proposals(&s.alice)
        .expect("sweep")
        .expect("the sweep carried the add");
    let commit = wire(sweep.commit);
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &commit)
        .expect("bob applies the add");
    (commit, wire(sweep.welcome.expect("an add has a Welcome")))
}

/// The person is a working member again: active at the group's epoch, a
/// message from them reads at alice, a message from alice reads at them.
fn assert_back_in(s: &mut Scene, person: &Identity, person_g: &mut Group, where_: &str) {
    assert!(person_g.is_active(), "{where_}: the person is active");
    assert_eq!(
        person_g.epoch(),
        s.alice_g.epoch(),
        "{where_}: the person is at the group's epoch"
    );

    let out = person_g
        .create_application_message(person, b"back again")
        .expect("the person sends");
    let got = s
        .alice_g
        .process_incoming_verifying_roles(&s.alice, &wire(out))
        .expect("alice reads the person");
    let Incoming::Application { bytes, .. } = got else {
        panic!("{where_}: expected an application message, got {got:?}");
    };
    assert_eq!(bytes, b"back again", "{where_}: alice reads the text");

    let out = s
        .alice_g
        .create_application_message(&s.alice, b"welcome back")
        .expect("alice sends");
    let got = person_g
        .process_incoming_verifying_roles(person, &wire(out))
        .expect("the person reads alice");
    let Incoming::Application { bytes, .. } = got else {
        panic!("{where_}: expected an application message, got {got:?}");
    };
    assert_eq!(
        bytes, b"welcome back",
        "{where_}: the person reads the text"
    );
}

/// A relaunch: a fresh identity from the seed, the snapshot restored, the
/// group loaded from storage. The same path every client takes at launch.
fn relaunch(person: &Identity, seed: &[u8], group_id: &[u8]) -> (Identity, Group) {
    let snapshot = person.snapshot_storage().expect("snapshot");
    let restored = Identity::from_seed(seed).expect("identity from seed");
    restored.restore_storage(&snapshot).expect("restore");
    let loaded = Group::load(&restored, group_id)
        .expect("load")
        .expect("the group is in storage");
    (restored, loaded)
}

// ============================================================================
// The report
// ============================================================================

/// VG3: removed, then added back by an admin, same identity on the same
/// device. The commit reached the device first, so it holds the group as
/// inactive when the Welcome comes.
#[test]
fn a_removed_person_added_back_is_in_the_group_again() {
    let seed = [0xC7u8; 64];
    let person = Identity::from_seed(&seed).expect("person");
    let mut s = scene(0x01, &person);
    let group_id = s.alice_g.group_id();

    let removal = remove_person(&mut s, &person);
    s.person_g
        .process_incoming_verifying_roles(&person, &removal)
        .expect("the person applies its own removal");
    assert!(!s.person_g.is_active(), "the removal took effect");

    let (_commit, welcome) = add_person_back(&mut s, &person);
    let mut rejoined = Group::join_from_welcome(&welcome, &person)
        .expect("a Welcome after a removal must join under the same group id");
    assert_eq!(rejoined.group_id(), group_id);
    assert_back_in(&mut s, &person, &mut rejoined, "after the rejoin");

    // The bar the client actually needs: the state survives a relaunch, and
    // the loaded handle is the rejoined group, not the evicted one.
    let (restored, mut loaded) = relaunch(&person, &seed, &group_id);
    assert_back_in(&mut s, &restored, &mut loaded, "after a relaunch");
}

/// The same with a device-bound leaf (ADR-0050): the person's leaf is a device
/// key whose credential binds it to an account, which is what `Group::members`
/// resolves and what the three clients run in production.
#[test]
fn a_removed_device_bound_person_added_back_is_in_the_group_again() {
    let account = Identity::from_seed(&[0xD0u8; 64]).expect("account");
    let device_seed = [0xD1u8; 32];
    let account_seed = account.ed25519_seed_bytes();
    let person =
        Identity::from_seed_bound(&device_seed, &account_seed, 1_700_000_000).expect("device");
    let mut s = scene(0x02, &person);

    let removal = remove_person(&mut s, &person);
    s.person_g
        .process_incoming_verifying_roles(&person, &removal)
        .expect("the device applies its own removal");
    assert!(!s.person_g.is_active());

    let (_commit, welcome) = add_person_back(&mut s, &person);
    let mut rejoined = Group::join_from_welcome(&welcome, &person)
        .expect("a Welcome after a removal must join for a bound device too");
    assert_back_in(&mut s, &person, &mut rejoined, "bound device");
}

/// The Welcome comes off the mailbox BEFORE the journal reached the removal:
/// the device still holds the group as active at the old epoch. Nothing here
/// says the first attempt has to succeed. What has to hold is that the Welcome
/// is not lost: once the removal has been applied, the same bytes join.
#[test]
fn a_welcome_ahead_of_the_removal_joins_once_the_removal_is_applied() {
    let seed = [0xC8u8; 64];
    let person = Identity::from_seed(&seed).expect("person");
    let mut s = scene(0x03, &person);

    let removal = remove_person(&mut s, &person);
    let (_commit, welcome) = add_person_back(&mut s, &person);

    // Still active at the old epoch, the removal unapplied: refused, typed, and
    // the Welcome is not spent.
    assert!(s.person_g.is_active());
    let before = rows(&person);
    let first = Group::join_from_welcome(&welcome, &person);
    assert_still_active(first.err(), &s.alice_g.group_id());
    assert_eq!(rows(&person), before, "a refused join changes nothing");

    s.person_g
        .process_incoming_verifying_roles(&person, &removal)
        .expect("the person applies its own removal");
    assert!(!s.person_g.is_active());

    let mut rejoined = Group::join_from_welcome(&welcome, &person)
        .expect("the held Welcome joins once the removal is applied");
    assert_back_in(&mut s, &person, &mut rejoined, "held Welcome");
}

// ============================================================================
// A Welcome never touches an active group
// ============================================================================

#[track_caller]
fn assert_still_active(refused: Option<Error>, group_id: &[u8]) {
    let error = refused.expect("refused");
    assert_eq!(
        error.refusal_reason(),
        Some(RefusalReason::GroupStillActive),
        "typed, got {error}"
    );
    let Error::GroupStillActive { group_id: named } = error else {
        panic!("carries the group");
    };
    assert_eq!(named, group_id, "it names the group");
}

/// A Welcome for a group this device is really in: a duplicate, the same bytes
/// a second time. Refused, typed, the storage untouched, the group working.
#[test]
fn a_welcome_for_an_active_group_is_refused_and_changes_nothing() {
    let person = Identity::from_seed(&[0xCAu8; 64]).expect("person");
    let alice = Identity::from_seed(&[0xA5u8; 64]).expect("alice");
    let mut alice_g = Group::create(&alice, GroupKind::Group).expect("group");
    let add = alice_g
        .add_member_from_wire(&alice, &kp_wire(&person))
        .expect("alice adds the person");
    let welcome = wire(add.welcome);
    let mut person_g = Group::join_from_welcome(&welcome, &person).expect("the person joins");

    let before = rows(&person);
    let refused = Group::join_from_welcome(&welcome, &person);
    assert_still_active(refused.err(), &alice_g.group_id());
    assert_eq!(rows(&person), before, "the storage is as it was");

    assert!(person_g.is_active());
    let out = person_g
        .create_application_message(&person, b"still here")
        .expect("the person sends");
    let got = alice_g
        .process_incoming_verifying_roles(&alice, &wire(out))
        .expect("alice reads the person");
    assert!(matches!(got, Incoming::Application { .. }));
}

/// A Welcome with no role table, naming the id of a live conversation: anyone
/// who knows the id can build this one. Refused on the staged Welcome, and the
/// conversation does not notice.
#[test]
fn a_forged_table_less_welcome_does_not_touch_a_live_conversation() {
    let person = Identity::from_seed(&[0xCBu8; 64]).expect("person");
    let mut s = scene(0x05, &person);
    let group_id = s.alice_g.group_id();

    let mallory = Identity::from_seed(&[0x66u8; 64]).expect("mallory");
    let mut fake = Group::create_with_id(&mallory, &group_id).expect("a group under the same id");
    let forged = fake
        .add_member_from_wire(&mallory, &kp_wire(&person))
        .expect("mallory invites the person");

    let before = rows(&person);
    let refused = Group::join_from_welcome(&wire(forged.welcome), &person);
    assert_eq!(
        refused.err().and_then(|e| e.refusal_reason()),
        Some(RefusalReason::NoRoleTable),
    );
    assert_eq!(rows(&person), before, "the storage is as it was");

    let out = s
        .person_g
        .create_application_message(&person, b"nothing happened")
        .expect("the person sends");
    let got = s
        .alice_g
        .process_incoming_verifying_roles(&s.alice, &wire(out))
        .expect("alice reads the person");
    assert!(matches!(got, Incoming::Application { .. }));
}

/// A refused join spends no KeyPackage, on the link path too: the package an
/// external-join proposal carries opens the same Welcome once the removal has
/// been applied. A spent package would open to `NoMatchingKeyPackage`, which
/// every client reads as "sealed to another device".
#[test]
fn a_refused_join_keeps_the_key_package_usable() {
    let person = Identity::from_seed(&[0xCCu8; 64]).expect("person");
    let mut s = scene(0x06, &person);
    let group_id = s.alice_g.group_id();
    let removal = remove_person(&mut s, &person);

    // The person, not yet aware of the removal, asks to join by link.
    let knock = person
        .create_external_join_proposal(&group_id, s.alice_g.epoch())
        .expect("the person knocks");
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &knock)
        .expect("alice retains the request");
    let approval = s
        .alice_g
        .approve_external_join(&s.alice, person.signature_public_key())
        .expect("alice approves");
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &approval)
        .expect("bob stores the add");
    let sweep = s
        .alice_g
        .commit_pending_proposals(&s.alice)
        .expect("sweep")
        .expect("the sweep carried the add");
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &wire(sweep.commit))
        .expect("bob applies the add");
    let welcome = wire(sweep.welcome.expect("an add has a Welcome"));

    let before = rows(&person);
    let refused = Group::join_from_welcome(&welcome, &person);
    assert_still_active(refused.err(), &group_id);
    assert_eq!(rows(&person), before, "the KeyPackage is still there");

    s.person_g
        .process_incoming_verifying_roles(&person, &removal)
        .expect("the person applies its own removal");
    let mut rejoined = Group::join_from_welcome(&welcome, &person)
        .expect("the same Welcome opens once the removal is applied");
    assert_back_in(&mut s, &person, &mut rejoined, "held Welcome, link path");
}

// ============================================================================
// Repetition and restart
// ============================================================================

/// Removed and added back, over and over, with no wipe in between: each Welcome
/// replaces the ended group, and a relaunch after the last round finds the
/// group the person is in.
#[test]
fn a_person_can_be_removed_and_added_back_repeatedly() {
    let seed = [0xCDu8; 64];
    let person = Identity::from_seed(&seed).expect("person");
    let mut s = scene(0x07, &person);
    let group_id = s.alice_g.group_id();

    let mut rows_after_a_round = None;
    for round in 0..3 {
        let removal = remove_person(&mut s, &person);
        s.person_g
            .process_incoming_verifying_roles(&person, &removal)
            .expect("the person applies its own removal");
        assert!(!s.person_g.is_active());

        let (_commit, welcome) = add_person_back(&mut s, &person);
        let mut rejoined = Group::join_from_welcome(&welcome, &person)
            .unwrap_or_else(|e| panic!("round {round}: {e}"));
        assert_back_in(&mut s, &person, &mut rejoined, "repeated");
        s.person_g = rejoined;

        // A replaced group leaves nothing of itself behind, so the rows under
        // the id do not grow with the rounds.
        let count = rows_of(&person, &group_id).len();
        assert_eq!(*rows_after_a_round.get_or_insert(count), count);
    }

    let (restored, mut loaded) = relaunch(&person, &seed, &group_id);
    assert_back_in(&mut s, &restored, &mut loaded, "after a relaunch");
}

/// The ended group survives a relaunch and is still replaced: this is the
/// device that was evicted before this build, whose snapshot holds the ended
/// group when it first runs it.
#[test]
fn an_ended_group_restored_from_a_snapshot_is_replaced_by_a_welcome() {
    let seed = [0xCEu8; 64];
    let person = Identity::from_seed(&seed).expect("person");
    let mut s = scene(0x08, &person);
    let group_id = s.alice_g.group_id();

    let removal = remove_person(&mut s, &person);
    s.person_g
        .process_incoming_verifying_roles(&person, &removal)
        .expect("the person applies its own removal");
    let (_commit, welcome) = add_person_back(&mut s, &person);

    let (restored, ended) = relaunch(&person, &seed, &group_id);
    assert!(!ended.is_active(), "the relaunch finds the ended group");

    let mut rejoined =
        Group::join_from_welcome(&welcome, &restored).expect("a Welcome replaces it");
    assert_back_in(&mut s, &restored, &mut rejoined, "after a relaunch");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(6))]

    /// Any run of rounds, each in either order: the removal applied before the
    /// Welcome comes, or the Welcome held until the removal is. The person ends
    /// every round in the group.
    #[test]
    fn every_order_of_removal_and_welcome_ends_in_the_group(
        tag in any::<u8>(),
        rounds in proptest::collection::vec(any::<bool>(), 1..4),
    ) {
        let person = Identity::from_seed(&[tag ^ 0x5A; 64]).expect("person");
        let mut s = scene(tag, &person);
        let group_id = s.alice_g.group_id();

        for welcome_first in rounds {
            let removal = remove_person(&mut s, &person);
            let (_commit, welcome) = add_person_back(&mut s, &person);
            if welcome_first {
                let refused = Group::join_from_welcome(&welcome, &person);
                assert_still_active(refused.err(), &group_id);
            }
            s.person_g
                .process_incoming_verifying_roles(&person, &removal)
                .expect("the person applies its own removal");
            let mut rejoined =
                Group::join_from_welcome(&welcome, &person).expect("the Welcome joins");
            assert_back_in(&mut s, &person, &mut rejoined, "any order");
            s.person_g = rejoined;
        }
    }
}

// ============================================================================
// Group::delete
// ============================================================================

/// The labels of the provider's rows, for what stays after a delete.
fn labels(id: &Identity) -> Vec<String> {
    let mut out: Vec<String> = rows(id)
        .into_keys()
        .map(|key| {
            String::from_utf8_lossy(
                &key[..key
                    .iter()
                    .position(|b| *b == b'[' || *b == b'{' || *b == b'"')
                    .unwrap_or(key.len())],
            )
            .into_owned()
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Nothing of an ended group stays under its id, its private keys above all.
///
/// The person commits once first, so it holds path keys and not only its leaf
/// key, and the eviction leaves them filed under the epoch before the one the
/// group reports: `MlsGroup::delete` alone would miss that row, and the queued
/// proposals too.
#[test]
fn delete_leaves_no_row_of_an_ended_group() {
    let person = Identity::from_seed(&[0xCFu8; 64]).expect("person");
    let mut s = scene(0x09, &person);
    let group_id = s.alice_g.group_id();

    let update = s.person_g.self_update(&person).expect("the person commits");
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &update)
        .expect("alice applies");
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &update)
        .expect("bob applies");

    // A stranger's request, retained in a row of Lakko's own.
    let stranger = Identity::from_seed(&[0xD6u8; 64]).expect("stranger");
    let knock = stranger
        .create_external_join_proposal(&group_id, s.person_g.epoch())
        .expect("the stranger knocks");
    s.person_g
        .process_incoming_verifying_roles(&person, &knock)
        .expect("the person retains the request");
    assert!(
        rows(&person)
            .keys()
            .any(|key| key.starts_with(b"lakko/pending-external-joins/v1\0")),
        "the request is in the provider",
    );

    let removal = remove_person(&mut s, &person);
    s.person_g
        .process_incoming_verifying_roles(&person, &removal)
        .expect("the person applies its own removal");
    assert!(!s.person_g.is_active());

    let before = rows_of(&person, &group_id);
    assert!(
        before.iter().any(|key| key.starts_with(b"EpochKeyPairs")),
        "the eviction left private keys under the group",
    );
    assert!(
        before.iter().any(|key| key.starts_with(b"QueuedProposal")),
        "and the proposal that removed the person, which OpenMLS's own clear misses",
    );

    s.person_g.delete(&person).expect("deleted");
    assert_eq!(rows_of(&person, &group_id), Vec::<Vec<u8>>::new());
    assert!(
        Group::load(&person, &group_id).expect("load").is_none(),
        "nothing of the group is left to load"
    );
    // What stays is the identity's own: its signature key and the KeyPackage
    // bundles, which name no group (ADR-0050 section 2.6). This pins the list,
    // so a new kind of private row left behind fails here.
    assert_eq!(labels(&person), ["KeyPackage", "SignatureKeyPair"]);

    // Repeating it changes nothing, also through a handle that is now stale.
    let after = rows(&person);
    s.person_g.delete(&person).expect("deleted again");
    assert_eq!(rows(&person), after);
}

/// A Welcome after a delete is an ordinary join, and the group is whole.
#[test]
fn a_welcome_after_a_delete_joins() {
    let seed = [0xD3u8; 64];
    let person = Identity::from_seed(&seed).expect("person");
    let mut s = scene(0x0A, &person);
    let group_id = s.alice_g.group_id();

    let removal = remove_person(&mut s, &person);
    s.person_g
        .process_incoming_verifying_roles(&person, &removal)
        .expect("the person applies its own removal");
    s.person_g.delete(&person).expect("deleted");

    let (_commit, welcome) = add_person_back(&mut s, &person);
    let mut rejoined = Group::join_from_welcome(&welcome, &person).expect("joins");
    assert_back_in(&mut s, &person, &mut rejoined, "after a delete");

    let (restored, mut loaded) = relaunch(&person, &seed, &group_id);
    assert_back_in(&mut s, &restored, &mut loaded, "after a relaunch");
}

/// An active group is not deleted, and neither is a group that replaced this
/// handle's: a wipe racing a Welcome must not take the group the person has
/// just come back to.
#[test]
fn delete_refuses_an_active_group_also_through_a_stale_handle() {
    let person = Identity::from_seed(&[0xD4u8; 64]).expect("person");
    let mut s = scene(0x0B, &person);
    let group_id = s.alice_g.group_id();

    let before = rows(&person);
    assert_still_active(s.person_g.delete(&person).err(), &group_id);
    assert_eq!(rows(&person), before, "the storage is as it was");

    let removal = remove_person(&mut s, &person);
    s.person_g
        .process_incoming_verifying_roles(&person, &removal)
        .expect("the person applies its own removal");
    let (_commit, welcome) = add_person_back(&mut s, &person);
    let mut rejoined = Group::join_from_welcome(&welcome, &person).expect("joins");

    // `s.person_g` is the ended handle.
    let before = rows(&person);
    assert_still_active(s.person_g.delete(&person).err(), &group_id);
    assert_eq!(rows(&person), before, "the rejoined group is untouched");
    assert_back_in(&mut s, &person, &mut rejoined, "stale handle");
}

/// A delete takes one group and leaves its neighbours in the provider alone.
#[test]
fn delete_leaves_other_groups_alone() {
    let person = Identity::from_seed(&[0xD5u8; 64]).expect("person");
    let mut s = scene(0x0C, &person);
    let mut other = Group::create(&person, GroupKind::Group).expect("another group");
    let other_id = other.group_id();
    let other_rows = rows_of(&person, &other_id);

    let removal = remove_person(&mut s, &person);
    s.person_g
        .process_incoming_verifying_roles(&person, &removal)
        .expect("the person applies its own removal");
    s.person_g.delete(&person).expect("deleted");

    assert_eq!(rows_of(&person, &other_id), other_rows);
    assert!(other.is_active());
    other
        .create_application_message(&person, b"still fine")
        .expect("the other group works");
}

// ============================================================================
// Group::delete_after_ended_elsewhere
// ============================================================================

/// The epoch of the end: what the group froze at on a device that applied the
/// removal, which is the epoch alice is at right after sweeping it.
fn remove_person_and_read_end(s: &mut Scene, person: &Identity) -> (Vec<u8>, u64) {
    let removal = remove_person(s, person);
    let end = s.alice_g.epoch();
    (removal, end)
}

/// A device that never applied the commit, which is the device this entry
/// point exists for: past the journal's retention it never can. Everything of
/// the group goes, the private keys of an ordinary active member and a retained
/// request among them, and a later Welcome is an ordinary join.
#[test]
fn a_device_that_never_applied_the_removal_deletes_on_the_register() {
    let seed = [0xD7u8; 64];
    let person = Identity::from_seed(&seed).expect("person");
    let mut s = scene(0x0D, &person);
    let group_id = s.alice_g.group_id();

    let update = s.person_g.self_update(&person).expect("the person commits");
    s.alice_g
        .process_incoming_verifying_roles(&s.alice, &update)
        .expect("alice applies");
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &update)
        .expect("bob applies");
    let stranger = Identity::from_seed(&[0xD8u8; 64]).expect("stranger");
    let knock = stranger
        .create_external_join_proposal(&group_id, s.person_g.epoch())
        .expect("the stranger knocks");
    s.person_g
        .process_incoming_verifying_roles(&person, &knock)
        .expect("the person retains the request");

    let (_removal, end) = remove_person_and_read_end(&mut s, &person);
    assert!(
        s.person_g.is_active(),
        "the commit never reached this device"
    );
    assert!(s.person_g.epoch() < end);
    assert!(!rows_of(&person, &group_id).is_empty());

    s.person_g
        .delete_after_ended_elsewhere(&person, end)
        .expect("deleted");
    assert_eq!(rows_of(&person, &group_id), Vec::<Vec<u8>>::new());
    assert!(Group::load(&person, &group_id).expect("load").is_none());
    assert_eq!(labels(&person), ["KeyPackage", "SignatureKeyPair"]);

    // Repeating it changes nothing, through this handle that is now stale.
    let after = rows(&person);
    s.person_g
        .delete_after_ended_elsewhere(&person, end)
        .expect("deleted again");
    assert_eq!(rows(&person), after);

    let (_commit, welcome) = add_person_back(&mut s, &person);
    let mut rejoined = Group::join_from_welcome(&welcome, &person).expect("joins");
    assert_back_in(&mut s, &person, &mut rejoined, "after the register");
    let (restored, mut loaded) = relaunch(&person, &seed, &group_id);
    assert_back_in(&mut s, &restored, &mut loaded, "after a relaunch");
}

/// The hole the epoch closes. The person is removed and added back, and a
/// device of the account applies the old removal late and writes "ended" with
/// the newest stamp. A device that has rejoined must not lose the group to it,
/// through the rejoined handle or a stale one.
#[test]
fn a_stale_register_does_not_delete_a_rejoined_group() {
    let person = Identity::from_seed(&[0xD9u8; 64]).expect("person");
    let mut s = scene(0x0E, &person);
    let group_id = s.alice_g.group_id();

    let (removal, end) = remove_person_and_read_end(&mut s, &person);
    s.person_g
        .process_incoming_verifying_roles(&person, &removal)
        .expect("the person applies its own removal");
    let (_commit, welcome) = add_person_back(&mut s, &person);
    let mut rejoined = Group::join_from_welcome(&welcome, &person).expect("joins");
    assert!(rejoined.epoch() > end);

    let before = rows(&person);
    assert_still_active(
        rejoined.delete_after_ended_elsewhere(&person, end).err(),
        &group_id,
    );
    assert_eq!(rows(&person), before, "the rejoined group is untouched");
    assert_still_active(
        s.person_g.delete_after_ended_elsewhere(&person, end).err(),
        &group_id,
    );
    assert_eq!(rows(&person), before, "and a stale handle cannot reach it");
    assert_back_in(&mut s, &person, &mut rejoined, "stale register");
}

/// A group that is already ended goes whatever the register says: a device
/// frozen at a later end than the one it hears about holds a dead group either
/// way, and this must never strand it.
#[test]
fn an_ended_group_goes_at_any_epoch() {
    let person = Identity::from_seed(&[0xDAu8; 64]).expect("person");
    let mut s = scene(0x0F, &person);
    let group_id = s.alice_g.group_id();

    let (removal, end) = remove_person_and_read_end(&mut s, &person);
    s.person_g
        .process_incoming_verifying_roles(&person, &removal)
        .expect("the person applies its own removal");
    assert!(!s.person_g.is_active());

    s.person_g
        .delete_after_ended_elsewhere(&person, end - 1)
        .expect("an older register still takes an ended group");
    assert_eq!(rows_of(&person, &group_id), Vec::<Vec<u8>>::new());
}

/// A staged sweep goes with the group: the commit never went out, or its
/// verdict no longer matters, and a group holding one is otherwise frozen.
#[test]
fn a_staged_sweep_goes_with_the_group() {
    let person = Identity::from_seed(&[0xDBu8; 64]).expect("person");
    let mut s = scene(0x10, &person);
    let group_id = s.alice_g.group_id();

    let target = leaf_of(&s.alice_g, &person);
    let proposal = s
        .alice_g
        .propose_remove_member(&s.alice, target)
        .expect("alice proposes");
    s.bob_g
        .process_incoming_verifying_roles(&s.bob, &proposal)
        .expect("bob stores the proposal");
    s.bob_g
        .stage_pending_proposals(&s.bob)
        .expect("bob stages")
        .expect("the sweep carried the proposal");
    assert!(s.bob_g.has_staged_sweep());
    let end = s.bob_g.epoch() + 1;
    let (bob, bob_g) = (&s.bob, &mut s.bob_g);
    bob_g
        .delete_after_ended_elsewhere(bob, end)
        .expect("deleted");
    assert_eq!(rows_of(&s.bob, &group_id), Vec::<Vec<u8>>::new());
    assert_eq!(labels(&s.bob), ["KeyPackage", "SignatureKeyPair"]);
}

/// One group goes and the provider's other rows stay exactly as they were.
#[test]
fn deleting_on_the_register_leaves_other_groups_alone() {
    let person = Identity::from_seed(&[0xDCu8; 64]).expect("person");
    let mut s = scene(0x11, &person);
    let mut other = Group::create(&person, GroupKind::Group).expect("another group");
    let other_id = other.group_id();
    let other_rows = rows_of(&person, &other_id);

    let (_removal, end) = remove_person_and_read_end(&mut s, &person);
    s.person_g
        .delete_after_ended_elsewhere(&person, end)
        .expect("deleted");

    assert_eq!(rows_of(&person, &other_id), other_rows);
    assert!(other.is_active());
    other
        .create_application_message(&person, b"still fine")
        .expect("the other group works");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(6))]

    /// Whatever rounds of removal and return came before, and whichever of
    /// them the device applied, one rule holds: an active group above the
    /// epoch of the end stays untouched; anything else goes, whole.
    #[test]
    fn a_register_takes_the_ended_group_and_never_a_later_one(
        tag in any::<u8>(),
        rounds in 0usize..3,
        applied in any::<bool>(),
        ended_at in -2i64..=2,
    ) {
        let person = Identity::from_seed(&[tag ^ 0xA5; 64]).expect("person");
        let mut s = scene(tag, &person);
        let group_id = s.alice_g.group_id();

        for _ in 0..rounds {
            let removal = remove_person(&mut s, &person);
            s.person_g
                .process_incoming_verifying_roles(&person, &removal)
                .expect("the person applies its own removal");
            let (_commit, welcome) = add_person_back(&mut s, &person);
            s.person_g = Group::join_from_welcome(&welcome, &person).expect("joins");
        }

        let removal = remove_person(&mut s, &person);
        if applied {
            s.person_g
                .process_incoming_verifying_roles(&person, &removal)
                .expect("the person applies its own removal");
        }
        let held = s.person_g.epoch();
        let active = s.person_g.is_active();

        // A register at, above and below what this device holds.
        let ended_epoch = held.saturating_add_signed(ended_at);
        let before = rows(&person);
        let outcome = s.person_g.delete_after_ended_elsewhere(&person, ended_epoch);
        if active && held > ended_epoch {
            assert_still_active(outcome.err(), &group_id);
            prop_assert_eq!(rows(&person), before);
            prop_assert!(s.person_g.is_active());
        } else {
            outcome.expect("deleted");
            prop_assert_eq!(rows_of(&person, &group_id), Vec::<Vec<u8>>::new());
            prop_assert!(Group::load(&person, &group_id).expect("load").is_none());
        }
    }
}

// ============================================================================
// The control
// ============================================================================

/// The cause, isolated: the same scene, with the evicted group's rows taken
/// out of the provider before the Welcome is opened. The Welcome, the
/// KeyPackage, the sweep, the leaf, the identity are the same, so a green here
/// next to a red above puts the refusal on the stored group alone.
///
/// Also the "does a Welcome after a wipe still join" answer, for a wipe that
/// reaches the provider, twice over.
#[test]
fn a_join_lands_once_the_evicted_rows_are_gone() {
    let seed = [0xC9u8; 64];
    let person = Identity::from_seed(&seed).expect("person");
    let mut s = scene(0x04, &person);
    let group_id = s.alice_g.group_id();

    for round in 0..2 {
        let removal = remove_person(&mut s, &person);
        s.person_g
            .process_incoming_verifying_roles(&person, &removal)
            .expect("the person applies its own removal");
        assert!(!s.person_g.is_active());

        assert!(
            strip_group_rows(&person, &group_id) > 0,
            "the evicted group has rows under its id"
        );
        assert!(
            Group::load(&person, &group_id).expect("load").is_none(),
            "nothing of the group is left to load"
        );

        let (_commit, welcome) = add_person_back(&mut s, &person);
        let mut rejoined = Group::join_from_welcome(&welcome, &person)
            .unwrap_or_else(|e| panic!("round {round}: join after the strip: {e}"));
        assert_back_in(&mut s, &person, &mut rejoined, "after the strip");
        s.person_g = rejoined;
    }

    let (restored, mut loaded) = relaunch(&person, &seed, &group_id);
    assert_back_in(&mut s, &restored, &mut loaded, "after a relaunch");
}
