// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The block register and its envelope (ADR-0099 section 6, UX
//! `reporting-and-moderation.md` section 1).
//!
//! Blocking someone is an account-level setting carried between the
//! account's own devices, like folders. One register per blocked account,
//! keyed on that account's 32-byte account signature key, holding
//! `Blocked` or `Unblocked` and the wall-clock time of the change, folded
//! by the ordinary rule ([`super::state`]): the strictly greater stamp
//! wins, and on an exact tie the larger canonical bytes. The state byte
//! follows the key, and `Blocked` is the larger byte, so on a tie a block
//! beats an unblock: when two devices disagree at one instant, the person
//! stays protected.
//!
//! An `Unblocked` register is kept, never deleted, so a stale `Blocked`
//! arriving later cannot bring the block back.
//!
//! ## Wire
//!
//! ```text
//! domain(22) "lakko/sync/blocked/v1\0"
//! version(1) = 0x01
//! count(1)                    1..=16
//! count x {
//!     hlc(8, BE)              the register's ORIGINAL stamp
//!     account_key(32)         the blocked account's signature key
//!     state(1)                1 unblocked, 2 blocked
//!     changed_at_ms(8, BE)
//! }
//! ```
//!
//! Entries are sorted by account key, strictly increasing; nothing may
//! trail. The encoding is canonical. Every violation refuses the whole
//! envelope.
//!
//! ## Trust
//!
//! The bytes arrive from another device of the same account, inside an MLS
//! application message of the DeviceGroup, or from a backup. A device of the
//! account can already block or unblock anyone through the app, so a hostile
//! device of the account gains nothing by forging one. The decoder is
//! bounded and total on arbitrary input.

use thiserror::Error;

use super::entry::{take_n, take_u64, take_u8, CodecError, Stamped};

/// Length of an account signature key.
pub const BLOCKED_KEY_LEN: usize = 32;

/// Domain separator of the block envelope, trailing NUL included. Frozen.
pub const BLOCKED_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/sync/blocked/v1\0";

/// The version byte this build writes and accepts.
pub const BLOCKED_ENVELOPE_VERSION_V1: u8 = 0x01;

/// Most entries one envelope carries.
pub const BLOCKED_ENVELOPE_MAX_ENTRIES: usize = 16;

/// The wire byte of an unblock.
const STATE_UNBLOCKED: u8 = 1;
/// The wire byte of a block. Larger than [`STATE_UNBLOCKED`] on purpose.
const STATE_BLOCKED: u8 = 2;

const ENTRY_LEN: usize = 8 + BLOCKED_KEY_LEN + 1 + 8;

/// Shortest valid envelope: the header and one entry.
pub const BLOCKED_ENVELOPE_MIN_LEN: usize = BLOCKED_ENVELOPE_DOMAIN_V1.len() + 1 + 1 + ENTRY_LEN;

/// Longest valid envelope.
pub const BLOCKED_ENVELOPE_MAX_LEN: usize =
    BLOCKED_ENVELOPE_DOMAIN_V1.len() + 1 + 1 + BLOCKED_ENVELOPE_MAX_ENTRIES * ENTRY_LEN;

/// One block register value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedEntry {
    /// The account signature key of the person blocked or unblocked.
    pub account_key: [u8; BLOCKED_KEY_LEN],
    /// `true` blocked, `false` unblocked.
    pub blocked: bool,
    /// Wall-clock millis of the change. Shown in the blocked list; decides
    /// nothing.
    pub changed_at_ms: u64,
}

impl BlockedEntry {
    /// Append the body (key, state byte, time) to `out`.
    pub(crate) fn encode_body(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.account_key);
        out.push(if self.blocked {
            STATE_BLOCKED
        } else {
            STATE_UNBLOCKED
        });
        out.extend_from_slice(&self.changed_at_ms.to_be_bytes());
    }

    /// Read a body written by [`Self::encode_body`].
    pub(crate) fn take_body(cur: &mut &[u8]) -> Result<Self, CodecError> {
        let key = take_n(cur, BLOCKED_KEY_LEN)?;
        let mut account_key = [0u8; BLOCKED_KEY_LEN];
        account_key.copy_from_slice(key);
        let blocked = match take_u8(cur)? {
            STATE_BLOCKED => true,
            STATE_UNBLOCKED => false,
            _ => return Err(CodecError::BadBool),
        };
        let changed_at_ms = take_u64(cur)?;
        Ok(Self {
            account_key,
            blocked,
            changed_at_ms,
        })
    }
}

