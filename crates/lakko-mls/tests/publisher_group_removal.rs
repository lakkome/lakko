// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Removing a publisher from a channel's PublisherGroup, and a publisher
//! stepping down ([ADR-0087] section 15, note of 2026-11-05).
//!
//! A PublisherGroup carries no role table, so who may remove whom is held by
//! the owner's device (`remove_publisher` refuses anyone but the owner, and
//! refuses the owner as a target). What these tests pin is what the commit
//! does for the channel's key:
//!
//! - a removed publisher cannot derive the channel seed of the epoch the
//!   removal opens, nor of any later rotation, whether or not it applies the
//!   removal commit;
//! - the remaining publishers derive the owner's seed;
//! - a step-down is an application message that changes nothing until the
//!   owner applies it: the owner can still rotate, and then removes the
//!   sender's account.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use lakko_mls::{
    is_publisher_step_down, key_package, ChannelId, Identity, Incoming, PublisherGroup,
    CHANNEL_ID_LEN,
};
use proptest::prelude::*;

/// A channel of `publishers + 1` members: the owner at index 0, the
/// publishers after it, every one joined through its Welcome.
struct Channel {
    owner: Identity,
    owner_pg: PublisherGroup,
    publishers: Vec<(Identity, PublisherGroup)>,
    cid: ChannelId,
}

fn channel(publishers: usize, tag: u8) -> Channel {
    let owner = Identity::generate("pg-owner").expect("owner");
    let cid = ChannelId::try_from_slice(&[tag; CHANNEL_ID_LEN]).expect("cid");
    let mut owner_pg = PublisherGroup::create(&owner, &cid).expect("create");
    let gid = owner_pg.group_id();
    let mut joined: Vec<(Identity, PublisherGroup)> = Vec::new();
    for i in 0..publishers {
        let id = Identity::generate(&format!("pg-publisher-{i}")).expect("publisher");
        let kp = key_package::serialize(&id.create_key_package().expect("kp")).expect("kp wire");
        let addition = owner_pg
            .inner_mut()
            .add_member_from_wire(&owner, &kp)
            .expect("add");
        let commit = lakko_mls::group::encode_message(&addition.commit).expect("commit");
        let welcome = lakko_mls::group::encode_message(&addition.welcome).expect("welcome");
        for (pid, pg) in &mut joined {
            apply(pg, pid, &commit);
        }
        let pg = PublisherGroup::join_from_welcome(&id, &welcome, &gid, &cid).expect("join");
        joined.push((id, pg));
    }
    Channel {
        owner,
        owner_pg,
        publishers: joined,
        cid,
    }
}

fn apply(pg: &mut PublisherGroup, id: &Identity, wire: &[u8]) {
    let out = pg
        .inner_mut()
        .process_incoming_verifying_roles(id, wire)
        .expect("apply");
    assert!(matches!(out, Incoming::Commit), "a commit");
}

