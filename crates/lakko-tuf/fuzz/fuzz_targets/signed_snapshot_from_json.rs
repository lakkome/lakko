// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `Signed::<Snapshot>::from_json` raw-JSON
//! decoder. See `signed_root_from_json.rs` for rationale.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_tuf::Signed::<lakko_tuf::Snapshot>::from_json(data);
});
