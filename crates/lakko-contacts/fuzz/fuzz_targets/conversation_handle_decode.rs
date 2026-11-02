// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_conversation_handle` (ADR-0087 section 11).
//!
//! A group's `@name` envelope rides an MLS application message any member
//! (or the zero-trust DS replaying a corrupt row) can craft, and reaches the
//! bounded decoder before any state is consulted: the domain, the version,
//! the flag, a canonical handle and an exact length; the signature check is
//! driven too.
//!
//! Asserted: no panic; whatever decodes re-composes to exactly the input,
//! lies within the advertised lengths, is never newer than itself, and the
//! dispatcher classifies the same bytes as a conversation handle.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(ok) = lakko_contacts::fuzz_api::conversation_handle_decode(data) {
        assert!(ok, "decode, compose and dispatch must agree on a conversation handle");
    }
});
