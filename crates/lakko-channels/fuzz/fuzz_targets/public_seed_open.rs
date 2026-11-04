// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `open_public_seed` (ADR-0087 section 6).
//!
//! A public channel's seed envelope is served openly and cached in front of
//! the service, so a subscriber opens bytes anyone between it and the
//! publisher could have chosen: the length, the clear epoch, and the AEAD
//! under a fixed read key.
//!
//! Asserted: no panic, and nothing opens (a fuzzer cannot forge the tag).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(refused) = lakko_channels::fuzz_api::public_seed_open(data) {
        assert!(refused, "a public seed envelope the fuzzer made must not open");
    }
});
