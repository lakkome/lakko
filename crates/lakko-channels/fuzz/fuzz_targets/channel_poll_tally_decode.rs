// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the frozen tally of a channel poll (ADR-0092 section 4.3).
//!
//! A publisher writes the tally into the post it edits at close; every
//! subscriber decodes it: the domain, the version, the voter count, the
//! option count and the exact length it names.
//!
//! Asserted: no panic, and whatever decodes has every count at most its
//! voters and re-encodes to the identical bytes.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    assert!(
        lakko_channels::fuzz_api::channel_poll_tally_decode(data),
        "a decoded poll tally must be bounded and canonical"
    );
});
