// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the sealed call relay payload and the peer announcement
//! it carries (ADR-0078 section 2).
//!
//! Every `signal-relay` payload of an SFU call reaches a receiver from the
//! SFU, which any participant (or the SFU itself) can fill with arbitrary
//! bytes. The receiver tries to open it under the relay key of its epoch,
//! and an opened plaintext is classified and, for an announcement, decoded
//! before any signature is checked. Every length in both layouts is
//! attacker-chosen.
//!
//! The harness asserts, on arbitrary input:
//!   1. `RelayKey::open` under a fixed key never panics and never accepts
//!      (the fuzzer cannot forge a tag);
//!   2. sealing the input (when within bounds) opens back to it;
//!   3. `classify_relay_plaintext` and `decode_peer_announcement` never
//!      panic, and an announcement that decodes has a peer id within its
//!      bounds and re-encodes to the exact input length.

#![no_main]

use lakko_calls::relay::{
    classify_relay_plaintext, decode_peer_announcement, peer_announcement_len, RelayKey,
    MAX_PEER_ID_LEN, MAX_RELAY_PLAINTEXT,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let key = RelayKey::from_call_seed(&[0x42; 32]);

    // (1) The open never accepts what it did not seal.
    assert!(key.open(data).is_err());

    // (2) A payload this key sealed opens back.
    if data.len() <= MAX_RELAY_PLAINTEXT {
        let sealed = key.seal(data).expect("seal within bounds");
        assert_eq!(key.open(&sealed).expect("opens"), data);
    }

    // (3) The plaintext side.
    let _ = classify_relay_plaintext(data);
    if let Ok(ann) = decode_peer_announcement(data) {
        assert!(!ann.peer_id.is_empty() && ann.peer_id.len() <= MAX_PEER_ID_LEN);
        assert_eq!(peer_announcement_len(ann.peer_id.len()), data.len());
    }
});
