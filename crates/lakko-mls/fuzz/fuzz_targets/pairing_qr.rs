// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `pairing::decode` (the device-pairing payload parser, v2
//! since ADR-0056). The bytes are the MOST attacker-controllable input in the
//! system: a victim physically scans a QR code a malicious actor crafted, or
//! the auth relay serves the payload a fresh device posted to
//! `POST /device/request`. The parser runs a prost decode + structural binding
//! checks (version == 2 / 32-byte login handle + constant-time match against
//! the confirming device's own handle / rendezvous-pseudonym length / the
//! device card's length bound) before any device is enrolled, so it must stay
//! panic-free on arbitrary input. The harness binds against a fixed expected
//! handle (`fuzz_api::PAIRING_QR_EXPECTED_HANDLE`) and asserts that anything
//! the decoder accepts really does carry version 2, exactly that handle, and a
//! device card no longer than `pairing::PAIRING_DEVICE_CARD_MAX_LEN`.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    assert!(
        lakko_mls::fuzz_api::pairing_qr_acceptance_is_bound(data),
        "pairing decode accepted a payload the handle or card-length guard should refuse"
    );
});
