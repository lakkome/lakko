// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Slim `lakko-contacts` link-founded surface — the CLASSIFY half only
//! (ADR-0041 Phase 2, for the ADR-0079 link-founded note).
//!
//! A note rides the account's own DeviceGroup, which this process never
//! reads; if one ever reaches it, it must be recognised and suppressed, never
//! rendered. **No compose and no field access live here:** the NSE stores
//! nothing (C-59), and answering a request is the foreground app's work.

use lakko_contacts::link_founded::{
    decode_link_founded, LINK_FOUNDED_ENVELOPE_MAX_LEN, LINK_FOUNDED_ENVELOPE_MIN_LEN,
};

/// Whether `plaintext` is a well-formed link-founded note the NSE must not
/// render. Fail-closed: anything else answers `false`.
#[uniffi::export]
#[must_use]
pub fn is_link_founded_envelope(plaintext: Vec<u8>) -> bool {
    decode_link_founded(&plaintext).is_ok()
}

/// The shortest well-formed note, from the core's constant.
#[uniffi::export]
#[must_use]
pub fn link_founded_envelope_min_len() -> u32 {
    u32::try_from(LINK_FOUNDED_ENVELOPE_MIN_LEN).unwrap_or(u32::MAX)
}

/// The longest well-formed note, from the core's constant.
#[uniffi::export]
#[must_use]
pub fn link_founded_envelope_max_len() -> u32 {
    u32::try_from(LINK_FOUNDED_ENVELOPE_MAX_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::link_founded::{compose_link_founded, LinkFounded};

    fn wire() -> Vec<u8> {
        compose_link_founded(&LinkFounded {
            link_id: [3; 16],
            requester_account_key: [4; 32],
            group_id: [5; 32],
            welcome: vec![6; 20],
        })
        .unwrap()
    }

    #[test]
    fn recognises_a_note_and_refuses_chat() {
        assert!(is_link_founded_envelope(wire()));
        let chat = lakko_attachments::compose("hello", Vec::new()).unwrap();
        assert!(!is_link_founded_envelope(chat));
    }

    #[test]
    fn an_unfiltered_note_is_never_chat_text() {
        assert_eq!(
            lakko_attachments::parse_body(&wire()),
            lakko_attachments::MessageBody::ServiceEnvelope,
        );
    }

    /// The Swift pre-filter keys on the length range and `first == 0x6c`;
    /// pinned as LITERALS so a wire change fails a Rust test.
    #[test]
    fn golden_pins_the_swift_prefilter_shape() {
        assert_eq!(link_founded_envelope_min_len(), 106);
        assert_eq!(link_founded_envelope_max_len(), 105 + 32 * 1024);
        let w = wire();
        assert_eq!(w[0], 0x6c);
        assert_eq!(&w[..22], b"lakko/link-founded/v1\x00");
        assert_eq!(w[22], 0x01, "v1: the version byte after the separator");
    }

    #[test]
    fn does_not_cross_accept_sibling_envelopes() {
        let w = wire();
        assert!(!crate::link_claim::is_link_claim_envelope(w.clone()));
        assert!(!crate::device_card::is_device_card_envelope(w));
        let claim = lakko_contacts::link_claim::compose_link_claim(
            &lakko_contacts::link_claim::LinkClaim {
                link_id: [1; 16],
                requester_account_key: [2; 32],
            },
        );
        assert!(!is_link_founded_envelope(claim));
    }
}
