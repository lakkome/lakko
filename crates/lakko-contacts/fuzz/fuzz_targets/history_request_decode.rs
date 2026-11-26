// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_history_request` (ADR-0101 section 9).
//!
//! A history request rides a conversation journal row any member (or the
//! zero-trust DS) can craft and reaches the decoder before any state or
//! role is consulted: the domain, the version byte (checked before the
//! length), the fixed length and the message id's sign.
//!
//! Asserted: no panic, and whatever decodes re-composes to the same bytes
//! and dispatches as the same request.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(ok) = lakko_contacts::fuzz_api::history_request_decode(data) {
        assert!(
            ok,
            "a decoded history request must re-compose and dispatch the same"
        );
    }
});
