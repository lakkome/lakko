// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A PublisherGroup of devices ([ADR-0090]): every leaf a bound device, every
//! commit staged and merged only on the Delivery Service's word, every commit
//! judged by every peer.
//!
//! The Delivery Service is modelled by its one rule that matters here: the
//! journal takes ONE commit per epoch (the ADR-0081 commit route answers the
//! first `201` and every other `409`). A test that "races" two devices stages
//! on both, merges the winner, abandons the loser and has it apply the
//! winner's commit, exactly what a client does on a `409`.
//!
//! [ADR-0090]: ../../../docs/adr/0090-publisher-group-device-leaves.md

use lakko_mls::{
    key_package, publisher_group_id_for, publisher_owner_account_key, ChannelId, Group, Identity,
    Incoming, PublisherGroup, RefusalReason, CHANNEL_ID_LEN,
};
use proptest::prelude::*;

const OWNER: [u8; 32] = [0x0A; 32];
const COPUB: [u8; 32] = [0x0C; 32];

fn device(tag: u8, account: &[u8; 32]) -> Identity {
    Identity::from_seed_bound(&[tag; 64], account, 1).expect("device")
}

fn kp(id: &Identity) -> Vec<u8> {
    key_package::serialize(&id.create_key_package().expect("kp")).expect("kp wire")
}

fn cid(tag: u8) -> ChannelId {
    ChannelId::try_from_slice(&[tag; CHANNEL_ID_LEN]).expect("cid")
}

fn owner_key() -> [u8; 32] {
    publisher_owner_account_key(&OWNER)
}

fn apply(pg: &mut PublisherGroup, id: &Identity, wire: &[u8]) {
    let out = pg.process_incoming(id, wire).expect("apply");
    assert!(matches!(out, Incoming::Commit), "a commit");
}

/// One device holding a PublisherGroup.
struct Member {
    id: Identity,
    pg: PublisherGroup,
}

/// A channel founded by owner device 0, with `owner_devices - 1` more owner
/// devices admitted and, when `copublisher`, one co-publisher device
/// approved. Every commit is staged then merged (the journal took it) and
/// applied by every other member.
struct Channel {
    cid: ChannelId,
    members: Vec<Member>,
}

impl Channel {
    fn new(owner_devices: u8, copublisher: bool, tag: u8) -> Self {
        let cid = cid(tag);
        let founder = device(tag, &OWNER);
        let pg = PublisherGroup::create(&founder, &OWNER, &cid).expect("create");
        let mut c = Self {
            cid,
            members: vec![Member { id: founder, pg }],
        };
        for i in 1..owner_devices {
            c.admit_owner_device(device(tag.wrapping_add(i), &OWNER));
        }
        if copublisher {
            let id = device(tag.wrapping_add(0x50), &COPUB);
            let committer = &mut c.members[0];
            committer
                .pg
                .retain_join_request(&committer.id, &kp(&id))
                .expect("retain");
            let add = committer
                .pg
                .stage_approve_external_join(&committer.id, id.signature_public_key())
                .expect("approve");
            c.land(0, &add.commit);
            let gid = c.members[0].pg.group_id();
            let pg =
                PublisherGroup::join_from_welcome(&id, &add.welcome, &gid, &c.cid, &owner_key())
                    .expect("copublisher joins");
            c.members.push(Member { id, pg });
        }
        c
    }

    fn admit_owner_device(&mut self, id: Identity) {
        let committer = &mut self.members[0];
        let add = committer
            .pg
            .stage_admit_devices(&committer.id, &[&kp(&id)])
            .expect("admit");
        self.land(0, &add.commit);
        let gid = publisher_group_id_for(&OWNER, &self.cid);
        let pg =
            PublisherGroup::join_from_welcome(&id, &add.welcome, &gid, &self.cid, &owner_key())
                .expect("owner device joins");
        self.members.push(Member { id, pg });
    }

    /// The journal took member `who`'s staged commit: it merges, everyone
    /// else applies it.
    fn land(&mut self, who: usize, commit: &[u8]) {
        let m = &mut self.members[who];
        assert!(m.pg.merge_staged_commit(&m.id).expect("merge"));
        for (i, other) in self.members.iter_mut().enumerate() {
            if i != who {
                apply(&mut other.pg, &other.id, commit);
            }
        }
    }

