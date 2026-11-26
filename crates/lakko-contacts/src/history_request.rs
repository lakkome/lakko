// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A request for a conversation's history, newest first ([ADR-0101]
//! section 9).
//!
//! After a key reset, once the new identity's device is back in a
//! conversation, it asks for what it missed inside that conversation: it
//! mints a fresh X25519 gap key (the ADR-0091 construction,
//! [`crate::gap_history`]) and sends this envelope as an MLS application
//! message. The other person of a one-to-one, or an owner or admin of a
//! group, answers with [`crate::gap_history::seal_gap_history`] to that gap
//! key, unchanged: a page of rows posted to
//! [`crate::gap_history::gap_history_rendezvous`] of the gap key, the newest
//! page first. The device asks for the next page with another request whose
//! `before_message_id` is the oldest message id it received, until the
//! sender has no more. The receiver keeps a page only from a signer
//! `lakko_mls::Group::gap_history_signer_accepted` accepts (ADR-0091).
//!
//! ## Wire format (version 1)
//!
//! ```text
//! HISTORY_REQUEST_DOMAIN_V1 ("lakko/history-request/v1\0")
//!   ‖ version(1)=0x01 ‖ gap_key_pub(32) ‖ before_message_id(8 BE i64)
//! ```
//!
//! Fixed length [`HISTORY_REQUEST_ENVELOPE_LEN`] (66). `before_message_id`
//! is a Delivery Service message id; `0` means "the newest". A negative id
//! is refused (DS ids are positive). The version byte is checked before the
//! length. The gap key's usability is checked where it is used: the sealer
//! refuses a low-order key.
//!
//! [ADR-0101]: ../../../docs/adr/0101-reaching-a-reset-account.md

use core::result::Result as CoreResult;

use thiserror::Error;

use crate::gap_history::GAP_KEY_LEN;

/// Domain separator. Frozen: the dispatcher matches on it.
pub const HISTORY_REQUEST_DOMAIN_V1: &[u8] = b"lakko/history-request/v1\x00";

/// Schema-version byte carried first after the separator.
pub const HISTORY_REQUEST_VERSION_V1: u8 = 0x01;

/// The one well-formed length.
pub const HISTORY_REQUEST_ENVELOPE_LEN: usize =
    HISTORY_REQUEST_DOMAIN_V1.len() + 1 + GAP_KEY_LEN + 8;

/// A history request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryRequest {
    /// The public half of the fresh gap key the answer is sealed to.
    pub gap_key_pub: [u8; GAP_KEY_LEN],
    /// Answer with messages older than this DS message id; `0` = the newest.
    pub before_message_id: i64,
}

/// Why a history request was refused. A reason code; nothing from the
/// payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum HistoryRequestError {
    /// No domain separator. Only from [`decode_history_request`].
    #[error("plaintext does not carry the history-request domain separator")]
    WrongDomain,
    /// The payload is not exactly the fixed length.
    #[error("history request has the wrong length")]
    BadLength,
    /// The version byte is not [`HISTORY_REQUEST_VERSION_V1`].
    #[error("history request carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
    /// `before_message_id` is negative.
    #[error("history request names a negative message id")]
    BadMessageId,
}

impl HistoryRequestError {
    /// Stable, language-neutral reason code. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::BadLength => "badLength",
            Self::UnknownVersion(_) => "unknownVersion",
            Self::BadMessageId => "badMessageId",
        }
    }
}

