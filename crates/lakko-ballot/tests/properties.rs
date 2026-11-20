// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Property tests of ADR-0092 section 10 for `lakko-ballot`.

use lakko_ballot::ballot::{cast_with_set, is_ballot_row, open_row, verify_ballot, MAX_ROW_LEN};
use lakko_ballot::proof::Proof;
use lakko_ballot::{
    cast, draw_set, fold, open_and_verify, AnonymitySet, BallotError, Choices, CountedBallot,
    PollKeys, PollRules, Ring, VoterKey, ANONYMITY_SET,
};
use proptest::prelude::*;

const GROUP: &[u8] = b"group-id-0123456789abcdef0123456";
const SECRET: [u8; 32] = [0x5a; 32];

fn seed(i: u32) -> [u8; 32] {
    let mut s = [0u8; 32];
    s[..4].copy_from_slice(&i.to_be_bytes());
    s[4] = 1;
    s
}

fn voter(i: u32) -> VoterKey {
    VoterKey::from_ed25519_seed(&seed(i))
}

fn sorted_keys(ids: impl IntoIterator<Item = u32>) -> Vec<[u8; 32]> {
    let mut keys: Vec<[u8; 32]> = ids.into_iter().map(|i| *voter(i).public_key()).collect();
    keys.sort_unstable();
    keys
}

fn poll(id: u8) -> PollKeys {
    PollKeys::new(GROUP, &[id; 16], &SECRET).unwrap()
}

fn one(i: u8) -> Choices {
    Choices::new(&[i]).unwrap()
}

fn all(len: u32) -> AnonymitySet {
    AnonymitySet::new(&(0..len).collect::<Vec<_>>()).unwrap()
}

#[test]
fn completeness_for_every_position_of_small_rings() {
    for size in [1u32, 2, 3, 5, 8] {
        let ring = Ring::new(&sorted_keys(1..=size)).unwrap();
        let keys = poll(1);
        for i in 1..=size {
            let (version, set) = draw_set(&keys, &ring, &voter(i), None).unwrap();
            assert_eq!((version, set), (0, all(size)));
            let row = cast(&keys, &ring, &voter(i), &one(0), None).unwrap().row;
            assert!(is_ballot_row(&row));
            assert!(row.len() <= MAX_ROW_LEN);
            let verified = open_and_verify(&keys, &ring, &row).unwrap();
            assert_eq!(verified.tag, keys.tag_of(&voter(i)));
        }
    }
}

#[test]
fn in_a_large_ring_the_set_is_256_keys_drawn_deterministically() {
    let mut ring = Ring::new(&sorted_keys(0..300)).unwrap();
    let keys = poll(1);
    let (version, set) = draw_set(&keys, &ring, &voter(7), None).unwrap();
    assert_eq!(version, 0);
    assert_eq!(set.indices().len(), ANONYMITY_SET);
    let own = u32::try_from(ring.position(voter(7).public_key()).unwrap()).unwrap();
    assert!(set.indices().contains(&own));
    assert!(set.check(300).is_ok());
    // Another device of the account (same seed) draws the same set at the
    // same version, and so does this one at that version after growth.
    assert_eq!(
        draw_set(&keys, &ring, &voter(7), None).unwrap(),
        (0, set.clone())
    );
    ring.extend(&sorted_keys(300..310)).unwrap();
    assert_eq!(
        draw_set(&keys, &ring, &voter(7), Some(0)).unwrap(),
        (0, set.clone())
    );
    // Another poll gives another set; another voter too; another version too.
    assert_ne!(
        draw_set(&poll(2), &ring, &voter(7), Some(0)).unwrap().1,
        set
    );
    assert_ne!(draw_set(&keys, &ring, &voter(8), Some(0)).unwrap().1, set);
    assert_ne!(draw_set(&keys, &ring, &voter(7), Some(1)).unwrap().1, set);
    // A version this device does not hold yet.
    assert_eq!(
        draw_set(&keys, &ring, &voter(7), Some(9)),
        Err(BallotError::UnknownRingVersion)
    );
}

