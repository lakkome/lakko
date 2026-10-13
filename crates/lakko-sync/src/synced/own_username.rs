// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The own-username envelope ([ADR-0076](../../../../docs/adr/0076-username-change.md)
//! section 3): how the account's own username register travels between the
//! account's own devices.
//!
//! ## Why an envelope and not a `StateSnapshot` field
//!
//! The same reason the lineage and recovery-email registers have one
//! ([`super::lineage`], [`super::recovery_email`]): the DeviceGroup
//! `StateSnapshot` codec lives in `lakko-mls`, and the opaque
//! `ServiceEnvelope` carrier is length-bounded there and never interpreted.
//! The fold, the persistence codec and therefore the backup payload carry
//! the register like every other one; only the device-to-device wire
//! differs.
//!
//! ## Wire
//!
//! ```text
//! domain(23) "lakko/sync/username/v1\0"
//! version(1) = 0x01
//! hlc(8, BE)              the register's ORIGINAL stamp
//! username_len(1)         0 (no username) or 3..=32
//! username                the canonical username, ASCII [a-z0-9_]
//! ```
//!
//! Nothing may trail. The encoding is canonical: whatever decodes re-encodes
//! to the identical bytes. Every violation refuses the whole envelope.
//!
//! One envelope serves both uses: a live change (a rename), and the catch-up
//! re-send after the DeviceGroup grew, which carries the held register with
//! its original stamp, so a re-send can never beat a later write. "No
//! username" is an envelope like any other (an empty value), so it reaches a
//! device that was offline and outranks the name that device still holds.
//!
//! ## Trust
//!
//! The bytes arrive from another device of the same account, inside an MLS
//! application message of the DeviceGroup, or from a backup. They are
//! authenticated as "one of my devices" and nothing more; the decoder is
//! bounded and total on arbitrary input. The register says what this
//! account calls itself; whether discovery agrees is a separate question the
//! client settles with the server.

use thiserror::Error;

use super::entry::{
    take_n, take_u64, take_u8, BoundsError, OwnUsernameEntry, Stamped, OWN_USERNAME_MAX_LEN,
};

/// Domain separator of the own-username envelope, trailing NUL included.
/// Frozen.
pub const OWN_USERNAME_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/sync/username/v1\0";

/// The version byte this build writes and accepts.
pub const OWN_USERNAME_ENVELOPE_VERSION_V1: u8 = 0x01;

/// Shortest valid envelope: the header and an empty value (no username).
pub const OWN_USERNAME_ENVELOPE_MIN_LEN: usize = OWN_USERNAME_ENVELOPE_DOMAIN_V1.len() + 1 + 8 + 1;

/// Longest valid envelope: the header and the longest username. Far below
/// the DeviceGroup service-envelope cap.
pub const OWN_USERNAME_ENVELOPE_MAX_LEN: usize =
    OWN_USERNAME_ENVELOPE_MIN_LEN + OWN_USERNAME_MAX_LEN;

/// Why an own-username envelope was refused. A reason, never the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum OwnUsernameEnvelopeError {
    /// The bytes do not open with [`OWN_USERNAME_ENVELOPE_DOMAIN_V1`].
    #[error("not an own-username envelope")]
    WrongDomain,
    /// The version byte is not [`OWN_USERNAME_ENVELOPE_VERSION_V1`].
    #[error("unknown own-username envelope version")]
    UnknownVersion,
    /// The bytes ended early.
    #[error("own-username envelope is truncated")]
    Truncated,
    /// Bytes remained after the username.
    #[error("trailing bytes in own-username envelope")]
    Trailing,
    /// The username is not UTF-8.
    #[error("own-username envelope username is not UTF-8")]
    NotUtf8,
    /// The username broke the register bound.
    #[error("own username out of bounds: {0}")]
    Bounds(#[from] BoundsError),
}