/// Compose a history request.
///
/// # Errors
///
/// [`HistoryRequestError::BadMessageId`] for a negative id: compose refuses
/// what decode refuses.
pub fn compose_history_request(req: &HistoryRequest) -> CoreResult<Vec<u8>, HistoryRequestError> {
    if req.before_message_id < 0 {
        return Err(HistoryRequestError::BadMessageId);
    }
    let mut out = Vec::with_capacity(HISTORY_REQUEST_ENVELOPE_LEN);
    out.extend_from_slice(HISTORY_REQUEST_DOMAIN_V1);
    out.push(HISTORY_REQUEST_VERSION_V1);
    out.extend_from_slice(&req.gap_key_pub);
    out.extend_from_slice(&req.before_message_id.to_be_bytes());
    Ok(out)
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher:
/// `Ok(None)` when the prefix does not match.
///
/// # Errors
///
/// [`HistoryRequestError::UnknownVersion`] (checked before the length),
/// [`HistoryRequestError::BadLength`], [`HistoryRequestError::BadMessageId`].
pub fn try_unwrap_history_request(
    plaintext: &[u8],
) -> CoreResult<Option<HistoryRequest>, HistoryRequestError> {
    let Some(payload) = plaintext.strip_prefix(HISTORY_REQUEST_DOMAIN_V1) else {
        return Ok(None);
    };
    let (&version, rest) = payload
        .split_first()
        .ok_or(HistoryRequestError::BadLength)?;
    if version != HISTORY_REQUEST_VERSION_V1 {
        return Err(HistoryRequestError::UnknownVersion(version));
    }
    if rest.len() != GAP_KEY_LEN + 8 {
        return Err(HistoryRequestError::BadLength);
    }
    let (key, id) = rest.split_at(GAP_KEY_LEN);
    let gap_key_pub: [u8; GAP_KEY_LEN] =
        key.try_into().map_err(|_| HistoryRequestError::BadLength)?;
    let before_message_id =
        i64::from_be_bytes(id.try_into().map_err(|_| HistoryRequestError::BadLength)?);
    if before_message_id < 0 {
        return Err(HistoryRequestError::BadMessageId);
    }
    Ok(Some(HistoryRequest {
        gap_key_pub,
        before_message_id,
    }))
}

/// Strict decoder: a plaintext without the separator is a hard error.
///
/// # Errors
///
/// [`HistoryRequestError::WrongDomain`] and every error of
/// [`try_unwrap_history_request`].
pub fn decode_history_request(bytes: &[u8]) -> CoreResult<HistoryRequest, HistoryRequestError> {
    try_unwrap_history_request(bytes)?.ok_or(HistoryRequestError::WrongDomain)
}

/// The fuzz oracle behind `fuzz_api::history_request_decode`: `None` when
/// the decoder refuses; `Some(true)` when what decoded re-composes to the
/// same bytes at the fixed length.
pub(crate) fn fuzz_decode(bytes: &[u8]) -> Option<bool> {
    let req = decode_history_request(bytes).ok()?;
    Some(
        compose_history_request(&req).ok()?.as_slice() == bytes
            && bytes.len() == HISTORY_REQUEST_ENVELOPE_LEN,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const REQ: HistoryRequest = HistoryRequest {
        gap_key_pub: [0x6A; GAP_KEY_LEN],
        before_message_id: 4242,
    };

    #[test]
    fn round_trips_at_a_fixed_length() {
        let wire = compose_history_request(&REQ).unwrap();
        assert_eq!(HISTORY_REQUEST_ENVELOPE_LEN, 66);
        assert_eq!(wire.len(), HISTORY_REQUEST_ENVELOPE_LEN);
        assert_eq!(decode_history_request(&wire), Ok(REQ));
        assert_eq!(fuzz_decode(&wire), Some(true));
        let newest = HistoryRequest {
            before_message_id: 0,
            ..REQ
        };
        let wire = compose_history_request(&newest).unwrap();
        assert_eq!(decode_history_request(&wire), Ok(newest));
    }

    #[test]
    fn version_length_and_id_are_checked() {
        let wire = compose_history_request(&REQ).unwrap();
        let d = HISTORY_REQUEST_DOMAIN_V1.len();
        let mut v2 = wire.clone();
        v2[d] = 2;
        assert_eq!(
            decode_history_request(&v2),
            Err(HistoryRequestError::UnknownVersion(2))
        );
        assert_eq!(
            decode_history_request(&wire[..wire.len() - 1]),
            Err(HistoryRequestError::BadLength)
        );
        let mut long = wire.clone();
        long.push(0);
        assert_eq!(
            decode_history_request(&long),
            Err(HistoryRequestError::BadLength)
        );
        assert_eq!(
            decode_history_request(HISTORY_REQUEST_DOMAIN_V1),
            Err(HistoryRequestError::BadLength)
        );
        let mut negative = wire;
        negative[d + 1 + GAP_KEY_LEN] = 0x80;
        assert_eq!(
            decode_history_request(&negative),
            Err(HistoryRequestError::BadMessageId)
        );
        assert_eq!(
            compose_history_request(&HistoryRequest {
                before_message_id: -1,
                ..REQ
            }),
            Err(HistoryRequestError::BadMessageId)
        );
        assert_eq!(try_unwrap_history_request(b"hello"), Ok(None));
        assert_eq!(
            decode_history_request(b"hello"),
            Err(HistoryRequestError::WrongDomain)
        );
    }

    #[test]
    fn it_dispatches_and_does_not_collide_with_its_siblings() {
        let wire = compose_history_request(&REQ).unwrap();
        assert!(matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(&wire),
            Ok(crate::incoming_envelope::IncomingEnvelope::HistoryRequest(r)) if r == REQ
        ));
        for other in [
            crate::profile_request::PROFILE_REQUEST_ENVELOPE_DOMAIN_V1,
            crate::conversation_reentry::CONVERSATION_REENTRY_ENVELOPE_DOMAIN_V1,
            crate::channel_return::CHANNEL_RETURN_DOMAIN_V1,
            crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
            crate::gap_history::GAP_HISTORY_AAD_DOMAIN,
            crate::gap_history::GAP_HISTORY_SIG_DOMAIN,
        ] {
            assert!(!other.starts_with(HISTORY_REQUEST_DOMAIN_V1));
            assert!(!HISTORY_REQUEST_DOMAIN_V1.starts_with(other));
        }
    }

    proptest! {
        #[test]
        fn decode_never_panics_and_round_trips(bytes in proptest::collection::vec(any::<u8>(), 0..80)) {
            let mut framed = HISTORY_REQUEST_DOMAIN_V1.to_vec();
            framed.extend_from_slice(&bytes);
            if let Some(ok) = fuzz_decode(&framed) {
                prop_assert!(ok);
            }
            let _ = fuzz_decode(&bytes);
        }
    }
}