    fn seeds(&self) -> Vec<[u8; 32]> {
        self.members
            .iter()
            .map(|m| m.pg.channel_seed(&m.id).expect("seed"))
            .collect()
    }
}

/// Section 1: two devices of one account hold distinct leaves under one
/// account key and derive the same channel seed; `is_owned_by` and
/// `is_owner_device` hold on every owner device and on no co-publisher's.
#[test]
fn owner_devices_hold_distinct_leaves_and_one_seed() {
    let c = Channel::new(2, true, 0x10);
    let members = c.members[0].pg.members();
    assert_eq!(members.len(), 3);
    let owner_leaves: Vec<_> = members
        .iter()
        .filter(|m| m.account_key == owner_key())
        .collect();
    assert_eq!(
        owner_leaves.len(),
        2,
        "two leaves for the one owner account"
    );
    assert_ne!(
        owner_leaves[0].signature_public_key,
        owner_leaves[1].signature_public_key
    );
    let seeds = c.seeds();
    assert!(seeds.iter().all(|s| *s == seeds[0]), "one seed everywhere");

    for m in &c.members[..2] {
        assert!(m.pg.is_owned_by(&OWNER));
        assert!(m.pg.is_owner_device(&m.id));
        assert_eq!(m.pg.owner_account_key(), &owner_key());
    }
    let co = &c.members[2];
    assert!(!co.pg.is_owned_by(&COPUB));
    assert!(!co.pg.is_owner_device(&co.id));
    assert_eq!(co.pg.owner_account_key(), &owner_key());
}