#[test]
fn a_first_vote_draws_from_the_newest_version_and_a_revote_reuses_the_recorded_one() {
    let mut ring = Ring::new(&sorted_keys(0..300)).unwrap();
    ring.extend(&sorted_keys([1000])).unwrap();
    ring.extend(&sorted_keys([1001, 1002])).unwrap();
    let keys = poll(1);
    // First vote of a member of version 0: the newest version, 2.
    let first = cast(&keys, &ring, &voter(7), &one(0), None).unwrap();
    assert_eq!(first.set_version, 2);
    let first_set = open_row(&keys, &first.row).unwrap().set;
    assert!(first_set.check(303).is_ok());
    assert_eq!(
        open_and_verify(&keys, &ring, &first.row).unwrap().version,
        2
    );
    // A later joiner's first vote names the same newest version, not its join
    // version.
    let joiner = cast(&keys, &ring, &voter(1000), &one(0), None).unwrap();
    assert_eq!(joiner.set_version, 2);
    // The ring grows; another device of the account (the same key) revotes
    // with the version recorded in the own poll-choice register and proves
    // over the identical set.
    ring.extend(&sorted_keys(2000..2010)).unwrap();
    let revote = cast(&keys, &ring, &voter(7), &one(1), Some(first.set_version)).unwrap();
    assert_eq!(revote.set_version, 2);
    assert_eq!(open_row(&keys, &revote.row).unwrap().set, first_set);
    let a = open_and_verify(&keys, &ring, &first.row).unwrap();
    let b = open_and_verify(&keys, &ring, &revote.row).unwrap();
    assert_eq!(a.tag, b.tag);
    // The fallback: the version of the account's own readable ballot.
    assert_eq!(
        lakko_ballot::own_set_version(&keys, &voter(7), &[a.clone(), b]),
        Some(2)
    );
    assert_eq!(lakko_ballot::own_set_version(&keys, &voter(8), &[a]), None);
    // A recorded version that does not hold the voter.
    assert_eq!(
        cast(&keys, &ring, &voter(2000), &one(0), Some(2)),
        Err(BallotError::NotInRing)
    );
}

#[test]
fn one_key_with_two_different_sets_has_one_tag_and_counts_once() {
    let ring = Ring::new(&sorted_keys(0..300)).unwrap();
    let keys = poll(1);
    let me = voter(5);
    let own = u32::try_from(ring.position(me.public_key()).unwrap()).unwrap();
    let (_, drawn) = draw_set(&keys, &ring, &me, None).unwrap();
    // A different well-formed set containing the voter: the verifier cannot
    // recompute the draw and accepts it.
    let mut other: Vec<u32> = (0..300)
        .filter(|&i| i != own)
        .take(ANONYMITY_SET - 1)
        .collect();
    other.push(own);
    other.sort_unstable();
    let other = AnonymitySet::new(&other).unwrap();
    assert_ne!(other, drawn);
    let a = open_and_verify(
        &keys,
        &ring,
        &cast(&keys, &ring, &me, &one(0), None).unwrap().row,
    )
    .unwrap();
    let b = open_and_verify(
        &keys,
        &ring,
        &cast_with_set(&keys, &ring, 0, &other, &me, &one(1)).unwrap(),
    )
    .unwrap();
    assert_eq!(a.tag, b.tag);
    let rules = PollRules {
        option_count: 2,
        multiple: false,
        close_message_id: None,
        close_at_ms: None,
    };
    let tally = fold(
        &rules,
        &[
            CountedBallot {
                message_id: 1,
                created_at_ms: 1,
                tag: a.tag,
                choices: a.choices,
            },
            CountedBallot {
                message_id: 2,
                created_at_ms: 2,
                tag: b.tag,
                choices: b.choices,
            },
        ],
    );
    assert_eq!(tally.voters, 1);
    assert_eq!(tally.counts, vec![0, 1]);
}

#[test]
fn a_ballot_verifies_only_for_its_version_set_poll_group_and_choices() {
    let mut ring = Ring::new(&sorted_keys(0..300)).unwrap();
    ring.extend(&sorted_keys([1000])).unwrap();
    let keys = poll(1);
    let row = cast(&keys, &ring, &voter(3), &one(1), Some(0)).unwrap().row;
    let ballot = open_row(&keys, &row).unwrap();
    assert!(verify_ballot(&keys, &ring, &ballot).is_ok());
    // The same set relabelled with version 1 (it fits: 256 indices < 301).
    let mut relabelled = ballot.clone();
    relabelled.ring_hash = ring.version_hash(1).unwrap();
    assert_eq!(
        verify_ballot(&keys, &ring, &relabelled),
        Err(BallotError::ProofRejected)
    );
    // One index swapped for another valid one.
    let mut swapped = ballot.clone();
    let mut indices = swapped.set.indices().to_vec();
    let free = (0..300).find(|i| !indices.contains(i)).unwrap();
    let pos = indices
        .iter()
        .position(|&i| i != u32::try_from(ring.position(voter(3).public_key()).unwrap()).unwrap())
        .unwrap();
    indices[pos] = free;
    indices.sort_unstable();
    swapped.set = AnonymitySet::new(&indices).unwrap();
    assert_eq!(
        verify_ballot(&keys, &ring, &swapped),
        Err(BallotError::ProofRejected)
    );
    // Unknown version.
    let mut unknown = ballot.clone();
    unknown.ring_hash = [0u8; 32];
    assert_eq!(
        verify_ballot(&keys, &ring, &unknown),
        Err(BallotError::UnknownRingVersion)
    );
    // Another poll: another seal key. Another group: opens, does not verify.
    assert_eq!(
        open_and_verify(&poll(2), &ring, &row),
        Err(BallotError::RowDidNotOpen)
    );
    let other_group = PollKeys::new(b"another-group", &[1; 16], &SECRET).unwrap();
    assert_eq!(
        open_and_verify(&other_group, &ring, &row),
        Err(BallotError::ProofRejected)
    );
    // Other choices.
    let mut changed = ballot;
    changed.choices = one(0);
    assert_eq!(
        verify_ballot(&keys, &ring, &changed),
        Err(BallotError::ProofRejected)
    );
}

