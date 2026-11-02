// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the post key proof header and `verify_post_key_proof`
//! (ADR-0087 section 13a).
//!
//! The `x-lakko-channel-post-key-proof` header reaches the channels service
//! on every reaction and comment write, from anyone: the length bound, the
//! four-field split, the version, the decimal time, two base64url decodes
//! with fixed lengths, and then the window, the post key's validity and the
//! signature over a fixed write.
//!
//! Asserted: no panic, and whatever parses re-encodes to the same text.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(value) = core::str::from_utf8(data) {
        if let Some(ok) = lakko_channels::fuzz_api::post_key_proof_parse(value) {
            assert!(ok, "a parsed post key proof must re-encode to the same text");
        }
    }
});
