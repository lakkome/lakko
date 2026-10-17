// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Slim `lakko-contacts` link-claim surface — the CLASSIFY half only
//! (ADR-0041 Phase 2, for the ADR-0079 link-request claim).
//!
//! A claim rides the account's own DeviceGroup, which this process never
//! reads; if one ever reaches it, it must be recognised and suppressed, never
//! rendered. **No compose and no field access live here:** the NSE stores
//! nothing (C-59), and claiming a request is the foreground app's work.

use lakko_contacts::link_claim::{decode_link_claim, LINK_CLAIM_ENVELOPE_LEN};

/// Whether `plaintext` is a well-formed link-claim envelope the NSE must not
/// render. Fail-closed: anything else, including a malformed envelope with the
/// right separator, answers `false`.
#[uniffi::export]
#[must_use]
pub fn is_link_claim_envelope(plaintext: Vec<u8>) -> bool {
    decode_link_claim(&plaintext).is_ok()
}

/// The one length of a link-claim envelope, from the core's constant.
#[uniffi::export]
#[must_use]
pub fn link_claim_envelope_len() -> u32 {
    u32::try_from(LINK_CLAIM_ENVELOPE_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::link_claim::{compose_link_claim, LinkClaim};

    fn wire() -> Vec<u8> {
        compose_link_claim(&LinkClaim {
            link_id: [3; 16],
            requester_account_key: [4; 32],
        })
    }

    #[test]
    fn recognises_a_claim_and_refuses_chat() {
        assert!(is_link_claim_envelope(wire()));
        let chat = lakko_attachments::compose("hello", Vec::new()).unwrap();
        assert!(!is_link_claim_envelope(chat));
    }

    #[test]
    fn an_unfiltered_claim_is_never_chat_text() {
        assert_eq!(
            lakko_attachments::parse_body(&wire()),
            lakko_attachments::MessageBody::ServiceEnvelope,
        );
    }

    /// The Swift pre-filter keys on the length and `first == 0x6c`; pinned as
    /// LITERALS so a wire change fails a Rust test.
    #[test]
    fn golden_pins_the_swift_prefilter_shape() {
        assert_eq!(link_claim_envelope_len(), 69);
        let w = wire();
        assert_eq!(w[0], 0x6c);
        assert_eq!(&w[..20], b"lakko/link-claim/v1\x00");
        assert_eq!(w[20], 0x01, "v1: the version byte after the separator");
        assert_eq!(w.len(), 69);
    }

    #[test]
    fn does_not_cross_accept_sibling_envelopes() {
        let w = wire();
        assert!(!crate::device_card::is_device_card_envelope(w.clone()));
        assert!(crate::pin::decode_pin_envelope(w).is_err());
        let beacon = lakko_contacts::committer_beacon::compose_committer_beacon();
        assert!(!is_link_claim_envelope(beacon));
    }
}
