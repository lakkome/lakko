// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Comments under a channel post ([ADR-0087] section 13a, "Comments").
//!
//! A comment is a row at the channel service, `(post, cursor, writer_key,
//! sealed)`, where `writer_key` is the key of that one comment
//! ([`crate::write_key::CommentWriteKey`], derived from the account seed and
//! a random comment nonce) and `sealed` is at most [`COMMENT_SEALED_MAX_LEN`]
//! bytes the service cannot open.
//!
//! ## Wire
//!
//! ```text
//! comment_key = HKDF-SHA-256(ikm  = the post's epoch seed,
//!                            salt = channel_id(16) ‖ epoch_id(4, BE) ‖ post_cursor(8, BE) ‖ writer_key(32),
//!                            info = "lakko/channel/comment-key/v1")
//! sealed      = version(1)=0x01 ‖ nonce(12) ‖ ChaCha20-Poly1305(plaintext) ‖ tag(16)
//! aad         = version ‖ channel_id ‖ epoch_id(4, BE) ‖ post_cursor(8, BE) ‖ writer_key
//! plaintext   = author_key(32) ‖ signature(64) ‖ body
//! body        = name_len(1) ‖ name ‖ avatar_len(1)=0|84 ‖ avatar ‖ reply(1)=0|1 ‖ [reply_to(8, BE)]
//!               ‖ text_len(2, BE) ‖ text
//! signature   = Ed25519_author("lakko/channel/comment-sig/v1" ‖ channel_id ‖ post_cursor(8, BE)
//!                              ‖ writer_key ‖ BLAKE3(body))
//! ```
//!
//! * The key is per `(post, writer key)`, and the AAD binds the same, so the
//!   service cannot move a comment to another post or under another writer's
//!   key: it then fails the authenticator. The nonce is 96 fresh random bits
//!   per seal, in the clear.
//! * **The author key** is per channel, derived from the account seed
//!   (`BLAKE3-derive-key("lakko/channel/comment-author/v1", account_seed ‖
//!   channel_id)`), and it exists only inside the seal: the service never
//!   sees it, so it links nothing at the service, while readers can see that
//!   two comments in this channel are by one author and a publisher can ban
//!   it ([`crate::ban_list`]). It carries no account key: a comment in a
//!   public channel is public speech under a name its author typed.
//! * The signature binds the post and the writer key, so a reader who opened a
//!   comment cannot re-post it under its own writer key, or under another post,
//!   as the same author.
//! * `name` follows the display-name rule (`lakko_contacts::profile`);
//!   `avatar` is an ADR-0084 still reference (square, at most 640 px and
//!   256 KiB); `reply_to` is the server cursor of the comment answered;
//!   `text` is non-empty, trimmed, at most [`COMMENT_TEXT_MAX_BYTES`], LINE
//!   FEED its only line break, no control character and none of the bidi or
//!   invisible format characters (`lakko_contacts::channel_text`). Comments
//!   take no attachments in this version.
//!
//! Every bound is checked by [`Comment::validate`], which both
//! [`seal_comment`] and [`open_comment`] run: a comment is refused whole.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use lakko_contacts::channel_text::is_refused_format_char;
use lakko_contacts::profile::{
    check_display_name, AvatarRef, DisplayNameError, AVATAR_REF_LEN, AVATAR_STILL_MAX_BYTES,
    AVATAR_STILL_MAX_PX, DISPLAY_NAME_MAX_BYTES,
};
use lakko_crypto::aead::{self, Algorithm, KEY_LEN, NONCE_LEN, TAG_LEN};
use lakko_crypto::hash;
use lakko_crypto::kdf::hkdf_sha256;
use lakko_crypto::random;
use lakko_crypto::secret::SecretBytes;
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN, SIGNATURE_LEN};
use thiserror::Error;

use crate::epoch::{EpochId, EPOCH_ID_LEN};
use crate::owner::ACCOUNT_SEED_LEN;
use crate::seed::ContentKeySeed;
use crate::write_key::WRITE_KEY_LEN;
use crate::CHANNEL_ID_LEN;

