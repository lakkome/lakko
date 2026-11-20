// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for a channel's comments and reactions (ADR-0087 section
//! 13a): the ban list publishers post, comments sealed under a post's epoch
//! seed, reaction labels the service counts without reading, and the
//! per-write keys whose proofs a subscriber's write carries.
//!
//! Mirror of `crates/lakko-ffi-wasm/src/channel_feedback.rs`. A subscriber
//! has no MLS leaf: every write is a row at `services/channels`, signed by a
//! key of its own (`ReactionWriteKey` / `CommentWriteKey`) so the service
//! cannot tell two rows came from one person. Clock and nonce of every proof
//! are minted here; a proof header is good for one request.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use lakko_channels::ban_list::{
    ban_list_is_newer as rust_ban_list_is_newer, compose_ban_list as rust_compose_ban_list,
    decode_ban_list as rust_decode_ban_list, BanList, BanListError, BAN_LIST_MAX_KEYS,
};
use lakko_channels::comment::{
    check_comment_text as rust_check_comment_text, open_comment as rust_open_comment,
    seal_comment as rust_seal_comment, Comment, CommentAuthor, CommentError, CommentPlace,
    COMMENT_TEXT_MAX_BYTES,
};
use lakko_channels::owner::OWNER_PROOF_NONCE_LEN;
use lakko_channels::reaction::{
    CommentReactionKey, ReactionKey, ReactionLabel, REACTION_LABEL_LEN,
};
use lakko_channels::write_key::{
    new_comment_nonce as rust_new_comment_nonce, CommentReactionWriteKey, CommentWriteKey,
    ReactionWriteKey, COMMENT_NONCE_LEN, WRITE_PROOF_HEADER_NAME,
};
use lakko_channels::{EpochId, CHANNEL_ID_LEN};

use crate::channels::{channel_id_from_vec, content_seed};
use crate::contacts::FfiAvatarRef;
use crate::{FfiError, Identity};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn fresh_nonce() -> [u8; OWNER_PROOF_NONCE_LEN] {
    let mut nonce = [0u8; OWNER_PROOF_NONCE_LEN];
    lakko_crypto::random::fill(&mut nonce);
    nonce
}

fn fixed<const N: usize>(what: &str, bytes: &[u8]) -> Result<[u8; N], FfiError> {
    <[u8; N]>::try_from(bytes).map_err(|_| {
        FfiError::Channels(format!(
            "badLength: {what} must be {N} bytes, got {}",
            bytes.len()
        ))
    })
}

fn len_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

// ============================================================================
// Ban list
// ============================================================================

/// A channel's ban list, as a publisher posted it: comment author keys whose
/// comments readers hide. Carried as the plaintext of an ordinary post;
/// `classifyChannelPostPlaintext` returns it as kind `BanList`.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiBanList {
    /// Publisher-clock UNIX milliseconds. Ordered by [`ban_list_is_newer`].
    pub updated_at_ms: u64,
    /// The banned 32-byte comment author keys, oldest first, no repeat.
    pub authors: Vec<Vec<u8>>,
}

impl From<BanList> for FfiBanList {
    fn from(b: BanList) -> Self {
        Self {
            updated_at_ms: b.updated_at_ms,
            authors: b.authors.iter().map(|a| a.to_vec()).collect(),
        }
    }
}

fn ban_list_error(e: BanListError) -> FfiError {
    FfiError::Channels(format!("banList.{}: {e}", e.code()))
}

/// Compose a ban list: the PLAINTEXT to hand to `composeChannelPost`.
///
/// # Errors
///
/// [`FfiError::Channels`] for a key that is not 32 bytes, or
/// `banList.<code>` for a list the decoder would refuse.
#[uniffi::export]
pub fn compose_ban_list(list: FfiBanList) -> Result<Vec<u8>, FfiError> {
    let authors = list
        .authors
        .iter()
        .map(|a| fixed::<32>("author key", a))
        .collect::<Result<Vec<_>, _>>()?;
    rust_compose_ban_list(&BanList {
        updated_at_ms: list.updated_at_ms,
        authors,
    })
    .map_err(ban_list_error)
}

/// Strictly decode a ban list.
///
/// # Errors
///
/// [`FfiError::Channels`] with `banList.<code>` first.
#[uniffi::export]
pub fn decode_ban_list(plaintext: Vec<u8>) -> Result<FfiBanList, FfiError> {
    rust_decode_ban_list(&plaintext)
        .map(Into::into)
        .map_err(ban_list_error)
}

