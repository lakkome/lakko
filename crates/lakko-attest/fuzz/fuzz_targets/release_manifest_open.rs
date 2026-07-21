// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `ReleaseManifest::open` (the signed update-notice parser,
//! ADR-0038). The fuzz input is the attacker-controlled wire envelope: a
//! fixed-layout 89-byte blob (`format_version | latest_release(8 BE) |
//! min_supported_release(8 BE) | signed_at(8 BE) | sig(64)`) fetched from a host
//! a malicious network could tamper, so a malformed envelope reaches this
//! bounded parser before any state is consulted. The harness opens it against a
//! FIXED verifying key (the fuzzer cannot forge a valid Ed25519 signature, so it
//! exercises the length / version / field-slicing / signature-parse /
//! verify-fail path). Any panic / slice-out-of-bounds here is a vulnerability.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_attest::fuzz_api::open_release_manifest(data);
});
