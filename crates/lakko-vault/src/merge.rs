// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The merge of saved messages (ADR-0064 section 2).
//!
//! Any device of the account writes a vault, and the storage service accepts
//! a write only at `current + 1`. A device that loses that race fetches what
//! is there, merges it with what it holds, and writes again. So the merge has
//! to give every device the same answer whatever order it sees the two sides
//! in.
//!
//! ## The rule
//!
//! Rows are keyed by `message_id`, the Delivery Service journal id every
//! device observes identically. The result is the union of the rows; for one
//! id:
//!
//! 1. the row with the higher `last_op_id` wins: it has applied a later
//!    edit, delete or reaction;
//! 2. on an equal `last_op_id`, the row whose canonical encoding
//!    ([`crate::record`]) is bytewise larger wins.
//!
//! Step 2 has no meaning beyond being the same choice on every device. Two
//! honest devices that applied the same operations to one message hold the
//! same row, and the tie is between equals.
//!
//! The output is sorted by `message_id` and holds each id once.
//!
//! ## Total
//!
//! Per id the winner is the greatest row under one total order, so the merge
//! is commutative, associative and idempotent (property tests in
//! `tests/merge_properties.rs`). It accepts any rows, including a row that
//! repeats an id within one input and a row outside the codec's bounds. Such
//! a row has no canonical encoding; the bytes the encoder would write are
//! compared instead (a length that does not fit its prefix saturates), and
//! should two different rows ever write the same bytes a field-by-field
//! comparison decides, so the order stays total. A row outside the bounds is
//! still refused when the result is encoded.
//!
//! A deleted message is a row like any other: the device that applied the
//! delete stores it emptied with `deleted` set and a `last_op_id` of the
//! delete, which outranks the row it replaces.

use std::cmp::Ordering;
use std::collections::btree_map::Entry;
use std::collections::BTreeMap;

use crate::record::VaultMessage;

/// Which of two rows with one `message_id` a merge keeps: `Greater` when it
/// is `a`.
fn precedence(a: &VaultMessage, b: &VaultMessage) -> Ordering {
    a.last_op_id.cmp(&b.last_op_id).then_with(|| {
        if a == b {
            return Ordering::Equal;
        }
        a.wire_bytes()
            .cmp(&b.wire_bytes())
            // It would take two different rows outside the codec's bounds
            // whose saturated encodings coincide; no such pair is known.
            // Kept so the order is total by construction, not by that
            // argument.
            .then_with(|| a.cmp(b))
    })
}

/// Merge two sets of saved messages (see the module docs).
#[must_use]
pub fn merge_messages(a: &[VaultMessage], b: &[VaultMessage]) -> Vec<VaultMessage> {
    let mut rows: BTreeMap<u64, &VaultMessage> = BTreeMap::new();
    for row in a.iter().chain(b) {
        match rows.entry(row.message_id) {
            Entry::Vacant(slot) => {
                slot.insert(row);
            },
            Entry::Occupied(mut slot) => {
                if precedence(row, slot.get()) == Ordering::Greater {
                    slot.insert(row);
                }
            },
        }
    }
    rows.into_values().cloned().collect()
}

#[cfg(test)]
mod tests {
    use std::slice::from_ref;

    use super::*;
    use crate::record::{Direction, VaultReaction};

    fn row(id: u64, last_op_id: u64, text: &str) -> VaultMessage {
        VaultMessage {
            message_id: id,
            direction: Direction::Incoming,
            author: Some([7; 32]),
            created_at_ms: 1_700_000_000_000,
            last_op_id,
            edited: false,
            deleted: false,
            text: text.into(),
            attachments: Vec::new(),
            reply: None,
            forward: None,
            reactions: Vec::new(),
            call: None,
            unsupported_version: None,
            content: None,
        }
    }

    #[test]
    fn the_union_is_sorted_by_message_id() {
        let a = [row(5, 5, "five"), row(1, 1, "one")];
        let b = [row(3, 3, "three"), row(9, 9, "nine")];
        let merged = merge_messages(&a, &b);
        let ids: Vec<u64> = merged.iter().map(|m| m.message_id).collect();
        assert_eq!(ids, [1, 3, 5, 9]);
        assert_eq!(merge_messages(&[], &[]), Vec::new());
        assert_eq!(merge_messages(&a, &[]), [a[1].clone(), a[0].clone()]);
    }

    #[test]
    fn the_higher_last_op_id_wins() {
        let original = row(4, 4, "hello");
        let mut edited = row(4, 11, "hello, edited");
        edited.edited = true;
        for (a, b) in [(&original, &edited), (&edited, &original)] {
            assert_eq!(merge_messages(from_ref(a), from_ref(b)), from_ref(&edited));
        }
    }

    #[test]
    fn a_deletion_reaches_the_saved_copy() {
        let original = row(4, 4, "regret");
        let mut deleted = row(4, 20, "");
        deleted.deleted = true;
        let merged = merge_messages(&[original, row(5, 5, "kept")], &[deleted.clone()]);
        assert_eq!(merged, [deleted, row(5, 5, "kept")]);
    }

    #[test]
    fn an_equal_last_op_id_falls_to_the_larger_encoding() {
        // Same id, same last_op_id, different content: "b" encodes larger.
        let smaller = row(4, 9, "a");
        let larger = row(4, 9, "b");
        assert!(larger.wire_bytes() > smaller.wire_bytes());
        for (a, b) in [(&smaller, &larger), (&larger, &smaller)] {
            assert_eq!(merge_messages(from_ref(a), from_ref(b)), from_ref(&larger));
        }
        // The encoding decides, not the text alone: a longer text has a
        // larger length prefix, which comes first.
        let short = row(4, 9, "zz");
        let long = row(4, 9, "aaa");
        assert!(long.wire_bytes() > short.wire_bytes());
        assert_eq!(merge_messages(&[short], from_ref(&long)), [long]);
    }

    #[test]
    fn a_repeated_id_within_one_input_is_merged_too() {
        let merged = merge_messages(&[row(2, 2, "old"), row(2, 6, "new"), row(2, 3, "mid")], &[]);
        assert_eq!(merged, [row(2, 6, "new")]);
    }

    #[test]
    fn rows_outside_the_codec_bounds_still_merge_deterministically() {
        let mut a = row(1, 1, "x");
        a.reactions = vec![VaultReaction {
            reactor: [1; 32],
            emoji: "e".repeat(300),
        }];
        let mut b = row(1, 1, "x");
        b.reactions = vec![VaultReaction {
            reactor: [1; 32],
            emoji: "e".repeat(301),
        }];
        assert!(a.validate().is_err() && b.validate().is_err());
        let forward = merge_messages(from_ref(&a), from_ref(&b));
        assert_eq!(forward, merge_messages(from_ref(&b), from_ref(&a)));
        // The longer run of the same byte is the bytewise larger encoding.
        assert_eq!(forward, [b]);
    }
}
