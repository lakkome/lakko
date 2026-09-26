// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Slim `lakko-contacts` shared-pin surface — the CLASSIFY half only
//! (ADR-0041 Phase 2, for the ADR-0052 pin envelope).
//!
//! A pin for everyone is sent as an ordinary MLS application message on the
//! conversation's journal, and [`crate::attachments::parse_body_envelope`]'s
//! deliberate lossy-UTF-8 fallback degrades it to a `Chat` body reading
//! `lakko/pin/v1` plus binary bytes. Pinning something is exactly the kind of
//! row that is newest when a wake arrives, so the NSE must recognise it.
//!
//! ## Scope: DECODE + the length, nothing else
//!
//! [`decode_pin_envelope`] answers "is this a pin envelope I must not render
//! as a message?" and returns the operation, which the NSE may use to word a
//! preview ("pinned a message") but must never persist (C-59): the acceptance
//! decision — the role gate and the newer-than order — is the app's, on the
//! app's own receive path. **No compose, no order comparison, no role gate
//! lives here.**
//!
//! [`pin_envelope_len`] is what the NSE's cheap pre-filter needs; the envelope
//! is fixed-length, so the peek is one length plus the first byte, exported
//! rather than hand-copied into Swift (the ADR-0044 drift class).

use lakko_contacts::pin::{decode_pin as rust_decode_pin, PinAction, PIN_ENVELOPE_LEN};

use crate::FfiError;

/// One shared-pin operation (ADR-0052 decision 3), as decoded by the slim
/// surface. Field-identical to `lakko-ffi-uniffi`'s `FfiPinOp`.
///
/// **Read-only in this process (C-59).**
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiPinOp {
    /// DS `messageId` of the message in this conversation.
    pub message_id: i64,
    /// `true` = pinned for everyone; `false` = no longer pinned.
    pub pinned: bool,
    /// Sender-clock UNIX milliseconds at which the action was taken.
    pub updated_at_ms: u64,
}

/// Strictly decode a shared-pin envelope. Fail-closed: a plaintext without
/// the pin domain separator, of the wrong length, with an unknown version or
/// an unknown action is refused as a whole.
///
/// # Errors
///
/// * [`FfiError::Contacts`] when `plaintext` is not a valid pin envelope. On
///   the NSE filter path that is the EXPECTED, common outcome — every chat
///   message lands here — not a fault.
#[uniffi::export]
pub fn decode_pin_envelope(plaintext: Vec<u8>) -> Result<FfiPinOp, FfiError> {
    rust_decode_pin(&plaintext)
        .map(|op| FfiPinOp {
            message_id: op.message_id,
            pinned: matches!(op.action, PinAction::Pin),
            updated_at_ms: op.updated_at_ms,
        })
        .map_err(|err| FfiError::Contacts(err.to_string()))
}

/// The one well-formed pin envelope length in bytes. Derived from the core's
/// constant, so a wire revision moves it here automatically.
#[uniffi::export]
#[must_use]
pub fn pin_envelope_len() -> u32 {
    u32::try_from(PIN_ENVELOPE_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_contacts::pin::{compose_pin, PinOp, PIN_ENVELOPE_DOMAIN_V1};

    fn op(pinned: bool) -> PinOp {
        PinOp {
            message_id: 41,
            action: if pinned {
                PinAction::Pin
            } else {
                PinAction::Unpin
            },
            updated_at_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn decodes_a_well_formed_pin() {
        for pinned in [true, false] {
            let decoded = decode_pin_envelope(compose_pin(&op(pinned))).expect("decodes");
            assert_eq!(decoded.message_id, 41);
            assert_eq!(decoded.pinned, pinned);
            assert_eq!(decoded.updated_at_ms, 1_700_000_000_000);
        }
    }

    #[test]
    fn refuses_a_chat_envelope() {
        let chat = lakko_attachments::compose("hello", Vec::new()).unwrap();
        assert!(matches!(
            decode_pin_envelope(chat),
            Err(FfiError::Contacts(_))
        ));
    }

    /// Pinned as the reason the module exists.
    #[test]
    fn an_unfiltered_pin_would_otherwise_render_as_chat_text() {
        let wire = compose_pin(&op(true));
        match lakko_attachments::parse_body(&wire) {
            lakko_attachments::MessageBody::Chat { text, .. } => assert!(
                text.starts_with("lakko/pin/v1"),
                "the lossy fallback is what this arm pre-empts",
            ),
            other => panic!("expected the lossy Chat fallback, got {other:?}"),
        }
        decode_pin_envelope(wire).expect("the filter arm must recognise it");
    }

    /// The Swift pre-filter keys on `count == len && first == 0x6c`; written
    /// as LITERALS so a wire change fails a Rust test rather than silently
    /// narrowing the NSE peek.
    #[test]
    fn golden_pins_the_swift_prefilter_shape() {
        assert_eq!(pin_envelope_len(), 31);
        let wire = compose_pin(&op(true));
        assert_eq!(wire.len(), 31);
        assert_eq!(wire[0], 0x6c);
        assert_eq!(&wire[..13], b"lakko/pin/v1\x00");
        assert!(matches!(
            decode_pin_envelope(PIN_ENVELOPE_DOMAIN_V1.to_vec()),
            Err(FfiError::Contacts(_))
        ));
    }

    #[test]
    fn does_not_cross_accept_sibling_envelopes() {
        let pin_wire = compose_pin(&op(true));
        assert!(crate::committer_beacon::decode_committer_beacon(pin_wire.clone()).is_err());
        assert!(crate::push_routing::decode_push_routing_secret(pin_wire.clone()).is_err());
        assert!(crate::profile::decode_profile_envelope(pin_wire.clone()).is_err());
        assert!(crate::group_name::decode_group_name_envelope(pin_wire.clone()).is_err());
        assert!(crate::profile_request::decode_profile_request(pin_wire).is_err());

        let beacon = lakko_contacts::committer_beacon::compose_committer_beacon();
        assert!(decode_pin_envelope(beacon).is_err());
        let request = lakko_contacts::profile_request::compose_profile_request();
        assert!(decode_pin_envelope(request).is_err());
    }
}
