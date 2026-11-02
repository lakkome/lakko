// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `open_comment` and the comment plaintext parser (ADR-0087
//! section 13a).
//!
//! A comment row is written by anyone holding a `channel-comment` token and
//! served back by the channels service, which can also replace it, so every
//! reader opens bytes a stranger chose: the length bounds and the version
//! byte, then the AEAD under the post's epoch seed. A fuzzer cannot forge the
//! tag, so the oracle also feeds the same bytes to the plaintext parser (the
//! author key, the signature, the name and avatar lengths, the reply flag,
//! the two-byte text length, UTF-8 and every field rule) and to the body
//! parser directly.
//!
//! Asserted: no panic; the open under a fixed seed never accepts; anything a
//! parser accepts is within every bound and re-encodes to the identical body.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    assert!(
        lakko_channels::fuzz_api::channel_comment_open(data),
        "a comment parser property was violated"
    );
});