/// The sealed comment's first byte.
pub const COMMENT_VERSION_V1: u8 = 0x01;
/// HKDF info label of the comment key, frozen at v1.
pub const COMMENT_KEY_INFO_V1: &[u8] = b"lakko/channel/comment-key/v1";
/// BLAKE3 derive-key context of the per-channel author key. Frozen.
pub const COMMENT_AUTHOR_KEY_DOMAIN: &str = "lakko/channel/comment-author/v1";
/// Domain that opens the bytes the author key signs. Frozen.
pub const COMMENT_SIG_DOMAIN_V1: &[u8] = b"lakko/channel/comment-sig/v1";
/// The most a comment row holds (ADR-0087 section 13a). The service refuses
/// a longer one.
pub const COMMENT_SEALED_MAX_LEN: usize = 2048;
/// The most UTF-8 bytes a comment's text holds. Chosen so a comment with
/// every other field at its bound still fits [`COMMENT_SEALED_MAX_LEN`].
pub const COMMENT_TEXT_MAX_BYTES: usize = 1536;
/// Length of an author key.
pub const COMMENT_AUTHOR_KEY_LEN: usize = PUBLIC_KEY_LEN;

const HEADER_LEN: usize = 1 + NONCE_LEN;
const SIGNED_PREFIX_LEN: usize = COMMENT_AUTHOR_KEY_LEN + SIGNATURE_LEN;
/// `name_len`, `avatar_len`, `reply`, `text_len`.
const BODY_FIXED_LEN: usize = 1 + 1 + 1 + 2;

/// The shortest valid sealed comment: a one-byte name and a one-byte text.
pub const COMMENT_SEALED_MIN_LEN: usize =
    HEADER_LEN + SIGNED_PREFIX_LEN + BODY_FIXED_LEN + 1 + 1 + TAG_LEN;

const COMMENT_BODY_MAX_LEN: usize =
    BODY_FIXED_LEN + DISPLAY_NAME_MAX_BYTES + AVATAR_REF_LEN + 8 + COMMENT_TEXT_MAX_BYTES;

/// The longest sealed comment this codec writes.
pub const COMMENT_SEALED_CODEC_MAX_LEN: usize =
    HEADER_LEN + SIGNED_PREFIX_LEN + COMMENT_BODY_MAX_LEN + TAG_LEN;

const _: () = assert!(COMMENT_SEALED_CODEC_MAX_LEN <= COMMENT_SEALED_MAX_LEN);

/// What a comment says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comment {
    /// The name the author typed: the display-name rule.
    pub name: String,
    /// The author's picture: an ADR-0084 still reference, or none.
    pub avatar: Option<AvatarRef>,
    /// The comment's text.
    pub text: String,
    /// The server cursor of the comment this one answers, or none.
    pub reply_to: Option<u64>,
}

/// A comment opened and its author signature verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedComment {
    /// The author's per-channel key: what a ban list names.
    pub author_key: [u8; COMMENT_AUTHOR_KEY_LEN],
    /// What it says.
    pub comment: Comment,
}

/// Why a comment's text was refused. A reason code; no variant carries the
/// text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CommentTextError {
    /// Empty.
    #[error("comment text is empty")]
    Empty,
    /// Leading or trailing whitespace.
    #[error("comment text has leading or trailing whitespace")]
    NotTrimmed,
    /// A line break other than LINE FEED.
    #[error("comment text contains a line break other than LF")]
    LineBreak,
    /// A control character other than LINE FEED, or a refused format
    /// character.
    #[error("comment text contains a control or format character")]
    ControlOrFormatCharacter,
    /// More than [`COMMENT_TEXT_MAX_BYTES`] bytes.
    #[error("comment text is longer than {max_bytes} bytes")]
    TooManyBytes {
        /// The cap.
        max_bytes: usize,
    },
    /// Not valid UTF-8. Decode-only.
    #[error("comment text is not valid UTF-8")]
    NotUtf8,
}

impl CommentTextError {
    /// Stable, language-neutral reason code. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::NotTrimmed => "notTrimmed",
            Self::LineBreak => "lineBreak",
            Self::ControlOrFormatCharacter => "controlOrFormatCharacter",
            Self::TooManyBytes { .. } => "tooManyBytes",
            Self::NotUtf8 => "notUtf8",
        }
    }
}

/// Why a comment was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CommentError {
    /// The sealed bytes are shorter than [`COMMENT_SEALED_MIN_LEN`], longer
    /// than [`COMMENT_SEALED_MAX_LEN`], or carry an unknown version.
    #[error("sealed comment is malformed")]
    Wire,
    /// Key derivation failed (impossible with these lengths).
    #[error("comment key derivation failed")]
    KeyDerivation,
    /// Sealing failed (impossible with valid inputs).
    #[error("comment seal failed")]
    Seal,
    /// The authenticator refused the comment: another seed, post, writer key,
    /// or a byte changed.
    #[error("comment did not open")]
    Open,
    /// The plaintext does not parse: a length past the end, trailing bytes,
    /// an avatar length that is neither 0 nor 84, a reply flag that is
    /// neither 0 nor 1.
    #[error("comment plaintext is malformed")]
    Malformed,
    /// The author key is not a valid Ed25519 key of full order, or its
    /// signature does not cover this channel, post, writer key and body.
    #[error("comment is not signed by its author key")]
    AuthorSignature,
    /// The name violates the display-name rule.
    #[error("invalid comment name: {0}")]
    Name(DisplayNameError),
    /// The avatar is not square, or out of the still bounds.
    #[error("invalid comment avatar")]
    Avatar,
    /// The text violates a bound.
    #[error("invalid comment text: {0}")]
    Text(CommentTextError),
}

