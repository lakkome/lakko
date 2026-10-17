// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Slim `lakko-contacts` device-card surface — the CLASSIFY half only
//! (ADR-0041 Phase 2, for the ADR-0080 device card).
//!
//! A device card rides the account's own DeviceGroup, which this process
//! never reads; if one ever reaches it, it must be recognised and
//! suppressed, never rendered. **No compose, no field access lives here:**
//! the NSE stores nothing (C-59) and has no use for what a device says
//! about itself.

use lakko_contacts::device_card::{
    decode_device_card, DEVICE_CARD_ENVELOPE_MAX_LEN, DEVICE_CARD_ENVELOPE_MIN_LEN,
};

/// Whether `plaintext` is a well-formed device-card envelope the NSE must not
/// render. Fail-closed: anything else, including a malformed envelope with
/// the right separator, answers `false`.
#[uniffi::export]
#[must_use]
pub fn is_device_card_envelope(plaintext: Vec<u8>) -> bool {
    decode_device_card(&plaintext).is_ok()
}

/// Shortest well-formed device-card envelope, from the core's constant.
#[uniffi::export]
#[must_use]
pub fn device_card_envelope_min_len() -> u32 {
    u32::try_from(DEVICE_CARD_ENVELOPE_MIN_LEN).unwrap_or(u32::MAX)
}

/// Longest well-formed device-card envelope, from the core's constant.
#[uniffi::export]
#[must_use]
pub fn device_card_envelope_max_len() -> u32 {
    u32::try_from(DEVICE_CARD_ENVELOPE_MAX_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::device_card::{compose_device_card, DeviceCard, DevicePlatform};

    fn wire() -> Vec<u8> {
        compose_device_card(&DeviceCard {
            updated_at_ms: 2,
            last_active_ms: 1,
            platform: DevicePlatform::Macos,
            model: "MacBook Pro".to_string(),
            app_version: "1.4".to_string(),
            os: "macOS 26".to_string(),
            city: String::new(),
            country: String::new(),
        })
        .unwrap()
    }

    #[test]
    fn recognises_a_card_and_refuses_chat() {
        assert!(is_device_card_envelope(wire()));
        let chat = lakko_attachments::compose("hello", Vec::new()).unwrap();
        assert!(!is_device_card_envelope(chat));
    }

    #[test]
    fn an_unfiltered_card_is_never_chat_text() {
        assert_eq!(
            lakko_attachments::parse_body(&wire()),
            lakko_attachments::MessageBody::ServiceEnvelope,
        );
    }

    /// The Swift pre-filter keys on the length range and `first == 0x6c`;
    /// pinned as LITERALS so a wire change fails a Rust test.
    #[test]
    fn golden_pins_the_swift_prefilter_shape() {
        assert_eq!(device_card_envelope_min_len(), 44);
        assert_eq!(device_card_envelope_max_len(), 270);
        let w = wire();
        assert_eq!(w[0], 0x6c);
        assert_eq!(&w[..21], b"lakko/device-card/v1\x00");
        assert_eq!(w[21], 0x01, "v1: the version byte after the separator");
        assert_eq!(w.len(), 44 + 11 + 3 + 8);
    }

    #[test]
    fn does_not_cross_accept_sibling_envelopes() {
        let w = wire();
        assert!(crate::pin::decode_pin_envelope(w.clone()).is_err());
        assert!(crate::profile::decode_profile_envelope(w.clone()).is_err());
        assert!(crate::group_name::decode_group_name_envelope(w).is_err());
        let beacon = lakko_contacts::committer_beacon::compose_committer_beacon();
        assert!(!is_device_card_envelope(beacon));
    }
}
