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
//!      bounds and re-encodes to the exact input length;
//!   4. `decode_call_name` (the name a participant gives themself for the
//!      call) never panics, on the raw input and behind the call-name
//!      domain, and a name that decodes has a peer id and a name within
//!      their bounds, a non-zero seq, and accounts for every input byte;
//!   5. `decode_call_chat` (a message in the call's chat) likewise: a
//!      message that decodes has a peer id and a text within their bounds,
//!      a text the composer would accept, a non-zero seq, and accounts for
//!      every input byte.

#![no_main]

use lakko_calls::call_chat::{
    call_chat_len, check_call_chat_text, decode_call_chat, CALL_CHAT_DOMAIN,
    MAX_CALL_CHAT_TEXT_BYTES,
};
use lakko_calls::call_name::{
    call_name_len, decode_call_name, CALL_NAME_DOMAIN, MAX_CALL_NAME_BYTES,
};
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

    // (4) The call name, raw and behind its domain so the fuzzer reaches the
    // field parsers without having to find the prefix.
    let mut prefixed = CALL_NAME_DOMAIN.to_vec();
    prefixed.extend_from_slice(data);
    for input in [data, prefixed.as_slice()] {
        if let Ok(n) = decode_call_name(input) {
            assert!(!n.peer_id.is_empty() && n.peer_id.len() <= MAX_PEER_ID_LEN);
            assert!(n.name.len() <= MAX_CALL_NAME_BYTES);
            assert!(n.seq >= 1);
            assert_eq!(call_name_len(n.peer_id.len(), n.name.len()), input.len());
        }
    }

    // (5) The call chat, raw and behind its domain.
    let mut prefixed = CALL_CHAT_DOMAIN.to_vec();
    prefixed.extend_from_slice(data);
    for input in [data, prefixed.as_slice()] {
        if let Ok(m) = decode_call_chat(input) {
            assert!(!m.peer_id.is_empty() && m.peer_id.len() <= MAX_PEER_ID_LEN);
            assert!(!m.text.is_empty() && m.text.len() <= MAX_CALL_CHAT_TEXT_BYTES);
            assert!(check_call_chat_text(&m.text).is_ok());
            assert!(m.seq >= 1);
            assert_eq!(call_chat_len(m.peer_id.len(), m.text.len()), input.len());
        }
    }
});
