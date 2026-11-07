// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for a channel's comments and reactions (ADR-0087
//! section 13a): the ban list, comments sealed under a post's epoch seed,
//! reaction labels, and the per-write keys whose proofs a subscriber's write
//! carries.
//!
//! Mirror of `crates/lakko-ffi-uniffi/src/channel_feedback.rs`. Lists of
//! fixed-size keys cross as one flat concatenation (32 bytes each).
//!
//! # Errors
//!
//! Every fallible function throws a string `"<area>.<code>: <reason>"`.

use lakko_channels::ban_list::{
    ban_list_is_newer as rust_ban_list_is_newer, compose_ban_list as rust_compose_ban_list,
    decode_ban_list as rust_decode_ban_list, BanList, BanListError, BAN_LIST_MAX_KEYS,
};
use lakko_channels::comment::{
    check_comment_text as rust_check_comment_text, open_comment as rust_open_comment,
    seal_comment as rust_seal_comment, Comment, CommentAuthor, CommentError, CommentPlace,
    OpenedComment, COMMENT_TEXT_MAX_BYTES,
};
use lakko_channels::reaction::{
    CommentReactionKey, ReactionKey, ReactionLabel, REACTION_LABEL_LEN,
};
use lakko_channels::write_key::{
    new_comment_nonce as rust_new_comment_nonce, CommentReactionWriteKey, CommentWriteKey,
    ReactionWriteKey, COMMENT_NONCE_LEN, WRITE_PROOF_HEADER_NAME,
};
use lakko_channels::{EpochId, CHANNEL_ID_LEN};
use wasm_bindgen::prelude::*;

use crate::channel_access::{channel16, fixed, fresh_nonce, now_ms, seed32};
use crate::contacts::AvatarRef;
use crate::mls::Identity;

fn len_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn flat32(area: &str, what: &str, bytes: &[u8]) -> Result<Vec<[u8; 32]>, String> {
    if bytes.len() % 32 != 0 {
        return Err(format!(
            "{area}.badLength: {what} must be a concatenation of 32-byte keys"
        ));
    }
    Ok(bytes
        .chunks_exact(32)
        .filter_map(|c| <[u8; 32]>::try_from(c).ok())
        .collect())
}

// ============================================================================
// Ban list
// ============================================================================

/// A channel's ban list (ADR-0087 section 13a). Carried as the plaintext of
/// an ordinary post; `classifyChannelPostPlaintext` returns it as kind
/// `"ban-list"`.
#[wasm_bindgen(js_name = BanList)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WasmBanList {
    inner: BanList,
}

impl From<BanList> for WasmBanList {
    fn from(inner: BanList) -> Self {
        Self { inner }
    }
}

#[wasm_bindgen(js_class = BanList)]
impl WasmBanList {
    /// A list stamped `updatedAtMs` of the banned comment author keys,
    /// concatenated (32 bytes each, oldest first).
    ///
    /// # Errors
    ///
    /// Throws `banList.badLength: ...`.
    #[wasm_bindgen(constructor)]
    pub fn new(updated_at_ms: u64, authors: &[u8]) -> Result<WasmBanList, String> {
        Ok(Self {
            inner: BanList {
                updated_at_ms,
                authors: flat32("banList", "authors", authors)?,
            },
        })
    }

    /// Publisher-clock UNIX milliseconds.
    #[wasm_bindgen(getter, js_name = updatedAtMs)]
    #[must_use]
    pub fn updated_at_ms(&self) -> u64 {
        self.inner.updated_at_ms
    }

    /// The banned author keys, concatenated (32 bytes each).
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn authors(&self) -> Vec<u8> {
        self.inner.authors.concat()
    }

    /// Whether `authorKey` is banned.
    #[wasm_bindgen(js_name = isBanned)]
    #[must_use]
    pub fn is_banned(&self, author_key: &[u8]) -> bool {
        <[u8; 32]>::try_from(author_key).is_ok_and(|k| self.inner.is_banned(&k))
    }
}

fn ban_list_error(e: BanListError) -> String {
    format!("banList.{}: {e}", e.code())
}

/// Compose a ban list: the PLAINTEXT to hand to `composeChannelPost`.
///
/// # Errors
///
/// Throws `banList.<code>: ...`.
#[wasm_bindgen(js_name = composeBanList)]
pub fn compose_ban_list(list: &WasmBanList) -> Result<Vec<u8>, String> {
    rust_compose_ban_list(&list.inner).map_err(ban_list_error)
}

