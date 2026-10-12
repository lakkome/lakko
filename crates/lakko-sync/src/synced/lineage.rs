// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The lineage envelope ([ADR-0065](../../../../docs/adr/0065-account-key-reset.md)
//! R.5): how conversation-lineage registers travel between the account's own
//! devices.
//!
//! ## Why an envelope and not a `StateSnapshot` field
//!
//! The DeviceGroup `StateSnapshot` is a protobuf message whose codec lives in
//! `lakko-mls`. Lineage registers ride the DeviceGroup's opaque
//! `ServiceEnvelope` carrier instead (the one the ADR-0060 visibility setting
//! uses), which `lakko-mls` length-bounds and never interprets, so adding the
//! register changes nothing in `lakko-mls` or in `devicegroup_v1.proto`. The
//! fold, the persistence codec and therefore the backup payload carry the
//! register like every other one; only the device-to-device wire differs.
//!
//! ## Wire
//!
//! ```text
//! domain(22) "lakko/sync/lineage/v1\0"
//! version(1) = 0x01
//! count(2, BE)            1..=LINEAGE_ENVELOPE_MAX_ENTRIES
//! count × {
//!     hlc(8, BE)          the register's ORIGINAL stamp
//!     successor_len(1)    1..=64
//!     successor
//!     root_len(1)         1..=64
//!     root
//! }
//! ```
//!
//! Canonical: entries are strictly ascending by successor id (so no successor
//! appears twice) and nothing may trail. Whatever decodes re-encodes to the
//! identical bytes. Every violation refuses the whole envelope.
//!
//! One envelope serves both uses: a live write is an envelope of one entry,
//! and the catch-up re-send after the DeviceGroup grew is every held register
//! with its original stamp, so a re-send can never beat a later write.
//!
//! ## Trust
//!
//! The bytes arrive from another device of the same account, inside an MLS
//! application message of the DeviceGroup, or from a backup. They are
//! authenticated as "one of my devices" and nothing more; the decoder is
//! bounded and total on arbitrary input.

use thiserror::Error;

use super::entry::{
    take_n, take_u64, take_u8, BoundsError, LineageEntry, Stamped, LINEAGE_GROUP_ID_MAX_LEN,
};

/// Domain separator of the lineage envelope, trailing NUL included. Frozen.
pub const LINEAGE_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/sync/lineage/v1\0";

/// The version byte this build writes and accepts.
pub const LINEAGE_ENVELOPE_VERSION_V1: u8 = 0x01;

/// The most registers one envelope carries. A state holding more is sent as
/// several envelopes ([`super::SyncedState::lineage_envelopes`]).
pub const LINEAGE_ENVELOPE_MAX_ENTRIES: usize = 1024;

/// Shortest valid envelope: the header and one entry of two one-byte ids.
pub const LINEAGE_ENVELOPE_MIN_LEN: usize = LINEAGE_ENVELOPE_DOMAIN_V1.len() + 1 + 2 + 8 + 2 + 2;

/// Longest valid envelope: the header and the most entries of the longest
/// ids. Below the DeviceGroup service-envelope cap.
pub const LINEAGE_ENVELOPE_MAX_LEN: usize = LINEAGE_ENVELOPE_DOMAIN_V1.len()
    + 1
    + 2
    + LINEAGE_ENVELOPE_MAX_ENTRIES * (8 + 2 + 2 * LINEAGE_GROUP_ID_MAX_LEN);

/// Why a lineage envelope was refused. A reason, never the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum LineageEnvelopeError {
    /// The bytes do not open with [`LINEAGE_ENVELOPE_DOMAIN_V1`].
    #[error("not a lineage envelope")]
    WrongDomain,
    /// The version byte is not [`LINEAGE_ENVELOPE_VERSION_V1`].
    #[error("unknown lineage envelope version")]
    UnknownVersion,
    /// The bytes ended early.
    #[error("lineage envelope is truncated")]
    Truncated,
    /// Bytes remained after the last entry.
    #[error("trailing bytes in lineage envelope")]
    Trailing,
    /// The entry count is zero or above [`LINEAGE_ENVELOPE_MAX_ENTRIES`].
    #[error("lineage envelope entry count is out of bounds")]
    Count,
    /// Entries are not strictly ascending by successor id.
    #[error("lineage envelope entries are not in canonical order")]
    NotCanonical,
    /// An entry broke a register bound.
    #[error("lineage entry out of bounds: {0}")]
    Bounds(#[from] BoundsError),
}

