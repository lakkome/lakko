// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `group_invite::decode_and_verify` (ADR-0028, G-3).
//!
//! The group join-ticket codec. A prospective joiner decodes the
//! base64url slug of a `/g/<slug>` link a stranger handed them
//! out-of-band, so the bytes are fully attacker-controlled and reach this
//! bounded, fail-closed parser (version gate + length-prefixed group_id
//! bound + Ed25519 signature verify) before any group operation. Any
//! panic / overflow / runaway allocation here is a DoS vector on the
//! joiner's link-landing path.

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
