// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `RoleTable::decode` (the ADR-0022 `0xFF02` GroupContext
//! role-table body parser). The bytes are attacker-touchable: the role table
//! travels inside a Welcome or a `GroupContextExtensions` commit a malicious
//! group founder / member can craft, so a malformed body reaches this bounded,
//! fail-closed parser (version gate + `admin_count` cap + exact-length check)
//! before any role lookup. Any panic / overflow / runaway allocation here is a
//! vulnerability.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_mls::fuzz_api::decode_role_table(data);
});