/// `true` when a list stamped `incoming_updated_at_ms` replaces the stored
/// one for the same channel. Strictly greater.
#[uniffi::export]
#[must_use]
pub fn ban_list_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    rust_ban_list_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
}

/// The most keys one list holds.
#[uniffi::export]
#[must_use]
pub fn ban_list_max_keys() -> u32 {
    len_u32(BAN_LIST_MAX_KEYS)
}

// ============================================================================
// Comments
// ============================================================================

/// What a comment says.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiComment {
    /// The name the author typed (the display-name rule).
    pub name: String,
    /// The author's picture: a profile still reference, or `None`. Its keys
    /// become readable by every subscriber.
    pub avatar: Option<FfiAvatarRef>,
    /// The comment's text (see [`check_comment_text`]).
    pub text: String,
    /// The server cursor of the comment this one answers, or `None`.
    pub reply_to: Option<u64>,
}

/// A comment opened and its author signature verified.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiOpenedComment {
    /// The author's 32-byte per-channel key: what a ban list names.
    pub author_key: Vec<u8>,
    /// What it says.
    pub comment: FfiComment,
}

fn comment_error(e: CommentError) -> FfiError {
    FfiError::Channels(format!("comment.{}: {e}", e.code()))
}

/// Check a comment's text the way sealing does. `Ok` for acceptable text.
///
/// # Errors
///
/// [`FfiError::Channels`] with `commentText.<code>` first.
#[uniffi::export]
pub fn check_comment_text(text: String) -> Result<(), FfiError> {
    rust_check_comment_text(&text)
        .map_err(|e| FfiError::Channels(format!("commentText.{}: {e}", e.code())))
}

/// The longest comment text in bytes.
#[uniffi::export]
#[must_use]
pub fn comment_text_max_bytes() -> u32 {
    len_u32(COMMENT_TEXT_MAX_BYTES)
}

/// The 32-byte comment author key of `account_identity` in `channel_id`:
/// what a ban list names. Every device of the account derives the same one.
///
/// # Errors
///
/// [`FfiError::Channels`] when `channel_id` is not 16 bytes.
#[uniffi::export]
pub fn comment_author_key(
    account_identity: &Identity,
    channel_id: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let channel = channel_id_from_vec(channel_id)?;
    Ok(
        CommentAuthor::from_account_seed(&account_identity.inner.ed25519_seed_bytes(), &channel)
            .public_key()
            .to_vec(),
    )
}

struct Place {
    channel: [u8; CHANNEL_ID_LEN],
    writer: [u8; 32],
}

impl Place {
    fn new(channel_id: Vec<u8>, writer_key: &[u8]) -> Result<Self, FfiError> {
        Ok(Self {
            channel: channel_id_from_vec(channel_id)?,
            writer: fixed::<32>("writer key", writer_key)?,
        })
    }

    fn at(&self, epoch_id: u32, post_cursor: u64) -> CommentPlace<'_> {
        CommentPlace {
            channel_id: &self.channel,
            epoch: EpochId::new(epoch_id),
            post_cursor,
            writer_key: &self.writer,
        }
    }
}

/// Seal `comment` by `account_identity` under the post at `post_cursor`
/// (sealed in epoch `epoch_id`, whose 32-byte seed is `epoch_seed`), for the
/// comment write key `writer_key` (`CommentWriteKey.publicKey`).
///
/// # Errors
///
/// [`FfiError::Channels`] for a wrong-length argument or `comment.<code>`.
#[uniffi::export]
#[allow(clippy::too_many_arguments)] // one argument per bound field
pub fn seal_channel_comment(
    account_identity: &Identity,
    epoch_seed: Vec<u8>,
    channel_id: Vec<u8>,
    epoch_id: u32,
    post_cursor: u64,
    writer_key: Vec<u8>,
    comment: FfiComment,
) -> Result<Vec<u8>, FfiError> {
    let seed = content_seed(&epoch_seed)?;
    let place = Place::new(channel_id, &writer_key)?;
    let author = CommentAuthor::from_account_seed(
        &account_identity.inner.ed25519_seed_bytes(),
        &place.channel,
    );
    let core = Comment {
        name: comment.name,
        avatar: comment.avatar.map(TryInto::try_into).transpose()?,
        text: comment.text,
        reply_to: comment.reply_to,
    };
    rust_seal_comment(&seed, &place.at(epoch_id, post_cursor), &author, &core)
        .map_err(comment_error)
}

