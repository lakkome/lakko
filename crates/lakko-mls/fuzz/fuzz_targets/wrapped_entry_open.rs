// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `entry_wrap::unwrap_entry_commit` and the clear epoch reader
//! (G-55, ADR-0078 section 4): a wrapped owner-device entry as a member reads
//! it from a PublisherGroup journal. Covers the magic, version and epoch
//! header, the AEAD refusal under a fixed key, `epoch_of_message` /
//! `epoch_after_message` on the same bytes, and a wrap-open round trip
//! (`fuzz_api::open_wrapped_entry`).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_mls::fuzz_api::open_wrapped_entry(data);
});
