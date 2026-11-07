// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_channel_read_key` (ADR-0087 phase B).
//!
//! A read-key envelope rides a PublisherGroup journal row any co-publisher
//! (or the zero-trust DS) can craft, and reaches the decoder before any
//! state is consulted: the domain, the version byte (checked before the
//! length), the fixed length, and an open under a fixed key.
//!
//! Asserted: no panic, and whatever decodes re-encodes to the same bytes and
//! dispatches as a read-key envelope.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(ok) = lakko_contacts::fuzz_api::channel_read_key_decode(data) {
        assert!(
            ok,
            "a decoded read-key envelope must re-encode and dispatch the same"
        );
    }
});
