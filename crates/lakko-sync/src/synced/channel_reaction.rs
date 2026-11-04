// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The account's own reactions under channel posts, and their envelope
//! ([ADR-0087] section 13a, "The account's own reactions").
//!
//! A reaction under a channel post is a row at the channel service signed by
//! a key derived per post, so the service cannot link two of one person's
//! reactions. For the same reason a device never asks the service which
//! reactions are its own: a request naming the keys of several posts would
//! link exactly what the per-post keys keep apart. The account's devices keep
//! the list among themselves instead, in this register, carried over the
//! DeviceGroup and in the backup ([`super::SyncedState::to_bytes`]).
//!
//! ## The register
//!
//! `(channel id, post cursor, emoji) -> reacted | removed, post time`. One
//! register per reaction the account ever gave or took off; folded by the
//! ordinary rule ([`super::state`]): the greater stamp wins, and on an exact
//! tie the larger canonical bytes, where the state byte follows the key, so a
//! removal beats a reaction. A removal is kept as a tombstone, so a device
//! that was offline across it cannot bring the reaction back with an older
//! stamp. The emoji is stored as its text, not its position in the fixed
//! list, so a later list that appends cannot shift an entry.
//!
//! **Bounded to the channel's retained posts.** `post_at_ms` is the server
//! time of the post. A post leaves the channel service after
//! [`CHANNEL_REACTION_RETENTION_MS`] (30 days), and it takes its entries with
//! it: [`super::SyncedState::prune_channel_reactions`] drops every register
//! whose post is older than that. The client calls it when it loads the
//! state and after applying envelopes; an entry for a gone post that a late
//! envelope brings back is harmless (nothing shows it) and goes at the next
//! prune. [`CHANNEL_REACTIONS_MAX_CLIENT`] is a client-side limit on the
//! writer, not a decode bound, as the other registers' limits are, and so is
//! [`CHANNEL_REACTIONS_PER_POST_MAX`]: one person gives a post at most three
//! reactions, which the service cannot check.
//!
//! ## Wire
//!
//! ```text
//! domain(31) "lakko/sync/channel-reaction/v1\0"
//! version(1) = 0x01
//! count(1)                    1..=32
//! count x {
//!     hlc(8, BE)              the register's ORIGINAL stamp
//!     channel_id(16)
//!     post_cursor(8, BE)
//!     emoji_len(1)            1..=64
//!     emoji
//!     state(1)                1 reacted, 2 removed
//!     post_at_ms(8, BE)
//! }
//! ```
//!
//! Entries are sorted by `(channel_id, post_cursor, emoji bytes)`, strictly
//! increasing; nothing may trail; the encoding is canonical and every
//! violation refuses the whole envelope. It is not in the growth snapshot: a
//! device re-sends [`super::SyncedState::channel_reaction_envelopes`] on
//! growth, as it does the device acknowledgements.
//!
//! ## Trust
//!
//! The bytes come from another device of the same account (inside a
//! DeviceGroup application message) or from a backup. A hostile device of
//! the account can at most make the others believe it reacted, or did not,
//! which it could do by reacting. The decoder is bounded and total.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use thiserror::Error;

use super::entry::{take_n, take_u64, take_u8, BoundsError, CodecError, Stamped};

/// Length of a channel id.
pub const CHANNEL_REACTION_CHANNEL_ID_LEN: usize = 16;

/// The most bytes an emoji takes (the chat reaction bound,
/// `lakko_attachments::MAX_EMOJI_BYTES`).
pub const CHANNEL_REACTION_EMOJI_MAX_BYTES: usize = 64;

/// How long a post stays at the channel service, and so how long its
/// entries stay here: 30 days (ADR-0087 section 13).
pub const CHANNEL_REACTION_RETENTION_MS: u64 = 30 * 24 * 60 * 60 * 1000;

/// The most registers a client writes (a removal of a held one is always
/// written). Chosen, not measured: 30 days of posts at several reactions a
/// day in many channels fits well below it. The fold never refuses an
/// entry for it.
pub const CHANNEL_REACTIONS_MAX_CLIENT: usize = 20_000;

/// The most reactions one person gives one post (maintainer, 2026-11-02,
/// the same rule as in a chat: `lakko_ui_rules::reaction::MAX_REACTIONS_PER_MESSAGE`).
/// Nothing at the service can enforce it (every reaction has its own write
/// key), so it is the writer's rule: [`super::SyncedState::set_channel_reaction`]
/// refuses a fourth. The fold never refuses: two devices reacting at once can
/// leave a post with four, which the next removal brings back under.
pub const CHANNEL_REACTIONS_PER_POST_MAX: usize = 3;

