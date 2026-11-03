// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The account's quick reaction (UX `docs/ux/reactions-and-emoji.md` section
//! 6, decision D3): the emoji a double tap sends, chosen in Settings >
//! Appearance and followed to the account's other devices, as Telegram's
//! `reactions_default` is.
//!
//! One register per account, last-writer-wins like the own-username register
//! ([`super::own_username`]): the value is the chosen emoji. "Never chosen" is
//! the absence of the register; the client then uses
//! `lakko_ui_rules::reaction::DEFAULT_QUICK_REACTION`. There is no "unset"
//! value, because nothing in the UX takes a choice back: picking the default
//! again is an ordinary write.
//!
//! ## Why an envelope and not a `StateSnapshot` field
//!
//! The reason the other account registers have one: the DeviceGroup
//! `StateSnapshot` codec lives in `lakko-mls`, and the opaque
//! `ServiceEnvelope` carrier is length-bounded there and never interpreted.
//! The fold and the persistence codec (so the backup) carry the register like
//! every other one.
//!
//! ## Wire
//!
//! ```text
//! domain(29) "lakko/sync/quick-reaction/v1\0"
//! version(1) = 0x01
//! hlc(8, BE)              the register's ORIGINAL stamp
//! emoji_len(1)            1..=QUICK_REACTION_MAX_BYTES
//! emoji                   UTF-8
//! ```
//!
//! Nothing may trail. The encoding is canonical. Every violation refuses the
//! whole envelope. One envelope serves both a live change and the catch-up
//! re-send after the DeviceGroup grew, which carries the original stamp.
//!
//! ## Trust
//!
//! The bytes come from another device of the same account (an MLS
//! application message of the DeviceGroup) or from a backup. The decoder is
//! bounded and total on arbitrary input. It checks the wire bound every
//! reaction parser checks (`lakko_contacts::allowed_reactions`); whether the
//! value is ONE emoji is the client's display rule (`lakko_ui_rules::emoji`),
//! which this crate cannot reach without a dependency cycle, so a client
//! reading a value that is not one emoji falls back to the default.

use thiserror::Error;

use lakko_contacts::allowed_reactions::{is_reaction_emoji_bytes, REACTION_EMOJI_MAX_BYTES};

use super::entry::{take_n, take_u32, take_u64, take_u8, BoundsError, CodecError, Stamped};

/// Domain separator of the quick-reaction envelope, trailing NUL included.
/// Frozen.
pub const QUICK_REACTION_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/sync/quick-reaction/v1\0";

/// The version byte this build writes and accepts.
pub const QUICK_REACTION_ENVELOPE_VERSION_V1: u8 = 0x01;

/// Longest quick reaction, in bytes: the reaction wire bound.
pub const QUICK_REACTION_MAX_BYTES: usize = REACTION_EMOJI_MAX_BYTES;

/// Shortest valid envelope: the header and a one-byte emoji.
pub const QUICK_REACTION_ENVELOPE_MIN_LEN: usize =
    QUICK_REACTION_ENVELOPE_DOMAIN_V1.len() + 1 + 8 + 1 + 1;

/// Longest valid envelope: the header and the longest emoji.
pub const QUICK_REACTION_ENVELOPE_MAX_LEN: usize =
    QUICK_REACTION_ENVELOPE_MIN_LEN - 1 + QUICK_REACTION_MAX_BYTES;

/// The account's quick reaction: one register per account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickReactionEntry {
    /// The chosen emoji: 1 to [`QUICK_REACTION_MAX_BYTES`] bytes.
    pub emoji: String,
}

impl QuickReactionEntry {
    /// The wire bound of the held value.
    ///
    /// # Errors
    ///
    /// [`BoundsError::QuickReaction`] for an empty or over-long value.
    pub fn validate(&self) -> Result<(), BoundsError> {
        if is_reaction_emoji_bytes(self.emoji.as_str()) {
            Ok(())
        } else {
            Err(BoundsError::QuickReaction)
        }
    }

    /// The persistence body after the tag: a u32 length, then the bytes.
    pub(crate) fn encode_body(&self, out: &mut Vec<u8>) {
        let b = self.emoji.as_bytes();
        // A validated value is at most 64 bytes; saturating keeps it total.
        out.extend_from_slice(&u32::try_from(b.len()).unwrap_or(u32::MAX).to_be_bytes());
        out.extend_from_slice(b);
    }

    /// Read the persistence body back. The length is checked against the
    /// bound BEFORE the bytes are taken, so a hostile length never sizes a
    /// copy.
    pub(crate) fn take_body(cur: &mut &[u8]) -> Result<Self, CodecError> {
        let n = take_u32(cur)? as usize;
        if n > QUICK_REACTION_MAX_BYTES {
            return Err(BoundsError::QuickReaction.into());
        }
        let emoji = String::from_utf8(take_n(cur, n)?.to_vec()).map_err(|_| CodecError::NotUtf8)?;
        Ok(Self { emoji })
    }
}