impl CommentError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub fn code(self) -> String {
        match self {
            Self::Wire => "wire".to_string(),
            Self::KeyDerivation => "keyDerivation".to_string(),
            Self::Seal => "seal".to_string(),
            Self::Open => "open".to_string(),
            Self::Malformed => "malformed".to_string(),
            Self::AuthorSignature => "authorSignature".to_string(),
            Self::Name(e) => format!("name.{}", e.code()),
            Self::Avatar => "avatar".to_string(),
            Self::Text(e) => format!("text.{}", e.code()),
        }
    }
}

/// Check a comment's text as it stands (no trimming applied).
///
/// # Errors
///
/// The first [`CommentTextError`] that applies, in the order empty,
/// untrimmed, line break, control / format character, length.
pub fn check_comment_text(text: &str) -> Result<(), CommentTextError> {
    if text.is_empty() {
        return Err(CommentTextError::Empty);
    }
    if text.trim() != text {
        return Err(CommentTextError::NotTrimmed);
    }
    if text.chars().any(|c| {
        matches!(
            c,
            '\u{000B}' | '\u{000C}' | '\r' | '\u{0085}' | '\u{2028}' | '\u{2029}'
        )
    }) {
        return Err(CommentTextError::LineBreak);
    }
    if text
        .chars()
        .any(|c| (c.is_control() && c != '\n') || is_refused_format_char(c))
    {
        return Err(CommentTextError::ControlOrFormatCharacter);
    }
    if text.len() > COMMENT_TEXT_MAX_BYTES {
        return Err(CommentTextError::TooManyBytes {
            max_bytes: COMMENT_TEXT_MAX_BYTES,
        });
    }
    Ok(())
}

impl Comment {
    /// Check every bound. The one rule [`seal_comment`] and [`open_comment`]
    /// both run.
    ///
    /// # Errors
    ///
    /// [`CommentError::Name`], [`CommentError::Avatar`] or
    /// [`CommentError::Text`].
    pub fn validate(&self) -> Result<(), CommentError> {
        check_display_name(&self.name).map_err(CommentError::Name)?;
        if let Some(a) = &self.avatar {
            if a.width != a.height
                || a.width == 0
                || a.width > AVATAR_STILL_MAX_PX
                || a.size == 0
                || a.size > AVATAR_STILL_MAX_BYTES
            {
                return Err(CommentError::Avatar);
            }
        }
        check_comment_text(&self.text).map_err(CommentError::Text)
    }

    fn encode_body(&self) -> Result<Vec<u8>, CommentError> {
        self.validate()?;
        let name = self.name.as_bytes();
        let text = self.text.as_bytes();
        let mut out = Vec::with_capacity(COMMENT_BODY_MAX_LEN);
        out.push(u8::try_from(name.len()).map_err(|_| CommentError::Malformed)?);
        out.extend_from_slice(name);
        match &self.avatar {
            Some(a) => {
                out.push(u8::try_from(AVATAR_REF_LEN).map_err(|_| CommentError::Malformed)?);
                out.extend_from_slice(&a.blob_id);
                out.extend_from_slice(&a.key);
                out.extend_from_slice(&a.nonce);
                out.extend_from_slice(&a.size.to_be_bytes());
                out.extend_from_slice(&a.width.to_be_bytes());
                out.extend_from_slice(&a.height.to_be_bytes());
            },
            None => out.push(0),
        }
        match self.reply_to {
            Some(cursor) => {
                out.push(1);
                out.extend_from_slice(&cursor.to_be_bytes());
            },
            None => out.push(0),
        }
        out.extend_from_slice(
            &u16::try_from(text.len())
                .map_err(|_| CommentError::Malformed)?
                .to_be_bytes(),
        );
        out.extend_from_slice(text);
        Ok(out)
    }

