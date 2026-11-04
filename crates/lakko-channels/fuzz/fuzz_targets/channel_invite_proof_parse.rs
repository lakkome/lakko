// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `InviteProof::parse` and `verify_invite_proof` (ADR-0087
//! section 7).
//!
//! The `x-lakko-channel-invite-proof` header reaches the channels service on
//! every subscribe to a private channel, from anyone, before any key is
//! looked up: the length bound, the five-field split, the version, three
//! base64url decodes with fixed lengths, the decimal time, and then the
//! window and the signature under the key the header names.
//!
//! Asserted: no panic, and whatever parses re-encodes to the same text.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(value) = core::str::from_utf8(data) {
        if let Some(ok) = lakko_channels::fuzz_api::channel_invite_proof_parse(value) {
            assert!(ok, "a parsed invite proof must re-encode to the same text");
        }
    }
});
