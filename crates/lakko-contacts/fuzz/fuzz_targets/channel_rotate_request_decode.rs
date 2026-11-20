// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_channel_rotate_request` (ADR-0087 section 15,
//! amended 2026-11-08).
//!
//! A publisher's request that the owner rotate rides a PublisherGroup
//! journal row any publisher (or the zero-trust DS) can craft, and reaches
//! the decoder before any state is consulted: the domain, the version byte
//! (checked before the length) and the fixed length.
//!
//! Asserted: no panic, and whatever decodes re-composes to the same bytes
//! and dispatches as the same channel.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(ok) = lakko_contacts::fuzz_api::channel_rotate_request_decode(data) {
        assert!(
            ok,
            "a decoded rotate request must re-compose and dispatch the same"
        );
    }
});