    fn decode_body(body: &[u8]) -> Result<Self, CommentError> {
        let mut cur = body;
        let name_len = usize::from(take_u8(&mut cur)?);
        let name = take(&mut cur, name_len)?;
        let avatar = match usize::from(take_u8(&mut cur)?) {
            0 => None,
            AVATAR_REF_LEN => {
                let mut r = take(&mut cur, AVATAR_REF_LEN)?;
                Some(AvatarRef {
                    blob_id: take_array(&mut r)?,
                    key: take_array(&mut r)?,
                    nonce: take_array(&mut r)?,
                    size: u32::from_be_bytes(take_array(&mut r)?),
                    width: u16::from_be_bytes(take_array(&mut r)?),
                    height: u16::from_be_bytes(take_array(&mut r)?),
                })
            },
            _ => return Err(CommentError::Malformed),
        };
        let reply_to = match take_u8(&mut cur)? {
            0 => None,
            1 => Some(u64::from_be_bytes(take_array(&mut cur)?)),
            _ => return Err(CommentError::Malformed),
        };
        let text_len = usize::from(u16::from_be_bytes(take_array(&mut cur)?));
        let text = take(&mut cur, text_len)?;
        if !cur.is_empty() {
            return Err(CommentError::Malformed);
        }
        let comment = Self {
            name: core::str::from_utf8(name)
                .map_err(|_| CommentError::Name(DisplayNameError::NotUtf8))?
                .to_string(),
            avatar,
            text: core::str::from_utf8(text)
                .map_err(|_| CommentError::Text(CommentTextError::NotUtf8))?
                .to_string(),
            reply_to,
        };
        comment.validate()?;
        Ok(comment)
    }
}

/// Derive the author seed of `channel_id` from the account's identity seed.
#[must_use]
pub fn comment_author_seed(
    account_seed: &[u8; ACCOUNT_SEED_LEN],
    channel_id: &[u8; CHANNEL_ID_LEN],
) -> [u8; 32] {
    let mut material = [0u8; ACCOUNT_SEED_LEN + CHANNEL_ID_LEN];
    material[..ACCOUNT_SEED_LEN].copy_from_slice(account_seed);
    material[ACCOUNT_SEED_LEN..].copy_from_slice(channel_id);
    let out = hash::blake3_derive_key_from_material(COMMENT_AUTHOR_KEY_DOMAIN, &material);
    material.fill(0);
    out
}

/// The account's comment author key in one channel.
pub struct CommentAuthor {
    key: SigningKey,
}

impl core::fmt::Debug for CommentAuthor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CommentAuthor")
            .field("public_key", &self.key.verifying_key().to_bytes())
            .finish_non_exhaustive()
    }
}

impl CommentAuthor {
    /// The author key of `channel_id` for the account whose identity seed is
    /// `account_seed`.
    #[must_use]
    pub fn from_account_seed(
        account_seed: &[u8; ACCOUNT_SEED_LEN],
        channel_id: &[u8; CHANNEL_ID_LEN],
    ) -> Self {
        let mut seed = comment_author_seed(account_seed, channel_id);
        let key = SigningKey::from_seed(&seed);
        seed.fill(0);
        Self { key }
    }

    /// The public key a ban list names.
    #[must_use]
    pub fn public_key(&self) -> [u8; COMMENT_AUTHOR_KEY_LEN] {
        self.key.verifying_key().to_bytes()
    }
}

/// Where a comment sits: the post, the post's epoch, and the writer's post
/// key. Both seal and open take it.
#[derive(Clone, Copy, Debug)]
pub struct CommentPlace<'a> {
    /// The channel.
    pub channel_id: &'a [u8; CHANNEL_ID_LEN],
    /// The epoch the post was sealed in; its seed is the comment key's IKM.
    pub epoch: EpochId,
    /// The post's server cursor.
    pub post_cursor: u64,
    /// The comment's write key
    /// ([`crate::write_key::CommentWriteKey::public_key`]), as the row names
    /// it.
    pub writer_key: &'a [u8; WRITE_KEY_LEN],
}

/// The bytes the author key signs.
#[must_use]
pub fn comment_signed_bytes(place: &CommentPlace<'_>, body: &[u8]) -> Vec<u8> {
    let digest = hash::blake3(body);
    let mut out = Vec::with_capacity(
        COMMENT_SIG_DOMAIN_V1.len() + CHANNEL_ID_LEN + 8 + WRITE_KEY_LEN + digest.len(),
    );
    out.extend_from_slice(COMMENT_SIG_DOMAIN_V1);
    out.extend_from_slice(place.channel_id);
    out.extend_from_slice(&place.post_cursor.to_be_bytes());
    out.extend_from_slice(place.writer_key);
    out.extend_from_slice(&digest);
    out
}