/// Section 3: an admission staged and abandoned on a lost race leaves the
/// group at N with its seed, and another channel's group in the same provider
/// untouched; the request can be staged again and then lands.
#[test]
fn an_abandoned_admission_leaves_the_group_at_n_and_other_groups_untouched() {
    let mut c = Channel::new(1, false, 0x20);
    let founder = &mut c.members[0];
    let mut other = PublisherGroup::create(&founder.id, &OWNER, &cid(0x21)).expect("other");
    other.stage_rotate(&founder.id).expect("rotate other");
    assert!(other.merge_staged_commit(&founder.id).expect("merge other"));
    let other_epoch = other.epoch();
    let other_seed = other.channel_seed(&founder.id).expect("seed");

    let epoch = founder.pg.epoch();
    let seed = founder.pg.channel_seed(&founder.id).expect("seed");
    let newcomer = device(0x2F, &OWNER);
    founder
        .pg
        .stage_admit_devices(&founder.id, &[&kp(&newcomer)])
        .expect("stage");
    // While staged, a second membership commit is refused.
    let err = founder.pg.stage_rotate(&founder.id).expect_err("refused");
    assert_eq!(
        err.refusal_reason(),
        Some(RefusalReason::SweepAlreadyStaged)
    );
    assert!(founder
        .pg
        .abandon_staged_commit(&founder.id)
        .expect("abandon"));
    assert!(!founder.pg.has_staged_commit());
    assert_eq!(founder.pg.epoch(), epoch);
    assert_eq!(founder.pg.channel_seed(&founder.id).expect("seed"), seed);
    assert_eq!(founder.pg.members().len(), 1);

    let reopened = PublisherGroup::load(&founder.id, &OWNER, &cid(0x21))
        .expect("load")
        .expect("some");
    assert_eq!(reopened.epoch(), other_epoch);
    assert_eq!(
        reopened.channel_seed(&founder.id).expect("seed"),
        other_seed
    );

    founder
        .pg
        .stage_admit_devices(&founder.id, &[&kp(&newcomer)])
        .expect("stage again");
    assert!(founder.pg.merge_staged_commit(&founder.id).expect("merge"));
    assert_eq!(founder.pg.members().len(), 2);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(12))]

    /// Section 4: two owner devices stage a rotation on one epoch; the journal
    /// takes either; the loser abandons and applies the winner; every member
    /// ends on one epoch and one seed, which neither staged seed of the loser
    /// is.
    #[test]
    fn two_owner_devices_rotating_on_one_epoch_converge(winner in 0usize..2, copub in any::<bool>()) {
        let mut c = Channel::new(2, copub, 0x30);
        let loser = 1 - winner;
        let epoch = c.members[0].pg.epoch();
        let win_commit = {
            let m = &mut c.members[winner];
            m.pg.stage_rotate(&m.id).expect("winner stages")
        };
        let lose_seed = {
            let m = &mut c.members[loser];
            m.pg.stage_rotate(&m.id).expect("loser stages");
            m.pg.staged_channel_seed(&m.id).expect("staged").expect("some")
        };
        {
            let m = &mut c.members[loser];
            prop_assert!(m.pg.abandon_staged_commit(&m.id).expect("abandon"));
            prop_assert_eq!(m.pg.epoch(), epoch);
        }
        c.land(winner, &win_commit);
        let seeds = c.seeds();
        prop_assert!(seeds.iter().all(|s| *s == seeds[0]));
        prop_assert_ne!(seeds[0], lose_seed, "the loser's seed never existed");
        for m in &c.members {
            prop_assert_eq!(m.pg.epoch(), epoch + 1);
        }
    }

    /// Section 2.1 / 2.4: a retired device cannot derive the seed of the
    /// epoch its retirement opens, nor of any later rotation.
    #[test]
    fn a_retired_device_cannot_derive_the_next_seed(rotations in 0usize..3, retired_applies in any::<bool>()) {
        let mut c = Channel::new(3, false, 0x40);
        let retired_key = c.members[2].id.signature_public_key().to_vec();
        let commit = {
            let m = &mut c.members[0];
            m.pg.stage_retire_devices(&m.id, &[&retired_key]).expect("retire")
        };
        let m0 = &mut c.members[0];
        prop_assert!(m0.pg.merge_staged_commit(&m0.id).expect("merge"));
        let m1 = &mut c.members[1];
        apply(&mut m1.pg, &m1.id, &commit);
        let mut retired = c.members.pop().expect("retired");
        if retired_applies {
            let _ = retired.pg.process_incoming(&retired.id, &commit);
            prop_assert!(!retired.pg.is_active());
        }
        let mut now = c.members[0].pg.channel_seed(&c.members[0].id).expect("seed");
        prop_assert!(retired.pg.channel_seed(&retired.id).map_or(true, |s| s != now));
        for _ in 0..rotations {
            let commit = {
                let m = &mut c.members[1];
                m.pg.stage_rotate(&m.id).expect("the other owner device rotates")
            };
            c.land(1, &commit);
            now = c.members[0].pg.channel_seed(&c.members[0].id).expect("seed");
            prop_assert!(retired.pg.channel_seed(&retired.id).map_or(true, |s| s != now));
            prop_assert!(retired.pg.process_incoming(&retired.id, &commit).is_err() || !retired.pg.is_active());
        }
        prop_assert_eq!(c.members[0].pg.members().len(), 2);
    }

    /// Section 1: removing a publisher never removes a leaf of the owner's
    /// account, whatever account is named.
    #[test]
    fn remove_publisher_never_removes_an_owner_leaf(owner_devices in 1u8..4, name_owner in any::<bool>()) {
        let mut c = Channel::new(owner_devices, true, 0x50);
        let target = if name_owner { owner_key().to_vec() } else {
            publisher_owner_account_key(&COPUB).to_vec()
        };
        let m = &mut c.members[0];
        let result = m.pg.stage_remove_publisher(&m.id, &target);
        if name_owner {
            let err = result.expect_err("refused");
            prop_assert_eq!(err.refusal_reason(), Some(RefusalReason::OwnerUnremovable));
            prop_assert!(!m.pg.has_staged_commit());
        } else {
            result.expect("removes the co-publisher");
            prop_assert!(m.pg.merge_staged_commit(&m.id).expect("merge"));
        }
        let owner_leaves = c.members[0]
            .pg
            .members()
            .iter()
            .filter(|l| l.account_key == owner_key())
            .count();
        prop_assert_eq!(owner_leaves, usize::from(owner_devices));
    }
}

