// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `lakko_channels::member_card_epoch` +
//! `open_member_card` (ADR-0060 section 3).
//!
//! A member card is served by the channels server from a paged
//! member list; the server (or any subscriber that uploaded one) can
//! put arbitrary bytes there, and every reader parses them before
//! any key material is consulted. The wire shape is
//! `version(1) || epoch_id(4 BE) || nonce(12) || ciphertext_with_tag`,
//! bounded to `MEMBER_CARD_MIN_LEN..=MEMBER_CARD_MAX_LEN`.
//!
//! The harness reads the clear epoch, then opens the card under a
//! FIXED seed and a fixed `(channel, subscriber)` binding. The
//! fuzzer cannot forge a valid tag, so it exercises the length
//! bounds, the version gate, the epoch / nonce slicing and the
//! AEAD refusal. Any panic / slice-out-of-bounds is a bug; an
//! opened card must fit the profile bound.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_channels::{
    member_card_epoch, open_member_card, seed::ContentKeySeed, CHANNEL_ID_LEN,
    MAX_MEMBER_CARD_PROFILE_LEN, SUBSCRIBER_PK_LEN,
};

const FIXED_CHANNEL: [u8; CHANNEL_ID_LEN] = *b"fuzz-channel-16b";
const FIXED_SUBSCRIBER: [u8; SUBSCRIBER_PK_LEN] = [0x5B; SUBSCRIBER_PK_LEN];

fuzz_target!(|data: &[u8]| {
    let _ = member_card_epoch(data);
    let seed = ContentKeySeed::from_bytes_for_test([0xAB; 32]);
    if let Ok(profile) = open_member_card(&seed, data, &FIXED_CHANNEL, &FIXED_SUBSCRIBER) {
        assert!(profile.len() <= MAX_MEMBER_CARD_PROFILE_LEN);
    }
});
