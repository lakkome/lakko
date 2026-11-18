// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — history-bundle per-message placeholder parser.
//!
//! `HistoryMessagePlaceholder::decode` runs on attacker-controlled
//! bytes: a malicious history-catch-up bundle reaches this parser
//! after the AEAD-open succeeds (e.g. a compromised peer device on
//! the same account). The bounded parser must never panic / OOM on
//! arbitrary input. As a stronger invariant, any successfully
//! decoded placeholder must round-trip stably — re-encoding and
//! re-decoding yields an equal value (catches encode/decode
//! asymmetry). Version 3 adds the channel invitation card, the reply and
//! the forward: whatever decodes carries a card of 1 to
//! `MAX_CHANNEL_INVITE_BYTES` bytes, snapshots within their caps, and a
//! forward origin group of 1 to `MAX_ORIGIN_GROUP_BYTES` bytes.
//!
//! Version 4 (ADR-0092): the audio title and artist tags, and the optional
//! content block (kind byte, payload, state snapshot), are asserted within
//! their structural bounds whenever a placeholder decodes. The content's
//! typed decode lives in `lakko-attachments` (fuzz target
//! `content_bundle_decode`).

#![no_main]

use lakko_mls::history_placeholder::{
    HistoryMessagePlaceholder, CONTENT_KINDS, MAX_AUDIO_TAG_BYTES, MAX_CHANNEL_INVITE_BYTES,
    MAX_CONTENT_PAYLOAD_BYTES, MAX_CONTENT_STATE_BYTES, MAX_ORIGIN_GROUP_BYTES,
    MAX_REPLY_EXCERPT_BYTES, MAX_SNAPSHOT_AUTHOR_BYTES,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Bounded-parse path — must not panic / OOM on garbage.
    let _ = lakko_mls::fuzz_api::parse_history_placeholder(data);

    // Round-trip stability on anything that parses.
    if let Ok(msg) = HistoryMessagePlaceholder::decode(data) {
        if let Some(card) = &msg.channel_invite {
            assert!(!card.is_empty() && card.len() <= MAX_CHANNEL_INVITE_BYTES);
        }
        if let Some(reply) = &msg.reply {
            assert!(reply.author.as_ref().is_none_or(|a| a.len() <= MAX_SNAPSHOT_AUTHOR_BYTES));
            assert!(reply.excerpt.as_ref().is_none_or(|e| e.len() <= MAX_REPLY_EXCERPT_BYTES));
        }
        if let Some(forward) = &msg.forward {
            assert!(forward.author.as_ref().is_none_or(|a| a.len() <= MAX_SNAPSHOT_AUTHOR_BYTES));
            if let Some(origin) = &forward.origin {
                assert!(!origin.group_id.is_empty() && origin.group_id.len() <= MAX_ORIGIN_GROUP_BYTES);
            }
        }
        if let Some(c) = &msg.content {
            assert!(CONTENT_KINDS.contains(&c.kind));
            assert!(!c.payload.is_empty() && c.payload.len() <= MAX_CONTENT_PAYLOAD_BYTES);
            assert!(c.state.len() <= MAX_CONTENT_STATE_BYTES);
        }
        for a in &msg.attachments {
            assert!(a
                .title
                .as_ref()
                .map_or(true, |t| t.len() <= MAX_AUDIO_TAG_BYTES));
            assert!(a
                .artist
                .as_ref()
                .map_or(true, |t| t.len() <= MAX_AUDIO_TAG_BYTES));
        }
        let re = msg.encode().expect("decoded placeholder must re-encode");
        let back =
            HistoryMessagePlaceholder::decode(&re).expect("re-encoded placeholder must re-decode");
        assert_eq!(msg, back, "history placeholder round-trip mismatch");
    }
});
