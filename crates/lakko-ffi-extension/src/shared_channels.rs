// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Slim `lakko-contacts` shared-channels surface — the CLASSIFY half only
//! (ADR-0041 Phase 2, for the ADR-0059 §6 shared-channels envelope).
//!
//! ## Why the NSE needs this at all
//!
//! The same reason every sibling arm exists. A member who shares the channels
//! they follow publishes the list as an ordinary MLS application message on
//! the group journal — when the switch goes on or off, when they subscribe or
//! unsubscribe, and when the group grows — and
//! [`crate::attachments::parse_body_envelope`]'s deliberate lossy-UTF-8
//! fallback degrades it to a `Chat` body. Unfiltered, the notification reads
//! `lakko/shared-channels/v1` followed by binary bytes and channel handles.
//!
//! ## Scope: DECODE + the length bounds, nothing else
//!
//! [`decode_shared_channels_envelope`] answers "is this a shared-channels
//! envelope I must not render?" and, like the profile arm, **returns nothing
//! else**: which channels a person follows is personal data, and the NSE is
//! read-only-throwaway (C-59) — it must never store or act on it. Compose and
//! the newer-than order are absent for the same reason.
//!
//! [`shared_channels_envelope_min_len`] / [`shared_channels_envelope_max_len`]
//! are what the NSE's cheap pre-filter needs: the envelope is variable-length,
//! so the peek is a RANGE plus the first byte, both ends exported rather than
//! hand-copied into Swift (the ADR-0044 drift class).
//!
//! A malformed envelope carrying the domain is refused like a non-envelope and
//! falls through to the chat parser, as for every sibling arm.

use lakko_contacts::shared_channels::{
    decode_shared_channels as rust_decode_shared_channels, SHARED_CHANNELS_ENVELOPE_MAX_LEN,
    SHARED_CHANNELS_ENVELOPE_MIN_LEN,
};

use crate::FfiError;

/// Strictly decode a shared-channels envelope (ADR-0059 §6). Returns `Ok(())`
/// — the classification is the entire result: "this is a service envelope,
/// render nothing". The list is deliberately not returned.
///
/// # Errors
///
/// * [`FfiError::Contacts`] when `plaintext` is not a valid shared-channels
///   envelope. On the NSE filter path that is the EXPECTED, common outcome.
#[uniffi::export]
pub fn decode_shared_channels_envelope(plaintext: Vec<u8>) -> Result<(), FfiError> {
    rust_decode_shared_channels(&plaintext)
        .map(|_| ())
        .map_err(|err| FfiError::Contacts(err.to_string()))
}

/// Shortest well-formed shared-channels envelope in bytes (an empty list).
#[uniffi::export]
#[must_use]
pub fn shared_channels_envelope_min_len() -> u32 {
    u32::try_from(SHARED_CHANNELS_ENVELOPE_MIN_LEN).unwrap_or(u32::MAX)
}

/// Longest well-formed shared-channels envelope in bytes (32 maximal entries).
/// Exported and pinned by a golden: a bound that is too LOW fails in the
/// dangerous direction, silently ceasing to suppress.
#[uniffi::export]
#[must_use]
pub fn shared_channels_envelope_max_len() -> u32 {
    u32::try_from(SHARED_CHANNELS_ENVELOPE_MAX_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::shared_channels::{
        compose_shared_channels, SharedChannel, SharedChannels, SHARED_CHANNELS_ENVELOPE_DOMAIN_V1,
    };

    fn list(n: u8) -> SharedChannels {
        SharedChannels {
            updated_at_ms: 1_700_000_000_000,
            channels: (0..n)
                .map(|i| SharedChannel {
                    channel_id: [i; 16],
                    handle: "lakko_news".to_string(),
                    name: "Lakko News".to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn decodes_a_well_formed_list() {
        for n in [0, 1, 3] {
            decode_shared_channels_envelope(compose_shared_channels(&list(n)).unwrap())
                .expect("a shared-channels envelope decodes");
        }
    }

    #[test]
    fn refuses_a_chat_envelope() {
        let chat = lakko_attachments::compose("hello", Vec::new()).unwrap();
        assert!(matches!(
            decode_shared_channels_envelope(chat),
            Err(FfiError::Contacts(_))
        ));
    }

    #[test]
    fn an_unfiltered_list_would_otherwise_render_as_chat_text() {
        let wire = compose_shared_channels(&list(1)).unwrap();
        match lakko_attachments::parse_body(&wire) {
            lakko_attachments::MessageBody::Chat { text, .. } => assert!(
                text.starts_with("lakko/shared-channels/v1"),
                "the lossy fallback is what this arm pre-empts",
            ),
            other => panic!("expected the lossy Chat fallback, got {other:?}"),
        }
        decode_shared_channels_envelope(wire).expect("the filter arm must recognise it");
    }

    /// The Swift pre-filter keys on `min <= count <= max && first == 0x6c`.
    /// Written as LITERALS so a wire change fails a Rust test rather than
    /// silently narrowing the NSE peek.
    #[test]
    fn golden_pins_the_swift_prefilter_shape() {
        assert_eq!(shared_channels_envelope_min_len(), 35);
        assert_eq!(shared_channels_envelope_max_len(), 9795);
        let smallest = compose_shared_channels(&list(0)).unwrap();
        assert_eq!(smallest.len(), 35);
        assert_eq!(smallest[0], 0x6c);
        assert_eq!(&smallest[..25], b"lakko/shared-channels/v1\x00");
        assert_eq!(
            SHARED_CHANNELS_ENVELOPE_DOMAIN_V1,
            b"lakko/shared-channels/v1\x00"
        );
    }

    #[test]
    fn does_not_cross_accept_sibling_envelopes() {
        let wire = compose_shared_channels(&list(1)).unwrap();
        assert!(crate::pin::decode_pin_envelope(wire.clone()).is_err());
        assert!(crate::group_name::decode_group_name_envelope(wire.clone()).is_err());
        assert!(crate::profile::decode_profile_envelope(wire).is_err());
        let beacon = lakko_contacts::committer_beacon::compose_committer_beacon();
        assert!(decode_shared_channels_envelope(beacon).is_err());
    }
}