/// Open a comment row of the post at `post_cursor` (epoch `epoch_id`) under
/// that epoch's seed, for the row's `writer_key`, and verify its author
/// signature. Fail-closed: drop the row on any error.
///
/// # Errors
///
/// [`FfiError::Channels`] for a wrong-length argument or `comment.<code>`.
#[uniffi::export]
pub fn open_channel_comment(
    epoch_seed: Vec<u8>,
    channel_id: Vec<u8>,
    epoch_id: u32,
    post_cursor: u64,
    writer_key: Vec<u8>,
    sealed: Vec<u8>,
) -> Result<FfiOpenedComment, FfiError> {
    let seed = content_seed(&epoch_seed)?;
    let place = Place::new(channel_id, &writer_key)?;
    let opened = rust_open_comment(&seed, &place.at(epoch_id, post_cursor), &sealed)
        .map_err(comment_error)?;
    Ok(FfiOpenedComment {
        author_key: opened.author_key.to_vec(),
        comment: FfiComment {
            name: opened.comment.name,
            avatar: opened.comment.avatar.map(Into::into),
            text: opened.comment.text,
            reply_to: opened.comment.reply_to,
        },
    })
}

// ============================================================================
// Reaction labels
// ============================================================================

/// A reaction count resolved to its emoji.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiReactionCount {
    /// The emoji.
    pub emoji: String,
    /// How many rows carry it.
    pub count: u64,
}

/// A label count as the channels service returns it.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiReactionLabelCount {
    /// The 16-byte label.
    pub label: Vec<u8>,
    /// Its count.
    pub count: u64,
}

/// The reaction key of one channel epoch: it makes the labels of reactions
/// on posts sealed in that epoch, so the service counts reactions without
/// reading them.
#[derive(uniffi::Object)]
pub struct ChannelReactionKey {
    pub(crate) inner: ReactionKey,
}

#[uniffi::export]
impl ChannelReactionKey {
    /// The reaction key of `channel_id` at `epoch_id`, from that epoch's
    /// 32-byte seed (the epoch the reacted-to post was sealed in).
    ///
    /// # Errors
    ///
    /// [`FfiError::Channels`] for a wrong-length seed or channel id.
    #[uniffi::constructor]
    pub fn derive(
        epoch_seed: Vec<u8>,
        channel_id: Vec<u8>,
        epoch_id: u32,
    ) -> Result<Arc<Self>, FfiError> {
        let seed = content_seed(&epoch_seed)?;
        let channel = channel_id_from_vec(channel_id)?;
        Ok(Arc::new(Self {
            inner: ReactionKey::derive(&seed, &channel, EpochId::new(epoch_id))?,
        }))
    }

    /// The 16-byte label of `emoji` under the post at `post_cursor`.
    ///
    /// # Errors
    ///
    /// [`FfiError::Channels`] when `emoji` fails the chat reaction rule.
    pub fn label(&self, post_cursor: u64, emoji: String) -> Result<Vec<u8>, FfiError> {
        Ok(self.inner.label(post_cursor, &emoji)?.to_vec())
    }

    /// Resolve the service's label counts for the post at `post_cursor`
    /// against `candidates` (the emoji the reader can name: the allowed list,
    /// the quick row, recent ones). Unknown labels are dropped, a label
    /// counted twice is summed, zero counts are left out; candidate order.
    pub fn resolve_counts(
        &self,
        post_cursor: u64,
        candidates: Vec<String>,
        counts: Vec<FfiReactionLabelCount>,
    ) -> Vec<FfiReactionCount> {
        let labels = self.inner.labels_for_post(post_cursor, &candidates);
        let counts: Vec<(ReactionLabel, u64)> = counts
            .into_iter()
            .filter_map(|c| {
                <ReactionLabel>::try_from(c.label.as_slice())
                    .ok()
                    .map(|l| (l, c.count))
            })
            .collect();
        labels
            .resolve_counts(&counts)
            .into_iter()
            .map(|(emoji, count)| FfiReactionCount { emoji, count })
            .collect()
    }
}

/// The comment-reaction key of one channel epoch (ADR-0087 section 13a,
/// phase B): it makes the labels of reactions on comments under posts sealed
/// in that epoch. Separate from [`ChannelReactionKey`] so a comment's label
/// can never equal a post's.
#[derive(uniffi::Object)]
pub struct ChannelCommentReactionKey {
    inner: CommentReactionKey,
}