/// Domain separator of the channel-reaction envelope, trailing NUL included.
/// Frozen.
pub const CHANNEL_REACTION_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/sync/channel-reaction/v1\0";

/// The version byte this build writes and accepts.
pub const CHANNEL_REACTION_ENVELOPE_VERSION_V1: u8 = 0x01;

/// Most entries one envelope carries.
pub const CHANNEL_REACTION_ENVELOPE_MAX_ENTRIES: usize = 32;

const STATE_REACTED: u8 = 1;
const STATE_REMOVED: u8 = 2;

/// Fixed part of one envelope entry: hlc, channel id, cursor, emoji length,
/// state, post time.
const ENTRY_FIXED_LEN: usize = 8 + CHANNEL_REACTION_CHANNEL_ID_LEN + 8 + 1 + 1 + 8;

/// Shortest valid envelope: the header and one entry with a one-byte emoji.
pub const CHANNEL_REACTION_ENVELOPE_MIN_LEN: usize =
    CHANNEL_REACTION_ENVELOPE_DOMAIN_V1.len() + 1 + 1 + ENTRY_FIXED_LEN + 1;

/// Longest valid envelope.
pub const CHANNEL_REACTION_ENVELOPE_MAX_LEN: usize = CHANNEL_REACTION_ENVELOPE_DOMAIN_V1.len()
    + 1
    + 1
    + CHANNEL_REACTION_ENVELOPE_MAX_ENTRIES * (ENTRY_FIXED_LEN + CHANNEL_REACTION_EMOJI_MAX_BYTES);

/// One own-reaction register value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelReactionEntry {
    /// The channel.
    pub channel_id: [u8; CHANNEL_REACTION_CHANNEL_ID_LEN],
    /// The post's server cursor.
    pub post_cursor: u64,
    /// The emoji, as the fixed list spells it.
    pub emoji: String,
    /// `true` while the reaction stands; `false` is the tombstone of one
    /// taken off.
    pub reacted: bool,
    /// The post's server time, unix milliseconds: what bounds the register's
    /// life.
    pub post_at_ms: u64,
}

impl ChannelReactionEntry {
    /// Check the emoji bound.
    ///
    /// # Errors
    ///
    /// [`BoundsError::ChannelReactionEmoji`] for an empty or over-long emoji.
    pub fn validate(&self) -> Result<(), BoundsError> {
        if self.emoji.is_empty() || self.emoji.len() > CHANNEL_REACTION_EMOJI_MAX_BYTES {
            return Err(BoundsError::ChannelReactionEmoji);
        }
        Ok(())
    }

    /// The register key's parts, in the envelope's sort order.
    pub(crate) fn sort_key(&self) -> ([u8; CHANNEL_REACTION_CHANNEL_ID_LEN], u64, &[u8]) {
        (self.channel_id, self.post_cursor, self.emoji.as_bytes())
    }

    /// Whether the post this register names has left the channel service at
    /// `now_ms`.
    #[must_use]
    pub const fn expired_at(&self, now_ms: u64) -> bool {
        self.post_at_ms
            .saturating_add(CHANNEL_REACTION_RETENTION_MS)
            < now_ms
    }

    /// Append the body to `out`: the key, then the state byte, then the post
    /// time. Call only on a validated entry.
    pub(crate) fn encode_body(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.channel_id);
        out.extend_from_slice(&self.post_cursor.to_be_bytes());
        // A validated emoji is at most 64 bytes.
        out.push(u8::try_from(self.emoji.len()).unwrap_or(u8::MAX));
        out.extend_from_slice(self.emoji.as_bytes());
        out.push(if self.reacted {
            STATE_REACTED
        } else {
            STATE_REMOVED
        });
        out.extend_from_slice(&self.post_at_ms.to_be_bytes());
    }

    /// Read a body written by [`Self::encode_body`]. The emoji length is
    /// checked against its bound before the bytes are taken.
    pub(crate) fn take_body(cur: &mut &[u8]) -> Result<Self, CodecError> {
        let id = take_n(cur, CHANNEL_REACTION_CHANNEL_ID_LEN)?;
        let mut channel_id = [0u8; CHANNEL_REACTION_CHANNEL_ID_LEN];
        channel_id.copy_from_slice(id);
        let post_cursor = take_u64(cur)?;
        let emoji_len = usize::from(take_u8(cur)?);
        if emoji_len == 0 || emoji_len > CHANNEL_REACTION_EMOJI_MAX_BYTES {
            return Err(BoundsError::ChannelReactionEmoji.into());
        }
        let emoji = core::str::from_utf8(take_n(cur, emoji_len)?)
            .map_err(|_| CodecError::NotUtf8)?
            .to_string();
        let reacted = match take_u8(cur)? {
            STATE_REACTED => true,
            STATE_REMOVED => false,
            _ => return Err(BoundsError::ChannelReactionState.into()),
        };
        let post_at_ms = take_u64(cur)?;
        Ok(Self {
            channel_id,
            post_cursor,
            emoji,
            reacted,
            post_at_ms,
        })
    }
}

