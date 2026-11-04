// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `ChannelInviteCard::decode` and `open` (ADR-0087 section
//! 14).
//!
//! A channel invitation card arrives in a chat message's `channel_invite`
//! field, chosen byte for byte by whoever sent the message: the length
//! bounds, the version byte, and then the sealed `/g/` link opened under the
//! secret the card itself carries, the capability decode and the preview.
//!
//! Asserted: no panic, and whatever decodes re-encodes to the same bytes.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(ok) = lakko_contacts::fuzz_api::channel_invite_card_open(data) {
        assert!(ok, "a decoded card must re-encode to the same bytes");
    }
});
