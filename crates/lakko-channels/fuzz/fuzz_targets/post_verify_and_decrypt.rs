// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — full receiver pipeline
//! `decode_post(...).and_then(verify_and_decrypt)`.
//!
//! Feeds arbitrary bytes through both halves of the receiver
//! path so the fuzzer exercises:
//!
//! - Wire-format parser robustness (`decode_post` itself —
//!   already covered by `post_decode`, included here so the
//!   fuzzer can also hit the AAD/HKDF/AEAD path on a parser-
//!   accepted blob).
//! - Ed25519 verify path on arbitrary `sender_pubkey` bytes.
//! - HKDF / AEAD-open path on the constructed
//!   `(key, nonce, aad)` triple.
//!
//! A fixed `(seed, channel_id)` pair drives the keying side so
//! the AEAD has a chance of opening on a hand-crafted attacker
//! input; any panic / OOB / overflow is the bug we are
//! hunting.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_channels::{
    epoch::EpochId,
    post::{decode_post, verify_and_decrypt},
    seed::ContentKeySeed,
    CHANNEL_ID_LEN,
};

const FIXED_CHANNEL: [u8; CHANNEL_ID_LEN] = *b"fuzz-channel-16b";

fuzz_target!(|data: &[u8]| {
    let Ok(post) = decode_post(data) else { return };
    // The seed value is fixed so the fuzzer can in principle
    // craft an input that successfully opens. Whether it
    // succeeds or not, the parser + crypto path must not
    // panic.
    let seed = ContentKeySeed::from_bytes_for_test([0xAB; 32]);
    let _ = verify_and_decrypt(&seed, &FIXED_CHANNEL, &post);
    let _ = post.epoch_id;
    let _ = EpochId::from_be_bytes(&post.epoch_id.to_be_bytes());
});