impl LineageEnvelopeError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::UnknownVersion => "unknownVersion",
            Self::Truncated => "truncated",
            Self::Trailing => "trailing",
            Self::Count => "count",
            Self::NotCanonical => "notCanonical",
            Self::Bounds(_) => "bounds",
        }
    }
}

/// `true` when `bytes` open with the lineage envelope domain: the cheap
/// routing question a client asks of a DeviceGroup service envelope before
/// handing it to a decoder. Says nothing about validity.
#[must_use]
pub fn is_lineage_envelope(bytes: &[u8]) -> bool {
    bytes.starts_with(LINEAGE_ENVELOPE_DOMAIN_V1)
}

/// Compose one envelope from stamped registers.
///
/// The entries are sorted here, so the caller's order does not matter.
///
/// # Errors
///
/// * [`LineageEnvelopeError::Count`] — none, or more than
///   [`LINEAGE_ENVELOPE_MAX_ENTRIES`].
/// * [`LineageEnvelopeError::NotCanonical`] — a successor appears twice.
/// * [`LineageEnvelopeError::Bounds`] — an entry breaks a bound.
pub fn compose_lineage_envelope(
    entries: &[Stamped<LineageEntry>],
) -> Result<Vec<u8>, LineageEnvelopeError> {
    if entries.is_empty() || entries.len() > LINEAGE_ENVELOPE_MAX_ENTRIES {
        return Err(LineageEnvelopeError::Count);
    }
    let count = u16::try_from(entries.len()).map_err(|_| LineageEnvelopeError::Count)?;
    let mut sorted: Vec<&Stamped<LineageEntry>> = entries.iter().collect();
    sorted.sort_by(|a, b| a.value.successor_group_id.cmp(&b.value.successor_group_id));
    let mut out = Vec::with_capacity(LINEAGE_ENVELOPE_DOMAIN_V1.len() + 3 + entries.len() * 74);
    out.extend_from_slice(LINEAGE_ENVELOPE_DOMAIN_V1);
    out.push(LINEAGE_ENVELOPE_VERSION_V1);
    out.extend_from_slice(&count.to_be_bytes());
    let mut previous: Option<&[u8]> = None;
    for e in sorted {
        e.value.validate()?;
        if previous == Some(e.value.successor_group_id.as_slice()) {
            return Err(LineageEnvelopeError::NotCanonical);
        }
        previous = Some(&e.value.successor_group_id);
        out.extend_from_slice(&e.hlc.to_be_bytes());
        put_id(&mut out, &e.value.successor_group_id)?;
        put_id(&mut out, &e.value.root_conversation_id)?;
    }
    Ok(out)
}

fn put_id(out: &mut Vec<u8>, id: &[u8]) -> Result<(), LineageEnvelopeError> {
    let len = u8::try_from(id.len()).map_err(|_| BoundsError::LineageIdLength)?;
    out.push(len);
    out.extend_from_slice(id);
    Ok(())
}

fn take_id(cur: &mut &[u8]) -> Result<Vec<u8>, LineageEnvelopeError> {
    let n = usize::from(take_u8(cur).map_err(|_| LineageEnvelopeError::Truncated)?);
    if n == 0 || n > LINEAGE_GROUP_ID_MAX_LEN {
        return Err(BoundsError::LineageIdLength.into());
    }
    Ok(take_n(cur, n)
        .map_err(|_| LineageEnvelopeError::Truncated)?
        .to_vec())
}