/// Strictly decode a ban list.
///
/// # Errors
///
/// Throws `banList.<code>: ...`.
#[wasm_bindgen(js_name = decodeBanList)]
pub fn decode_ban_list(plaintext: &[u8]) -> Result<WasmBanList, String> {
    rust_decode_ban_list(plaintext)
        .map(Into::into)
        .map_err(ban_list_error)
}

/// `true` when `incoming` replaces `stored` for the same channel.
#[wasm_bindgen(js_name = banListIsNewer)]
#[must_use]
pub fn ban_list_is_newer(incoming_updated_at_ms: u64, stored_updated_at_ms: u64) -> bool {
    rust_ban_list_is_newer(incoming_updated_at_ms, stored_updated_at_ms)
}

/// The most keys one list holds.
#[wasm_bindgen(js_name = banListMaxKeys)]
#[must_use]
pub fn ban_list_max_keys() -> u32 {
    len_u32(BAN_LIST_MAX_KEYS)
}

// ============================================================================
// Comments
// ============================================================================

/// A comment opened and its author signature verified.
#[wasm_bindgen(js_name = OpenedComment)]
pub struct WasmOpenedComment {
    inner: OpenedComment,
}

#[wasm_bindgen(js_class = OpenedComment)]
impl WasmOpenedComment {
    /// The author's 32-byte per-channel key: what a ban list names.
    #[wasm_bindgen(getter, js_name = authorKey)]
    #[must_use]
    pub fn author_key(&self) -> Vec<u8> {
        self.inner.author_key.to_vec()
    }

    /// The name the author typed.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn name(&self) -> String {
        self.inner.comment.name.clone()
    }

    /// The author's picture, or `undefined`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn avatar(&self) -> Option<AvatarRef> {
        self.inner
            .comment
            .avatar
            .clone()
            .map(|inner| AvatarRef { inner })
    }

    /// The comment's text.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn text(&self) -> String {
        self.inner.comment.text.clone()
    }

    /// The server cursor of the comment this one answers, or `undefined`.
    #[wasm_bindgen(getter, js_name = replyTo)]
    #[must_use]
    pub fn reply_to(&self) -> Option<u64> {
        self.inner.comment.reply_to
    }
}

fn comment_error(e: CommentError) -> String {
    format!("comment.{}: {e}", e.code())
}

/// Check a comment's text the way sealing does.
///
/// # Errors
///
/// Throws `commentText.<code>: ...`.
#[wasm_bindgen(js_name = checkCommentText)]
pub fn check_comment_text(text: &str) -> Result<(), String> {
    rust_check_comment_text(text).map_err(|e| format!("commentText.{}: {e}", e.code()))
}

/// The longest comment text in bytes.
#[wasm_bindgen(js_name = commentTextMaxBytes)]
#[must_use]
pub fn comment_text_max_bytes() -> u32 {
    len_u32(COMMENT_TEXT_MAX_BYTES)
}

/// The 32-byte comment author key of `accountIdentity` in `channelId`.
///
/// # Errors
///
/// Throws `comment.badLength: ...`.
#[wasm_bindgen(js_name = commentAuthorKey)]
pub fn comment_author_key(
    account_identity: &Identity,
    channel_id: &[u8],
) -> Result<Vec<u8>, String> {
    let channel = channel16("comment", channel_id)?;
    Ok(CommentAuthor::from_account_seed(
        &account_identity.mls_identity().ed25519_seed_bytes(),
        &channel,
    )
    .public_key()
    .to_vec())
}

fn place<'a>(
    channel: &'a [u8; CHANNEL_ID_LEN],
    epoch_id: u32,
    post_cursor: u64,
    writer: &'a [u8; 32],
) -> CommentPlace<'a> {
    CommentPlace {
        channel_id: channel,
        epoch: EpochId::new(epoch_id),
        post_cursor,
        writer_key: writer,
    }
}

