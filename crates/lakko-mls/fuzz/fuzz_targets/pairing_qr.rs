// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `pairing::decode` (the device-pairing QR scanner-side parser).
//! The bytes are the MOST attacker-controllable input in the system: a victim
//! physically scans a QR code a malicious actor crafted. The parser runs a
//! prost decode + structural binding checks (version / account_id length +
//! match / rendezvous-pseudonym length) before any device is enrolled, so it
//! must stay panic-free on arbitrary input. A fixed all-zero
//! `expected_account_id` keeps the harness reproducible.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_mls::fuzz_api::decode_pairing_qr(data);
});
