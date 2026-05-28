// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — full ECH wire-up assembly:
//! `parse_ech_config_list` → `rustls::client::EchConfig::new`
//! → `EchMode::Enable(..)`.
//!
//! `ech_decode` already covers our defense-in-depth
//! parser in isolation; this target exercises the
//! *integration* — what happens when the parser accepts
//! bytes but rustls's own parser does not (or vice
//! versa), and what happens during HPKE suite matching
//! against the three suites in
//! `crate::ech::adapter::ALL_SUPPORTED_SUITES`.
//!
//! Any panic / overflow / stack abuse here is a
//! vulnerability on the anti-censorship bootstrap path —
//! the DNS HTTPS RR `ech=` parameter is attacker-
//! controlled if the DNS path is poisoned. Pinned in
//! audit-pack invariant T-12 §2 (rustls-side validation
//! must hold under arbitrary attacker input).

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_transport::ech::build_ech_mode_from_bytes;

fuzz_target!(|data: &[u8]| {
    let _ = build_ech_mode_from_bytes(data);
});