/// Seal a comment by `accountIdentity` under the post at `postCursor`
/// (sealed in `epochId`, whose seed is `epochSeed`) for the comment write
/// key `writerKey` (`ChannelCommentWriteKey.publicKey`).
///
/// # Errors
///
/// Throws `comment.<code>: ...`.
#[wasm_bindgen(js_name = sealChannelComment)]
#[allow(clippy::too_many_arguments)] // one argument per bound field
pub fn seal_channel_comment(
    account_identity: &Identity,
    epoch_seed: &[u8],
    channel_id: &[u8],
    epoch_id: u32,
    post_cursor: u64,
    writer_key: &[u8],
    name: String,
    avatar: Option<AvatarRef>,
    text: String,
    reply_to: Option<u64>,
) -> Result<Vec<u8>, String> {
    let seed = seed32("comment", epoch_seed)?;
    let channel = channel16("comment", channel_id)?;
    let writer = fixed::<32>("comment", "writer key", writer_key)?;
    let author = CommentAuthor::from_account_seed(
        &account_identity.mls_identity().ed25519_seed_bytes(),
        &channel,
    );
    let comment = Comment {
        name,
        avatar: avatar.map(|a| a.inner),
        text,
        reply_to,
    };
    rust_seal_comment(
        &seed,
        &place(&channel, epoch_id, post_cursor, &writer),
        &author,
        &comment,
    )
    .map_err(comment_error)
}

/// Open a comment row and verify its author signature. Fail-closed.
///
/// # Errors
///
/// Throws `comment.<code>: ...`.
#[wasm_bindgen(js_name = openChannelComment)]
pub fn open_channel_comment(
    epoch_seed: &[u8],
    channel_id: &[u8],
    epoch_id: u32,
    post_cursor: u64,
    writer_key: &[u8],
    sealed: &[u8],
) -> Result<WasmOpenedComment, String> {
    let seed = seed32("comment", epoch_seed)?;
    let channel = channel16("comment", channel_id)?;
    let writer = fixed::<32>("comment", "writer key", writer_key)?;
    rust_open_comment(
        &seed,
        &place(&channel, epoch_id, post_cursor, &writer),
        sealed,
    )
    .map(|inner| WasmOpenedComment { inner })
    .map_err(comment_error)
}

// ============================================================================
// Reaction labels
// ============================================================================

/// The reaction key of one channel epoch.
#[wasm_bindgen]
pub struct ChannelReactionKey {
    inner: ReactionKey,
}

#[wasm_bindgen]
impl ChannelReactionKey {
    /// The reaction key of `channelId` at `epochId`, from that epoch's seed.
    ///
    /// # Errors
    ///
    /// Throws `reaction.<code>: ...`.
    #[wasm_bindgen]
    pub fn derive(
        epoch_seed: &[u8],
        channel_id: &[u8],
        epoch_id: u32,
    ) -> Result<ChannelReactionKey, String> {
        let seed = seed32("reaction", epoch_seed)?;
        let channel = channel16("reaction", channel_id)?;
        ReactionKey::derive(&seed, &channel, EpochId::new(epoch_id))
            .map(|inner| Self { inner })
            .map_err(|e| format!("reaction.keyDerivation: {e}"))
    }

    /// The 16-byte label of `emoji` under the post at `postCursor`.
    ///
    /// # Errors
    ///
    /// Throws `reaction.invalidEmoji: ...`.
    #[wasm_bindgen]
    pub fn label(&self, post_cursor: u64, emoji: &str) -> Result<Vec<u8>, String> {
        self.inner
            .label(post_cursor, emoji)
            .map(|l| l.to_vec())
            .map_err(|e| format!("reaction.invalidEmoji: {e}"))
    }

    /// Resolve the service's counts for the post at `postCursor`:
    /// `labels` is the labels concatenated (16 bytes each) and `counts` the
    /// count of each, in the same order. Returns the emoji that resolved,
    /// in candidate order; read each one's count with the same index of
    /// [`Self::resolve_count_values`].
    #[wasm_bindgen(js_name = resolveCountEmoji)]
    #[must_use]
    pub fn resolve_count_emoji(
        &self,
        post_cursor: u64,
        candidates: Vec<String>,
        labels: &[u8],
        counts: Vec<u64>,
    ) -> Vec<String> {
        self.resolve(post_cursor, &candidates, labels, &counts)
            .into_iter()
            .map(|(e, _)| e)
            .collect()
    }

    /// The counts that go with [`Self::resolve_count_emoji`], index for
    /// index.
    #[wasm_bindgen(js_name = resolveCountValues)]
    #[must_use]
    pub fn resolve_count_values(
        &self,
        post_cursor: u64,
        candidates: Vec<String>,
        labels: &[u8],
        counts: Vec<u64>,
    ) -> Vec<u64> {
        self.resolve(post_cursor, &candidates, labels, &counts)
            .into_iter()
            .map(|(_, n)| n)
            .collect()
    }
}

