// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target - `Group::retain_join_request` (ADR-0087 section 5). A sealed
//! knock row opens to bytes anyone holding the group's public knock key
//! chose; they reach the exact-length KeyPackage decode, the validation, the
//! ADR-0050 binding resolve and the group admission check before any group
//! state is written. Must never panic, and never change the group's epoch or
//! roster (asserted inside the harness).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_mls::fuzz_api::retain_join_request(data);
});