#[uniffi::export]
impl ChannelCommentReactionKey {
    /// The comment-reaction key of `channel_id` at `epoch_id`, from the
    /// 32-byte seed of the epoch the commented post was sealed in.
    ///
    /// # Errors
    ///
    /// [`FfiError::Channels`] for a wrong-length seed or channel id.
    #[uniffi::constructor]
    pub fn derive(
        epoch_seed: Vec<u8>,
        channel_id: Vec<u8>,
        epoch_id: u32,
    ) -> Result<Arc<Self>, FfiError> {
        let seed = content_seed(&epoch_seed)?;
        let channel = channel_id_from_vec(channel_id)?;
        Ok(Arc::new(Self {
            inner: CommentReactionKey::derive(&seed, &channel, EpochId::new(epoch_id))?,
        }))
    }

    /// The 16-byte label of `emoji` on the comment `comment_cursor` under
    /// the post at `post_cursor`.
    ///
    /// # Errors
    ///
    /// [`FfiError::Channels`] when `emoji` fails the chat reaction rule.
    pub fn label(
        &self,
        post_cursor: u64,
        comment_cursor: u64,
        emoji: String,
    ) -> Result<Vec<u8>, FfiError> {
        Ok(self
            .inner
            .label(post_cursor, comment_cursor, &emoji)?
            .to_vec())
    }

    /// Resolve the service's label counts for one comment against
    /// `candidates`; the rules of [`ChannelReactionKey::resolve_counts`].
    pub fn resolve_counts(
        &self,
        post_cursor: u64,
        comment_cursor: u64,
        candidates: Vec<String>,
        counts: Vec<FfiReactionLabelCount>,
    ) -> Vec<FfiReactionCount> {
        let labels = self
            .inner
            .labels_for_comment(post_cursor, comment_cursor, &candidates);
        let counts: Vec<(ReactionLabel, u64)> = counts
            .into_iter()
            .filter_map(|c| {
                <ReactionLabel>::try_from(c.label.as_slice())
                    .ok()
                    .map(|l| (l, c.count))
            })
            .collect();
        labels
            .resolve_counts(&counts)
            .into_iter()
            .map(|(emoji, count)| FfiReactionCount { emoji, count })
            .collect()
    }
}

/// Length of a reaction label: 16.
#[uniffi::export]
#[must_use]
pub fn channel_reaction_label_len() -> u32 {
    len_u32(REACTION_LABEL_LEN)
}

// ============================================================================
// Write keys and their proofs
// ============================================================================

/// The account's key for one reaction under one post: it signs adding and
/// taking off that reaction, nothing else.
#[derive(uniffi::Object)]
pub struct ChannelReactionWriteKey {
    inner: ReactionWriteKey,
}

#[uniffi::export]
impl ChannelReactionWriteKey {
    /// The key of reaction `label` (from `ChannelReactionKey.label`) on the
    /// post at `post_cursor` in `channel_id`, for `account_identity`.
    ///
    /// # Errors
    ///
    /// [`FfiError::Channels`] for a wrong-length channel id or label.
    #[uniffi::constructor]
    pub fn new(
        account_identity: &Identity,
        channel_id: Vec<u8>,
        post_cursor: u64,
        label: Vec<u8>,
    ) -> Result<Arc<Self>, FfiError> {
        let channel = channel_id_from_vec(channel_id)?;
        let label = fixed::<REACTION_LABEL_LEN>("label", &label)?;
        Ok(Arc::new(Self {
            inner: ReactionWriteKey::new(
                &account_identity.inner.ed25519_seed_bytes(),
                &channel,
                post_cursor,
                label,
            ),
        }))
    }

    /// The 32-byte public half the reaction row names.
    pub fn public_key(&self) -> Vec<u8> {
        self.inner.public_key().to_vec()
    }

    /// The `x-lakko-channel-write-proof` header value for adding the
    /// reaction.
    pub fn sign_add(&self) -> String {
        self.inner.sign_add(now_ms(), fresh_nonce()).encode()
    }

    /// The header value for taking the reaction off.
    pub fn sign_delete(&self) -> String {
        self.inner.sign_delete(now_ms(), fresh_nonce()).encode()
    }
}

