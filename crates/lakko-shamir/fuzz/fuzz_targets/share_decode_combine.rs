// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — the social-recovery share parse + reconstruct path
//! (`Share::decode` + `combine`, via `fuzz_api::decode_and_combine`).
//! Shares are distributed to and later collected from peers / guardians,
//! so a malicious holder can submit crafted share strings. The harness
//! treats the input as newline-separated candidate shares, decodes each
//! via the `<index>:<base64>` parser, and `combine`s the decoded set,
//! exercising the share-count / length / duplicate-index guards and the
//! GF(256) Lagrange interpolation (the duplicate-index check is what
//! keeps the field division from dividing by zero). Any panic / overflow
//! / divide-by-zero here is a vulnerability.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_shamir::fuzz_api::decode_and_combine(data);
});
