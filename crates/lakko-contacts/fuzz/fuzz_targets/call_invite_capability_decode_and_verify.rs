// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `call_invite::decode_and_verify`
//! (Phase 3c3-fuzz-envelopes).
//!
//! The owner-side discovery service runs this on every uploaded
//! `CallInviteCapability` blob before storing it; the joiner
//! re-runs it locally on every server-fetched blob before
//! consuming (defence-in-depth against malicious-server
//! substitution per C-31). Both sides handle attacker-controlled
//! bytes — any panic is a DoS vector for the discovery service
//! and the joiner-side landing page alike.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_contacts::call_invite::decode_and_verify(data);
});
