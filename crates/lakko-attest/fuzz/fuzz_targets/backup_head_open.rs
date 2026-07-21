// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `BackupHead::open` (the auth-signed backup-head parser,
//! ADR-0037). The fuzz input is the attacker-controlled wire envelope: a
//! fixed-layout 129-byte blob (`version | account_id(16) | backup_version(8 BE) |
//! envelope_hash(32) | signed_at(8 BE) | sig(64)`) that rides as a prefix of the
//! storage backup blob (`CONTAINER_TAG | head | envelope`), which a malicious
//! storage tier can craft, so a malformed envelope reaches this bounded parser
//! before any state is consulted. The harness opens it against a FIXED verifying
//! key (the fuzzer cannot forge a valid Ed25519 signature, so it exercises the
//! length / version / field-slicing / signature-parse / verify-fail path). Any
//! panic / slice-out-of-bounds here is a vulnerability.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_attest::fuzz_api::open_backup_head(data);
});