#[test]
fn ill_fitting_sets_and_sets_over_bad_keys_are_refused() {
    let mut ring = Ring::new(&sorted_keys(1..=3)).unwrap();
    let keys = poll(1);
    let ballot = open_row(
        &keys,
        &cast(&keys, &ring, &voter(1), &one(0), None).unwrap().row,
    )
    .unwrap();
    ring.extend(&[[0u8; 32]]).unwrap(); // version 1: four keys, the last not a point
                                        // Version 0 still verifies.
    assert!(verify_ballot(&keys, &ring, &ballot).is_ok());
    // A short set for version 1, and a set past its end.
    let mut short = ballot.clone();
    short.ring_hash = ring.version_hash(1).unwrap();
    assert_eq!(
        verify_ballot(&keys, &ring, &short),
        Err(BallotError::InvalidSet)
    );
    let mut past = short.clone();
    past.set = AnonymitySet::new(&[0, 1, 2, 9]).unwrap();
    assert_eq!(
        verify_ballot(&keys, &ring, &past),
        Err(BallotError::InvalidSet)
    );
    // The full set of version 1 selects the bad key.
    let mut bad = short;
    bad.set = all(4);
    assert_eq!(
        verify_ballot(&keys, &ring, &bad),
        Err(BallotError::RingVersionUnusable)
    );
    assert_eq!(
        cast_with_set(&keys, &ring, 1, &all(4), &voter(1), &one(0)),
        Err(BallotError::RingVersionUnusable)
    );
}

#[test]
fn the_draw_skips_keys_that_are_not_points() {
    let mut keys_v0 = sorted_keys(0..299);
    keys_v0.push([0xffu8; 32]); // sorts last; y >= p, not a valid encoding
    let ring = Ring::new(&keys_v0).unwrap();
    let keys = poll(1);
    for v in 0..5 {
        let (_, set) = draw_set(&keys, &ring, &voter(v), None).unwrap();
        assert!(!set.indices().contains(&299));
    }
}

#[test]
fn a_non_member_cannot_produce_a_verifying_ballot() {
    let ring = Ring::new(&sorted_keys(1..=4)).unwrap();
    let keys = poll(1);
    assert_eq!(
        cast(&keys, &ring, &voter(99), &one(0), None),
        Err(BallotError::NotInRing)
    );
    // A non-member proves over a ring that includes itself and relabels the
    // ballot with the real ring's hash: the transcript binds the keys.
    let mut widened = sorted_keys(1..=4);
    widened.push(*voter(99).public_key());
    widened.sort_unstable();
    let wide = Ring::new(&widened).unwrap();
    let row = cast(&keys, &wide, &voter(99), &one(0), None).unwrap().row;
    assert_eq!(
        open_and_verify(&keys, &ring, &row),
        Err(BallotError::UnknownRingVersion)
    );
    let mut ballot = open_row(&keys, &row).unwrap();
    ballot.ring_hash = ring.version_hash(0).unwrap();
    ballot.set = all(4);
    assert_eq!(
        verify_ballot(&keys, &ring, &ballot),
        Err(BallotError::ProofRejected)
    );
    // A member's valid proof with a non-member's tag attached.
    let mut ballot = open_row(
        &keys,
        &cast(&keys, &ring, &voter(1), &one(0), None).unwrap().row,
    )
    .unwrap();
    ballot.tag = keys.tag_of(&voter(99));
    assert_eq!(
        verify_ballot(&keys, &ring, &ballot),
        Err(BallotError::ProofRejected)
    );
}

#[test]
fn a_ring_extension_never_counts_one_account_twice() {
    let mut ring = Ring::new(&sorted_keys(1..=3)).unwrap();
    assert_eq!(ring.extend(&sorted_keys(1..=3)).unwrap(), None);
    ring.extend(&sorted_keys(2..=5)).unwrap();
    assert_eq!(ring.keys().len(), 5);
    let keys = poll(1);
    let rules = PollRules {
        option_count: 2,
        multiple: false,
        close_message_id: None,
        close_at_ms: None,
    };
    let mut counted = Vec::new();
    for (id, version, len) in [(1i64, 0usize, 3u32), (2, 1, 5)] {
        let row = cast_with_set(&keys, &ring, version, &all(len), &voter(2), &one(0)).unwrap();
        let v = open_and_verify(&keys, &ring, &row).unwrap();
        counted.push(CountedBallot {
            message_id: id,
            created_at_ms: id,
            tag: v.tag,
            choices: v.choices,
        });
    }
    let tally = fold(&rules, &counted);
    assert_eq!(tally.voters, 1);
    assert_eq!(tally.counts, vec![1, 0]);
}

