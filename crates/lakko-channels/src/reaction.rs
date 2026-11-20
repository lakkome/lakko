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
//! map a label to an emoji without the seed. A row is `(post, writer_key,
//! label)`, unique; the writer key is derived from the account seed, the post
//! and the label ([`crate::write_key::ReactionWriteKey`]), so one account
//! gives one reaction of a kind and its add and its removal use one key.
//!
//! **HKDF, then keyed BLAKE3.** The key follows this crate's convention for
//! keys derived from an epoch seed (HKDF-SHA-256 with the seed as IKM, as the
//! post and member-card keys are), with an info label distinct from theirs;
//! the label is the ADR's keyed BLAKE3, truncated to 16 bytes. Two candidate
//! emoji colliding on one post's label has probability about `n² / 2^129`
//! for `n` candidates, and a label is not a MAC anyone relies on: it only
//! names a counter.
//!
//! ## Any emoji, as in a chat
//!
//! A channel post takes any reaction a chat message takes (maintainer,
//! 2026-11-02): the rule is `lakko_attachments::is_reaction_emoji`, the chat
//! rule, not list membership. A reader turns a label back into an emoji by
//! computing the labels of a **candidate list the caller supplies**
//! ([`ReactionKey::labels_for_post`]): the client's bundled emoji data set
//! plus its own recent emoji. An emoji a client does not know is dropped, as
//! is a row written by someone without the seed, so neither shows anything.
//! The cost of resolving one post is one keyed BLAKE3 per candidate.
//!
//! A channel owner may narrow what its posts take (the allowed-reactions
//! field of [`crate::meta`]). The service cannot enforce that (it sees labels
//! only); readers and the composer hide what is not allowed.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use std::collections::HashMap;

use lakko_attachments::is_reaction_emoji;
use lakko_crypto::hash;
use lakko_crypto::kdf::hkdf_sha256;
use lakko_crypto::secret::SecretBytes;

use crate::epoch::{EpochId, EPOCH_ID_LEN};
use crate::error::{ChannelError, Result};
use crate::seed::ContentKeySeed;
use crate::CHANNEL_ID_LEN;

/// HKDF info label of the reaction key, frozen at v1. Distinct from the
/// post, member-card and comment labels.
pub const REACTION_KEY_INFO_V1: &[u8] = b"lakko/channel/reaction-key/v1";

/// HKDF info label of the key for reactions on COMMENTS ([ADR-0087]
/// section 13a, UX 13.1: "A reaction on a comment works the same way"),
/// frozen at v1. A separate key, so a comment's labels and a post's are
/// unrelated even for the same cursor bytes.
pub const COMMENT_REACTION_KEY_INFO_V1: &[u8] = b"lakko/channel/comment-reaction-key/v1";

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
        self.keyed_label(&input)
    }

    /// `BLAKE3-keyed(reaction_key, input)[..16]`: the one keyed hash every
    /// label under this key is. Shared with the channel poll option labels
    /// ([`crate::poll`], ADR-0092 section 4.3). The reaction rule is a byte
    /// length only, so a crafted "emoji" can equal an option label's input;
    /// that buys nothing, since votes and reactions are counted in separate
    /// tables at the service and resolved against separate candidate lists.
    pub(crate) fn keyed_label(&self, input: &[u8]) -> ReactionLabel {
        let digest = hash::blake3_keyed(self.0.as_bytes(), input);
        let mut label = [0u8; REACTION_LABEL_LEN];
        label.copy_from_slice(&digest[..REACTION_LABEL_LEN]);
        label
    }

    /// The label of `emoji` under the post at `post_cursor`.
    ///
    /// # Errors
    ///
    /// [`ChannelError::InvalidReactionEmoji`] when `emoji` fails the chat
    /// reaction rule (`lakko_attachments::is_reaction_emoji`).
    pub fn label(&self, post_cursor: u64, emoji: &str) -> Result<ReactionLabel> {
        if !is_reaction_emoji(emoji) {
            return Err(ChannelError::InvalidReactionEmoji);
        }
        Ok(self.raw_label(post_cursor, emoji))
    }

    /// The labels of `candidates` under the post at `post_cursor`, for
    /// resolving the post's counts. A candidate that fails the chat rule, or
    /// repeats an earlier one, is skipped; the order of the rest is kept.
    #[must_use]
    pub fn labels_for_post<S: AsRef<str>>(
        &self,
        post_cursor: u64,
        candidates: &[S],
    ) -> ReactionLabels {
        ReactionLabels::build(candidates, |c| self.raw_label(post_cursor, c))
    }
}

