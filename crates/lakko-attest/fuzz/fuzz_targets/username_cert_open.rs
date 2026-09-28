// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `UsernameCertificate::open` (the discovery-signed username
//! certificate parser, ADR-0058 section 5). The fuzz input is the
//! attacker-controlled wire envelope: a fixed-layout 153-byte blob (`version |
//! oprf_hash(64) | account_id(16) | issued_at(8 BE) | sig(64)`) carried opaquely
//! inside a profile envelope any group member, channel subscriber or stranger
//! can craft, so a malformed certificate reaches this bounded parser before any
//! state is consulted. The harness opens it against a FIXED verifying key (the
//! fuzzer cannot forge a valid Ed25519 signature, so it exercises the length /
//! version / field-slicing / signature-parse / verify-fail path). Any panic /
//! slice-out-of-bounds here is a vulnerability.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_attest::fuzz_api::open_username_cert(data);
});
