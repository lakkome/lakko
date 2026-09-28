// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - `lakko_feed::decode_feed_rule` (ADR-0058 section 1).
//!
//! A shared feed is a file or a `lakko.me/feed#...` link made by someone
//! else, so every byte is attacker-chosen. Asserted, not merely exercised:
//!
//! 1. The decoder never panics.
//! 2. A decoded rule is within the size bound and validates.
//! 3. It re-encodes to the identical bytes (the encoding is canonical).
//! 4. Its export carries no conversation id and still encodes.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    lakko_feed::fuzz_api::decode_and_check(data);
});
