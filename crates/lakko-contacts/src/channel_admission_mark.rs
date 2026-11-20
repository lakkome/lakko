// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! A publishing device shares how far it has checked a private channel's
//! subscriber rows ([ADR-0090] section 6).
//!
//! ## Why a carrier is needed
//!
//! A publisher reseals a private channel's seed only to subscriber rows at or
//! below its admission mark (`admittedThroughSeq`): the service's row sequence
//! up to which every row was checked against an admission ticket and every
//! invalid one removed. Since ADR-0090 several devices publish for one
//! channel, and a device that never set the mark resealed to nobody. Every
//! publishing device therefore sends the mark it advanced on the
//! PublisherGroup journal, and every publishing device keeps the largest it
//! saw.
//!
//! ## Wire format (v1)
//!
//! ```text
//! CHANNEL_ADMISSION_MARK_ENVELOPE_DOMAIN_V1 ("lakko/channel-admission-mark/v1\0")
//!   ‖ version(1)=0x01 ‖ channel_id(16) ‖ through_seq(8, BE)
//! ```
//!
//! Fixed length [`CHANNEL_ADMISSION_MARK_ENVELOPE_LEN`] (57).
//!
//! ## The receiver's rule
//!
//! A publishing device keeps `max(held, through_seq)` for the channel when the
//! envelope arrived on that channel's PublisherGroup journal and the MLS
//! sender resolves at receipt (C-68) to a current publisher account (the
//! owner's or a co-publisher's). Merging by maximum is safe: a device
//! advances its mark only after removing every invalid row at or below it, so
//! every row still at the service at or below any device's mark was admitted.
//! A co-publisher that inflates the mark gives the seed to rows it could hand
//! the seed to anyway.
//!
//! [ADR-0090]: ../../../docs/adr/0090-publisher-group-device-leaves.md

use core::result::Result as CoreResult;

use thiserror::Error;

/// Domain separator prepended to the envelope. Frozen: the dispatcher
/// matches on it.
pub const CHANNEL_ADMISSION_MARK_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/channel-admission-mark/v1\x00";

/// Schema-version byte carried first after the separator.
pub const CHANNEL_ADMISSION_MARK_VERSION_V1: u8 = 0x01;

/// Length of a channel id.
pub const CHANNEL_ADMISSION_MARK_CHANNEL_ID_LEN: usize = 16;

/// Bytes after the separator: version, channel id, mark.
pub const CHANNEL_ADMISSION_MARK_PAYLOAD_LEN: usize = 1 + CHANNEL_ADMISSION_MARK_CHANNEL_ID_LEN + 8;

/// The one well-formed length.
pub const CHANNEL_ADMISSION_MARK_ENVELOPE_LEN: usize =
    CHANNEL_ADMISSION_MARK_ENVELOPE_DOMAIN_V1.len() + CHANNEL_ADMISSION_MARK_PAYLOAD_LEN;

/// A decoded admission mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelAdmissionMark {
    /// The channel.
    pub channel_id: [u8; CHANNEL_ADMISSION_MARK_CHANNEL_ID_LEN],
    /// The service row sequence up to which every row was checked.
    pub through_seq: u64,
}

/// Why an admission-mark envelope was refused. A reason code; nothing from
/// the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ChannelAdmissionMarkError {
    /// No domain separator. Only from [`decode_channel_admission_mark`].
    #[error("plaintext does not carry the channel admission-mark domain separator")]
    WrongDomain,
    /// The payload is not exactly the fixed length.
    #[error("channel admission-mark envelope has the wrong length")]
    BadLength,
    /// The version byte is not [`CHANNEL_ADMISSION_MARK_VERSION_V1`].
    #[error("channel admission-mark envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
}

