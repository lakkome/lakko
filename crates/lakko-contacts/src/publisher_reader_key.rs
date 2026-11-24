// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A publishing account tells the channel's other publishers which
//! subscriber key is its own reader key (`docs/ux/group-and-channel-privacy.md`
//! section 15.1; live walk 2026-11-24).
//!
//! ## Why a carrier is needed
//!
//! The device that holds a channel's PublisherGroup registers its account's
//! one subscriber key at the channels service as a subscriber, so the
//! account's other devices can read. The service counts that row like any
//! subscriber's and cannot tell it apart. A publisher knows its own reader
//! key; the owner and every other publisher did not, so the owner saw a
//! co-publisher's key as an anonymous "From an invitation" row with
//! `Remove`, counted it, and the publisher saw the owner's the same way.
//! Every publishing device therefore sends its account's reader key on the
//! PublisherGroup journal after registering it, and again when the
//! PublisherGroup grows, and every publishing device leaves the reader keys
//! of current publishers out of the subscriber list and the count.
//!
//! ## Wire format (v1)
//!
//! ```text
//! PUBLISHER_READER_KEY_ENVELOPE_DOMAIN_V1 ("lakko/publisher-reader-key/v1\0")
//!   ‖ version(1)=0x01 ‖ channel_id(16) ‖ reader_key(32)
//! ```
//!
//! Fixed length [`PUBLISHER_READER_KEY_ENVELOPE_LEN`] (79). The reader key is
//! the Ed25519 public subscriber key the service holds as a row; it reads
//! nothing by itself and every publisher already sees it in the row list.
//!
//! ## The receiver's rule
//!
//! A publishing device keeps, per channel and per publisher account, the
//! reader key named by the newest envelope that arrived on that channel's
//! PublisherGroup journal from a sender the MLS layer resolves at receipt
//! (C-68) to a current publisher account. It leaves those keys of current
//! publishers out of the subscriber list and out of the count. A publisher
//! that names another subscriber's key only hides a row it could remove
//! anyway. An envelope for another channel, on another journal, or from a
//! non-publisher is dropped.

use core::result::Result as CoreResult;

use thiserror::Error;

/// Domain separator prepended to the envelope. Frozen: the dispatcher
/// matches on it.
pub const PUBLISHER_READER_KEY_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/publisher-reader-key/v1\x00";

/// Schema-version byte carried first after the separator.
pub const PUBLISHER_READER_KEY_VERSION_V1: u8 = 0x01;

/// Length of a channel id.
pub const PUBLISHER_READER_KEY_CHANNEL_ID_LEN: usize = 16;

/// Length of the reader key (an Ed25519 public subscriber key).
pub const PUBLISHER_READER_KEY_LEN: usize = 32;

/// Bytes after the separator: version, channel id, reader key.
pub const PUBLISHER_READER_KEY_PAYLOAD_LEN: usize =
    1 + PUBLISHER_READER_KEY_CHANNEL_ID_LEN + PUBLISHER_READER_KEY_LEN;

/// The one well-formed length.
pub const PUBLISHER_READER_KEY_ENVELOPE_LEN: usize =
    PUBLISHER_READER_KEY_ENVELOPE_DOMAIN_V1.len() + PUBLISHER_READER_KEY_PAYLOAD_LEN;

/// A decoded publisher reader key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublisherReaderKey {
    /// The channel.
    pub channel_id: [u8; PUBLISHER_READER_KEY_CHANNEL_ID_LEN],
    /// The sending account's reader key, as the channels service holds it.
    pub reader_key: [u8; PUBLISHER_READER_KEY_LEN],
}

/// Why a publisher reader-key envelope was refused. A reason code; nothing
/// from the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PublisherReaderKeyError {
    /// No domain separator. Only from [`decode_publisher_reader_key`].
    #[error("plaintext does not carry the publisher reader-key domain separator")]
    WrongDomain,
    /// The payload is not exactly the fixed length.
    #[error("publisher reader-key envelope has the wrong length")]
    BadLength,
    /// The version byte is not [`PUBLISHER_READER_KEY_VERSION_V1`].
    #[error("publisher reader-key envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
}

