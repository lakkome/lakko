// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The account's own comments under channel posts, and their envelope
//! ([ADR-0087] section 13a).
//!
//! A comment row at the channel service names a write key derived from the
//! account seed, the post and a random 16-byte **comment nonce** the author
//! picked (`lakko_channels::write_key::CommentWriteKey`), so the service
//! cannot link two of one person's writes, not even a reaction and a comment
//! under one post (maintainer, 2026-11-02). Deleting a comment needs its key,
//! so every device of the account must know the nonce: this register carries
//! it, over the DeviceGroup and in the backup ([`super::SyncedState::to_bytes`]).
//! A device recognises its own comments in a page by deriving the key of
//! each nonce it holds for that post and matching the rows' writer keys.
//!
//! ## The register
//!
//! `(channel id, post cursor, comment nonce) -> live | deleted, post time`.
//! Folded by the ordinary rule ([`super::state`]): the greater stamp wins and,
//! on an exact tie, the larger canonical bytes, where the state byte follows
//! the key, so a deletion beats the comment. A deletion is kept as a
//! tombstone until the post goes, so an older envelope cannot bring the
//! comment back into the list of the account's own.
//!
//! **Bounded to the channel's retained posts**, as the own-reactions register
//! is: [`super::SyncedState::prune_channel_activity`] drops every register
//! whose post is older than [`CHANNEL_COMMENT_RETENTION_MS`] (30 days).
//! [`CHANNEL_COMMENTS_MAX_CLIENT`] is a client-side limit on the writer, not
//! a decode bound.
//!
//! ## Wire
//!
//! ```text
//! domain(30) "lakko/sync/channel-comment/v1\0"
//! version(1) = 0x01
//! count(1)                    1..=32
//! count x {
//!     hlc(8, BE)              the register's ORIGINAL stamp
//!     channel_id(16)
//!     post_cursor(8, BE)
//!     comment_nonce(16)
//!     state(1)                1 live, 2 deleted
//!     post_at_ms(8, BE)
//! }
//! ```
//!
//! Entries are sorted by `(channel_id, post_cursor, comment_nonce)`, strictly
//! increasing; nothing may trail; the encoding is canonical and every
//! violation refuses the whole envelope. Not in the growth snapshot: a device
//! re-sends [`super::SyncedState::channel_comment_envelopes`] on growth.
//!
//! ## Trust
//!
//! The bytes come from another device of the same account or from a backup.
//! A nonce is a secret of the account's: whoever holds it and the account
//! seed can delete that comment, and nobody without the account seed can.
//! The decoder is fixed-size per entry, bounded and total.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use thiserror::Error;

use super::channel_reaction::{CHANNEL_REACTION_CHANNEL_ID_LEN, CHANNEL_REACTION_RETENTION_MS};
use super::entry::{take_n, take_u64, take_u8, BoundsError, CodecError, Stamped};

/// Length of a comment nonce.
pub const CHANNEL_COMMENT_NONCE_LEN: usize = 16;

/// How long a post stays at the channel service, and so how long its
/// entries stay here: 30 days, the reactions register's retention.
pub const CHANNEL_COMMENT_RETENTION_MS: u64 = CHANNEL_REACTION_RETENTION_MS;

/// The most registers a client writes (a deletion of a held one is always
/// written). Chosen, not measured. The fold never refuses an entry for it.
pub const CHANNEL_COMMENTS_MAX_CLIENT: usize = 5_000;

/// Domain separator of the own-comments envelope, trailing NUL included.
/// Frozen.
pub const CHANNEL_COMMENT_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/sync/channel-comment/v1\0";

/// The version byte this build writes and accepts.
pub const CHANNEL_COMMENT_ENVELOPE_VERSION_V1: u8 = 0x01;

/// Most entries one envelope carries.
pub const CHANNEL_COMMENT_ENVELOPE_MAX_ENTRIES: usize = 32;

const STATE_LIVE: u8 = 1;
const STATE_DELETED: u8 = 2;

const ENTRY_LEN: usize =
    8 + CHANNEL_REACTION_CHANNEL_ID_LEN + 8 + CHANNEL_COMMENT_NONCE_LEN + 1 + 8;

/// Shortest valid envelope: the header and one entry.
pub const CHANNEL_COMMENT_ENVELOPE_MIN_LEN: usize =
    CHANNEL_COMMENT_ENVELOPE_DOMAIN_V1.len() + 1 + 1 + ENTRY_LEN;

