// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A publisher asks the channel's owner to rotate ([ADR-0087] section 15,
//! amended 2026-11-08; UX `docs/ux/group-and-channel-privacy.md` section
//! 15.2).
//!
//! ## Why a carrier is needed
//!
//! Only the owner's device rotates a channel's key (maintainer decision,
//! 2026-11-08): the DS accepts a PublisherGroup commit only from the owner,
//! so a co-publisher's own rotation would fork its group from the owner's.
//! A publisher may still remove a subscriber, and a removal is followed by a
//! rotation so the removed key reads nothing new. The publisher therefore
//! asks: it sends this envelope on the channel's PublisherGroup journal, and
//! the owner's device rotates at its next publisher pass.
//!
//! ## Wire format (v1)
//!
//! ```text
//! CHANNEL_ROTATE_REQUEST_ENVELOPE_DOMAIN_V1 ("lakko/channel-rotate-request/v1\0")
//!   ‖ version(1)=0x01 ‖ channel_id(16)
//! ```
//!
//! Fixed length [`CHANNEL_ROTATE_REQUEST_ENVELOPE_LEN`] (49). It carries no
//! reason, no subscriber and no time: the MLS-authenticated sender is the
//! only claim the owner acts on, and what was removed is on the channels
//! service already.
//!
//! ## The owner's rule
//!
//! The owner's device rotates the channel at its next publisher pass when a
//! request arrived on the PublisherGroup journal of the channel
//! `channel_id` names and the MLS sender resolves at receipt (C-68) to a
//! current publisher of it. A request from anyone else, for another
//! channel, or on another journal is dropped. Requests that arrive before
//! the rotation collapse into that one rotation. A publisher's device never
//! acts on one.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use core::result::Result as CoreResult;

use thiserror::Error;

/// 32-byte domain separator prepended to the envelope. Frozen: the
/// dispatcher matches on it.
pub const CHANNEL_ROTATE_REQUEST_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/channel-rotate-request/v1\x00";

/// Schema-version byte carried first after the separator.
pub const CHANNEL_ROTATE_REQUEST_VERSION_V1: u8 = 0x01;

/// Length of a channel id.
pub const CHANNEL_ROTATE_REQUEST_CHANNEL_ID_LEN: usize = 16;

/// Bytes after the separator: version and channel id.
pub const CHANNEL_ROTATE_REQUEST_PAYLOAD_LEN: usize = 1 + CHANNEL_ROTATE_REQUEST_CHANNEL_ID_LEN;

/// The one well-formed length.
pub const CHANNEL_ROTATE_REQUEST_ENVELOPE_LEN: usize =
    CHANNEL_ROTATE_REQUEST_ENVELOPE_DOMAIN_V1.len() + CHANNEL_ROTATE_REQUEST_PAYLOAD_LEN;

/// Why a rotate-request envelope was refused. A reason code; nothing from
/// the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ChannelRotateRequestError {
    /// No domain separator. Only from [`decode_channel_rotate_request`].
    #[error("plaintext does not carry the channel rotate-request domain separator")]
    WrongDomain,
    /// The payload is not exactly the fixed length.
    #[error("channel rotate-request envelope has the wrong length")]
    BadLength,
    /// The version byte is not [`CHANNEL_ROTATE_REQUEST_VERSION_V1`].
    #[error("channel rotate-request envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
}

impl ChannelRotateRequestError {
    /// Stable, language-neutral reason code. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::BadLength => "badLength",
            Self::UnknownVersion(_) => "unknownVersion",
        }
    }
}

