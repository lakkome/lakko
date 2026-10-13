// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The recovery-email envelope ([ADR-0075](../../../../docs/adr/0075-recovery-email.md),
//! amended 2026-10-13): how the account's recovery email register travels
//! between the account's own devices.
//!
//! ## Why an envelope and not a `StateSnapshot` field
//!
//! The same reason the lineage register has one ([`super::lineage`]): the
//! DeviceGroup `StateSnapshot` codec lives in `lakko-mls`, and the opaque
//! `ServiceEnvelope` carrier is length-bounded there and never interpreted,
//! so the register changes nothing in `lakko-mls` or in
//! `devicegroup_v1.proto`. The fold, the persistence codec and therefore the
//! backup payload carry the register like every other one; only the
//! device-to-device wire differs.
//!
//! ## Wire
//!
//! ```text
//! domain(29) "lakko/sync/recovery-email/v1\0"
//! version(1) = 0x01
//! hlc(8, BE)              the register's ORIGINAL stamp
//! address_len(1)          0 (no recovery email) or 3..=254
//! address                 UTF-8, the normalised address
//! ```
//!
//! Nothing may trail. The encoding is canonical: whatever decodes re-encodes
//! to the identical bytes. Every violation refuses the whole envelope.
//!
//! One envelope serves both uses: a live change, and the catch-up re-send
//! after the DeviceGroup grew, which carries the held register with its
//! original stamp, so a re-send can never beat a later write. A removal is
//! an envelope like any other (an empty address), so it reaches a device
//! that was offline and outranks the address that device still holds.
//!
//! ## Trust
//!
//! The bytes arrive from another device of the same account, inside an MLS
//! application message of the DeviceGroup, or from a backup. They are
//! authenticated as "one of my devices" and nothing more; the decoder is
//! bounded and total on arbitrary input. The address is personal data: no
//! error here carries it, and it must never be logged.

use thiserror::Error;

use super::entry::{
    take_n, take_u64, take_u8, BoundsError, RecoveryEmailEntry, Stamped, RECOVERY_EMAIL_MAX_LEN,
};

/// Domain separator of the recovery-email envelope, trailing NUL included.
/// Frozen.
pub const RECOVERY_EMAIL_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/sync/recovery-email/v1\0";

/// The version byte this build writes and accepts.
pub const RECOVERY_EMAIL_ENVELOPE_VERSION_V1: u8 = 0x01;

/// Shortest valid envelope: the header and an empty address (a removal).
pub const RECOVERY_EMAIL_ENVELOPE_MIN_LEN: usize =
    RECOVERY_EMAIL_ENVELOPE_DOMAIN_V1.len() + 1 + 8 + 1;

/// Longest valid envelope: the header and the longest address. Far below
/// the DeviceGroup service-envelope cap.
pub const RECOVERY_EMAIL_ENVELOPE_MAX_LEN: usize =
    RECOVERY_EMAIL_ENVELOPE_MIN_LEN + RECOVERY_EMAIL_MAX_LEN;

/// Why a recovery-email envelope was refused. A reason, never the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RecoveryEmailEnvelopeError {
    /// The bytes do not open with [`RECOVERY_EMAIL_ENVELOPE_DOMAIN_V1`].
    #[error("not a recovery-email envelope")]
    WrongDomain,
    /// The version byte is not [`RECOVERY_EMAIL_ENVELOPE_VERSION_V1`].
    #[error("unknown recovery-email envelope version")]
    UnknownVersion,
    /// The bytes ended early.
    #[error("recovery-email envelope is truncated")]
    Truncated,
    /// Bytes remained after the address.
    #[error("trailing bytes in recovery-email envelope")]
    Trailing,
    /// The address is not UTF-8.
    #[error("recovery-email envelope address is not UTF-8")]
    NotUtf8,
    /// The address broke a register bound.
    #[error("recovery email out of bounds: {0}")]
    Bounds(#[from] BoundsError),
}

impl RecoveryEmailEnvelopeError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::UnknownVersion => "unknownVersion",
            Self::Truncated => "truncated",
            Self::Trailing => "trailing",
            Self::NotUtf8 => "notUtf8",
            Self::Bounds(_) => "bounds",
        }
    }
}

/// `true` when `bytes` open with the recovery-email envelope domain: the
/// cheap routing question a client asks of a DeviceGroup service envelope
/// before handing it to a decoder. Says nothing about validity.
#[must_use]
pub fn is_recovery_email_envelope(bytes: &[u8]) -> bool {
    bytes.starts_with(RECOVERY_EMAIL_ENVELOPE_DOMAIN_V1)
}