/// The account's key for one reaction on one comment (ADR-0087 phase B): it
/// signs adding and taking off that reaction, nothing else
/// (`POST .../posts/{post}/comments/{comment}/reactions`, op 5;
/// `DELETE .../comments/{comment}/reactions/{writer}/{label}`, op 6).
#[derive(uniffi::Object)]
pub struct ChannelCommentReactionWriteKey {
    inner: CommentReactionWriteKey,
}

#[uniffi::export]
impl ChannelCommentReactionWriteKey {
    /// The key of reaction `label` (from `ChannelCommentReactionKey.label`)
    /// on the comment `comment_cursor` under the post at `post_cursor` in
    /// `channel_id`, for `account_identity`.
    ///
    /// # Errors
    ///
    /// [`FfiError::Channels`] for a wrong-length channel id or label.
    #[uniffi::constructor]
    pub fn new(
        account_identity: &Identity,
        channel_id: Vec<u8>,
        post_cursor: u64,
        comment_cursor: u64,
        label: Vec<u8>,
    ) -> Result<Arc<Self>, FfiError> {
        let channel = channel_id_from_vec(channel_id)?;
        let label = fixed::<REACTION_LABEL_LEN>("label", &label)?;
        Ok(Arc::new(Self {
            inner: CommentReactionWriteKey::new(
                &account_identity.inner.ed25519_seed_bytes(),
                &channel,
                post_cursor,
                comment_cursor,
                label,
            ),
        }))
    }

    /// The 32-byte public half the reaction row names.
    pub fn public_key(&self) -> Vec<u8> {
        self.inner.public_key().to_vec()
    }

    /// The `x-lakko-channel-write-proof` header value for adding the
    /// reaction.
    pub fn sign_add(&self) -> String {
        self.inner.sign_add(now_ms(), fresh_nonce()).encode()
    }

    /// The header value for taking the reaction off.
    pub fn sign_delete(&self) -> String {
        self.inner.sign_delete(now_ms(), fresh_nonce()).encode()
    }
}

/// The account's key for one comment under one post: it signs adding that
/// comment and deleting it, nothing else.
#[derive(uniffi::Object)]
pub struct ChannelCommentWriteKey {
    inner: CommentWriteKey,
}

#[uniffi::export]
impl ChannelCommentWriteKey {
    /// The key of the comment `comment_nonce` (16 bytes, from
    /// [`new_channel_comment_nonce`]; the account's other devices learn it
    /// from the own-comments register) on the post at `post_cursor`.
    ///
    /// # Errors
    ///
    /// [`FfiError::Channels`] for a wrong-length channel id or nonce.
    #[uniffi::constructor]
    pub fn new(
        account_identity: &Identity,
        channel_id: Vec<u8>,
        post_cursor: u64,
        comment_nonce: Vec<u8>,
    ) -> Result<Arc<Self>, FfiError> {
        let channel = channel_id_from_vec(channel_id)?;
        let nonce = fixed::<COMMENT_NONCE_LEN>("comment nonce", &comment_nonce)?;
        Ok(Arc::new(Self {
            inner: CommentWriteKey::new(
                &account_identity.inner.ed25519_seed_bytes(),
                &channel,
                post_cursor,
                &nonce,
            ),
        }))
    }

    /// The 32-byte public half the comment row names; also the `writer_key`
    /// the comment is sealed for.
    pub fn public_key(&self) -> Vec<u8> {
        self.inner.public_key().to_vec()
    }

    /// The header value for adding the comment whose sealed bytes are
    /// `sealed`.
    pub fn sign_add(&self, sealed: Vec<u8>) -> String {
        self.inner
            .sign_add(&sealed, now_ms(), fresh_nonce())
            .encode()
    }

    /// The header value for deleting the comment the service filed at
    /// `comment_cursor`.
    pub fn sign_delete(&self, comment_cursor: u64) -> String {
        self.inner
            .sign_delete(comment_cursor, now_ms(), fresh_nonce())
            .encode()
    }
}

/// A fresh random 16-byte comment nonce.
#[uniffi::export]
#[must_use]
pub fn new_channel_comment_nonce() -> Vec<u8> {
    rust_new_comment_nonce().to_vec()
}

