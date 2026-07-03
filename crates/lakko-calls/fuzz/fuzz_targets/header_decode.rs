// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `SframeHeader::decode` (RFC 9605 §4.3, ADR-0013).
//!
//! The SFrame header is the on-wire prefix of every sealed media frame
//! and the AEAD AAD. A receiver decodes it — via the `lakko-sframe-native`
//! C ABI on iOS/Android or the wasm worker on web — on bytes handed over
//! by an untrusted SFU / peer, BEFORE the AEAD authenticates the frame, so
//! the config-byte packing + the variable-length extended KID/CTR field
//! arithmetic are fully attacker-controlled. Any panic / overflow / runaway
//! allocation here is a DoS on the call receive path.
//!
//! The harness asserts three properties on arbitrary input:
//!   1. `decode` never panics (the fuzzer explores all first-byte packings
//!      + truncated extended fields);
//!   2. a successful decode reports a `consumed` length within the buffer
//!      and within `MAX_HEADER_LEN`, and its canonical re-encode decodes
//!      back to the same header (round-trip stability of the canonical form);
//!   3. an `encode` of a fuzzer-chosen `(kid, ctr)` always decodes back
//!      verbatim (the encoder-side round-trip, exercising the extended
//!      big-endian field paths for large values).

#![no_main]

use lakko_calls::{SframeHeader, MAX_HEADER_LEN};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // (1)+(2) The decoder must never panic on attacker bytes; a success
    // reports a bounded `consumed` and its canonical form round-trips.
    if let Ok((header, consumed)) = SframeHeader::decode(data) {
        assert!(consumed <= data.len(), "consumed past the buffer end");
        assert!(
            consumed <= MAX_HEADER_LEN,
            "consumed exceeds MAX_HEADER_LEN"
        );

        let re = header.encode();
        assert!(
            re.len() <= MAX_HEADER_LEN,
            "canonical encode exceeds MAX_HEADER_LEN"
        );
        let (again, n) = SframeHeader::decode(&re).expect("a canonical re-encode must decode");
        assert_eq!(again, header, "canonical round-trip changed the header");
        assert_eq!(n, re.len(), "canonical round-trip consumed length drifted");
    }

    // (3) Independently derive a (kid, ctr) from the fuzzer bytes and prove
    // the encode -> decode round-trip for arbitrary 64-bit field values.
    if data.len() >= 16 {
        let mut kid_bytes = [0u8; 8];
        let mut ctr_bytes = [0u8; 8];
        kid_bytes.copy_from_slice(&data[0..8]);
        ctr_bytes.copy_from_slice(&data[8..16]);
        let header = SframeHeader {
            kid: u64::from_le_bytes(kid_bytes),
            ctr: u64::from_le_bytes(ctr_bytes),
        };
        let bytes = header.encode();
        let (parsed, consumed) = SframeHeader::decode(&bytes).expect("encode output must decode");
        assert_eq!(parsed, header, "encode -> decode changed the header");
        assert_eq!(
            consumed,
            bytes.len(),
            "encode -> decode consumed length drifted"
        );
    }
});
