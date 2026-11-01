// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `verify_conversation_link` (ADR-0074 section 3.7).
//!
//! A conversation link is served from the directory beside the identity
//! bundle, so a malicious directory reaches this parser with bytes of its
//! choosing on every contact a client looks up. Fixed 201-byte layout, version
//! gate, two Ed25519 verifications. The current key is fixed, so the fuzzer
//! cannot forge a signature and exercises the length, version, slicing and
//! refusal paths. Anything that verifies must name that current key.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_crypto::signing::SigningKey;
use lakko_transparency::{verify_conversation_link, ConversationLink};

fuzz_target!(|data: &[u8]| {
    let current = SigningKey::from_seed(&[7u8; 32]).verifying_key().to_bytes();
    let _ = verify_conversation_link(data, &current);
    if let Ok(link) = ConversationLink::open(data) {
        assert_ne!(link.conversation_key, link.current_key);
    }
});