/// Why a quick-reaction envelope was refused. A reason, never the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum QuickReactionEnvelopeError {
    /// The bytes do not open with [`QUICK_REACTION_ENVELOPE_DOMAIN_V1`].
    #[error("not a quick-reaction envelope")]
    WrongDomain,
    /// The version byte is not [`QUICK_REACTION_ENVELOPE_VERSION_V1`].
    #[error("unknown quick-reaction envelope version")]
    UnknownVersion,
    /// The bytes ended early.
    #[error("quick-reaction envelope is truncated")]
    Truncated,
    /// Bytes remained after the emoji.
    #[error("trailing bytes in quick-reaction envelope")]
    Trailing,
    /// The emoji is not UTF-8.
    #[error("quick-reaction envelope emoji is not UTF-8")]
    NotUtf8,
    /// The emoji broke the register bound.
    #[error("quick reaction out of bounds: {0}")]
    Bounds(#[from] BoundsError),
}

impl QuickReactionEnvelopeError {
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

/// `true` when `bytes` open with the quick-reaction envelope domain: the
/// cheap routing question. Says nothing about validity.
#[must_use]
pub fn is_quick_reaction_envelope(bytes: &[u8]) -> bool {
    bytes.starts_with(QUICK_REACTION_ENVELOPE_DOMAIN_V1)
}

/// Compose the envelope of one stamped register.
///
/// # Errors
///
/// [`QuickReactionEnvelopeError::Bounds`] for an empty or over-long value.
pub fn compose_quick_reaction_envelope(
    register: &Stamped<QuickReactionEntry>,
) -> Result<Vec<u8>, QuickReactionEnvelopeError> {
    register.value.validate()?;
    let emoji = register.value.emoji.as_bytes();
    let len = u8::try_from(emoji.len()).map_err(|_| BoundsError::QuickReaction)?;
    let mut out = Vec::with_capacity(QUICK_REACTION_ENVELOPE_MIN_LEN - 1 + emoji.len());
    out.extend_from_slice(QUICK_REACTION_ENVELOPE_DOMAIN_V1);
    out.push(QUICK_REACTION_ENVELOPE_VERSION_V1);
    out.extend_from_slice(&register.hlc.to_be_bytes());
    out.push(len);
    out.extend_from_slice(emoji);
    Ok(out)
}

/// Decode one envelope, fail-closed: the whole envelope or nothing.
///
/// # Errors
///
/// Every [`QuickReactionEnvelopeError`]; the domain is checked first, then
/// the version, then the stamp, the length against its bound (before any
/// copy), the bytes, trailing bytes, UTF-8.
pub fn decode_quick_reaction_envelope(
    bytes: &[u8],
) -> Result<Stamped<QuickReactionEntry>, QuickReactionEnvelopeError> {
    let mut cur = bytes
        .strip_prefix(QUICK_REACTION_ENVELOPE_DOMAIN_V1)
        .ok_or(QuickReactionEnvelopeError::WrongDomain)?;
    let version = take_u8(&mut cur).map_err(|_| QuickReactionEnvelopeError::Truncated)?;
    if version != QUICK_REACTION_ENVELOPE_VERSION_V1 {
        return Err(QuickReactionEnvelopeError::UnknownVersion);
    }
    let hlc = take_u64(&mut cur).map_err(|_| QuickReactionEnvelopeError::Truncated)?;
    let len = usize::from(take_u8(&mut cur).map_err(|_| QuickReactionEnvelopeError::Truncated)?);
    if len == 0 || len > QUICK_REACTION_MAX_BYTES {
        return Err(BoundsError::QuickReaction.into());
    }
    let raw = take_n(&mut cur, len).map_err(|_| QuickReactionEnvelopeError::Truncated)?;
    if !cur.is_empty() {
        return Err(QuickReactionEnvelopeError::Trailing);
    }
    let emoji = String::from_utf8(raw.to_vec()).map_err(|_| QuickReactionEnvelopeError::NotUtf8)?;
    let value = QuickReactionEntry { emoji };
    value.validate()?;
    Ok(Stamped { hlc, value })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(hlc: u64, emoji: &str) -> Stamped<QuickReactionEntry> {
        Stamped {
            hlc,
            value: QuickReactionEntry {
                emoji: emoji.to_owned(),
            },
        }
    }

    #[test]
    fn the_domain_and_the_layout_are_pinned() {
        assert_eq!(QUICK_REACTION_ENVELOPE_DOMAIN_V1.len(), 29);
        assert_eq!(QUICK_REACTION_ENVELOPE_DOMAIN_V1.last(), Some(&0));
        let wire = compose_quick_reaction_envelope(&r(0x0102, "\u{1F525}")).unwrap();
        let mut expected = b"lakko/sync/quick-reaction/v1\0".to_vec();
        expected.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 1, 2, 4]);
        expected.extend_from_slice("\u{1F525}".as_bytes());
        assert_eq!(wire, expected);
        assert_eq!(QUICK_REACTION_ENVELOPE_MIN_LEN, 40);
        assert_eq!(QUICK_REACTION_ENVELOPE_MAX_LEN, 103);
        assert!(is_quick_reaction_envelope(&wire));
        assert!(!super::super::own_username::is_own_username_envelope(&wire));
        assert!(!super::super::recovery_email::is_recovery_email_envelope(
            &wire
        ));
        assert!(!is_quick_reaction_envelope(
            super::super::own_username::OWN_USERNAME_ENVELOPE_DOMAIN_V1
        ));
    }

    #[test]
    fn values_round_trip() {
        let longest = "x".repeat(QUICK_REACTION_MAX_BYTES);
        for register in [
            r(7, "\u{1F44D}"),
            r(u64::MAX, "\u{2764}\u{FE0F}"),
            r(0, &longest),
        ] {
            let wire = compose_quick_reaction_envelope(&register).unwrap();
            assert!(wire.len() >= QUICK_REACTION_ENVELOPE_MIN_LEN);
            assert!(wire.len() <= QUICK_REACTION_ENVELOPE_MAX_LEN);
            assert_eq!(decode_quick_reaction_envelope(&wire), Ok(register));
        }
    }

    #[test]
    fn compose_refuses_what_decode_refuses() {
        for bad in ["", &"x".repeat(65)] {
            assert_eq!(
                compose_quick_reaction_envelope(&r(1, bad)),
                Err(QuickReactionEnvelopeError::Bounds(
                    BoundsError::QuickReaction
                )),
                "{bad:?}"
            );
        }
        let header = QUICK_REACTION_ENVELOPE_DOMAIN_V1.len() + 9;
        let good = compose_quick_reaction_envelope(&r(1, "x")).unwrap();
        let mut empty = good[..header].to_vec();
        empty.push(0);
        assert_eq!(
            decode_quick_reaction_envelope(&empty),
            Err(QuickReactionEnvelopeError::Bounds(
                BoundsError::QuickReaction
            ))
        );
        let mut huge = good[..header].to_vec();
        huge.push(65);
        assert_eq!(
            decode_quick_reaction_envelope(&huge),
            Err(QuickReactionEnvelopeError::Bounds(
                BoundsError::QuickReaction
            ))
        );
    }

    #[test]
    fn decode_is_fail_closed() {
        let good = compose_quick_reaction_envelope(&r(1, "\u{1F44D}")).unwrap();
        for len in 0..good.len() {
            let err = decode_quick_reaction_envelope(&good[..len]).err();
            assert!(
                matches!(
                    err,
                    Some(
                        QuickReactionEnvelopeError::WrongDomain
                            | QuickReactionEnvelopeError::Truncated
                    )
                ),
                "prefix of {len} bytes gave {err:?}"
            );
        }
        let mut trailing = good.clone();
        trailing.push(0);
        assert_eq!(
            decode_quick_reaction_envelope(&trailing),
            Err(QuickReactionEnvelopeError::Trailing)
        );
        let header = QUICK_REACTION_ENVELOPE_DOMAIN_V1.len();
        let mut version = good.clone();
        version[header] = 2;
        assert_eq!(
            decode_quick_reaction_envelope(&version[..=header]),
            Err(QuickReactionEnvelopeError::UnknownVersion)
        );
        let mut not_utf8 = good[..header + 9].to_vec();
        not_utf8.extend_from_slice(&[2, 0xFF, 0xFE]);
        assert_eq!(
            decode_quick_reaction_envelope(&not_utf8),
            Err(QuickReactionEnvelopeError::NotUtf8)
        );
    }

    #[test]
    fn the_body_codec_checks_the_length_before_the_copy() {
        let mut out = Vec::new();
        QuickReactionEntry {
            emoji: "\u{1F44D}".to_owned(),
        }
        .encode_body(&mut out);
        let mut cur = out.as_slice();
        assert_eq!(
            QuickReactionEntry::take_body(&mut cur),
            Ok(QuickReactionEntry {
                emoji: "\u{1F44D}".to_owned()
            })
        );
        assert!(cur.is_empty());
        let huge = 65_u32.to_be_bytes();
        let mut cur = &huge[..];
        assert_eq!(
            QuickReactionEntry::take_body(&mut cur),
            Err(CodecError::from(BoundsError::QuickReaction))
        );
    }

    #[test]
    fn codes_are_frozen() {
        assert_eq!(
            QuickReactionEnvelopeError::WrongDomain.code(),
            "wrongDomain"
        );
        assert_eq!(
            QuickReactionEnvelopeError::Bounds(BoundsError::QuickReaction).code(),
            "bounds"
        );
    }
}
