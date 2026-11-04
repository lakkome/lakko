// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_group_reactions` (UX `reactions-and-emoji.md`
//! section 9, the group allowed-reactions envelope).
//!
//! The envelope arrives as an MLS application message on a group journal —
//! from any member (the role gate runs AFTER this decode, on the receiver)
//! or from the zero-trust DS replaying a corrupt row — so the bytes reach
//! this bounded, fail-closed decoder before any state is consulted. The
//! value is the shared allowed-reactions encoding (a mode byte, then a
//! counted list of length-prefixed UTF-8 entries) the channel metadata
//! record carries too, so this also drives that codec.
//!
//! Asserted, not merely exercised:
//!
//! * no input panics the decoder;
//! * a decoded envelope is within the advertised length bounds;
//! * round-trip stability: anything that decodes re-composes to the
//!   identical bytes (the encoding is canonical);
//! * a stamp is never newer than itself;
//! * the standalone value codec agrees with the envelope on the value it
//!   carried.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::allowed_reactions::AllowedReactions;
use lakko_contacts::group_reactions::{
    compose_group_reactions, decode_group_reactions, group_reactions_is_newer,
    GROUP_REACTIONS_ENVELOPE_DOMAIN_V1, GROUP_REACTIONS_ENVELOPE_MAX_LEN,
    GROUP_REACTIONS_ENVELOPE_MIN_LEN,
};

fuzz_target!(|data: &[u8]| {
    // The bare value codec on the raw input, so the list parser sees
    // arbitrary bytes without the fuzzer having to find the domain first.
    let _ = AllowedReactions::from_bytes(data);

    let Ok(value) = decode_group_reactions(data) else {
        return;
    };
    assert!(
        (GROUP_REACTIONS_ENVELOPE_MIN_LEN..=GROUP_REACTIONS_ENVELOPE_MAX_LEN).contains(&data.len())
    );
    let recomposed =
        compose_group_reactions(&value).expect("compose must accept what decode accepted");
    assert_eq!(recomposed.as_slice(), data, "the encoding must be canonical");
    assert!(!group_reactions_is_newer(
        value.updated_at_ms,
        value.updated_at_ms
    ));
    let tail = &data[GROUP_REACTIONS_ENVELOPE_DOMAIN_V1.len() + 9..];
    assert_eq!(AllowedReactions::from_bytes(tail), Ok(value.reactions));
});
