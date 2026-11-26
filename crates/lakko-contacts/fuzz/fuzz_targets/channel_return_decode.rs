// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_channel_return` (ADR-0101 section 7).
//!
//! A channel return is a row in a reset account's continuity mailbox, which
//! anyone who knows the account id can write to for the window. The opened
//! body reaches the bounded decoder (version before any length, a role byte,
//! a length-prefixed invitation, a counted seed list, a length-prefixed
//! profile) before any state is consulted.
//!
//! Asserted: no panic; whatever decodes re-composes to the same bytes within
//! the advertised bounds, its old card binding never verifies under a fixed
//! key the fuzzer does not hold, and it dispatches as the same return.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(ok) = lakko_contacts::fuzz_api::channel_return_decode(data) {
        assert!(
            ok,
            "a decoded channel return must re-compose and dispatch the same"
        );
    }
});
