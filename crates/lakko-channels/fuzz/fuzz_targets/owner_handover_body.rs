// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `lakko_channels::parse_owner_handover_body` and
//! `OwnerProof::parse` (ADR-0066 section 3.5, the channel owner handover).
//!
//! `PUT /v1/channels/<id>/owner` takes a body anyone can send (the owner
//! proof beside it is what authorises it) and a proof header anyone can
//! send. The body parser decides which key gates every owner route of the
//! channel from then on, so it must accept exactly a usable key.
//!
//! Properties asserted, not merely exercised:
//!
//! 1. Neither parser panics.
//! 2. A body that parses is exactly `OWNER_HANDOVER_BODY_LEN` bytes, is not
//!    a small-order key, and re-encodes to the identical bytes.
//! 3. A proof header that parses re-encodes to text that parses to the same
//!    proof.

#![no_main]

use lakko_channels::{
    owner_handover_body, parse_owner_handover_body, OwnerProof, OWNER_HANDOVER_BODY_LEN,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(key) = parse_owner_handover_body(data) {
        assert_eq!(data.len(), OWNER_HANDOVER_BODY_LEN);
        assert!(!key.is_weak());
        assert_eq!(owner_handover_body(&key).as_slice(), data);
    }
    if let Ok(text) = core::str::from_utf8(data) {
        if let Ok(proof) = OwnerProof::parse(text) {
            assert_eq!(
                OwnerProof::parse(&proof.encode()).ok(),
                Some(proof),
                "a parsed proof must survive a re-encode"
            );
        }
    }
});