/// Longest valid envelope.
pub const CHANNEL_COMMENT_ENVELOPE_MAX_LEN: usize = CHANNEL_COMMENT_ENVELOPE_DOMAIN_V1.len()
    + 1
    + 1
    + CHANNEL_COMMENT_ENVELOPE_MAX_ENTRIES * ENTRY_LEN;

/// One own-comment register value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelCommentEntry {
    /// The channel.
    pub channel_id: [u8; CHANNEL_REACTION_CHANNEL_ID_LEN],
    /// The post's server cursor.
    pub post_cursor: u64,
    /// The nonce the comment's write key was derived from. Secret.
    pub comment_nonce: [u8; CHANNEL_COMMENT_NONCE_LEN],
    /// `false` while the comment stands; `true` is the tombstone of a
    /// deleted one.
    pub deleted: bool,
    /// The post's server time, unix milliseconds: what bounds the register's
    /// life.
    pub post_at_ms: u64,
}

impl ChannelCommentEntry {
    /// The register key's parts, in the envelope's sort order.
    pub(crate) const fn sort_key(
        &self,
    ) -> (
        [u8; CHANNEL_REACTION_CHANNEL_ID_LEN],
        u64,
        [u8; CHANNEL_COMMENT_NONCE_LEN],
    ) {
        (self.channel_id, self.post_cursor, self.comment_nonce)
    }

    /// Whether the post this register names has left the channel service at
    /// `now_ms`.
    #[must_use]
    pub const fn expired_at(&self, now_ms: u64) -> bool {
        self.post_at_ms.saturating_add(CHANNEL_COMMENT_RETENTION_MS) < now_ms
    }

    /// Append the body: the key, then the state byte, then the post time.
    pub(crate) fn encode_body(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.channel_id);
        out.extend_from_slice(&self.post_cursor.to_be_bytes());
        out.extend_from_slice(&self.comment_nonce);
        out.push(if self.deleted {
            STATE_DELETED
        } else {
            STATE_LIVE
        });
        out.extend_from_slice(&self.post_at_ms.to_be_bytes());
    }

    /// Read a body written by [`Self::encode_body`].
    pub(crate) fn take_body(cur: &mut &[u8]) -> Result<Self, CodecError> {
        let mut channel_id = [0u8; CHANNEL_REACTION_CHANNEL_ID_LEN];
        channel_id.copy_from_slice(take_n(cur, CHANNEL_REACTION_CHANNEL_ID_LEN)?);
        let post_cursor = take_u64(cur)?;
        let mut comment_nonce = [0u8; CHANNEL_COMMENT_NONCE_LEN];
        comment_nonce.copy_from_slice(take_n(cur, CHANNEL_COMMENT_NONCE_LEN)?);
        let deleted = match take_u8(cur)? {
            STATE_LIVE => false,
            STATE_DELETED => true,
            _ => return Err(BoundsError::ChannelCommentState.into()),
        };
        let post_at_ms = take_u64(cur)?;
        Ok(Self {
            channel_id,
            post_cursor,
            comment_nonce,
            deleted,
            post_at_ms,
        })
    }
}

/// Why an own-comments envelope was refused. A reason, never the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ChannelCommentEnvelopeError {
    /// The bytes do not open with [`CHANNEL_COMMENT_ENVELOPE_DOMAIN_V1`].
    #[error("not an own-comments envelope")]
    WrongDomain,
    /// The version byte is not [`CHANNEL_COMMENT_ENVELOPE_VERSION_V1`].
    #[error("unknown own-comments envelope version")]
    UnknownVersion,
    /// The count is zero or above [`CHANNEL_COMMENT_ENVELOPE_MAX_ENTRIES`].
    #[error("own-comments envelope count is out of bounds")]
    Count,
    /// The bytes ended early, or an entry's state byte is unknown.
    #[error("own-comments envelope is truncated or malformed")]
    Truncated,
    /// Entries were not strictly increasing by key.
    #[error("own-comments envelope is not canonical")]
    NotCanonical,
    /// Bytes remained after the last entry.
    #[error("trailing bytes in own-comments envelope")]
    Trailing,
}