impl ChannelReactionKey {
    fn resolve(
        &self,
        post_cursor: u64,
        candidates: &[String],
        labels: &[u8],
        counts: &[u64],
    ) -> Vec<(String, u64)> {
        let pairs: Vec<(ReactionLabel, u64)> = labels
            .chunks_exact(REACTION_LABEL_LEN)
            .zip(counts.iter())
            .filter_map(|(l, n)| ReactionLabel::try_from(l).ok().map(|l| (l, *n)))
            .collect();
        self.inner
            .labels_for_post(post_cursor, candidates)
            .resolve_counts(&pairs)
    }
}

/// The comment-reaction key of one channel epoch (ADR-0087 section 13a,
/// phase B): labels of reactions on comments under posts sealed in that
/// epoch, never equal to a post's.
#[wasm_bindgen]
pub struct ChannelCommentReactionKey {
    inner: CommentReactionKey,
}

#[wasm_bindgen]
impl ChannelCommentReactionKey {
    /// The comment-reaction key of `channelId` at `epochId`, from the seed of
    /// the epoch the commented post was sealed in.
    ///
    /// # Errors
    ///
    /// Throws `reaction.<code>: ...`.
    #[wasm_bindgen]
    pub fn derive(
        epoch_seed: &[u8],
        channel_id: &[u8],
        epoch_id: u32,
    ) -> Result<ChannelCommentReactionKey, String> {
        let seed = seed32("reaction", epoch_seed)?;
        let channel = channel16("reaction", channel_id)?;
        CommentReactionKey::derive(&seed, &channel, EpochId::new(epoch_id))
            .map(|inner| Self { inner })
            .map_err(|e| format!("reaction.keyDerivation: {e}"))
    }

    /// The 16-byte label of `emoji` on the comment `commentCursor` under the
    /// post at `postCursor`.
    ///
    /// # Errors
    ///
    /// Throws `reaction.invalidEmoji: ...`.
    #[wasm_bindgen]
    pub fn label(
        &self,
        post_cursor: u64,
        comment_cursor: u64,
        emoji: &str,
    ) -> Result<Vec<u8>, String> {
        self.inner
            .label(post_cursor, comment_cursor, emoji)
            .map(|l| l.to_vec())
            .map_err(|e| format!("reaction.invalidEmoji: {e}"))
    }

    /// The emoji that resolved for one comment, in candidate order; the
    /// counts are [`Self::resolve_count_values`] index for index.
    #[wasm_bindgen(js_name = resolveCountEmoji)]
    #[must_use]
    pub fn resolve_count_emoji(
        &self,
        post_cursor: u64,
        comment_cursor: u64,
        candidates: Vec<String>,
        labels: &[u8],
        counts: Vec<u64>,
    ) -> Vec<String> {
        self.resolve(post_cursor, comment_cursor, &candidates, labels, &counts)
            .into_iter()
            .map(|(e, _)| e)
            .collect()
    }

    /// The counts that go with [`Self::resolve_count_emoji`].
    #[wasm_bindgen(js_name = resolveCountValues)]
    #[must_use]
    pub fn resolve_count_values(
        &self,
        post_cursor: u64,
        comment_cursor: u64,
        candidates: Vec<String>,
        labels: &[u8],
        counts: Vec<u64>,
    ) -> Vec<u64> {
        self.resolve(post_cursor, comment_cursor, &candidates, labels, &counts)
            .into_iter()
            .map(|(_, n)| n)
            .collect()
    }
}

impl ChannelCommentReactionKey {
    fn resolve(
        &self,
        post_cursor: u64,
        comment_cursor: u64,
        candidates: &[String],
        labels: &[u8],
        counts: &[u64],
    ) -> Vec<(String, u64)> {
        let pairs: Vec<(ReactionLabel, u64)> = labels
            .chunks_exact(REACTION_LABEL_LEN)
            .zip(counts.iter())
            .filter_map(|(l, n)| ReactionLabel::try_from(l).ok().map(|l| (l, *n)))
            .collect();
        self.inner
            .labels_for_comment(post_cursor, comment_cursor, candidates)
            .resolve_counts(&pairs)
    }
}