/// Why a channel-reaction envelope was refused. A reason, never the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ChannelReactionEnvelopeError {
    /// The bytes do not open with [`CHANNEL_REACTION_ENVELOPE_DOMAIN_V1`].
    #[error("not a channel-reaction envelope")]
    WrongDomain,
    /// The version byte is not [`CHANNEL_REACTION_ENVELOPE_VERSION_V1`].
    #[error("unknown channel-reaction envelope version")]
    UnknownVersion,
    /// The count is zero or above [`CHANNEL_REACTION_ENVELOPE_MAX_ENTRIES`].
    #[error("channel-reaction envelope count is out of bounds")]
    Count,
    /// The bytes ended early, or an entry is malformed (an emoji length out
    /// of bounds, not UTF-8, an unknown state byte).
    #[error("channel-reaction envelope is truncated or malformed")]
    Truncated,
    /// Entries were not strictly increasing by key.
    #[error("channel-reaction envelope is not canonical")]
    NotCanonical,
    /// Bytes remained after the last entry.
    #[error("trailing bytes in channel-reaction envelope")]
    Trailing,
}

impl ChannelReactionEnvelopeError {
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

/// `true` when `bytes` open with the channel-reaction envelope domain: the
/// cheap routing question. Says nothing about validity.
#[must_use]
pub fn is_channel_reaction_envelope(bytes: &[u8]) -> bool {
    bytes.starts_with(CHANNEL_REACTION_ENVELOPE_DOMAIN_V1)
}

/// Compose one envelope of 1..=32 stamped registers, sorted by key.
///
/// # Errors
///
/// [`ChannelReactionEnvelopeError::Count`] for none or too many entries,
/// [`ChannelReactionEnvelopeError::NotCanonical`] for two entries of one
/// register, [`ChannelReactionEnvelopeError::Truncated`] for an entry that
/// breaks the emoji bound.
pub fn compose_channel_reaction_envelope(
    entries: &[Stamped<ChannelReactionEntry>],
) -> Result<Vec<u8>, ChannelReactionEnvelopeError> {
    if entries.is_empty() || entries.len() > CHANNEL_REACTION_ENVELOPE_MAX_ENTRIES {
        return Err(ChannelReactionEnvelopeError::Count);
    }
    if entries.iter().any(|e| e.value.validate().is_err()) {
        return Err(ChannelReactionEnvelopeError::Truncated);
    }
    let count = u8::try_from(entries.len()).map_err(|_| ChannelReactionEnvelopeError::Count)?;
    let mut sorted: Vec<&Stamped<ChannelReactionEntry>> = entries.iter().collect();
    sorted.sort_by(|a, b| a.value.sort_key().cmp(&b.value.sort_key()));
    if sorted
        .windows(2)
        .any(|w| w[0].value.sort_key() == w[1].value.sort_key())
    {
        return Err(ChannelReactionEnvelopeError::NotCanonical);
    }
    let mut out = Vec::with_capacity(CHANNEL_REACTION_ENVELOPE_MAX_LEN);
    out.extend_from_slice(CHANNEL_REACTION_ENVELOPE_DOMAIN_V1);
    out.push(CHANNEL_REACTION_ENVELOPE_VERSION_V1);
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
/// Every [`ChannelReactionEnvelopeError`]; the domain is checked first, then
/// the version, then the count, then each entry, then trailing bytes.
pub fn decode_channel_reaction_envelope(
    bytes: &[u8],
) -> Result<Vec<Stamped<ChannelReactionEntry>>, ChannelReactionEnvelopeError> {
    let mut cur = bytes
        .strip_prefix(CHANNEL_REACTION_ENVELOPE_DOMAIN_V1)
        .ok_or(ChannelReactionEnvelopeError::WrongDomain)?;
    let version = take_u8(&mut cur).map_err(|_| ChannelReactionEnvelopeError::Truncated)?;
    if version != CHANNEL_REACTION_ENVELOPE_VERSION_V1 {
        return Err(ChannelReactionEnvelopeError::UnknownVersion);
    }
    let count =
        usize::from(take_u8(&mut cur).map_err(|_| ChannelReactionEnvelopeError::Truncated)?);
    if count == 0 || count > CHANNEL_REACTION_ENVELOPE_MAX_ENTRIES {
        return Err(ChannelReactionEnvelopeError::Count);
    }
    let mut out: Vec<Stamped<ChannelReactionEntry>> = Vec::with_capacity(count);
    for _ in 0..count {
        let hlc = take_u64(&mut cur).map_err(|_| ChannelReactionEnvelopeError::Truncated)?;
        let value = ChannelReactionEntry::take_body(&mut cur)
            .map_err(|_| ChannelReactionEnvelopeError::Truncated)?;
        if let Some(last) = out.last() {
            if last.value.sort_key() >= value.sort_key() {
                return Err(ChannelReactionEnvelopeError::NotCanonical);
            }
        }
        out.push(Stamped { hlc, value });
    }
    if !cur.is_empty() {
        return Err(ChannelReactionEnvelopeError::Trailing);
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
        emoji: &str,
        reacted: bool,
        hlc: u64,
    ) -> Stamped<ChannelReactionEntry> {
        Stamped {
            hlc,
            value: ChannelReactionEntry {
                channel_id: [channel; 16],
                post_cursor: cursor,
                emoji: emoji.to_string(),
                reacted,
                post_at_ms: 1_700_000_000_000 + cursor,
            },
        }
    }

    #[test]
    fn the_layout_is_pinned() {
        let wire = compose_channel_reaction_envelope(&[entry(1, 7, "\u{1F44D}", true, 9)])
            .unwrap_or_default();
        assert_eq!(CHANNEL_REACTION_ENVELOPE_DOMAIN_V1.len(), 31);
        assert_eq!(CHANNEL_REACTION_ENVELOPE_MIN_LEN, 31 + 2 + 42 + 1);
        assert_eq!(wire.len(), CHANNEL_REACTION_ENVELOPE_MIN_LEN + 3);
        assert_eq!(wire[31], 0x01);
        assert_eq!(wire[32], 1);
        assert_eq!(&wire[33..41], &9u64.to_be_bytes());
        assert_eq!(&wire[41..57], &[1u8; 16]);
        assert_eq!(&wire[57..65], &7u64.to_be_bytes());
        assert_eq!(wire[65], 4);
        assert_eq!(&wire[66..70], "\u{1F44D}".as_bytes());
        assert_eq!(wire[70], STATE_REACTED);
        assert!(is_channel_reaction_envelope(&wire));
        assert!(!is_channel_reaction_envelope(b"lakko/sync/device-ack/v1\0"));
    }

    #[test]
    fn compose_sorts_and_refuses_duplicates_bad_counts_and_bad_emoji() {
        let wire = compose_channel_reaction_envelope(&[
            entry(2, 1, "a", true, 1),
            entry(1, 9, "b", false, 2),
            entry(1, 9, "a", true, 3),
        ])
        .unwrap_or_default();
        let back = decode_channel_reaction_envelope(&wire).unwrap_or_default();
        assert_eq!(
            back,
            vec![
                entry(1, 9, "a", true, 3),
                entry(1, 9, "b", false, 2),
                entry(2, 1, "a", true, 1)
            ]
        );
        assert_eq!(
            compose_channel_reaction_envelope(&[
                entry(1, 1, "a", true, 1),
                entry(1, 1, "a", false, 2)
            ]),
            Err(ChannelReactionEnvelopeError::NotCanonical)
        );
        assert_eq!(
            compose_channel_reaction_envelope(&[]),
            Err(ChannelReactionEnvelopeError::Count)
        );
        let many: Vec<_> = (0..33).map(|c| entry(1, c, "a", true, 1)).collect();
        assert_eq!(
            compose_channel_reaction_envelope(&many),
            Err(ChannelReactionEnvelopeError::Count)
        );
        assert_eq!(
            compose_channel_reaction_envelope(&[entry(1, 1, "", true, 1)]),
            Err(ChannelReactionEnvelopeError::Truncated)
        );
        assert_eq!(
            compose_channel_reaction_envelope(&[entry(1, 1, &"x".repeat(65), true, 1)]),
            Err(ChannelReactionEnvelopeError::Truncated)
        );
    }

    #[test]
    fn decode_refuses_every_malformed_shape() {
        let wire = compose_channel_reaction_envelope(&[
            entry(1, 1, "a", true, 1),
            entry(1, 2, "a", true, 2),
        ])
        .unwrap_or_default();
        for cut in 0..wire.len() {
            assert!(
                decode_channel_reaction_envelope(&wire[..cut]).is_err(),
                "{cut}"
            );
        }
        let mut version = wire.clone();
        version[31] = 2;
        assert_eq!(
            decode_channel_reaction_envelope(&version),
            Err(ChannelReactionEnvelopeError::UnknownVersion)
        );
        let mut count = wire.clone();
        count[32] = 0;
        assert_eq!(
            decode_channel_reaction_envelope(&count),
            Err(ChannelReactionEnvelopeError::Count)
        );
        let mut trailing = wire.clone();
        trailing.push(0);
        assert_eq!(
            decode_channel_reaction_envelope(&trailing),
            Err(ChannelReactionEnvelopeError::Trailing)
        );
        // The first entry's state byte (header 33, then 8 + 16 + 8 + 1 + 1).
        let mut state = wire.clone();
        state[33 + 34] = 3;
        assert_eq!(
            decode_channel_reaction_envelope(&state),
            Err(ChannelReactionEnvelopeError::Truncated)
        );
        // The first entry's emoji length past the bound.
        let mut emoji = wire.clone();
        emoji[33 + 32] = 65;
        assert_eq!(
            decode_channel_reaction_envelope(&emoji),
            Err(ChannelReactionEnvelopeError::Truncated)
        );
        let entry_len = ENTRY_FIXED_LEN + 1;
        let mut swapped = wire[..33].to_vec();
        swapped.extend_from_slice(&wire[33 + entry_len..]);
        swapped.extend_from_slice(&wire[33..33 + entry_len]);
        assert_eq!(
            decode_channel_reaction_envelope(&swapped),
            Err(ChannelReactionEnvelopeError::NotCanonical)
        );
        assert_eq!(
            decode_channel_reaction_envelope(b"lakko/sync/device-ack/v1\0\x01"),
            Err(ChannelReactionEnvelopeError::WrongDomain)
        );
    }

    #[test]
    fn expiry_is_thirty_days_after_the_post() {
        let e = entry(1, 0, "a", true, 1).value;
        assert!(!e.expired_at(e.post_at_ms + CHANNEL_REACTION_RETENTION_MS));
        assert!(e.expired_at(e.post_at_ms + CHANNEL_REACTION_RETENTION_MS + 1));
        let late = ChannelReactionEntry {
            post_at_ms: u64::MAX,
            ..e
        };
        assert!(!late.expired_at(u64::MAX));
    }

    proptest! {
        #[test]
        fn decode_is_total_and_canonical(bytes in proptest::collection::vec(any::<u8>(), 0..900)) {
            let mut input = CHANNEL_REACTION_ENVELOPE_DOMAIN_V1.to_vec();
            input.extend_from_slice(&bytes);
            if let Ok(entries) = decode_channel_reaction_envelope(&input) {
                prop_assert!(input.len() <= CHANNEL_REACTION_ENVELOPE_MAX_LEN);
                prop_assert_eq!(compose_channel_reaction_envelope(&entries), Ok(input));
            }
        }

        #[test]
        fn round_trip(cursors in proptest::collection::btree_set(any::<u64>(), 1..=32), hlc in any::<u64>()) {
            let entries: Vec<_> = cursors.iter().map(|c| entry(3, *c, "\u{2764}\u{FE0F}", c % 2 == 0, hlc)).collect();
            let wire = compose_channel_reaction_envelope(&entries).unwrap_or_default();
            prop_assert!(wire.len() <= CHANNEL_REACTION_ENVELOPE_MAX_LEN);
            prop_assert_eq!(decode_channel_reaction_envelope(&wire), Ok(entries));
        }
    }
}