/// Compose the envelope a publisher sends on the channel's PublisherGroup
/// journal to ask the owner's device to rotate.
#[must_use]
pub fn compose_channel_rotate_request(
    channel_id: &[u8; CHANNEL_ROTATE_REQUEST_CHANNEL_ID_LEN],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(CHANNEL_ROTATE_REQUEST_ENVELOPE_LEN);
    out.extend_from_slice(CHANNEL_ROTATE_REQUEST_ENVELOPE_DOMAIN_V1);
    out.push(CHANNEL_ROTATE_REQUEST_VERSION_V1);
    out.extend_from_slice(channel_id);
    out
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher:
/// `Ok(None)` when the prefix does not match, else the channel id.
///
/// # Errors
///
/// [`ChannelRotateRequestError::UnknownVersion`] (checked before the length)
/// and [`ChannelRotateRequestError::BadLength`].
pub fn try_unwrap_channel_rotate_request(
    plaintext: &[u8],
) -> CoreResult<Option<[u8; CHANNEL_ROTATE_REQUEST_CHANNEL_ID_LEN]>, ChannelRotateRequestError> {
    let Some(payload) = plaintext.strip_prefix(CHANNEL_ROTATE_REQUEST_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let (&version, rest) = payload
        .split_first()
        .ok_or(ChannelRotateRequestError::BadLength)?;
    if version != CHANNEL_ROTATE_REQUEST_VERSION_V1 {
        return Err(ChannelRotateRequestError::UnknownVersion(version));
    }
    let channel: [u8; CHANNEL_ROTATE_REQUEST_CHANNEL_ID_LEN] = rest
        .try_into()
        .map_err(|_| ChannelRotateRequestError::BadLength)?;
    Ok(Some(channel))
}

/// Strict decoder: a plaintext without the separator is a hard error.
///
/// # Errors
///
/// [`ChannelRotateRequestError::WrongDomain`] and every error of
/// [`try_unwrap_channel_rotate_request`].
pub fn decode_channel_rotate_request(
    bytes: &[u8],
) -> CoreResult<[u8; CHANNEL_ROTATE_REQUEST_CHANNEL_ID_LEN], ChannelRotateRequestError> {
    try_unwrap_channel_rotate_request(bytes)?.ok_or(ChannelRotateRequestError::WrongDomain)
}

/// The fuzz oracle behind `fuzz_api::channel_rotate_request_decode`: `None`
/// when the decoder refuses; `Some(true)` when what decoded re-composes to
/// the same bytes at the fixed length.
pub(crate) fn fuzz_decode(bytes: &[u8]) -> Option<bool> {
    let channel = decode_channel_rotate_request(bytes).ok()?;
    Some(
        compose_channel_rotate_request(&channel) == bytes
            && bytes.len() == CHANNEL_ROTATE_REQUEST_ENVELOPE_LEN,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const CHANNEL: [u8; CHANNEL_ROTATE_REQUEST_CHANNEL_ID_LEN] =
        [0x0C; CHANNEL_ROTATE_REQUEST_CHANNEL_ID_LEN];

    #[test]
    fn compose_decode_round_trips_at_a_fixed_length() {
        let bytes = compose_channel_rotate_request(&CHANNEL);
        assert_eq!(CHANNEL_ROTATE_REQUEST_ENVELOPE_DOMAIN_V1.len(), 32);
        assert_eq!(bytes.len(), CHANNEL_ROTATE_REQUEST_ENVELOPE_LEN);
        assert_eq!(CHANNEL_ROTATE_REQUEST_ENVELOPE_LEN, 49);
        assert_eq!(decode_channel_rotate_request(&bytes), Ok(CHANNEL));
        assert_eq!(fuzz_decode(&bytes), Some(true));
    }

    #[test]
    fn version_and_length_are_checked() {
        let good = compose_channel_rotate_request(&CHANNEL);
        let domain = CHANNEL_ROTATE_REQUEST_ENVELOPE_DOMAIN_V1.len();
        assert_eq!(
            try_unwrap_channel_rotate_request(&good[..good.len() - 1]),
            Err(ChannelRotateRequestError::BadLength)
        );
        let mut long = good.clone();
        long.push(0);
        assert_eq!(
            try_unwrap_channel_rotate_request(&long),
            Err(ChannelRotateRequestError::BadLength)
        );
        let mut v2 = good;
        v2[domain] = 0x02;
        assert_eq!(
            try_unwrap_channel_rotate_request(&v2),
            Err(ChannelRotateRequestError::UnknownVersion(0x02))
        );
        assert_eq!(
            try_unwrap_channel_rotate_request(CHANNEL_ROTATE_REQUEST_ENVELOPE_DOMAIN_V1),
            Err(ChannelRotateRequestError::BadLength)
        );
        assert_eq!(try_unwrap_channel_rotate_request(b"hello"), Ok(None));
        assert_eq!(
            decode_channel_rotate_request(b"hello"),
            Err(ChannelRotateRequestError::WrongDomain)
        );
    }

    #[test]
    fn separator_distinct_from_sibling_domains_and_dispatched() {
        for d in [
            crate::channel_epoch_seeds::CHANNEL_EPOCH_SEEDS_ENVELOPE_DOMAIN_V1,
            crate::channel_read_key::CHANNEL_READ_KEY_ENVELOPE_DOMAIN_V1,
            crate::admission_seed::ADMISSION_SEED_ENVELOPE_DOMAIN_V1,
            crate::knock_seed::KNOCK_SEED_ENVELOPE_DOMAIN_V1,
            crate::registrar_seed::REGISTRAR_SEED_ENVELOPE_DOMAIN_V1,
            crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
            b"lakko/publisher-step-down/v1\x00".as_slice(),
        ] {
            assert!(
                !d.starts_with(CHANNEL_ROTATE_REQUEST_ENVELOPE_DOMAIN_V1)
                    && !CHANNEL_ROTATE_REQUEST_ENVELOPE_DOMAIN_V1.starts_with(d),
                "{d:?}"
            );
        }
        assert!(matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(
                &compose_channel_rotate_request(&CHANNEL)
            ),
            Ok(crate::incoming_envelope::IncomingEnvelope::ChannelRotateRequest(c)) if c == CHANNEL
        ));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn any_channel_round_trips(channel in any::<[u8; 16]>()) {
            let bytes = compose_channel_rotate_request(&channel);
            prop_assert_eq!(decode_channel_rotate_request(&bytes), Ok(channel));
        }

        #[test]
        fn arbitrary_bytes_behind_the_domain_never_panic(tail in proptest::collection::vec(any::<u8>(), 0..64)) {
            let mut bytes = CHANNEL_ROTATE_REQUEST_ENVELOPE_DOMAIN_V1.to_vec();
            bytes.extend_from_slice(&tail);
            if let Some(ok) = fuzz_decode(&bytes) {
                prop_assert!(ok);
            }
        }
    }
}
