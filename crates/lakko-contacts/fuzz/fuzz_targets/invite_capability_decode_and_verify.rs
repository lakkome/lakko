// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `invite::decode_and_verify` (Phase 1.18-fuzz-parity).
//!
//! Sibling of `call_invite_capability_decode_and_verify` for the
//! **contact-invite** codec (ADR-0005 scenario A; `/i/<slug>`
//! URL). Since the 2026-09-28 security pass the link carries
//! the capability SEALED under a secret in its fragment; the
//! joiner reaches this parser on the bytes the sealed link
//! opens to (`open_link`; the open is fuzzed by
//! `invite_link_open`), and the discovery service no longer
//! parses capabilities. The link's minter chooses those bytes,
//! so any panic is a DoS vector on the joiner's landing page.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = lakko_contacts::invite::decode_and_verify(data);
});
