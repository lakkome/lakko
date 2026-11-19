// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_knock_key` (ADR-0091 phase 2, route 3).
//!
//! The group's knock public key rides a group journal row any member (or the
//! zero-trust DS) can craft, and reaches the decoder before any state or role
//! is consulted: the domain, the version byte (checked before the length)
//! and the fixed length.
//!
//! Asserted: no panic, and whatever decodes re-composes to the same bytes
//! and dispatches as the same key.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(ok) = lakko_contacts::fuzz_api::knock_key_decode(data) {
        assert!(ok, "a decoded knock key must re-compose and dispatch the same");
    }
});
