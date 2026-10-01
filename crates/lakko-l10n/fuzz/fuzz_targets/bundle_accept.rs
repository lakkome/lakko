// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `accept_bundle` (the signed translation bundle, ADR-0061),
//! under a FIXED verifying key and policy. The fuzzer cannot forge the
//! Ed25519 signature, so this exercises the size bound, the header layout,
//! the length arithmetic and the signature-refusal path. Any panic or
//! out-of-bounds slice here is a vulnerability.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    lakko_l10n::fuzz_api::accept_arbitrary_bundle(data);
});
