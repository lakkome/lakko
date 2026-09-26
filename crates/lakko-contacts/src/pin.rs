// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Shared pin envelope (ADR-0052 decision 3): one member tells the
//! conversation that a message is pinned for everyone, or no longer is.
//!
//! ## Why this exists
//!
//! A pin "for me" is a flag on one device and needs no wire. A pin "for
//! everyone" is group state, and group state needs three things
//! (`docs/ux/message-actions.md` section 4, ADR-0052 decision 3): who may
//! write it, which write survives, and a reading that runs against current
//! state. This module carries the wire and owns the ORDER; the authority is
//! the receiver's role-table check and the lock is the receiver's, exactly as
//! for the group name (ADR-0051), whose template this reuses.
//!
//! ## Wire format (v1)
//!
//! ```text
//! PIN_ENVELOPE_DOMAIN_V1 || version(1) || updated_at_ms(8, BE) || action(1) || message_id(8, BE i64)
//! ```
//!
//! Fixed length: [`PIN_ENVELOPE_LEN`] bytes. One envelope names ONE message,
//! so a conversation's shared pins are a set the receiver folds envelope by
//! envelope: for each `message_id` the newest action wins. "Several pins, no
//! product limit" is therefore free at this layer; the envelope stays bounded
//! like every sibling.
//!
//! ## The order
//!
//! [`pin_is_newer`] compares two stamps for the SAME message: strictly greater
//! wins, equal is not newer. A re-send (a growth republish) carries the
//! original stamp verbatim and is therefore not an update, and two people
//! acting on one message with one stamp keep what is stored. The receiver
//! consults it against its CURRENT stored stamp under its per-group lock
//! (C-62 sub-invariant 5); consulted against a stale snapshot it decides
//! nothing, which is the ADR-0044 lesson applied before the defect.
//!
//! ## What the receiver still does
//!
//! Resolves the MLS sender to the account key at receipt (C-68) and accepts
//! only from the owner or an admin of the live role table - or from any member
//! when the group has no table, and from either participant in a one-to-one
//! (ADR-0052: "Pin for both", either may clear). Nothing in the envelope
//! asserts who sent it.

use core::result::Result as CoreResult;

use thiserror::Error;

/// 13-byte domain separator prepended to the pin payload. The trailing
/// `\x00` keeps a UTF-8 free-text payload from ever starting with it.
///
/// **The `v1` names the envelope FAMILY.** These bytes are frozen:
/// [`crate::incoming_envelope`] dispatches on this exact prefix; the
/// in-payload version byte is the knob that moves.
pub const PIN_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/pin/v1\x00";

/// Schema-version byte carried first after the separator.
pub const PIN_VERSION_V1: u8 = 0x01;

/// Bytes after the separator: version, `updated_at_ms`, action, `message_id`.
const PAYLOAD_LEN: usize = 1 + 8 + 1 + 8;

/// The one well-formed length: the envelope has no variable field.
pub const PIN_ENVELOPE_LEN: usize = PIN_ENVELOPE_DOMAIN_V1.len() + PAYLOAD_LEN;

const ACTION_PIN: u8 = 0x01;
const ACTION_UNPIN: u8 = 0x02;

/// What the sender did to the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinAction {
    /// The message is pinned for everyone.
    Pin,
    /// The message is no longer pinned for everyone.
    Unpin,
}

/// One shared-pin operation on one message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinOp {
    /// DS `messageId` of the message in this conversation.
    pub message_id: i64,
    /// Pin or unpin.
    pub action: PinAction,
    /// Sender-clock UNIX milliseconds at which the action was taken. Orders
    /// actions on the same message via [`pin_is_newer`]; persisted and
    /// re-emitted verbatim on a re-send.
    pub updated_at_ms: u64,
}

/// `true` when an action stamped `incoming_updated_at_ms` must replace the
/// stored action stamped `stored_updated_at_ms` for the **same message**.
///
/// Strictly greater wins; **equal is not newer**.
#[must_use]
pub const fn pin_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    incoming_updated_at_ms > stored_updated_at_ms
}

