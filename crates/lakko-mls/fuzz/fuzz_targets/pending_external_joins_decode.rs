// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the persisted pending-external-joins row decoder
//! (`Group::load` reads it back from the MLS storage snapshot, ADR-0027). The
//! row is local bookkeeping wrapped under the device key, but the snapshot
//! also travels inside the encrypted backup, and the parser is the first
//! Lakko-owned one `Group::load` runs. The bounded, fail-closed decoder
//! (version gate + count cap + per-package length bound + re-validation +
//! trailing-bytes refusal) must stay panic-free on arbitrary input.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_mls::fuzz_api::decode_pending_external_joins(data);
});