/// The request header a subscriber write proof travels in.
#[uniffi::export]
#[must_use]
pub fn channel_write_proof_header_name() -> String {
    WRITE_PROOF_HEADER_NAME.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_channels::write_key::{
        verify_write_proof, SubscriberWrite, WriteProof, WriteStatement,
    };

    const CHANNEL: [u8; 16] = [0x0C; 16];

    fn identity() -> Identity {
        Identity::from_seed(vec![0x51; 64]).unwrap()
    }

    #[test]
    fn ban_list_round_trips_and_classifies() {
        let list = FfiBanList {
            updated_at_ms: 4,
            authors: vec![vec![1; 32], vec![2; 32]],
        };
        let wire = compose_ban_list(list.clone()).unwrap();
        assert_eq!(decode_ban_list(wire.clone()).unwrap(), list);
        let content = crate::channels::classify_channel_post_plaintext(wire);
        assert_eq!(content.kind, crate::channels::FfiChannelPostKind::BanList);
        assert_eq!(content.ban_list, Some(list));
        assert!(ban_list_is_newer(5, 4));
        assert!(compose_ban_list(FfiBanList {
            updated_at_ms: 1,
            authors: vec![vec![1; 31]],
        })
        .is_err());
    }

    #[test]
    fn comment_seals_opens_and_names_its_author() {
        let id = identity();
        let writer = ChannelCommentWriteKey::new(&id, CHANNEL.to_vec(), 9, vec![7; 16]).unwrap();
        let comment = FfiComment {
            name: "Alice".into(),
            avatar: None,
            text: "Nice post".into(),
            reply_to: Some(3),
        };
        let sealed = seal_channel_comment(
            &id,
            vec![0x22; 32],
            CHANNEL.to_vec(),
            2,
            9,
            writer.public_key(),
            comment.clone(),
        )
        .unwrap();
        let opened = open_channel_comment(
            vec![0x22; 32],
            CHANNEL.to_vec(),
            2,
            9,
            writer.public_key(),
            sealed.clone(),
        )
        .unwrap();
        assert_eq!(opened.comment, comment);
        assert_eq!(
            opened.author_key,
            comment_author_key(&id, CHANNEL.to_vec()).unwrap()
        );
        assert!(open_channel_comment(
            vec![0x22; 32],
            CHANNEL.to_vec(),
            2,
            10,
            writer.public_key(),
            sealed
        )
        .is_err());
        assert!(check_comment_text(String::new()).is_err());
        assert!(check_comment_text("ok".into()).is_ok());
    }

    #[test]
    fn reaction_labels_resolve_counts() {
        let key = ChannelReactionKey::derive(vec![0x22; 32], CHANNEL.to_vec(), 2).unwrap();
        let thumbs = key.label(9, "\u{1F44D}".into()).unwrap();
        assert_eq!(thumbs.len(), channel_reaction_label_len() as usize);
        let counts = key.resolve_counts(
            9,
            vec!["\u{1F44D}".into(), "\u{1F602}".into()],
            vec![
                FfiReactionLabelCount {
                    label: thumbs,
                    count: 4,
                },
                FfiReactionLabelCount {
                    label: vec![0; 16],
                    count: 99,
                },
            ],
        );
        assert_eq!(
            counts,
            vec![FfiReactionCount {
                emoji: "\u{1F44D}".into(),
                count: 4
            }]
        );
        assert!(key.label(9, String::new()).is_err());
    }

    #[test]
    fn write_keys_sign_proofs_the_service_accepts() {
        let id = identity();
        let label = vec![5_u8; 16];
        let reaction =
            ChannelReactionWriteKey::new(&id, CHANNEL.to_vec(), 9, label.clone()).unwrap();
        let header = reaction.sign_add();
        let proof = WriteProof::parse(&header).unwrap();
        let writer: [u8; 32] = reaction.public_key().try_into().unwrap();
        let statement = WriteStatement {
            channel_id: &CHANNEL,
            post_cursor: 9,
            writer_key: &writer,
            write: SubscriberWrite::AddReaction {
                label: label.try_into().unwrap(),
            },
        };
        verify_write_proof(&proof, &statement, proof.unix_ms).unwrap();
        let comment =
            ChannelCommentWriteKey::new(&id, CHANNEL.to_vec(), 9, new_channel_comment_nonce())
                .unwrap();
        assert_ne!(comment.public_key(), reaction.public_key());
        assert!(WriteProof::parse(&comment.sign_delete(3)).is_ok());
        assert_eq!(
            channel_write_proof_header_name(),
            "x-lakko-channel-write-proof"
        );
    }
}
