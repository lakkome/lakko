// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Slim `lakko-contacts` group-contact-request surface — the CLASSIFY half
//! only (ADR-0041 Phase 2, for the ADR-0086 section 6 envelope).
//!
//! The envelope rides a group journal as an application message, so it can
//! be the newest row a push wake decrypts. It must be recognised and
//! suppressed, never rendered as chat text. **No compose and no check live
//! here:** the NSE stores nothing (C-59), and showing a request is the
//! foreground app's work after the receiver's rule.

use lakko_contacts::group_contact_request::{
    decode_group_contact_request, GROUP_CONTACT_REQUEST_ENVELOPE_MAX_LEN,
    GROUP_CONTACT_REQUEST_ENVELOPE_MIN_LEN,
};

/// Whether `plaintext` is a well-formed group contact request the NSE must
/// not render. Fail-closed: anything else answers `false`.
#[uniffi::export]
#[must_use]
pub fn is_group_contact_request_envelope(plaintext: Vec<u8>) -> bool {
    decode_group_contact_request(&plaintext).is_ok()
}

/// The shortest well-formed envelope, from the core's constant.
#[uniffi::export]
#[must_use]
pub fn group_contact_request_envelope_min_len() -> u32 {
    u32::try_from(GROUP_CONTACT_REQUEST_ENVELOPE_MIN_LEN).unwrap_or(u32::MAX)
}

/// The longest well-formed envelope, from the core's constant.
#[uniffi::export]
#[must_use]
pub fn group_contact_request_envelope_max_len() -> u32 {
    u32::try_from(GROUP_CONTACT_REQUEST_ENVELOPE_MAX_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::group_contact_request::seal_group_contact_request;

    fn wire() -> Vec<u8> {
        let target = lakko_crypto::agreement::SecretKey::from_bytes([4; 32]);
        seal_group_contact_request(&target.public_key(), &[6; 20]).unwrap()
    }

    #[test]
    fn recognises_an_envelope_and_refuses_chat() {
        assert!(is_group_contact_request_envelope(wire()));
        let chat = lakko_attachments::compose("hello", Vec::new()).unwrap();
        assert!(!is_group_contact_request_envelope(chat));
    }

    #[test]
    fn an_unfiltered_envelope_is_never_chat_text() {
        assert_eq!(
            lakko_attachments::parse_body(&wire()),
            lakko_attachments::MessageBody::ServiceEnvelope,
        );
    }

    /// The Swift pre-filter keys on the length range and `first == 0x6c`;
    /// pinned as LITERALS so a wire change fails a Rust test.
    #[test]
    fn golden_pins_the_swift_prefilter_shape() {
        // 31 separator + 1 version + 4 length + 32 ephemeral key + 1 + 16 tag.
        assert_eq!(group_contact_request_envelope_min_len(), 85);
        assert_eq!(
            group_contact_request_envelope_max_len(),
            84 + u32::try_from(lakko_contacts::mailbox::MAILBOX_MAX_BODY_LEN).unwrap()
        );
        let w = wire();
        assert_eq!(w[0], 0x6c);
        assert_eq!(&w[..31], b"lakko/group-contact-request/v1\x00");
        assert_eq!(w[31], 0x01, "v1: the version byte after the separator");
        // The envelope names nobody: the target's public key is not in it.
        let target_pub = lakko_crypto::agreement::SecretKey::from_bytes([4; 32])
            .public_key()
            .to_bytes();
        assert!(!w.windows(32).any(|x| x == target_pub));
    }

    #[test]
    fn does_not_cross_accept_sibling_envelopes() {
        let w = wire();
        assert!(!crate::link_founded::is_link_founded_envelope(w.clone()));
        assert!(crate::profile_request::decode_profile_request(w).is_err());
        let founded = lakko_contacts::link_founded::compose_link_founded(
            &lakko_contacts::link_founded::LinkFounded {
                link_id: [1; 16],
                requester_account_key: [2; 32],
                group_id: [3; 32],
                welcome: vec![4; 10],
            },
        )
        .unwrap();
        assert!(!is_group_contact_request_envelope(founded));
    }
}
