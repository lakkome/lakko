// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0087 section 5 - a request to join is a bare KeyPackage with no epoch.
//!
//! `Group::retain_join_request` takes the KeyPackage a sealed knock row
//! carried and keeps it in the ADR-0027 quarantine; approving is unchanged.
//! These tests drive the public API only and pin the three properties the
//! ADR's plan names for step 3:
//!
//! - a request survives any number of commits (and restarts) and is still
//!   approvable, sweepable and joinable afterwards;
//! - the cap holds: never more than `MAX_PENDING_JOIN_REQUESTS` waiting, and a
//!   request past it is answered `QueueFull` with nothing kept;
//! - a commit (or any other MLS message) written into a knock row does
//!   nothing: it is refused before any group state is read or written, and the
//!   same commit still applies through the journal afterwards.
//!
//! Plus the admission rules (one-to-one, an unresolvable credential, a bound
//! device) and the publisher path.

use std::sync::OnceLock;

use lakko_mls::group::encode_message;
use lakko_mls::{
    key_package, ChannelId, Group, GroupKind, Identity, JoinRequestOutcome, PublisherGroup,
    MAX_PENDING_JOIN_REQUESTS,
};
use proptest::prelude::*;

const OWNER_SEED: [u8; 64] = [0x0Au8; 64];

fn id(tag: u8) -> Identity {
    Identity::from_seed(&[tag; 64]).expect("identity")
}

fn kp_wire(who: &Identity) -> Vec<u8> {
    key_package::serialize(&who.create_key_package().expect("key package")).expect("serialize kp")
}

fn pk(who: &Identity) -> Vec<u8> {
    who.signature_public_key().to_vec()
}

fn wire(msg: &openmls::framing::MlsMessageOut) -> Vec<u8> {
    encode_message(msg).expect("encode")
}

/// Snapshot the owner's storage, rebuild the identity from its seed, restore
/// and load the group: what an app restart does.
fn restart(identity: Identity, group: Group) -> (Identity, Group) {
    let group_id = group.group_id();
    let snapshot = identity.snapshot_storage().expect("snapshot");
    drop(group);
    drop(identity);
    let identity = Identity::from_seed(&OWNER_SEED).expect("owner again");
    identity.restore_storage(&snapshot).expect("restore");
    let group = Group::load(&identity, &group_id)
        .expect("load")
        .expect("group present");
    (identity, group)
}

/// Approve `joiner`'s retained request, sweep, and land the joiner from the
/// sweep's Welcome. Asserts the joiner is a member on both sides at one epoch.
fn approve_sweep_and_join(owner: &Identity, g: &mut Group, joiner: &Identity) {
    let proposal = g
        .approve_external_join(owner, &pk(joiner))
        .expect("approve");
    assert!(!proposal.is_empty());
    assert!(!g.pending_external_joins().contains(&pk(joiner)));
    let sweep = g
        .commit_pending_proposals(owner)
        .expect("sweep")
        .expect("a commit was produced");
    let welcome = sweep.welcome.expect("the sweep carries the Welcome");
    let landed = Group::join_from_welcome(&wire(&welcome), joiner).expect("join");
    assert_eq!(landed.group_id(), g.group_id());
    assert_eq!(landed.epoch(), g.epoch());
    assert!(g
        .members()
        .iter()
        .any(|m| m.signature_public_key == pk(joiner)));
}

