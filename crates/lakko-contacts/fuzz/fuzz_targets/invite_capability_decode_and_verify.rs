// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `invite::decode_and_verify` (Phase 1.18-fuzz-parity).
//!
//! Sibling of `call_invite_capability_decode_and_verify` for the
//! **contact-invite** codec (ADR-0005 scenario A; `/i/<slug>`
//! URL). The discovery service runs this on every uploaded
//! `InviteCapability` blob server-side AND the joiner re-runs
//! it locally on every server-fetched blob before consuming
//! (defence-in-depth re-verify per `contact_invite_full_e2e_via_envelope`).
//! Both sides handle attacker-controlled bytes — any panic is
//! a DoS vector for the discovery service and the joiner-side
//! landing page alike.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_contacts::invite::decode_and_verify(data);
});
