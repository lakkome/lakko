// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The merge is total and deterministic (ADR-0064 section 2): whichever
//! order two devices meet each other's copies in, they end with the same
//! rows.
//!
//! The generator draws ids, `last_op_id`s and contents from small sets, so
//! the same id on both sides, an equal `last_op_id` with different content,
//! and an id repeated within one side all occur often.

use lakko_vault::record::{HistoryChunk, MAX_CHUNK_MESSAGES};
use lakko_vault::{merge_messages, Direction, VaultMessage, VaultReaction};
use proptest::prelude::*;

fn row() -> impl Strategy<Value = VaultMessage> {
    (
        0u64..12,
        0u64..4,
        prop::sample::select(vec!["", "a", "b", "aa", "longer text"]),
        any::<bool>(),
        any::<bool>(),
        prop::collection::vec((0u8..3, prop::sample::select(vec!["+", "ok"])), 0..3),
    )
        .prop_map(
            |(message_id, last_op_id, text, edited, deleted, reactions)| VaultMessage {
                message_id,
                direction: Direction::Incoming,
                author: Some([1; 32]),
                created_at_ms: 1_700_000_000_000,
                last_op_id,
                edited,
                deleted,
                text: text.to_owned(),
                attachments: Vec::new(),
                reply: None,
                forward: None,
                reactions: reactions
                    .into_iter()
                    .map(|(who, emoji)| VaultReaction {
                        reactor: [who; 32],
                        emoji: emoji.to_owned(),
                    })
                    .collect(),
                call: None,
                unsupported_version: None,
                content: None,
            },
        )
}

fn rows() -> impl Strategy<Value = Vec<VaultMessage>> {
    prop::collection::vec(row(), 0..24)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn commutative(a in rows(), b in rows()) {
        prop_assert_eq!(merge_messages(&a, &b), merge_messages(&b, &a));
    }

    #[test]
    fn associative(a in rows(), b in rows(), c in rows()) {
        let left = merge_messages(&merge_messages(&a, &b), &c);
        let right = merge_messages(&a, &merge_messages(&b, &c));
        prop_assert_eq!(left, right);
    }

    #[test]
    fn idempotent(a in rows(), b in rows()) {
        let merged = merge_messages(&a, &b);
        prop_assert_eq!(&merge_messages(&merged, &merged), &merged);
        prop_assert_eq!(&merge_messages(&merged, &a), &merged);
        prop_assert_eq!(&merge_messages(&merged, &b), &merged);
        prop_assert_eq!(&merge_messages(&merged, &[]), &merged);
        prop_assert_eq!(merge_messages(&a, &a), merge_messages(&a, &[]));
    }

    #[test]
    fn sorted_union_of_the_ids(a in rows(), b in rows()) {
        let merged = merge_messages(&a, &b);
        prop_assert!(merged.windows(2).all(|w| w[0].message_id < w[1].message_id));
        let mut ids: Vec<u64> = a.iter().chain(&b).map(|m| m.message_id).collect();
        ids.sort_unstable();
        ids.dedup();
        let merged_ids: Vec<u64> = merged.iter().map(|m| m.message_id).collect();
        prop_assert_eq!(merged_ids, ids);
    }

    #[test]
    fn every_kept_row_is_one_of_the_inputs_and_none_outranks_it(a in rows(), b in rows()) {
        let merged = merge_messages(&a, &b);
        for kept in &merged {
            prop_assert!(a.iter().chain(&b).any(|m| m == kept));
            for other in a.iter().chain(&b).filter(|m| m.message_id == kept.message_id) {
                prop_assert!(other.last_op_id <= kept.last_op_id);
            }
        }
    }

    #[test]
    fn the_order_of_rows_within_an_input_does_not_matter(a in rows(), b in rows()) {
        let mut reversed = a.clone();
        reversed.reverse();
        prop_assert_eq!(merge_messages(&a, &b), merge_messages(&reversed, &b));
    }

    #[test]
    fn a_merge_of_valid_rows_encodes(a in rows(), b in rows()) {
        let merged = merge_messages(&a, &b);
        prop_assert!(merged.len() <= MAX_CHUNK_MESSAGES);
        let chunk = HistoryChunk { messages: merged };
        let bytes = chunk.encode();
        prop_assert!(bytes.is_ok());
        prop_assert_eq!(HistoryChunk::decode(&bytes.unwrap_or_default()), Ok(chunk));
    }
}