// ---------------------------------------------------------------------------
// A request survives commits
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Step {
    /// The owner commits a self update.
    SelfUpdate,
    /// The owner adds a new member directly.
    Add,
    /// The owner removes the most recently added extra member, if any.
    RemoveExtra,
    /// The owner's app restarts.
    Restart,
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        Just(Step::SelfUpdate),
        Just(Step::Add),
        Just(Step::RemoveExtra),
        Just(Step::Restart),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(12))]

    /// Any sequence of commits and restarts between the request and the
    /// decision: the request is still listed, still approvable, and the
    /// joiner lands from the Welcome. Under ADR-0027 the same request was an
    /// external proposal framed at one epoch, and the first commit made it
    /// unprocessable.
    #[test]
    fn a_request_survives_any_number_of_commits(steps in prop::collection::vec(step(), 0..10)) {
        let mut owner = Identity::from_seed(&OWNER_SEED).expect("owner");
        let mut g = Group::create(&owner, GroupKind::Group).expect("create");
        let dave = id(0xD1);
        let first_epoch = g.epoch();

        let answer = g.retain_join_request(&owner, &kp_wire(&dave)).expect("retain");
        prop_assert_eq!(answer.outcome, JoinRequestOutcome::Retained);
        prop_assert_eq!(&answer.joiner_signature_key, &pk(&dave));

        let mut extras: Vec<Vec<u8>> = Vec::new();
        let mut next_tag = 0x20u8;
        for s in steps {
            match s {
                Step::SelfUpdate => {
                    g.self_update(&owner).expect("self update");
                },
                Step::Add => {
                    let extra = id(next_tag);
                    next_tag = next_tag.wrapping_add(1);
                    g.add_member(&owner, extra.create_key_package().expect("kp"))
                        .expect("add");
                    extras.push(pk(&extra));
                },
                Step::RemoveExtra => {
                    if let Some(key) = extras.pop() {
                        let leaf = g
                            .members()
                            .iter()
                            .find(|m| m.signature_public_key == key)
                            .expect("extra present")
                            .leaf_index;
                        g.remove_member(&owner, lakko_mls::LeafNodeIndex::new(leaf))
                            .expect("remove");
                    }
                },
                Step::Restart => {
                    (owner, g) = restart(owner, g);
                },
            }
            prop_assert_eq!(g.pending_external_joins(), vec![pk(&dave)]);
        }
        prop_assert!(g.epoch() >= first_epoch);
        approve_sweep_and_join(&owner, &mut g, &dave);
    }
}

// ---------------------------------------------------------------------------
// The cap holds
// ---------------------------------------------------------------------------

/// More distinct joiners than the cap, minted once: a KeyPackage validates
/// on its own, so the identities need not stay alive.
fn pool() -> &'static Vec<(Vec<u8>, Vec<u8>)> {
    static POOL: OnceLock<Vec<(Vec<u8>, Vec<u8>)>> = OnceLock::new();
    POOL.get_or_init(|| {
        (0..MAX_PENDING_JOIN_REQUESTS + 8)
            .map(|i| {
                let mut seed = [0x77u8; 64];
                seed[..8].copy_from_slice(&(i as u64).to_be_bytes());
                let who = Identity::from_seed(&seed).expect("joiner");
                (pk(&who), kp_wire(&who))
            })
            .collect()
    })
}

#[derive(Clone, Debug)]
enum CapOp {
    Retain(usize),
    Reject(usize),
    Restart,
}

fn cap_op() -> impl Strategy<Value = CapOp> {
    let n = MAX_PENDING_JOIN_REQUESTS + 8;
    prop_oneof![
        12 => (0..n).prop_map(CapOp::Retain),
        2 => (0..n).prop_map(CapOp::Reject),
        1 => Just(CapOp::Restart),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(8))]

    /// A model of the queue against the real one, over any interleaving of
    /// requests (new and repeated), rejections and restarts: the outcome of
    /// every request, the listed set and the room all match the model, and
    /// the listed set never exceeds the cap.
    #[test]
    fn the_cap_holds(ops in prop::collection::vec(cap_op(), 1..220)) {
        let mut owner = Identity::from_seed(&OWNER_SEED).expect("owner");
        let mut g = Group::create(&owner, GroupKind::Group).expect("create");
        let mut model: Vec<Vec<u8>> = Vec::new();
        for op in ops {
            match op {
                CapOp::Retain(i) => {
                    let (key, kp) = &pool()[i];
                    let answer = g.retain_join_request(&owner, kp).expect("retain");
                    let expected = if model.contains(key) {
                        JoinRequestOutcome::AlreadyPending
                    } else if model.len() >= MAX_PENDING_JOIN_REQUESTS {
                        JoinRequestOutcome::QueueFull
                    } else {
                        model.push(key.clone());
                        JoinRequestOutcome::Retained
                    };
                    prop_assert_eq!(answer.outcome, expected);
                    prop_assert_eq!(&answer.joiner_signature_key, key);
                },
                CapOp::Reject(i) => {
                    let key = &pool()[i].0;
                    let result = g.reject_external_join(&owner, key);
                    if let Some(pos) = model.iter().position(|k| k == key) {
                        prop_assert!(result.is_ok());
                        model.remove(pos);
                    } else {
                        prop_assert!(result.is_err());
                    }
                },
                CapOp::Restart => {
                    (owner, g) = restart(owner, g);
                },
            }
            prop_assert_eq!(&g.pending_external_joins(), &model);
            prop_assert!(model.len() <= MAX_PENDING_JOIN_REQUESTS);
            prop_assert_eq!(g.join_request_room(), MAX_PENDING_JOIN_REQUESTS - model.len());
        }
    }
}