/// Section 1 and 2.1, the local half: an admitted leaf with no valid binding,
/// a device of an account that does not publish, and the ninth leaf of an
/// account are refused before anything is staged; a co-publisher's device
/// commits nothing.
#[test]
fn bad_admissions_are_refused_locally() {
    let mut c = Channel::new(1, true, 0x60);
    let m = &mut c.members[0];

    let unbound = Identity::from_seed(&[0x61; 64]).expect("unbound");
    let err =
        m.pg.stage_admit_devices(&m.id, &[&kp(&unbound)])
            .err()
            .expect("refused");
    assert_eq!(
        err.refusal_reason(),
        Some(RefusalReason::DeviceBindingInvalid)
    );

    let stranger = device(0x62, &[0x77; 32]);
    let err =
        m.pg.stage_admit_devices(&m.id, &[&kp(&stranger)])
            .err()
            .expect("refused");
    assert!(err.to_string().contains("already publishes"), "{err}");
    assert!(!m.pg.has_staged_commit());

    let seven: Vec<Vec<u8>> = (0..7).map(|i| kp(&device(0x63 + i, &OWNER))).collect();
    let refs: Vec<&[u8]> = seven.iter().map(Vec::as_slice).collect();
    m.pg.stage_admit_devices(&m.id, &refs)
        .expect("eight leaves are allowed");
    assert!(m.pg.merge_staged_commit(&m.id).expect("merge"));
    let ninth = device(0x6F, &OWNER);
    let err =
        m.pg.stage_admit_devices(&m.id, &[&kp(&ninth)])
            .err()
            .expect("refused");
    assert_eq!(
        err.refusal_reason(),
        Some(RefusalReason::DeviceLimitReached)
    );

    let co = &mut c.members[1];
    let err = co.pg.stage_rotate(&co.id).expect_err("refused");
    assert_eq!(err.refusal_reason(), Some(RefusalReason::NotOwner));
    let err = co
        .pg
        .stage_admit_devices(&co.id, &[&kp(&device(0x70, &COPUB))])
        .err()
        .expect("refused");
    assert_eq!(err.refusal_reason(), Some(RefusalReason::NotOwner));
}

/// The peer half: a commit that breaks the rule, built outside the
/// PublisherGroup surface on a raw handle of the group, is refused by every
/// peer and not merged.
fn peers_refuse(build: impl FnOnce(&mut Group, &Identity) -> Vec<u8>, reason: RefusalReason) {
    let mut c = Channel::new(2, true, 0x80);
    let forger_index = if reason == RefusalReason::NotOwner {
        2
    } else {
        0
    };
    let gid = c.members[0].pg.group_id();
    let commit = {
        let forger_id = &c.members[forger_index].id;
        let mut raw = Group::load(forger_id, &gid).expect("load").expect("stored");
        build(&mut raw, forger_id)
    };
    for (i, m) in c.members.iter_mut().enumerate() {
        if i == forger_index {
            continue;
        }
        let epoch = m.pg.epoch();
        let err = m.pg.process_incoming(&m.id, &commit).expect_err("refused");
        assert_eq!(err.refusal_reason(), Some(reason), "{err}");
        assert_eq!(m.pg.epoch(), epoch, "not merged");
    }
}

#[test]
fn a_co_publishers_commit_is_refused_by_every_peer() {
    peers_refuse(
        |raw, id| raw.self_update(id).expect("forged rotation"),
        RefusalReason::NotOwner,
    );
}

#[test]
fn an_unbound_leaf_is_refused_by_every_peer() {
    peers_refuse(
        |raw, id| {
            let unbound = Identity::from_seed(&[0x91; 64]).expect("unbound");
            let add = raw
                .add_member(id, unbound.create_key_package().expect("kp"))
                .expect("forged add");
            lakko_mls::group::encode_message(&add.commit).expect("encode")
        },
        RefusalReason::DeviceBindingInvalid,
    );
}

#[test]
fn a_ninth_leaf_is_refused_by_every_peer() {
    peers_refuse(
        |raw, id| {
            let kps = (0..7)
                .map(|i| device(0xA0 + i, &OWNER).create_key_package().expect("kp"))
                .collect();
            let add = raw.add_members(id, kps).expect("forged add");
            lakko_mls::group::encode_message(&add.commit).expect("encode")
        },
        RefusalReason::DeviceLimitReached,
    );
}

/// Section 2.3: a Welcome is joined only under the owner key that sent it.
/// The `/g/` capability's issuer is the owner's account key (the clients sign
/// it with the same seed they derive the group id from); a co-publisher that
/// passes any other key is refused before anything is written.
#[test]
fn a_welcome_from_anyone_but_the_owner_is_refused() {
    let c = Channel::new(1, false, 0xB0);
    let channel = c.cid;
    let mut founder = c.members.into_iter().next().expect("founder");
    let joiner = device(0xB1, &COPUB);
    founder
        .pg
        .retain_join_request(&founder.id, &kp(&joiner))
        .expect("retain");
    let add = founder
        .pg
        .stage_approve_external_join(&founder.id, joiner.signature_public_key())
        .expect("approve");
    assert!(founder.pg.merge_staged_commit(&founder.id).expect("merge"));
    let gid = founder.pg.group_id();
    let wrong = publisher_owner_account_key(&[0x99; 32]);
    let err = PublisherGroup::join_from_welcome(&joiner, &add.welcome, &gid, &channel, &wrong)
        .err()
        .expect("refused");
    assert_eq!(err.refusal_reason(), Some(RefusalReason::NotOwner));
    assert!(PublisherGroup::load_joined(&joiner, &gid, &channel)
        .expect("load")
        .is_none());
    PublisherGroup::join_from_welcome(&joiner, &add.welcome, &gid, &channel, &owner_key())
        .expect("the owner's key joins");
}

