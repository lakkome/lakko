// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Slim `lakko-contacts` registrar-seed surface — the CLASSIFY half only
//! (ADR-0041 Phase 2, for the ADR-0057 registrar-seed envelope).
//!
//! A journal root sends each member its Delivery Service registrar seed as an
//! ordinary MLS application message, and
//! [`crate::attachments::parse_body_envelope`]'s lossy-UTF-8 fallback would
//! degrade it to a `Chat` body reading `lakko/ds-registrar-seed/v1` plus
//! binary bytes. An owner appoints registrars right after a join, so such a
//! row can be the newest one when a wake arrives; the NSE must recognise it
//! and suppress it.
//!
//! ## Scope: DECODE + the length, nothing else
//!
//! The NSE never opens the seed, never stores it and never answers (C-59):
//! the envelope carries a sealed secret meant for the foreground app's own
//! receive path. **No compose, no open lives here.**

use lakko_contacts::registrar_seed::{decode_registrar_seed, REGISTRAR_SEED_ENVELOPE_LEN};

use crate::FfiError;

/// Whether `plaintext` is a well-formed registrar-seed envelope the NSE must
/// not render. Fail-closed: anything else, including a malformed envelope
/// with the right separator, is refused.
///
/// # Errors
///
/// * [`FfiError::Contacts`] when `plaintext` is not a valid registrar-seed
///   envelope. On the NSE filter path that is the EXPECTED, common outcome.
#[uniffi::export]
pub fn decode_registrar_seed_envelope(plaintext: Vec<u8>) -> Result<(), FfiError> {
    decode_registrar_seed(&plaintext)
        .map(|_| ())
        .map_err(|err| FfiError::Contacts(err.to_string()))
}

/// The one well-formed registrar-seed envelope length in bytes. Derived from
/// the core's constant, so a wire revision moves it here automatically.
#[uniffi::export]
#[must_use]
pub fn registrar_seed_envelope_len() -> u32 {
    u32::try_from(REGISTRAR_SEED_ENVELOPE_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::registrar_seed::{compose_registrar_seed, RegistrarSeedRank};
    use lakko_crypto::agreement::SecretKey;

    fn wire() -> Vec<u8> {
        compose_registrar_seed(
            &[1; 32],
            &[2; 32],
            &SecretKey::from_bytes([3; 32]).public_key(),
            RegistrarSeedRank::Member,
            &[4; 32],
        )
        .unwrap()
    }

    #[test]
    fn recognises_a_registrar_seed_and_refuses_chat() {
        decode_registrar_seed_envelope(wire()).expect("recognised");
        let chat = lakko_attachments::compose("hello", Vec::new()).unwrap();
        assert!(decode_registrar_seed_envelope(chat).is_err());
    }

    /// Pinned as the reason the module exists.
    #[test]
    fn an_unfiltered_registrar_seed_would_otherwise_render_as_chat_text() {
        match lakko_attachments::parse_body(&wire()) {
            lakko_attachments::MessageBody::Chat { text, .. } => {
                assert!(text.starts_with("lakko/ds-registrar-seed/v1"));
            },
            other => panic!("expected the lossy Chat fallback, got {other:?}"),
        }
    }

    /// The Swift pre-filter keys on `count == len && first == 0x6c`; written
    /// as LITERALS so a wire change fails a Rust test.
    #[test]
    fn golden_pins_the_swift_prefilter_shape() {
        assert_eq!(registrar_seed_envelope_len(), 173);
        let w = wire();
        assert_eq!(w.len(), 173);
        assert_eq!(w[0], 0x6c);
        assert_eq!(&w[..27], b"lakko/ds-registrar-seed/v1\x00");
    }

    #[test]
    fn does_not_cross_accept_sibling_envelopes() {
        let w = wire();
        assert!(crate::pin::decode_pin_envelope(w.clone()).is_err());
        assert!(crate::committer_beacon::decode_committer_beacon(w.clone()).is_err());
        assert!(crate::push_routing::decode_push_routing_secret(w.clone()).is_err());
        assert!(crate::profile::decode_profile_envelope(w.clone()).is_err());
        assert!(crate::group_name::decode_group_name_envelope(w.clone()).is_err());
        assert!(crate::profile_request::decode_profile_request(w).is_err());
        let beacon = lakko_contacts::committer_beacon::compose_committer_beacon();
        assert!(decode_registrar_seed_envelope(beacon).is_err());
    }
}