impl ChannelAdmissionMarkError {
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

/// Compose the envelope a publishing device sends after advancing its mark.
#[must_use]
pub fn compose_channel_admission_mark(mark: &ChannelAdmissionMark) -> Vec<u8> {
    let mut out = Vec::with_capacity(CHANNEL_ADMISSION_MARK_ENVELOPE_LEN);
    out.extend_from_slice(CHANNEL_ADMISSION_MARK_ENVELOPE_DOMAIN_V1);
    out.push(CHANNEL_ADMISSION_MARK_VERSION_V1);
    out.extend_from_slice(&mark.channel_id);
    out.extend_from_slice(&mark.through_seq.to_be_bytes());
    out
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher:
/// `Ok(None)` when the prefix does not match.
///
/// # Errors
///
/// [`ChannelAdmissionMarkError::UnknownVersion`] (checked before the length)
/// and [`ChannelAdmissionMarkError::BadLength`].
pub fn try_unwrap_channel_admission_mark(
    plaintext: &[u8],
) -> CoreResult<Option<ChannelAdmissionMark>, ChannelAdmissionMarkError> {
    let Some(payload) = plaintext.strip_prefix(CHANNEL_ADMISSION_MARK_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let (&version, rest) = payload
        .split_first()
        .ok_or(ChannelAdmissionMarkError::BadLength)?;
    if version != CHANNEL_ADMISSION_MARK_VERSION_V1 {
        return Err(ChannelAdmissionMarkError::UnknownVersion(version));
    }
    if rest.len() != CHANNEL_ADMISSION_MARK_CHANNEL_ID_LEN + 8 {
        return Err(ChannelAdmissionMarkError::BadLength);
    }
    let (channel, seq) = rest.split_at(CHANNEL_ADMISSION_MARK_CHANNEL_ID_LEN);
    let mut channel_id = [0u8; CHANNEL_ADMISSION_MARK_CHANNEL_ID_LEN];
    channel_id.copy_from_slice(channel);
    let seq: [u8; 8] = seq
        .try_into()
        .map_err(|_| ChannelAdmissionMarkError::BadLength)?;
    Ok(Some(ChannelAdmissionMark {
        channel_id,
        through_seq: u64::from_be_bytes(seq),
    }))
}

/// Strict decoder: a plaintext without the separator is a hard error.
///
/// # Errors
///
/// [`ChannelAdmissionMarkError::WrongDomain`] and every error of
/// [`try_unwrap_channel_admission_mark`].
pub fn decode_channel_admission_mark(
    bytes: &[u8],
) -> CoreResult<ChannelAdmissionMark, ChannelAdmissionMarkError> {
    try_unwrap_channel_admission_mark(bytes)?.ok_or(ChannelAdmissionMarkError::WrongDomain)
}

/// The fuzz oracle behind `fuzz_api::channel_admission_mark_decode`: `None`
/// when the decoder refuses; `Some(true)` when what decoded re-composes to
/// the same bytes at the fixed length.
pub(crate) fn fuzz_decode(bytes: &[u8]) -> Option<bool> {
    let mark = decode_channel_admission_mark(bytes).ok()?;
    Some(
        compose_channel_admission_mark(&mark) == bytes
            && bytes.len() == CHANNEL_ADMISSION_MARK_ENVELOPE_LEN,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const MARK: ChannelAdmissionMark = ChannelAdmissionMark {
        channel_id: [0x0C; 16],
        through_seq: 0x0102_0304_0506_0708,
    };

    #[test]
    fn compose_decode_round_trips_at_a_fixed_length() {
        let bytes = compose_channel_admission_mark(&MARK);
        assert_eq!(CHANNEL_ADMISSION_MARK_ENVELOPE_DOMAIN_V1.len(), 32);
        assert_eq!(bytes.len(), CHANNEL_ADMISSION_MARK_ENVELOPE_LEN);
        assert_eq!(CHANNEL_ADMISSION_MARK_ENVELOPE_LEN, 57);
        assert_eq!(&bytes[49..], &MARK.through_seq.to_be_bytes());
        assert_eq!(decode_channel_admission_mark(&bytes), Ok(MARK));
        assert_eq!(fuzz_decode(&bytes), Some(true));
        assert!(matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(&bytes),
            Ok(crate::incoming_envelope::IncomingEnvelope::ChannelAdmissionMark(m)) if m == MARK
        ));
    }

    #[test]
    fn version_and_length_are_checked() {
        let good = compose_channel_admission_mark(&MARK);
        let domain = CHANNEL_ADMISSION_MARK_ENVELOPE_DOMAIN_V1.len();
        assert_eq!(
            try_unwrap_channel_admission_mark(&good[..good.len() - 1]),
            Err(ChannelAdmissionMarkError::BadLength)
        );
        let mut long = good.clone();
        long.push(0);
        assert_eq!(
            try_unwrap_channel_admission_mark(&long),
            Err(ChannelAdmissionMarkError::BadLength)
        );
        let mut v2 = good;
        v2[domain] = 0x02;
        assert_eq!(
            try_unwrap_channel_admission_mark(&v2),
            Err(ChannelAdmissionMarkError::UnknownVersion(0x02))
        );
        assert_eq!(
            try_unwrap_channel_admission_mark(CHANNEL_ADMISSION_MARK_ENVELOPE_DOMAIN_V1),
            Err(ChannelAdmissionMarkError::BadLength)
        );
        assert_eq!(try_unwrap_channel_admission_mark(b"hello"), Ok(None));
        assert_eq!(
            decode_channel_admission_mark(b"hello"),
            Err(ChannelAdmissionMarkError::WrongDomain)
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn any_mark_round_trips(channel in any::<[u8; 16]>(), seq in any::<u64>()) {
            let mark = ChannelAdmissionMark { channel_id: channel, through_seq: seq };
            prop_assert_eq!(decode_channel_admission_mark(&compose_channel_admission_mark(&mark)), Ok(mark));
        }

        #[test]
        fn arbitrary_bytes_behind_the_domain_never_panic(tail in proptest::collection::vec(any::<u8>(), 0..64)) {
            let mut bytes = CHANNEL_ADMISSION_MARK_ENVELOPE_DOMAIN_V1.to_vec();
            bytes.extend_from_slice(&tail);
            if let Some(ok) = fuzz_decode(&bytes) {
                prop_assert!(ok);
            }
        }
    }
}
