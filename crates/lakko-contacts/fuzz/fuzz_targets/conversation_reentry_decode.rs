// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_conversation_reentry` (ADR-0091 phase 2).
//!
//! A request to be brought back into a conversation arrives as a DeviceGroup
//! service envelope (route 1), the body of a sealed mailbox row a stranger or
//! the zero-trust DS can write (route 2), or the plaintext of a knock row
//! (route 3), and reaches the decoder before any state is consulted: the
//! domain, the version byte (checked before any length), the fixed fields
//! and a length-prefixed KeyPackage bounded before it is copied. The opened
//! knock-row plaintext parser (length prefix, zero padding) is driven on the
//! same bytes.
//!
//! Asserted: no panic, and whatever decodes re-composes to the same bytes
//! within the advertised bounds and dispatches as the same request.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(ok) = lakko_contacts::fuzz_api::conversation_reentry_decode(data) {
        assert!(
            ok,
            "a decoded re-entry request must re-compose and dispatch the same"
        );
    }
});