/// Length of a reaction label: 16.
#[wasm_bindgen(js_name = channelReactionLabelLen)]
#[must_use]
pub fn channel_reaction_label_len() -> u32 {
    len_u32(REACTION_LABEL_LEN)
}

// ============================================================================
// Write keys
// ============================================================================

/// The account's key for one reaction under one post.
#[wasm_bindgen]
pub struct ChannelReactionWriteKey {
    inner: ReactionWriteKey,
}

#[wasm_bindgen]
impl ChannelReactionWriteKey {
    /// The key of reaction `label` on the post at `postCursor`.
    ///
    /// # Errors
    ///
    /// Throws `write.badLength: ...`.
    #[wasm_bindgen(constructor)]
    pub fn new(
        account_identity: &Identity,
        channel_id: &[u8],
        post_cursor: u64,
        label: &[u8],
    ) -> Result<ChannelReactionWriteKey, String> {
        let channel = channel16("write", channel_id)?;
        let label = fixed::<REACTION_LABEL_LEN>("write", "label", label)?;
        Ok(Self {
            inner: ReactionWriteKey::new(
                &account_identity.mls_identity().ed25519_seed_bytes(),
                &channel,
                post_cursor,
                label,
            ),
        })
    }

    /// The 32-byte public half the reaction row names.
    #[wasm_bindgen(getter, js_name = publicKey)]
    #[must_use]
    pub fn public_key(&self) -> Vec<u8> {
        self.inner.public_key().to_vec()
    }

    /// The write-proof header value for adding the reaction.
    #[wasm_bindgen(js_name = signAdd)]
    #[must_use]
    pub fn sign_add(&self) -> String {
        self.inner.sign_add(now_ms(), fresh_nonce()).encode()
    }

    /// The write-proof header value for taking the reaction off.
    #[wasm_bindgen(js_name = signDelete)]
    #[must_use]
    pub fn sign_delete(&self) -> String {
        self.inner.sign_delete(now_ms(), fresh_nonce()).encode()
    }
}

/// The account's key for one reaction on one comment (ADR-0087 phase B).
#[wasm_bindgen]
pub struct ChannelCommentReactionWriteKey {
    inner: CommentReactionWriteKey,
}

#[wasm_bindgen]
impl ChannelCommentReactionWriteKey {
    /// The key of reaction `label` on the comment `commentCursor` under the
    /// post at `postCursor`.
    ///
    /// # Errors
    ///
    /// Throws `write.badLength: ...`.
    #[wasm_bindgen(constructor)]
    pub fn new(
        account_identity: &Identity,
        channel_id: &[u8],
        post_cursor: u64,
        comment_cursor: u64,
        label: &[u8],
    ) -> Result<ChannelCommentReactionWriteKey, String> {
        let channel = channel16("write", channel_id)?;
        let label = fixed::<REACTION_LABEL_LEN>("write", "label", label)?;
        Ok(Self {
            inner: CommentReactionWriteKey::new(
                &account_identity.mls_identity().ed25519_seed_bytes(),
                &channel,
                post_cursor,
                comment_cursor,
                label,
            ),
        })
    }

    /// The 32-byte public half the reaction row names.
    #[wasm_bindgen(getter, js_name = publicKey)]
    #[must_use]
    pub fn public_key(&self) -> Vec<u8> {
        self.inner.public_key().to_vec()
    }

    /// The write-proof header value for adding the reaction.
    #[wasm_bindgen(js_name = signAdd)]
    #[must_use]
    pub fn sign_add(&self) -> String {
        self.inner.sign_add(now_ms(), fresh_nonce()).encode()
    }

    /// The write-proof header value for taking the reaction off.
    #[wasm_bindgen(js_name = signDelete)]
    #[must_use]
    pub fn sign_delete(&self) -> String {
        self.inner.sign_delete(now_ms(), fresh_nonce()).encode()
    }
}

/// The account's key for one comment under one post.
#[wasm_bindgen]
pub struct ChannelCommentWriteKey {
    inner: CommentWriteKey,
}

