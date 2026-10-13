// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `KeyResetAttestation::open` (the key-reset attestation
//! parser, ADR-0073). The fuzz input is the attacker-controlled wire envelope:
//! a fixed-layout 177-byte blob (`version | account_ref(32) |
//! device_group_journal_id(32) | key_generation(8 BE) | new_signature_pubkey(32)
//! | signed_at(8 BE) | sig(64)`) that reaches discovery's identity upload, the
//! Delivery Service's journal reset and a client's own persisted publish-owed
//! state, so a malformed envelope reaches this bounded parser before any state
//! is consulted. The harness opens it against a FIXED verifying key (the fuzzer
//! cannot forge a valid Ed25519 signature, so it exercises the length / version
//! / field-slicing / signature-parse / verify-fail path). Any panic /
//! slice-out-of-bounds here is a vulnerability.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_attest::fuzz_api::open_key_reset_attestation(data);
});