/// Seal `comment` by `author` at `place` under the post's epoch seed.
///
/// # Errors
///
/// [`Comment::validate`]'s errors, [`CommentError::KeyDerivation`] or
/// [`CommentError::Seal`] (both impossible with valid inputs).
pub fn seal_comment(
    epoch_seed: &ContentKeySeed,
    place: &CommentPlace<'_>,
    author: &CommentAuthor,
    comment: &Comment,
) -> Result<Vec<u8>, CommentError> {
    let body = comment.encode_body()?;
    let signature = author.key.sign(&comment_signed_bytes(place, &body));
    let mut plaintext = Vec::with_capacity(SIGNED_PREFIX_LEN + body.len());
    plaintext.extend_from_slice(&author.public_key());
    plaintext.extend_from_slice(&signature.to_bytes());
    plaintext.extend_from_slice(&body);

    let key = comment_key(epoch_seed, place)?;
    let mut nonce = [0u8; NONCE_LEN];
    random::fill(&mut nonce);
    let ciphertext = aead::seal(
        Algorithm::ChaCha20Poly1305,
        &key,
        &nonce,
        &comment_aad(place),
        &plaintext,
    )
    .map_err(|_| CommentError::Seal)?;
    let mut out = Vec::with_capacity(HEADER_LEN + ciphertext.len());
    out.push(COMMENT_VERSION_V1);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Open a comment row at `place` under the post's epoch seed and verify its
/// author signature. Fail-closed.
///
/// # Errors
///
/// [`CommentError::Wire`], [`CommentError::Open`],
/// [`CommentError::Malformed`], [`CommentError::AuthorSignature`], or a field
/// bound.
pub fn open_comment(
    epoch_seed: &ContentKeySeed,
    place: &CommentPlace<'_>,
    sealed: &[u8],
) -> Result<OpenedComment, CommentError> {
    if sealed.len() < COMMENT_SEALED_MIN_LEN
        || sealed.len() > COMMENT_SEALED_MAX_LEN
        || sealed[0] != COMMENT_VERSION_V1
    {
        return Err(CommentError::Wire);
    }
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&sealed[1..HEADER_LEN]);
    let key = comment_key(epoch_seed, place)?;
    let plaintext = aead::open(
        Algorithm::ChaCha20Poly1305,
        &key,
        &nonce,
        &comment_aad(place),
        &sealed[HEADER_LEN..],
    )
    .map_err(|_| CommentError::Open)?;
    open_plaintext(place, &plaintext)
}

/// Split and verify a comment plaintext. Also the fuzz entry point: the AEAD
/// keeps a fuzzer from reaching this parser through [`open_comment`].
pub(crate) fn open_plaintext(
    place: &CommentPlace<'_>,
    plaintext: &[u8],
) -> Result<OpenedComment, CommentError> {
    if plaintext.len() < SIGNED_PREFIX_LEN {
        return Err(CommentError::Malformed);
    }
    let (author_bytes, rest) = plaintext.split_at(COMMENT_AUTHOR_KEY_LEN);
    let (sig_bytes, body) = rest.split_at(SIGNATURE_LEN);
    let author_key: [u8; COMMENT_AUTHOR_KEY_LEN] = author_bytes
        .try_into()
        .map_err(|_| CommentError::Malformed)?;
    let comment = Comment::decode_body(body)?;
    let verifying =
        VerifyingKey::from_bytes(&author_key).map_err(|_| CommentError::AuthorSignature)?;
    // A small-order key verifies a forged signature over any message.
    if verifying.is_weak() {
        return Err(CommentError::AuthorSignature);
    }
    let signature = Signature::from_bytes(sig_bytes).map_err(|_| CommentError::AuthorSignature)?;
    verifying
        .verify(&comment_signed_bytes(place, body), &signature)
        .map_err(|_| CommentError::AuthorSignature)?;
    Ok(OpenedComment {
        author_key,
        comment,
    })
}

fn comment_key(
    epoch_seed: &ContentKeySeed,
    place: &CommentPlace<'_>,
) -> Result<SecretBytes<KEY_LEN>, CommentError> {
    let mut salt = Vec::with_capacity(CHANNEL_ID_LEN + EPOCH_ID_LEN + 8 + WRITE_KEY_LEN);
    salt.extend_from_slice(place.channel_id);
    salt.extend_from_slice(&place.epoch.to_be_bytes());
    salt.extend_from_slice(&place.post_cursor.to_be_bytes());
    salt.extend_from_slice(place.writer_key);
    hkdf_sha256::<KEY_LEN>(epoch_seed.as_inner_bytes(), &salt, COMMENT_KEY_INFO_V1)
        .map_err(|_| CommentError::KeyDerivation)
}

