// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the TUF delegated-targets walk (GA-5,
//! `resolve_target`). The harness fixes a top-level targets
//! that delegates `"*"` to one role and returns the raw fuzz
//! input as that role's delegated metadata, so this drives the
//! delegated-metadata `serde_json` parse (now carrying typed
//! `Delegations`) + the signature / threshold / type / expiry
//! verification + the walk's depth / cycle / budget bounds on
//! FULLY attacker-controlled bytes — a compromised mirror
//! serving arbitrary delegated targets reaches exactly here.
//! Any panic / stack-overflow / runaway is a vulnerability.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    lakko_tuf::fuzz_api::resolve_delegated_from_bytes(data);
});
