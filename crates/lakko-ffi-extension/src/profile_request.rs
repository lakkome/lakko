// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The ADR-0048 profile-request envelope, decode only.
//!
//! The fourth service envelope the NSE must recognise and suppress, next to
//! the push-routing secret, the committer beacon and the group name. A
//! newcomer sends one right after joining, so the newest journal row on a
//! push wake is a request whenever someone has just joined; unfiltered it
//! reaches the notification body as the literal text
//! `lakko/profile-request/v1` plus a control byte, through
//! `parse_body_envelope`'s deliberate lossy-UTF-8 legacy fallback.
//!
//! `compose_profile_request` and `should_request_profiles` are deliberately
//! absent: this process never sends (C-59) and holds no per-group request
//! history to decide with.

use lakko_contacts::profile_request::{
    decode_profile_request as rust_decode_profile_request, PROFILE_REQUEST_ENVELOPE_LEN,
};

use crate::FfiError;

/// Recognise a profile request. `Ok(())` for a well-formed one, an error for
/// a foreign domain or a malformed payload.
#[uniffi::export]
pub fn decode_profile_request(plaintext: Vec<u8>) -> Result<(), FfiError> {
    rust_decode_profile_request(&plaintext)
        .map(|_| ())
        .map_err(|err| FfiError::Contacts(err.to_string()))
}

/// The wire length the Swift pre-filter compares against. Exported rather
/// than hand-copied for the reason `committer_beacon_envelope_len` gives: a
/// stale literal fails in the dangerous direction.
#[uniffi::export]
#[must_use]
pub fn profile_request_envelope_len() -> u32 {
    u32::try_from(PROFILE_REQUEST_ENVELOPE_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::profile_request::{
        compose_profile_request, PROFILE_REQUEST_ENVELOPE_DOMAIN_V1,
    };

    #[test]
    fn decodes_a_well_formed_request() {
        decode_profile_request(compose_profile_request()).expect("a request decodes");
    }

    #[test]
    fn refuses_a_chat_envelope_and_a_malformed_payload() {
        let chat = lakko_attachments::compose("hello", Vec::new()).unwrap();
        assert!(matches!(
            decode_profile_request(chat),
            Err(FfiError::Contacts(_))
        ));
        assert!(matches!(
            decode_profile_request(PROFILE_REQUEST_ENVELOPE_DOMAIN_V1.to_vec()),
            Err(FfiError::Contacts(_))
        ));
        let mut bad_version = PROFILE_REQUEST_ENVELOPE_DOMAIN_V1.to_vec();
        bad_version.push(0xFF);
        assert!(matches!(
            decode_profile_request(bad_version),
            Err(FfiError::Contacts(_))
        ));
    }

    /// The reason the module exists, pinned so the arm cannot be deleted
    /// silently.
    #[test]
    fn an_unfiltered_request_would_otherwise_render_as_chat_text() {
        let wire = compose_profile_request();
        match lakko_attachments::parse_body(&wire) {
            lakko_attachments::MessageBody::Chat { text, .. } => {
                assert!(text.starts_with("lakko/profile-request/v1"));
            },
            other => panic!("expected the lossy Chat fallback, got {other:?}"),
        }
        decode_profile_request(wire).expect("the filter arm must recognise it");
    }

    /// Literals on purpose: the Swift pre-filter keys on the length and the
    /// first byte, and a wire change must fail here.
    #[test]
    fn golden_pins_the_swift_prefilter_shape() {
        let wire = compose_profile_request();
        assert_eq!(wire.len(), 26);
        assert_eq!(wire[0], 0x6c);
        assert_eq!(profile_request_envelope_len(), 26);
        assert_eq!(
            usize::try_from(profile_request_envelope_len()).unwrap(),
            wire.len()
        );
    }

    #[test]
    fn does_not_cross_accept_the_sibling_envelopes() {
        let beacon = lakko_contacts::committer_beacon::compose_committer_beacon();
        assert!(decode_profile_request(beacon.clone()).is_err());
        assert!(
            crate::committer_beacon::decode_committer_beacon(compose_profile_request()).is_err()
        );
    }
}
