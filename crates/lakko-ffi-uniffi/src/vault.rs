// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI exposure of `lakko-vault` - the conversation vault ([ADR-0064]) and
//! the blob delete key ([ADR-0065] section 6).
//!
//! A vault is an object on the storage service, one per conversation plus two
//! per account (the index and the devices vault), that no request ties to the
//! account. This module hands a client:
//!
//! - [`VaultKeys`], built from the ACCOUNT identity by [`vault_keys`]: the
//!   vault's name, the public half of its write key, sealing and opening of
//!   its objects, and the request signature of the storage routes. It never
//!   returns the vault root, a vault key or a write seed.
//! - The saved records: [`encode_conversation_head`] /
//!   [`decode_conversation_head`], [`encode_history_chunk`] /
//!   [`decode_history_chunk`], [`encode_vault_index`] / [`decode_vault_index`],
//!   [`encode_devices_head`] / [`decode_devices_head`], and
//!   [`merge_vault_messages`].
//!
//! The pure core takes the clock and the nonce as arguments; both are read
//! here (the wall clock for a proof, the OS random source for a seal), as
//! `ds_proof` does.
//!
//! ## Media metadata
//!
//! The core codec's bounds are structural. The ADR-0047 media rules live in
//! `lakko-attachments`, and this module applies
//! `lakko_attachments::RawMediaMetadata::bounded` to every attachment in both
//! directions - on encode, on decode and on merge - the way the
//! history-bundle placeholder does. So a restored row never carries metadata
//! an ordinary received message could not: a field outside its bound is
//! dropped alone, a poster frame outside its size limit is dropped whole.
//! The attachment records therefore have the shape of `HistoryAttachment` /
//! `HistoryThumb` (optional strings, `u32` dimensions, a `u64` thumb size)
//! rather than the core's narrower integers.
//!
//! ## Errors
//!
//! Every refusal is [`FfiError::Vault`] with the message `"<code>: <reason>"`.
//! The code is the core's frozen `code()` (`RecordError`, `SealError`,
//! `ProofError`, `VaultRefError`), or one of the argument-length codes this
//! module adds: `blobIdLength`, `keyLength`, `nonceLength`,
//! `accountKeyLength`, `seedLength`. A client branches on the code, never on
//! the prose.
//!
//! [ADR-0064]: ../../../docs/adr/0064-conversation-backup-and-device-re-entry.md
//! [ADR-0065]: ../../../docs/adr/0065-attachment-lifetime-and-access.md

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use lakko_attachments::{RawMediaMetadata, RawThumbMetadata};
use lakko_crypto::aead::NONCE_LEN;
use lakko_crypto::SecretBytes;
use lakko_vault::keys::{RefKind, VaultRef as CoreVaultRef, VaultRefError, VaultRoot};
use lakko_vault::proof::{self, ProofError, PROOF_HEADER_NAME, PROOF_WINDOW_MS};
use lakko_vault::record::{
    self as rec, RecordError, MAX_CHUNK_MESSAGES, MAX_GROUP_INFO_BYTES, MAX_PEOPLE,
    MAX_REENTRY_EPOCHS, MAX_TAIL_MESSAGES,
};
use lakko_vault::seal::{open_object, seal_object, sealed_len, ObjectKind, SealError};
use lakko_vault::{merge_messages, ConversationRef as CoreConversationRef};

use crate::synced_state::{FolderConversationKind, FolderConversationRef};
use crate::{FfiError, Identity};

// --- errors -------------------------------------------------------------

fn refusal(code: &str, reason: impl std::fmt::Display) -> FfiError {
    FfiError::Vault(format!("{code}: {reason}"))
}

impl From<RecordError> for FfiError {
    fn from(e: RecordError) -> Self {
        refusal(e.code(), e)
    }
}

impl From<SealError> for FfiError {
    fn from(e: SealError) -> Self {
        refusal(e.code(), e)
    }
}

impl From<ProofError> for FfiError {
    fn from(e: ProofError) -> Self {
        refusal(e.code(), e)
    }
}

impl From<VaultRefError> for FfiError {
    fn from(e: VaultRefError) -> Self {
        refusal(e.code(), e)
    }
}

