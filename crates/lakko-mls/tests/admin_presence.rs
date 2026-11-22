// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0022 amendment of 2026-11-22 (C-100): **a person who is not in the
//! group is not its admin.**
//!
//! The role table names account keys, not leaves, so before this amendment a
//! removal never touched it: an owner who removed an admin and later added them
//! back found them an admin again. Now the commit that takes an admin's last
//! leaf out of the group drops them from the table in the same epoch (the
//! committer folds a by-value prune into its sweep), every peer refuses a commit
//! that would leave an admin without a leaf, and the Welcome a re-added person
//! receives carries a table without them.
//!
//! Public API only. The adversarial cases (a committer that omits the prune, or
//! writes a different table under its name) live in-crate, in
//! `group/admin_presence_tests.rs`, because building them needs the commit
//! builder.

use lakko_mls::{
    group, key_package, Group, GroupKind, Identity, LeafNodeIndex, RefusalReason, Role, RoleTable,
};
use openmls::extensions::{
    Extension, ExtensionType, Extensions, RequiredCapabilitiesExtension, UnknownExtension,
};
use proptest::prelude::*;

const ROLE_EXTENSION_TYPE: u16 = 0xFF02;
const KIND_EXTENSION_TYPE: u16 = 0xFF03;

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

fn pk32(id: &Identity) -> [u8; 32] {
    id.account_public_key().expect("32-byte account key")
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

/// The extension set a role-table rewrite installs, restated so a test can
/// commit one that `set_group_roles` itself refuses to build.
fn role_extensions(table: &RoleTable) -> Extensions<openmls::group::GroupContext> {
    Extensions::from_vec(vec![
        Extension::RequiredCapabilities(RequiredCapabilitiesExtension::new(
            &[
                ExtensionType::Unknown(ROLE_EXTENSION_TYPE),
                ExtensionType::Unknown(KIND_EXTENSION_TYPE),
            ],
            &[],
            &[],
        )),
        Extension::Unknown(ROLE_EXTENSION_TYPE, UnknownExtension(table.encode())),
        Extension::Unknown(
            KIND_EXTENSION_TYPE,
            UnknownExtension(GroupKind::Group.encode().to_vec()),
        ),
    ])
    .expect("build role extensions")
}

/// One participant: an identity and, while it is in the group, its handle.
struct Member {
    id: Identity,
    g: Option<Group>,
}

/// A group of named members. `members[0]` is the owner and the only committer.
struct Room {
    members: Vec<Member>,
}

impl Room {
    /// The owner creates the group and adds every other identity, applying each
    /// commit on every member already in.
    fn new(ids: Vec<Identity>) -> Self {
        let mut members: Vec<Member> = ids.into_iter().map(|id| Member { id, g: None }).collect();
        members[0].g = Some(Group::create(&members[0].id, GroupKind::Group).expect("create"));
        let mut room = Self { members };
        for i in 1..room.members.len() {
            room.add(i);
        }
        room
    }

    /// Member `i`'s identity and group handle, borrowed together.
    fn split(&mut self, i: usize) -> (&Identity, &mut Group) {
        let m = &mut self.members[i];
        (&m.id, m.g.as_mut().expect("in the group"))
    }

    fn g(&self, i: usize) -> &Group {
        self.members[i].g.as_ref().expect("in the group")
    }

    fn is_in(&self, i: usize) -> bool {
        self.members[i].g.as_ref().is_some_and(Group::is_active)
    }

    /// Hand `bytes` (a proposal or a commit) to every active member but `from`.
    fn deliver(&mut self, from: usize, bytes: &[u8]) {
        for (j, m) in self.members.iter_mut().enumerate() {
            if j == from {
                continue;
            }
            if let Some(g) = m.g.as_mut().filter(|g| g.is_active()) {
                g.process_incoming_verifying_roles(&m.id, bytes)
                    .unwrap_or_else(|e| panic!("member {j} applies: {e:?}"));
            }
        }
    }

    /// The owner sweeps; the commit reaches every other active member, and a
    /// Welcome is joined by everyone in `joiners`. Returns how many proposals
    /// the commit carried.
    fn sweep(&mut self, joiners: &[usize]) -> usize {
        let (owner, g) = self.split(0);
        let commit = g
            .commit_pending_proposals(owner)
            .expect("owner sweeps")
            .expect("something is swept");
        let carried = commit.swept.len();
        self.deliver(0, &wire(commit.commit));
        if let Some(welcome) = commit.welcome {
            let welcome = wire(welcome);
            for &j in joiners {
                let m = &mut self.members[j];
                m.g = Some(Group::join_from_welcome(&welcome, &m.id).expect("joins"));
            }
        }
        carried
    }

    /// `proposer` proposes (already done), the proposal reaches everybody, the
    /// owner sweeps.
    fn propose_and_sweep(&mut self, proposer: usize, proposal: &[u8], joiners: &[usize]) -> usize {
        self.deliver(proposer, proposal);
        self.sweep(joiners)
    }

    fn add(&mut self, i: usize) {
        let kp = kp_wire(&self.members[i].id);
        let (owner, g) = self.split(0);
        let p = g
            .propose_add_member_from_wire(owner, &kp)
            .expect("owner proposes the add");
        self.propose_and_sweep(0, &p, &[i]);
    }

    fn remove(&mut self, i: usize) -> usize {
        let leaf = leaf_of(self.g(0), &self.members[i].id);
        let (owner, g) = self.split(0);
        let p = g
            .propose_remove_member(owner, leaf)
            .expect("owner proposes the removal");
        self.propose_and_sweep(0, &p, &[])
    }

    fn leave(&mut self, i: usize) -> usize {
        let leaf = leaf_of(self.g(i), &self.members[i].id);
        let (id, g) = self.split(i);
        let p = g
            .propose_remove_member(id, leaf)
            .expect("a member may ask to leave");
        self.propose_and_sweep(i, &p, &[])
    }

    fn set_roles(&mut self, table: &RoleTable) {
        let (owner, g) = self.split(0);
        let p = g
            .propose_group_roles(owner, table)
            .expect("owner proposes the role change");
        self.propose_and_sweep(0, &p, &[]);
    }

    /// The role every active member sees `i` holding, asserting they agree.
    fn role_of(&self, i: usize) -> Role {
        let key = pk32(&self.members[i].id);
        let mut seen: Option<Role> = None;
        for m in self.members.iter().filter_map(|m| m.g.as_ref()) {
            if !m.is_active() {
                continue;
            }
            let role = m.group_roles().expect("role table").role_of(&key);
            if let Some(prev) = seen {
                assert_eq!(prev, role, "every member reads the same role");
            }
            seen = Some(role);
        }
        seen.expect("someone is in the group")
    }

    /// C-100: every admin any active member's table names holds a leaf there.
    fn assert_every_admin_holds_a_leaf(&self) {
        for m in self.members.iter().filter_map(|m| m.g.as_ref()) {
            if !m.is_active() {
                continue;
            }
            let table = m.group_roles().expect("role table");
            let roster = m.members();
            for admin in table.admins() {
                assert!(
                    roster
                        .iter()
                        .any(|r| r.account_key.as_slice() == admin.as_slice()),
                    "an admin without a leaf survived a commit",
                );
            }
        }
    }
}

fn promote(room: &Room, who: &[usize]) -> RoleTable {
    RoleTable::new(
        pk32(&room.members[0].id),
        who.iter().map(|&i| pk32(&room.members[i].id)).collect(),
    )
}

/// The defect seen live on 2026-11-22: owner removes an admin, adds them back,
/// and every client showed them as admin again.
#[test]
fn an_admin_the_owner_removes_comes_back_as_a_member() {
    let mut room = Room::new(vec![account(0xA1), account(0xB1), account(0xC1)]);
    let table = promote(&room, &[1]);
    room.set_roles(&table);
    assert_eq!(room.role_of(1), Role::Admin);

    let carried = room.remove(1);
    assert_eq!(carried, 2, "the removal and the prune ride one commit");
    assert!(!room.is_in(1));
    assert!(
        room.g(0).group_roles().expect("table").admins().is_empty(),
        "the owner's table no longer names bob",
    );
    room.assert_every_admin_holds_a_leaf();

    room.add(1);
    assert!(room.is_in(1));
    assert_eq!(room.role_of(1), Role::Member, "re-added as a plain member");
    assert_eq!(
        room.g(1).own_role(&room.members[1].id),
        Some(Role::Member),
        "and the Welcome bob joined from carries a table without him",
    );
}

#[test]
fn an_admin_who_leaves_comes_back_as_a_member() {
    let mut room = Room::new(vec![account(0xA2), account(0xB2), account(0xC2)]);
    room.set_roles(&promote(&room, &[1, 2]));

    let carried = room.leave(1);
    assert_eq!(carried, 2, "the departure and the prune ride one commit");
    assert_eq!(
        room.role_of(2),
        Role::Admin,
        "the other admin keeps the role"
    );
    room.assert_every_admin_holds_a_leaf();

    room.add(1);
    assert_eq!(room.role_of(1), Role::Member);
}

/// ADR-0050: a person is an account. Retiring one of an admin's devices while
/// another stays changes nothing; taking out the last one drops the role.
#[test]
fn removing_one_device_of_an_admin_keeps_the_role_and_the_last_drops_it() {
    let owner = account(0xA3);
    let bob = account(0xB3);
    let b1 = device(&bob, 0xB4);
    let b2 = device(&bob, 0xB5);
    let mut room = Room::new(vec![owner, b1, b2, account(0xC3)]);
    // Both devices hold bob's account key, so one promotion covers both.
    room.set_roles(&promote(&room, &[1]));
    assert_eq!(room.role_of(1), Role::Admin);
    assert_eq!(room.role_of(2), Role::Admin, "the same account");

    let carried = room.remove(2);
    assert_eq!(carried, 1, "no prune: bob still holds a leaf");
    assert_eq!(room.role_of(1), Role::Admin, "bob is still an admin");

    let carried = room.remove(1);
    assert_eq!(carried, 2, "bob's last leaf goes, and so does the role");
    assert!(room.g(0).group_roles().expect("table").admins().is_empty());
    room.assert_every_admin_holds_a_leaf();
}

/// The prune is not an owner privilege: an admin who sweeps the removal of
/// another admin writes it, and every peer — the owner included — accepts it,
/// because it is the one table every peer computes for that commit.
#[test]
fn an_admin_committer_drops_another_admin_and_every_peer_accepts() {
    let mut room = Room::new(vec![
        account(0xA6),
        account(0xB6),
        account(0xC6),
        account(0xD6),
    ]);
    room.set_roles(&promote(&room, &[1, 2]));

    // bob (admin) proposes and sweeps charlie's (admin) removal himself.
    let charlie_leaf = leaf_of(room.g(1), &room.members[2].id);
    let (bob_id, bob_g) = room.split(1);
    let p = bob_g
        .propose_remove_member(bob_id, charlie_leaf)
        .expect("an admin may ask to remove another admin");
    room.deliver(1, &p);
    let (bob_id, bob_g) = room.split(1);
    let commit = bob_g
        .commit_pending_proposals(bob_id)
        .expect("bob sweeps")
        .expect("something is swept");
    assert_eq!(commit.swept.len(), 2, "the removal and bob's prune");
    room.deliver(1, &wire(commit.commit));

    assert!(!room.is_in(2));
    assert_eq!(room.role_of(1), Role::Admin, "bob keeps his own role");
    assert!(
        !room
            .g(0)
            .group_roles()
            .expect("table")
            .can_administer(&pk32(&room.members[2].id)),
        "the owner accepted bob's prune",
    );
    room.assert_every_admin_holds_a_leaf();
}

/// There is one `GroupContextExtensions` slot per commit (RFC 9420, C-65). A
/// pending role change that still names an admin the same sweep takes out of
/// the group is left out; the departure lands and carries the prune.
#[test]
fn a_role_change_naming_a_departing_admin_is_left_out_of_the_sweep() {
    let mut room = Room::new(vec![account(0xA7), account(0xB7), account(0xC7)]);
    room.set_roles(&promote(&room, &[1]));

    // bob (admin) asks to leave; it reaches everyone.
    let bob_leaf = leaf_of(room.g(1), &room.members[1].id);
    let (bob_id, bob_g) = room.split(1);
    let leave = bob_g
        .propose_remove_member(bob_id, bob_leaf)
        .expect("bob asks to leave");
    room.deliver(1, &leave);
    // The owner, not yet having swept, proposes promoting charlie while still
    // listing bob.
    let both = promote(&room, &[1, 2]);
    let (owner, g) = room.split(0);
    let p = g
        .propose_group_roles(owner, &both)
        .expect("bob still holds a leaf when this is proposed");
    room.deliver(0, &p);

    let carried = room.sweep(&[]);
    assert_eq!(
        carried, 2,
        "bob's departure and the prune; the role change is left out"
    );
    assert!(!room.is_in(1));
    assert_eq!(
        room.role_of(2),
        Role::Member,
        "the left-out promotion did not land"
    );
    room.assert_every_admin_holds_a_leaf();
}

#[test]
fn the_owner_cannot_name_an_admin_who_is_not_a_member() {
    let mut room = Room::new(vec![account(0xA8), account(0xB8)]);
    let stranger = account(0xE8);
    let table = RoleTable::new(pk32(&room.members[0].id), vec![pk32(&stranger)]);
    let (owner, g) = room.split(0);
    let err = g
        .propose_group_roles(owner, &table)
        .expect_err("the proposal path refuses it");
    assert_eq!(err.refusal_reason(), Some(RefusalReason::AdminNotAMember));
    let err = g
        .set_group_roles(owner, &table)
        .expect_err("the direct path refuses it too");
    assert_eq!(err.refusal_reason(), Some(RefusalReason::AdminNotAMember));
}

/// The peer gate, for a committer that skips the local refusal: an owner's
/// commit installing a table that names an absent admin is refused by every
/// peer and does not advance its epoch (fail-closed, invariant #4).
#[test]
fn every_peer_refuses_a_table_naming_an_admin_with_no_leaf() {
    let mut room = Room::new(vec![account(0xA9), account(0xB9)]);
    let stranger = account(0xE9);
    let table = RoleTable::new(pk32(&room.members[0].id), vec![pk32(&stranger)]);
    let (owner, g) = room.split(0);
    let commit = g
        .set_group_context_extensions(owner, role_extensions(&table))
        .expect("the raw extension path builds it");
    let (bob, bob_g) = room.split(1);
    let before = bob_g.epoch();
    let err = bob_g
        .process_incoming_verifying_roles(bob, &wire(commit))
        .expect_err("bob refuses it");
    assert_eq!(err.refusal_reason(), Some(RefusalReason::AdminNotAMember));
    assert_eq!(bob_g.epoch(), before, "a refused commit is not merged");
}

#[derive(Clone, Copy, Debug)]
enum Op {
    Promote(usize),
    Demote(usize),
    Remove(usize),
    Leave(usize),
    ReAdd(usize),
}

fn op() -> impl Strategy<Value = Op> {
    (0u8..5, 1usize..4).prop_map(|(k, who)| match k {
        0 => Op::Promote(who),
        1 => Op::Demote(who),
        2 => Op::Remove(who),
        3 => Op::Leave(who),
        _ => Op::ReAdd(who),
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 12, .. ProptestConfig::default() })]

    /// C-100 over arbitrary histories: after every commit, every admin any
    /// member's table names holds a leaf, every member reads the same roles,
    /// and anybody who was ever out of the group comes back as a member.
    #[test]
    fn no_history_leaves_an_admin_without_a_leaf(ops in proptest::collection::vec(op(), 1..7)) {
        let mut room = Room::new(vec![account(0x11), account(0x22), account(0x33), account(0x44)]);
        for op in ops {
            match op {
                Op::Promote(i) if room.is_in(i) && room.role_of(i) == Role::Member => {
                    let mut admins: Vec<usize> =
                        (1..4).filter(|&j| room.is_in(j) && room.role_of(j) == Role::Admin).collect();
                    admins.push(i);
                    let t = promote(&room, &admins);
                    room.set_roles(&t);
                    prop_assert_eq!(room.role_of(i), Role::Admin);
                },
                Op::Demote(i) if room.is_in(i) && room.role_of(i) == Role::Admin => {
                    let admins: Vec<usize> = (1..4)
                        .filter(|&j| j != i && room.is_in(j) && room.role_of(j) == Role::Admin)
                        .collect();
                    let t = promote(&room, &admins);
                    room.set_roles(&t);
                    prop_assert_eq!(room.role_of(i), Role::Member);
                },
                Op::Remove(i) if room.is_in(i) => {
                    room.remove(i);
                },
                Op::Leave(i) if room.is_in(i) => {
                    room.leave(i);
                },
                Op::ReAdd(i) if !room.is_in(i) => {
                    room.add(i);
                    prop_assert_eq!(room.role_of(i), Role::Member);
                },
                _ => {},
            }
            room.assert_every_admin_holds_a_leaf();
        }
    }
}
