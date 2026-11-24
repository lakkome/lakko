// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_publisher_reader_key` (UX
//! `group-and-channel-privacy.md` 15.1, live walk 2026-11-24).
//!
//! A publishing account's reader-key envelope rides a PublisherGroup journal
//! row any publisher (or the zero-trust DS) can craft, and reaches the
//! decoder before any state is consulted: the domain, the version byte
//! (checked before the length) and the fixed length.
//!
//! Asserted: no panic, and whatever decodes re-composes to the same bytes
//! and dispatches as the same key.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(ok) = lakko_contacts::fuzz_api::publisher_reader_key_decode(data) {
        assert!(ok, "a decoded reader key must re-compose and dispatch the same");
    }
});