fn fixed<const N: usize>(bytes: &[u8], code: &str, what: &str) -> Result<[u8; N], FfiError> {
    <[u8; N]>::try_from(bytes).map_err(|_| {
        refusal(
            code,
            format_args!("{what} must be {N} bytes, got {}", bytes.len()),
        )
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn as_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

// --- which vault --------------------------------------------------------

/// Which vault: one conversation's, or one of the two account-level vaults.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum VaultRef {
    /// The vault of one conversation or channel, named the ADR-0058 way (the
    /// same reference the chat folders use).
    Conversation {
        /// The conversation.
        conversation: FolderConversationRef,
    },
    /// The vault that lists the account's conversations.
    Index,
    /// The vault of the account's own device group.
    Devices,
}

const fn ref_kind_to_core(kind: FolderConversationKind) -> RefKind {
    match kind {
        FolderConversationKind::OneToOne => RefKind::OneToOne,
        FolderConversationKind::Group => RefKind::Group,
        FolderConversationKind::Channel => RefKind::Channel,
    }
}

const fn ref_kind_from_core(kind: RefKind) -> FolderConversationKind {
    match kind {
        RefKind::OneToOne => FolderConversationKind::OneToOne,
        RefKind::Group => FolderConversationKind::Group,
        RefKind::Channel => FolderConversationKind::Channel,
    }
}

fn conversation_to_core(r: &FolderConversationRef) -> Result<CoreConversationRef, FfiError> {
    Ok(CoreConversationRef::new(ref_kind_to_core(r.kind), &r.id)?)
}

fn conversation_from_core(r: &CoreConversationRef) -> FolderConversationRef {
    FolderConversationRef {
        kind: ref_kind_from_core(r.kind()),
        id: r.id().to_vec(),
    }
}

fn vault_to_core(vault: &VaultRef) -> Result<CoreVaultRef, FfiError> {
    Ok(match vault {
        VaultRef::Conversation { conversation } => {
            CoreVaultRef::Conversation(conversation_to_core(conversation)?)
        },
        VaultRef::Index => CoreVaultRef::Index,
        VaultRef::Devices => CoreVaultRef::Devices,
    })
}

// --- keys ---------------------------------------------------------------

/// The vault names and keys of one account. See the module doc.
///
/// Holds the vault root, from which every vault of the account can be named,
/// read and rewritten and every file the account uploaded deleted. The root
/// and what it derives stay inside this object: no method returns the root, a
/// vault key or a write seed.
#[derive(uniffi::Object)]
pub struct VaultKeys {
    root: VaultRoot,
}

/// The vault keys of `account_identity`'s account.
///
/// `account_identity` is the ACCOUNT identity (the ADR-0017 phrase-seeded
/// one, the same on every device of the account), not a device identity:
/// its Ed25519 seed is the derivation input, exactly as for
/// `dsJournalRootSigner`. A device identity here would name vaults no other
/// device of the account can find.
#[uniffi::export]
pub fn vault_keys(account_identity: &Identity) -> Arc<VaultKeys> {
    let seed = SecretBytes::new(account_identity.inner.ed25519_seed_bytes());
    Arc::new(VaultKeys {
        root: VaultRoot::from_account_seed(seed.as_bytes()),
    })
}

impl VaultKeys {
    fn seal(
        &self,
        vault: &VaultRef,
        kind: ObjectKind,
        chunk_no: u32,
        version: u64,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, FfiError> {
        let vault = vault_to_core(vault)?;
        let mut nonce = [0u8; NONCE_LEN];
        lakko_crypto::random::fill(&mut nonce);
        Ok(seal_object(
            &self.root.vault_key(&vault),
            &self.root.vault_id(&vault),
            kind,
            chunk_no,
            version,
            &nonce,
            plaintext,
        )?)
    }

    fn open(
        &self,
        vault: &VaultRef,
        kind: ObjectKind,
        chunk_no: u32,
        version: u64,
        sealed: &[u8],
    ) -> Result<Vec<u8>, FfiError> {
        let vault = vault_to_core(vault)?;
        Ok(open_object(
            &self.root.vault_key(&vault),
            &self.root.vault_id(&vault),
            kind,
            chunk_no,
            version,
            sealed,
        )?)
    }
}

#[uniffi::export]
impl VaultKeys {
    /// The 32-byte name of the vault on the storage service: what the request
    /// path carries, in lowercase hex. Not secret, and never sent to another
    /// person.
    ///
    /// # Errors
    ///
    /// [`FfiError::Vault`] `idLength` when a conversation id is empty or
    /// longer than 64 bytes.
    pub fn vault_id(&self, vault: VaultRef) -> Result<Vec<u8>, FfiError> {
        Ok(self.root.vault_id(&vault_to_core(&vault)?).to_vec())
    }

    /// The 32-byte Ed25519 public key a claim (`PUT /vault/{id}`) stores: the
    /// public half of the vault's write key.
    ///
    /// # Errors
    ///
    /// [`FfiError::Vault`] `idLength`.
    pub fn write_public_key(&self, vault: VaultRef) -> Result<Vec<u8>, FfiError> {
        let seed = self.root.vault_write_seed(&vault_to_core(&vault)?);
        Ok(proof::public_key(seed.as_bytes()).to_vec())
    }

    /// Seal `plaintext` (an encoded head) as the vault's head at `version`.
    /// The 12-byte nonce is drawn here from the OS random source, fresh for
    /// every call; the result is padded to a size bucket.
    ///
    /// # Errors
    ///
    /// [`FfiError::Vault`] `idLength`, `plaintextTooLarge` or `seal`.
    pub fn seal_head(
        &self,
        vault: VaultRef,
        version: u64,
        plaintext: Vec<u8>,
    ) -> Result<Vec<u8>, FfiError> {
        self.seal(&vault, ObjectKind::Head, 0, version, &plaintext)
    }

    /// Seal `plaintext` (an encoded chunk) as chunk `chunk_no` of the vault
    /// at `version`. The nonce is drawn here, as for [`Self::seal_head`].
    ///
    /// # Errors
    ///
    /// [`FfiError::Vault`] `idLength`, `plaintextTooLarge` or `seal`.
    pub fn seal_chunk(
        &self,
        vault: VaultRef,
        chunk_no: u32,
        version: u64,
        plaintext: Vec<u8>,
    ) -> Result<Vec<u8>, FfiError> {
        self.seal(&vault, ObjectKind::Chunk, chunk_no, version, &plaintext)
    }

    /// Open bytes the storage service returned as the vault's head at
    /// `version`. Bytes sealed as anything else - another vault, a chunk,
    /// another version - fail with `authentication`.
    ///
    /// # Errors
    ///
    /// [`FfiError::Vault`] `idLength`, `objectTooLarge`, `truncated`,
    /// `unknownFormat`, `notABucket`, `authentication` or `padding`.
    pub fn open_head(
        &self,
        vault: VaultRef,
        version: u64,
        sealed: Vec<u8>,
    ) -> Result<Vec<u8>, FfiError> {
        self.open(&vault, ObjectKind::Head, 0, version, &sealed)
    }

    /// Open bytes the storage service returned as chunk `chunk_no` of the
    /// vault at `version`. Bytes sealed as anything else - another vault, a
    /// head, another chunk number, another version - fail with
    /// `authentication`.
    ///
    /// # Errors
    ///
    /// As [`Self::open_head`].
    pub fn open_chunk(
        &self,
        vault: VaultRef,
        chunk_no: u32,
        version: u64,
        sealed: Vec<u8>,
    ) -> Result<Vec<u8>, FfiError> {
        self.open(&vault, ObjectKind::Chunk, chunk_no, version, &sealed)
    }

    /// The [`storage_proof_header_name`] header value for one request to the
    /// vault's routes, signed with the vault's write key. `method` is upper
    /// case (`GET`, `PUT`, `DELETE`); `path` is the service path
    /// (`/vault/<hex>/head`), never a reverse-proxy prefix; `body` is the
    /// exact bytes sent (empty for a `GET`). The clock is read here; a value
    /// is good within [`storage_proof_window_ms`] of the server's clock.
    ///
    /// # Errors
    ///
    /// [`FfiError::Vault`] `idLength`, or `requestNotCanonical` when `method`
    /// or `path` contains a NUL byte.
    pub fn sign_request(
        &self,
        vault: VaultRef,
        method: String,
        path: String,
        body: Vec<u8>,
    ) -> Result<String, FfiError> {
        let seed = self.root.vault_write_seed(&vault_to_core(&vault)?);
        Ok(proof::sign(
            seed.as_bytes(),
            &method,
            &path,
            &body,
            now_ms(),
        )?)
    }

    /// The 32-byte Ed25519 public key an upload of the blob `blob_id`
    /// carries, so that only a device of this account can delete it
    /// (ADR-0065 section 6).
    ///
    /// # Errors
    ///
    /// [`FfiError::Vault`] `blobIdLength` when `blob_id` is not 32 bytes.
    pub fn blob_delete_public_key(&self, blob_id: Vec<u8>) -> Result<Vec<u8>, FfiError> {
        let blob_id = fixed::<32>(&blob_id, "blobIdLength", "blob id")?;
        let seed = self.root.blob_delete_seed(&blob_id);
        Ok(proof::public_key(seed.as_bytes()).to_vec())
    }

    /// The [`storage_proof_header_name`] header value for
    /// `DELETE /blob/{hex}` of the blob `blob_id`, signed with that blob's
    /// delete key. Arguments as for [`Self::sign_request`].
    ///
    /// # Errors
    ///
    /// [`FfiError::Vault`] `blobIdLength`, or `requestNotCanonical`.
    pub fn sign_blob_delete(
        &self,
        blob_id: Vec<u8>,
        method: String,
        path: String,
        body: Vec<u8>,
    ) -> Result<String, FfiError> {
        let blob_id = fixed::<32>(&blob_id, "blobIdLength", "blob id")?;
        let seed = self.root.blob_delete_seed(&blob_id);
        Ok(proof::sign(
            seed.as_bytes(),
            &method,
            &path,
            &body,
            now_ms(),
        )?)
    }
}

/// The request header a storage proof travels in (`x-lakko-storage-proof`).
#[uniffi::export]
pub fn storage_proof_header_name() -> String {
    PROOF_HEADER_NAME.to_string()
}

/// How far from the server's clock a storage proof's time may lie, either
/// way, in milliseconds (120 000).
#[uniffi::export]
pub fn storage_proof_window_ms() -> u64 {
    PROOF_WINDOW_MS
}

/// The sealed size of an object carrying `plaintext_len` bytes - what the
/// storage service will see - or `None` when that is more than one object
/// can carry. For a client that budgets a vault's bytes.
#[uniffi::export]
pub fn vault_sealed_len(plaintext_len: u64) -> Option<u64> {
    let len = usize::try_from(plaintext_len).ok()?;
    u64::try_from(sealed_len(len)?).ok()
}

/// Most messages in one chunk (512).
#[uniffi::export]
pub fn vault_max_chunk_messages() -> u32 {
    as_u32(MAX_CHUNK_MESSAGES)
}

/// Most messages in a head's tail (512).
#[uniffi::export]
pub fn vault_max_tail_messages() -> u32 {
    as_u32(MAX_TAIL_MESSAGES)
}

/// Most saved epochs of re-entry material in a head (2: the two newest).
#[uniffi::export]
pub fn vault_max_reentry_epochs() -> u32 {
    as_u32(MAX_REENTRY_EPOCHS)
}

/// Most people in one head (4096).
#[uniffi::export]
pub fn vault_max_people() -> u32 {
    as_u32(MAX_PEOPLE)
}

/// Longest saved GroupInfo, in bytes (1 MiB).
#[uniffi::export]
pub fn vault_max_group_info_bytes() -> u32 {
    as_u32(MAX_GROUP_INFO_BYTES)
}

// --- records ------------------------------------------------------------

/// A message's direction relative to the account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum VaultDirection {
    /// Sent by this account.
    Outgoing,
    /// Received from someone else.
    Incoming,
    /// A system row (a membership change, a call).
    System,
}

/// What kind of conversation a head describes (ADR-0062).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum VaultConversationKind {
    /// The saving device did not know.
    Unknown,
    /// A one-to-one conversation.
    OneToOne,
    /// A group.
    Group,
}

/// A person's role at save time (ADR-0022).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum VaultRole {
    /// A plain member.
    Member,
    /// An admin.
    Admin,
    /// The owner.
    Owner,
}

/// The account's membership at save time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum VaultMembership {
    /// In the conversation.
    In,
    /// Left it.
    Left,
    /// Was removed from it.
    Removed,
}

macro_rules! mirror_enum {
    ($ffi:ident, $core:path, [$($variant:ident),+ $(,)?]) => {
        impl From<$ffi> for $core {
            fn from(v: $ffi) -> Self {
                match v {
                    $( $ffi::$variant => Self::$variant, )+
                }
            }
        }

        impl From<$core> for $ffi {
            fn from(v: $core) -> Self {
                use $core as C;
                match v {
                    $( C::$variant => Self::$variant, )+
                }
            }
        }
    };
}

