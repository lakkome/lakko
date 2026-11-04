// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `HandleCertificate::open` (the discovery-signed handle
//! certificate parser, ADR-0087 section 3). The fuzz input is the
//! attacker-controlled 169-byte blob (`version | lookup_key(64) |
//! write_pk(32) | issued_at(8 BE) | sig(64)`) carried opaquely inside a
//! conversation-handle envelope any group member can relay. The harness opens
//! it against a FIXED verifying key, so it exercises the length / version /
//! field-slicing / signature-parse / verify-fail path. Any panic here is a
//! vulnerability.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_attest::fuzz_api::open_handle_cert(data);
});
