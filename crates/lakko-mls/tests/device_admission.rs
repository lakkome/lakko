// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0050 section 2.5a — a device of a present member needs no admin.
//!
//! A **device admission** is an `Add` whose leaf binding resolves to an
//! account that already holds a leaf in the group. Any member may propose one
//! (section 2.4); since 2.5a any member's device may also **sweep** it, the
//! member's account ranked after the owner and the admins in bytewise
//! account-key order (every leaf of an account holds its rank, item 6). A
//! non-admin sweeper carries only device admissions and its own account's
//! self-service proposals; everything else is left for an owner or admin and is
//! dropped by the merge. Peers judge every by-reference proposal against its
//! own proposer, never the committer, so they accept such a commit unchanged.
//!
//! These tests drive the public API only and pin: the restored-owner 1x1, the
//! member scope of a mixed set, the refusal with nothing in scope, rank
//! agreement across every view (and its independence from leaf indices), an
//! admin still sweeping the whole set, a new device of the owner sweeping
//! while a lost lower-keyed leaf remains (item 6), and an owner on a bound
//! device changing roles (item 10).

use lakko_mls::{
    account_committer_rank, group, key_package, Group, GroupKind, Identity, Incoming,
    LeafNodeIndex, OutgoingOwner, ProposalKind, RefusalReason, Role, RoleTable,
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

fn account_key(id: &Identity) -> [u8; 32] {
    id.account_public_key().expect("32-byte key")
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

fn is_member(g: &Group, id: &Identity) -> bool {
    g.members()
        .iter()
        .any(|m| m.signature_public_key == id.signature_public_key())
}

/// Deliver `bytes` to every group in `peers`.
fn deliver(bytes: &[u8], peers: &mut [(&Identity, &mut Group)]) {
    for (id, g) in peers.iter_mut() {
        g.process_incoming_verifying_roles(id, bytes)
            .expect("peer applies");
    }
}

/// Deliver a proposal and assert every peer stored it.
fn deliver_stored(bytes: &[u8], peers: &mut [(&Identity, &mut Group)]) {
    for (id, g) in peers.iter_mut() {
        let got = g
            .process_incoming_verifying_roles(id, bytes)
            .expect("peer processes the proposal");
        let Incoming::Proposal(record) = got else {
            panic!("expected a proposal, got {got:?}");
        };
        assert!(record.stored, "every peer retains the proposal");
    }
}

/// The founder `creator` adds every identity in `joiners` one direct commit at
/// a time, delivering each commit to the members already in. Returns the
/// founder's group and each joiner's group, in order.
fn found(creator: &Identity, joiners: &[&Identity]) -> (Group, Vec<Group>) {
    let mut g = Group::create(creator, GroupKind::Group).expect("create");
    let mut others: Vec<Group> = Vec::new();
    for joiner in joiners {
        let added = g
            .add_member_from_wire(creator, &kp_wire(joiner))
            .expect("founder adds");
        let commit = wire(added.commit);
        for (i, og) in others.iter_mut().enumerate() {
            og.process_incoming_verifying_roles(joiners[i], &commit)
                .expect("member applies the add");
        }
        others.push(Group::join_from_welcome(&wire(added.welcome), joiner).expect("joins"));
    }
    (g, others)
}

/// The expected 2.5a rank of `account` given the admins count and every plain
/// member account, computed independently of the crate: `n + 1 + i` over the
/// plain accounts sorted bytewise.
fn expected_member_rank(admins: u32, plain: &[[u8; 32]], account: &[u8; 32]) -> u32 {
    let mut sorted = plain.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let i = sorted
        .iter()
        .position(|a| a == account)
        .expect("listed as plain");
    admins + 1 + u32::try_from(i).expect("small")
}

// ============================================================================
// 1. The restored owner of a 1x1
// ============================================================================

/// The owner of a 1x1 loses its only device and restores onto a new one bound
/// to the same account. The other, plain member proposes the device admission
/// and sweeps it on its own — nobody else is online. The commit verifies on a
/// peer view, the new device joins from the Welcome, and both directions of
/// chat work.
#[test]
fn a_plain_member_readmits_the_restored_owner_of_a_one_to_one() {
    let a = account(0x11);
    let a1 = device(&a, 0x12);
    let b = account(0x13);
    let b1 = device(&b, 0x14);
    let (mut a1_g, mut joined) = found(&a1, &[&b1]);
    let mut b1_g = joined.remove(0);

    // Bob is a plain member and nothing is pending: no rank, no sweep.
    assert_eq!(b1_g.own_role(&b1), Some(Role::Member));
    assert_eq!(b1_g.own_committer_rank(&b1), None);

    // Alice's only device is lost; her account binds a2. Bob re-admits it.
    let a2 = device(&a, 0x15);
    let proposal = b1_g
        .propose_add_member_from_wire(&b1, &kp_wire(&a2))
        .expect("a member proposes a device of a present account");
    // a1's state stands in for "every peer" (in a 1x1 the only peer is the lost
    // device; here it still verifies what the network would deliver).
    deliver_stored(&proposal, &mut [(&a1, &mut a1_g)]);

    // Bob now ranks after the (zero) admins: 1, and every view agrees.
    assert_eq!(b1_g.own_committer_rank(&b1), Some(1));
    assert_eq!(a1_g.committer_rank_of(b1.signature_public_key()), Some(1));

    let sweep = b1_g
        .stage_pending_proposals(&b1)
        .expect("a plain member sweeps a device admission")
        .expect("carried");
    assert_eq!(sweep.swept.len(), 1);
    assert!(matches!(
        &sweep.swept[0].kind,
        ProposalKind::Add { account_key: Some(k), .. } if k.as_slice() == account_key(&a)
    ));
    let commit = wire(sweep.commit);
    // The peer-verified commit gate accepts a plain member's sweep: the Add is
    // judged against its proposer (bob, "already a member" arm).
    deliver(&commit, &mut [(&a1, &mut a1_g)]);
    assert!(b1_g.merge_staged_sweep(&b1).expect("merge"));
    assert_eq!(a1_g.epoch(), b1_g.epoch());
    assert!(b1_g.pending_proposals().is_empty());
    assert!(a1_g.pending_proposals().is_empty());

    let mut a2_g =
        Group::join_from_welcome(&wire(sweep.welcome.expect("welcome")), &a2).expect("a2 joins");
    assert_eq!(a2_g.epoch(), b1_g.epoch());
    assert_eq!(a2_g.own_role(&a2), Some(Role::Owner), "still alice's group");

    // Both directions.
    let to_alice = wire(
        b1_g.create_application_message(&b1, b"welcome back")
            .expect("bob seals"),
    );
    match a2_g
        .process_incoming_verifying_roles(&a2, &to_alice)
        .expect("a2 opens")
    {
        Incoming::Application { bytes, .. } => assert_eq!(bytes, b"welcome back"),
        other => panic!("expected application, got {other:?}"),
    }
    let to_bob = wire(
        a2_g.create_application_message(&a2, b"thanks")
            .expect("a2 seals"),
    );
    match b1_g
        .process_incoming_verifying_roles(&b1, &to_bob)
        .expect("bob opens")
    {
        Incoming::Application {
            bytes,
            sender_account,
            ..
        } => {
            assert_eq!(bytes, b"thanks");
            assert_eq!(sender_account, Some(account_key(&a).to_vec()));
        },
        other => panic!("expected application, got {other:?}"),
    }
}

// ============================================================================
// 2 + 5. A mixed pending set: a member leader vs an admin leader
// ============================================================================

/// The cast of the mixed-set tests: owner `a1`, admin `x1`, plain members
/// `b1`, `c1`, `d1`; each a device of its own account.
struct Mixed {
    a: Identity,
    a1: Identity,
    x1: Identity,
    b: Identity,
    b1: Identity,
    c1: Identity,
    d1: Identity,
    a1_g: Group,
    x1_g: Group,
    b1_g: Group,
    c1_g: Group,
    d1_g: Group,
}

fn mixed(tag: u8) -> Mixed {
    // Every member is a bound device of its own account, the owner and the
    // admin included (ADR-0050 section 2.5a item 10: the role-table paths
    // compare the device's ACCOUNT with the table).
    let alice = account(0x20 ^ tag);
    let a1 = device(&alice, 0x21 ^ tag);
    let xavier = account(0x22 ^ tag);
    let x1 = device(&xavier, 0x23 ^ tag);
    let bob = account(0x24 ^ tag);
    let b1 = device(&bob, 0x25 ^ tag);
    let carol = account(0x26 ^ tag);
    let c1 = device(&carol, 0x27 ^ tag);
    let dave = account(0x28 ^ tag);
    let d1 = device(&dave, 0x29 ^ tag);
    let (mut a1_g, mut joined) = found(&a1, &[&x1, &b1, &c1, &d1]);
    let mut d1_g = joined.pop().expect("d1");
    let mut c1_g = joined.pop().expect("c1");
    let mut b1_g = joined.pop().expect("b1");
    let mut x1_g = joined.pop().expect("x1");
    let proposal = a1_g
        .propose_group_roles(
            &a1,
            &RoleTable::new(account_key(&alice), vec![account_key(&xavier)]),
        )
        .expect("owner proposes promoting x");
    deliver(
        &proposal,
        &mut [
            (&x1, &mut x1_g),
            (&b1, &mut b1_g),
            (&c1, &mut c1_g),
            (&d1, &mut d1_g),
        ],
    );
    let promote = wire(
        a1_g.commit_pending_proposals(&a1)
            .expect("owner sweeps")
            .expect("carried")
            .commit,
    );
    deliver(
        &promote,
        &mut [
            (&x1, &mut x1_g),
            (&b1, &mut b1_g),
            (&c1, &mut c1_g),
            (&d1, &mut d1_g),
        ],
    );
    assert_eq!(x1_g.own_committer_rank(&x1), Some(1));
    Mixed {
        a: alice,
        a1,
        x1,
        b: bob,
        b1,
        c1,
        d1,
        a1_g,
        x1_g,
        b1_g,
        c1_g,
        d1_g,
    }
}

/// Five proposals at one epoch, delivered everywhere:
/// 1. c1 re-admits alice's new device `a2` (device admission),
/// 2. b1 adds its own second device `b2` (self-service, also an admission),
/// 3. the owner adds a stranger `e`,
/// 4. the owner removes `d1`,
/// 5. the owner promotes c to admin.
///
/// Returns `(a2, b2, e)`.
fn propose_mixed(m: &mut Mixed, tag: u8) -> (Identity, Identity, Identity) {
    let a2 = device(&m.a, 0x30 ^ tag);
    let b2 = device(&m.b, 0x31 ^ tag);
    let e = account(0x32 ^ tag);

    let to_admit_alice = m
        .c1_g
        .propose_add_member_from_wire(&m.c1, &kp_wire(&a2))
        .expect("c1 re-admits a2");
    let to_admit_own = m
        .b1_g
        .propose_add_member_from_wire(&m.b1, &kp_wire(&b2))
        .expect("b1 adds its own device");
    let p_e = m
        .a1_g
        .propose_add_member_from_wire(&m.a1, &kp_wire(&e))
        .expect("owner adds a stranger");
    let d_leaf = leaf_of(&m.a1_g, &m.d1);
    let p_rm = m
        .a1_g
        .propose_remove_member(&m.a1, d_leaf)
        .expect("owner removes d");
    let c_account = m.c1.account_public_key().expect("c1 bound to an account");
    let x_account = m.x1.account_public_key().expect("x1 bound");
    let p_role = m
        .a1_g
        .propose_group_roles(
            &m.a1,
            &RoleTable::new(account_key(&m.a), vec![x_account, c_account]),
        )
        .expect("owner promotes c");

    // Everyone stores everyone else's proposals.
    deliver_stored(
        &to_admit_alice,
        &mut [
            (&m.a1, &mut m.a1_g),
            (&m.x1, &mut m.x1_g),
            (&m.b1, &mut m.b1_g),
            (&m.d1, &mut m.d1_g),
        ],
    );
    deliver_stored(
        &to_admit_own,
        &mut [
            (&m.a1, &mut m.a1_g),
            (&m.x1, &mut m.x1_g),
            (&m.c1, &mut m.c1_g),
            (&m.d1, &mut m.d1_g),
        ],
    );
    for p in [&p_e, &p_rm, &p_role] {
        deliver_stored(
            p,
            &mut [
                (&m.x1, &mut m.x1_g),
                (&m.b1, &mut m.b1_g),
                (&m.c1, &mut m.c1_g),
                (&m.d1, &mut m.d1_g),
            ],
        );
    }
    for g in [&m.a1_g, &m.x1_g, &m.b1_g, &m.c1_g, &m.d1_g] {
        assert_eq!(g.pending_proposals().len(), 5, "one shared pending set");
    }
    (a2, b2, e)
}

/// A plain-member leader commits only the device admissions (and its own
/// account's self-service); the owner's stranger add, removal and role change
/// are not in the commit, every peer merges it, and after the merge the left
/// out proposals are gone from every store.
#[test]
fn a_plain_member_leader_commits_only_device_admissions_out_of_a_mixed_set() {
    let mut m = mixed(0x01);
    let (a2, b2, e) = propose_mixed(&mut m, 0x01);

    // Every plain member holds a device admission, so each has its rank.
    for (id, g) in [(&m.b1, &m.b1_g), (&m.c1, &m.c1_g), (&m.d1, &m.d1_g)] {
        assert!(g.own_committer_rank(id).is_some_and(|r| r >= 2));
    }

    let sweep = m
        .b1_g
        .stage_pending_proposals(&m.b1)
        .expect("b1 sweeps its scope")
        .expect("carried");
    let mut added: Vec<Vec<u8>> = sweep
        .swept
        .iter()
        .map(|r| match &r.kind {
            ProposalKind::Add { signature_key, .. } => signature_key.clone(),
            other => panic!("a member sweep carried a non-admission: {other:?}"),
        })
        .collect();
    added.sort();
    let mut want = vec![
        a2.signature_public_key().to_vec(),
        b2.signature_public_key().to_vec(),
    ];
    want.sort();
    assert_eq!(added, want, "exactly the two device admissions");

    let commit = wire(sweep.commit);
    deliver(
        &commit,
        &mut [
            (&m.a1, &mut m.a1_g),
            (&m.x1, &mut m.x1_g),
            (&m.c1, &mut m.c1_g),
            (&m.d1, &mut m.d1_g),
        ],
    );
    assert!(m.b1_g.merge_staged_sweep(&m.b1).expect("merge"));
    let welcome = wire(sweep.welcome.expect("welcome"));
    let a2_g = Group::join_from_welcome(&welcome, &a2).expect("a2 joins");
    let b2_g = Group::join_from_welcome(&welcome, &b2).expect("b2 joins");

    for g in [&m.a1_g, &m.x1_g, &m.b1_g, &m.c1_g, &m.d1_g, &a2_g, &b2_g] {
        assert!(
            g.pending_proposals().is_empty(),
            "left-out proposals dropped"
        );
        assert_eq!(g.epoch(), m.b1_g.epoch());
        assert!(is_member(g, &a2) && is_member(g, &b2));
        assert!(!is_member(g, &e), "the stranger was not admitted");
        assert!(is_member(g, &m.d1), "the removal was not carried");
        let table = g.group_roles().expect("role table");
        assert_eq!(table.admins().len(), 1, "the role change was not carried");
    }
}

/// An admin leader with the same mixed set sweeps the whole of it.
#[test]
fn an_admin_leader_still_sweeps_the_whole_mixed_set() {
    let mut m = mixed(0x02);
    let (a2, b2, e) = propose_mixed(&mut m, 0x02);
    let sweep = m
        .x1_g
        .stage_pending_proposals(&m.x1)
        .expect("admin sweeps")
        .expect("carried");
    assert_eq!(sweep.swept.len(), 5, "the whole authorised set");
    let commit = wire(sweep.commit);
    deliver(
        &commit,
        &mut [
            (&m.a1, &mut m.a1_g),
            (&m.b1, &mut m.b1_g),
            (&m.c1, &mut m.c1_g),
        ],
    );
    assert!(m.x1_g.merge_staged_sweep(&m.x1).expect("merge"));
    for g in [&m.a1_g, &m.x1_g, &m.b1_g, &m.c1_g] {
        assert!(g.pending_proposals().is_empty());
        assert!(is_member(g, &a2) && is_member(g, &b2) && is_member(g, &e));
        assert!(!is_member(g, &m.d1));
        assert_eq!(g.group_roles().expect("table").admins().len(), 2);
    }
}

// ============================================================================
// 3. Nothing in scope
// ============================================================================

/// A plain member with no device admission pending has no rank and is refused
/// `NotACommitter` — whether the store is empty or holds only owner/admin work.
#[test]
fn a_plain_member_without_a_device_admission_cannot_sweep() {
    let mut m = mixed(0x03);
    assert_eq!(m.b1_g.own_committer_rank(&m.b1), None);
    let Err(err) = m.b1_g.stage_pending_proposals(&m.b1) else {
        panic!("nothing pending, not a committer");
    };
    assert_eq!(err.refusal_reason(), Some(RefusalReason::NotACommitter));

    // Only the owner's add of a stranger pending: still out of scope.
    let e = account(0x77);
    let p = m
        .a1_g
        .propose_add_member_from_wire(&m.a1, &kp_wire(&e))
        .expect("owner adds a stranger");
    deliver_stored(&p, &mut [(&m.b1, &mut m.b1_g)]);
    assert_eq!(m.b1_g.own_committer_rank(&m.b1), None);
    let Err(err) = m.b1_g.stage_pending_proposals(&m.b1) else {
        panic!("a stranger's add is owner/admin work");
    };
    assert_eq!(err.refusal_reason(), Some(RefusalReason::NotACommitter));
    assert!(!m.b1_g.has_staged_sweep());
}

// ============================================================================
// 4. Ranks are one function of the roster and the table
// ============================================================================

/// The plain (non-owner, non-admin) member accounts of a [`Mixed`] scene.
fn plain_accounts(m: &Mixed) -> Vec<[u8; 32]> {
    [&m.b1, &m.c1, &m.d1]
        .iter()
        .map(|id| id.account_public_key().expect("bound"))
        .collect()
}

/// Every view ranks every acting leaf identically; the ranks are owner 0,
/// admin 1, then the plain accounts in key order, a permutation of 0..=4.
/// Returns the ranks of `[a1, x1, b1, c1, d1]`.
fn check_ranks(m: &Mixed, plain: &[[u8; 32]]) -> Vec<u32> {
    let views = [&m.a1_g, &m.x1_g, &m.b1_g, &m.c1_g, &m.d1_g];
    let leaves = [&m.a1, &m.x1, &m.b1, &m.c1, &m.d1];
    let mut ranks = Vec::new();
    for leaf in leaves {
        let r = views[0]
            .committer_rank_of(leaf.signature_public_key())
            .expect("every acting leaf is ranked");
        for v in &views[1..] {
            assert_eq!(v.committer_rank_of(leaf.signature_public_key()), Some(r));
        }
        ranks.push(r);
    }
    assert_eq!(&ranks[..2], &[0, 1], "owner then admin");
    for (i, id) in [&m.b1, &m.c1, &m.d1].iter().enumerate() {
        let acc = id.account_public_key().expect("bound");
        assert_eq!(ranks[2 + i], expected_member_rank(1, plain, &acc));
    }
    let mut sorted = ranks.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, vec![0, 1, 2, 3, 4], "a permutation, no ties");
    ranks
}

/// Every member computes the same rank for every account; a plain member's own
/// rank (while it holds a device admission) equals what every other view ranks
/// its leaf; the ranks are a permutation starting after the admins; and the
/// pure function agrees.
#[test]
fn ranks_agree_across_every_view() {
    let mut m = mixed(0x04);
    let plain = plain_accounts(&m);
    let before = check_ranks(&m, &plain);

    // Own rank of a plain member, while it holds a device admission, matches
    // every other view of its leaf.
    let (_a2, _b2, _e) = propose_mixed(&mut m, 0x04);
    for (id, g) in [(&m.b1, &m.b1_g), (&m.c1, &m.c1_g), (&m.d1, &m.d1_g)] {
        let own = g.own_committer_rank(id).expect("holds an admission");
        assert_eq!(
            m.a1_g.committer_rank_of(id.signature_public_key()),
            Some(own)
        );
    }
    // And the pure function says the same.
    let table = m.a1_g.group_roles().expect("table");
    let roster = m.a1_g.members();
    for (id, r) in [&m.b1, &m.c1, &m.d1].iter().zip(&before[2..]) {
        assert_eq!(
            account_committer_rank(
                &table,
                roster.iter().map(|mr| mr.account_key.as_slice()),
                &id.account_public_key().expect("bound"),
            ),
            Some(*r)
        );
    }
}

/// A vacated leaf handed to the same account's new device leaves that
/// account's rank where it was (ranks key on account keys, never leaf indices).
#[test]
fn ranks_do_not_move_when_a_leaf_is_vacated_and_reused() {
    let mut m = mixed(0x05);
    let plain = plain_accounts(&m);
    let before = check_ranks(&m, &plain);
    // The owner removes c1 and admits c's new device c2 directly; OpenMLS
    // hands c2 the vacated leaf. c's account keeps its rank.
    let c = m.c1.account_public_key().expect("bound");
    let c_leaf = leaf_of(&m.a1_g, &m.c1);
    let rm = wire(
        m.a1_g
            .remove_member(&m.a1, c_leaf)
            .expect("owner removes c1"),
    );
    deliver(
        &rm,
        &mut [
            (&m.x1, &mut m.x1_g),
            (&m.b1, &mut m.b1_g),
            (&m.d1, &mut m.d1_g),
        ],
    );
    assert_eq!(m.a1_g.committer_rank_of(m.c1.signature_public_key()), None);
    // c's account is gone from the roster, so the remaining plain accounts
    // renumber by key order — the same function, a smaller roster.
    let without_c: Vec<[u8; 32]> = plain.iter().copied().filter(|a| *a != c).collect();
    for id in [&m.b1, &m.d1] {
        let acc = id.account_public_key().expect("bound");
        assert_eq!(
            m.x1_g.committer_rank_of(id.signature_public_key()),
            Some(expected_member_rank(1, &without_c, &acc))
        );
    }
    let c_account_identity = account(0x26 ^ 0x05);
    let c2 = device(&c_account_identity, 0x5C);
    let added = m
        .a1_g
        .add_member_from_wire(&m.a1, &kp_wire(&c2))
        .expect("owner adds c2");
    deliver(
        &wire(added.commit),
        &mut [
            (&m.x1, &mut m.x1_g),
            (&m.b1, &mut m.b1_g),
            (&m.d1, &mut m.d1_g),
        ],
    );
    let c2_g = Group::join_from_welcome(&wire(added.welcome), &c2).expect("c2 joins");
    assert_eq!(
        leaf_of(&m.a1_g, &c2),
        c_leaf,
        "the vacated leaf is reused (the case the rank must not depend on)"
    );
    for v in [&m.a1_g, &m.x1_g, &m.b1_g, &m.d1_g, &c2_g] {
        assert_eq!(
            v.committer_rank_of(c2.signature_public_key()),
            Some(before[3]),
            "c's account holds the rank it held before, on a reused leaf"
        );
        for (id, r) in [(&m.b1, before[2]), (&m.d1, before[4])] {
            assert_eq!(v.committer_rank_of(id.signature_public_key()), Some(r));
        }
    }
}

// ============================================================================
// 6. Item 6 — every leaf of a ranked account holds the rank
// ============================================================================

/// Two devices of `owner` whose leaf keys sort `(lower, higher)`.
fn two_devices_sorted(owner: &Identity, t1: u8, t2: u8) -> (Identity, Identity) {
    let d1 = device(owner, t1);
    let d2 = device(owner, t2);
    if d1.signature_public_key() < d2.signature_public_key() {
        (d1, d2)
    } else {
        (d2, d1)
    }
}

/// ADR-0050 section 2.5a item 6. The owner of a 1x1 loses its device, whose
/// leaf stays in the roster and whose key sorts LOWER than the account's new
/// device. Before item 6 the new device never ranked (only the account's
/// lowest leaf did) and so could never sweep. Now it holds the owner's rank 0
/// from every view, stages and sweeps a later proposal, and every peer merges.
#[test]
fn a_new_device_of_the_owner_sweeps_although_a_lost_lower_keyed_leaf_remains() {
    let a = account(0x41);
    let (a_lost, a_new) = two_devices_sorted(&a, 0x42, 0x43);
    assert!(a_lost.signature_public_key() < a_new.signature_public_key());
    let b = account(0x44);
    let b1 = device(&b, 0x45);
    let (mut lost_g, mut joined) = found(&a_lost, &[&b1]);
    let mut b1_g = joined.remove(0);

    // Bob re-admits alice's new device (a device admission, swept by bob).
    let p = b1_g
        .propose_add_member_from_wire(&b1, &kp_wire(&a_new))
        .expect("bob proposes alice's new device");
    deliver_stored(&p, &mut [(&a_lost, &mut lost_g)]);
    let sweep = b1_g
        .stage_pending_proposals(&b1)
        .expect("bob sweeps")
        .expect("carried");
    deliver(&wire(sweep.commit), &mut [(&a_lost, &mut lost_g)]);
    assert!(b1_g.merge_staged_sweep(&b1).expect("merge"));
    let mut new_g = Group::join_from_welcome(&wire(sweep.welcome.expect("welcome")), &a_new)
        .expect("alice's new device joins");
    assert!(
        is_member(&new_g, &a_lost),
        "the lost leaf stays in the roster"
    );

    // Both of alice's leaves hold rank 0 from every view; bob holds 1.
    assert_eq!(new_g.own_committer_rank(&a_new), Some(0));
    assert_eq!(lost_g.own_committer_rank(&a_lost), Some(0));
    for v in [&lost_g, &new_g, &b1_g] {
        assert_eq!(v.committer_rank_of(a_lost.signature_public_key()), Some(0));
        assert_eq!(v.committer_rank_of(a_new.signature_public_key()), Some(0));
        assert_eq!(v.committer_rank_of(b1.signature_public_key()), Some(1));
    }

    // Bob adds his own second device; alice's NEW device sweeps it as owner.
    let b2 = device(&b, 0x46);
    let p = b1_g
        .propose_add_member_from_wire(&b1, &kp_wire(&b2))
        .expect("bob proposes his own device");
    deliver_stored(&p, &mut [(&a_lost, &mut lost_g), (&a_new, &mut new_g)]);
    let sweep = new_g
        .stage_pending_proposals(&a_new)
        .expect("alice's new device may sweep")
        .expect("carried");
    assert_eq!(sweep.swept.len(), 1);
    let commit = wire(sweep.commit);
    deliver(&commit, &mut [(&a_lost, &mut lost_g), (&b1, &mut b1_g)]);
    assert!(new_g.merge_staged_sweep(&a_new).expect("merge"));
    let b2_g =
        Group::join_from_welcome(&wire(sweep.welcome.expect("welcome")), &b2).expect("b2 joins");
    for g in [&lost_g, &new_g, &b1_g, &b2_g] {
        assert_eq!(g.epoch(), new_g.epoch());
        assert!(g.pending_proposals().is_empty());
        assert!(is_member(g, &b2));
        // Ranks of all leaves of one account are equal from every view.
        assert_eq!(g.committer_rank_of(a_lost.signature_public_key()), Some(0));
        assert_eq!(g.committer_rank_of(a_new.signature_public_key()), Some(0));
        assert_eq!(g.committer_rank_of(b1.signature_public_key()), Some(1));
        assert_eq!(g.committer_rank_of(b2.signature_public_key()), Some(1));
    }
}

/// Two devices of one plain member both hold the member's rank while each
/// holds a device admission, and every view ranks them the same.
#[test]
fn both_devices_of_a_plain_member_hold_its_rank() {
    let a = account(0x51);
    let a1 = device(&a, 0x52);
    let b = account(0x53);
    let (b_low, b_high) = two_devices_sorted(&b, 0x54, 0x55);
    let (mut a1_g, mut joined) = found(&a1, &[&b_low, &b_high]);
    let mut high_g = joined.pop().expect("b_high");
    let mut low_g = joined.pop().expect("b_low");
    // Nothing pending: a plain member has no rank on either device.
    assert_eq!(low_g.own_committer_rank(&b_low), None);
    assert_eq!(high_g.own_committer_rank(&b_high), None);
    let b3 = device(&b, 0x56);
    let p = high_g
        .propose_add_member_from_wire(&b_high, &kp_wire(&b3))
        .expect("own device");
    deliver_stored(&p, &mut [(&a1, &mut a1_g), (&b_low, &mut low_g)]);
    assert_eq!(low_g.own_committer_rank(&b_low), Some(1));
    assert_eq!(high_g.own_committer_rank(&b_high), Some(1));
    for v in [&a1_g, &low_g, &high_g] {
        assert_eq!(v.committer_rank_of(b_low.signature_public_key()), Some(1));
        assert_eq!(v.committer_rank_of(b_high.signature_public_key()), Some(1));
    }
    // The higher-keyed device may sweep it (the lower one may be lost).
    let sweep = high_g
        .stage_pending_proposals(&b_high)
        .expect("the higher-keyed device sweeps")
        .expect("carried");
    deliver(
        &wire(sweep.commit),
        &mut [(&a1, &mut a1_g), (&b_low, &mut low_g)],
    );
    assert!(high_g.merge_staged_sweep(&b_high).expect("merge"));
    assert_eq!(a1_g.epoch(), high_g.epoch());
    assert!(is_member(&a1_g, &b3));
}

// ============================================================================
// 7. Item 10 — an owner on a bound device changes roles
// ============================================================================

/// Owner `a1` (a bound device of account `a`), members `b1`, `c1` (bound
/// devices of accounts `b`, `c`).
struct Bound {
    a1: Identity,
    b: Identity,
    b1: Identity,
    c: Identity,
    c1: Identity,
    a1_g: Group,
    b1_g: Group,
    c1_g: Group,
}

fn bound(tag: u8) -> Bound {
    let a = account(0x60 ^ tag);
    let a1 = device(&a, 0x61 ^ tag);
    let b = account(0x62 ^ tag);
    let b1 = device(&b, 0x63 ^ tag);
    let c = account(0x64 ^ tag);
    let c1 = device(&c, 0x65 ^ tag);
    let (a1_g, mut joined) = found(&a1, &[&b1, &c1]);
    let c1_g = joined.pop().expect("c1");
    let b1_g = joined.pop().expect("b1");
    assert_ne!(
        a1.signature_public_key(),
        account_key(&a).as_slice(),
        "the owner device is bound: its leaf key is not the account key"
    );
    Bound {
        a1,
        b,
        b1,
        c,
        c1,
        a1_g,
        b1_g,
        c1_g,
    }
}

#[test]
fn an_owner_on_a_bound_device_sets_an_admin_directly_and_by_proposal() {
    // Direct commit.
    let mut s = bound(0x01);
    let owner = s.a1_g.group_roles().expect("table").owner().to_owned();
    let table = RoleTable::new(owner, vec![account_key(&s.b)]);
    let commit = s
        .a1_g
        .set_group_roles(&s.a1, &table)
        .expect("a bound owner device promotes bob");
    deliver(&commit, &mut [(&s.b1, &mut s.b1_g), (&s.c1, &mut s.c1_g)]);
    for g in [&s.a1_g, &s.b1_g, &s.c1_g] {
        assert_eq!(g.group_roles(), Some(table.clone()));
    }
    assert_eq!(s.b1_g.own_role(&s.b1), Some(Role::Admin));

    // Proposal path.
    let mut s = bound(0x02);
    let owner = s.a1_g.group_roles().expect("table").owner().to_owned();
    let table = RoleTable::new(owner, vec![account_key(&s.c)]);
    let p = s
        .a1_g
        .propose_group_roles(&s.a1, &table)
        .expect("a bound owner device proposes promoting carol");
    deliver_stored(&p, &mut [(&s.b1, &mut s.b1_g), (&s.c1, &mut s.c1_g)]);
    let sweep = wire(
        s.a1_g
            .commit_pending_proposals(&s.a1)
            .expect("sweep")
            .expect("carried")
            .commit,
    );
    deliver(&sweep, &mut [(&s.b1, &mut s.b1_g), (&s.c1, &mut s.c1_g)]);
    for g in [&s.a1_g, &s.b1_g, &s.c1_g] {
        assert_eq!(g.group_roles(), Some(table.clone()));
    }
}

#[test]
fn an_owner_on_a_bound_device_transfers_ownership_directly_and_by_proposal() {
    // Direct commit.
    let mut s = bound(0x03);
    let b_account = account_key(&s.b);
    let commit = s
        .a1_g
        .transfer_ownership(&s.a1, &b_account, OutgoingOwner::Admin)
        .expect("a bound owner device transfers to bob");
    deliver(&commit, &mut [(&s.b1, &mut s.b1_g), (&s.c1, &mut s.c1_g)]);
    for g in [&s.a1_g, &s.b1_g, &s.c1_g] {
        assert_eq!(g.group_roles().expect("table").owner(), &b_account);
    }
    assert_eq!(s.b1_g.own_role(&s.b1), Some(Role::Owner));
    assert_eq!(s.a1_g.own_role(&s.a1), Some(Role::Admin));

    // Proposal path.
    let mut s = bound(0x04);
    let c_account = account_key(&s.c);
    let p = s
        .a1_g
        .propose_ownership_transfer(&s.a1, &c_account, OutgoingOwner::Member)
        .expect("a bound owner device proposes a transfer to carol");
    deliver_stored(&p, &mut [(&s.b1, &mut s.b1_g), (&s.c1, &mut s.c1_g)]);
    let sweep = wire(
        s.a1_g
            .commit_pending_proposals(&s.a1)
            .expect("sweep")
            .expect("carried")
            .commit,
    );
    deliver(&sweep, &mut [(&s.b1, &mut s.b1_g), (&s.c1, &mut s.c1_g)]);
    for g in [&s.a1_g, &s.b1_g, &s.c1_g] {
        assert_eq!(g.group_roles().expect("table").owner(), &c_account);
    }
    assert_eq!(s.c1_g.own_role(&s.c1), Some(Role::Owner));
    assert_eq!(s.a1_g.own_role(&s.a1), Some(Role::Member));
}

#[test]
fn the_role_table_refusals_still_fire_for_bound_devices() {
    let mut s = bound(0x05);
    let owner = s.a1_g.group_roles().expect("table").owner().to_owned();
    let epoch = s.a1_g.epoch();

    // A non-owner bound device: refused NotOwner on every path.
    let table = RoleTable::new(owner, vec![account_key(&s.b)]);
    for err in [
        s.b1_g
            .set_group_roles(&s.b1, &table)
            .expect_err("not owner"),
        s.b1_g
            .propose_group_roles(&s.b1, &table)
            .expect_err("not owner"),
        s.b1_g
            .transfer_ownership(&s.b1, &account_key(&s.c), OutgoingOwner::Admin)
            .expect_err("not owner"),
        s.b1_g
            .propose_ownership_transfer(&s.b1, &account_key(&s.c), OutgoingOwner::Admin)
            .expect_err("not owner"),
    ] {
        assert_eq!(err.refusal_reason(), Some(RefusalReason::NotOwner));
    }

    // A named owner with no leaf in the roster: refused OwnerNotAMember.
    let stranger = account(0x7E);
    let stranger_key = account_key(&stranger);
    let foreign = RoleTable::new(stranger_key, vec![]);
    for err in [
        s.a1_g
            .set_group_roles(&s.a1, &foreign)
            .expect_err("owner not a member"),
        s.a1_g
            .propose_group_roles(&s.a1, &foreign)
            .expect_err("owner not a member"),
        s.a1_g
            .transfer_ownership(&s.a1, &stranger_key, OutgoingOwner::Admin)
            .expect_err("owner not a member"),
        s.a1_g
            .propose_ownership_transfer(&s.a1, &stranger_key, OutgoingOwner::Admin)
            .expect_err("owner not a member"),
    ] {
        assert_eq!(err.refusal_reason(), Some(RefusalReason::OwnerNotAMember));
    }

    // A device leaf key (not an account key) named as owner is not a member
    // account either — the check reads resolved account keys.
    let leaf_named: [u8; 32] = s.b1.signature_public_key().try_into().expect("32 bytes");
    let err = s
        .a1_g
        .set_group_roles(&s.a1, &RoleTable::new(leaf_named, vec![]))
        .expect_err("a leaf key is not an account");
    assert_eq!(err.refusal_reason(), Some(RefusalReason::OwnerNotAMember));

    // Nothing moved.
    assert_eq!(s.a1_g.epoch(), epoch);
    assert!(s.a1_g.pending_proposals().is_empty());
    assert!(s.b1_g.pending_proposals().is_empty());
}
