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
//! asymmetry).
//!
//! Version 4 (ADR-0090): the optional content block (kind byte, payload,
//! state snapshot) is asserted within its structural bounds whenever a
//! placeholder decodes. Its typed decode lives in `lakko-attachments`
//! (fuzz target `content_bundle_decode`).

#![no_main]

use lakko_mls::history_placeholder::{
    HistoryMessagePlaceholder, CONTENT_KINDS, MAX_AUDIO_TAG_BYTES, MAX_CONTENT_PAYLOAD_BYTES,
    MAX_CONTENT_STATE_BYTES,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Bounded-parse path — must not panic / OOM on garbage.
    let _ = lakko_mls::fuzz_api::parse_history_placeholder(data);

    // Round-trip stability on anything that parses.
    if let Ok(msg) = HistoryMessagePlaceholder::decode(data) {
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
