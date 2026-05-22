// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `try_unwrap_call_kp_forward` (Phase 3c3-fuzz-envelopes).
//!
//! Sibling of `call_welcome_unwrap` for the subsequent-joiner
//! KeyPackage-forward envelope (`lakko/call-keypackage/v1\0` domain
//! separator + ≥ 256-byte payload floor — KPs are larger than
//! Welcomes). The parser sees HPKE-decrypted plaintexts from
//! untrusted senders; any panic is a DoS vector.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_contacts::try_unwrap_call_kp_forward(data);
});