/// Decision A (section 2.5): a device that founded the account alone founds
/// the PublisherGroup again at the same id; the old device cannot read the
/// new group's commits; once it deletes its state it is admitted and shares
/// the new seed.
#[test]
fn re_founding_at_the_same_id_shuts_out_the_old_state_until_admitted() {
    let mut c = Channel::new(1, false, 0xC0);
    let old = c.members.pop().expect("old");
    let restored = device(0xC1, &OWNER);
    let mut new_pg = PublisherGroup::create(&restored, &OWNER, &c.cid).expect("re-found");
    assert_eq!(new_pg.group_id(), old.pg.group_id(), "same id");
    let rotation = new_pg.stage_rotate(&restored).expect("rotate");
    assert!(new_pg.merge_staged_commit(&restored).expect("merge"));

    let Member {
        id: old_id,
        pg: mut old_pg,
    } = old;
    assert!(
        old_pg.process_incoming(&old_id, &rotation).is_err(),
        "the old state cannot read it"
    );
    assert_ne!(
        old_pg.channel_seed(&old_id).expect("old seed"),
        new_pg.channel_seed(&restored).expect("new seed")
    );

    old_pg.delete(&old_id).expect("delete");
    assert!(PublisherGroup::load(&old_id, &OWNER, &c.cid)
        .expect("load")
        .is_none());
    let add = new_pg
        .stage_admit_devices(&restored, &[&kp(&old_id)])
        .expect("admit");
    assert!(new_pg.merge_staged_commit(&restored).expect("merge"));
    let gid = publisher_group_id_for(&OWNER, &c.cid);
    let back = PublisherGroup::join_from_welcome(&old_id, &add.welcome, &gid, &c.cid, &owner_key())
        .expect("admitted again");
    assert_eq!(
        back.channel_seed(&old_id).expect("seed"),
        new_pg.channel_seed(&restored).expect("seed")
    );
}

// ---------------------------------------------------------------------------
// ADR-0064 section 7, amendment of 2026-11-14: an owner device that holds no
// PublisherGroup enters from a GroupInfo another owner device saved, by an
// external commit carrying its ExternalInit alone, held until the Delivery
// Service answers.
// ---------------------------------------------------------------------------

