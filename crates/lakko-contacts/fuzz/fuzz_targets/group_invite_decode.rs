// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `group_invite::decode_and_verify` (ADR-0028, G-3).
//!
//! The plaintext group join-ticket codec. A prospective joiner reaches it
//! on the bytes a sealed `/g/` link opens to (the open is fuzzed by
//! `invite_link_open`, since the 2026-09-28 security pass), and an admin
//! reaches it on the capability a joiner's `group_join_attribution` carries
//! from the Delivery Service knock journal. Either way the bytes are chosen
//! by a stranger and reach this bounded, fail-closed parser (version gate +
//! length-prefixed group_id bound + Ed25519 signature verify) before any
//! group operation. Any panic / overflow / runaway allocation here is a DoS
//! vector on the joiner's link-landing path or the admin's receive loop.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // A capability that verifies (the fuzzer cannot forge the Ed25519 signature,
    // so this arm is reached only from a corpus entry a test minted) also drives
    // the preview open: the sealed bytes are attacker-controlled and the link
    // secret is fixed (HKDF-expanded to the AEAD key, bound to the capability's
    // group id and issuer), so the AEAD refuses and the plaintext decoder is
    // what remains.
    if let Ok(cap) = lakko_contacts::group_invite::decode_and_verify(data) {
        let _ = cap.open_preview(&[0x42u8; lakko_contacts::group_invite::LINK_SECRET_LEN]);
    }
});
