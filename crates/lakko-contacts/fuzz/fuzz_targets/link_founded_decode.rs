// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_link_founded` (ADR-0079, ordering of 2026-10-17).
//!
//! A link-founded note arrives as a DeviceGroup `ServiceEnvelope`, from
//! another device of the account or from the zero-trust DS replaying a
//! corrupt row, and reaches this bounded, fail-closed decoder before any
//! state is consulted: a domain, a version byte checked before any length, a
//! fixed part, and a length-prefixed Welcome bounded before it is copied.
//!
//! Asserted, not merely exercised:
//!
//! * no input panics the decoder or the dispatcher;
//! * a decoded note lies within the advertised lengths;
//! * round-trip stability: whatever decodes re-composes to exactly the input
//!   (the encoding is canonical), and the dispatcher classifies the same
//!   bytes as the same note.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(ok) = lakko_contacts::fuzz_api::link_founded_decode(data) {
        assert!(ok, "decode, compose, the length bounds and dispatch must agree on a note");
    }
});
