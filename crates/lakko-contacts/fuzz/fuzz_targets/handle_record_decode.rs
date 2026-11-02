// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the public record of a public group or channel (ADR-0087
//! section 4).
//!
//! Discovery serves a record any owner (or a malicious discovery) chose, and
//! a searcher parses it before any key is derived; the payload plaintext is
//! reachable only past the AEAD, so the same bytes are fed to the payload
//! decoder directly. Driven: `IntroductionRecord::decode_public`,
//! `open_search_record` under a fixed OPRF output, and
//! `HandleRecordPayload::decode`.
//!
//! Asserted: no panic, and whatever decodes (as a record or as a payload)
//! re-encodes to exactly the input.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(ok) = lakko_contacts::fuzz_api::handle_record_decode(data) {
        assert!(ok, "a decoded record or payload must re-encode to its input");
    }
});