mirror_enum!(VaultDirection, rec::Direction, [Outgoing, Incoming, System]);
mirror_enum!(
    VaultConversationKind,
    rec::ConversationKind,
    [Unknown, OneToOne, Group]
);
mirror_enum!(VaultRole, rec::Role, [Member, Admin, Owner]);
mirror_enum!(VaultMembership, rec::Membership, [In, Left, Removed]);

/// A poster-frame blob of a [`VaultAttachment`] (ADR-0047). `blob_id` / `key`
/// MUST be 32 bytes, `nonce` 12 bytes.
#[derive(Clone, PartialEq, Eq, uniffi::Record)]
pub struct VaultThumb {
    /// 32-byte blob id.
    pub blob_id: Vec<u8>,
    /// 32-byte content-encryption key.
    pub key: Vec<u8>,
    /// 12-byte AEAD nonce.
    pub nonce: Vec<u8>,
    /// Plaintext size in bytes. A thumb outside the ADR-0047 size limit is
    /// dropped whole.
    pub size: u64,
    /// MIME type.
    pub mime: Option<String>,
    /// Pixel width.
    pub width: Option<u32>,
    /// Pixel height.
    pub height: Option<u32>,
}

impl std::fmt::Debug for VaultThumb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultThumb")
            .field("blob_id", &self.blob_id)
            .field("key", &"<redacted>")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

/// An attachment reference of a saved message: where the encrypted file is
/// and how to open it. A reference, not the file, which may have expired by
/// the time the history is read (ADR-0065 section 8). `blob_id` / `key` MUST
/// be 32 bytes, `nonce` 12 bytes. The media fields are ADR-0047's, bounded
/// in both directions (see the module doc).
#[derive(Clone, PartialEq, Eq, uniffi::Record)]
pub struct VaultAttachment {
    /// 32-byte blob id.
    pub blob_id: Vec<u8>,
    /// 32-byte content-encryption key.
    pub key: Vec<u8>,
    /// 12-byte AEAD nonce.
    pub nonce: Vec<u8>,
    /// Plaintext size in bytes.
    pub size: u64,
    /// MIME type.
    pub mime: Option<String>,
    /// File name.
    pub name: Option<String>,
    /// Media kind (`voice`, `video_note`, `gif`, or one this build does not
    /// know); `None` for a plain file.
    pub kind: Option<String>,
    /// Playback length in milliseconds.
    pub duration_ms: Option<u32>,
    /// Waveform amplitude bytes (at most 128).
    pub waveform: Option<Vec<u8>>,
    /// Pixel width.
    pub width: Option<u32>,
    /// Pixel height.
    pub height: Option<u32>,
    /// Poster frame.
    pub thumb: Option<VaultThumb>,
    /// ADR-0092 audio title tag.
    #[uniffi(default = None)]
    pub title: Option<String>,
    /// ADR-0092 audio artist tag.
    #[uniffi(default = None)]
    pub artist: Option<String>,
}

impl std::fmt::Debug for VaultAttachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultAttachment")
            .field("blob_id", &self.blob_id)
            .field("key", &"<redacted>")
            .field("size", &self.size)
            .field("kind", &self.kind)
            .field("thumb", &self.thumb)
            .finish_non_exhaustive()
    }
}

/// The quote a reply carries (ADR-0052): a snapshot taken when the reply was
/// composed.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct VaultReply {
    /// Message id of the quoted message.
    pub target: u64,
    /// The quoted author's display name; empty when none.
    pub author: String,
    /// An excerpt of the quoted text; empty when none.
    pub excerpt: String,
}

/// Where a forwarded message came from (ADR-0052).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct VaultForwardOrigin {
    /// The origin conversation, as the message carried it (1..=128 bytes).
    pub group_id: Vec<u8>,
    /// Message id of the original in that conversation.
    pub message_id: u64,
}

/// What a forwarded message says about its origin (ADR-0052).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct VaultForward {
    /// The original author's display name; empty when none.
    pub author: String,
    /// The original's location, when the forwarder held it.
    pub origin: Option<VaultForwardOrigin>,
}

/// One reaction.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct VaultReaction {
    /// The reacting person's 32-byte account key.
    pub reactor: Vec<u8>,
    /// The emoji (1..=64 bytes).
    pub emoji: String,
}

/// How a call row ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct VaultCall {
    /// The outcome, as the clients number it. Opaque to the codec.
    pub outcome: u8,
    /// Whether it was a video call.
    pub video: bool,
    /// How long it lasted, in seconds.
    pub duration_secs: u32,
}

/// One saved message: the row shape of a head's tail and of every chunk.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct VaultMessage {
    /// The Delivery Service journal id, which every device observes
    /// identically. The merge key.
    pub message_id: u64,
    /// Direction relative to the account.
    pub direction: VaultDirection,
    /// The author's 32-byte account key, when known.
    pub author: Option<Vec<u8>>,
    /// When it was sent, unix milliseconds.
    pub created_at_ms: i64,
    /// Journal id of the newest edit, delete or reaction applied to this
    /// row; the row's own id when none was. Decides a merge.
    pub last_op_id: u64,
    /// The text was edited.
    pub edited: bool,
    /// The message was deleted; the row is kept, emptied.
    pub deleted: bool,
    /// The text.
    pub text: String,
    /// Attachment references (at most 64).
    pub attachments: Vec<VaultAttachment>,
    /// The quote, for a reply.
    pub reply: Option<VaultReply>,
    /// The origin, for a forwarded message.
    pub forward: Option<VaultForward>,
    /// Reactions, keyed by account key.
    pub reactions: Vec<VaultReaction>,
    /// The outcome, for a call row.
    pub call: Option<VaultCall>,
    /// Set when the message was of a wire version the saving device could
    /// not read: that version.
    pub unsupported_version: Option<u32>,
    /// ADR-0092 — the content of a poll, checklist, location, contact card
    /// or article message and its saved state. On decode a payload that does
    /// not decode is `None` and a state that does not belong is dropped.
    #[uniffi(default = None)]
    pub content: Option<crate::content::ContentMessage>,
}

/// One person in a conversation, as shown at save time. Never a device leaf.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct VaultPerson {
    /// The person's 32-byte account key.
    pub account_key: Vec<u8>,
    /// Display-name snapshot; empty when none.
    pub display_name: String,
    /// Username snapshot; empty when none.
    pub username: String,
    /// Role at save time.
    pub role: VaultRole,
}

/// A pin shared with the conversation (ADR-0052). An unpin is kept, so a
/// stale pin cannot bring it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct VaultSharedPin {
    /// The pinned message.
    pub message_id: u64,
    /// Pinned, or unpinned.
    pub pinned: bool,
    /// The stamp of this state, unix milliseconds.
    pub updated_at_ms: u64,
}

/// A pin of this account alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct VaultPersonalPin {
    /// The pinned message.
    pub message_id: u64,
    /// When it was pinned, unix milliseconds.
    pub pinned_at_ms: u64,
}

/// One row of a head's chunk table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct VaultChunkEntry {
    /// How many messages the chunk holds.
    pub count: u16,
    /// Its lowest message id.
    pub first_id: u64,
    /// Its highest message id.
    pub last_id: u64,
    /// The chunk object's current version on the storage service.
    pub version: u64,
}

/// Re-entry material for one epoch (ADR-0064 section 4; the access seed by
/// the section 7 amendment of 2026-11-14).
///
/// **`ds_access_seed` is secret**: the 32-byte Delivery Service access seed
/// of `epoch` (`PublisherGroup.dsAccessSeed` / `stagedDsAccessSeed`), from
/// which an entering device rebuilds the epoch's access signer
/// (`dsAccessSignerFromSeed`). Never log it.
#[derive(Clone, PartialEq, Eq, uniffi::Record)]
pub struct VaultReentryEpoch {
    /// The epoch the GroupInfo describes.
    pub epoch: u64,
    /// The 32-byte Delivery Service access seed of `epoch`. Secret.
    pub ds_access_seed: Vec<u8>,
    /// The GroupInfo with its ratchet tree, as MLS encodes it. Opaque to the
    /// codec; 1 byte to 1 MiB.
    pub group_info: Vec<u8>,
}

impl std::fmt::Debug for VaultReentryEpoch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultReentryEpoch")
            .field("epoch", &self.epoch)
            .field("ds_access_seed", &"<redacted>")
            .field("group_info_len", &self.group_info.len())
            .finish()
    }
}

/// The account's registrar seed for a conversation's journal (ADR-0057).
///
/// **`seed` is secret.** It crosses this boundary as bytes because the
/// client already holds it that way: it stores the one it receives in-group
/// (`openRegistrarSeedEnvelope`) under the device key, and it must store one
/// read from a head the same way and nowhere else. Never log it.
#[derive(Clone, PartialEq, Eq, uniffi::Record)]
pub struct VaultRegistrarSeed {
    /// The rank the seed was issued at. Opaque to the codec.
    pub rank: u8,
    /// The 32-byte seed. Secret.
    pub seed: Vec<u8>,
}

