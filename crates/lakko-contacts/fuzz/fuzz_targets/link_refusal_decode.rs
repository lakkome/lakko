// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_link_refusal` + `verify_link_refusal` (ADR-0079
//! amendment of 2026-10-19).
//!
//! A link refusal arrives as the body of a sealed mailbox row of kind
//! `LinkRefusal` in the requester's account mailbox. Anyone holding a write
//! key to that mailbox (the inviter, or whoever else the requester handed a
//! reply key) chooses the bytes, so they reach this bounded, fail-closed
//! decoder before any state is consulted: the domain, a version byte checked
//! before the length, a fixed-length payload, then the signature under a key
//! the requester already holds.
//!
//! Asserted, not merely exercised:
//!
//! * no input panics the decoder or the verifier;
//! * a decoded refusal re-encodes to exactly the input (canonical);
//! * nothing the fuzzer produces verifies under a fixed inviter key it does
//!   not hold the seed of.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::link_refusal::{
    decode_link_refusal, verify_link_refusal, LINK_REFUSAL_DOMAIN_V1, LINK_REFUSAL_LEN,
    LINK_REFUSAL_VERSION_V1,
};

/// A valid Ed25519 public key whose seed the fuzzer never sees (the base point
/// encoding of the RFC 8032 test key 1).
const INVITER_KEY: [u8; 32] = [
    0xd7, 0x5a, 0x98, 0x01, 0x82, 0xb1, 0x0a, 0xb7, 0xd5, 0x4b, 0xfe, 0xd3, 0xc9, 0x64, 0x07, 0x3a,
    0x0e, 0xe1, 0x72, 0xf3, 0xda, 0xa6, 0x23, 0x25, 0xaf, 0x02, 0x1a, 0x68, 0xf7, 0x07, 0x51, 0x1a,
];

fuzz_target!(|data: &[u8]| {
    if let Ok(r) = decode_link_refusal(data) {
        assert_eq!(data.len(), LINK_REFUSAL_LEN);
        let mut again = Vec::with_capacity(LINK_REFUSAL_LEN);
        again.extend_from_slice(LINK_REFUSAL_DOMAIN_V1);
        again.push(LINK_REFUSAL_VERSION_V1);
        again.extend_from_slice(&r.link_id);
        again.extend_from_slice(&r.requester_account_key);
        again.extend_from_slice(&r.signature);
        assert_eq!(again, data);
        assert!(
            verify_link_refusal(data, &INVITER_KEY, &r.link_id, &r.requester_account_key).is_err(),
            "a forged refusal verified"
        );
    }
    // The verifier on arbitrary bytes with arbitrary expectations.
    let _ = verify_link_refusal(data, data.get(..32).unwrap_or(&[]), &[0u8; 16], &[0u8; 32]);
});
