// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_poll_ballots` (ADR-0090 section 3.9).
//!
//! An old-ballots envelope is an MLS application message any member (or the
//! zero-trust DS) can craft, and it reaches this bounded decoder before any
//! row is opened: a domain, a version byte checked before any length, a row
//! count checked against what is left, and each row bounded, required to be
//! a ballot row and strictly ascending.
//!
//! Asserted: no input panics; whatever decodes re-composes to exactly the
//! input within the advertised lengths, and the dispatcher classifies it the
//! same.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(ok) = lakko_contacts::fuzz_api::poll_ballots_decode(data) {
        assert!(ok, "decode, compose, the bounds and dispatch must agree");
    }
});
