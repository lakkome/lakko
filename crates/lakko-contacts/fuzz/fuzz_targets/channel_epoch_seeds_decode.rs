// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_channel_epoch_seeds` (ADR-0087 section 7, amended
//! 2026-11-08).
//!
//! An epoch-seeds envelope rides a PublisherGroup journal row any publisher
//! (or the zero-trust DS) can craft, and reaches the decoder before any state
//! is consulted: the domain, the version byte (checked before any length),
//! the clear count (1 to 64), the exact length the count names, and an open
//! under a fixed key. The opened-list parser is driven on the same bytes
//! directly, since a fuzzer cannot forge the tag.
//!
//! Asserted: no panic, and whatever decodes re-encodes to the same bytes,
//! lies within the advertised length bounds and dispatches as an
//! epoch-seeds envelope.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(ok) = lakko_contacts::fuzz_api::channel_epoch_seeds_decode(data) {
        assert!(
            ok,
            "a decoded epoch-seeds envelope must re-encode, fit its bounds and dispatch the same"
        );
    }
});
