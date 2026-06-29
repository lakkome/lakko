// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `group_join_attribution::decode_attribution` (ADR-0028 OQ2, G-3).
//!
//! The join-attribution envelope a joiner posts to the group journal alongside
//! its external-join proposal, conveying which member's `/g/` link the request
//! arrived on. An admin's poll loop decodes journal entries that a malicious
//! poster / the zero-trust DS could have crafted, so the bytes are fully
//! attacker-controlled and reach this bounded, fail-closed parser (version gate
//! + length-prefixed joiner-key + capability bounds + embedded
//! `GroupInviteCapability` Ed25519 verify) before any field is surfaced to the
//! admin. Any panic / overflow / runaway allocation here is a DoS vector on the
//! admin's pending-request UI.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_contacts::group_join_attribution::decode_attribution(data);
});
