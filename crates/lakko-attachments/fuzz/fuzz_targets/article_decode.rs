// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `Article::decode` (ADR-0092 section 8). An article rides
//! base64 in a chat envelope any group member or channel publisher writes, so
//! every byte reaches this decoder before anything is rendered. The harness
//! asserts `fuzz_api::article_decode_round_trip`: decoding never panics, and
//! an article that decodes re-encodes to the identical bytes, within the size
//! bound, with an excerpt within its bound.

#![no_main]

use lakko_attachments::fuzz_api::article_decode_round_trip;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    assert!(article_decode_round_trip(data));
});