/// The key reaction labels on the COMMENTS of one epoch's posts are made
/// with:
///
/// ```text
/// comment_reaction_key = HKDF-SHA-256(ikm  = the post's epoch seed,
///                                     salt = channel_id(16) ‖ epoch_id(4, BE),
///                                     info = "lakko/channel/comment-reaction-key/v1")
/// label = BLAKE3-keyed(comment_reaction_key,
///                      post_cursor(8, BE) ‖ comment_cursor(8, BE) ‖ emoji)[..16]
/// ```
///
/// The key differs from [`ReactionKey`]'s, so a label on a comment is never
/// a label on a post. The label folds in the post and the comment, so the
/// service cannot compare labels between comments. Zeroed on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct CommentReactionKey(SecretBytes<32>);

impl core::fmt::Debug for CommentReactionKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("CommentReactionKey(..)")
    }
}

impl CommentReactionKey {
    /// The comment-reaction key of `channel_id` at `epoch`, from the seed of
    /// the epoch the commented post was sealed in.
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
        let key = hkdf_sha256::<32>(
            epoch_seed.as_inner_bytes(),
            &salt,
            COMMENT_REACTION_KEY_INFO_V1,
        )
        .map_err(|_| ChannelError::KeyDerivation)?;
        Ok(Self(key))
    }

    fn raw_label(&self, post_cursor: u64, comment_cursor: u64, emoji: &str) -> ReactionLabel {
        let mut input = Vec::with_capacity(16 + emoji.len());
        input.extend_from_slice(&post_cursor.to_be_bytes());
        input.extend_from_slice(&comment_cursor.to_be_bytes());
        input.extend_from_slice(emoji.as_bytes());
        let digest = hash::blake3_keyed(self.0.as_bytes(), &input);
        let mut label = [0u8; REACTION_LABEL_LEN];
        label.copy_from_slice(&digest[..REACTION_LABEL_LEN]);
        label
    }

    /// The label of `emoji` on the comment `comment_cursor` under the post
    /// `post_cursor`.
    ///
    /// # Errors
    ///
    /// [`ChannelError::InvalidReactionEmoji`] when `emoji` fails the chat
    /// reaction rule.
    pub fn label(
        &self,
        post_cursor: u64,
        comment_cursor: u64,
        emoji: &str,
    ) -> Result<ReactionLabel> {
        if !is_reaction_emoji(emoji) {
            return Err(ChannelError::InvalidReactionEmoji);
        }
        Ok(self.raw_label(post_cursor, comment_cursor, emoji))
    }

    /// The labels of `candidates` on one comment, for resolving its counts;
    /// the rules of [`ReactionKey::labels_for_post`].
    #[must_use]
    pub fn labels_for_comment<S: AsRef<str>>(
        &self,
        post_cursor: u64,
        comment_cursor: u64,
        candidates: &[S],
    ) -> ReactionLabels {
        ReactionLabels::build(candidates, |c| {
            self.raw_label(post_cursor, comment_cursor, c)
        })
    }
}

/// The labels of one post's candidate emoji.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReactionLabels {
    /// The kept candidates, in the caller's order.
    emoji: Vec<String>,
    /// Label → index into `emoji`.
    by_label: HashMap<ReactionLabel, usize>,
}

impl ReactionLabels {
    /// Keep each candidate that passes the chat rule and whose label is new.
    fn build<S: AsRef<str>>(candidates: &[S], label_of: impl Fn(&str) -> ReactionLabel) -> Self {
        let mut emoji: Vec<String> = Vec::with_capacity(candidates.len());
        let mut by_label: HashMap<ReactionLabel, usize> = HashMap::with_capacity(candidates.len());
        for c in candidates {
            let c = c.as_ref();
            if !is_reaction_emoji(c) {
                continue;
            }
            let label = label_of(c);
            if by_label.contains_key(&label) {
                continue;
            }
            by_label.insert(label, emoji.len());
            emoji.push(c.to_string());
        }
        Self { emoji, by_label }
    }

    /// The emoji `label` stands for under this post, or `None` for a label no
    /// candidate makes (dropped by the reader).
    #[must_use]
    pub fn resolve(&self, label: &ReactionLabel) -> Option<&str> {
        self.by_label.get(label).map(|&i| self.emoji[i].as_str())
    }

    /// Turn the service's `{label: count}` for this post into `(emoji,
    /// count)` pairs in candidate order: unknown labels dropped, a label
    /// counted twice summed (saturating), zero counts left out.
    #[must_use]
    pub fn resolve_counts(&self, counts: &[(ReactionLabel, u64)]) -> Vec<(String, u64)> {
        let mut totals = vec![0u64; self.emoji.len()];
        for (label, count) in counts {
            if let Some(&i) = self.by_label.get(label) {
                totals[i] = totals[i].saturating_add(*count);
            }
        }
        totals
            .iter()
            .zip(&self.emoji)
            .filter(|(n, _)| **n > 0)
            .map(|(n, e)| (e.clone(), *n))
            .collect()
    }

