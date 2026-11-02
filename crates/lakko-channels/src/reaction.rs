// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Reaction labels under a channel post ([ADR-0087] section 13a,
//! "Reactions").
//!
//! A subscriber holds a read key and no MLS leaf, so a reaction cannot be a
//! group message; it is a row at the channel service, and readers get a
//! count per label. The service holds no emoji:
//!
//! ```text
//! reaction_key = HKDF-SHA-256(ikm  = the post's epoch seed,
//!                             salt = channel_id(16) ‖ epoch_id(4, BE),
//!                             info = "lakko/channel/reaction-key/v1")
//! label        = BLAKE3-keyed(reaction_key, post_cursor(8, BE) ‖ emoji)[..16]
//! ```
//!
//! The label folds in the post, so the service cannot compare labels between
//! posts (one emoji has a different label under every post), and it cannot
//! map a label to an emoji without the seed. A row is `(post, post_key,
//! label)`, unique, so one account gives one reaction of a kind
//! ([`crate::post_key`]).
//!
//! **HKDF, then keyed BLAKE3.** The key follows this crate's convention for
//! keys derived from an epoch seed (HKDF-SHA-256 with the seed as IKM, as the
//! post and member-card keys are), with an info label distinct from theirs;
//! the label is the ADR's keyed BLAKE3, truncated to 16 bytes. Truncation is
//! safe here: a collision between two of the 64 labels of one post has
//! probability about `64² / 2^129`, and a label is not a MAC anyone relies
//! on: it only names a counter.
//!
//! ## The fixed list
//!
//! A reader turns a label back into an emoji by computing the 64 labels of
//! [`lakko_ui_rules::reaction::CHANNEL_REACTION_EMOJI`] for the post
//! ([`ReactionKey::labels_for_post`]) and looking each counted label up; an
//! unknown label is dropped, so a row written by someone without the seed
//! shows nothing. [`ReactionKey::label`] refuses an emoji outside the list,
//! since its label could never be read.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use lakko_crypto::hash;
use lakko_crypto::kdf::hkdf_sha256;
use lakko_crypto::secret::SecretBytes;
use lakko_ui_rules::reaction::{
    channel_reaction_index, CHANNEL_REACTION_EMOJI, CHANNEL_REACTION_EMOJI_COUNT,
};

use crate::epoch::{EpochId, EPOCH_ID_LEN};
use crate::error::{ChannelError, Result};
use crate::seed::ContentKeySeed;
use crate::CHANNEL_ID_LEN;

/// HKDF info label of the reaction key, frozen at v1. Distinct from the
/// post, member-card and comment labels.
pub const REACTION_KEY_INFO_V1: &[u8] = b"lakko/channel/reaction-key/v1";

/// Length of a reaction label.
pub const REACTION_LABEL_LEN: usize = 16;

/// A reaction label.
pub type ReactionLabel = [u8; REACTION_LABEL_LEN];

/// The key reaction labels of one epoch are made with. Zeroed on drop; the
/// bytes never leave this type.
#[derive(Clone, PartialEq, Eq)]
pub struct ReactionKey(SecretBytes<32>);

impl core::fmt::Debug for ReactionKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ReactionKey(..)")
    }
}

impl ReactionKey {
    /// The reaction key of `channel_id` at `epoch`, from that epoch's seed:
    /// the seed of the epoch the reacted-to post was sealed in.
    ///
    /// # Errors
    ///
    /// [`ChannelError::KeyDerivation`] (impossible with these lengths).
    pub fn derive(
        epoch_seed: &ContentKeySeed,
        channel_id: &[u8; CHANNEL_ID_LEN],
        epoch: EpochId,
    ) -> Result<Self> {
        let mut salt = [0u8; CHANNEL_ID_LEN + EPOCH_ID_LEN];
        salt[..CHANNEL_ID_LEN].copy_from_slice(channel_id);
        salt[CHANNEL_ID_LEN..].copy_from_slice(&epoch.to_be_bytes());
        let key = hkdf_sha256::<32>(epoch_seed.as_inner_bytes(), &salt, REACTION_KEY_INFO_V1)
            .map_err(|_| ChannelError::KeyDerivation)?;
        Ok(Self(key))
    }

    fn raw_label(&self, post_cursor: u64, emoji: &str) -> ReactionLabel {
        let mut input = Vec::with_capacity(8 + emoji.len());
        input.extend_from_slice(&post_cursor.to_be_bytes());
        input.extend_from_slice(emoji.as_bytes());
        let digest = hash::blake3_keyed(self.0.as_bytes(), &input);
        let mut label = [0u8; REACTION_LABEL_LEN];
        label.copy_from_slice(&digest[..REACTION_LABEL_LEN]);
        label
    }

    /// The label of `emoji` under the post at `post_cursor`.
    ///
    /// # Errors
    ///
    /// [`ChannelError::UnknownReactionEmoji`] when `emoji` is not in the
    /// fixed list (an exact byte match).
    pub fn label(&self, post_cursor: u64, emoji: &str) -> Result<ReactionLabel> {
        if channel_reaction_index(emoji).is_none() {
            return Err(ChannelError::UnknownReactionEmoji);
        }
        Ok(self.raw_label(post_cursor, emoji))
    }

    /// The labels of every listed emoji under the post at `post_cursor`, for
    /// resolving the post's counts.
    #[must_use]
    pub fn labels_for_post(&self, post_cursor: u64) -> ReactionLabels {
        let mut labels = [[0u8; REACTION_LABEL_LEN]; CHANNEL_REACTION_EMOJI_COUNT];
        for (slot, emoji) in labels.iter_mut().zip(CHANNEL_REACTION_EMOJI) {
            *slot = self.raw_label(post_cursor, emoji);
        }
        ReactionLabels { labels }
    }
}

