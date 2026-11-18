// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_channel_admission_mark` (ADR-0090 section 6).
//!
//! A publishing device's admission mark rides a PublisherGroup journal row
//! any publisher (or the zero-trust DS) can craft, and reaches the decoder
//! before any state is consulted: the domain, the version byte (checked
//! before the length) and the fixed length.
//!
//! Asserted: no panic, and whatever decodes re-composes to the same bytes and
//! dispatches as the same mark.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(ok) = lakko_contacts::fuzz_api::channel_admission_mark_decode(data) {
        assert!(
            ok,
            "a decoded admission mark must re-compose and dispatch the same"
        );
    }
});
