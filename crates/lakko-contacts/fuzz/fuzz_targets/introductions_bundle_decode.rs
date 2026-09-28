// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - introductions bundle and offer decode (ADR-0058 §4).
//!
//! A bundle arrives from a contact in a 1:1 conversation, an offer from the
//! owner of a record; both reach the dispatcher before any state is
//! consulted. The bundle carries a two-byte count and length-prefixed
//! records, so the count check (a size bucket, before any allocation) and
//! every record bound are reachable from raw bytes. Asserted: no panic,
//! a decoded bundle holds a bucket count, and round-trip stability.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::introduction::{
    bundle_is_newer, compose_introduction_offer, decode_introduction_offer, decode_introductions,
    BUNDLE_SIZE_BUCKETS,
};

fuzz_target!(|data: &[u8]| {
    if let Ok(bundle) = decode_introductions(data) {
        assert!(BUNDLE_SIZE_BUCKETS.contains(&bundle.records.len()));
        let encoded = bundle
            .encode()
            .expect("encode must accept what decode accepted");
        assert_eq!(
            encoded.as_slice(),
            data,
            "the bundle encoding must be canonical"
        );
        assert!(!bundle_is_newer(bundle.updated_at_ms, bundle.updated_at_ms));
    }
    if let Ok(record) = decode_introduction_offer(data) {
        let wire = record
            .encode()
            .expect("encode must accept what decode accepted");
        let offer = compose_introduction_offer(&wire).expect("a decoded record re-offers");
        assert_eq!(
            offer.as_slice(),
            data,
            "the offer encoding must be canonical"
        );
    }
});
