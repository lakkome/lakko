// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - `VaultIndex::decode` (ADR-0064 section 1).
//!
//! The index lists the account's conversations and is the first thing a
//! device reads. Asserted, not merely exercised:
//!
//! 1. The decoder never panics.
//! 2. A decoded index is within the size bound and validates (no
//!    conversation twice).
//! 3. It re-encodes to the identical bytes (the encoding is canonical).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    lakko_vault::fuzz_api::index_decode(data);
});
