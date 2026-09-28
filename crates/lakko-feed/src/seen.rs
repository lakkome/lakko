// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! How far a conversation is read (ADR-0058 section 5).
//!
//! A read receipt (ADR-0024) and the DeviceGroup `ReadCursor` say "read up
//! to message N". The feed shows messages out of order, so the highest id
//! the person has seen is not a truthful N: messages below it may never have
//! reached the screen. The truthful N is the end of the contiguous seen
//! prefix, and this is its one definition.

/// The highest id in `ids` such that every id in `ids` at or below it is in
/// `seen`; `None` when the lowest id is unseen or `ids` is empty.
///
/// `ids` are the conversation's message ids this device holds, in any order
/// and possibly repeated; `seen` likewise. The person's own messages belong
/// in `seen`: the client adds them when it sends.
#[must_use]
pub fn contiguous_seen(ids: &[i64], seen: &[i64]) -> Option<i64> {
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let mut seen = seen.to_vec();
    seen.sort_unstable();
    seen.dedup();
    let mut last = None;
    for id in ids {
        if seen.binary_search(&id).is_err() {
            break;
        }
        last = Some(id);
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn stops_at_the_first_gap() {
        assert_eq!(contiguous_seen(&[1, 2, 3, 5, 8], &[1, 2, 3, 8]), Some(3));
        assert_eq!(contiguous_seen(&[1, 2, 3], &[2, 3]), None);
        assert_eq!(contiguous_seen(&[], &[1]), None);
        assert_eq!(contiguous_seen(&[3, 1, 2, 2], &[2, 1, 3]), Some(3));
    }

    proptest! {
        #[test]
        fn never_claims_an_unseen_message(
            ids in proptest::collection::vec(any::<i64>(), 0..64),
            seen in proptest::collection::vec(any::<i64>(), 0..64),
        ) {
            if let Some(n) = contiguous_seen(&ids, &seen) {
                for id in ids.iter().filter(|&&id| id <= n) {
                    prop_assert!(seen.contains(id));
                }
            }
        }
    }
}
