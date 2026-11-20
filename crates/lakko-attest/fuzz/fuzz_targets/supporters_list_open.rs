// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `SupportersList::open` (the signed supporters list,
//! ADR-0097). The input is the attacker-controlled file served at
//! `/.well-known/lakko-supporters`: a variable-length blob
//! (`format | list_version(8) | signed_at(8) | count(4) | names | sig(64)`)
//! a malicious network or operator could tamper. The harness opens it against
//! a FIXED verifying key, exercising the length, version and count bounds
//! (checked before allocation) and the signature refusal. Any panic /
//! slice-out-of-bounds / oversized allocation here is a vulnerability.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_attest::fuzz_api::open_supporters_list(data);
});
