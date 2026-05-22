// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `try_unwrap_call_welcome` (Phase 3c3-fuzz-envelopes).
//!
//! Drives arbitrary bytes through the owner-side unwrap helper for
//! the call-welcome envelope (`lakko/call-welcome/v1\0` domain
//! separator + ≥ 32-byte payload floor). The parser sees
//! HPKE-decrypted plaintexts the platform-side polling loop hands
//! it, so its input space is **fully attacker-controlled** after
//! the HPKE-seal opens — any panic, integer overflow, or
//! out-of-bounds index in the parser is a DoS vector.
//!
//! Reject-as-`None` (not-a-welcome) and reject-as-`Err(TruncatedPayload)`
//! (a hostile envelope) are both acceptable failure modes; panic is
//! not.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_contacts::try_unwrap_call_welcome(data);
});
