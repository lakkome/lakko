// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `parse_signaling_payload` (call-signaling wire parser).
//!
//! The call-signaling body decoder runs on the plaintext bytes that fall
//! out of an `Incoming::Application` frame. Those bytes are E2EE-decrypted,
//! but the sender is any group member — so a malicious or buggy member can
//! craft an arbitrary `CallSignalingPayload` and reach this parser BEFORE
//! any field is trusted. The reachable surface is fully attacker-controlled:
//! the prost decode, the oneof-presence check, the fixed 16-byte `call_id`
//! length gate + array copy, `MediaFlags::from_bits`, and
//! `EndReason::from_proto` (which folds unknown discriminators, C-13), and
//! every body variant including the one-to-one `MediaState` (field 18,
//! 2026-11-07: camera and microphone, the mute mark's carrier). Any
//! panic / overflow / runaway allocation here is a DoS on the call-control
//! receive path.
//!
//! Asserts on arbitrary input:
//!   1. `parse_signaling_payload` never panics;
//!   2. every accepted body re-composes and re-parses back to an EQUAL
//!      (normalized) value — round-trip stability of the accepted set, which
//!      also transitively exercises the `call_id`-length invariant (a parsed
//!      body always carries an exactly-16-byte id).

#![no_main]

use lakko_calls::{compose_signaling_payload, parse_signaling_payload};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(body) = parse_signaling_payload(data) {
        // A successfully-parsed body must re-compose and re-parse to an equal
        // value — the accepted set is a fixed point under compose/parse.
        let re = compose_signaling_payload(&body).expect("a parsed body must re-compose");
        let again = parse_signaling_payload(&re).expect("a re-composed body must re-parse");
        assert_eq!(again, body, "signaling payload round-trip changed the body");
    }
});