impl std::fmt::Debug for VaultRegistrarSeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultRegistrarSeed")
            .field("rank", &self.rank)
            .field("seed", &"<redacted>")
            .finish()
    }
}

/// A conversation's head.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct VaultConversationHead {
    /// Which conversation.
    pub conversation: FolderConversationRef,
    /// One-to-one or group.
    pub kind: VaultConversationKind,
    /// The group name; empty for a one-to-one or an unnamed group.
    pub title: String,
    /// The people (at most [`vault_max_people`]).
    pub people: Vec<VaultPerson>,
    /// The account's membership at save time.
    pub membership: VaultMembership,
    /// Shared pins (at most 1024).
    pub shared_pins: Vec<VaultSharedPin>,
    /// Personal pins (at most 1024).
    pub personal_pins: Vec<VaultPersonalPin>,
    /// Last activity, unix milliseconds.
    pub last_activity_ms: u64,
    /// The chunk table (at most 65 536 entries).
    pub chunks: Vec<VaultChunkEntry>,
    /// The newest messages, not yet moved into a chunk (at most
    /// [`vault_max_tail_messages`]).
    pub tail: Vec<VaultMessage>,
    /// Re-entry material for the newest epochs (at most
    /// [`vault_max_reentry_epochs`]); empty when the account is not in the
    /// conversation.
    pub reentry: Vec<VaultReentryEpoch>,
    /// The account's registrar seed for this journal, when it holds one.
    /// Secret: see [`VaultRegistrarSeed`].
    pub registrar_seed: Option<VaultRegistrarSeed>,
}

/// One conversation in the index.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct VaultIndexEntry {
    /// Which conversation.
    pub conversation: FolderConversationRef,
    /// Last activity, unix milliseconds. Orders the loading.
    pub last_activity_ms: u64,
    /// The account's membership at save time.
    pub membership: VaultMembership,
}

// --- record conversions -------------------------------------------------

fn some_text(text: String) -> Option<String> {
    (!text.is_empty()).then_some(text)
}

fn narrow(px: Option<u32>) -> Option<u16> {
    px.and_then(|px| u16::try_from(px).ok())
}

/// To the core shape, with the ADR-0047 bounds applied: what is out of
/// bounds is dropped, so the narrower core integers always fit.
fn attachment_to_core(a: VaultAttachment) -> Result<rec::VaultAttachment, FfiError> {
    let blob_id = fixed::<32>(&a.blob_id, "blobIdLength", "attachment blob id")?;
    let key = fixed::<32>(&a.key, "keyLength", "attachment key")?;
    let nonce = fixed::<12>(&a.nonce, "nonceLength", "attachment nonce")?;
    let (thumb_refs, thumb_meta) = match a.thumb {
        None => (None, None),
        Some(t) => (
            Some((
                fixed::<32>(&t.blob_id, "blobIdLength", "thumb blob id")?,
                fixed::<32>(&t.key, "keyLength", "thumb key")?,
                fixed::<12>(&t.nonce, "nonceLength", "thumb nonce")?,
            )),
            Some(RawThumbMetadata {
                size: t.size,
                mime: t.mime,
                width: t.width,
                height: t.height,
            }),
        ),
    };
    let media = RawMediaMetadata {
        kind: a.kind,
        duration_ms: a.duration_ms,
        waveform: a.waveform,
        width: a.width,
        height: a.height,
        thumb: thumb_meta,
        title: a.title,
        artist: a.artist,
    }
    .bounded();
    let thumb = match (thumb_refs, media.thumb) {
        (Some((blob_id, key, nonce)), Some(t)) => {
            u32::try_from(t.size).ok().map(|size| rec::VaultThumb {
                blob_id,
                key,
                nonce,
                mime: t.mime.unwrap_or_default(),
                size,
                width: narrow(t.width),
                height: narrow(t.height),
            })
        },
        _ => None,
    };
    Ok(rec::VaultAttachment {
        blob_id,
        key,
        nonce,
        size: a.size,
        mime: a.mime.unwrap_or_default(),
        name: a.name.unwrap_or_default(),
        kind: media.kind.unwrap_or_default(),
        duration_ms: media.duration_ms,
        waveform: media.waveform.unwrap_or_default(),
        width: narrow(media.width),
        height: narrow(media.height),
        thumb,
        title: media.title.unwrap_or_default(),
        artist: media.artist.unwrap_or_default(),
    })
}

/// From the core shape, with the ADR-0047 bounds applied: a decoded row
/// keeps exactly what a live envelope would have kept.
fn attachment_from_core(a: rec::VaultAttachment) -> VaultAttachment {
    let media = RawMediaMetadata {
        kind: some_text(a.kind),
        duration_ms: a.duration_ms,
        waveform: (!a.waveform.is_empty()).then_some(a.waveform),
        width: a.width.map(u32::from),
        height: a.height.map(u32::from),
        thumb: a.thumb.as_ref().map(|t| RawThumbMetadata {
            size: u64::from(t.size),
            mime: some_text(t.mime.clone()),
            width: t.width.map(u32::from),
            height: t.height.map(u32::from),
        }),
        title: some_text(a.title),
        artist: some_text(a.artist),
    }
    .bounded();
    let thumb = match (a.thumb, media.thumb) {
        (Some(t), Some(bounded)) => Some(VaultThumb {
            blob_id: t.blob_id.to_vec(),
            key: t.key.to_vec(),
            nonce: t.nonce.to_vec(),
            size: bounded.size,
            mime: bounded.mime,
            width: bounded.width,
            height: bounded.height,
        }),
        _ => None,
    };
    VaultAttachment {
        blob_id: a.blob_id.to_vec(),
        key: a.key.to_vec(),
        nonce: a.nonce.to_vec(),
        size: a.size,
        mime: some_text(a.mime),
        name: some_text(a.name),
        kind: media.kind,
        duration_ms: media.duration_ms,
        waveform: media.waveform,
        width: media.width,
        height: media.height,
        thumb,
        title: media.title,
        artist: media.artist,
    }
}

fn message_to_core(m: VaultMessage) -> Result<rec::VaultMessage, FfiError> {
    let content = m
        .content
        .map(|c| {
            crate::content::content_message_to_core(c).map(|(kind, payload, state)| {
                rec::VaultContent {
                    kind,
                    payload,
                    state,
                }
            })
        })
        .transpose()?;
    let author = m
        .author
        .map(|a| fixed::<32>(&a, "accountKeyLength", "author account key"))
        .transpose()?;
    let attachments = m
        .attachments
        .into_iter()
        .map(attachment_to_core)
        .collect::<Result<Vec<_>, _>>()?;
    let reactions = m
        .reactions
        .into_iter()
        .map(|r| {
            Ok(rec::VaultReaction {
                reactor: fixed::<32>(&r.reactor, "accountKeyLength", "reactor account key")?,
                emoji: r.emoji,
            })
        })
        .collect::<Result<Vec<_>, FfiError>>()?;
    Ok(rec::VaultMessage {
        message_id: m.message_id,
        direction: m.direction.into(),
        author,
        created_at_ms: m.created_at_ms,
        last_op_id: m.last_op_id,
        edited: m.edited,
        deleted: m.deleted,
        text: m.text,
        attachments,
        reply: m.reply.map(|r| rec::VaultReply {
            target: r.target,
            author: r.author,
            excerpt: r.excerpt,
        }),
        forward: m.forward.map(|f| rec::VaultForward {
            author: f.author,
            origin: f.origin.map(|o| rec::VaultForwardOrigin {
                group_id: o.group_id,
                message_id: o.message_id,
            }),
        }),
        reactions,
        call: m.call.map(|c| rec::VaultCall {
            outcome: c.outcome,
            video: c.video,
            duration_secs: c.duration_secs,
        }),
        unsupported_version: m.unsupported_version,
        content,
    })
}

