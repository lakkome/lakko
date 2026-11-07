// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! 2026-11-07: a device reads who founded a Welcome before joining it
//! (`Group::peek_welcome_founder`), so a Welcome that arrived under a retired
//! `contact` reach key (ADR-0060 section 1, amended 2026-11-06) can be judged
//! before the group exists. The peek writes nothing: a later join of the same
//! bytes works.

use lakko_mls::{group, key_package, Group, GroupKind, Identity};
use proptest::prelude::*;

fn wire(msg: openmls::framing::MlsMessageOut) -> Vec<u8> {
    group::encode_message(&msg).expect("encode message")
}

fn kp_wire(id: &Identity) -> Vec<u8> {
    key_package::serialize(&id.create_key_package().expect("key package")).expect("serialize kp")
}

fn account(tag: u8) -> Identity {
    Identity::from_seed(&[tag; 64]).expect("account identity")
}

fn device(owner: &Identity, tag: u8) -> Identity {
    Identity::from_seed_bound(&[tag; 32], &owner.ed25519_seed_bytes(), 1_700_000_000)
        .expect("bound device identity")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(8))]

    /// The peek names the founding account (not its device), and the same
    /// Welcome then joins, twice peeked or not.
    #[test]
    fn peek_names_the_founder_and_leaves_the_join_working(tag in 1u8..=0x3F, kind_one_to_one in any::<bool>()) {
        let a = account(0x40 | tag);
        let a1 = device(&a, 0x80 | tag);
        let b = account(0xC0 | tag);
        let b1 = device(&b, tag);
        let kind = if kind_one_to_one { GroupKind::OneToOne } else { GroupKind::Group };
        let mut a1_g = Group::create(&a1, kind).expect("a1 founds");
        let add = a1_g
            .add_members_from_wire(&a1, &[&kp_wire(&b1)])
            .expect("the founding commit admits bob");
        let welcome = wire(add.welcome);

        for _ in 0..2 {
            let peek = Group::peek_welcome_founder(&welcome, &b1).expect("peek");
            prop_assert_eq!(peek.founder_account_key.as_slice(), a.signature_public_key());
            prop_assert_eq!(&peek.group_id, &a1_g.group_id());
        }
        let joined = Group::join_from_welcome(&welcome, &b1).expect("the join still works");
        prop_assert_eq!(joined.group_id(), a1_g.group_id());
    }

    /// Bytes that are not a Welcome for this identity are an error, never a
    /// founder.
    #[test]
    fn a_peek_of_garbage_is_an_error(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let b1 = device(&account(0xC1), 0x01);
        prop_assert!(Group::peek_welcome_founder(&bytes, &b1).is_err());
    }
}

/// A Welcome for someone else does not open for this identity.
#[test]
fn a_welcome_for_another_device_is_an_error() {
    let a = account(0x41);
    let a1 = device(&a, 0x81);
    let b1 = device(&account(0xC2), 0x02);
    let stranger = device(&account(0xC3), 0x03);
    let mut a1_g = Group::create(&a1, GroupKind::OneToOne).expect("a1 founds");
    let add = a1_g
        .add_members_from_wire(&a1, &[&kp_wire(&b1)])
        .expect("admit bob");
    assert!(Group::peek_welcome_founder(&wire(add.welcome), &stranger).is_err());
}
