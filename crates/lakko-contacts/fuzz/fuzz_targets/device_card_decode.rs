// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Fuzz target — `decode_device_card` (ADR-0080).
//!
//! A device card arrives as a DeviceGroup `ServiceEnvelope`, from another
//! device of the account or from the zero-trust DS replaying a corrupt row,
//! and reaches this bounded, fail-closed decoder before any state is
//! consulted: two stamps, a platform byte from a closed set, and five
//! one-byte-length-prefixed strings with UTF-8 and character-table checks.
//!
//! Asserted, not merely exercised:
//!
//! * no input panics the decoder;
//! * a decoded card is within the advertised length bounds;
//! * round-trip stability: whatever decodes re-composes to exactly the input
//!   (the encoding is canonical) and decodes to the identical card;
//! * `normalized` is the identity on a card that already validates, and
//!   always produces a card that validates;
//! * a card is never newer than itself.

#![no_main]

use libfuzzer_sys::fuzz_target;

use lakko_contacts::device_card::{
    compose_device_card, decode_device_card, device_card_is_newer, DEVICE_CARD_ENVELOPE_MAX_LEN,
    DEVICE_CARD_ENVELOPE_MIN_LEN,
};

fuzz_target!(|data: &[u8]| {
    let Ok(card) = decode_device_card(data) else {
        return;
    };
    assert!((DEVICE_CARD_ENVELOPE_MIN_LEN..=DEVICE_CARD_ENVELOPE_MAX_LEN).contains(&data.len()));
    let recomposed = compose_device_card(&card).expect("compose must accept what decode accepted");
    assert_eq!(recomposed.as_slice(), data, "the encoding must be canonical");
    assert_eq!(decode_device_card(&recomposed).expect("must decode"), card);
    let normalized = card.normalized();
    assert_eq!(normalized, card, "normalized must not change a valid card");
    assert!(normalized.validate().is_ok());
    assert!(!device_card_is_newer(card.updated_at_ms, card.updated_at_ms));
});
