// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `lakko_channels::decode_post` (Phase 4a).
//!
//! Drives arbitrary bytes through the wire-format parser. The
//! parser walks attacker-controlled length prefixes (4-byte
//! ciphertext length, 32-byte Ed25519 pubkey, 8-byte counter,
//! 64-byte signature, variable ciphertext), so any
//! out-of-bounds slice or panic on a malformed length is a DoS
//! vector against the receiver side.
//!
//! Reject-as-`Err(ChannelError::Wire(...))` is the expected
//! happy path on a malformed input; panic is not.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_channels::post::decode_post(data);
});