    /// How many candidates were kept.
    #[must_use]
    pub fn len(&self) -> usize {
        self.emoji.len()
    }

    /// `true` when no candidate was kept.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.emoji.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANNEL: [u8; CHANNEL_ID_LEN] = [0x0C; CHANNEL_ID_LEN];
    const THUMBS: &str = "\u{1F44D}";
    const HEART: &str = "\u{2764}\u{FE0F}";
    const FAMILY: &str = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";

    fn key(seed: u8, epoch: u32) -> ReactionKey {
        ReactionKey::derive(
            &ContentKeySeed::from_bytes_for_test([seed; 32]),
            &CHANNEL,
            EpochId::new(epoch),
        )
        .unwrap()
    }

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
        assert_eq!(format!("{k:?}"), "ReactionKey(..)");
    }

    #[test]
    fn a_comment_label_is_the_documented_derivation_and_never_a_post_label() {
        let seed = ContentKeySeed::from_bytes_for_test([7; 32]);
        let mut salt = CHANNEL.to_vec();
        salt.extend_from_slice(&3u32.to_be_bytes());
        let raw: SecretBytes<32> =
            hkdf_sha256(&[7; 32], &salt, b"lakko/channel/comment-reaction-key/v1").unwrap();
        let mut input = 9u64.to_be_bytes().to_vec();
        input.extend_from_slice(&4u64.to_be_bytes());
        input.extend_from_slice(THUMBS.as_bytes());
        let expected = &hash::blake3_keyed(raw.as_bytes(), &input)[..16];
        let ck = CommentReactionKey::derive(&seed, &CHANNEL, EpochId::new(3)).unwrap();
        let label = ck.label(9, 4, THUMBS).unwrap();
        assert_eq!(&label[..], expected);
        // Not the post's label, not another comment's, not another post's.
        assert_ne!(label, key(7, 3).label(9, THUMBS).unwrap());
        assert_ne!(label, ck.label(9, 5, THUMBS).unwrap());
        assert_ne!(label, ck.label(10, 4, THUMBS).unwrap());
        assert!(matches!(
            ck.label(9, 4, ""),
            Err(ChannelError::InvalidReactionEmoji)
        ));
        let labels = ck.labels_for_comment(9, 4, &[THUMBS, HEART, "", THUMBS]);
        assert_eq!(labels.len(), 2);
        assert_eq!(labels.resolve(&label), Some(THUMBS));
        assert_eq!(format!("{ck:?}"), "CommentReactionKey(..)");
    }

    /// The chat rule: any non-empty emoji text of at most 64 bytes, a ZWJ
    /// sequence included; nothing else.
    #[test]
    fn the_chat_rule_decides_what_has_a_label() {
        let k = key(7, 3);
        assert!(k.label(9, FAMILY).is_ok());
        assert!(k.label(9, "\u{2764}").is_ok());
        assert!(matches!(
            k.label(9, ""),
            Err(ChannelError::InvalidReactionEmoji)
        ));
        assert!(matches!(
            k.label(9, &"x".repeat(65)),
            Err(ChannelError::InvalidReactionEmoji)
        ));
    }

    #[test]
    fn a_label_resolves_only_against_a_candidate_list_that_holds_its_emoji() {
        let k = key(7, 3);
        let candidates = [THUMBS, HEART, FAMILY, "", THUMBS];
        let table = k.labels_for_post(9, &candidates);
        // The empty candidate and the repeat are skipped.
        assert_eq!(table.len(), 3);
        for e in [THUMBS, HEART, FAMILY] {
            assert_eq!(table.resolve(&k.label(9, e).unwrap()), Some(e));
        }
        // An emoji this client does not know, another post's label, junk.
        assert_eq!(table.resolve(&k.label(9, "\u{1F525}").unwrap()), None);
        assert_eq!(table.resolve(&k.label(10, THUMBS).unwrap()), None);
        assert_eq!(table.resolve(&[0u8; 16]), None);
        assert!(k.labels_for_post::<&str>(9, &[]).is_empty());
    }

    #[test]
    fn counts_resolve_in_candidate_order_and_junk_is_dropped() {
        let k = key(7, 3);
        let table = k.labels_for_post(9, &[THUMBS, HEART, FAMILY]);
        let counts = vec![
            (k.label(9, HEART).unwrap(), 4),
            ([0xEE; 16], 1_000),
            (k.label(9, THUMBS).unwrap(), 2),
            (k.label(9, THUMBS).unwrap(), u64::MAX),
            (k.label(9, FAMILY).unwrap(), 0),
        ];
        assert_eq!(
            table.resolve_counts(&counts),
            vec![(THUMBS.to_string(), u64::MAX), (HEART.to_string(), 4)]
        );
        assert!(table.resolve_counts(&[]).is_empty());
    }
}