impl PublisherReaderKeyError {
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

/// Compose the envelope a publishing device sends after registering its
/// account's reader key, and again on PublisherGroup growth.
#[must_use]
pub fn compose_publisher_reader_key(key: &PublisherReaderKey) -> Vec<u8> {
    let mut out = Vec::with_capacity(PUBLISHER_READER_KEY_ENVELOPE_LEN);
    out.extend_from_slice(PUBLISHER_READER_KEY_ENVELOPE_DOMAIN_V1);
    out.push(PUBLISHER_READER_KEY_VERSION_V1);
    out.extend_from_slice(&key.channel_id);
    out.extend_from_slice(&key.reader_key);
    out
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher:
/// `Ok(None)` when the prefix does not match.
///
/// # Errors
///
/// [`PublisherReaderKeyError::UnknownVersion`] (checked before the length)
/// and [`PublisherReaderKeyError::BadLength`].
pub fn try_unwrap_publisher_reader_key(
    plaintext: &[u8],
) -> CoreResult<Option<PublisherReaderKey>, PublisherReaderKeyError> {
    let Some(payload) = plaintext.strip_prefix(PUBLISHER_READER_KEY_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let (&version, rest) = payload
        .split_first()
        .ok_or(PublisherReaderKeyError::BadLength)?;
    if version != PUBLISHER_READER_KEY_VERSION_V1 {
        return Err(PublisherReaderKeyError::UnknownVersion(version));
    }
    if rest.len() != PUBLISHER_READER_KEY_CHANNEL_ID_LEN + PUBLISHER_READER_KEY_LEN {
        return Err(PublisherReaderKeyError::BadLength);
    }
    let (channel, key) = rest.split_at(PUBLISHER_READER_KEY_CHANNEL_ID_LEN);
    let mut channel_id = [0u8; PUBLISHER_READER_KEY_CHANNEL_ID_LEN];
    channel_id.copy_from_slice(channel);
    let mut reader_key = [0u8; PUBLISHER_READER_KEY_LEN];
    reader_key.copy_from_slice(key);
    Ok(Some(PublisherReaderKey {
        channel_id,
        reader_key,
    }))
}

/// Strict decoder: a plaintext without the separator is a hard error.
///
/// # Errors
///
/// [`PublisherReaderKeyError::WrongDomain`] and every error of
/// [`try_unwrap_publisher_reader_key`].
pub fn decode_publisher_reader_key(
    bytes: &[u8],
) -> CoreResult<PublisherReaderKey, PublisherReaderKeyError> {
    try_unwrap_publisher_reader_key(bytes)?.ok_or(PublisherReaderKeyError::WrongDomain)
}

/// The fuzz oracle behind `fuzz_api::publisher_reader_key_decode`: `None`
/// when the decoder refuses; `Some(true)` when what decoded re-composes to
/// the same bytes at the fixed length.
pub(crate) fn fuzz_decode(bytes: &[u8]) -> Option<bool> {
    let key = decode_publisher_reader_key(bytes).ok()?;
    Some(
        compose_publisher_reader_key(&key) == bytes
            && bytes.len() == PUBLISHER_READER_KEY_ENVELOPE_LEN,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const KEY: PublisherReaderKey = PublisherReaderKey {
        channel_id: [0x0C; 16],
        reader_key: [0xD7; 32],
    };

    #[test]
    fn compose_decode_round_trips_at_a_fixed_length() {
        let bytes = compose_publisher_reader_key(&KEY);
        assert_eq!(PUBLISHER_READER_KEY_ENVELOPE_DOMAIN_V1.len(), 30);
        assert_eq!(bytes.len(), PUBLISHER_READER_KEY_ENVELOPE_LEN);
        assert_eq!(PUBLISHER_READER_KEY_ENVELOPE_LEN, 79);
        assert_eq!(bytes[0], b'l');
        assert_eq!(&bytes[47..], &KEY.reader_key);
        assert_eq!(decode_publisher_reader_key(&bytes), Ok(KEY));
        assert_eq!(fuzz_decode(&bytes), Some(true));
        assert!(matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(&bytes),
            Ok(crate::incoming_envelope::IncomingEnvelope::PublisherReaderKey(k)) if k == KEY
        ));
    }

    #[test]
    fn version_and_length_are_checked() {
        let good = compose_publisher_reader_key(&KEY);
        let domain = PUBLISHER_READER_KEY_ENVELOPE_DOMAIN_V1.len();
        assert_eq!(
            try_unwrap_publisher_reader_key(&good[..good.len() - 1]),
            Err(PublisherReaderKeyError::BadLength)
        );
        let mut long = good.clone();
        long.push(0);
        assert_eq!(
            try_unwrap_publisher_reader_key(&long),
            Err(PublisherReaderKeyError::BadLength)
        );
        let mut v2 = good;
        v2[domain] = 0x02;
        assert_eq!(
            try_unwrap_publisher_reader_key(&v2),
            Err(PublisherReaderKeyError::UnknownVersion(0x02))
        );
        assert_eq!(
            try_unwrap_publisher_reader_key(PUBLISHER_READER_KEY_ENVELOPE_DOMAIN_V1),
            Err(PublisherReaderKeyError::BadLength)
        );
        assert_eq!(try_unwrap_publisher_reader_key(b"hello"), Ok(None));
        assert_eq!(
            decode_publisher_reader_key(b"hello"),
            Err(PublisherReaderKeyError::WrongDomain)
        );
        let mut broken = PUBLISHER_READER_KEY_ENVELOPE_DOMAIN_V1.to_vec();
        broken.push(1);
        assert!(crate::incoming_envelope::dispatch_incoming_envelope(&broken).is_err());
    }

    /// The new separator is not a prefix of any sibling envelope domain, nor
    /// the reverse, and it dispatches as itself.
    #[test]
    fn separator_is_isolated_from_every_sibling_domain() {
        let siblings: &[&[u8]] = &[
            crate::admission_seed::ADMISSION_SEED_ENVELOPE_DOMAIN_V1,
            crate::channel_admission_mark::CHANNEL_ADMISSION_MARK_ENVELOPE_DOMAIN_V1,
            crate::channel_epoch_seeds::CHANNEL_EPOCH_SEEDS_ENVELOPE_DOMAIN_V1,
            crate::channel_read_key::CHANNEL_READ_KEY_ENVELOPE_DOMAIN_V1,
            crate::channel_rotate_request::CHANNEL_ROTATE_REQUEST_ENVELOPE_DOMAIN_V1,
            crate::committer_beacon::COMMITTER_BEACON_ENVELOPE_DOMAIN_V1,
            crate::committer_delegation::COMMITTER_DELEGATION_ENVELOPE_DOMAIN_V1,
            crate::device_card::DEVICE_CARD_ENVELOPE_DOMAIN_V1,
            crate::group_name::GROUP_NAME_ENVELOPE_DOMAIN_V1,
            crate::pin::PIN_ENVELOPE_DOMAIN_V1,
            crate::profile::PROFILE_ENVELOPE_DOMAIN_V1,
            crate::profile_request::PROFILE_REQUEST_ENVELOPE_DOMAIN_V1,
            crate::publisher_device_request::PUBLISHER_DEVICE_REQUEST_ENVELOPE_DOMAIN_V1,
            crate::push_routing::PUSH_ROUTING_SECRET_ENVELOPE_DOMAIN_V1,
            crate::registrar_seed::REGISTRAR_SEED_ENVELOPE_DOMAIN_V1,
            b"lakko/publisher-step-down/v1\x00".as_slice(),
        ];
        let ours = PUBLISHER_READER_KEY_ENVELOPE_DOMAIN_V1;
        for s in siblings {
            assert!(!ours.starts_with(s) && !s.starts_with(ours), "{s:?}");
        }
        for s in siblings {
            let mut other = s.to_vec();
            other.extend_from_slice(&[1; 49]);
            assert!(!matches!(
                crate::incoming_envelope::dispatch_incoming_envelope(&other),
                Ok(crate::incoming_envelope::IncomingEnvelope::PublisherReaderKey(_))
            ));
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn arbitrary_bytes_behind_the_domain_never_panic(tail in proptest::collection::vec(any::<u8>(), 0..120)) {
            let mut bytes = PUBLISHER_READER_KEY_ENVELOPE_DOMAIN_V1.to_vec();
            bytes.extend_from_slice(&tail);
            if let Some(ok) = fuzz_decode(&bytes) {
                prop_assert!(ok);
            }
        }

        #[test]
        fn any_key_round_trips(channel in any::<[u8; 16]>(), key in any::<[u8; 32]>()) {
            let k = PublisherReaderKey { channel_id: channel, reader_key: key };
            let bytes = compose_publisher_reader_key(&k);
            prop_assert_eq!(decode_publisher_reader_key(&bytes), Ok(k));
        }
    }
}