/// Why a block envelope was refused. A reason, never the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BlockedEnvelopeError {
    /// The bytes do not open with [`BLOCKED_ENVELOPE_DOMAIN_V1`].
    #[error("not a block envelope")]
    WrongDomain,
    /// The version byte is not [`BLOCKED_ENVELOPE_VERSION_V1`].
    #[error("unknown block envelope version")]
    UnknownVersion,
    /// The count is zero or above [`BLOCKED_ENVELOPE_MAX_ENTRIES`].
    #[error("block envelope count is out of bounds")]
    Count,
    /// The bytes ended early, or a state byte is unknown.
    #[error("block envelope is truncated or malformed")]
    Truncated,
    /// Entries were not strictly increasing by account key.
    #[error("block envelope is not canonical")]
    NotCanonical,
    /// Bytes remained after the last entry.
    #[error("trailing bytes in block envelope")]
    Trailing,
}

impl BlockedEnvelopeError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::UnknownVersion => "unknownVersion",
            Self::Count => "count",
            Self::Truncated => "truncated",
            Self::NotCanonical => "notCanonical",
            Self::Trailing => "trailing",
        }
    }
}

/// `true` when `bytes` open with the block envelope domain: the cheap
/// routing question a client asks of a DeviceGroup service envelope. Says
/// nothing about validity.
#[must_use]
pub fn is_blocked_envelope(bytes: &[u8]) -> bool {
    bytes.starts_with(BLOCKED_ENVELOPE_DOMAIN_V1)
}

/// Compose one envelope of 1..=16 stamped registers, sorted by account key.
///
/// # Errors
///
/// [`BlockedEnvelopeError::Count`] for none or too many entries,
/// [`BlockedEnvelopeError::NotCanonical`] for two entries of one account.
pub fn compose_blocked_envelope(
    entries: &[Stamped<BlockedEntry>],
) -> Result<Vec<u8>, BlockedEnvelopeError> {
    if entries.is_empty() || entries.len() > BLOCKED_ENVELOPE_MAX_ENTRIES {
        return Err(BlockedEnvelopeError::Count);
    }
    let count = u8::try_from(entries.len()).map_err(|_| BlockedEnvelopeError::Count)?;
    let mut sorted: Vec<&Stamped<BlockedEntry>> = entries.iter().collect();
    sorted.sort_by_key(|e| e.value.account_key);
    let mut out =
        Vec::with_capacity(BLOCKED_ENVELOPE_DOMAIN_V1.len() + 2 + entries.len() * ENTRY_LEN);
    out.extend_from_slice(BLOCKED_ENVELOPE_DOMAIN_V1);
    out.push(BLOCKED_ENVELOPE_VERSION_V1);
    out.push(count);
    let mut previous: Option<&[u8; BLOCKED_KEY_LEN]> = None;
    for e in sorted {
        if previous == Some(&e.value.account_key) {
            return Err(BlockedEnvelopeError::NotCanonical);
        }
        previous = Some(&e.value.account_key);
        out.extend_from_slice(&e.hlc.to_be_bytes());
        e.value.encode_body(&mut out);
    }
    Ok(out)
}

