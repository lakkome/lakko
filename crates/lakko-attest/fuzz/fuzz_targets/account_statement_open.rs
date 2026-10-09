// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `AccountStatement::decode_unverified`, `AccountStatement::open`
//! and `verify_account_statement` (the client-signed account-key statement,
//! ADR-0066 section 0.3). The fuzz input is the attacker-controlled wire
//! form: a fixed-layout 138-byte blob (`version | kind | account_id(16) |
//! unix_ms(8 BE) | nonce(16) | body_hash(32) | sig(64)`) that anyone can send
//! to auth's device-revoke route as a body field and to discovery's
//! KeyPackage delete as a header, so a malformed statement reaches this
//! bounded parser before any state is consulted. The harness verifies against
//! a FIXED key (the fuzzer cannot forge an Ed25519 signature, so it exercises
//! the length / version / kind gates, the field slicing, the account and
//! window comparisons at extreme clocks, the signature parse and the verify
//! failure) and asserts that decode and open agree on the layout verdict. Any
//! panic / overflow / slice-out-of-bounds here is a vulnerability.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_attest::fuzz_api::open_account_statement(data);
});
