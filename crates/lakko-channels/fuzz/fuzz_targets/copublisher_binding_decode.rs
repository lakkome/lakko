// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `lakko_channels::decode_copublisher_binding` and
//! `CoPublisherProof::parse` (ADR-0059 section 1, co-publisher keys).
//!
//! A binding reaches the channel owner as a row of a PublisherGroup's knock
//! queue (ADR-0053), an unauthenticated write anyone who can knock chooses
//! byte for byte; the proof header reaches `services/channels` from anyone.
//! Both parsers run before any key is trusted.
//!
//! Properties asserted, not merely exercised:
//!
//! 1. Neither parser panics.
//! 2. A binding that decodes is exactly `COPUBLISHER_BINDING_LEN` bytes.
//! 3. A proof header that parses re-encodes to text that parses to the same
//!    proof (the time field may carry leading zeros, so not the same text).

#![no_main]

use lakko_channels::{decode_copublisher_binding, CoPublisherProof, COPUBLISHER_BINDING_LEN};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if decode_copublisher_binding(data).is_ok() {
        assert_eq!(data.len(), COPUBLISHER_BINDING_LEN);
    }
    if let Ok(text) = core::str::from_utf8(data) {
        if let Ok(proof) = CoPublisherProof::parse(text) {
            assert_eq!(
                CoPublisherProof::parse(&proof.encode()).ok(),
                Some(proof),
                "a parsed proof must survive a re-encode"
            );
        }
    }
});