/// The 64 labels of one post, in the order of the fixed list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReactionLabels {
    labels: [ReactionLabel; CHANNEL_REACTION_EMOJI_COUNT],
}

impl ReactionLabels {
    /// The emoji `label` stands for under this post, or `None` for a label
    /// no listed emoji makes (dropped by every reader).
    #[must_use]
    pub fn resolve(&self, label: &ReactionLabel) -> Option<&'static str> {
        self.labels
            .iter()
            .position(|l| l == label)
            .map(|i| CHANNEL_REACTION_EMOJI[i])
    }

    /// Turn the service's `{label: count}` for this post into `(emoji,
    /// count)` pairs in list order: unknown labels dropped, a label counted
    /// twice summed (saturating), zero counts left out.
    #[must_use]
    pub fn resolve_counts(&self, counts: &[(ReactionLabel, u64)]) -> Vec<(&'static str, u64)> {
        let mut totals = [0u64; CHANNEL_REACTION_EMOJI_COUNT];
        for (label, count) in counts {
            if let Some(i) = self.labels.iter().position(|l| l == label) {
                totals[i] = totals[i].saturating_add(*count);
            }
        }
        totals
            .iter()
            .zip(CHANNEL_REACTION_EMOJI)
            .filter(|(n, _)| **n > 0)
            .map(|(n, e)| (e, *n))
            .collect()
    }

    /// The labels, in the order of the fixed list.
    #[must_use]
    pub const fn labels(&self) -> &[ReactionLabel; CHANNEL_REACTION_EMOJI_COUNT] {
        &self.labels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANNEL: [u8; CHANNEL_ID_LEN] = [0x0C; CHANNEL_ID_LEN];

    fn key(seed: u8, epoch: u32) -> ReactionKey {
        ReactionKey::derive(
            &ContentKeySeed::from_bytes_for_test([seed; 32]),
            &CHANNEL,
            EpochId::new(epoch),
        )
        .unwrap()
    }

    const THUMBS: &str = "\u{1F44D}";

    #[test]
    fn the_label_is_the_documented_derivation() {
        let seed = ContentKeySeed::from_bytes_for_test([7; 32]);
        let mut salt = CHANNEL.to_vec();
        salt.extend_from_slice(&3u32.to_be_bytes());
        let raw: SecretBytes<32> =
            hkdf_sha256(&[7; 32], &salt, b"lakko/channel/reaction-key/v1").unwrap();
        let mut input = 9u64.to_be_bytes().to_vec();
        input.extend_from_slice(THUMBS.as_bytes());
        let expected = &hash::blake3_keyed(raw.as_bytes(), &input)[..16];
        let k = ReactionKey::derive(&seed, &CHANNEL, EpochId::new(3)).unwrap();
        assert_eq!(&k.label(9, THUMBS).unwrap()[..], expected);
    }

    /// One emoji has a different label under every post, every epoch seed
    /// and every channel, so the service cannot compare labels.
    #[test]
    fn labels_differ_between_posts_seeds_and_channels() {
        let k = key(7, 3);
        let a = k.label(9, THUMBS).unwrap();
        assert_ne!(a, k.label(10, THUMBS).unwrap());
        assert_ne!(a, key(8, 3).label(9, THUMBS).unwrap());
        assert_ne!(a, key(7, 4).label(9, THUMBS).unwrap());
        let other = ReactionKey::derive(
            &ContentKeySeed::from_bytes_for_test([7; 32]),
            &[0x0D; CHANNEL_ID_LEN],
            EpochId::new(3),
        )
        .unwrap();
        assert_ne!(a, other.label(9, THUMBS).unwrap());
        assert!(!format!("{k:?}").contains('7'));
    }

    #[test]
    fn an_emoji_outside_the_list_has_no_label() {
        let k = key(7, 3);
        assert!(matches!(
            k.label(9, "\u{2764}"),
            Err(ChannelError::UnknownReactionEmoji)
        ));
        assert!(matches!(
            k.label(9, ""),
            Err(ChannelError::UnknownReactionEmoji)
        ));
    }

    #[test]
    fn every_label_resolves_to_its_emoji_and_nothing_else_does() {
        let k = key(7, 3);
        let table = k.labels_for_post(9);
        let mut seen = std::collections::BTreeSet::new();
        for emoji in CHANNEL_REACTION_EMOJI {
            let label = k.label(9, emoji).unwrap();
            assert!(seen.insert(label), "two emoji share a label");
            assert_eq!(table.resolve(&label), Some(emoji));
        }
        // Another post's label, and junk, resolve to nothing.
        assert_eq!(table.resolve(&k.label(10, THUMBS).unwrap()), None);
        assert_eq!(table.resolve(&[0u8; 16]), None);
        assert_eq!(table.labels().len(), 64);
    }

    #[test]
    fn counts_resolve_in_list_order_and_junk_is_dropped() {
        let k = key(7, 3);
        let table = k.labels_for_post(9);
        let heart = CHANNEL_REACTION_EMOJI[1];
        let counts = vec![
            (k.label(9, heart).unwrap(), 4),
            ([0xEE; 16], 1_000),
            (k.label(9, THUMBS).unwrap(), 2),
            (k.label(9, THUMBS).unwrap(), u64::MAX),
            (k.label(9, CHANNEL_REACTION_EMOJI[5]).unwrap(), 0),
        ];
        assert_eq!(
            table.resolve_counts(&counts),
            vec![(THUMBS, u64::MAX), (heart, 4)]
        );
        assert!(table.resolve_counts(&[]).is_empty());
    }
}
