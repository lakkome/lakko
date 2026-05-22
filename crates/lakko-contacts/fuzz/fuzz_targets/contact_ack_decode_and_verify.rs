// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `ack::decode_and_verify_ack` (Phase 1.18-fuzz-parity).
//!
//! ContactAck is the reply Bob ships back to Alice's stranger
//! inbox after he accepts her ContactRequest (Phase 1.9a;
//! `lakko_contacts::ack`). Alice's polling loop opens the
//! HPKE envelope, then decodes the ack wire bytes through
//! this parser. Server-side routing doesn't validate ack
//! structure (it only routes opaque envelopes to the
//! pseudonym), so the parser sees attacker-controlled bytes
//! post-decrypt. A panic here would disable Alice's polling
//! loop until restart.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_contacts::decode_and_verify_ack(data);
});