/// Compose a pin envelope: the inner plaintext handed to the MLS
/// application-message send path on the conversation's journal.
#[must_use]
pub fn compose_pin(op: &PinOp) -> Vec<u8> {
    let mut out = Vec::with_capacity(PIN_ENVELOPE_LEN);
    out.extend_from_slice(PIN_ENVELOPE_DOMAIN_V1);
    out.push(PIN_VERSION_V1);
    out.extend_from_slice(&op.updated_at_ms.to_be_bytes());
    out.push(match op.action {
        PinAction::Pin => ACTION_PIN,
        PinAction::Unpin => ACTION_UNPIN,
    });
    out.extend_from_slice(&op.message_id.to_be_bytes());
    out
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher.
///
/// * `Ok(Some(op))` — the separator matched and the envelope is well-formed.
/// * `Ok(None)` — the prefix does not match; fall through.
/// * `Err(_)` — the prefix matched but the payload is malformed. The caller
///   drops it (fail-closed), never renders it.
///
/// # Errors
///
/// * [`PinError::UnknownVersion`] — the version byte is not
///   [`PIN_VERSION_V1`]. Checked BEFORE the length, so a later revision that
///   changes the length reports as a version, not as a truncation.
/// * [`PinError::BadLength`] — the payload is not exactly [`PAYLOAD_LEN`].
/// * [`PinError::UnknownAction`] — the action byte is neither pin nor unpin.
pub fn try_unwrap_pin(plaintext: &[u8]) -> CoreResult<Option<PinOp>, PinError> {
    let Some(payload) = plaintext.strip_prefix(PIN_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let (&version, rest) = payload.split_first().ok_or(PinError::BadLength {
        actual: payload.len(),
    })?;
    if version != PIN_VERSION_V1 {
        return Err(PinError::UnknownVersion(version));
    }
    if rest.len() != PAYLOAD_LEN - 1 {
        return Err(PinError::BadLength {
            actual: payload.len(),
        });
    }
    let updated_at_bytes: [u8; 8] = rest[0..8].try_into().map_err(|_| PinError::BadLength {
        actual: payload.len(),
    })?;
    let action = match rest[8] {
        ACTION_PIN => PinAction::Pin,
        ACTION_UNPIN => PinAction::Unpin,
        other => return Err(PinError::UnknownAction(other)),
    };
    let message_id_bytes: [u8; 8] = rest[9..17].try_into().map_err(|_| PinError::BadLength {
        actual: payload.len(),
    })?;
    Ok(Some(PinOp {
        message_id: i64::from_be_bytes(message_id_bytes),
        action,
        updated_at_ms: u64::from_be_bytes(updated_at_bytes),
    }))
}

/// Strict standalone decoder: a non-pin plaintext is a hard error.
///
/// # Errors
///
/// * [`PinError::WrongDomain`] — `bytes` does not start with
///   [`PIN_ENVELOPE_DOMAIN_V1`].
/// * Every error [`try_unwrap_pin`] returns.
pub fn decode_pin(bytes: &[u8]) -> CoreResult<PinOp, PinError> {
    try_unwrap_pin(bytes)?.ok_or(PinError::WrongDomain)
}

/// Why a pin envelope was refused. A reason code; nothing from the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PinError {
    /// The plaintext does not carry the pin domain separator. Only from
    /// [`decode_pin`]; [`try_unwrap_pin`] returns `Ok(None)`.
    #[error("plaintext does not carry the pin domain separator")]
    WrongDomain,
    /// The payload is not exactly the fixed length.
    #[error("pin envelope has {actual} payload bytes, expected {PAYLOAD_LEN}")]
    BadLength {
        /// Payload bytes after the separator.
        actual: usize,
    },
    /// The version byte is not [`PIN_VERSION_V1`].
    #[error("pin envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// The action byte is neither pin nor unpin.
    #[error("pin envelope carries unknown action {0:#04x}")]
    UnknownAction(u8),
}

impl PinError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::BadLength { .. } => "badLength",
            Self::UnknownVersion(_) => "unknownVersion",
            Self::UnknownAction(_) => "unknownAction",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(message_id: i64, action: PinAction, at: u64) -> PinOp {
        PinOp {
            message_id,
            action,
            updated_at_ms: at,
        }
    }

    fn raw(version: u8, at: u64, action: u8, message_id: i64) -> Vec<u8> {
        let mut out = PIN_ENVELOPE_DOMAIN_V1.to_vec();
        out.push(version);
        out.extend_from_slice(&at.to_be_bytes());
        out.push(action);
        out.extend_from_slice(&message_id.to_be_bytes());
        out
    }

    #[test]
    fn compose_then_decode_round_trips_and_is_fixed_length() {
        for o in [
            op(41, PinAction::Pin, 1_700_000_000_000),
            op(i64::MAX, PinAction::Unpin, u64::MAX),
            op(-7, PinAction::Pin, 0),
        ] {
            let bytes = compose_pin(&o);
            assert_eq!(bytes.len(), PIN_ENVELOPE_LEN);
            assert_eq!(decode_pin(&bytes).unwrap(), o);
            assert_eq!(try_unwrap_pin(&bytes).unwrap(), Some(o));
        }
    }

    #[test]
    fn domain_separator_is_frozen() {
        assert_eq!(PIN_ENVELOPE_DOMAIN_V1, b"lakko/pin/v1\x00");
        assert_eq!(PIN_ENVELOPE_LEN, 13 + 18);
        assert_eq!(compose_pin(&op(1, PinAction::Pin, 2))[0], b'l');
    }

    #[test]
    fn a_foreign_plaintext_falls_through_and_decode_refuses_it() {
        assert_eq!(try_unwrap_pin(b"hello").unwrap(), None);
        assert_eq!(decode_pin(b"hello"), Err(PinError::WrongDomain));
        assert_eq!(try_unwrap_pin(b"").unwrap(), None);
    }

    #[test]
    fn a_bare_separator_is_an_error_not_a_fall_through() {
        assert!(matches!(
            try_unwrap_pin(PIN_ENVELOPE_DOMAIN_V1),
            Err(PinError::BadLength { actual: 0 })
        ));
    }

    #[test]
    fn version_is_checked_before_length() {
        let mut v2 = PIN_ENVELOPE_DOMAIN_V1.to_vec();
        v2.push(0x02);
        assert_eq!(try_unwrap_pin(&v2), Err(PinError::UnknownVersion(0x02)));
        let mut v2_long = raw(0x02, 1, ACTION_PIN, 1);
        v2_long.push(0);
        assert_eq!(
            try_unwrap_pin(&v2_long),
            Err(PinError::UnknownVersion(0x02))
        );
    }

    #[test]
    fn a_short_or_long_payload_is_refused() {
        let good = raw(PIN_VERSION_V1, 1, ACTION_PIN, 1);
        assert!(matches!(
            try_unwrap_pin(&good[..good.len() - 1]),
            Err(PinError::BadLength { .. })
        ));
        let mut long = good;
        long.push(0);
        assert!(matches!(
            try_unwrap_pin(&long),
            Err(PinError::BadLength { .. })
        ));
    }

    #[test]
    fn an_unknown_action_is_refused() {
        assert_eq!(
            try_unwrap_pin(&raw(PIN_VERSION_V1, 1, 0x03, 1)),
            Err(PinError::UnknownAction(0x03))
        );
        assert_eq!(
            try_unwrap_pin(&raw(PIN_VERSION_V1, 1, 0x00, 1)),
            Err(PinError::UnknownAction(0x00))
        );
    }

    #[test]
    fn the_order_is_strict_and_a_resend_is_not_an_update() {
        assert!(pin_is_newer(11, 10));
        assert!(!pin_is_newer(10, 10));
        assert!(!pin_is_newer(9, 10));
        assert!(!pin_is_newer(0, 0));
        assert!(pin_is_newer(u64::MAX, u64::MAX - 1));
    }

    #[test]
    fn an_unpin_stamped_later_wins_over_a_pin_and_vice_versa() {
        let pinned = op(41, PinAction::Pin, 10);
        let unpinned = op(41, PinAction::Unpin, 11);
        assert!(pin_is_newer(unpinned.updated_at_ms, pinned.updated_at_ms));
        let pinned_again = op(41, PinAction::Pin, 12);
        assert!(pin_is_newer(
            pinned_again.updated_at_ms,
            unpinned.updated_at_ms
        ));
    }

    #[test]
    fn separator_distinct_from_sibling_domains() {
        let other_domains: &[&[u8]] = &[
            crate::call_welcome::CALL_WELCOME_ENVELOPE_DOMAIN_V1,
            crate::call_kp_forward::CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1,
            crate::media_state::MEDIA_STATE_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_GID_DERIVE_DOMAIN_V1,
            crate::committer_beacon::COMMITTER_BEACON_ENVELOPE_DOMAIN_V1,
            crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
            crate::profile_request::PROFILE_REQUEST_ENVELOPE_DOMAIN_V1,
            crate::group_name::GROUP_NAME_ENVELOPE_DOMAIN_V1,
            crate::invite::REVOKE_DOMAIN_V1,
            crate::invite::OWNER_LIST_DOMAIN_V1,
            crate::invite::JOINER_TAG_SALT_DOMAIN_V1,
            crate::call_invite::CALL_INVITE_REVOKE_DOMAIN_V1,
            crate::group_invite::GROUP_INVITE_REVOKE_DOMAIN_V1,
            b"lakko/group-join-attribution/v1",
        ];
        for d in other_domains {
            assert_ne!(*d, PIN_ENVELOPE_DOMAIN_V1, "{d:?}");
            assert!(
                !d.starts_with(PIN_ENVELOPE_DOMAIN_V1) && !PIN_ENVELOPE_DOMAIN_V1.starts_with(d),
                "neither separator may prefix the other: {d:?}",
            );
        }
    }
}