impl OwnUsernameEnvelopeError {
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

/// `true` when `bytes` open with the own-username envelope domain: the
/// cheap routing question a client asks of a DeviceGroup service envelope
/// before handing it to a decoder. Says nothing about validity.
#[must_use]
pub fn is_own_username_envelope(bytes: &[u8]) -> bool {
    bytes.starts_with(OWN_USERNAME_ENVELOPE_DOMAIN_V1)
}

/// Compose the envelope of one stamped register.
///
/// # Errors
///
/// [`OwnUsernameEnvelopeError::Bounds`] when the value is neither empty nor
/// a canonical username.
pub fn compose_own_username_envelope(
    register: &Stamped<OwnUsernameEntry>,
) -> Result<Vec<u8>, OwnUsernameEnvelopeError> {
    register.value.validate()?;
    let username = register.value.username.as_bytes();
    let len = u8::try_from(username.len()).map_err(|_| BoundsError::OwnUsername)?;
    let mut out = Vec::with_capacity(OWN_USERNAME_ENVELOPE_MIN_LEN + username.len());
    out.extend_from_slice(OWN_USERNAME_ENVELOPE_DOMAIN_V1);
    out.push(OWN_USERNAME_ENVELOPE_VERSION_V1);
    out.extend_from_slice(&register.hlc.to_be_bytes());
    out.push(len);
    out.extend_from_slice(username);
    Ok(out)
}

/// Decode one envelope, fail-closed: the whole envelope or nothing.
///
/// # Errors
///
/// Every [`OwnUsernameEnvelopeError`]; the domain is checked first, then
/// the version, then the stamp, the length against its bound (before any
/// copy), the bytes, trailing bytes, UTF-8 and the username rule.
pub fn decode_own_username_envelope(
    bytes: &[u8],
) -> Result<Stamped<OwnUsernameEntry>, OwnUsernameEnvelopeError> {
    let mut cur = bytes
        .strip_prefix(OWN_USERNAME_ENVELOPE_DOMAIN_V1)
        .ok_or(OwnUsernameEnvelopeError::WrongDomain)?;
    let version = take_u8(&mut cur).map_err(|_| OwnUsernameEnvelopeError::Truncated)?;
    if version != OWN_USERNAME_ENVELOPE_VERSION_V1 {
        return Err(OwnUsernameEnvelopeError::UnknownVersion);
    }
    let hlc = take_u64(&mut cur).map_err(|_| OwnUsernameEnvelopeError::Truncated)?;
    let len = usize::from(take_u8(&mut cur).map_err(|_| OwnUsernameEnvelopeError::Truncated)?);
    if len > OWN_USERNAME_MAX_LEN {
        return Err(BoundsError::OwnUsername.into());
    }
    let raw = take_n(&mut cur, len).map_err(|_| OwnUsernameEnvelopeError::Truncated)?;
    if !cur.is_empty() {
        return Err(OwnUsernameEnvelopeError::Trailing);
    }
    let username =
        String::from_utf8(raw.to_vec()).map_err(|_| OwnUsernameEnvelopeError::NotUtf8)?;
    let value = OwnUsernameEntry { username };
    value.validate()?;
    Ok(Stamped { hlc, value })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(hlc: u64, username: &str) -> Stamped<OwnUsernameEntry> {
        Stamped {
            hlc,
            value: OwnUsernameEntry {
                username: username.to_owned(),
            },
        }
    }

    #[test]
    fn the_domain_and_the_layout_are_pinned() {
        assert_eq!(OWN_USERNAME_ENVELOPE_DOMAIN_V1.len(), 23);
        assert_eq!(OWN_USERNAME_ENVELOPE_DOMAIN_V1.last(), Some(&0));
        let wire = compose_own_username_envelope(&r(0x0102, "bob")).unwrap();
        let mut expected = b"lakko/sync/username/v1\0".to_vec();
        expected.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 1, 2, 3, b'b', b'o', b'b']);
        assert_eq!(wire, expected);
        assert_eq!(OWN_USERNAME_ENVELOPE_MIN_LEN, 33);
        assert_eq!(OWN_USERNAME_ENVELOPE_MAX_LEN, 65);
        assert!(is_own_username_envelope(&wire));
        assert!(!is_own_username_envelope(b"lakko/sync/username/v1"));
        assert!(!is_own_username_envelope(b"lakko/sync/recovery-email/v1\0"));
        assert!(!super::super::lineage::is_lineage_envelope(&wire));
        assert!(!super::super::recovery_email::is_recovery_email_envelope(
            &wire
        ));
    }

    #[test]
    fn a_username_and_no_username_round_trip() {
        let longest = "a".repeat(OWN_USERNAME_MAX_LEN);
        for register in [r(7, "bob_42"), r(u64::MAX, ""), r(0, &longest)] {
            let wire = compose_own_username_envelope(&register).unwrap();
            assert!(wire.len() >= OWN_USERNAME_ENVELOPE_MIN_LEN);
            assert!(wire.len() <= OWN_USERNAME_ENVELOPE_MAX_LEN);
            assert_eq!(decode_own_username_envelope(&wire), Ok(register));
        }
        let none = compose_own_username_envelope(&r(9, "")).unwrap();
        assert_eq!(none.len(), OWN_USERNAME_ENVELOPE_MIN_LEN);
    }

    #[test]
    fn compose_refuses_what_decode_refuses() {
        for bad in [
            "ab",
            "Bob",
            "bo b",
            "b\u{f6}b",
            &"a".repeat(33),
            &"a".repeat(300),
        ] {
            assert_eq!(
                compose_own_username_envelope(&r(1, bad)),
                Err(OwnUsernameEnvelopeError::Bounds(BoundsError::OwnUsername)),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn decode_is_fail_closed() {
        let good = compose_own_username_envelope(&r(1, "bob_42")).unwrap();
        for len in 0..good.len() {
            let err = decode_own_username_envelope(&good[..len]).err();
            assert!(
                matches!(
                    err,
                    Some(
                        OwnUsernameEnvelopeError::WrongDomain | OwnUsernameEnvelopeError::Truncated
                    )
                ),
                "prefix of {len} bytes gave {err:?}"
            );
        }
        let mut trailing = good.clone();
        trailing.push(0);
        assert_eq!(
            decode_own_username_envelope(&trailing),
            Err(OwnUsernameEnvelopeError::Trailing)
        );

        let header = OWN_USERNAME_ENVELOPE_DOMAIN_V1.len();
        let mut version = good.clone();
        version[header] = 2;
        assert_eq!(
            decode_own_username_envelope(&version),
            Err(OwnUsernameEnvelopeError::UnknownVersion)
        );
        // The version is judged before everything after it.
        assert_eq!(
            decode_own_username_envelope(&version[..=header]),
            Err(OwnUsernameEnvelopeError::UnknownVersion)
        );

        let bounds = Err(OwnUsernameEnvelopeError::Bounds(BoundsError::OwnUsername));
        // A length above the bound is refused before the bytes are read.
        let mut huge = good[..header + 9].to_vec();
        huge.push(33);
        assert_eq!(decode_own_username_envelope(&huge), bounds);
        // Below the shortest username, an upper-case letter, a space.
        for body in [
            &[2, b'a', b'b'][..],
            &[3, b'B', b'o', b'b'],
            &[3, b'b', b' ', b'b'],
        ] {
            let mut wire = good[..header + 9].to_vec();
            wire.extend_from_slice(body);
            assert_eq!(decode_own_username_envelope(&wire), bounds);
        }
        let mut not_utf8 = good[..header + 9].to_vec();
        not_utf8.extend_from_slice(&[3, b'a', 0xFF, b'b']);
        assert_eq!(
            decode_own_username_envelope(&not_utf8),
            Err(OwnUsernameEnvelopeError::NotUtf8)
        );
        assert_eq!(
            decode_own_username_envelope(b"lakko/sync/username/v2\0\x01"),
            Err(OwnUsernameEnvelopeError::WrongDomain)
        );
    }
}