/// Compose the envelope of one stamped register.
///
/// # Errors
///
/// [`RecoveryEmailEnvelopeError::Bounds`] when the address is neither empty
/// nor a normalised address of 3..=254 bytes.
pub fn compose_recovery_email_envelope(
    register: &Stamped<RecoveryEmailEntry>,
) -> Result<Vec<u8>, RecoveryEmailEnvelopeError> {
    register.value.validate()?;
    let address = register.value.address.as_bytes();
    let len = u8::try_from(address.len()).map_err(|_| BoundsError::RecoveryEmailLength)?;
    let mut out = Vec::with_capacity(RECOVERY_EMAIL_ENVELOPE_MIN_LEN + address.len());
    out.extend_from_slice(RECOVERY_EMAIL_ENVELOPE_DOMAIN_V1);
    out.push(RECOVERY_EMAIL_ENVELOPE_VERSION_V1);
    out.extend_from_slice(&register.hlc.to_be_bytes());
    out.push(len);
    out.extend_from_slice(address);
    Ok(out)
}

/// Decode one envelope, fail-closed: the whole envelope or nothing.
///
/// # Errors
///
/// Every [`RecoveryEmailEnvelopeError`]; the domain is checked first, then
/// the version, then the stamp, the length against its bound (before any
/// copy), the bytes, trailing bytes, UTF-8 and the address shape.
pub fn decode_recovery_email_envelope(
    bytes: &[u8],
) -> Result<Stamped<RecoveryEmailEntry>, RecoveryEmailEnvelopeError> {
    let mut cur = bytes
        .strip_prefix(RECOVERY_EMAIL_ENVELOPE_DOMAIN_V1)
        .ok_or(RecoveryEmailEnvelopeError::WrongDomain)?;
    let version = take_u8(&mut cur).map_err(|_| RecoveryEmailEnvelopeError::Truncated)?;
    if version != RECOVERY_EMAIL_ENVELOPE_VERSION_V1 {
        return Err(RecoveryEmailEnvelopeError::UnknownVersion);
    }
    let hlc = take_u64(&mut cur).map_err(|_| RecoveryEmailEnvelopeError::Truncated)?;
    let len = usize::from(take_u8(&mut cur).map_err(|_| RecoveryEmailEnvelopeError::Truncated)?);
    if len > RECOVERY_EMAIL_MAX_LEN {
        return Err(BoundsError::RecoveryEmailLength.into());
    }
    let raw = take_n(&mut cur, len).map_err(|_| RecoveryEmailEnvelopeError::Truncated)?;
    if !cur.is_empty() {
        return Err(RecoveryEmailEnvelopeError::Trailing);
    }
    let address =
        String::from_utf8(raw.to_vec()).map_err(|_| RecoveryEmailEnvelopeError::NotUtf8)?;
    let value = RecoveryEmailEntry { address };
    value.validate()?;
    Ok(Stamped { hlc, value })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(hlc: u64, address: &str) -> Stamped<RecoveryEmailEntry> {
        Stamped {
            hlc,
            value: RecoveryEmailEntry {
                address: address.to_owned(),
            },
        }
    }

    #[test]
    fn the_domain_and_the_layout_are_pinned() {
        assert_eq!(RECOVERY_EMAIL_ENVELOPE_DOMAIN_V1.len(), 29);
        assert_eq!(RECOVERY_EMAIL_ENVELOPE_DOMAIN_V1.last(), Some(&0));
        let wire = compose_recovery_email_envelope(&r(0x0102, "a@b")).unwrap();
        let mut expected = b"lakko/sync/recovery-email/v1\0".to_vec();
        expected.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 1, 2, 3, b'a', b'@', b'b']);
        assert_eq!(wire, expected);
        assert_eq!(RECOVERY_EMAIL_ENVELOPE_MIN_LEN, 39);
        assert_eq!(RECOVERY_EMAIL_ENVELOPE_MAX_LEN, 293);
        assert!(is_recovery_email_envelope(&wire));
        assert!(!is_recovery_email_envelope(b"lakko/sync/recovery-email/v1"));
        assert!(!is_recovery_email_envelope(b"lakko/sync/lineage/v1\0"));
        assert!(!super::super::lineage::is_lineage_envelope(&wire));
    }

    #[test]
    fn an_address_and_a_removal_round_trip() {
        let longest = format!("{}@b", "a".repeat(RECOVERY_EMAIL_MAX_LEN - 2));
        for register in [r(7, "änna@büro.example"), r(u64::MAX, ""), r(0, &longest)] {
            let wire = compose_recovery_email_envelope(&register).unwrap();
            assert!(wire.len() >= RECOVERY_EMAIL_ENVELOPE_MIN_LEN);
            assert!(wire.len() <= RECOVERY_EMAIL_ENVELOPE_MAX_LEN);
            assert_eq!(decode_recovery_email_envelope(&wire), Ok(register));
        }
        let removal = compose_recovery_email_envelope(&r(9, "")).unwrap();
        assert_eq!(removal.len(), RECOVERY_EMAIL_ENVELOPE_MIN_LEN);
    }

    #[test]
    fn compose_refuses_what_decode_refuses() {
        assert_eq!(
            compose_recovery_email_envelope(&r(1, "ab")),
            Err(RecoveryEmailEnvelopeError::Bounds(
                BoundsError::RecoveryEmailLength
            ))
        );
        assert_eq!(
            compose_recovery_email_envelope(&r(1, &format!("{}@b", "a".repeat(253)))),
            Err(RecoveryEmailEnvelopeError::Bounds(
                BoundsError::RecoveryEmailLength
            ))
        );
        assert_eq!(
            compose_recovery_email_envelope(&r(1, "a b@c")),
            Err(RecoveryEmailEnvelopeError::Bounds(
                BoundsError::RecoveryEmailShape
            ))
        );
    }

    #[test]
    fn decode_is_fail_closed() {
        let good = compose_recovery_email_envelope(&r(1, "ab@cd")).unwrap();
        for len in 0..good.len() {
            let err = decode_recovery_email_envelope(&good[..len]).err();
            assert!(
                matches!(
                    err,
                    Some(
                        RecoveryEmailEnvelopeError::WrongDomain
                            | RecoveryEmailEnvelopeError::Truncated
                    )
                ),
                "prefix of {len} bytes gave {err:?}"
            );
        }
        let mut trailing = good.clone();
        trailing.push(0);
        assert_eq!(
            decode_recovery_email_envelope(&trailing),
            Err(RecoveryEmailEnvelopeError::Trailing)
        );

        let header = RECOVERY_EMAIL_ENVELOPE_DOMAIN_V1.len();
        let mut version = good.clone();
        version[header] = 2;
        assert_eq!(
            decode_recovery_email_envelope(&version),
            Err(RecoveryEmailEnvelopeError::UnknownVersion)
        );
        // The version is judged before everything after it.
        assert_eq!(
            decode_recovery_email_envelope(&version[..=header]),
            Err(RecoveryEmailEnvelopeError::UnknownVersion)
        );

        // A length above the bound is refused before the bytes are read.
        let mut huge = good[..header + 9].to_vec();
        huge.push(0xFF);
        assert_eq!(
            decode_recovery_email_envelope(&huge),
            Err(RecoveryEmailEnvelopeError::Bounds(
                BoundsError::RecoveryEmailLength
            ))
        );
        // One and two bytes are below the shortest address.
        let mut short = good[..header + 9].to_vec();
        short.extend_from_slice(&[2, b'a', b'@']);
        assert_eq!(
            decode_recovery_email_envelope(&short),
            Err(RecoveryEmailEnvelopeError::Bounds(
                BoundsError::RecoveryEmailLength
            ))
        );
        let mut not_utf8 = good[..header + 9].to_vec();
        not_utf8.extend_from_slice(&[3, b'a', 0xFF, b'b']);
        assert_eq!(
            decode_recovery_email_envelope(&not_utf8),
            Err(RecoveryEmailEnvelopeError::NotUtf8)
        );
        let mut shape = good[..header + 9].to_vec();
        shape.extend_from_slice(&[3, b'a', b'\n', b'b']);
        assert_eq!(
            decode_recovery_email_envelope(&shape),
            Err(RecoveryEmailEnvelopeError::Bounds(
                BoundsError::RecoveryEmailShape
            ))
        );
        assert_eq!(
            decode_recovery_email_envelope(b"lakko/sync/recovery-email/v2\0\x01"),
            Err(RecoveryEmailEnvelopeError::WrongDomain)
        );
    }
}