fn message_from_core(m: rec::VaultMessage) -> VaultMessage {
    let mimes: Vec<Option<String>> = m
        .attachments
        .iter()
        .map(|a| some_text(a.mime.clone()))
        .collect();
    let content = m.content.as_ref().and_then(|c| {
        crate::content::content_message_from_core(c.kind, &c.payload, &c.state, &mimes)
    });
    VaultMessage {
        message_id: m.message_id,
        direction: m.direction.into(),
        author: m.author.map(|a| a.to_vec()),
        created_at_ms: m.created_at_ms,
        last_op_id: m.last_op_id,
        edited: m.edited,
        deleted: m.deleted,
        text: m.text,
        attachments: m
            .attachments
            .into_iter()
            .map(attachment_from_core)
            .collect(),
        reply: m.reply.map(|r| VaultReply {
            target: r.target,
            author: r.author,
            excerpt: r.excerpt,
        }),
        forward: m.forward.map(|f| VaultForward {
            author: f.author,
            origin: f.origin.map(|o| VaultForwardOrigin {
                group_id: o.group_id,
                message_id: o.message_id,
            }),
        }),
        reactions: m
            .reactions
            .into_iter()
            .map(|r| VaultReaction {
                reactor: r.reactor.to_vec(),
                emoji: r.emoji,
            })
            .collect(),
        call: m.call.map(|c| VaultCall {
            outcome: c.outcome,
            video: c.video,
            duration_secs: c.duration_secs,
        }),
        unsupported_version: m.unsupported_version,
        content,
    }
}

// The vault's content bounds must equal the content codec's.
const _: () = assert!(
    rec::MAX_CONTENT_PAYLOAD_BYTES == lakko_attachments::content_state::MAX_CONTENT_PAYLOAD_BYTES
);
const _: () = assert!(
    rec::MAX_CONTENT_STATE_BYTES == lakko_attachments::content_state::MAX_CONTENT_STATE_BYTES
);
const _: () = assert!(rec::MAX_AUDIO_TAG_BYTES == lakko_attachments::MAX_AUDIO_TAG_BYTES);
const _: () = assert!(
    *rec::CONTENT_KINDS.start() == lakko_attachments::content_state::CONTENT_KIND_POLL
        && *rec::CONTENT_KINDS.end() == lakko_attachments::content_state::CONTENT_KIND_ARTICLE
);

fn messages_to_core(messages: Vec<VaultMessage>) -> Result<Vec<rec::VaultMessage>, FfiError> {
    messages.into_iter().map(message_to_core).collect()
}

fn messages_from_core(messages: Vec<rec::VaultMessage>) -> Vec<VaultMessage> {
    messages.into_iter().map(message_from_core).collect()
}

fn reentry_to_core(reentry: Vec<VaultReentryEpoch>) -> Result<Vec<rec::ReentryEpoch>, FfiError> {
    reentry
        .into_iter()
        .map(|e| {
            Ok(rec::ReentryEpoch {
                epoch: e.epoch,
                ds_access_seed: SecretBytes::new(fixed::<32>(
                    &e.ds_access_seed,
                    "seedLength",
                    "re-entry access seed",
                )?),
                group_info: e.group_info,
            })
        })
        .collect()
}

fn reentry_from_core(reentry: Vec<rec::ReentryEpoch>) -> Vec<VaultReentryEpoch> {
    reentry
        .into_iter()
        .map(|e| VaultReentryEpoch {
            epoch: e.epoch,
            ds_access_seed: e.ds_access_seed.as_bytes().to_vec(),
            group_info: e.group_info,
        })
        .collect()
}

fn head_to_core(head: VaultConversationHead) -> Result<rec::ConversationHead, FfiError> {
    let people = head
        .people
        .into_iter()
        .map(|p| {
            Ok(rec::VaultPerson {
                account_key: fixed::<32>(&p.account_key, "accountKeyLength", "person account key")?,
                display_name: p.display_name,
                username: p.username,
                role: p.role.into(),
            })
        })
        .collect::<Result<Vec<_>, FfiError>>()?;
    let registrar_seed = head
        .registrar_seed
        .map(|r| {
            Ok::<_, FfiError>(rec::RegistrarSeed {
                rank: r.rank,
                seed: SecretBytes::new(fixed::<32>(&r.seed, "seedLength", "registrar seed")?),
            })
        })
        .transpose()?;
    Ok(rec::ConversationHead {
        conversation: conversation_to_core(&head.conversation)?,
        kind: head.kind.into(),
        title: head.title,
        people,
        membership: head.membership.into(),
        shared_pins: head
            .shared_pins
            .into_iter()
            .map(|p| rec::SharedPin {
                message_id: p.message_id,
                pinned: p.pinned,
                updated_at_ms: p.updated_at_ms,
            })
            .collect(),
        personal_pins: head
            .personal_pins
            .into_iter()
            .map(|p| rec::PersonalPin {
                message_id: p.message_id,
                pinned_at_ms: p.pinned_at_ms,
            })
            .collect(),
        last_activity_ms: head.last_activity_ms,
        chunks: head
            .chunks
            .into_iter()
            .map(|c| rec::ChunkEntry {
                count: c.count,
                first_id: c.first_id,
                last_id: c.last_id,
                version: c.version,
            })
            .collect(),
        tail: messages_to_core(head.tail)?,
        reentry: reentry_to_core(head.reentry)?,
        registrar_seed,
    })
}

fn head_from_core(head: rec::ConversationHead) -> VaultConversationHead {
    VaultConversationHead {
        conversation: conversation_from_core(&head.conversation),
        kind: head.kind.into(),
        title: head.title,
        people: head
            .people
            .into_iter()
            .map(|p| VaultPerson {
                account_key: p.account_key.to_vec(),
                display_name: p.display_name,
                username: p.username,
                role: p.role.into(),
            })
            .collect(),
        membership: head.membership.into(),
        shared_pins: head
            .shared_pins
            .into_iter()
            .map(|p| VaultSharedPin {
                message_id: p.message_id,
                pinned: p.pinned,
                updated_at_ms: p.updated_at_ms,
            })
            .collect(),
        personal_pins: head
            .personal_pins
            .into_iter()
            .map(|p| VaultPersonalPin {
                message_id: p.message_id,
                pinned_at_ms: p.pinned_at_ms,
            })
            .collect(),
        last_activity_ms: head.last_activity_ms,
        chunks: head
            .chunks
            .into_iter()
            .map(|c| VaultChunkEntry {
                count: c.count,
                first_id: c.first_id,
                last_id: c.last_id,
                version: c.version,
            })
            .collect(),
        tail: messages_from_core(head.tail),
        reentry: reentry_from_core(head.reentry),
        registrar_seed: head.registrar_seed.map(|r| VaultRegistrarSeed {
            rank: r.rank,
            seed: r.seed.as_bytes().to_vec(),
        }),
    }
}

// --- record functions ---------------------------------------------------

/// Encode a conversation's head: the plaintext [`VaultKeys::seal_head`]
/// takes.
///
/// # Errors
///
/// [`FfiError::Vault`] with an argument-length code, a `RecordError` bound
/// (`title`, `people`, `pins`, `chunks`, `messages`, `text`, `attachments`,
/// `attachment`, `reply`, `forward`, `reactions`, `reentry`), `idLength`, or
/// `tooLarge` when the encoding does not fit one object - move messages from
/// the tail into chunks and encode again.
#[uniffi::export]
pub fn encode_conversation_head(head: VaultConversationHead) -> Result<Vec<u8>, FfiError> {
    Ok(head_to_core(head)?.encode()?)
}

/// Decode what [`VaultKeys::open_head`] returned for a conversation's vault.
/// The registrar seed in the result is secret: see [`VaultRegistrarSeed`].
///
/// # Errors
///
/// [`FfiError::Vault`] with the `RecordError` code of the first thing that
/// is wrong (`unknownVersion`, `truncated`, `trailingBytes`, a bound, ...).
#[uniffi::export]
pub fn decode_conversation_head(bytes: Vec<u8>) -> Result<VaultConversationHead, FfiError> {
    Ok(head_from_core(rec::ConversationHead::decode(&bytes)?))
}

/// Encode one chunk of earlier messages (at most
/// [`vault_max_chunk_messages`]): the plaintext [`VaultKeys::seal_chunk`]
/// takes. The codec asks for no order; [`merge_vault_messages`] is what
/// yields rows sorted by id, each id once.
///
/// # Errors
///
/// [`FfiError::Vault`] with an argument-length code, a message bound, or
/// `tooLarge` when the encoding does not fit one object - split the chunk.
#[uniffi::export]
pub fn encode_history_chunk(messages: Vec<VaultMessage>) -> Result<Vec<u8>, FfiError> {
    let chunk = rec::HistoryChunk {
        messages: messages_to_core(messages)?,
    };
    Ok(chunk.encode()?)
}

/// Decode what [`VaultKeys::open_chunk`] returned into its messages.
///
/// # Errors
///
/// [`FfiError::Vault`] with the `RecordError` code of the first thing that
/// is wrong.
#[uniffi::export]
pub fn decode_history_chunk(bytes: Vec<u8>) -> Result<Vec<VaultMessage>, FfiError> {
    Ok(messages_from_core(
        rec::HistoryChunk::decode(&bytes)?.messages,
    ))
}