fn account_of(id: &Identity) -> Vec<u8> {
    id.signature_public_key().to_vec()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(8))]

    /// The headline property: after the owner removes publisher `victim`,
    /// that publisher holds no seed of the new epoch or of any later
    /// rotation, whether it applied the removal or never saw it, while every
    /// remaining publisher derives the owner's seed.
    #[test]
    fn a_removed_publisher_cannot_derive_the_next_epochs_seed(
        publishers in 1usize..4,
        victim_pick in 0usize..4,
        rotations_before in 0usize..3,
        rotations_after in 1usize..3,
        victim_applies in any::<bool>(),
    ) {
        let victim = victim_pick % publishers;
        let mut c = channel(publishers, 0x31);
        for _ in 0..rotations_before {
            let commit = c.owner_pg.rotate(&c.owner).expect("rotate");
            for (id, pg) in &mut c.publishers {
                apply(pg, id, &commit);
            }
        }
        let before = c.owner_pg.channel_seed(&c.owner).expect("seed");
        let victim_key = account_of(&c.publishers[victim].0);
        let removal = c
            .owner_pg
            .remove_publisher(&c.owner, &victim_key)
            .expect("owner removes");
        prop_assert_eq!(c.owner_pg.members().len(), publishers);
        let after = c.owner_pg.channel_seed(&c.owner).expect("seed");
        prop_assert_ne!(after, before);

        for (i, (id, pg)) in c.publishers.iter_mut().enumerate() {
            if i == victim {
                if victim_applies {
                    let _ = pg.inner_mut().process_incoming_verifying_roles(id, &removal);
                    prop_assert!(!pg.is_active(), "the removed device knows it is out");
                }
                let seed = pg.channel_seed(id);
                prop_assert!(seed.map_or(true, |s| s != after), "no seed of the new epoch");
            } else {
                apply(pg, id, &removal);
                prop_assert_eq!(pg.channel_seed(id).expect("seed"), after);
            }
        }

        for _ in 0..rotations_after {
            let commit = c.owner_pg.rotate(&c.owner).expect("rotate");
            let now = c.owner_pg.channel_seed(&c.owner).expect("seed");
            for (i, (id, pg)) in c.publishers.iter_mut().enumerate() {
                if i == victim {
                    // The rotation cannot be applied, and nothing the device
                    // still holds derives the new seed.
                    let applied = pg.inner_mut().process_incoming_verifying_roles(id, &commit);
                    prop_assert!(applied.is_err() || !pg.is_active());
                    prop_assert!(pg.channel_seed(id).map_or(true, |s| s != now));
                } else {
                    apply(pg, id, &commit);
                    prop_assert_eq!(pg.channel_seed(id).expect("seed"), now);
                }
            }
        }
    }

    /// A step-down changes nothing until the owner applies it: the owner's
    /// rotations still land on every publisher, the step-down included,
    /// and the owner's removal of the sender then cuts it off.
    #[test]
    fn step_down_leaves_the_owner_able_to_rotate(
        publishers in 1usize..4,
        leaver_pick in 0usize..4,
        rotations_between in 0usize..3,
    ) {
        let leaver = leaver_pick % publishers;
        let mut c = channel(publishers, 0x32);
        let (lid, lpg) = &mut c.publishers[leaver];
        let message = lpg.step_down(lid).expect("step down");

        let incoming = c
            .owner_pg
            .inner_mut()
            .process_incoming_verifying_roles(&c.owner, &message)
            .expect("owner reads it");
        let Incoming::Application { bytes, sender_account, .. } = incoming else {
            panic!("an application message");
        };
        prop_assert!(is_publisher_step_down(&bytes, &c.cid));
        let sender = sender_account.expect("sender resolves");
        prop_assert_eq!(&sender, &account_of(&c.publishers[leaver].0));

        for _ in 0..rotations_between {
            let commit = c.owner_pg.rotate(&c.owner).expect("owner still rotates");
            for (id, pg) in &mut c.publishers {
                apply(pg, id, &commit);
            }
            let seed = c.owner_pg.channel_seed(&c.owner).expect("seed");
            prop_assert_eq!(c.publishers[leaver].1.channel_seed(&c.publishers[leaver].0).expect("seed"), seed);
        }

        let removal = c.owner_pg.remove_publisher(&c.owner, &sender).expect("owner applies it");
        let after = c.owner_pg.channel_seed(&c.owner).expect("seed");
        let commit = c.owner_pg.rotate(&c.owner).expect("owner rotates after");
        let now = c.owner_pg.channel_seed(&c.owner).expect("seed");
        prop_assert_ne!(after, now);
        for (i, (id, pg)) in c.publishers.iter_mut().enumerate() {
            if i == leaver {
                let _ = pg.inner_mut().process_incoming_verifying_roles(id, &removal);
                prop_assert!(!pg.is_active());
                prop_assert!(pg.channel_seed(id).map_or(true, |s| s != now));
            } else {
                apply(pg, id, &removal);
                apply(pg, id, &commit);
                prop_assert_eq!(pg.channel_seed(id).expect("seed"), now);
            }
        }
    }
}

#[test]
fn only_the_owner_removes_and_the_owner_is_never_removed() {
    let mut c = channel(2, 0x33);
    let owner_key = account_of(&c.owner);
    let other = account_of(&c.publishers[1].0);

    // A co-publisher may not remove anyone.
    let (pid, ppg) = &mut c.publishers[0];
    assert!(!ppg.is_owned_by(pid));
    let refused = ppg.remove_publisher(pid, &other);
    assert!(refused.is_err(), "a co-publisher is refused");

    // The owner may not remove itself, nor a key nobody holds.
    assert!(c.owner_pg.is_owned_by(&c.owner));
    assert!(c.owner_pg.remove_publisher(&c.owner, &owner_key).is_err());
    assert!(c.owner_pg.remove_publisher(&c.owner, &[0x55; 32]).is_err());
    let epoch = c.owner_pg.epoch();
    assert_eq!(c.owner_pg.epoch(), epoch, "a refusal commits nothing");

    // The owner does not step down.
    assert!(c.owner_pg.step_down(&c.owner).is_err());
}

#[test]
fn the_members_list_names_every_publisher_by_account_key() {
    let c = channel(2, 0x34);
    let mut keys: Vec<Vec<u8>> = c
        .owner_pg
        .members()
        .into_iter()
        .map(|m| m.account_key)
        .collect();
    keys.sort();
    let mut expected = vec![account_of(&c.owner)];
    expected.extend(c.publishers.iter().map(|(id, _)| account_of(id)));
    expected.sort();
    assert_eq!(keys, expected);
}

#[test]
fn a_step_down_names_its_channel() {
    let a = ChannelId::try_from_slice(&[1; CHANNEL_ID_LEN]).unwrap();
    let b = ChannelId::try_from_slice(&[2; CHANNEL_ID_LEN]).unwrap();
    let wire = lakko_mls::compose_publisher_step_down(&a);
    assert_eq!(wire.len(), lakko_mls::PUBLISHER_STEP_DOWN_LEN);
    assert!(is_publisher_step_down(&wire, &a));
    assert!(!is_publisher_step_down(&wire, &b));
    assert!(lakko_mls::is_publisher_step_down_envelope(&wire));
    let mut longer = wire.clone();
    longer.push(0);
    assert!(!is_publisher_step_down(&longer, &a));
    assert!(!lakko_mls::is_publisher_step_down_envelope(&longer));
    let mut other_version = wire;
    other_version[lakko_mls::PUBLISHER_STEP_DOWN_DOMAIN_V1.len()] = 2;
    assert!(!lakko_mls::is_publisher_step_down_envelope(&other_version));
}