#[wasm_bindgen]
impl ChannelCommentWriteKey {
    /// The key of the comment `commentNonce` (16 bytes) on the post at
    /// `postCursor`.
    ///
    /// # Errors
    ///
    /// Throws `write.badLength: ...`.
    #[wasm_bindgen(constructor)]
    pub fn new(
        account_identity: &Identity,
        channel_id: &[u8],
        post_cursor: u64,
        comment_nonce: &[u8],
    ) -> Result<ChannelCommentWriteKey, String> {
        let channel = channel16("write", channel_id)?;
        let nonce = fixed::<COMMENT_NONCE_LEN>("write", "comment nonce", comment_nonce)?;
        Ok(Self {
            inner: CommentWriteKey::new(
                &account_identity.mls_identity().ed25519_seed_bytes(),
                &channel,
                post_cursor,
                &nonce,
            ),
        })
    }

    /// The 32-byte public half the comment row names (the comment's
    /// `writerKey`).
    #[wasm_bindgen(getter, js_name = publicKey)]
    #[must_use]
    pub fn public_key(&self) -> Vec<u8> {
        self.inner.public_key().to_vec()
    }

    /// The header value for adding the comment whose sealed bytes are
    /// `sealed`.
    #[wasm_bindgen(js_name = signAdd)]
    #[must_use]
    pub fn sign_add(&self, sealed: &[u8]) -> String {
        self.inner
            .sign_add(sealed, now_ms(), fresh_nonce())
            .encode()
    }

    /// The header value for deleting the comment filed at `commentCursor`.
    #[wasm_bindgen(js_name = signDelete)]
    #[must_use]
    pub fn sign_delete(&self, comment_cursor: u64) -> String {
        self.inner
            .sign_delete(comment_cursor, now_ms(), fresh_nonce())
            .encode()
    }
}

/// A fresh random 16-byte comment nonce.
#[wasm_bindgen(js_name = newChannelCommentNonce)]
#[must_use]
pub fn new_channel_comment_nonce() -> Vec<u8> {
    rust_new_comment_nonce().to_vec()
}

/// The request header a subscriber write proof travels in.
#[wasm_bindgen(js_name = channelWriteProofHeaderName)]
#[must_use]
pub fn channel_write_proof_header_name() -> String {
    WRITE_PROOF_HEADER_NAME.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_channels::write_key::WriteProof;

    const CHANNEL: [u8; 16] = [0x0C; 16];

    #[test]
    fn ban_list_comment_labels_and_write_keys() {
        let list = WasmBanList::new(4, &[[1_u8; 32], [2; 32]].concat()).unwrap();
        let wire = compose_ban_list(&list).unwrap();
        assert_eq!(decode_ban_list(&wire).unwrap(), list);
        assert!(list.is_banned(&[2; 32]));
        let content = crate::channels::classify_channel_post_plaintext(&wire);
        assert_eq!(content.kind(), "ban-list");
        assert!(WasmBanList::new(1, &[1; 31]).is_err());

        let id = Identity::from_seed(&[0x51; 64]).unwrap();
        let writer = ChannelCommentWriteKey::new(&id, &CHANNEL, 9, &[7; 16]).unwrap();
        let sealed = seal_channel_comment(
            &id,
            &[0x22; 32],
            &CHANNEL,
            2,
            9,
            &writer.public_key(),
            "Alice".into(),
            None,
            "Nice post".into(),
            Some(3),
        )
        .unwrap();
        let opened =
            open_channel_comment(&[0x22; 32], &CHANNEL, 2, 9, &writer.public_key(), &sealed)
                .unwrap();
        assert_eq!(opened.text(), "Nice post");
        assert_eq!(
            opened.author_key(),
            comment_author_key(&id, &CHANNEL).unwrap()
        );
        assert!(WriteProof::parse(&writer.sign_add(&sealed)).is_ok());

        let key = ChannelReactionKey::derive(&[0x22; 32], &CHANNEL, 2).unwrap();
        let thumbs = key.label(9, "\u{1F44D}").unwrap();
        let labels = [thumbs.clone(), vec![0; 16]].concat();
        let candidates = vec!["\u{1F44D}".to_string()];
        assert_eq!(
            key.resolve_count_emoji(9, candidates.clone(), &labels, vec![4, 99]),
            vec!["\u{1F44D}".to_string()]
        );
        assert_eq!(
            key.resolve_count_values(9, candidates, &labels, vec![4, 99]),
            vec![4]
        );
        let reaction = ChannelReactionWriteKey::new(&id, &CHANNEL, 9, &thumbs).unwrap();
        assert!(WriteProof::parse(&reaction.sign_delete()).is_ok());
        assert_ne!(reaction.public_key(), writer.public_key());
    }
}