/// Encode the account's index (at most 65 536 entries, no conversation
/// twice): the plaintext of the head of the [`VaultRef::Index`] vault.
///
/// # Errors
///
/// [`FfiError::Vault`] `idLength`, `entries`, `repeatedRef` or `tooLarge`.
#[uniffi::export]
pub fn encode_vault_index(entries: Vec<VaultIndexEntry>) -> Result<Vec<u8>, FfiError> {
    let entries = entries
        .into_iter()
        .map(|e| {
            Ok(rec::IndexEntry {
                conversation: conversation_to_core(&e.conversation)?,
                last_activity_ms: e.last_activity_ms,
                membership: e.membership.into(),
            })
        })
        .collect::<Result<Vec<_>, FfiError>>()?;
    Ok(rec::VaultIndex { entries }.encode()?)
}

/// Decode the head of the [`VaultRef::Index`] vault into its entries.
///
/// # Errors
///
/// [`FfiError::Vault`] with the `RecordError` code of the first thing that
/// is wrong.
#[uniffi::export]
pub fn decode_vault_index(bytes: Vec<u8>) -> Result<Vec<VaultIndexEntry>, FfiError> {
    Ok(rec::VaultIndex::decode(&bytes)?
        .entries
        .into_iter()
        .map(|e| VaultIndexEntry {
            conversation: conversation_from_core(&e.conversation),
            last_activity_ms: e.last_activity_ms,
            membership: e.membership.into(),
        })
        .collect())
}

/// Encode the head of the account's own device group (ADR-0064 section 5):
/// re-entry material for at most [`vault_max_reentry_epochs`] epochs, the
/// plaintext of the head of the [`VaultRef::Devices`] vault.
///
/// # Errors
///
/// [`FfiError::Vault`] `reentry` or `tooLarge`.
#[uniffi::export]
pub fn encode_devices_head(reentry: Vec<VaultReentryEpoch>) -> Result<Vec<u8>, FfiError> {
    let head = rec::DevicesHead {
        reentry: reentry_to_core(reentry)?,
    };
    Ok(head.encode()?)
}

/// Decode the head of the [`VaultRef::Devices`] vault into its re-entry
/// material.
///
/// # Errors
///
/// [`FfiError::Vault`] with the `RecordError` code of the first thing that
/// is wrong.
#[uniffi::export]
pub fn decode_devices_head(bytes: Vec<u8>) -> Result<Vec<VaultReentryEpoch>, FfiError> {
    Ok(reentry_from_core(rec::DevicesHead::decode(&bytes)?.reentry))
}

