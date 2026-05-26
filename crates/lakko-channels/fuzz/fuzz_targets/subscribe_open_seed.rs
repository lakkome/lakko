// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `lakko_channels::open_seed` (Phase 4b).
//!
//! Drives arbitrary bytes through the subscriber-side
//! envelope opener. The wire shape is
//! `eph_pub(32) || ciphertext_with_tag(48)`, so the parser
//! walks attacker-controlled bytes for both the ephemeral
//! X25519 pubkey and the AEAD payload. Failure modes the
//! fuzzer is trying to surface:
//!
//! - Panic on a too-short buffer (the `Wire` error path
//!   should fire instead).
//! - Panic / OOB on an invalid X25519 pubkey (the
//!   `Wire("ephemeral pubkey invalid")` rejection should
//!   fire).
//! - Panic / OOB inside the HKDF / AEAD-open path on a
//!   ciphertext that parses but does not authenticate
//!   (the expected outcome is `AeadOpen`).
//!
//! A fixed subscriber X25519 secret drives the asymmetric
//! side so the fuzzer can in principle craft an envelope
//! that opens; whether it succeeds or not, no panic is
//! allowed.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_channels::{epoch::EpochId, open_seed, CHANNEL_ID_LEN};
use lakko_crypto::agreement::SecretKey;

const FIXED_CHANNEL: [u8; CHANNEL_ID_LEN] = *b"fuzz-channel-16b";

fuzz_target!(|data: &[u8]| {
    // Single static subscriber secret so the fuzzer's input
    // space narrows to "envelope bytes" — same as the
    // production code's perspective from the subscriber
    // side.
    let secret = SecretKey::from_bytes([0x42u8; 32]);
    let _ = open_seed(&secret, &FIXED_CHANNEL, EpochId::new(7), data);
});