#[test]
fn every_tampered_proof_byte_is_refused() {
    let ring = Ring::new(&sorted_keys(1..=4)).unwrap();
    let keys = poll(1);
    let ballot = open_row(
        &keys,
        &cast(&keys, &ring, &voter(4), &one(1), None).unwrap().row,
    )
    .unwrap();
    let encoded = ballot.proof.encode();
    // Skip byte 0 (the digit count, which changes the length).
    for i in 1..encoded.len() {
        let mut bad = encoded.clone();
        bad[i] ^= 0x01;
        let Ok(proof) = Proof::decode(&bad) else {
            continue;
        };
        let mut tampered = ballot.clone();
        tampered.proof = proof;
        assert!(
            verify_ballot(&keys, &ring, &tampered).is_err(),
            "byte {i} flip verified"
        );
    }
}

#[test]
fn fuzz_entry_points_accept_a_real_row_and_hold_their_assertions() {
    let keys = PollKeys::new(b"fuzz-group", &[7u8; 16], &[9u8; 32]).unwrap();
    let ring = Ring::new(&sorted_keys(1..=4)).unwrap();
    let row = cast(&keys, &ring, &voter(2), &one(0), None).unwrap().row;
    for selector in [1u8, 3] {
        let mut input = vec![selector];
        input.extend_from_slice(&row);
        lakko_ballot::fuzz_api::ballot_open(&input);
    }
    lakko_ballot::fuzz_api::ballot_open(&[0u8; 300]);
    lakko_ballot::fuzz_api::ring_decode(&ring.encode());
    lakko_ballot::fuzz_api::ring_decode(&[0u8; 10]);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn hostile_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..2600)) {
        let ring = Ring::new(&sorted_keys(1..=2)).unwrap();
        let keys = poll(1);
        let _ = open_and_verify(&keys, &ring, &bytes);
        let mut prefixed = b"lakko/poll-ballot/v1\0\x01".to_vec();
        prefixed.extend_from_slice(&bytes);
        let _ = open_and_verify(&keys, &ring, &prefixed);
        let _ = Ring::decode(&bytes);
        let _ = Proof::decode(&bytes);
        let _ = lakko_ballot::Ballot::decode(&bytes);
    }

    #[test]
    fn tally_is_independent_of_order(
        entries in proptest::collection::vec((0i64..20, 0u8..4, proptest::collection::btree_set(0u8..4, 0..3)), 0..30),
        seed in any::<u64>(),
        multiple in any::<bool>(),
    ) {
        let ballots: Vec<CountedBallot> = entries
            .iter()
            .map(|(id, tag, set)| CountedBallot {
                message_id: *id,
                created_at_ms: *id,
                tag: [*tag; 32],
                choices: Choices::new(&set.iter().copied().collect::<Vec<_>>()).unwrap(),
            })
            .collect();
        let rules = PollRules { option_count: 3, multiple, close_message_id: Some(15), close_at_ms: None };
        let mut shuffled = ballots.clone();
        let mut state = seed;
        for i in (1..shuffled.len()).rev() {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let j = usize::try_from(state >> 33).unwrap() % (i + 1);
            shuffled.swap(i, j);
        }
        prop_assert_eq!(fold(&rules, &ballots), fold(&rules, &shuffled));
    }

    #[test]
    fn ring_encoding_round_trips(extra in proptest::collection::vec(proptest::collection::btree_set(10u32..40, 0..4), 0..4)) {
        let mut ring = Ring::new(&sorted_keys(1..=3)).unwrap();
        for batch in extra {
            ring.extend(&batch.into_iter().map(|i| *voter(i).public_key()).collect::<Vec<_>>()).unwrap();
        }
        let again = Ring::decode(&ring.encode()).unwrap();
        prop_assert_eq!(again.encode(), ring.encode());
        for v in 0..ring.version_count() {
            prop_assert_eq!(again.version_hash(v), ring.version_hash(v));
        }
    }

    #[test]
    fn a_set_decodes_only_in_canonical_form(indices in proptest::collection::vec(any::<u32>(), 0..300)) {
        if let Ok(set) = AnonymitySet::new(&indices) {
            prop_assert!(set.indices().windows(2).all(|w| w[0] < w[1]));
            prop_assert!(set.indices().len() <= ANONYMITY_SET);
        }
    }
}
