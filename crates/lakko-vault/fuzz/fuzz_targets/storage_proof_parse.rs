// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - `StorageProof::parse` + `verify` (ADR-0064 section 1,
//! ADR-0065 section 6).
//!
//! The header value is attacker-controlled on every vault request and every
//! blob delete, and is parsed before the stored key is consulted. Asserted,
//! not merely exercised:
//!
//! 1. The parser never panics, and the verify that follows never panics on a
//!    hostile time.
//! 2. A parsed proof re-encodes to the identical text.
//! 3. Only the proof the fixed key really signs verifies.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    lakko_vault::fuzz_api::proof_parse(data);
});