/// What an owner device saves in the vault head for the current epoch: the
/// GroupInfo and the Delivery Service access seed of that epoch.
fn current_entry_point(c: &Channel) -> (Vec<u8>, [u8; 32]) {
    let m = &c.members[0];
    let info = m.pg.group_info(&m.id).expect("info");
    let seed = *m.pg.inner().ds_access_seed(&m.id).expect("access seed");
    (info, seed)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// G-55: the row the Delivery Service stores names neither the owner's
/// account key nor the entering device's binding or key.
fn assert_row_names_nobody(row: &[u8], entering: &Identity) {
    assert!(lakko_mls::is_wrapped_entry(row), "the entry is wrapped");
    assert!(!contains(row, &owner_key()), "no account key in the row");
    let binding = entering.device_binding().expect("bound device");
    assert!(!contains(row, binding), "no device binding in the row");
    assert!(
        !contains(row, entering.signature_public_key()),
        "no device key in the row"
    );
}

/// The journal took an entry: the entering device merges, every member
/// applies the commit.
fn land_entry(c: &mut Channel, id: Identity, mut pg: PublisherGroup, commit: &[u8]) {
    assert!(pg.merge_staged_commit(&id).expect("merge entry"));
    for m in &mut c.members {
        apply(&mut m.pg, &m.id, commit);
    }
    c.members.push(Member { id, pg });
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10))]

    /// An owner device with nothing but a saved GroupInfo enters and reaches
    /// the same channel seed as every member, whether the GroupInfo was the
    /// current one or the one saved before a commit that then landed; it then
    /// commits like any owner device.
    #[test]
    fn an_owner_device_enters_from_a_saved_group_info(
        owner_devices in 1u8..3,
        copub in any::<bool>(),
        saved_before_a_rotation in any::<bool>(),
    ) {
        let mut c = Channel::new(owner_devices, copub, 0xD0);
        let (info, access) = if saved_before_a_rotation {
            let m = &mut c.members[0];
            let rotation = m.pg.stage_rotate(&m.id).expect("rotate");
            let saved = m.pg.staged_group_info(&m.id).expect("staged info").expect("some");
            let seed = *m.pg.inner().staged_ds_access_seed(&m.id).expect("seed").expect("some");
            c.land(0, &rotation);
            (saved, seed)
        } else {
            current_entry_point(&c)
        };
        let entering = device(0xD9, &OWNER);
        let entry =
            PublisherGroup::enter(&entering, &info, &OWNER, &c.cid, &access).expect("entered");
        assert_row_names_nobody(&entry.commit, &entering);
        let pg = entry.group;
        prop_assert!(pg.has_staged_commit());
        prop_assert_eq!(pg.epoch(), c.members[0].pg.epoch(), "it reports the epoch it leaves");
        prop_assert!(pg.channel_seed(&entering).is_err(), "no current epoch is held");
        let next = pg.staged_channel_seed(&entering).expect("staged").expect("some");
        land_entry(&mut c, entering, pg, &entry.commit);
        let seeds = c.seeds();
        prop_assert!(seeds.iter().all(|s| *s == next), "one seed");
        let entered = c.members.len() - 1;
        let entered_key = c.members[entered].id.signature_public_key().to_vec();
        let listed = c.members[0]
            .pg
            .members()
            .iter()
            .any(|m| m.signature_public_key == entered_key && m.account_key == owner_key());
        prop_assert!(listed, "the members list the entered owner device");

        let m = &mut c.members[entered];
        let rotation = m.pg.stage_rotate(&m.id).expect("the entered device rotates");
        c.land(entered, &rotation);
        let seeds = c.seeds();
        prop_assert!(seeds.windows(2).all(|w| w[0] == w[1]));
    }

    /// Two owner devices enter from the same GroupInfo: the journal takes one
    /// (`201`), every member applies it and refuses the other; the other
    /// device abandons, holds nothing, and enters again from the newer
    /// GroupInfo.
    #[test]
    fn two_entries_at_one_epoch_only_one_merges(first_wins in any::<bool>()) {
        let mut c = Channel::new(1, true, 0xE0);
        let (info, access) = current_entry_point(&c);
        let a_id = device(0xE7, &OWNER);
        let b_id = device(0xE8, &OWNER);
        let a = PublisherGroup::enter(&a_id, &info, &OWNER, &c.cid, &access).expect("a");
        let b = PublisherGroup::enter(&b_id, &info, &OWNER, &c.cid, &access).expect("b");
        let (win_id, win, lose_id, mut lose) = if first_wins {
            (a_id, a, b_id, b)
        } else {
            (b_id, b, a_id, a)
        };
        land_entry(&mut c, win_id, win.group, &win.commit);
        let holders = c.members.len() - 1;
        for m in &mut c.members[..holders] {
            let epoch = m.pg.epoch();
            prop_assert!(m.pg.process_incoming(&m.id, &lose.commit).is_err(), "the loser is refused");
            prop_assert_eq!(m.pg.epoch(), epoch);
        }
        prop_assert!(lose.group.abandon_staged_commit(&lose_id).expect("abandon"));
        prop_assert!(!lose.group.has_staged_commit());
        prop_assert!(!lose.group.is_active(), "the handle holds nothing");
        prop_assert!(lose.group.group_info(&lose_id).is_err());
        prop_assert!(PublisherGroup::load(&lose_id, &OWNER, &c.cid).expect("load").is_none());

        let (newer, access) = current_entry_point(&c);
        let again =
            PublisherGroup::enter(&lose_id, &newer, &OWNER, &c.cid, &access).expect("enter again");
        land_entry(&mut c, lose_id, again.group, &again.commit);
        let seeds = c.seeds();
        prop_assert!(seeds.windows(2).all(|w| w[0] == w[1]));
    }
}

