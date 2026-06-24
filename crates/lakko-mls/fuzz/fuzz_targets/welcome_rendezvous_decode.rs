// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `WelcomeRendezvous::decode` (ADR-0027 Phase 3, G-3). The bytes
//! are attacker-touchable: a prospective joiner decodes group-journal entries
//! that a malicious poster / the zero-trust Delivery Service could have crafted,
//! before any inner Welcome is opened. The bounded, fail-closed parser
//! (version gate + length bounds + slice arithmetic) must stay panic-free on
//! arbitrary input.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_mls::fuzz_api::decode_welcome_rendezvous(data);
});