/// The deterministic edge: the request past the cap is `QueueFull` and kept
/// nowhere; approving one frees a slot, and the same request is then kept.
#[test]
fn a_full_queue_refuses_until_a_request_is_decided() {
    let owner = Identity::from_seed(&OWNER_SEED).expect("owner");
    let mut g = Group::create(&owner, GroupKind::Group).expect("create");
    for (key, kp) in pool().iter().take(MAX_PENDING_JOIN_REQUESTS) {
        let answer = g.retain_join_request(&owner, kp).expect("retain");
        assert_eq!(answer.outcome, JoinRequestOutcome::Retained);
        assert_eq!(&answer.joiner_signature_key, key);
    }
    assert_eq!(g.join_request_room(), 0);
    let (late_key, late_kp) = &pool()[MAX_PENDING_JOIN_REQUESTS];
    let answer = g.retain_join_request(&owner, late_kp).expect("retain");
    assert_eq!(answer.outcome, JoinRequestOutcome::QueueFull);
    assert!(!answer.outcome.is_retained());
    assert!(!g.pending_external_joins().contains(late_key));

    let first = &pool()[0].0;
    g.approve_external_join(&owner, first).expect("approve");
    assert_eq!(g.join_request_room(), 1);
    let answer = g.retain_join_request(&owner, late_kp).expect("retain");
    assert_eq!(answer.outcome, JoinRequestOutcome::Retained);
    assert_eq!(g.join_request_room(), 0);
}

// ---------------------------------------------------------------------------
// A commit in a knock row does nothing
// ---------------------------------------------------------------------------

/// What a hostile or confused writer could put in a knock row instead of a
/// KeyPackage.
#[derive(Clone, Copy, Debug)]
enum Row {
    /// A real commit by a member, which the owner would apply from the journal.
    Commit,
    /// A member's application message.
    Application,
    /// An ADR-0027 external-join proposal (the old knock).
    ExternalProposal,
    /// A Welcome.
    Welcome,
    /// A valid KeyPackage with bytes after it.
    TrailingBytes,
    /// A valid KeyPackage cut short.
    Truncated,
}

fn row() -> impl Strategy<Value = Row> {
    prop_oneof![
        Just(Row::Commit),
        Just(Row::Application),
        Just(Row::ExternalProposal),
        Just(Row::Welcome),
        Just(Row::TrailingBytes),
        Just(Row::Truncated),
    ]
}