/// Decode one envelope, fail-closed: the whole envelope or nothing.
///
/// # Errors
///
/// Every [`BlockedEnvelopeError`]; the domain is checked first, then the
/// version, then the count, then each entry, then trailing bytes.
pub fn decode_blocked_envelope(
    bytes: &[u8],
) -> Result<Vec<Stamped<BlockedEntry>>, BlockedEnvelopeError> {
    let mut cur = bytes
        .strip_prefix(BLOCKED_ENVELOPE_DOMAIN_V1)
        .ok_or(BlockedEnvelopeError::WrongDomain)?;
    let version = take_u8(&mut cur).map_err(|_| BlockedEnvelopeError::Truncated)?;
    if version != BLOCKED_ENVELOPE_VERSION_V1 {
        return Err(BlockedEnvelopeError::UnknownVersion);
    }
    let count = usize::from(take_u8(&mut cur).map_err(|_| BlockedEnvelopeError::Truncated)?);
    if count == 0 || count > BLOCKED_ENVELOPE_MAX_ENTRIES {
        return Err(BlockedEnvelopeError::Count);
    }
    let mut out: Vec<Stamped<BlockedEntry>> = Vec::with_capacity(count);
    for _ in 0..count {
        let hlc = take_u64(&mut cur).map_err(|_| BlockedEnvelopeError::Truncated)?;
        let value =
            BlockedEntry::take_body(&mut cur).map_err(|_| BlockedEnvelopeError::Truncated)?;
        if let Some(last) = out.last() {
            if last.value.account_key >= value.account_key {
                return Err(BlockedEnvelopeError::NotCanonical);
            }
        }
        out.push(Stamped { hlc, value });
    }
    if !cur.is_empty() {
        return Err(BlockedEnvelopeError::Trailing);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn entry(key: u8, blocked: bool, hlc: u64) -> Stamped<BlockedEntry> {
        Stamped {
            hlc,
            value: BlockedEntry {
                account_key: [key; BLOCKED_KEY_LEN],
                blocked,
                changed_at_ms: 1_700_000_000_000 + u64::from(key),
            },
        }
    }

    #[test]
    fn the_layout_is_pinned() {
        let wire = compose_blocked_envelope(&[entry(1, true, 7)]).unwrap_or_default();
        assert_eq!(wire.len(), BLOCKED_ENVELOPE_MIN_LEN);
        assert_eq!(BLOCKED_ENVELOPE_DOMAIN_V1.len(), 22);
        assert_eq!(BLOCKED_ENVELOPE_MIN_LEN, 73);
        assert_eq!(BLOCKED_ENVELOPE_MAX_LEN, 22 + 2 + 16 * 49);
        assert_eq!(wire[22], 0x01);
        assert_eq!(wire[23], 1);
        assert_eq!(&wire[24..32], &7u64.to_be_bytes());
        assert_eq!(&wire[32..64], &[1u8; 32]);
        assert_eq!(wire[64], STATE_BLOCKED);
        assert!(is_blocked_envelope(&wire));
        assert!(!is_blocked_envelope(b"lakko/sync/device-ack/v1\0"));
    }

    #[test]
    fn compose_sorts_and_refuses_duplicates_and_bad_counts() {
        let wire =
            compose_blocked_envelope(&[entry(9, true, 1), entry(3, false, 2)]).unwrap_or_default();
        let back = decode_blocked_envelope(&wire).unwrap_or_default();
        assert_eq!(back, vec![entry(3, false, 2), entry(9, true, 1)]);
        assert_eq!(
            compose_blocked_envelope(&[entry(3, true, 1), entry(3, false, 2)]),
            Err(BlockedEnvelopeError::NotCanonical)
        );
        assert_eq!(
            compose_blocked_envelope(&[]),
            Err(BlockedEnvelopeError::Count)
        );
        let many: Vec<_> = (0..17).map(|k| entry(k, true, 1)).collect();
        assert_eq!(
            compose_blocked_envelope(&many),
            Err(BlockedEnvelopeError::Count)
        );
    }

    #[test]
    fn decode_refuses_every_malformed_shape() {
        let wire =
            compose_blocked_envelope(&[entry(1, true, 1), entry(2, false, 2)]).unwrap_or_default();
        let mut v = wire.clone();
        v[22] = 2;
        assert_eq!(
            decode_blocked_envelope(&v),
            Err(BlockedEnvelopeError::UnknownVersion)
        );
        let mut c = wire.clone();
        c[23] = 0;
        assert_eq!(
            decode_blocked_envelope(&c),
            Err(BlockedEnvelopeError::Count)
        );
        let mut s = wire.clone();
        s[64] = 3;
        assert_eq!(
            decode_blocked_envelope(&s),
            Err(BlockedEnvelopeError::Truncated)
        );
        let mut t = wire.clone();
        t.push(0);
        assert_eq!(
            decode_blocked_envelope(&t),
            Err(BlockedEnvelopeError::Trailing)
        );
        for cut in 0..wire.len() {
            assert!(decode_blocked_envelope(&wire[..cut]).is_err(), "{cut}");
        }
        let mut swapped = wire[..24].to_vec();
        swapped.extend_from_slice(&wire[24 + ENTRY_LEN..]);
        swapped.extend_from_slice(&wire[24..24 + ENTRY_LEN]);
        assert_eq!(
            decode_blocked_envelope(&swapped),
            Err(BlockedEnvelopeError::NotCanonical)
        );
        assert_eq!(
            decode_blocked_envelope(b"lakko/sync/username/v1\0\x01"),
            Err(BlockedEnvelopeError::WrongDomain)
        );
    }

    proptest! {
        #[test]
        fn decode_is_total_and_canonical(bytes in proptest::collection::vec(any::<u8>(), 0..900)) {
            let mut input = BLOCKED_ENVELOPE_DOMAIN_V1.to_vec();
            input.extend_from_slice(&bytes);
            if let Ok(entries) = decode_blocked_envelope(&input) {
                prop_assert_eq!(compose_blocked_envelope(&entries), Ok(input));
            }
        }

        #[test]
        fn round_trip(
            keys in proptest::collection::btree_map(any::<u8>(), any::<bool>(), 1..=16),
            hlc in any::<u64>(),
        ) {
            let entries: Vec<_> = keys.iter().map(|(k, b)| entry(*k, *b, hlc)).collect();
            let wire = compose_blocked_envelope(&entries).unwrap_or_default();
            prop_assert!(wire.len() <= BLOCKED_ENVELOPE_MAX_LEN);
            prop_assert_eq!(decode_blocked_envelope(&wire), Ok(entries));
        }
    }
}