impl ChannelCommentEnvelopeError {
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

/// `true` when `bytes` open with the own-comments envelope domain.
#[must_use]
pub fn is_channel_comment_envelope(bytes: &[u8]) -> bool {
    bytes.starts_with(CHANNEL_COMMENT_ENVELOPE_DOMAIN_V1)
}

/// Compose one envelope of 1..=32 stamped registers, sorted by key.
///
/// # Errors
///
/// [`ChannelCommentEnvelopeError::Count`] or
/// [`ChannelCommentEnvelopeError::NotCanonical`] (two entries of one
/// register).
pub fn compose_channel_comment_envelope(
    entries: &[Stamped<ChannelCommentEntry>],
) -> Result<Vec<u8>, ChannelCommentEnvelopeError> {
    if entries.is_empty() || entries.len() > CHANNEL_COMMENT_ENVELOPE_MAX_ENTRIES {
        return Err(ChannelCommentEnvelopeError::Count);
    }
    let count = u8::try_from(entries.len()).map_err(|_| ChannelCommentEnvelopeError::Count)?;
    let mut sorted: Vec<&Stamped<ChannelCommentEntry>> = entries.iter().collect();
    sorted.sort_by_key(|e| e.value.sort_key());
    if sorted
        .windows(2)
        .any(|w| w[0].value.sort_key() == w[1].value.sort_key())
    {
        return Err(ChannelCommentEnvelopeError::NotCanonical);
    }
    let mut out = Vec::with_capacity(
        CHANNEL_COMMENT_ENVELOPE_DOMAIN_V1.len() + 2 + entries.len() * ENTRY_LEN,
    );
    out.extend_from_slice(CHANNEL_COMMENT_ENVELOPE_DOMAIN_V1);
    out.push(CHANNEL_COMMENT_ENVELOPE_VERSION_V1);
    out.push(count);
    for e in sorted {
        out.extend_from_slice(&e.hlc.to_be_bytes());
        e.value.encode_body(&mut out);
    }
    Ok(out)
}

/// Decode one envelope, fail-closed: the whole envelope or nothing.
///
/// # Errors
///
/// Every [`ChannelCommentEnvelopeError`]; the domain first, then the
/// version, then the count, then each entry, then trailing bytes.
pub fn decode_channel_comment_envelope(
    bytes: &[u8],
) -> Result<Vec<Stamped<ChannelCommentEntry>>, ChannelCommentEnvelopeError> {
    let mut cur = bytes
        .strip_prefix(CHANNEL_COMMENT_ENVELOPE_DOMAIN_V1)
        .ok_or(ChannelCommentEnvelopeError::WrongDomain)?;
    let version = take_u8(&mut cur).map_err(|_| ChannelCommentEnvelopeError::Truncated)?;
    if version != CHANNEL_COMMENT_ENVELOPE_VERSION_V1 {
        return Err(ChannelCommentEnvelopeError::UnknownVersion);
    }
    let count = usize::from(take_u8(&mut cur).map_err(|_| ChannelCommentEnvelopeError::Truncated)?);
    if count == 0 || count > CHANNEL_COMMENT_ENVELOPE_MAX_ENTRIES {
        return Err(ChannelCommentEnvelopeError::Count);
    }
    let mut out: Vec<Stamped<ChannelCommentEntry>> = Vec::with_capacity(count);
    for _ in 0..count {
        let hlc = take_u64(&mut cur).map_err(|_| ChannelCommentEnvelopeError::Truncated)?;
        let value = ChannelCommentEntry::take_body(&mut cur)
            .map_err(|_| ChannelCommentEnvelopeError::Truncated)?;
        if let Some(last) = out.last() {
            if last.value.sort_key() >= value.sort_key() {
                return Err(ChannelCommentEnvelopeError::NotCanonical);
            }
        }
        out.push(Stamped { hlc, value });
    }
    if !cur.is_empty() {
        return Err(ChannelCommentEnvelopeError::Trailing);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn entry(
        channel: u8,
        cursor: u64,
        nonce: u8,
        deleted: bool,
        hlc: u64,
    ) -> Stamped<ChannelCommentEntry> {
        Stamped {
            hlc,
            value: ChannelCommentEntry {
                channel_id: [channel; 16],
                post_cursor: cursor,
                comment_nonce: [nonce; 16],
                deleted,
                post_at_ms: 1_700_000_000_000_u64.saturating_add(cursor),
            },
        }
    }

    #[test]
    fn the_layout_is_pinned() {
        let wire =
            compose_channel_comment_envelope(&[entry(1, 7, 5, false, 9)]).unwrap_or_default();
        assert_eq!(CHANNEL_COMMENT_ENVELOPE_DOMAIN_V1.len(), 30);
        assert_eq!(ENTRY_LEN, 57);
        assert_eq!(CHANNEL_COMMENT_ENVELOPE_MIN_LEN, 30 + 2 + 57);
        assert_eq!(wire.len(), CHANNEL_COMMENT_ENVELOPE_MIN_LEN);
        assert_eq!(wire[30], 0x01);
        assert_eq!(wire[31], 1);
        assert_eq!(&wire[32..40], &9u64.to_be_bytes());
        assert_eq!(&wire[40..56], &[1u8; 16]);
        assert_eq!(&wire[56..64], &7u64.to_be_bytes());
        assert_eq!(&wire[64..80], &[5u8; 16]);
        assert_eq!(wire[80], STATE_LIVE);
        assert!(is_channel_comment_envelope(&wire));
        assert!(!is_channel_comment_envelope(
            b"lakko/sync/channel-reaction/v1\0"
        ));
    }

    #[test]
    fn compose_sorts_and_refuses_duplicates_and_bad_counts() {
        let wire =
            compose_channel_comment_envelope(&[entry(2, 1, 1, false, 1), entry(1, 9, 3, true, 2)])
                .unwrap_or_default();
        assert_eq!(
            decode_channel_comment_envelope(&wire).unwrap_or_default(),
            vec![entry(1, 9, 3, true, 2), entry(2, 1, 1, false, 1)]
        );
        assert_eq!(
            compose_channel_comment_envelope(&[entry(1, 1, 1, false, 1), entry(1, 1, 1, true, 2)]),
            Err(ChannelCommentEnvelopeError::NotCanonical)
        );
        assert_eq!(
            compose_channel_comment_envelope(&[]),
            Err(ChannelCommentEnvelopeError::Count)
        );
        let many: Vec<_> = (0..33).map(|c| entry(1, c, 1, false, 1)).collect();
        assert_eq!(
            compose_channel_comment_envelope(&many),
            Err(ChannelCommentEnvelopeError::Count)
        );
    }

    #[test]
    fn decode_refuses_every_malformed_shape() {
        let wire =
            compose_channel_comment_envelope(&[entry(1, 1, 1, false, 1), entry(1, 2, 1, false, 2)])
                .unwrap_or_default();
        for cut in 0..wire.len() {
            assert!(
                decode_channel_comment_envelope(&wire[..cut]).is_err(),
                "{cut}"
            );
        }
        let mut version = wire.clone();
        version[30] = 2;
        assert_eq!(
            decode_channel_comment_envelope(&version),
            Err(ChannelCommentEnvelopeError::UnknownVersion)
        );
        let mut count = wire.clone();
        count[31] = 0;
        assert_eq!(
            decode_channel_comment_envelope(&count),
            Err(ChannelCommentEnvelopeError::Count)
        );
        let mut trailing = wire.clone();
        trailing.push(0);
        assert_eq!(
            decode_channel_comment_envelope(&trailing),
            Err(ChannelCommentEnvelopeError::Trailing)
        );
        let mut state = wire.clone();
        state[80] = 3;
        assert_eq!(
            decode_channel_comment_envelope(&state),
            Err(ChannelCommentEnvelopeError::Truncated)
        );
        let mut swapped = wire[..32].to_vec();
        swapped.extend_from_slice(&wire[32 + ENTRY_LEN..]);
        swapped.extend_from_slice(&wire[32..32 + ENTRY_LEN]);
        assert_eq!(
            decode_channel_comment_envelope(&swapped),
            Err(ChannelCommentEnvelopeError::NotCanonical)
        );
        assert_eq!(
            decode_channel_comment_envelope(b"lakko/sync/channel-reaction/v1\0\x01"),
            Err(ChannelCommentEnvelopeError::WrongDomain)
        );
    }

    proptest! {
        #[test]
        fn decode_is_total_and_canonical(bytes in proptest::collection::vec(any::<u8>(), 0..600)) {
            let mut input = CHANNEL_COMMENT_ENVELOPE_DOMAIN_V1.to_vec();
            input.extend_from_slice(&bytes);
            if let Ok(entries) = decode_channel_comment_envelope(&input) {
                prop_assert!(input.len() <= CHANNEL_COMMENT_ENVELOPE_MAX_LEN);
                prop_assert_eq!(compose_channel_comment_envelope(&entries), Ok(input));
            }
        }

        #[test]
        fn round_trip(nonces in proptest::collection::btree_set(any::<u8>(), 1..=32), hlc in any::<u64>()) {
            let entries: Vec<_> = nonces.iter().map(|n| entry(3, 7, *n, n % 2 == 0, hlc)).collect();
            let wire = compose_channel_comment_envelope(&entries).unwrap_or_default();
            prop_assert_eq!(wire.len(), CHANNEL_COMMENT_ENVELOPE_MIN_LEN + (entries.len() - 1) * ENTRY_LEN);
            prop_assert_eq!(decode_channel_comment_envelope(&wire), Ok(entries));
        }
    }
}
