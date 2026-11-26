// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `ContinuityBinding::decode` and `verify` (ADR-0101
//! section 2).
//!
//! Discovery serves a reset account's continuity binding beside its
//! identity bundle; a hostile directory chooses every byte, and a peer
//! parses it before deciding where to write the successor Welcome.
//!
//! Asserted: no panic; whatever decodes re-encodes to the same bytes; it
//! never verifies under a fixed identity key the fuzzer does not hold; and a
//! binding that key signs over the decoded mailbox key verifies inside its
//! window.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(ok) = lakko_contacts::fuzz_api::continuity_binding_decode(data) {
        assert!(
            ok,
            "a decoded continuity binding must re-encode and never forge"
        );
    }
});