/// A GroupInfo of an epoch the group has left builds a commit every member
/// refuses (the Delivery Service answers it `409` too); the abandoned entry
/// leaves nothing on the device.
#[test]
fn an_entry_from_a_stale_group_info_is_refused() {
    let mut c = Channel::new(2, false, 0xF0);
    let (stale, access) = current_entry_point(&c);
    let m = &mut c.members[1];
    let rotation = m.pg.stage_rotate(&m.id).expect("rotate");
    c.land(1, &rotation);
    let entering = device(0xF9, &OWNER);
    let mut entry =
        PublisherGroup::enter(&entering, &stale, &OWNER, &c.cid, &access).expect("built");
    for m in &mut c.members {
        let epoch = m.pg.epoch();
        assert!(m.pg.process_incoming(&m.id, &entry.commit).is_err());
        assert_eq!(m.pg.epoch(), epoch);
    }
    assert!(entry
        .group
        .abandon_staged_commit(&entering)
        .expect("abandon"));
    assert!(PublisherGroup::load(&entering, &OWNER, &c.cid)
        .expect("load")
        .is_none());
}

/// `enter` refuses, before writing anything: a co-publisher's device, a
/// GroupInfo of another channel, a GroupInfo signed by a co-publisher's
/// device, a device that already holds the group, and bytes that are not a
/// GroupInfo.
#[test]
fn enter_refuses_what_it_must_before_writing() {
    let c = Channel::new(2, true, 0xA8);
    let (owner_info, access) = current_entry_point(&c);

    let copub_device = device(0xAF, &COPUB);
    let err = PublisherGroup::enter(&copub_device, &owner_info, &OWNER, &c.cid, &access)
        .err()
        .expect("co-publisher");
    assert_eq!(
        err.refusal_reason(),
        Some(RefusalReason::DeviceBindingInvalid)
    );

    let entering = device(0xAE, &OWNER);
    let other = cid(0xA9);
    assert!(PublisherGroup::enter(&entering, &owner_info, &OWNER, &other, &access).is_err());

    let copub = c.members.last().expect("copub");
    let copub_info = copub
        .pg
        .group_info(&copub.id)
        .expect("a co-publisher can export");
    let err = PublisherGroup::enter(&entering, &copub_info, &OWNER, &c.cid, &access)
        .err()
        .expect("signed by a co-publisher");
    assert_eq!(err.refusal_reason(), Some(RefusalReason::NotOwner));

    let holder = &c.members[1];
    assert!(PublisherGroup::enter(&holder.id, &owner_info, &OWNER, &c.cid, &access).is_err());

    assert!(PublisherGroup::enter(&entering, &kp(&entering), &OWNER, &c.cid, &access).is_err());
    assert!(PublisherGroup::enter(&entering, &[0u8; 3], &OWNER, &c.cid, &access).is_err());

    assert!(PublisherGroup::load(&entering, &OWNER, &c.cid)
        .expect("load")
        .is_none());
}

/// The held entry rides the storage snapshot: after a restore the group
/// opens still held, and merges then.
#[test]
fn a_held_entry_survives_a_restart() {
    let mut c = Channel::new(1, false, 0x98);
    let (info, access) = current_entry_point(&c);
    let entering = device(0x99, &OWNER);
    let entry = PublisherGroup::enter(&entering, &info, &OWNER, &c.cid, &access).expect("entered");
    let next = entry
        .group
        .staged_channel_seed(&entering)
        .expect("seed")
        .expect("some");
    let snapshot = entering.snapshot_storage().expect("snapshot");
    drop(entry.group);

    let restarted = device(0x99, &OWNER);
    restarted.restore_storage(&snapshot).expect("restore");
    let pg = PublisherGroup::load(&restarted, &OWNER, &c.cid)
        .expect("load")
        .expect("held");
    assert!(pg.has_staged_commit());
    assert_eq!(
        pg.staged_channel_seed(&restarted).expect("seed"),
        Some(next)
    );
    assert!(pg.staged_group_info(&restarted).expect("info").is_some());
    land_entry(&mut c, restarted, pg, &entry.commit);
    assert!(c.seeds().iter().all(|s| *s == next));
    let entered = c.members.last().expect("entered");
    assert!(!entered.pg.has_staged_commit());
}

