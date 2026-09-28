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
//! A fixed subscriber Ed25519 key (its X25519 secret derived,
//! ADR-0058) drives the asymmetric
//! side so the fuzzer can in principle craft an envelope
//! that opens; whether it succeeds or not, no panic is
//! allowed.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_channels::{
    epoch::EpochId, open_seed, subscriber_public_key, verify_member_card_request, CHANNEL_ID_LEN,
};
use lakko_crypto::signing::SigningKey;

const FIXED_CHANNEL: [u8; CHANNEL_ID_LEN] = *b"fuzz-channel-16b";

fuzz_target!(|data: &[u8]| {
    // Single static subscriber secret so the fuzzer's input
    // space narrows to "envelope bytes" — same as the
    // production code's perspective from the subscriber
    // side.
    // Since ADR-0058 the subscriber key is Ed25519 and the
    // X25519 secret is derived from it.
    let secret = SigningKey::from_seed(&[0x42u8; 32]);
    let _ = open_seed(&secret, &FIXED_CHANNEL, EpochId::new(7), data);

    // The subscriber-key validation (point decode + small-order
    // refusal) and the member-card request signature check, on
    // an attacker-chosen key, signature and card: what the
    // channels server runs on every registration and upload.
    if data.len() >= 32 + 64 {
        let mut pk = [0u8; 32];
        pk.copy_from_slice(&data[..32]);
        let _ = subscriber_public_key(&pk);
        let _ = verify_member_card_request(&FIXED_CHANNEL, &pk, &data[96..], &data[32..96]);
    }
});
