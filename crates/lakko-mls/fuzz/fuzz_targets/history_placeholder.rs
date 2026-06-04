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

#![no_main]

use lakko_mls::history_placeholder::HistoryMessagePlaceholder;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Bounded-parse path — must not panic / OOM on garbage.
    let _ = lakko_mls::fuzz_api::parse_history_placeholder(data);

    // Round-trip stability on anything that parses.
    if let Ok(msg) = HistoryMessagePlaceholder::decode(data) {
        let re = msg.encode().expect("decoded placeholder must re-encode");
        let back =
            HistoryMessagePlaceholder::decode(&re).expect("re-encoded placeholder must re-decode");
        assert_eq!(msg, back, "history placeholder round-trip mismatch");
    }
});
