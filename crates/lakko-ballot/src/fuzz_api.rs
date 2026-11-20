// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Entry points for the fuzz targets. Not a supported API.

use crate::ballot::{self, PollKeys};
use crate::point::hash_to_point;
use crate::ring::Ring;

/// A ring of four keys nobody holds the secret of: each is a hash-to-point
/// output, so no ballot proven against it can be produced by anyone.
fn unholdable_ring() -> Option<Ring> {
    let mut keys = Vec::new();
    for i in 0u8..4 {
        keys.push(
            hash_to_point("lakko/poll/fuzz-ring/v1", &[i])
                .ok()?
                .compress()
                .to_bytes(),
        );
    }
    keys.sort_unstable();
    Ring::new(&keys).ok()
}

/// `ballot_open`: the row split, AEAD refusal, padding and ballot parser, and
/// proof verification under a fixed ring. Asserts that nothing verifies.
///
/// The first byte chooses the path: bit 0 set feeds the input to the row opener
/// under fixed poll keys; even feeds it to the padding and ballot parser
/// directly (the fuzzer cannot forge an AEAD tag), and then verifies whatever
/// decodes against the unholdable ring.
pub fn ballot_open(data: &[u8]) {
    let Some((&selector, rest)) = data.split_first() else {
        return;
    };
    let Ok(keys) = PollKeys::new(b"fuzz-group", &[7u8; 16], &[9u8; 32]) else {
        return;
    };
    let Some(ring) = unholdable_ring() else {
        return;
    };
    let decoded = if selector & 1 == 1 {
        ballot::open_row(&keys, rest)
    } else {
        ballot::fuzz_unpad_and_decode(rest)
    };
    if let Ok(mut b) = decoded {
        assert!(b.encode().len() <= ballot::MAX_BALLOT_LEN);
        // Point the ballot at a version the ring holds, so verification runs
        // past the version lookup.
        if let Some(hash) = ring.version_hash(0) {
            b.ring_hash = hash;
        }
        // Half the time also give it the one set that fits the ring, so
        // verification reaches the proof; the other half exercises the set
        // checks on whatever set the input carried.
        if selector & 2 == 2 {
            if let Ok(set) = ballot::AnonymitySet::new(&[0, 1, 2, 3]) {
                b.set = set;
            }
        }
        b.poll_id = *keys.poll_id();
        assert!(
            ballot::verify_ballot(&keys, &ring, &b).is_err(),
            "a ballot over an unholdable ring verified"
        );
    }
}

/// `ring_decode`: the ring encoding the hand-over carries. Asserts canonical
/// round trip.
pub fn ring_decode(data: &[u8]) {
    if let Ok(ring) = Ring::decode(data) {
        assert_eq!(ring.encode(), data, "ring decode is not canonical");
        for v in 0..ring.version_count() {
            assert!(ring.version_len(v).is_some());
        }
    }
}