/// Merge two sets of saved messages: what a device does when its write
/// meets `409`. The union of the rows by `message_id`; for one id the row
/// with the higher `last_op_id` wins, and on a tie the same row wins on
/// every device. The result is sorted by `message_id` and holds each id
/// once. Commutative, associative and idempotent.
///
/// # Errors
///
/// [`FfiError::Vault`] with an argument-length code when a key, nonce or
/// blob id in a row has the wrong length.
#[uniffi::export]
pub fn merge_vault_messages(
    a: Vec<VaultMessage>,
    b: Vec<VaultMessage>,
) -> Result<Vec<VaultMessage>, FfiError> {
    let a = messages_to_core(a)?;
    let b = messages_to_core(b)?;
    Ok(messages_from_core(merge_messages(&a, &b)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> (Identity, Arc<VaultKeys>) {
        let identity = Identity::new("account".into()).unwrap();
        let keys = vault_keys(&identity);
        (identity, keys)
    }

    fn conversation(kind: FolderConversationKind, id: &[u8]) -> FolderConversationRef {
        FolderConversationRef {
            kind,
            id: id.to_vec(),
        }
    }

    fn group(id: u8) -> VaultRef {
        VaultRef::Conversation {
            conversation: conversation(FolderConversationKind::Group, &[id; 32]),
        }
    }

    fn code(e: &FfiError) -> String {
        let FfiError::Vault(message) = e else {
            panic!("not a vault refusal: {e:?}");
        };
        let (code, reason) = message.split_once(": ").expect("code: reason");
        assert!(!reason.is_empty());
        code.to_string()
    }

    fn attachment() -> VaultAttachment {
        VaultAttachment {
            blob_id: vec![1; 32],
            key: vec![2; 32],
            nonce: vec![3; 12],
            size: 123_456,
            mime: Some("audio/mp4".into()),
            name: Some("voice.m4a".into()),
            kind: Some("voice".into()),
            duration_ms: Some(7_250),
            waveform: Some(vec![10, 20, 30]),
            width: Some(640),
            height: Some(480),
            thumb: Some(VaultThumb {
                blob_id: vec![4; 32],
                key: vec![5; 32],
                nonce: vec![6; 12],
                size: 5_000,
                mime: Some("image/jpeg".into()),
                width: Some(240),
                height: Some(180),
            }),
            title: None,
            artist: None,
        }
    }

    /// A message carrying every optional field.
    fn full_message(message_id: u64, last_op_id: u64) -> VaultMessage {
        VaultMessage {
            message_id,
            direction: VaultDirection::Incoming,
            author: Some(vec![0xA1; 32]),
            created_at_ms: 1_790_000_000_123,
            last_op_id,
            edited: true,
            deleted: false,
            text: "hello".into(),
            attachments: vec![attachment()],
            reply: Some(VaultReply {
                target: 3,
                author: "Ann".into(),
                excerpt: "earlier".into(),
            }),
            forward: Some(VaultForward {
                author: "Bob".into(),
                origin: Some(VaultForwardOrigin {
                    group_id: vec![0xF0; 32],
                    message_id: 9,
                }),
            }),
            reactions: vec![VaultReaction {
                reactor: vec![0xB2; 32],
                emoji: "+1".into(),
            }],
            call: Some(VaultCall {
                outcome: 2,
                video: true,
                duration_secs: 61,
            }),
            unsupported_version: Some(7),
            content: None,
        }
    }

    fn plain_message(message_id: u64, last_op_id: u64, text: &str) -> VaultMessage {
        VaultMessage {
            message_id,
            direction: VaultDirection::Outgoing,
            author: None,
            created_at_ms: 0,
            last_op_id,
            edited: false,
            deleted: false,
            text: text.into(),
            attachments: vec![],
            reply: None,
            forward: None,
            reactions: vec![],
            call: None,
            unsupported_version: None,
            content: None,
        }
    }

    fn head() -> VaultConversationHead {
        VaultConversationHead {
            conversation: conversation(FolderConversationKind::Group, &[0xA7; 32]),
            kind: VaultConversationKind::Group,
            title: "Friends".into(),
            people: vec![VaultPerson {
                account_key: vec![0xA1; 32],
                display_name: "Ann".into(),
                username: "ann".into(),
                role: VaultRole::Owner,
            }],
            membership: VaultMembership::In,
            shared_pins: vec![VaultSharedPin {
                message_id: 4,
                pinned: true,
                updated_at_ms: 99,
            }],
            personal_pins: vec![VaultPersonalPin {
                message_id: 5,
                pinned_at_ms: 100,
            }],
            last_activity_ms: 1_790_000_000_999,
            chunks: vec![VaultChunkEntry {
                count: 512,
                first_id: 1,
                last_id: 600,
                version: 3,
            }],
            tail: vec![full_message(601, 650), plain_message(602, 602, "tail")],
            reentry: vec![
                VaultReentryEpoch {
                    epoch: 11,
                    ds_access_seed: vec![0xA1; 32],
                    group_info: vec![0x11; 40],
                },
                VaultReentryEpoch {
                    epoch: 12,
                    ds_access_seed: vec![0xA2; 32],
                    group_info: vec![0x12; 40],
                },
            ],
            registrar_seed: Some(VaultRegistrarSeed {
                rank: 2,
                seed: vec![0x5E; 32],
            }),
        }
    }

    #[test]
    fn keys_are_stable_and_distinct_per_ref() {
        let (identity, keys) = keys();
        let again = vault_keys(&identity);
        let refs = [
            group(1),
            group(2),
            VaultRef::Conversation {
                conversation: conversation(FolderConversationKind::OneToOne, &[1; 32]),
            },
            VaultRef::Conversation {
                conversation: conversation(FolderConversationKind::Channel, &[1; 32]),
            },
            VaultRef::Index,
            VaultRef::Devices,
        ];
        let mut seen = Vec::new();
        for vault in refs {
            let id = keys.vault_id(vault.clone()).unwrap();
            let public = keys.write_public_key(vault.clone()).unwrap();
            assert_eq!(id.len(), 32);
            assert_eq!(public.len(), 32);
            assert_eq!(id, again.vault_id(vault.clone()).unwrap());
            assert_eq!(public, again.write_public_key(vault).unwrap());
            assert_ne!(id, public);
            assert!(!seen.contains(&id));
            assert!(!seen.contains(&public));
            seen.push(id);
            seen.push(public);
        }

        // The derivation is the core's, from the account identity seed.
        let root = VaultRoot::from_account_seed(&identity.inner.ed25519_seed_bytes());
        assert_eq!(
            keys.vault_id(VaultRef::Index).unwrap(),
            root.vault_id(&CoreVaultRef::Index)
        );

        // Another account names unrelated vaults.
        let (_, other) = self::keys();
        assert_ne!(
            keys.vault_id(VaultRef::Index).unwrap(),
            other.vault_id(VaultRef::Index).unwrap()
        );

        // A conversation id outside 1..=64 bytes is refused, never derived.
        for id in [vec![], vec![0u8; 65]] {
            let bad = VaultRef::Conversation {
                conversation: conversation(FolderConversationKind::Group, &id),
            };
            assert_eq!(code(&keys.vault_id(bad).unwrap_err()), "idLength");
        }
    }

    #[test]
    fn seal_and_open_round_trip_and_refuse_another_address() {
        let (_, keys) = keys();
        let vault = group(1);
        let plaintext = b"a head".to_vec();

        let head = keys.seal_head(vault.clone(), 7, plaintext.clone()).unwrap();
        assert_eq!(
            u64::try_from(head.len()).unwrap(),
            vault_sealed_len(u64::try_from(plaintext.len()).unwrap()).unwrap()
        );
        assert_eq!(
            keys.open_head(vault.clone(), 7, head.clone()).unwrap(),
            plaintext
        );
        // A fresh nonce each time: the same plaintext seals differently.
        assert_ne!(
            head,
            keys.seal_head(vault.clone(), 7, plaintext.clone()).unwrap()
        );

        // Wrong version, wrong vault, wrong kind.
        for refused in [
            keys.open_head(vault.clone(), 8, head.clone()),
            keys.open_head(group(2), 7, head.clone()),
            keys.open_head(VaultRef::Index, 7, head.clone()),
            keys.open_chunk(vault.clone(), 0, 7, head.clone()),
        ] {
            assert_eq!(code(&refused.unwrap_err()), "authentication");
        }

        let chunk = keys
            .seal_chunk(vault.clone(), 3, 9, plaintext.clone())
            .unwrap();
        assert_eq!(
            keys.open_chunk(vault.clone(), 3, 9, chunk.clone()).unwrap(),
            plaintext
        );
        // Wrong chunk number, wrong version, wrong kind, wrong vault.
        for refused in [
            keys.open_chunk(vault.clone(), 4, 9, chunk.clone()),
            keys.open_chunk(vault.clone(), 3, 10, chunk.clone()),
            keys.open_head(vault.clone(), 9, chunk.clone()),
            keys.open_chunk(group(2), 3, 9, chunk.clone()),
        ] {
            assert_eq!(code(&refused.unwrap_err()), "authentication");
        }

        // Hostile bytes are refused with the core's codes.
        assert_eq!(
            code(&keys.open_head(vault.clone(), 7, vec![]).unwrap_err()),
            "truncated"
        );
        let mut altered = head;
        altered[0] = 0x02;
        assert_eq!(
            code(&keys.open_head(vault, 7, altered).unwrap_err()),
            "unknownFormat"
        );
    }

    #[test]
    fn a_head_round_trips_with_every_optional_field() {
        let original = head();
        let bytes = encode_conversation_head(original.clone()).unwrap();
        let back = decode_conversation_head(bytes.clone()).unwrap();
        assert_eq!(back, original);
        // Canonical: what decodes re-encodes to the identical bytes.
        assert_eq!(encode_conversation_head(back).unwrap(), bytes);

        // Sealed and opened through the keys, it is still the same head.
        let (_, keys) = keys();
        let vault = VaultRef::Conversation {
            conversation: original.conversation.clone(),
        };
        let sealed = keys.seal_head(vault.clone(), 1, bytes).unwrap();
        let opened = keys.open_head(vault, 1, sealed).unwrap();
        assert_eq!(decode_conversation_head(opened).unwrap(), original);
    }

    #[test]
    fn a_chunk_an_index_and_a_devices_head_round_trip() {
        let messages = vec![full_message(1, 5), plain_message(2, 2, "two")];
        let bytes = encode_history_chunk(messages.clone()).unwrap();
        assert_eq!(decode_history_chunk(bytes).unwrap(), messages);

        let entries = vec![
            VaultIndexEntry {
                conversation: conversation(FolderConversationKind::OneToOne, &[1; 32]),
                last_activity_ms: 10,
                membership: VaultMembership::In,
            },
            VaultIndexEntry {
                conversation: conversation(FolderConversationKind::Channel, &[2; 16]),
                last_activity_ms: 20,
                membership: VaultMembership::Removed,
            },
        ];
        let bytes = encode_vault_index(entries.clone()).unwrap();
        assert_eq!(decode_vault_index(bytes).unwrap(), entries);

        let reentry = vec![VaultReentryEpoch {
            epoch: 4,
            ds_access_seed: vec![0x44; 32],
            group_info: vec![9; 12],
        }];
        let bytes = encode_devices_head(reentry.clone()).unwrap();
        assert_eq!(decode_devices_head(bytes).unwrap(), reentry);
    }

    #[test]
    fn refusals_carry_the_core_code() {
        // A record bound.
        let mut repeated = vec![VaultIndexEntry {
            conversation: conversation(FolderConversationKind::Group, &[1; 32]),
            last_activity_ms: 1,
            membership: VaultMembership::Left,
        }];
        repeated.push(repeated[0].clone());
        assert_eq!(
            code(&encode_vault_index(repeated).unwrap_err()),
            "repeatedRef"
        );
        assert_eq!(
            code(
                &encode_devices_head(vec![VaultReentryEpoch {
                    epoch: 1,
                    ds_access_seed: vec![0; 32],
                    group_info: vec![],
                }])
                .unwrap_err()
            ),
            "reentry"
        );
        // A re-entry access seed is exactly 32 bytes.
        assert_eq!(
            code(
                &encode_devices_head(vec![VaultReentryEpoch {
                    epoch: 1,
                    ds_access_seed: vec![0; 31],
                    group_info: vec![1],
                }])
                .unwrap_err()
            ),
            "seedLength"
        );
        assert!(!format!(
            "{:?}",
            VaultReentryEpoch {
                epoch: 1,
                ds_access_seed: vec![0xA7; 32],
                group_info: vec![1],
            }
        )
        .contains("167"));
        // Hostile bytes.
        assert_eq!(
            code(&decode_conversation_head(vec![]).unwrap_err()),
            "truncated"
        );
        assert_eq!(
            code(&decode_history_chunk(vec![0x01, 0, 0]).unwrap_err()),
            "unknownVersion"
        );
        let mut trailing = encode_devices_head(vec![]).unwrap();
        trailing.push(0);
        assert_eq!(
            code(&decode_devices_head(trailing).unwrap_err()),
            "trailingBytes"
        );
        // Argument lengths this module checks.
        let mut bad = plain_message(1, 1, "x");
        bad.author = Some(vec![0; 31]);
        assert_eq!(
            code(&encode_history_chunk(vec![bad]).unwrap_err()),
            "accountKeyLength"
        );
        let mut bad = plain_message(1, 1, "x");
        bad.attachments = vec![VaultAttachment {
            nonce: vec![0; 11],
            ..attachment()
        }];
        assert_eq!(
            code(&encode_history_chunk(vec![bad]).unwrap_err()),
            "nonceLength"
        );
        let mut bad = head();
        bad.registrar_seed = Some(VaultRegistrarSeed {
            rank: 1,
            seed: vec![0; 33],
        });
        assert_eq!(
            code(&encode_conversation_head(bad).unwrap_err()),
            "seedLength"
        );
    }

    #[test]
    fn merge_keeps_the_later_operation_and_sorts_by_id() {
        let a = vec![plain_message(2, 2, "two"), plain_message(1, 1, "one")];
        let b = vec![plain_message(2, 9, "two, edited"), full_message(3, 3)];
        let merged = merge_vault_messages(a.clone(), b.clone()).unwrap();
        assert_eq!(
            merged.iter().map(|m| m.message_id).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(merged[1].text, "two, edited");
        assert_eq!(merged[2], full_message(3, 3));
        // The same answer whichever side a device holds.
        assert_eq!(merge_vault_messages(b, a.clone()).unwrap(), merged);
        // Idempotent.
        assert_eq!(
            merge_vault_messages(merged.clone(), merged.clone()).unwrap(),
            merged
        );
        // And what it yields encodes.
        assert!(encode_history_chunk(merged).is_ok());

        let mut bad = plain_message(1, 1, "x");
        bad.reactions = vec![VaultReaction {
            reactor: vec![0; 5],
            emoji: "+1".into(),
        }];
        assert_eq!(
            code(&merge_vault_messages(a, vec![bad]).unwrap_err()),
            "accountKeyLength"
        );
    }

    #[test]
    fn a_signed_request_verifies_under_the_write_public_key() {
        let (_, keys) = keys();
        let vault = group(1);
        let public: [u8; 32] = keys
            .write_public_key(vault.clone())
            .unwrap()
            .try_into()
            .unwrap();
        let body = b"sealed head".to_vec();
        let header = keys
            .sign_request(
                vault.clone(),
                "PUT".into(),
                "/vault/00ff/head".into(),
                body.clone(),
            )
            .unwrap();
        proof::verify(&public, "PUT", "/vault/00ff/head", &body, &header, now_ms()).unwrap();
        // Another body, path or vault's key does not verify.
        assert!(proof::verify(
            &public,
            "PUT",
            "/vault/00ff/head",
            b"other",
            &header,
            now_ms()
        )
        .is_err());
        assert!(proof::verify(
            &public,
            "PUT",
            "/vault/00ff/chunk/0",
            &body,
            &header,
            now_ms()
        )
        .is_err());
        let other: [u8; 32] = keys.write_public_key(group(2)).unwrap().try_into().unwrap();
        assert!(
            proof::verify(&other, "PUT", "/vault/00ff/head", &body, &header, now_ms()).is_err()
        );

        assert_eq!(
            code(
                &keys
                    .sign_request(vault, "PUT".into(), "/vault/\0".into(), vec![])
                    .unwrap_err()
            ),
            "requestNotCanonical"
        );
        assert_eq!(storage_proof_header_name(), "x-lakko-storage-proof");
    }

    #[test]
    fn a_blob_delete_proof_verifies_under_the_blob_delete_public_key() {
        let (_, keys) = keys();
        let blob_id = vec![0x5C; 32];
        let public: [u8; 32] = keys
            .blob_delete_public_key(blob_id.clone())
            .unwrap()
            .try_into()
            .unwrap();
        let header = keys
            .sign_blob_delete(
                blob_id.clone(),
                "DELETE".into(),
                "/blob/5c5c".into(),
                vec![],
            )
            .unwrap();
        proof::verify(&public, "DELETE", "/blob/5c5c", b"", &header, now_ms()).unwrap();

        // Another blob has an unrelated key, and neither is a vault's.
        let other = keys.blob_delete_public_key(vec![0x5D; 32]).unwrap();
        assert_ne!(other, public.to_vec());
        let other: [u8; 32] = other.try_into().unwrap();
        assert!(proof::verify(&other, "DELETE", "/blob/5c5c", b"", &header, now_ms()).is_err());
        assert_ne!(
            keys.write_public_key(VaultRef::Index).unwrap(),
            public.to_vec()
        );

        assert_eq!(
            code(&keys.blob_delete_public_key(vec![0; 31]).unwrap_err()),
            "blobIdLength"
        );
        assert_eq!(
            code(
                &keys
                    .sign_blob_delete(vec![0; 33], "DELETE".into(), "/blob/x".into(), vec![])
                    .unwrap_err()
            ),
            "blobIdLength"
        );
    }

    /// A chunk written by the core alone - as an older or a compromised
    /// device of the account could - with media metadata an ordinary
    /// received message could never carry.
    fn chunk_with_out_of_bounds_media() -> Vec<u8> {
        let attachment = |kind: &str, duration_ms: u32, thumb_size: u32| rec::VaultAttachment {
            blob_id: [1; 32],
            key: [2; 32],
            nonce: [3; 12],
            size: 1_000,
            mime: "video/mp4".into(),
            name: String::new(),
            kind: kind.into(),
            duration_ms: Some(duration_ms),
            waveform: vec![7; 3],
            width: Some(5_000),
            height: Some(480),
            thumb: Some(rec::VaultThumb {
                blob_id: [4; 32],
                key: [5; 32],
                nonce: [6; 12],
                mime: "image/jpeg".into(),
                size: thumb_size,
                width: Some(240),
                height: Some(0),
            }),
            title: String::new(),
            artist: String::new(),
        };
        let message = rec::VaultMessage {
            message_id: 1,
            direction: rec::Direction::Incoming,
            author: None,
            created_at_ms: 0,
            last_op_id: 1,
            edited: false,
            deleted: false,
            text: String::new(),
            attachments: vec![
                attachment("video_note", 61_000, 5_000),
                attachment("gif", 1_000, 300_000),
                attachment("voice", 7_250, 5_000),
            ],
            reply: None,
            forward: None,
            reactions: vec![],
            call: None,
            unsupported_version: None,
            content: None,
        };
        rec::HistoryChunk {
            messages: vec![message],
        }
        .encode()
        .unwrap()
    }

    #[test]
    fn out_of_bounds_media_is_dropped_on_decode() {
        let bytes = chunk_with_out_of_bounds_media();
        let mut messages = decode_history_chunk(bytes.clone()).unwrap();
        let attachments = messages.pop().unwrap().attachments;
        let [note, gif, voice] = attachments.as_slice() else {
            panic!("three attachments");
        };

        // A video note longer than a minute keeps no duration; a width
        // outside 1..=4096 is dropped alone, the height beside it is kept.
        assert_eq!(note.duration_ms, None);
        assert_eq!((note.width, note.height), (None, Some(480)));
        // A thumb within its size limit is kept, its zero height dropped.
        let thumb = note.thumb.as_ref().unwrap();
        assert_eq!(
            (thumb.size, thumb.width, thumb.height),
            (5_000, Some(240), None)
        );

        // A gif has no duration; a thumb over the size limit goes whole.
        assert_eq!(gif.duration_ms, None);
        assert!(gif.thumb.is_none());

        // What is within its bound survives.
        assert_eq!(voice.kind.as_deref(), Some("voice"));
        assert_eq!(voice.duration_ms, Some(7_250));
        assert_eq!(voice.waveform, Some(vec![7; 3]));

        // The same holds on the way in: re-encoding what a client was handed
        // writes the bounded row, not the original.
        let rewritten = encode_history_chunk(decode_history_chunk(bytes.clone()).unwrap()).unwrap();
        assert_ne!(rewritten, bytes);
        assert_eq!(
            decode_history_chunk(rewritten).unwrap(),
            decode_history_chunk(bytes).unwrap()
        );
    }

    #[test]
    fn out_of_bounds_media_is_dropped_on_encode_and_merge() {
        let mut message = plain_message(1, 1, "");
        message.attachments = vec![VaultAttachment {
            kind: Some("gif".into()),
            duration_ms: Some(1_000),
            width: Some(70_000),
            thumb: Some(VaultThumb {
                size: 0,
                ..attachment().thumb.unwrap()
            }),
            ..attachment()
        }];
        let back = decode_history_chunk(encode_history_chunk(vec![message.clone()]).unwrap())
            .unwrap()
            .pop()
            .unwrap();
        let merged = merge_vault_messages(vec![message], vec![])
            .unwrap()
            .pop()
            .unwrap();
        for row in [back, merged] {
            let a = &row.attachments[0];
            assert_eq!(a.duration_ms, None);
            assert_eq!((a.width, a.height), (None, Some(480)));
            assert!(a.thumb.is_none());
        }
    }

    #[test]
    fn constants_are_the_core_values() {
        assert_eq!(vault_max_chunk_messages(), 512);
        assert_eq!(vault_max_tail_messages(), 512);
        assert_eq!(vault_max_reentry_epochs(), 2);
        assert_eq!(vault_max_people(), 4096);
        assert_eq!(vault_max_group_info_bytes(), 1 << 20);
        assert_eq!(storage_proof_window_ms(), 120_000);
        // 1 KiB bucket plus the format byte, the nonce and the tag.
        assert_eq!(vault_sealed_len(0), Some(1024 + 29));
        assert_eq!(vault_sealed_len(1020), Some(1024 + 29));
        assert_eq!(vault_sealed_len(1021), Some(2048 + 29));
        assert_eq!(vault_sealed_len(u64::MAX), None);
    }

    #[test]
    fn debug_output_redacts_keys_and_the_registrar_seed() {
        let shown = format!("{:?}", head());
        assert!(shown.contains("<redacted>"));
        // The registrar seed (0x5E x 32 = 94 decimal) and the attachment
        // keys (2 and 5) are never printed as byte lists.
        assert!(!shown.contains("94, 94"));
        assert!(!shown.contains("2, 2, 2"));
        assert!(!shown.contains("5, 5, 5"));
    }
}