/// What the owner's group looks like from outside, before and after.
fn observe(g: &Group) -> (u64, Vec<Vec<u8>>, Vec<Vec<u8>>, usize) {
    (
        g.epoch(),
        g.members()
            .into_iter()
            .map(|m| m.signature_public_key)
            .collect(),
        g.pending_external_joins(),
        g.pending_proposals().len(),
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// Every kind of row that is not exactly one KeyPackage is refused with an
    /// error and changes nothing: epoch, roster, retained requests and the
    /// pending proposals are as they were. A commit refused here still applies
    /// through the journal afterwards, so the knock path neither applied nor
    /// consumed it.
    #[test]
    fn a_commit_in_a_knock_row_does_nothing(kind in row(), cut in 1usize..64) {
        let owner = Identity::from_seed(&OWNER_SEED).expect("owner");
        let mut g = Group::create(&owner, GroupKind::Group).expect("create");
        let bob = id(0xB2);
        let added = g
            .add_member(&owner, bob.create_key_package().expect("kp"))
            .expect("add bob");
        let mut bob_g = Group::join_from_welcome(&wire(&added.welcome), &bob).expect("bob joins");
        // One request already waiting, so "nothing changed" covers the queue.
        let carol = id(0xC3);
        g.retain_join_request(&owner, &kp_wire(&carol)).expect("carol");

        let mut commit = None;
        let bytes = match kind {
            Row::Commit => {
                let c = bob_g.self_update(&bob).expect("bob commits");
                commit = Some(c.clone());
                c
            },
            Row::Application => wire(
                &bob_g
                    .create_application_message(&bob, b"hello")
                    .expect("message"),
            ),
            Row::ExternalProposal => id(0xE4)
                .create_external_join_proposal(&g.group_id(), g.epoch())
                .expect("external proposal"),
            Row::Welcome => {
                let mut other = Group::create(&bob, GroupKind::Group).expect("other");
                let a = other
                    .add_member(&bob, id(0xF5).create_key_package().expect("kp"))
                    .expect("add");
                wire(&a.welcome)
            },
            Row::TrailingBytes => {
                let mut b = kp_wire(&id(0xA6));
                b.push(0);
                b
            },
            Row::Truncated => {
                let b = kp_wire(&id(0xA7));
                b[..b.len().saturating_sub(cut)].to_vec()
            },
        };

        let before = observe(&g);
        prop_assert!(g.retain_join_request(&owner, &bytes).is_err());
        prop_assert_eq!(observe(&g), before.clone());

        if let Some(c) = commit {
            g.process_incoming_verifying_roles(&owner, &c)
                .expect("the commit still applies from the journal");
            prop_assert_eq!(g.epoch(), before.0 + 1);
            prop_assert_eq!(g.pending_external_joins(), vec![pk(&carol)]);
        }
    }

    /// Arbitrary bytes never panic and never change the group.
    #[test]
    fn arbitrary_bytes_in_a_knock_row_change_nothing(bytes in prop::collection::vec(any::<u8>(), 0..1024)) {
        let owner = Identity::from_seed(&OWNER_SEED).expect("owner");
        let mut g = Group::create(&owner, GroupKind::Group).expect("create");
        let before = observe(&g);
        let result = g.retain_join_request(&owner, &bytes);
        prop_assert!(result.is_err());
        prop_assert_eq!(observe(&g), before);
    }
}

// ---------------------------------------------------------------------------
// Admission
// ---------------------------------------------------------------------------

#[test]
fn a_one_to_one_takes_no_request() {
    let owner = Identity::from_seed(&OWNER_SEED).expect("owner");
    let mut g = Group::create(&owner, GroupKind::OneToOne).expect("create");
    let answer = g
        .retain_join_request(&owner, &kp_wire(&id(0x31)))
        .expect("retain");
    assert_eq!(answer.outcome, JoinRequestOutcome::NotAdmissible);
    assert!(g.pending_external_joins().is_empty());
}

/// A leaf whose credential is neither empty nor a valid ADR-0050 binding
/// resolves to no account and is never kept.
#[test]
fn a_credential_that_resolves_to_no_account_is_not_admissible() {
    let owner = Identity::from_seed(&OWNER_SEED).expect("owner");
    let mut g = Group::create(&owner, GroupKind::Group).expect("create");
    let mallory = id(0x32);
    let kp = mallory
        .adversary_key_package_with_credential(vec![0x42; 40])
        .expect("adversary kp");
    let answer = g
        .retain_join_request(&owner, &key_package::serialize(&kp).expect("serialize"))
        .expect("retain");
    assert_eq!(answer.outcome, JoinRequestOutcome::NotAdmissible);
    assert!(g.pending_external_joins().is_empty());
}

/// A device bound to its account (ADR-0050) is kept, the request names the
/// account, and the device lands.
#[test]
fn a_bound_device_is_kept_and_named_by_its_account() {
    let owner = Identity::from_seed(&OWNER_SEED).expect("owner");
    let mut g = Group::create(&owner, GroupKind::Group).expect("create");
    let account = id(0x33);
    let device =
        Identity::from_seed_bound(&[0x34; 32], &account.ed25519_seed_bytes(), 1_700_000_000)
            .expect("bound device");
    let answer = g
        .retain_join_request(&owner, &kp_wire(&device))
        .expect("retain");
    assert_eq!(answer.outcome, JoinRequestOutcome::Retained);
    assert_eq!(
        g.pending_external_join_account_key(&pk(&device))
            .map(|k| k.to_vec()),
        account.account_public_key().map(|k| k.to_vec()),
    );
    approve_sweep_and_join(&owner, &mut g, &device);
}

/// Asking twice keeps one request; asking as a member keeps none.
#[test]
fn a_repeated_request_and_a_member_s_request_keep_nothing_new() {
    let owner = Identity::from_seed(&OWNER_SEED).expect("owner");
    let mut g = Group::create(&owner, GroupKind::Group).expect("create");
    let dave = id(0x35);
    assert_eq!(
        g.retain_join_request(&owner, &kp_wire(&dave))
            .expect("retain")
            .outcome,
        JoinRequestOutcome::Retained
    );
    let again = g
        .retain_join_request(&owner, &kp_wire(&dave))
        .expect("retain again");
    assert_eq!(again.outcome, JoinRequestOutcome::AlreadyPending);
    assert!(again.outcome.is_retained());
    assert_eq!(g.pending_external_joins(), vec![pk(&dave)]);

    let answer = g
        .retain_join_request(&owner, &kp_wire(&owner))
        .expect("the owner asks");
    assert_eq!(answer.outcome, JoinRequestOutcome::AlreadyMember);
    assert_eq!(g.pending_external_joins(), vec![pk(&dave)]);
}

/// Another committer admits the joiner; the retained copy on this device is
/// not listed any more, and a new request from that joiner is `AlreadyMember`.
#[test]
fn a_joiner_admitted_elsewhere_leaves_the_queue() {
    let owner = Identity::from_seed(&OWNER_SEED).expect("owner");
    let mut g = Group::create(&owner, GroupKind::Group).expect("create");
    let dave = id(0x36);
    g.retain_join_request(&owner, &kp_wire(&dave))
        .expect("retain");
    // Added by a direct path, as another device's sweep would land it.
    g.add_member(&owner, dave.create_key_package().expect("kp"))
        .expect("add dave");
    assert!(g.pending_external_joins().is_empty());
    assert_eq!(g.join_request_room(), MAX_PENDING_JOIN_REQUESTS);
    let answer = g
        .retain_join_request(&owner, &kp_wire(&dave))
        .expect("retain");
    assert_eq!(answer.outcome, JoinRequestOutcome::AlreadyMember);
}

// ---------------------------------------------------------------------------
// The publisher path
// ---------------------------------------------------------------------------

/// A co-publisher's request rides the same row and is approved by the
/// PublisherGroup's direct commit; both publishers then derive one seed, at
/// an epoch the request did not name.
#[test]
fn a_co_publisher_joins_from_a_retained_request() {
    let owner_account = [0xA7u8; 32];
    let alice = Identity::from_seed_bound(&OWNER_SEED, &owner_account, 1).expect("alice");
    let channel =
        ChannelId::try_from_slice(&[0x5Cu8; lakko_mls::CHANNEL_ID_LEN]).expect("channel id");
    let mut alice_pg =
        PublisherGroup::create(&alice, &owner_account, &channel).expect("publisher group");
    let bob = Identity::from_seed_bound(&[0x37; 64], &[0xB7; 32], 1).expect("bob");

    let answer = alice_pg
        .retain_join_request(&alice, &kp_wire(&bob))
        .expect("retain");
    assert_eq!(answer.outcome, JoinRequestOutcome::Retained);
    // A rotation between the request and the decision.
    alice_pg.stage_rotate(&alice).expect("rotate");
    assert!(alice_pg.merge_staged_commit(&alice).expect("merge"));

    let addition = alice_pg
        .stage_approve_external_join(&alice, &pk(&bob))
        .expect("approve");
    assert!(alice_pg.merge_staged_commit(&alice).expect("merge"));
    let bob_pg = PublisherGroup::join_from_welcome(
        &bob,
        &addition.welcome,
        &alice_pg.group_id(),
        &channel,
        &lakko_mls::publisher_owner_account_key(&owner_account),
    )
    .expect("bob joins");
    assert_eq!(
        bob_pg.channel_seed(&bob).expect("bob's seed"),
        alice_pg.channel_seed(&alice).expect("alice's seed"),
    );
    assert!(alice_pg.pending_external_joins().is_empty());
}

// ---------------------------------------------------------------------------
// A request sent while leaving
// ---------------------------------------------------------------------------

/// The owner's group with `dave` admitted through a proposal and a sweep, and
/// Dave's own handle on it.
fn group_with_member(owner: &Identity, dave: &Identity) -> (Group, Group) {
    let mut g = Group::create(owner, GroupKind::Group).expect("create");
    g.propose_add_member_from_wire(owner, &kp_wire(dave))
        .expect("owner proposes the add");
    let sweep = g
        .commit_pending_proposals(owner)
        .expect("sweep")
        .expect("a commit was produced");
    let welcome = wire(&sweep.welcome.expect("the sweep carries the Welcome"));
    let dave_g = Group::join_from_welcome(&welcome, dave).expect("dave joins");
    (g, dave_g)
}

fn leaf_of(g: &Group, who: &Identity) -> lakko_mls::LeafNodeIndex {
    let key = pk(who);
    lakko_mls::LeafNodeIndex::new(
        g.members()
            .iter()
            .find(|m| m.signature_public_key == key)
            .expect("in the roster")
            .leaf_index,
    )
}

/// Seen live 2026-11-23: a member asks to leave and opens the group's link
/// again before an admin swept the leave. The request reaches the admin's
/// device while the member is still in its roster; it must be kept, hidden
/// while the member is in, listed once the leave lands, and approvable.
/// Before, it was answered `AlreadyMember`, and the knock reader moved past
/// the row: the request was gone for good.
#[test]
fn a_request_from_a_member_whose_leave_waits_is_kept_and_listed_once_they_are_out() {
    let owner = Identity::from_seed(&OWNER_SEED).expect("owner");
    let dave = id(0xD2);
    let (mut g, mut dave_g) = group_with_member(&owner, &dave);

    let leave = dave_g
        .propose_remove_member(&dave, leaf_of(&dave_g, &dave))
        .expect("a member may ask to leave");
    g.process_incoming_verifying_roles(&owner, &leave)
        .expect("the owner stores the leave");

    let answer = g
        .retain_join_request(&owner, &kp_wire(&dave))
        .expect("retain");
    assert_eq!(answer.outcome, JoinRequestOutcome::Retained);
    // Still a member: not listed, so nobody approves an Add of a current leaf.
    assert!(g.pending_external_joins().is_empty());

    // Another stranger's knock arrives before the sweep: the reap it runs
    // leaves Dave's request alone.
    let erin = id(0xE2);
    let other = g
        .retain_join_request(&owner, &kp_wire(&erin))
        .expect("retain");
    assert_eq!(other.outcome, JoinRequestOutcome::Retained);

    // The owner sweeps the leave: Dave is out, and his request is listed.
    let sweep = g
        .commit_pending_proposals(&owner)
        .expect("sweep")
        .expect("the leave is swept");
    // Dave's own device applies its removal, which ends the group there.
    dave_g
        .process_incoming_verifying_roles(&dave, &wire(&sweep.commit))
        .expect("dave applies the commit that removes him");
    assert!(!dave_g.is_active());
    assert!(!g
        .members()
        .iter()
        .any(|m| m.signature_public_key == pk(&dave)));
    let listed = g.pending_external_joins();
    assert!(listed.contains(&pk(&dave)), "Dave's request is listed");
    assert!(listed.contains(&pk(&erin)));

    // It survives a restart and is approved like any other.
    let (owner, mut g) = restart(owner, g);
    assert!(g.pending_external_joins().contains(&pk(&dave)));
    approve_sweep_and_join(&owner, &mut g, &dave);
}

/// The control: a member with no removal waiting is answered `AlreadyMember`
/// and nothing is kept.
#[test]
fn a_request_from_a_staying_member_is_not_kept() {
    let owner = Identity::from_seed(&OWNER_SEED).expect("owner");
    let dave = id(0xD3);
    let (mut g, _dave_g) = group_with_member(&owner, &dave);

    let answer = g
        .retain_join_request(&owner, &kp_wire(&dave))
        .expect("retain");
    assert_eq!(answer.outcome, JoinRequestOutcome::AlreadyMember);
    assert!(g.pending_external_joins().is_empty());
    assert_eq!(g.join_request_room(), MAX_PENDING_JOIN_REQUESTS);
}
