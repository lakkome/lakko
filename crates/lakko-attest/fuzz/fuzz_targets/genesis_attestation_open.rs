// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `GenesisAttestation::open` (the auth-attested genesis parser,
//! ADR-0036). The fuzz input is the attacker-controlled wire envelope: a
//! fixed-layout 153-byte blob (`version | account_id(16) | username_hash(32) |
//! pubkey(32) | expires_at(8 BE) | sig(64)`) served as a bundle sidecar field
//! that a malicious discovery / Delivery Service can craft, so a malformed
//! envelope reaches this bounded parser before any state is consulted. The
//! harness opens it against a FIXED verifying key (the fuzzer cannot forge a
//! valid Ed25519 signature, so it exercises the length / version / field-slicing
//! / signature-parse / verify-fail path). Any panic / slice-out-of-bounds here
//! is a vulnerability.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_attest::fuzz_api::open_genesis_attestation(data);
});