fn comment_aad(place: &CommentPlace<'_>) -> Vec<u8> {
    let mut aad = Vec::with_capacity(1 + CHANNEL_ID_LEN + EPOCH_ID_LEN + 8 + WRITE_KEY_LEN);
    aad.push(COMMENT_VERSION_V1);
    aad.extend_from_slice(place.channel_id);
    aad.extend_from_slice(&place.epoch.to_be_bytes());
    aad.extend_from_slice(&place.post_cursor.to_be_bytes());
    aad.extend_from_slice(place.writer_key);
    aad
}

fn take_u8(cur: &mut &[u8]) -> Result<u8, CommentError> {
    let (&first, rest) = cur.split_first().ok_or(CommentError::Malformed)?;
    *cur = rest;
    Ok(first)
}

fn take<'a>(cur: &mut &'a [u8], n: usize) -> Result<&'a [u8], CommentError> {
    if cur.len() < n {
        return Err(CommentError::Malformed);
    }
    let (head, rest) = cur.split_at(n);
    *cur = rest;
    Ok(head)
}

fn take_array<const N: usize>(cur: &mut &[u8]) -> Result<[u8; N], CommentError> {
    take(cur, N)?
        .try_into()
        .map_err(|_| CommentError::Malformed)
}

/// Fuzz oracle behind `fuzz_api::channel_comment_open`: open `sealed` at a
/// fixed place under a fixed seed, and feed the same bytes to the plaintext
/// parser and the body parser directly (the AEAD keeps a fuzzer from
/// reaching them otherwise). `true` when every property holds: the open
/// refused (a fuzzer cannot forge the tag), and anything a parser accepted is
/// within every bound and re-encodes to the identical body.
pub(crate) fn fuzz_open(sealed: &[u8]) -> bool {
    let place = CommentPlace {
        channel_id: &[0x0C; CHANNEL_ID_LEN],
        epoch: EpochId::new(3),
        post_cursor: 7,
        writer_key: &[0x5B; WRITE_KEY_LEN],
    };
    let seed = ContentKeySeed::from_bytes_for_test([0x42; 32]);
    let mut holds = open_comment(&seed, &place, sealed).is_err();
    if let Ok(o) = open_plaintext(&place, sealed) {
        holds &= o.comment.validate().is_ok();
        holds &= o
            .comment
            .encode_body()
            .is_ok_and(|b| sealed.get(SIGNED_PREFIX_LEN..) == Some(&b[..]));
    }
    if let Some(body) = sealed.get(SIGNED_PREFIX_LEN..) {
        if let Ok(c) = Comment::decode_body(body) {
            holds &= c.encode_body().is_ok_and(|b| b == body);
        }
    }
    holds
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::write_key::CommentWriteKey;

    const CHANNEL: [u8; CHANNEL_ID_LEN] = [0x0C; CHANNEL_ID_LEN];
    const ACCOUNT: [u8; 32] = [0x11; 32];

    fn seed() -> ContentKeySeed {
        ContentKeySeed::from_bytes_for_test([0x42; 32])
    }

    fn avatar() -> AvatarRef {
        AvatarRef {
            blob_id: [1; 32],
            key: [2; 32],
            nonce: [3; 12],
            size: 1000,
            width: 320,
            height: 320,
        }
    }

    fn comment() -> Comment {
        Comment {
            name: "Ana".to_string(),
            avatar: Some(avatar()),
            text: "First line\nsecond line".to_string(),
            reply_to: Some(12),
        }
    }

    fn writer_key() -> [u8; WRITE_KEY_LEN] {
        CommentWriteKey::new(&ACCOUNT, &CHANNEL, 7, &[0x31; 16]).public_key()
    }

    fn place(key: &[u8; WRITE_KEY_LEN]) -> CommentPlace<'_> {
        CommentPlace {
            channel_id: &CHANNEL,
            epoch: EpochId::new(3),
            post_cursor: 7,
            writer_key: key,
        }
    }

    #[test]
    fn a_comment_round_trips_and_names_its_author() {
        let key = writer_key();
        let author = CommentAuthor::from_account_seed(&ACCOUNT, &CHANNEL);
        let sealed = seal_comment(&seed(), &place(&key), &author, &comment()).unwrap();
        assert!(sealed.len() <= COMMENT_SEALED_MAX_LEN);
        let opened = open_comment(&seed(), &place(&key), &sealed).unwrap();
        assert_eq!(opened.comment, comment());
        assert_eq!(opened.author_key, author.public_key());

        let bare = Comment {
            avatar: None,
            reply_to: None,
            text: "x".to_string(),
            name: "A".to_string(),
        };
        let sealed = seal_comment(&seed(), &place(&key), &author, &bare).unwrap();
        assert_eq!(sealed.len(), COMMENT_SEALED_MIN_LEN);
        assert_eq!(
            open_comment(&seed(), &place(&key), &sealed)
                .unwrap()
                .comment,
            bare
        );
    }

    #[test]
    fn a_maximal_comment_fits_the_row() {
        let key = writer_key();
        let author = CommentAuthor::from_account_seed(&ACCOUNT, &CHANNEL);
        // 64 four-byte scalars fills the 255-byte name bound below its char cap.
        let big = Comment {
            name: "\u{1F600}".repeat(63),
            avatar: Some(avatar()),
            text: "a".repeat(COMMENT_TEXT_MAX_BYTES),
            reply_to: Some(u64::MAX),
        };
        let sealed = seal_comment(&seed(), &place(&key), &author, &big).unwrap();
        assert!(sealed.len() <= COMMENT_SEALED_CODEC_MAX_LEN);
        assert_eq!(COMMENT_SEALED_MIN_LEN, 132);
        assert_eq!(COMMENT_SEALED_CODEC_MAX_LEN, 2013);
        assert_eq!(
            open_comment(&seed(), &place(&key), &sealed)
                .unwrap()
                .comment,
            big
        );
    }

    /// The service cannot move a comment to another post, epoch, channel or
    /// writer key, and a changed byte does not open.
    #[test]
    fn a_comment_opens_only_where_it_was_written() {
        let key = writer_key();
        let author = CommentAuthor::from_account_seed(&ACCOUNT, &CHANNEL);
        let sealed = seal_comment(&seed(), &place(&key), &author, &comment()).unwrap();
        let other_key = CommentWriteKey::new(&[0x12; 32], &CHANNEL, 7, &[0x31; 16]).public_key();
        let elsewhere = [
            CommentPlace {
                post_cursor: 8,
                ..place(&key)
            },
            CommentPlace {
                epoch: EpochId::new(4),
                ..place(&key)
            },
            CommentPlace {
                channel_id: &[0x0D; CHANNEL_ID_LEN],
                ..place(&key)
            },
            place(&other_key),
        ];
        for p in elsewhere {
            assert_eq!(open_comment(&seed(), &p, &sealed), Err(CommentError::Open));
        }
        assert_eq!(
            open_comment(
                &ContentKeySeed::from_bytes_for_test([0x43; 32]),
                &place(&key),
                &sealed
            ),
            Err(CommentError::Open)
        );
        for i in 1..sealed.len() {
            let mut t = sealed.clone();
            t[i] ^= 0x01;
            assert!(open_comment(&seed(), &place(&key), &t).is_err(), "{i}");
        }
        let mut v = sealed.clone();
        v[0] = 2;
        assert_eq!(
            open_comment(&seed(), &place(&key), &v),
            Err(CommentError::Wire)
        );
        assert_eq!(
            open_comment(&seed(), &place(&key), &sealed[..COMMENT_SEALED_MIN_LEN - 1]),
            Err(CommentError::Wire)
        );
        assert_eq!(
            open_comment(&seed(), &place(&key), &[1u8; COMMENT_SEALED_MAX_LEN + 1]),
            Err(CommentError::Wire)
        );
    }

    /// Someone who opened a comment cannot re-seal it under its own writer key
    /// as the same author: the signature names the original writer key.
    #[test]
    fn a_resealed_comment_under_another_writer_key_fails_the_author_signature() {
        let key = writer_key();
        let author = CommentAuthor::from_account_seed(&ACCOUNT, &CHANNEL);
        let body = comment().encode_body().unwrap();
        let sig = author.key.sign(&comment_signed_bytes(&place(&key), &body));
        let mut plaintext = author.public_key().to_vec();
        plaintext.extend_from_slice(&sig.to_bytes());
        plaintext.extend_from_slice(&body);
        assert!(open_plaintext(&place(&key), &plaintext).is_ok());
        let thief = CommentWriteKey::new(&[0x12; 32], &CHANNEL, 7, &[0x31; 16]).public_key();
        assert_eq!(
            open_plaintext(&place(&thief), &plaintext),
            Err(CommentError::AuthorSignature)
        );
        assert_eq!(
            open_plaintext(
                &CommentPlace {
                    post_cursor: 8,
                    ..place(&key)
                },
                &plaintext
            ),
            Err(CommentError::AuthorSignature)
        );
        // A weak author key is refused even with any signature.
        let mut weak = plaintext.clone();
        weak[..32].fill(0);
        weak[0] = 1;
        assert_eq!(
            open_plaintext(&place(&key), &weak),
            Err(CommentError::AuthorSignature)
        );
    }

    #[test]
    fn the_author_key_is_per_channel_and_per_account() {
        let a = CommentAuthor::from_account_seed(&ACCOUNT, &CHANNEL).public_key();
        assert_eq!(
            a,
            CommentAuthor::from_account_seed(&ACCOUNT, &CHANNEL).public_key()
        );
        assert_ne!(
            a,
            CommentAuthor::from_account_seed(&ACCOUNT, &[0x0D; CHANNEL_ID_LEN]).public_key()
        );
        assert_ne!(
            a,
            CommentAuthor::from_account_seed(&[0x12; 32], &CHANNEL).public_key()
        );
        assert_ne!(
            comment_author_seed(&ACCOUNT, &CHANNEL),
            crate::write_key::comment_write_seed(&ACCOUNT, &CHANNEL, 0, &[0; 16])
        );
    }

    #[test]
    fn seal_refuses_what_open_refuses() {
        let key = writer_key();
        let author = CommentAuthor::from_account_seed(&ACCOUNT, &CHANNEL);
        let bad = |c: Comment| seal_comment(&seed(), &place(&key), &author, &c).unwrap_err();
        assert_eq!(
            bad(Comment {
                name: String::new(),
                ..comment()
            }),
            CommentError::Name(DisplayNameError::Empty)
        );
        assert_eq!(
            bad(Comment {
                text: " x".into(),
                ..comment()
            }),
            CommentError::Text(CommentTextError::NotTrimmed)
        );
        assert_eq!(
            bad(Comment {
                text: String::new(),
                ..comment()
            }),
            CommentError::Text(CommentTextError::Empty)
        );
        assert_eq!(
            bad(Comment {
                text: "a\r\nb".into(),
                ..comment()
            }),
            CommentError::Text(CommentTextError::LineBreak)
        );
        assert_eq!(
            bad(Comment {
                text: "a\u{202E}b".into(),
                ..comment()
            }),
            CommentError::Text(CommentTextError::ControlOrFormatCharacter)
        );
        assert_eq!(
            bad(Comment {
                text: "a".repeat(COMMENT_TEXT_MAX_BYTES + 1),
                ..comment()
            }),
            CommentError::Text(CommentTextError::TooManyBytes {
                max_bytes: COMMENT_TEXT_MAX_BYTES
            })
        );
        let mut wide = avatar();
        wide.width = 321;
        assert_eq!(
            bad(Comment {
                avatar: Some(wide),
                ..comment()
            }),
            CommentError::Avatar
        );
        // ZWJ (an emoji sequence) is permitted.
        assert!(check_comment_text("\u{1F468}\u{200D}\u{1F469}").is_ok());
        assert_eq!(
            CommentError::Text(CommentTextError::Empty).code(),
            "text.empty"
        );
    }

    #[test]
    fn the_body_parser_refuses_bad_flags_and_trailing_bytes() {
        let body = comment().encode_body().unwrap();
        assert_eq!(Comment::decode_body(&body).unwrap(), comment());
        for cut in 0..body.len() {
            assert!(Comment::decode_body(&body[..cut]).is_err(), "{cut}");
        }
        let mut trailing = body.clone();
        trailing.push(0);
        assert_eq!(
            Comment::decode_body(&trailing),
            Err(CommentError::Malformed)
        );
        // name "Ana" (1 + 3), then the avatar length byte.
        let mut avatar_len = body.clone();
        avatar_len[4] = 83;
        assert_eq!(
            Comment::decode_body(&avatar_len),
            Err(CommentError::Malformed)
        );
        let mut reply = body;
        reply[4 + 1 + AVATAR_REF_LEN] = 2;
        assert_eq!(Comment::decode_body(&reply), Err(CommentError::Malformed));
        assert!(fuzz_open(&[0u8; 200]));
        let mut plaintext = vec![0u8; SIGNED_PREFIX_LEN];
        plaintext.extend_from_slice(&comment().encode_body().unwrap());
        assert!(fuzz_open(&plaintext));
    }
}
