// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `lakko_channels::classify_post_plaintext` (ADR-0051 §4).
//!
//! The channel metadata record rides as the plaintext of an ordinary
//! post, so every reader runs this classifier on every decrypted post
//! **before** the text path. The bytes come from a signed post, but the
//! signer is any publisher — a malicious co-publisher, or a publisher
//! key that leaked — so the parser must treat them as hostile: the
//! one-byte and two-byte length prefixes, the version byte and the
//! UTF-8 / bound checks all run on attacker-chosen input.
//!
//! Three properties are asserted, not merely exercised:
//!
//! 1. The classifier never panics.
//! 2. A `Meta` result is within every documented bound (so the record
//!    the reader stores is one `compose` would have produced) and
//!    re-composes to the identical bytes — the encoding is canonical.
//! 3. A `Text` result is the input verbatim — the classifier never
//!    alters what the text path renders.

#![no_main]

use lakko_channels::{
    classify_post_plaintext, compose_channel_meta, PostContent, CHANNEL_META_MAX_LEN,
    CHANNEL_META_MIN_LEN,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    match classify_post_plaintext(data) {
        PostContent::Text(text) => {
            assert_eq!(text, data, "Text must be the input verbatim");
        },
        PostContent::Meta(meta) => {
            assert!(
                data.len() >= CHANNEL_META_MIN_LEN && data.len() <= CHANNEL_META_MAX_LEN,
                "a decoded record must lie within the documented length bounds"
            );
            assert!(meta.validate().is_ok(), "a decoded record must pass validate");
            let recomposed =
                compose_channel_meta(&meta).expect("a decoded record must re-compose");
            assert_eq!(recomposed, data, "the encoding must be canonical");
        },
        PostContent::Malformed(_) => {},
    }
});