/// The staged GroupInfo is kept only while its commit is staged.
#[test]
fn the_staged_group_info_lives_with_the_staged_commit() {
    let mut c = Channel::new(1, false, 0x88);
    let m = &mut c.members[0];
    assert!(m.pg.staged_group_info(&m.id).expect("none").is_none());
    m.pg.stage_rotate(&m.id).expect("rotate");
    assert!(m.pg.staged_group_info(&m.id).expect("some").is_some());
    assert!(m.pg.abandon_staged_commit(&m.id).expect("abandon"));
    assert!(m.pg.staged_group_info(&m.id).expect("none").is_none());
    m.pg.stage_rotate(&m.id).expect("rotate");
    let rotation_info = m.pg.staged_group_info(&m.id).expect("info").expect("some");
    let access = *m
        .pg
        .inner()
        .staged_ds_access_seed(&m.id)
        .expect("seed")
        .expect("some");
    assert!(m.pg.merge_staged_commit(&m.id).expect("merge"));
    assert!(m.pg.staged_group_info(&m.id).expect("none").is_none());
    let entering = device(0x89, &OWNER);
    PublisherGroup::enter(&entering, &rotation_info, &OWNER, &c.cid, &access)
        .expect("the saved GroupInfo of the landed epoch enters");
}

/// G-55: a wrapped entry opens only under the epoch-N access seed of this
/// group. A row sealed under another key, framed for another epoch, or
/// replayed into another channel's group is refused and never merged; the
/// right row then applies.
#[test]
fn a_wrapped_entry_opens_only_under_this_groups_epoch_key() {
    let mut c = Channel::new(1, false, 0x78);
    let (info, access) = current_entry_point(&c);
    let entering = device(0x79, &OWNER);

    // Another key: the same GroupInfo, a wrong seed.
    let mut wrong = access;
    wrong[0] ^= 1;
    let mut bad = PublisherGroup::enter(&entering, &info, &OWNER, &c.cid, &wrong).expect("built");
    let m = &mut c.members[0];
    let epoch = m.pg.epoch();
    assert!(m.pg.process_incoming(&m.id, &bad.commit).is_err());
    assert_eq!(m.pg.epoch(), epoch);
    assert_eq!(lakko_mls::epoch_of_message(&bad.commit), Some(epoch));
    assert_eq!(lakko_mls::epoch_after_message(&bad.commit), Some(epoch + 1));
    assert!(bad.group.abandon_staged_commit(&entering).expect("abandon"));

    // Another group: the same channel's row replayed into another channel.
    let mut other = Channel::new(1, false, 0x7A);
    let entry = PublisherGroup::enter(&entering, &info, &OWNER, &c.cid, &access).expect("built");
    let o = &mut other.members[0];
    let other_epoch = o.pg.epoch();
    assert!(o.pg.process_incoming(&o.id, &entry.commit).is_err());
    assert_eq!(o.pg.epoch(), other_epoch);

    // Another epoch: a member that moved on refuses the old row; one at the
    // row's epoch applies it.
    let mut later = Channel::new(1, false, 0x7B);
    let (later_info, later_access) = current_entry_point(&later);
    let later_entering = device(0x7C, &OWNER);
    let later_entry = PublisherGroup::enter(
        &later_entering,
        &later_info,
        &OWNER,
        &later.cid,
        &later_access,
    )
    .expect("built");
    let m = &mut later.members[0];
    let rotation = m.pg.stage_rotate(&m.id).expect("rotate");
    later.land(0, &rotation);
    let m = &mut later.members[0];
    let moved = m.pg.epoch();
    assert!(m.pg.process_incoming(&m.id, &later_entry.commit).is_err());
    assert_eq!(m.pg.epoch(), moved);

    land_entry(&mut c, entering, entry.group, &entry.commit);
    assert!(c.seeds().windows(2).all(|w| w[0] == w[1]));
}

/// G-55: a wrapped row is never processed raw. A row carrying the magic but
/// not opening is an error, and so is one that opens to a member's commit
/// instead of an external entry.
#[test]
fn a_wrapped_row_that_holds_no_entry_is_refused() {
    let mut c = Channel::new(2, false, 0x68);
    let m = &mut c.members[0];
    let epoch = m.pg.epoch();
    let mut forged = lakko_mls::WRAPPED_ENTRY_MAGIC.to_vec();
    forged.push(0x01);
    forged.extend_from_slice(&epoch.to_be_bytes());
    forged.extend_from_slice(&[0u8; 64]);
    assert!(m.pg.process_incoming(&m.id, &forged).is_err());
    assert_eq!(m.pg.epoch(), epoch);
}