/// Decode one envelope, fail-closed: the whole envelope or nothing.
///
/// # Errors
///
/// Every [`LineageEnvelopeError`]; the domain is checked first, then the
/// version, then the count, then each entry.
pub fn decode_lineage_envelope(
    bytes: &[u8],
) -> Result<Vec<Stamped<LineageEntry>>, LineageEnvelopeError> {
    let mut cur = bytes
        .strip_prefix(LINEAGE_ENVELOPE_DOMAIN_V1)
        .ok_or(LineageEnvelopeError::WrongDomain)?;
    let version = take_u8(&mut cur).map_err(|_| LineageEnvelopeError::Truncated)?;
    if version != LINEAGE_ENVELOPE_VERSION_V1 {
        return Err(LineageEnvelopeError::UnknownVersion);
    }
    let count_bytes = take_n(&mut cur, 2).map_err(|_| LineageEnvelopeError::Truncated)?;
    let count = usize::from(u16::from_be_bytes([count_bytes[0], count_bytes[1]]));
    if count == 0 || count > LINEAGE_ENVELOPE_MAX_ENTRIES {
        return Err(LineageEnvelopeError::Count);
    }
    // Grown, not pre-sized from the count: a short hostile envelope must not
    // ask for the allocation of a full one.
    let mut out: Vec<Stamped<LineageEntry>> = Vec::new();
    for _ in 0..count {
        let hlc = take_u64(&mut cur).map_err(|_| LineageEnvelopeError::Truncated)?;
        let successor_group_id = take_id(&mut cur)?;
        let root_conversation_id = take_id(&mut cur)?;
        let value = LineageEntry {
            successor_group_id,
            root_conversation_id,
        };
        value.validate()?;
        if let Some(last) = out.last() {
            if last.value.successor_group_id >= value.successor_group_id {
                return Err(LineageEnvelopeError::NotCanonical);
            }
        }
        out.push(Stamped { hlc, value });
    }
    if !cur.is_empty() {
        return Err(LineageEnvelopeError::Trailing);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(hlc: u64, s: &[u8], r: &[u8]) -> Stamped<LineageEntry> {
        Stamped {
            hlc,
            value: LineageEntry {
                successor_group_id: s.to_vec(),
                root_conversation_id: r.to_vec(),
            },
        }
    }

    #[test]
    fn the_domain_and_the_layout_are_pinned() {
        assert_eq!(LINEAGE_ENVELOPE_DOMAIN_V1.len(), 22);
        assert_eq!(LINEAGE_ENVELOPE_DOMAIN_V1.last(), Some(&0));
        let wire = compose_lineage_envelope(&[e(0x0102, b"\xAA", b"\xBB\xCC")]).unwrap();
        let mut expected = b"lakko/sync/lineage/v1\0".to_vec();
        expected.extend_from_slice(&[1, 0, 1, 0, 0, 0, 0, 0, 0, 1, 2, 1, 0xAA, 2, 0xBB, 0xCC]);
        assert_eq!(wire, expected);
        assert_eq!(LINEAGE_ENVELOPE_MIN_LEN, 37);
        assert_eq!(LINEAGE_ENVELOPE_MAX_LEN, 22 + 3 + 1024 * 138);
        assert!(is_lineage_envelope(&wire));
        assert!(!is_lineage_envelope(b"lakko/sync/lineage/v1"));
    }

    #[test]
    fn compose_sorts_and_decode_returns_the_same_entries() {
        let entries = [e(5, b"b", b"r"), e(9, b"a", b"r"), e(1, b"ab", b"q")];
        let wire = compose_lineage_envelope(&entries).unwrap();
        let back = decode_lineage_envelope(&wire).unwrap();
        assert_eq!(
            back,
            vec![e(9, b"a", b"r"), e(1, b"ab", b"q"), e(5, b"b", b"r")]
        );
        assert_eq!(compose_lineage_envelope(&back).unwrap(), wire);
    }

    #[test]
    fn compose_refuses_what_decode_refuses() {
        assert_eq!(
            compose_lineage_envelope(&[]),
            Err(LineageEnvelopeError::Count)
        );
        assert_eq!(
            compose_lineage_envelope(&[e(1, b"a", b"r"), e(2, b"a", b"q")]),
            Err(LineageEnvelopeError::NotCanonical)
        );
        assert_eq!(
            compose_lineage_envelope(&[e(1, b"a", b"a")]),
            Err(LineageEnvelopeError::Bounds(
                BoundsError::LineageSelfReference
            ))
        );
        assert_eq!(
            compose_lineage_envelope(&[e(1, &[1; 65], b"a")]),
            Err(LineageEnvelopeError::Bounds(BoundsError::LineageIdLength))
        );
        let many: Vec<_> = (0..=LINEAGE_ENVELOPE_MAX_ENTRIES)
            .map(|i| e(1, &u32::try_from(i).unwrap().to_be_bytes(), b"root"))
            .collect();
        assert_eq!(
            compose_lineage_envelope(&many),
            Err(LineageEnvelopeError::Count)
        );
        let full = compose_lineage_envelope(&many[..LINEAGE_ENVELOPE_MAX_ENTRIES]).unwrap();
        assert!(full.len() <= LINEAGE_ENVELOPE_MAX_LEN);
        assert_eq!(
            decode_lineage_envelope(&full).unwrap().len(),
            LINEAGE_ENVELOPE_MAX_ENTRIES
        );
    }

    #[test]
    fn decode_is_fail_closed() {
        let good = compose_lineage_envelope(&[e(1, b"a", b"r"), e(2, b"b", b"r")]).unwrap();
        for len in 0..good.len() {
            let err = decode_lineage_envelope(&good[..len]).err();
            assert!(
                matches!(
                    err,
                    Some(LineageEnvelopeError::WrongDomain | LineageEnvelopeError::Truncated)
                ),
                "prefix of {len} bytes gave {err:?}"
            );
        }
        let mut trailing = good.clone();
        trailing.push(0);
        assert_eq!(
            decode_lineage_envelope(&trailing),
            Err(LineageEnvelopeError::Trailing)
        );

        let header = LINEAGE_ENVELOPE_DOMAIN_V1.len();
        let mut version = good.clone();
        version[header] = 2;
        assert_eq!(
            decode_lineage_envelope(&version),
            Err(LineageEnvelopeError::UnknownVersion)
        );
        // The version is judged before the count and the entries.
        assert_eq!(
            decode_lineage_envelope(&version[..=header]),
            Err(LineageEnvelopeError::UnknownVersion)
        );

        let mut zero = good.clone();
        zero[header + 1] = 0;
        zero[header + 2] = 0;
        assert_eq!(
            decode_lineage_envelope(&zero),
            Err(LineageEnvelopeError::Count)
        );
        let mut huge = good.clone();
        huge[header + 1] = 0xFF;
        huge[header + 2] = 0xFF;
        assert_eq!(
            decode_lineage_envelope(&huge),
            Err(LineageEnvelopeError::Count)
        );

        // Descending and duplicate successors.
        let mut swapped = good[..header + 3].to_vec();
        swapped.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 2, 1, b'b', 1, b'r']);
        swapped.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1, 1, b'a', 1, b'r']);
        assert_eq!(
            decode_lineage_envelope(&swapped),
            Err(LineageEnvelopeError::NotCanonical)
        );
        let mut dup = good[..header + 3].to_vec();
        dup.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1, 1, b'a', 1, b'r']);
        dup.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 2, 1, b'a', 1, b'q']);
        assert_eq!(
            decode_lineage_envelope(&dup),
            Err(LineageEnvelopeError::NotCanonical)
        );

        // A zero-length id and a self reference.
        let mut empty_id = good[..header].to_vec();
        empty_id.extend_from_slice(&[1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1, b'r']);
        assert_eq!(
            decode_lineage_envelope(&empty_id),
            Err(LineageEnvelopeError::Bounds(BoundsError::LineageIdLength))
        );
        let mut own = good[..header].to_vec();
        own.extend_from_slice(&[1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 1, b'a', 1, b'a']);
        assert_eq!(
            decode_lineage_envelope(&own),
            Err(LineageEnvelopeError::Bounds(
                BoundsError::LineageSelfReference
            ))
        );
        assert_eq!(
            decode_lineage_envelope(b"lakko/sync/lineage/v2\0\x01"),
            Err(LineageEnvelopeError::WrongDomain)
        );
    }
}
