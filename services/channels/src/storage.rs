// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Storage backends for channels + posts + subscribers +
//! envelopes.
//!
//! Two implementations of the [`ChannelStore`] trait:
//!
//! 1. [`InMemoryChannelStore`] — `HashMap` behind a
//!    [`tokio::sync::RwLock`]. Used by every integration
//!    test (no MariaDB dependency in the test suite) and
//!    as the runtime default when
//!    `LAKKO_CHANNELS_DATABASE_URL` is unset.
//! 2. [`SqlChannelStore`] in [`crate::sql_store`] —
//!    MariaDB-backed, used in production. Same trait, same
//!    method signatures, same opacity contract.
//!
//! ## Opacity contract
//!
//! The server treats every post body as **opaque bytes**.
//! It never decodes the ChannelPost wire layout, never
//! verifies the Ed25519 signature, never derives the
//! AEAD key. Subscribers do all of those steps client-
//! side after fetching. The trait's API surface reinforces
//! the contract — there's no method called `decode_post`,
//! `verify_signature`, or `decrypt_payload` here, only
//! `put` and `fetch`. Same contract holds for the SQL
//! impl: every `payload` / `envelope` column is opaque
//! bytes on the way in and on the way out.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::config::DEFAULT_ENVELOPE_RETENTION_EPOCHS;
use crate::directory::{DirectoryEntry, GROWTH_WINDOW_MS};
use crate::feedback::{
    CommentReactionCounts, Label, MemoryFeedback, PostReactionCounts, StoredComment, WriterKey,
    WRITE_NONCE_LEN,
};
use crate::votes::{PostVoteCounts, Tag, VoteOutcome};

/// Prune a per-subscriber `epoch -> envelope` map down to its
/// highest `keep_n` epochs, dropping the lowest first
/// (ADR-0031 N-epoch retention). `keep_n` is always `>= 1`
/// (the store's builder clamps it), so the map never drains
/// to empty here.
fn prune_envelope_map_to_n(map: &mut BTreeMap<u32, Vec<u8>>, keep_n: usize) {
    while map.len() > keep_n {
        // `pop_first` removes the lowest-epoch entry.
        map.pop_first();
    }
}

/// 32-byte subscriber Ed25519 public key (ADR-0060), used
/// as the subscriber's identifier on the wire. Stored as
/// 64-char lowercase hex in URLs; this type is the parsed
/// binary form. The server NEVER does Diffie-Hellman against
/// this key — it only matches identities in the registry and
/// verifies the subscriber's member-card signatures under it.
/// The publisher seals the per-subscriber seed envelope to
/// its X25519 image (see
/// [`lakko_channels::subscribe::seal_seed`]). The
/// registration route refuses a key that is not a valid
/// Ed25519 point of full order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SubscriberId([u8; 32]);

impl SubscriberId {
    /// Build from a 32-byte slice.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::BadSubscriberId`] when the
    /// input is not 32 bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, StorageError> {
        if bytes.len() != 32 {
            return Err(StorageError::BadSubscriberId);
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(bytes);
        Ok(Self(out))
    }

    /// Build from a 64-char lowercase hex string.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::BadSubscriberId`] when the
    /// input is not 64 hex chars.
    pub fn from_hex(hex_str: &str) -> Result<Self, StorageError> {
        let bytes = hex::decode(hex_str).map_err(|_| StorageError::BadSubscriberId)?;
        Self::from_bytes(&bytes)
    }

    /// Render as 64-char lowercase hex.
    #[must_use]
    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }

    /// Borrow the underlying bytes — the subscriber's
    /// Ed25519 public key, ready for
    /// `lakko_channels::subscribe::seal_seed`.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// 16-byte channel pseudonym. Stored as 32-char lowercase
/// hex in the URL; this type is the parsed binary form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChannelId([u8; 16]);

impl ChannelId {
    /// Build from a 16-byte slice.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::BadChannelId`] when the
    /// input is not 16 bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, StorageError> {
        if bytes.len() != 16 {
            return Err(StorageError::BadChannelId);
        }
        let mut out = [0u8; 16];
        out.copy_from_slice(bytes);
        Ok(Self(out))
    }

    /// Build from a 32-char lowercase hex string (the URL
    /// form clients use).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::BadChannelId`] when the
    /// input is not 32 hex chars.
    pub fn from_hex(hex_str: &str) -> Result<Self, StorageError> {
        let bytes = hex::decode(hex_str).map_err(|_| StorageError::BadChannelId)?;
        Self::from_bytes(&bytes)
    }

    /// Render as 32-char lowercase hex.
    #[must_use]
    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }

    /// Borrow the underlying bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Per-post storage record. The opaque `payload` is the
/// canonically-encoded ChannelPost; the `cursor` is the
/// server-assigned monotone offset that fetch endpoints
/// use for pagination.
#[derive(Clone, Debug)]
pub struct StoredPost {
    /// Server-assigned offset, strictly monotone within a
    /// channel.
    pub cursor: u64,
    /// Server wall-clock at insert time, unix milliseconds.
    /// Surfaced to clients alongside the payload so they
    /// can sort independently if a multi-server replica
    /// ever happens.
    pub server_ts_ms: u64,
    /// Raw ChannelPost bytes. Opaque to the server.
    pub payload: Vec<u8>,
}

/// Length of a channel owner's Ed25519 public key.
pub const OWNER_PUBKEY_LEN: usize = 32;

/// Length of an owner request-proof nonce.
pub const OWNER_NONCE_LEN: usize = 16;

/// Length of a public channel's seed envelope
/// (`lakko_channels::public_seed`, ADR-0087 section 6). Opaque here.
pub const PUBLIC_SEED_LEN: usize = lakko_channels::PUBLIC_SEED_ENVELOPE_LEN;

/// Length of a private channel subscriber's sealed admission ticket
/// (`lakko_channels::invite`, ADR-0087 section 7). Opaque here: the service
/// stores it and never opens it.
pub const SEALED_TICKET_LEN: usize = lakko_channels::ADMISSION_TICKET_LEN;

/// A sealed admission ticket.
pub type SealedTicket = [u8; SEALED_TICKET_LEN];

/// Length of an invite public key.
pub const INVITE_KEY_LEN: usize = lakko_channels::INVITE_KEY_LEN;

/// `max_uses` of an invite key that may be used any number of times. `0`
/// means one use: the discovery ledger's rule, so a link's ledger row and its
/// invite key carry the same number.
pub const INVITE_USES_UNLIMITED: u32 = u32::MAX;

/// A private channel's registered invite key (ADR-0087 section 7). Names no
/// account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InviteKeyRow {
    /// The Ed25519 invite public key.
    pub invite_pk: [u8; INVITE_KEY_LEN],
    /// Unix milliseconds after which the key admits nobody; `0` = never.
    pub expiry_ms: u64,
    /// `0` = one use, [`INVITE_USES_UNLIMITED`] = unlimited, else the
    /// ceiling.
    pub max_uses: u32,
    /// How many new subscribers it let in.
    pub used_count: u32,
}

impl InviteKeyRow {
    /// Whether the key is past its expiry at `now_ms`.
    #[must_use]
    pub const fn is_expired(&self, now_ms: u64) -> bool {
        self.expiry_ms != 0 && now_ms >= self.expiry_ms
    }

    /// Whether one more subscriber may come in under the key.
    #[must_use]
    pub const fn has_uses_left(&self) -> bool {
        self.used_count < invite_use_ceiling(self.max_uses)
    }

    /// Whether a new subscriber may come in under the key at `now_ms`.
    #[must_use]
    pub const fn is_live(&self, now_ms: u64) -> bool {
        !self.is_expired(now_ms) && self.has_uses_left()
    }
}

/// The effective ceiling of a `max_uses` value: `0` is one use,
/// [`INVITE_USES_UNLIMITED`] never runs out.
#[must_use]
pub const fn invite_use_ceiling(max_uses: u32) -> u32 {
    if max_uses == 0 {
        1
    } else {
        max_uses
    }
}

/// One row of the paged subscriber list (ADR-0087 section 7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscriberEntry {
    /// The row number in insertion order; the page cursor.
    pub seq: u64,
    /// The subscriber key.
    pub subscriber: SubscriberId,
    /// The sealed admission ticket the row was filed with, if any.
    pub sealed_ticket: Option<SealedTicket>,
}

/// What [`ChannelStore::put_public_seed`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicSeedOutcome {
    /// The envelope was stored.
    Stored,
    /// The identical envelope was already stored for the epoch.
    AlreadyStored,
}

/// A decoded listing as the directory stores it (ADR-0059 section 3). The
/// canonical bytes are served back verbatim; the decoded fields feed the
/// bucket index and search only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredListing {
    /// Bucket topic: the listing's topic, or Other (0) for an id outside the
    /// known list.
    pub topic: u8,
    /// ISO 639 language code.
    pub language: String,
    /// Canonical handle.
    pub handle: String,
    /// Channel name.
    pub name: String,
    /// Channel description.
    pub description: String,
    /// The canonical `lakko/channel-listing/v1` bytes.
    pub bytes: Vec<u8>,
    /// The owner's stamp; a replacement must be strictly newer.
    pub updated_at_ms: u64,
}

impl StoredListing {
    /// Whether a lowercased `needle` occurs in the name, handle or
    /// description, case-insensitively (the in-memory search rule; the SQL
    /// store matches with `LIKE` under the table's case-insensitive
    /// collation).
    #[must_use]
    pub fn matches(&self, needle: &str) -> bool {
        self.name.to_lowercase().contains(needle)
            || self.handle.contains(needle)
            || self.description.to_lowercase().contains(needle)
    }
}

/// Which listings a directory read wants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DirectoryQuery {
    /// Every listing of one (topic, language) bucket.
    Bucket {
        /// A known topic id.
        topic: u8,
        /// ISO 639 language code.
        language: String,
    },
    /// Every listing whose name, handle or description contains `needle`
    /// (already normalised by [`crate::directory::normalize_search_query`]).
    Search {
        /// The lowercased query.
        needle: String,
    },
}

/// An operator moderation action (internal listener only, ADR-0059
/// section 7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModerationAction {
    /// Remove the listing; the owner may list again.
    Remove,
    /// Remove the listing and refuse any later listing of the channel.
    Block,
    /// Lift a block.
    Unblock,
}

/// Errors the storage layer surfaces.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// Channel id was not 16 bytes / 32 hex chars.
    #[error("channel id must be 16 bytes (32 hex chars)")]
    BadChannelId,
    /// Subscriber id was not 32 bytes / 64 hex chars.
    #[error("subscriber id must be 32 bytes (64 hex chars)")]
    BadSubscriberId,
    /// Channel does not exist in the registry. Clients must
    /// call `POST /v1/channels` to register it before
    /// posting.
    #[error("channel not found")]
    UnknownChannel,
    /// The post holds no comment at the cursor named (reactions on
    /// comments).
    #[error("comment not found under this post")]
    UnknownComment,
    /// The owner deleted the channel; its id is never registered again.
    #[error("channel was deleted")]
    ChannelDeleted,
    /// Subscriber is not registered for this channel.
    #[error("subscriber not found in this channel")]
    UnknownSubscriber,
    /// Post body exceeded the configured per-post cap.
    #[error("post body exceeds {max} bytes (got {got})")]
    PostTooLarge {
        /// Configured maximum.
        max: usize,
        /// Actual payload length.
        got: usize,
    },
    /// Post body was empty — a zero-byte ChannelPost cannot
    /// be a valid encoded form, so we reject it at the
    /// storage gate as an early sanity check.
    #[error("post body is empty")]
    EmptyPost,
    /// HPKE seed envelope did not match the expected size
    /// `lakko_channels::subscribe::SEED_ENVELOPE_LEN`.
    /// The server validates the byte length only — it
    /// never inspects the envelope content.
    #[error("seed envelope must be exactly {expected} bytes (got {got})")]
    BadEnvelopeLen {
        /// Required length (pinned to
        /// `lakko_channels::subscribe::SEED_ENVELOPE_LEN`).
        expected: usize,
        /// Length the caller actually delivered.
        got: usize,
    },
    /// The `target_epoch` supplied to `stage_envelopes` /
    /// `commit_rotation` was not exactly `current_epoch + 1`
    /// (ADR-0030). A stale / replayed / out-of-order phase
    /// call is refused so a rotation cannot roll the epoch
    /// back or skip — the monotone-target-epoch property.
    #[error("target epoch must be current + 1 (expected {expected}, got {got})")]
    StaleTargetEpoch {
        /// The only accepted target — `current_epoch + 1`.
        expected: u32,
        /// The target the caller actually supplied.
        got: u32,
    },
    /// `deliver_envelope` was asked to file an envelope at an
    /// epoch greater than the channel's `current_epoch`
    /// (ADR-0031). A publisher must not pre-seed a future
    /// epoch outside the staged / commit rotation path — this
    /// preserves C-42, so `channel_envelopes` only ever holds
    /// epochs `<= current_epoch`.
    #[error("envelope epoch {got} is in the future (current epoch is {current})")]
    EnvelopeEpochInFuture {
        /// The channel's live epoch at refusal time.
        current: u32,
        /// The future epoch the caller supplied.
        got: u32,
    },
    /// `create` named an existing channel with a different owner key. A
    /// channel's owner key is fixed at creation (ADR-0059 section 1).
    #[error("channel exists with a different owner key")]
    OwnerMismatch,
    /// Appointing one more co-publisher key would exceed the per-channel
    /// cap (`lakko_channels::MAX_COPUBLISHERS`).
    #[error("too many co-publisher keys for this channel")]
    TooManyCoPublishers,
    /// A listing whose `updated_at_ms` is not newer than the stored one.
    #[error("listing is not newer than the stored one")]
    StaleListing,
    /// The operator blocked this channel from the directory.
    #[error("channel is blocked from the directory")]
    ListingBlocked,
    /// A member card names an epoch greater than the channel's
    /// `current_epoch` (ADR-0060): a card is sealed under a seed a reader
    /// already holds, never a future one.
    #[error("member card epoch {got} is in the future (current epoch is {current})")]
    MemberCardEpochInFuture {
        /// The channel's live epoch at refusal time.
        current: u32,
        /// The epoch the card carries.
        got: u32,
    },
    /// A member card names an epoch lower than the card it would replace
    /// (ADR-0060): a card only ever moves forward, so a replayed old card
    /// cannot roll a subscriber's profile back to a retired seed.
    #[error("member card epoch {got} is older than the stored card's {stored}")]
    MemberCardStale {
        /// The stored card's epoch.
        stored: u32,
        /// The epoch the refused card carries.
        got: u32,
    },
    /// The operation is for public channels and the channel is private
    /// (ADR-0087: a listing or a public seed on a private channel).
    #[error("the channel is private")]
    ChannelPrivate,
    /// The operation is for private channels and the channel is public
    /// (ADR-0087: a publisher removing a subscriber).
    #[error("the channel is public")]
    ChannelPublic,
    /// A public seed for an epoch beyond `current_epoch + 1` (ADR-0087
    /// section 6). The next epoch's seed may be filed before the rotation
    /// commits, nothing further.
    #[error("public seed epoch {got} is beyond the next epoch (current epoch is {current})")]
    PublicSeedEpochInFuture {
        /// The channel's live epoch at refusal time.
        current: u32,
        /// The epoch the caller named.
        got: u32,
    },
    /// A different public seed is already stored for the epoch. A stored
    /// seed is immutable (caches hold it).
    #[error("a different public seed is stored for this epoch")]
    PublicSeedConflict,
    /// Registering one more invite key would exceed the per-channel cap.
    #[error("too many invite keys for this channel")]
    TooManyInviteKeys,
    /// The channel holds no post at the cursor a reaction or comment names
    /// (ADR-0087 section 13a).
    #[error("post not found")]
    UnknownPost,
    /// Database-level failure — connection lost, query
    /// timeout, constraint violation, anything sqlx
    /// surfaces. The string carries the underlying
    /// `sqlx::Error`'s `Display` so logs point at the
    /// real cause; the API layer maps this to 500.
    #[error("database error: {0}")]
    Database(String),
}

/// Storage backend used by the channels service. Object-
/// safe (via `#[async_trait]`) so `AppState` can hold an
/// `Arc<dyn ChannelStore + Send + Sync>` and dispatch
/// dynamically between the in-memory and SQL impls.
///
/// All methods are async — even on the in-memory path
/// the lock is async so a slow `await` elsewhere in the
/// request handler does not block a tokio worker.
#[async_trait]
pub trait ChannelStore: Send + Sync {
    /// Register a fresh channel under its owner's public key (ADR-0059
    /// section 1). Idempotent for the same key; a different key for an
    /// existing channel is [`StorageError::OwnerMismatch`] and changes
    /// nothing, so nobody can take a channel over by re-creating it.
    async fn create(
        &self,
        channel: ChannelId,
        owner_pubkey: [u8; OWNER_PUBKEY_LEN],
    ) -> Result<(), StorageError> {
        self.create_with_access(channel, owner_pubkey, false).await
    }

    /// [`Self::create`] with the channel's flag (ADR-0087): `private` is
    /// applied only when the row is created; re-creating an existing channel
    /// never changes it ([`Self::set_private`] does, owner-signed).
    async fn create_with_access(
        &self,
        channel: ChannelId,
        owner_pubkey: [u8; OWNER_PUBKEY_LEN],
        private: bool,
    ) -> Result<(), StorageError>;

    /// Whether `channel` is private; `None` for an unknown channel.
    async fn is_private(&self, channel: ChannelId) -> Result<Option<bool>, StorageError>;

    /// Set the channel's flag (ADR-0087 section 8). Going private withdraws
    /// the listing and deletes every public seed in the same step, since a
    /// private channel is never listed and its seeds are sealed to each key.
    /// Going public changes only the flag. Returns whether the flag changed.
    ///
    /// # Errors
    ///
    /// [`StorageError::UnknownChannel`].
    async fn set_private(&self, channel: ChannelId, private: bool) -> Result<bool, StorageError>;

    /// File a public channel's seed envelope for `epoch` (ADR-0087 section
    /// 6). Immutable once stored: the identical envelope again is
    /// [`PublicSeedOutcome::AlreadyStored`], another is
    /// [`StorageError::PublicSeedConflict`]. Storing epoch `e` marks every
    /// lower epoch's seed superseded at `now_ms`, which starts its prune
    /// clock.
    ///
    /// # Errors
    ///
    /// [`StorageError::UnknownChannel`], [`StorageError::ChannelPrivate`],
    /// [`StorageError::PublicSeedEpochInFuture`] (`epoch > current + 1`),
    /// [`StorageError::PublicSeedConflict`].
    async fn put_public_seed(
        &self,
        channel: ChannelId,
        epoch: u32,
        envelope: [u8; PUBLIC_SEED_LEN],
        now_ms: u64,
    ) -> Result<PublicSeedOutcome, StorageError>;

    /// The public seed envelope of `(channel, epoch)`, if stored.
    async fn public_seed(
        &self,
        channel: ChannelId,
        epoch: u32,
    ) -> Result<Option<[u8; PUBLIC_SEED_LEN]>, StorageError>;

    /// Drop public seeds superseded before `cutoff_ms` (the post retention
    /// cutoff: every post such a seed opens is gone by then). Returns how
    /// many.
    async fn prune_public_seeds_superseded_before(
        &self,
        cutoff_ms: u64,
    ) -> Result<u64, StorageError>;

    /// Register or update an invite key (ADR-0087 section 7). A key already
    /// registered keeps its use count and takes the new expiry and ceiling.
    ///
    /// # Errors
    ///
    /// [`StorageError::UnknownChannel`]; [`StorageError::TooManyInviteKeys`]
    /// when `max_keys` keys are registered and this one is new.
    async fn put_invite_key(
        &self,
        channel: ChannelId,
        invite_pk: [u8; INVITE_KEY_LEN],
        expiry_ms: u64,
        max_uses: u32,
        max_keys: usize,
        now_ms: u64,
    ) -> Result<(), StorageError>;

    /// Revoke an invite key. `Ok(false)` when it was not registered.
    async fn delete_invite_key(
        &self,
        channel: ChannelId,
        invite_pk: [u8; INVITE_KEY_LEN],
    ) -> Result<bool, StorageError>;

    /// The registered invite key, if any.
    async fn invite_key(
        &self,
        channel: ChannelId,
        invite_pk: [u8; INVITE_KEY_LEN],
    ) -> Result<Option<InviteKeyRow>, StorageError>;

    /// Every registered invite key of `channel` (at most the cap), ascending
    /// by key.
    async fn list_invite_keys(&self, channel: ChannelId)
        -> Result<Vec<InviteKeyRow>, StorageError>;

    /// Count one use of the key if it is live at `now_ms`, atomically.
    /// `Ok(false)` when it is not registered, expired or used up.
    async fn claim_invite_use(
        &self,
        channel: ChannelId,
        invite_pk: [u8; INVITE_KEY_LEN],
        now_ms: u64,
    ) -> Result<bool, StorageError>;

    /// Give back one use claimed by [`Self::claim_invite_use`] (the request
    /// that claimed it was then refused). Never below zero.
    async fn release_invite_use(
        &self,
        channel: ChannelId,
        invite_pk: [u8; INVITE_KEY_LEN],
    ) -> Result<(), StorageError>;

    /// Drop invite keys whose expiry is before `cutoff_ms` (never one with
    /// no expiry). Returns how many.
    async fn prune_invite_keys_expired_before(&self, cutoff_ms: u64) -> Result<u64, StorageError>;

    /// The owner key registered for `channel`, `None` for an unknown
    /// channel.
    async fn owner_pubkey(
        &self,
        channel: ChannelId,
    ) -> Result<Option<[u8; OWNER_PUBKEY_LEN]>, StorageError>;

    /// Replace the owner key of `channel` (ADR-0074 section 3.5: the owning
    /// account changed its keys), as a compare-and-swap: the key becomes
    /// `new_owner` only while it is still `current_owner`, the key the
    /// caller verified the handover under. `Ok(true)` when it was replaced,
    /// `Ok(false)` when the stored key is no longer `current_owner` (a
    /// concurrent handover won) and nothing changed. The co-publisher
    /// appointments, the listing and everything else about the channel are
    /// untouched: none of them is signed by the owner key.
    ///
    /// # Errors
    ///
    /// [`StorageError::UnknownChannel`] when the channel does not exist.
    async fn replace_owner(
        &self,
        channel: ChannelId,
        current_owner: [u8; OWNER_PUBKEY_LEN],
        new_owner: [u8; OWNER_PUBKEY_LEN],
    ) -> Result<bool, StorageError>;

    /// Record an owner request-proof nonce. `Ok(false)` when it was already
    /// recorded (a replay).
    async fn consume_owner_nonce(
        &self,
        nonce: [u8; OWNER_NONCE_LEN],
        now_ms: u64,
    ) -> Result<bool, StorageError>;

    /// Drop nonces recorded before `cutoff_ms`. Returns how many.
    async fn prune_owner_nonces_older_than(&self, cutoff_ms: u64) -> Result<u64, StorageError>;

    /// Appoint a co-publisher key for `channel` (ADR-0059 section 1,
    /// co-publisher keys). Idempotent for a key already appointed. Refused
    /// with [`StorageError::UnknownChannel`] or, when `max` keys are already
    /// appointed, [`StorageError::TooManyCoPublishers`].
    async fn appoint_copublisher(
        &self,
        channel: ChannelId,
        key: [u8; OWNER_PUBKEY_LEN],
        max: usize,
        now_ms: u64,
    ) -> Result<(), StorageError>;

    /// Revoke a co-publisher key. `Ok(false)` when it was not appointed.
    async fn revoke_copublisher(
        &self,
        channel: ChannelId,
        key: [u8; OWNER_PUBKEY_LEN],
    ) -> Result<bool, StorageError>;

    /// Whether `key` is an appointed co-publisher key of `channel`.
    async fn is_copublisher(
        &self,
        channel: ChannelId,
        key: [u8; OWNER_PUBKEY_LEN],
    ) -> Result<bool, StorageError>;

    /// Store or replace the channel's listing. The first listing records
    /// `now_ms` as the first-listed time, which a later withdrawal keeps.
    ///
    /// Refused with [`StorageError::UnknownChannel`],
    /// [`StorageError::ListingBlocked`] or [`StorageError::StaleListing`]
    /// (not strictly newer than the stored listing).
    async fn put_listing(
        &self,
        channel: ChannelId,
        listing: StoredListing,
        now_ms: u64,
    ) -> Result<(), StorageError>;

    /// Withdraw the channel's listing. `Ok(false)` when none was listed.
    async fn withdraw_listing(&self, channel: ChannelId) -> Result<bool, StorageError>;

    /// The channel's listing with its counts at `now_ms`, `None` when it is
    /// not listed.
    async fn listing(
        &self,
        channel: ChannelId,
        now_ms: u64,
    ) -> Result<Option<DirectoryEntry>, StorageError>;

    /// Every listing matching `query`, with counts at `now_ms`, unordered.
    /// Ranking and the page cut are [`crate::directory::rank`]'s.
    async fn directory_candidates(
        &self,
        query: &DirectoryQuery,
        now_ms: u64,
    ) -> Result<Vec<DirectoryEntry>, StorageError>;

    /// Apply an operator moderation action with a reason code. Works for a
    /// channel id that does not exist (a block may precede creation).
    async fn moderate(
        &self,
        channel: ChannelId,
        action: ModerationAction,
        reason: &str,
        now_ms: u64,
    ) -> Result<(), StorageError>;

    /// `true` if the channel is registered.
    async fn exists(&self, channel: ChannelId) -> bool;

    /// Append a post. Returns the freshly-stored record.
    async fn put(
        &self,
        channel: ChannelId,
        payload: Vec<u8>,
        max_bytes: usize,
        server_ts_ms: u64,
    ) -> Result<StoredPost, StorageError>;

    /// Fetch posts strictly after `after`, up to `limit`.
    async fn fetch(
        &self,
        channel: ChannelId,
        after: u64,
        limit: usize,
    ) -> Result<Vec<StoredPost>, StorageError>;

    /// Total number of posts stored across every channel.
    async fn total_posts(&self) -> usize;

    /// Delete every post across every channel whose
    /// `server_ts_ms` is strictly less than
    /// `cutoff_ms`. Returns the number of rows that were
    /// removed. Idempotent and safe to run concurrently
    /// with `put` / `fetch` (the in-memory impl holds the
    /// write lock for the duration of the prune; the
    /// SQL impl emits a single bulk DELETE).
    ///
    /// Cursor counters on `channels.next_cursor` are
    /// **not** rolled back — cursors stay monotone across
    /// pruning so a paginating subscriber's
    /// `WHERE cursor > ?` resume point continues to
    /// point at the same post if it survived, and at the
    /// next-survivor otherwise.
    async fn prune_posts_older_than(&self, cutoff_ms: u64) -> Result<u64, StorageError>;

    /// Register a subscriber against the channel.
    async fn subscribe(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
        server_ts_ms: u64,
    ) -> Result<(), StorageError> {
        self.subscribe_with_ticket(channel, subscriber, server_ts_ms, None)
            .await
            .map(|_| ())
    }

    /// Register a subscriber with an optional sealed admission ticket
    /// (ADR-0087 section 7). A new row gets the next row number and moves the
    /// subscriber counter; an existing row is a re-subscribe (activity, and
    /// the ticket replaced when one is given). Clears a removal record of the
    /// key. Returns whether the row is new.
    ///
    /// # Errors
    ///
    /// [`StorageError::UnknownChannel`].
    async fn subscribe_with_ticket(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
        server_ts_ms: u64,
        sealed_ticket: Option<SealedTicket>,
    ) -> Result<bool, StorageError>;

    /// A page of subscriber rows in insertion order, strictly after row
    /// number `after_seq` (`0` = from the start), at most `limit`.
    ///
    /// # Errors
    ///
    /// [`StorageError::UnknownChannel`].
    async fn list_subscriber_page(
        &self,
        channel: ChannelId,
        after_seq: u64,
        limit: usize,
    ) -> Result<Vec<SubscriberEntry>, StorageError>;

    /// The maintained count of the channel's subscriber rows.
    ///
    /// # Errors
    ///
    /// [`StorageError::UnknownChannel`].
    async fn subscriber_count(&self, channel: ChannelId) -> Result<u64, StorageError>;

    /// A publisher removes a subscriber from a private channel (ADR-0087
    /// section 7): as [`Self::unsubscribe`], and the key is recorded as
    /// removed so its envelope read answers so. Idempotent. Returns whether a
    /// row was removed.
    ///
    /// # Errors
    ///
    /// [`StorageError::UnknownChannel`], [`StorageError::ChannelPublic`].
    async fn remove_subscriber(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
        now_ms: u64,
    ) -> Result<bool, StorageError>;

    /// Whether a publisher removed `subscriber` from `channel` (and it has
    /// not subscribed again since).
    async fn is_removed(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
    ) -> Result<bool, StorageError>;

    /// `true` if the subscriber is registered on the
    /// channel.
    async fn is_subscribed(&self, channel: ChannelId, subscriber: SubscriberId) -> bool;

    /// Mark the subscriber as active right now — bump
    /// `last_active_at` so the stale-subscriber pruner
    /// does not GC them on its next tick. Called by the
    /// API layer on every successful envelope GET so the
    /// subscriber's own poll loop is the activity
    /// signal; no separate heartbeat protocol.
    ///
    /// Idempotent: no-op if the subscriber is not
    /// registered (returns `Ok(())` — touch is a hint,
    /// not a contract).
    async fn touch_subscriber_activity(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
        now_ms: u64,
    ) -> Result<(), StorageError>;

    /// Delete every subscriber across every channel
    /// whose `last_active_at` is strictly less than
    /// `cutoff_ms`, moving each channel's subscriber counter,
    /// and every removal record older than `cutoff_ms`.
    /// Returns the number of subscriber
    /// rows removed (envelopes are cascaded out by the
    /// FK on the SQL side; the in-memory impl drops
    /// the matching envelope entries explicitly).
    ///
    /// Idempotent and safe to run concurrently with
    /// `subscribe` / `deliver_envelope` / `rotate`
    /// (each impl holds the appropriate lock for the
    /// duration of the prune).
    async fn prune_subscribers_older_than(&self, cutoff_ms: u64) -> Result<u64, StorageError>;

    /// List every subscriber registered on the channel.
    async fn list_subscribers(&self, channel: ChannelId)
        -> Result<Vec<SubscriberId>, StorageError>;

    /// Store the publisher-supplied HPKE seed envelope for
    /// `(channel, subscriber)` at a concrete epoch, then prune
    /// that subscriber's retained set to the store's highest-N
    /// epochs (ADR-0031). Envelope is opaque.
    ///
    /// `epoch` selects the epoch the envelope is filed under;
    /// `None` defaults to the channel's `current_epoch`. An
    /// epoch **greater than** `current_epoch` is refused with
    /// [`StorageError::EnvelopeEpochInFuture`] — a publisher
    /// cannot pre-seed a future epoch outside the staged /
    /// commit rotation path (C-42).
    async fn deliver_envelope(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
        epoch: Option<u32>,
        envelope: Vec<u8>,
        expected_len: usize,
    ) -> Result<(), StorageError>;

    /// Fetch the HPKE seed envelope at the **highest** retained
    /// epoch for `(channel, subscriber)` — the current seed a
    /// fresh subscriber needs. Returns `Ok(None)` when the
    /// subscriber is registered but no envelope has been
    /// delivered yet.
    ///
    /// The highest retained epoch is always `<= current_epoch`
    /// (the future-epoch refusal in [`Self::deliver_envelope`]
    /// plus the atomic rotate/commit guarantee that), so this
    /// wire is backward-compatible with the pre-ADR-0031 N=1
    /// behaviour.
    async fn fetch_envelope(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
    ) -> Result<Option<Vec<u8>>, StorageError>;

    /// Fetch the whole retained `[(epoch, envelope)]` set for
    /// `(channel, subscriber)`, ascending by epoch (ADR-0031) —
    /// what a catching-up subscriber opens into its
    /// `epoch -> seed` map. Empty when the subscriber is
    /// registered but no envelope has been delivered yet.
    ///
    /// Gated exactly like [`Self::fetch_envelope`]: unknown
    /// channel / unregistered subscriber surface as errors.
    async fn fetch_envelopes(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
    ) -> Result<Vec<(u32, Vec<u8>)>, StorageError>;

    /// Total number of subscribers across every channel.
    async fn total_subscribers(&self) -> usize;

    /// Read the channel's current epoch id.
    async fn current_epoch(&self, channel: ChannelId) -> Result<u32, StorageError>;

    /// Atomic rotation: bump the current epoch + replace
    /// the per-subscriber envelopes for every entry in the
    /// batch. Either every step lands or none.
    async fn rotate(
        &self,
        channel: ChannelId,
        batch: Vec<(SubscriberId, Vec<u8>)>,
        expected_envelope_len: usize,
    ) -> Result<u32, StorageError>;

    /// Stage a chunk of the next epoch's envelopes into the
    /// staging area (ADR-0030 prepare phase). Does **not**
    /// bump the epoch and the staged rows are **never**
    /// served by [`Self::fetch_envelope`] — subscribers keep
    /// seeing the current live epoch until [`Self::commit_rotation`]
    /// runs.
    ///
    /// Validates like [`Self::rotate`]'s first pass: every
    /// envelope must be `expected_envelope_len` bytes, the
    /// channel must exist, `target_epoch` must equal
    /// `current_epoch + 1`, and every subscriber must be
    /// registered — any failure aborts the chunk without
    /// staging anything. On success each `(subscriber →
    /// envelope)` is an idempotent upsert keyed on
    /// `(channel, target_epoch, subscriber)`, so a client
    /// that dropped mid-fanout simply re-sends the missing
    /// chunks (resumability).
    async fn stage_envelopes(
        &self,
        channel: ChannelId,
        target_epoch: u32,
        batch: Vec<(SubscriberId, Vec<u8>)>,
        expected_envelope_len: usize,
    ) -> Result<(), StorageError>;

    /// Commit a staged rotation (ADR-0030 commit phase) —
    /// the single atomic point. In one transaction: re-verify
    /// `target_epoch == current_epoch + 1`, promote the
    /// staged envelopes for that `target_epoch` into the live
    /// set, bump `current_epoch` to `target_epoch`, and clear
    /// the staged rows for the channel. Returns the new epoch.
    ///
    /// Partial coverage is allowed: a registered subscriber
    /// with no staged row keeps their prior-epoch envelope,
    /// and a staged row whose subscriber is no longer
    /// registered (e.g. pruned between stage and commit) is
    /// skipped rather than aborting the commit. C-42 holds —
    /// a subscriber observes either the old epoch or the new
    /// one, never a mix.
    async fn commit_rotation(
        &self,
        channel: ChannelId,
        target_epoch: u32,
    ) -> Result<u32, StorageError>;

    /// Store `(channel, subscriber)`'s member card (ADR-0060), replacing the
    /// previous one. `epoch` is the card's clear epoch, read by the caller;
    /// the card itself is opaque.
    ///
    /// # Errors
    ///
    /// [`StorageError::UnknownChannel`], [`StorageError::UnknownSubscriber`],
    /// [`StorageError::MemberCardEpochInFuture`] (`epoch > current_epoch`),
    /// [`StorageError::MemberCardStale`] (`epoch` below the stored card's).
    async fn put_member_card(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
        epoch: u32,
        card: Vec<u8>,
    ) -> Result<(), StorageError>;

    /// Delete `(channel, subscriber)`'s member card. Idempotent: an absent
    /// card (or subscriber) is not an error.
    ///
    /// # Errors
    ///
    /// [`StorageError::UnknownChannel`].
    async fn delete_member_card(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
    ) -> Result<(), StorageError>;

    /// A page of member cards, ascending by subscriber key, starting
    /// strictly after `after` (from the beginning when `None`), at most
    /// `limit`.
    ///
    /// # Errors
    ///
    /// [`StorageError::UnknownChannel`].
    async fn list_member_cards(
        &self,
        channel: ChannelId,
        after: Option<SubscriberId>,
        limit: usize,
    ) -> Result<Vec<(SubscriberId, Vec<u8>)>, StorageError>;

    /// Remove `(channel, subscriber)` (ADR-0060 unsubscribe): the subscriber
    /// row, every retained seed envelope, the member card and any staged
    /// envelope for it, in one step. Idempotent: an absent subscriber is not
    /// an error.
    ///
    /// # Errors
    ///
    /// [`StorageError::UnknownChannel`].
    async fn unsubscribe(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
    ) -> Result<(), StorageError>;

    // ---------------------------------------------------------
    // Comments and reactions (ADR-0087 section 13a)
    // ---------------------------------------------------------

    /// Whether the channel holds a post at `post_cursor`: the check an add
    /// runs before it spends a token. A keyed lookup.
    ///
    /// # Errors
    ///
    /// [`StorageError::Database`] only.
    async fn post_exists(&self, channel: ChannelId, post_cursor: u64)
        -> Result<bool, StorageError>;

    /// Store the reaction row `(post, writer_key, label)`. Returns whether it
    /// is new (an existing row is left as it is).
    ///
    /// # Errors
    ///
    /// [`StorageError::UnknownPost`] when the channel holds no post at
    /// `post_cursor` (never stored, or pruned).
    async fn add_reaction(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        writer_key: WriterKey,
        label: Label,
        now_ms: u64,
    ) -> Result<bool, StorageError>;

    /// Remove the reaction row; returns whether one was there.
    ///
    /// # Errors
    ///
    /// [`StorageError::Database`] only.
    async fn delete_reaction(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        writer_key: WriterKey,
        label: Label,
    ) -> Result<bool, StorageError>;

    /// The label counts of each post in `post_cursors`, in that order (a post
    /// with no rows, or none at all, gets an empty list). See
    /// [`PostReactionCounts`] for the ranking.
    ///
    /// # Errors
    ///
    /// [`StorageError::Database`] only.
    async fn reaction_counts(
        &self,
        channel: ChannelId,
        post_cursors: &[u64],
    ) -> Result<Vec<PostReactionCounts>, StorageError>;

    /// Store a comment row and return its cursor.
    ///
    /// # Errors
    ///
    /// [`StorageError::UnknownPost`].
    async fn add_comment(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        writer_key: WriterKey,
        sealed: Vec<u8>,
        now_ms: u64,
    ) -> Result<u64, StorageError>;

    /// The write key of the comment at `comment_cursor` under the post, or
    /// `None` when there is no such comment.
    ///
    /// # Errors
    ///
    /// [`StorageError::Database`] only.
    async fn comment_writer(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        comment_cursor: u64,
    ) -> Result<Option<WriterKey>, StorageError>;

    /// Remove the comment row; returns whether one was there.
    ///
    /// # Errors
    ///
    /// [`StorageError::Database`] only.
    async fn delete_comment(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        comment_cursor: u64,
    ) -> Result<bool, StorageError>;

    /// The comments under the post with a cursor above `after`, ascending, at
    /// most `limit`.
    ///
    /// # Errors
    ///
    /// [`StorageError::Database`] only.
    async fn list_comments(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        after: u64,
        limit: usize,
    ) -> Result<Vec<StoredComment>, StorageError>;

    /// Record a write-proof nonce; `false` when it was seen before.
    ///
    /// # Errors
    ///
    /// [`StorageError::Database`].
    async fn consume_write_nonce(
        &self,
        nonce: [u8; WRITE_NONCE_LEN],
        now_ms: u64,
    ) -> Result<bool, StorageError>;

    /// Forget write-proof nonces seen before `cutoff_ms`.
    ///
    /// # Errors
    ///
    /// [`StorageError::Database`].
    async fn prune_write_nonces_older_than(&self, cutoff_ms: u64) -> Result<u64, StorageError>;

    /// The number of comments under each post in `post_cursors`, in that
    /// order (0 for a post with none, or none at all). A maintained counter,
    /// not a count of rows (ADR-0087 phase B, 2026-11-05).
    ///
    /// # Errors
    ///
    /// [`StorageError::Database`] only.
    async fn comment_counts(
        &self,
        channel: ChannelId,
        post_cursors: &[u64],
    ) -> Result<Vec<(u64, u64)>, StorageError>;

    /// Store the reaction row `(comment, writer_key, label)` on the comment
    /// `comment_cursor` under the post. Returns whether it is new.
    ///
    /// # Errors
    ///
    /// [`StorageError::UnknownComment`] when the post holds no such comment.
    async fn add_comment_reaction(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        comment_cursor: u64,
        writer_key: WriterKey,
        label: Label,
        now_ms: u64,
    ) -> Result<bool, StorageError>;

    /// Remove a reaction row on a comment; returns whether one was there.
    ///
    /// # Errors
    ///
    /// [`StorageError::Database`] only.
    async fn delete_comment_reaction(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        comment_cursor: u64,
        writer_key: WriterKey,
        label: Label,
    ) -> Result<bool, StorageError>;

    /// The label counts of each comment in `comment_cursors` under the post,
    /// in that order (a comment that is not there gets an empty list).
    ///
    /// # Errors
    ///
    /// [`StorageError::Database`] only.
    async fn comment_reaction_counts(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        comment_cursors: &[u64],
    ) -> Result<Vec<CommentReactionCounts>, StorageError>;

    /// Store, replace (non-empty `labels`, sorted) or retract (empty) the
    /// channel poll vote of `tag` under the post, when `at_ms` is strictly
    /// greater than the stored one (ADR-0092 section 4.3); the per-label
    /// counters and the voter count move in the same transaction. A retract
    /// keeps the row with no labels so its `at_ms` still orders.
    ///
    /// # Errors
    ///
    /// [`StorageError::UnknownPost`] when the channel holds no post at
    /// `post_cursor`.
    async fn apply_vote(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        tag: Tag,
        labels: Vec<Label>,
        at_ms: u64,
    ) -> Result<VoteOutcome, StorageError>;

    /// The vote counts of each post in `post_cursors`, in that order.
    ///
    /// # Errors
    ///
    /// [`StorageError::Database`] only.
    async fn vote_counts(
        &self,
        channel: ChannelId,
        post_cursors: &[u64],
    ) -> Result<Vec<PostVoteCounts>, StorageError>;

    /// Delete the channel (ADR-0087 phase B, 2026-11-05): every row of it
    /// goes, and a tombstone stays so its reads answer `410 channel-deleted`
    /// and nobody registers the id again. Returns whether a channel was
    /// there. Idempotent.
    ///
    /// # Errors
    ///
    /// [`StorageError::Database`] only.
    async fn delete_channel(&self, channel: ChannelId, now_ms: u64) -> Result<bool, StorageError>;

    /// Whether the owner deleted `channel`.
    ///
    /// # Errors
    ///
    /// [`StorageError::Database`] only.
    async fn is_deleted(&self, channel: ChannelId) -> Result<bool, StorageError>;
}

/// One channel's public seeds: epoch → (envelope, superseded at).
type PublicSeeds = BTreeMap<u32, ([u8; PUBLIC_SEED_LEN], Option<u64>)>;

/// In-memory state container. Cheap to clone — the inner
/// state is behind an `Arc<RwLock<…>>`.
#[derive(Clone)]
pub struct InMemoryChannelStore {
    inner: Arc<RwLock<Inner>>,
    /// The most-recent N seed envelopes retained per
    /// `(channel, subscriber)` (ADR-0031). Always `>= 1` —
    /// [`InMemoryChannelStore::with_envelope_retention`]
    /// clamps it and the default is
    /// [`DEFAULT_ENVELOPE_RETENTION_EPOCHS`].
    envelope_retention_epochs: usize,
}

impl Default for InMemoryChannelStore {
    /// The default retains [`DEFAULT_ENVELOPE_RETENTION_EPOCHS`]
    /// (4) epochs per subscriber — never 0, so a `#[derive]`d
    /// `Default` (which would zero the field and prune every
    /// envelope) is deliberately avoided.
    fn default() -> Self {
        Self {
            inner: Arc::new(RwLock::new(Inner::default())),
            envelope_retention_epochs: DEFAULT_ENVELOPE_RETENTION_EPOCHS,
        }
    }
}

#[derive(Default)]
struct Inner {
    /// Channel id → ordered post list.
    posts: HashMap<ChannelId, Vec<StoredPost>>,
    /// Next cursor to hand out per channel (1-based; 0 is
    /// reserved as "before any post" in the fetch query).
    next_cursor: HashMap<ChannelId, u64>,
    /// Channel id → set of registered subscribers.
    /// `HashMap` rather than `HashSet` lets us iterate
    /// in insertion-stable order on the publisher's
    /// fan-out path; the value is the **last-active-at**
    /// timestamp (unix-ms) used by the stale-subscriber
    /// pruner. Bumped on every successful envelope GET
    /// via [`InMemoryChannelStore::touch_subscriber_activity`]
    /// and re-bumped to the registration time on each
    /// idempotent re-subscribe.
    subscribers: HashMap<ChannelId, HashMap<SubscriberId, u64>>,
    /// (Channel, Subscriber) → `epoch -> HPKE-sealed seed
    /// envelope` map (ADR-0031). The most-recent N epochs are
    /// retained per subscriber (`envelope_retention_epochs`,
    /// default 4); each write files at a concrete epoch and
    /// prunes the lowest epochs beyond N. A `BTreeMap` keeps
    /// the epochs sorted so "highest retained" (the current
    /// seed) and "whole set ascending" (the catch-up set) are
    /// both cheap. The server never opens or parses a seed —
    /// the `epoch` key is bookkeeping alongside opaque bytes.
    envelopes: HashMap<(ChannelId, SubscriberId), BTreeMap<u32, Vec<u8>>>,
    /// Channel id → current epoch id. Server-side bookkeeping
    /// only; the authoritative epoch lives in each
    /// `ChannelPost`'s header on the client. Surfaced via
    /// `GET /v1/channels/:id/epoch` so a subscriber that
    /// comes back online after weeks can ask "did I miss a
    /// rotation?" before deciding to refresh its envelope.
    current_epoch: HashMap<ChannelId, u32>,
    /// (Channel, target_epoch) → (Subscriber → staged HPKE
    /// seed envelope). The ADR-0030 staging area for the
    /// two-phase rotation fanout. NEVER served by
    /// `fetch_envelope` — `commit_rotation` is the only path
    /// that promotes a staged envelope into `envelopes`. So
    /// the prepare phase is invisible to subscribers (no
    /// torn rotation is observable mid-stage) and re-staging
    /// the same key is an idempotent overwrite (resumability).
    staged_envelopes: HashMap<(ChannelId, u32), HashMap<SubscriberId, Vec<u8>>>,
    /// Channel id → (subscriber → (epoch, opaque member card)), ADR-0060.
    /// A `BTreeMap` so a page in subscriber-key order is a range scan.
    member_cards: HashMap<ChannelId, BTreeMap<SubscriberId, (u32, Vec<u8>)>>,
    /// Channel id → owner key (ADR-0059 section 1).
    owners: HashMap<ChannelId, [u8; OWNER_PUBKEY_LEN]>,
    /// Channel id → (subscriber → first registration time). Kept across
    /// re-subscribes; feeds the directory's 7-day growth signal.
    registered_at: HashMap<ChannelId, HashMap<SubscriberId, u64>>,
    /// Channel id → appointed co-publisher keys → when appointed.
    copublishers: HashMap<ChannelId, HashMap<[u8; OWNER_PUBKEY_LEN], u64>>,
    /// Owner request-proof nonces → when seen.
    owner_nonces: HashMap<[u8; OWNER_NONCE_LEN], u64>,
    /// Channel id → its published listing.
    listings: HashMap<ChannelId, StoredListing>,
    /// Channel id → first-listed time; survives a withdrawal.
    first_listed_at: HashMap<ChannelId, u64>,
    /// Channel id → (blocked, reason, action time).
    moderation: HashMap<ChannelId, (bool, String, u64)>,
    /// Channel id → private flag (ADR-0087); absent = public.
    private: HashMap<ChannelId, bool>,
    /// The last row number handed out (global, like the SQL
    /// `AUTO_INCREMENT`).
    last_seq: u64,
    /// Channel id → row number → subscriber, the paged list's order.
    subscriber_rows: HashMap<ChannelId, BTreeMap<u64, SubscriberId>>,
    /// (Channel, Subscriber) → (row number, sealed ticket).
    subscriber_meta: HashMap<(ChannelId, SubscriberId), (u64, Option<SealedTicket>)>,
    /// Channel id → epoch → (public seed envelope, superseded at).
    public_seeds: HashMap<ChannelId, PublicSeeds>,
    /// Channel id → invite key → row.
    invite_keys: HashMap<ChannelId, BTreeMap<[u8; INVITE_KEY_LEN], InviteKeyRow>>,
    /// (Channel, Subscriber) removed by a publisher → when.
    removed: HashMap<(ChannelId, SubscriberId), u64>,
    /// Comments and reactions (ADR-0087 section 13a).
    feedback: MemoryFeedback,
    /// Channel id → when its owner deleted it (ADR-0087 phase B).
    deleted: HashMap<ChannelId, u64>,
}

impl Inner {
    /// Drop every trace of `channel`: the SQL store's cascade from
    /// `channels`. Returns whether the channel was there.
    fn drop_channel(&mut self, channel: ChannelId) -> bool {
        let existed = self.posts.remove(&channel).is_some();
        self.next_cursor.remove(&channel);
        self.subscribers.remove(&channel);
        self.envelopes.retain(|(c, _), _| *c != channel);
        self.current_epoch.remove(&channel);
        self.staged_envelopes.retain(|(c, _), _| *c != channel);
        self.member_cards.remove(&channel);
        self.owners.remove(&channel);
        self.registered_at.remove(&channel);
        self.copublishers.remove(&channel);
        self.listings.remove(&channel);
        self.private.remove(&channel);
        self.subscriber_rows.remove(&channel);
        self.subscriber_meta.retain(|(c, _), _| *c != channel);
        self.public_seeds.remove(&channel);
        self.invite_keys.remove(&channel);
        self.removed.retain(|(c, _), _| *c != channel);
        self.feedback.retain_posts(|c, _| c != channel);
        existed
    }

    fn has_post(&self, channel: ChannelId, post_cursor: u64) -> bool {
        self.posts
            .get(&channel)
            .is_some_and(|posts| posts.iter().any(|p| p.cursor == post_cursor))
    }
}

impl Inner {
    /// Drop every trace of `(channel, subscriber)`: the row, its envelopes,
    /// member card, registration time, row number and staged envelopes.
    /// Returns whether a row was removed.
    fn drop_subscriber(&mut self, channel: ChannelId, subscriber: SubscriberId) -> bool {
        let removed = self
            .subscribers
            .get_mut(&channel)
            .is_some_and(|set| set.remove(&subscriber).is_some());
        self.envelopes.remove(&(channel, subscriber));
        if let Some(cards) = self.member_cards.get_mut(&channel) {
            cards.remove(&subscriber);
        }
        // The SQL store loses `registered_at` with the subscriber row; keep
        // the directory growth signal the same in both stores (ADR-0059).
        if let Some(map) = self.registered_at.get_mut(&channel) {
            map.remove(&subscriber);
        }
        if let Some((seq, _)) = self.subscriber_meta.remove(&(channel, subscriber)) {
            if let Some(rows) = self.subscriber_rows.get_mut(&channel) {
                rows.remove(&seq);
            }
        }
        for ((staged_channel, _), staged) in &mut self.staged_envelopes {
            if *staged_channel == channel {
                staged.remove(&subscriber);
            }
        }
        removed
    }

    fn entry_for(&self, channel: ChannelId, now_ms: u64) -> Option<DirectoryEntry> {
        let listing = self.listings.get(&channel)?;
        let since = now_ms.saturating_sub(GROWTH_WINDOW_MS);
        let registered = self.registered_at.get(&channel);
        let subscribers = self.subscribers.get(&channel).map_or(0, HashMap::len) as u64;
        let recent_registrations =
            registered.map_or(0, |m| m.values().filter(|&&t| t >= since).count()) as u64;
        Some(DirectoryEntry {
            channel,
            listing: listing.bytes.clone(),
            subscribers,
            recent_registrations,
            first_listed_at_ms: self
                .first_listed_at
                .get(&channel)
                .copied()
                .unwrap_or(now_ms),
        })
    }
}

impl InMemoryChannelStore {
    /// Construct an empty store with the default envelope
    /// retention ([`DEFAULT_ENVELOPE_RETENTION_EPOCHS`], 4).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the per-subscriber envelope retention window
    /// (ADR-0031). `n` is clamped to `>= 1` — a retention of 0
    /// would prune every envelope on the next write.
    #[must_use]
    pub fn with_envelope_retention(mut self, n: usize) -> Self {
        self.envelope_retention_epochs = n.max(1);
        self
    }
}

#[async_trait]
impl ChannelStore for InMemoryChannelStore {
    async fn create_with_access(
        &self,
        channel: ChannelId,
        owner_pubkey: [u8; OWNER_PUBKEY_LEN],
        private: bool,
    ) -> Result<(), StorageError> {
        let mut guard = self.inner.write().await;
        if guard.deleted.contains_key(&channel) {
            return Err(StorageError::ChannelDeleted);
        }
        match guard.owners.get(&channel) {
            Some(existing) if *existing != owner_pubkey => return Err(StorageError::OwnerMismatch),
            Some(_) => {},
            None => {
                guard.owners.insert(channel, owner_pubkey);
                guard.private.insert(channel, private);
            },
        }
        guard.posts.entry(channel).or_default();
        guard.next_cursor.entry(channel).or_insert(1);
        // Genesis epoch is 0 — the first rotation bumps it
        // to 1, matching `lakko_channels::EpochId::GENESIS`.
        guard.current_epoch.entry(channel).or_insert(0);
        Ok(())
    }

    async fn exists(&self, channel: ChannelId) -> bool {
        self.inner.read().await.posts.contains_key(&channel)
    }

    /// Append a post. Assigns the cursor + timestamp and
    /// returns the freshly-stored record.
    ///
    /// # Errors
    ///
    /// - [`StorageError::UnknownChannel`] — the caller must
    ///   register the channel via [`Self::create`] first.
    /// - [`StorageError::EmptyPost`] — zero-byte payload.
    /// - [`StorageError::PostTooLarge`] — payload exceeds
    ///   `max_bytes`.
    async fn put(
        &self,
        channel: ChannelId,
        payload: Vec<u8>,
        max_bytes: usize,
        server_ts_ms: u64,
    ) -> Result<StoredPost, StorageError> {
        if payload.is_empty() {
            return Err(StorageError::EmptyPost);
        }
        if payload.len() > max_bytes {
            return Err(StorageError::PostTooLarge {
                max: max_bytes,
                got: payload.len(),
            });
        }
        let mut guard = self.inner.write().await;
        if !guard.posts.contains_key(&channel) {
            return Err(StorageError::UnknownChannel);
        }
        let cursor = *guard.next_cursor.get(&channel).unwrap_or(&1);
        guard.next_cursor.insert(channel, cursor.saturating_add(1));
        let stored = StoredPost {
            cursor,
            server_ts_ms,
            payload,
        };
        guard
            .posts
            .get_mut(&channel)
            .expect("channel exists per check above")
            .push(stored.clone());
        Ok(stored)
    }

    /// Fetch posts strictly after `after` cursor, up to
    /// `limit` entries. Returns empty when the channel has
    /// nothing newer or is unknown.
    async fn fetch(
        &self,
        channel: ChannelId,
        after: u64,
        limit: usize,
    ) -> Result<Vec<StoredPost>, StorageError> {
        let guard = self.inner.read().await;
        let Some(list) = guard.posts.get(&channel) else {
            return Err(StorageError::UnknownChannel);
        };
        Ok(list
            .iter()
            .filter(|p| p.cursor > after)
            .take(limit)
            .cloned()
            .collect())
    }

    /// Total number of posts stored across every channel
    /// — surfaced for the `/metrics` endpoint future-self.
    async fn total_posts(&self) -> usize {
        self.inner.read().await.posts.values().map(Vec::len).sum()
    }

    async fn prune_posts_older_than(&self, cutoff_ms: u64) -> Result<u64, StorageError> {
        let mut guard = self.inner.write().await;
        let mut removed: u64 = 0;
        for posts in guard.posts.values_mut() {
            let before = posts.len();
            posts.retain(|p| p.server_ts_ms >= cutoff_ms);
            removed = removed.saturating_add((before - posts.len()) as u64);
        }
        // A pruned post takes its comments and reactions with it, as the SQL
        // foreign keys cascade.
        let live: std::collections::HashSet<(ChannelId, u64)> = guard
            .posts
            .iter()
            .flat_map(|(c, posts)| posts.iter().map(move |p| (*c, p.cursor)))
            .collect();
        guard.feedback.retain_posts(|c, p| live.contains(&(c, p)));
        Ok(removed)
    }

    /// Register a subscriber's X25519 public key for the
    /// channel. Idempotent — repeated calls with the same
    /// pubkey just update the registration timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::UnknownChannel`] when the
    /// channel was never registered via [`Self::create`].
    async fn subscribe_with_ticket(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
        server_ts_ms: u64,
        sealed_ticket: Option<SealedTicket>,
    ) -> Result<bool, StorageError> {
        let mut guard = self.inner.write().await;
        if !guard.posts.contains_key(&channel) {
            return Err(StorageError::UnknownChannel);
        }
        let is_new = guard
            .subscribers
            .entry(channel)
            .or_default()
            .insert(subscriber, server_ts_ms)
            .is_none();
        guard
            .registered_at
            .entry(channel)
            .or_default()
            .entry(subscriber)
            .or_insert(server_ts_ms);
        guard.removed.remove(&(channel, subscriber));
        if is_new {
            guard.last_seq = guard.last_seq.saturating_add(1);
            let seq = guard.last_seq;
            guard
                .subscriber_rows
                .entry(channel)
                .or_default()
                .insert(seq, subscriber);
            guard
                .subscriber_meta
                .insert((channel, subscriber), (seq, sealed_ticket));
        } else if let Some(ticket) = sealed_ticket {
            if let Some(meta) = guard.subscriber_meta.get_mut(&(channel, subscriber)) {
                meta.1 = Some(ticket);
            }
        }
        Ok(is_new)
    }

    /// `true` if the subscriber is registered against the
    /// channel.
    async fn is_subscribed(&self, channel: ChannelId, subscriber: SubscriberId) -> bool {
        self.inner
            .read()
            .await
            .subscribers
            .get(&channel)
            .is_some_and(|set| set.contains_key(&subscriber))
    }

    async fn touch_subscriber_activity(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
        now_ms: u64,
    ) -> Result<(), StorageError> {
        let mut guard = self.inner.write().await;
        if let Some(set) = guard.subscribers.get_mut(&channel) {
            if let Some(ts) = set.get_mut(&subscriber) {
                *ts = now_ms;
            }
        }
        // Non-registered subscriber: silent no-op. Touch
        // is a hint, not a contract — the API layer
        // already gates registration through `subscribe`.
        Ok(())
    }

    async fn prune_subscribers_older_than(&self, cutoff_ms: u64) -> Result<u64, StorageError> {
        let mut guard = self.inner.write().await;
        let mut removed: u64 = 0;
        // First pass: collect the (channel, subscriber)
        // pairs that fail the cutoff. We cannot mutate
        // `subscribers` and read it for envelope cleanup
        // in the same loop body.
        let mut to_drop: Vec<(ChannelId, SubscriberId)> = Vec::new();
        for (channel, set) in &guard.subscribers {
            for (sub, last_active_ms) in set {
                if *last_active_ms < cutoff_ms {
                    to_drop.push((*channel, *sub));
                }
            }
        }
        // Second pass: drop the subscriber + the
        // matching envelope. The SQL impl gets this for
        // free via FK ON DELETE CASCADE.
        for (channel, sub) in to_drop {
            if guard.drop_subscriber(channel, sub) {
                removed = removed.saturating_add(1);
            }
        }
        guard.removed.retain(|_, at| *at >= cutoff_ms);
        Ok(removed)
    }

    /// List every subscriber currently registered against
    /// the channel. Used by the publisher to fan out
    /// per-subscriber HPKE seed envelopes.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::UnknownChannel`] when the
    /// channel is unknown.
    async fn list_subscribers(
        &self,
        channel: ChannelId,
    ) -> Result<Vec<SubscriberId>, StorageError> {
        let guard = self.inner.read().await;
        if !guard.posts.contains_key(&channel) {
            return Err(StorageError::UnknownChannel);
        }
        Ok(guard
            .subscribers
            .get(&channel)
            .map(|set| set.keys().copied().collect())
            .unwrap_or_default())
    }

    /// Store the publisher-supplied HPKE seed envelope for
    /// a specific (channel, subscriber) at a concrete epoch,
    /// then prune that subscriber to the highest-N retained
    /// epochs (ADR-0031). The server never inspects
    /// `envelope` — it stores opaque bytes and hands them back
    /// unchanged on the matching GET.
    ///
    /// `epoch` defaults to the channel's `current_epoch`; an
    /// epoch greater than `current_epoch` is refused (C-42).
    ///
    /// # Errors
    ///
    /// - [`StorageError::UnknownChannel`] — channel was
    ///   never registered.
    /// - [`StorageError::UnknownSubscriber`] — subscriber
    ///   has not registered against this channel.
    /// - [`StorageError::BadEnvelopeLen`] — the byte
    ///   length does not match `expected`. Callers pass
    ///   `lakko_channels::subscribe::SEED_ENVELOPE_LEN`
    ///   here.
    /// - [`StorageError::EnvelopeEpochInFuture`] — the
    ///   supplied epoch exceeds the channel's current epoch.
    async fn deliver_envelope(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
        epoch: Option<u32>,
        envelope: Vec<u8>,
        expected_len: usize,
    ) -> Result<(), StorageError> {
        if envelope.len() != expected_len {
            return Err(StorageError::BadEnvelopeLen {
                expected: expected_len,
                got: envelope.len(),
            });
        }
        let mut guard = self.inner.write().await;
        if !guard.posts.contains_key(&channel) {
            return Err(StorageError::UnknownChannel);
        }
        let registered = guard
            .subscribers
            .get(&channel)
            .is_some_and(|set| set.contains_key(&subscriber));
        if !registered {
            return Err(StorageError::UnknownSubscriber);
        }
        let current = guard.current_epoch.get(&channel).copied().unwrap_or(0);
        let ep = epoch.unwrap_or(current);
        if ep > current {
            return Err(StorageError::EnvelopeEpochInFuture { current, got: ep });
        }
        let retention = self.envelope_retention_epochs;
        let map = guard.envelopes.entry((channel, subscriber)).or_default();
        map.insert(ep, envelope);
        prune_envelope_map_to_n(map, retention);
        Ok(())
    }

    /// Fetch the HPKE seed envelope at the highest retained
    /// epoch for the (channel, subscriber) pair. Returns
    /// `Ok(None)` if the subscriber is registered but no
    /// envelope has been delivered yet (e.g. the subscriber
    /// registered faster than the publisher's fan-out
    /// completed).
    ///
    /// # Errors
    ///
    /// - [`StorageError::UnknownChannel`] — channel is
    ///   unknown.
    /// - [`StorageError::UnknownSubscriber`] — subscriber
    ///   has not registered against this channel.
    async fn fetch_envelope(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
    ) -> Result<Option<Vec<u8>>, StorageError> {
        let guard = self.inner.read().await;
        if !guard.posts.contains_key(&channel) {
            return Err(StorageError::UnknownChannel);
        }
        let registered = guard
            .subscribers
            .get(&channel)
            .is_some_and(|set| set.contains_key(&subscriber));
        if !registered {
            return Err(StorageError::UnknownSubscriber);
        }
        Ok(guard
            .envelopes
            .get(&(channel, subscriber))
            .and_then(|map| map.last_key_value().map(|(_, env)| env.clone())))
    }

    /// Fetch the whole retained `[(epoch, envelope)]` set for
    /// the (channel, subscriber) pair, ascending by epoch
    /// (ADR-0031). Empty when the subscriber is registered but
    /// no envelope has been delivered yet.
    ///
    /// # Errors
    ///
    /// - [`StorageError::UnknownChannel`] — channel is
    ///   unknown.
    /// - [`StorageError::UnknownSubscriber`] — subscriber
    ///   has not registered against this channel.
    async fn fetch_envelopes(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
    ) -> Result<Vec<(u32, Vec<u8>)>, StorageError> {
        let guard = self.inner.read().await;
        if !guard.posts.contains_key(&channel) {
            return Err(StorageError::UnknownChannel);
        }
        let registered = guard
            .subscribers
            .get(&channel)
            .is_some_and(|set| set.contains_key(&subscriber));
        if !registered {
            return Err(StorageError::UnknownSubscriber);
        }
        Ok(guard
            .envelopes
            .get(&(channel, subscriber))
            .map(|map| {
                map.iter()
                    .map(|(epoch, env)| (*epoch, env.clone()))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Total number of subscribers across every channel
    /// — surfaced for the `/metrics` endpoint future-self.
    async fn total_subscribers(&self) -> usize {
        self.inner
            .read()
            .await
            .subscribers
            .values()
            .map(HashMap::len)
            .sum()
    }

    /// Read the channel's current epoch id. Returns
    /// `Ok(0)` for a freshly-created channel (genesis).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::UnknownChannel`] when the
    /// channel was never registered.
    async fn current_epoch(&self, channel: ChannelId) -> Result<u32, StorageError> {
        let guard = self.inner.read().await;
        if !guard.posts.contains_key(&channel) {
            return Err(StorageError::UnknownChannel);
        }
        Ok(*guard.current_epoch.get(&channel).unwrap_or(&0))
    }

    /// Atomic rotation: bump the current epoch by one **and**
    /// replace the per-subscriber envelopes for every entry
    /// in `batch`. Either every step lands or none — the
    /// entire operation runs under a single write lock so a
    /// concurrent reader cannot observe a torn rotation.
    ///
    /// `expected_envelope_len` is checked per entry; one
    /// wrong-sized envelope aborts the whole batch (epoch is
    /// NOT bumped). This matches the "rotation is atomic"
    /// property a subscriber-side catch-up flow assumes —
    /// if `current_epoch` advanced, every registered
    /// subscriber's envelope is the freshly-rotated one.
    ///
    /// Returns the new epoch id on success.
    ///
    /// # Errors
    ///
    /// - [`StorageError::UnknownChannel`] — channel unknown.
    /// - [`StorageError::UnknownSubscriber`] — a subscriber
    ///   in the batch is not registered against this
    ///   channel.
    /// - [`StorageError::BadEnvelopeLen`] — any entry's
    ///   envelope does not match `expected_envelope_len`.
    async fn rotate(
        &self,
        channel: ChannelId,
        batch: Vec<(SubscriberId, Vec<u8>)>,
        expected_envelope_len: usize,
    ) -> Result<u32, StorageError> {
        // First pass: validate without mutating. We want
        // either-all-or-none semantics.
        for (_sub, env) in &batch {
            if env.len() != expected_envelope_len {
                return Err(StorageError::BadEnvelopeLen {
                    expected: expected_envelope_len,
                    got: env.len(),
                });
            }
        }
        let mut guard = self.inner.write().await;
        if !guard.posts.contains_key(&channel) {
            return Err(StorageError::UnknownChannel);
        }
        // Cross-check every subscriber id against the
        // registry before any mutation lands.
        let subs = guard.subscribers.get(&channel).cloned().unwrap_or_default();
        for (sub, _env) in &batch {
            if !subs.contains_key(sub) {
                return Err(StorageError::UnknownSubscriber);
            }
        }
        // Validations passed — file each envelope at the new
        // epoch and prune the subscriber to the retained N
        // (ADR-0031). Same write lock, so a reader never
        // observes a torn rotation.
        let next = guard
            .current_epoch
            .get(&channel)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        let retention = self.envelope_retention_epochs;
        for (sub, env) in batch {
            let map = guard.envelopes.entry((channel, sub)).or_default();
            map.insert(next, env);
            prune_envelope_map_to_n(map, retention);
        }
        guard.current_epoch.insert(channel, next);
        Ok(next)
    }

    /// Stage a chunk of the next epoch's envelopes (ADR-0030
    /// prepare phase). Validates like [`Self::rotate`] then
    /// upserts each entry into the staging area under a
    /// single write lock. The epoch is not bumped and the
    /// staged rows are invisible to [`Self::fetch_envelope`].
    ///
    /// # Errors
    ///
    /// - [`StorageError::BadEnvelopeLen`] — any entry's
    ///   envelope is not `expected_envelope_len` bytes.
    /// - [`StorageError::UnknownChannel`] — channel unknown.
    /// - [`StorageError::StaleTargetEpoch`] — `target_epoch`
    ///   is not `current_epoch + 1`.
    /// - [`StorageError::UnknownSubscriber`] — a subscriber
    ///   in the chunk is not registered against this channel.
    async fn stage_envelopes(
        &self,
        channel: ChannelId,
        target_epoch: u32,
        batch: Vec<(SubscriberId, Vec<u8>)>,
        expected_envelope_len: usize,
    ) -> Result<(), StorageError> {
        // First pass: validate lengths without mutating —
        // all-or-none, matching the single-shot rotate.
        for (_sub, env) in &batch {
            if env.len() != expected_envelope_len {
                return Err(StorageError::BadEnvelopeLen {
                    expected: expected_envelope_len,
                    got: env.len(),
                });
            }
        }
        let mut guard = self.inner.write().await;
        if !guard.posts.contains_key(&channel) {
            return Err(StorageError::UnknownChannel);
        }
        // Monotone target: refuse anything but current + 1.
        let expected = guard
            .current_epoch
            .get(&channel)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        if target_epoch != expected {
            return Err(StorageError::StaleTargetEpoch {
                expected,
                got: target_epoch,
            });
        }
        // Cross-check every subscriber id against the
        // registry before any staged row lands.
        let subs = guard.subscribers.get(&channel).cloned().unwrap_or_default();
        for (sub, _env) in &batch {
            if !subs.contains_key(sub) {
                return Err(StorageError::UnknownSubscriber);
            }
        }
        // Validations passed — upsert into the staging area.
        let staged = guard
            .staged_envelopes
            .entry((channel, target_epoch))
            .or_default();
        for (sub, env) in batch {
            staged.insert(sub, env);
        }
        Ok(())
    }

    /// Commit a staged rotation (ADR-0030 commit phase) —
    /// the single atomic point. Runs entirely under the write
    /// lock so a concurrent reader cannot observe a torn
    /// rotation.
    ///
    /// Re-verifies `target_epoch == current_epoch + 1`,
    /// promotes every staged row for `target_epoch` whose
    /// subscriber is still registered into the live envelope
    /// set, bumps `current_epoch` to `target_epoch`, and
    /// clears every staged row for the channel. Returns the
    /// new epoch.
    ///
    /// # Errors
    ///
    /// - [`StorageError::UnknownChannel`] — channel unknown.
    /// - [`StorageError::StaleTargetEpoch`] — `target_epoch`
    ///   is not `current_epoch + 1`.
    async fn commit_rotation(
        &self,
        channel: ChannelId,
        target_epoch: u32,
    ) -> Result<u32, StorageError> {
        let mut guard = self.inner.write().await;
        if !guard.posts.contains_key(&channel) {
            return Err(StorageError::UnknownChannel);
        }
        let expected = guard
            .current_epoch
            .get(&channel)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        if target_epoch != expected {
            return Err(StorageError::StaleTargetEpoch {
                expected,
                got: target_epoch,
            });
        }
        // Snapshot the registered set + the staged rows for
        // this target so the promotion loop doesn't borrow
        // `guard` immutably and mutably at once.
        let registered = guard.subscribers.get(&channel).cloned().unwrap_or_default();
        let staged = guard
            .staged_envelopes
            .get(&(channel, target_epoch))
            .cloned()
            .unwrap_or_default();
        // Promote only staged rows whose subscriber is still
        // registered (partial-coverage policy — a subscriber
        // pruned between stage and commit is skipped, not an
        // abort). A registered subscriber with no staged row
        // simply keeps their prior-epoch envelope. Each
        // promoted envelope is filed at `target_epoch` and the
        // subscriber is pruned to the retained N (ADR-0031).
        let retention = self.envelope_retention_epochs;
        for (sub, env) in staged {
            if registered.contains_key(&sub) {
                let map = guard.envelopes.entry((channel, sub)).or_default();
                map.insert(target_epoch, env);
                prune_envelope_map_to_n(map, retention);
            }
        }
        // Single atomic transition: bump the epoch, then
        // clear every staged row for the channel (this
        // target and any abandoned older prepare).
        guard.current_epoch.insert(channel, target_epoch);
        guard.staged_envelopes.retain(|(c, _e), _| *c != channel);
        Ok(target_epoch)
    }

    async fn owner_pubkey(
        &self,
        channel: ChannelId,
    ) -> Result<Option<[u8; OWNER_PUBKEY_LEN]>, StorageError> {
        Ok(self.inner.read().await.owners.get(&channel).copied())
    }

    async fn replace_owner(
        &self,
        channel: ChannelId,
        current_owner: [u8; OWNER_PUBKEY_LEN],
        new_owner: [u8; OWNER_PUBKEY_LEN],
    ) -> Result<bool, StorageError> {
        let mut guard = self.inner.write().await;
        let stored = guard
            .owners
            .get_mut(&channel)
            .ok_or(StorageError::UnknownChannel)?;
        if *stored != current_owner {
            return Ok(false);
        }
        *stored = new_owner;
        Ok(true)
    }

    async fn consume_owner_nonce(
        &self,
        nonce: [u8; OWNER_NONCE_LEN],
        now_ms: u64,
    ) -> Result<bool, StorageError> {
        let mut guard = self.inner.write().await;
        if guard.owner_nonces.contains_key(&nonce) {
            return Ok(false);
        }
        guard.owner_nonces.insert(nonce, now_ms);
        Ok(true)
    }

    async fn prune_owner_nonces_older_than(&self, cutoff_ms: u64) -> Result<u64, StorageError> {
        let mut guard = self.inner.write().await;
        let before = guard.owner_nonces.len();
        guard.owner_nonces.retain(|_, seen| *seen >= cutoff_ms);
        Ok((before - guard.owner_nonces.len()) as u64)
    }

    async fn appoint_copublisher(
        &self,
        channel: ChannelId,
        key: [u8; OWNER_PUBKEY_LEN],
        max: usize,
        now_ms: u64,
    ) -> Result<(), StorageError> {
        let mut guard = self.inner.write().await;
        if !guard.owners.contains_key(&channel) {
            return Err(StorageError::UnknownChannel);
        }
        let keys = guard.copublishers.entry(channel).or_default();
        if keys.contains_key(&key) {
            return Ok(());
        }
        if keys.len() >= max {
            return Err(StorageError::TooManyCoPublishers);
        }
        keys.insert(key, now_ms);
        Ok(())
    }

    async fn revoke_copublisher(
        &self,
        channel: ChannelId,
        key: [u8; OWNER_PUBKEY_LEN],
    ) -> Result<bool, StorageError> {
        let mut guard = self.inner.write().await;
        Ok(guard
            .copublishers
            .get_mut(&channel)
            .is_some_and(|keys| keys.remove(&key).is_some()))
    }

    async fn is_copublisher(
        &self,
        channel: ChannelId,
        key: [u8; OWNER_PUBKEY_LEN],
    ) -> Result<bool, StorageError> {
        Ok(self
            .inner
            .read()
            .await
            .copublishers
            .get(&channel)
            .is_some_and(|keys| keys.contains_key(&key)))
    }

    async fn put_listing(
        &self,
        channel: ChannelId,
        listing: StoredListing,
        now_ms: u64,
    ) -> Result<(), StorageError> {
        let mut guard = self.inner.write().await;
        if !guard.posts.contains_key(&channel) {
            return Err(StorageError::UnknownChannel);
        }
        if guard.private.get(&channel).copied().unwrap_or(false) {
            return Err(StorageError::ChannelPrivate);
        }
        if guard.moderation.get(&channel).is_some_and(|(b, _, _)| *b) {
            return Err(StorageError::ListingBlocked);
        }
        if let Some(stored) = guard.listings.get(&channel) {
            if !lakko_channels::listing::listing_is_newer(
                listing.updated_at_ms,
                stored.updated_at_ms,
            ) {
                return Err(StorageError::StaleListing);
            }
        }
        guard.listings.insert(channel, listing);
        guard.first_listed_at.entry(channel).or_insert(now_ms);
        Ok(())
    }

    async fn withdraw_listing(&self, channel: ChannelId) -> Result<bool, StorageError> {
        Ok(self.inner.write().await.listings.remove(&channel).is_some())
    }

    async fn listing(
        &self,
        channel: ChannelId,
        now_ms: u64,
    ) -> Result<Option<DirectoryEntry>, StorageError> {
        Ok(self.inner.read().await.entry_for(channel, now_ms))
    }

    async fn directory_candidates(
        &self,
        query: &DirectoryQuery,
        now_ms: u64,
    ) -> Result<Vec<DirectoryEntry>, StorageError> {
        let guard = self.inner.read().await;
        let hits = guard.listings.iter().filter(|(_, l)| match query {
            DirectoryQuery::Bucket { topic, language } => {
                l.topic == *topic && l.language == *language
            },
            DirectoryQuery::Search { needle } => l.matches(needle),
        });
        Ok(hits
            .filter_map(|(c, _)| guard.entry_for(*c, now_ms))
            .collect())
    }

    async fn moderate(
        &self,
        channel: ChannelId,
        action: ModerationAction,
        reason: &str,
        now_ms: u64,
    ) -> Result<(), StorageError> {
        let mut guard = self.inner.write().await;
        let blocked = match action {
            ModerationAction::Remove => guard.moderation.get(&channel).is_some_and(|(b, _, _)| *b),
            ModerationAction::Block => true,
            ModerationAction::Unblock => false,
        };
        if action != ModerationAction::Unblock {
            guard.listings.remove(&channel);
        }
        guard
            .moderation
            .insert(channel, (blocked, reason.to_string(), now_ms));
        Ok(())
    }

    async fn put_member_card(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
        epoch: u32,
        card: Vec<u8>,
    ) -> Result<(), StorageError> {
        let mut guard = self.inner.write().await;
        if !guard.posts.contains_key(&channel) {
            return Err(StorageError::UnknownChannel);
        }
        if !guard
            .subscribers
            .get(&channel)
            .is_some_and(|set| set.contains_key(&subscriber))
        {
            return Err(StorageError::UnknownSubscriber);
        }
        let current = guard.current_epoch.get(&channel).copied().unwrap_or(0);
        if epoch > current {
            return Err(StorageError::MemberCardEpochInFuture {
                current,
                got: epoch,
            });
        }
        let cards = guard.member_cards.entry(channel).or_default();
        if let Some((stored, _)) = cards.get(&subscriber) {
            if epoch < *stored {
                return Err(StorageError::MemberCardStale {
                    stored: *stored,
                    got: epoch,
                });
            }
        }
        cards.insert(subscriber, (epoch, card));
        Ok(())
    }

    async fn delete_member_card(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
    ) -> Result<(), StorageError> {
        let mut guard = self.inner.write().await;
        if !guard.posts.contains_key(&channel) {
            return Err(StorageError::UnknownChannel);
        }
        if let Some(cards) = guard.member_cards.get_mut(&channel) {
            cards.remove(&subscriber);
        }
        Ok(())
    }

    async fn unsubscribe(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
    ) -> Result<(), StorageError> {
        let mut guard = self.inner.write().await;
        if !guard.posts.contains_key(&channel) {
            return Err(StorageError::UnknownChannel);
        }
        guard.drop_subscriber(channel, subscriber);
        Ok(())
    }

    async fn post_exists(
        &self,
        channel: ChannelId,
        post_cursor: u64,
    ) -> Result<bool, StorageError> {
        Ok(self.inner.read().await.has_post(channel, post_cursor))
    }

    async fn add_reaction(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        writer_key: WriterKey,
        label: Label,
        _now_ms: u64,
    ) -> Result<bool, StorageError> {
        let mut guard = self.inner.write().await;
        if !guard.has_post(channel, post_cursor) {
            return Err(StorageError::UnknownPost);
        }
        Ok(guard
            .feedback
            .add_reaction(channel, post_cursor, writer_key, label))
    }

    async fn delete_reaction(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        writer_key: WriterKey,
        label: Label,
    ) -> Result<bool, StorageError> {
        Ok(self.inner.write().await.feedback.delete_reaction(
            channel,
            post_cursor,
            writer_key,
            label,
        ))
    }

    async fn reaction_counts(
        &self,
        channel: ChannelId,
        post_cursors: &[u64],
    ) -> Result<Vec<PostReactionCounts>, StorageError> {
        Ok(self
            .inner
            .read()
            .await
            .feedback
            .reaction_counts(channel, post_cursors))
    }

    async fn add_comment(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        writer_key: WriterKey,
        sealed: Vec<u8>,
        now_ms: u64,
    ) -> Result<u64, StorageError> {
        let mut guard = self.inner.write().await;
        if !guard.has_post(channel, post_cursor) {
            return Err(StorageError::UnknownPost);
        }
        Ok(guard
            .feedback
            .add_comment(channel, post_cursor, writer_key, sealed, now_ms))
    }

    async fn comment_writer(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        comment_cursor: u64,
    ) -> Result<Option<WriterKey>, StorageError> {
        Ok(self
            .inner
            .read()
            .await
            .feedback
            .comment_writer(channel, post_cursor, comment_cursor))
    }

    async fn delete_comment(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        comment_cursor: u64,
    ) -> Result<bool, StorageError> {
        Ok(self
            .inner
            .write()
            .await
            .feedback
            .delete_comment(channel, post_cursor, comment_cursor))
    }

    async fn list_comments(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        after: u64,
        limit: usize,
    ) -> Result<Vec<StoredComment>, StorageError> {
        Ok(self
            .inner
            .read()
            .await
            .feedback
            .list_comments(channel, post_cursor, after, limit))
    }

    async fn consume_write_nonce(
        &self,
        nonce: [u8; WRITE_NONCE_LEN],
        now_ms: u64,
    ) -> Result<bool, StorageError> {
        Ok(self
            .inner
            .write()
            .await
            .feedback
            .consume_nonce(nonce, now_ms))
    }

    async fn prune_write_nonces_older_than(&self, cutoff_ms: u64) -> Result<u64, StorageError> {
        Ok(self
            .inner
            .write()
            .await
            .feedback
            .prune_nonces_older_than(cutoff_ms))
    }

    async fn comment_counts(
        &self,
        channel: ChannelId,
        post_cursors: &[u64],
    ) -> Result<Vec<(u64, u64)>, StorageError> {
        Ok(self
            .inner
            .read()
            .await
            .feedback
            .comment_counts(channel, post_cursors))
    }

    async fn add_comment_reaction(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        comment_cursor: u64,
        writer_key: WriterKey,
        label: Label,
        _now_ms: u64,
    ) -> Result<bool, StorageError> {
        let mut guard = self.inner.write().await;
        if !guard
            .feedback
            .has_comment(channel, post_cursor, comment_cursor)
        {
            return Err(StorageError::UnknownComment);
        }
        Ok(guard.feedback.add_comment_reaction(
            channel,
            post_cursor,
            comment_cursor,
            writer_key,
            label,
        ))
    }

    async fn delete_comment_reaction(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        comment_cursor: u64,
        writer_key: WriterKey,
        label: Label,
    ) -> Result<bool, StorageError> {
        Ok(self.inner.write().await.feedback.delete_comment_reaction(
            channel,
            post_cursor,
            comment_cursor,
            writer_key,
            label,
        ))
    }

    async fn comment_reaction_counts(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        comment_cursors: &[u64],
    ) -> Result<Vec<CommentReactionCounts>, StorageError> {
        Ok(self.inner.read().await.feedback.comment_reaction_counts(
            channel,
            post_cursor,
            comment_cursors,
        ))
    }

    async fn apply_vote(
        &self,
        channel: ChannelId,
        post_cursor: u64,
        tag: Tag,
        labels: Vec<Label>,
        at_ms: u64,
    ) -> Result<VoteOutcome, StorageError> {
        let mut guard = self.inner.write().await;
        if !guard.has_post(channel, post_cursor) {
            return Err(StorageError::UnknownPost);
        }
        Ok(guard
            .feedback
            .votes
            .apply(channel, post_cursor, tag, labels, at_ms))
    }

    async fn vote_counts(
        &self,
        channel: ChannelId,
        post_cursors: &[u64],
    ) -> Result<Vec<PostVoteCounts>, StorageError> {
        Ok(self
            .inner
            .read()
            .await
            .feedback
            .votes
            .counts(channel, post_cursors))
    }

    async fn delete_channel(&self, channel: ChannelId, now_ms: u64) -> Result<bool, StorageError> {
        let mut guard = self.inner.write().await;
        let existed = guard.drop_channel(channel);
        guard.deleted.entry(channel).or_insert(now_ms);
        Ok(existed)
    }

    async fn is_deleted(&self, channel: ChannelId) -> Result<bool, StorageError> {
        Ok(self.inner.read().await.deleted.contains_key(&channel))
    }

    async fn list_member_cards(
        &self,
        channel: ChannelId,
        after: Option<SubscriberId>,
        limit: usize,
    ) -> Result<Vec<(SubscriberId, Vec<u8>)>, StorageError> {
        use std::ops::Bound;
        let guard = self.inner.read().await;
        if !guard.posts.contains_key(&channel) {
            return Err(StorageError::UnknownChannel);
        }
        let Some(cards) = guard.member_cards.get(&channel) else {
            return Ok(Vec::new());
        };
        let lower = after.map_or(Bound::Unbounded, Bound::Excluded);
        Ok(cards
            .range((lower, Bound::Unbounded))
            .take(limit)
            .map(|(sub, (_, card))| (*sub, card.clone()))
            .collect())
    }

    async fn is_private(&self, channel: ChannelId) -> Result<Option<bool>, StorageError> {
        let guard = self.inner.read().await;
        if !guard.posts.contains_key(&channel) {
            return Ok(None);
        }
        Ok(Some(guard.private.get(&channel).copied().unwrap_or(false)))
    }

    async fn set_private(&self, channel: ChannelId, private: bool) -> Result<bool, StorageError> {
        let mut guard = self.inner.write().await;
        if !guard.posts.contains_key(&channel) {
            return Err(StorageError::UnknownChannel);
        }
        let was = guard.private.insert(channel, private).unwrap_or(false);
        if private {
            guard.listings.remove(&channel);
            guard.public_seeds.remove(&channel);
        }
        Ok(was != private)
    }

    async fn put_public_seed(
        &self,
        channel: ChannelId,
        epoch: u32,
        envelope: [u8; PUBLIC_SEED_LEN],
        now_ms: u64,
    ) -> Result<PublicSeedOutcome, StorageError> {
        let mut guard = self.inner.write().await;
        if !guard.posts.contains_key(&channel) {
            return Err(StorageError::UnknownChannel);
        }
        if guard.private.get(&channel).copied().unwrap_or(false) {
            return Err(StorageError::ChannelPrivate);
        }
        let current = guard.current_epoch.get(&channel).copied().unwrap_or(0);
        if epoch > current.saturating_add(1) {
            return Err(StorageError::PublicSeedEpochInFuture {
                current,
                got: epoch,
            });
        }
        let seeds = guard.public_seeds.entry(channel).or_default();
        if let Some((stored, _)) = seeds.get(&epoch) {
            return if *stored == envelope {
                Ok(PublicSeedOutcome::AlreadyStored)
            } else {
                Err(StorageError::PublicSeedConflict)
            };
        }
        for (_, (_, superseded)) in seeds.range_mut(..epoch) {
            superseded.get_or_insert(now_ms);
        }
        // A seed stored after a higher epoch's is superseded at once.
        let superseded = seeds.range(epoch..).next().map(|_| now_ms);
        seeds.insert(epoch, (envelope, superseded));
        Ok(PublicSeedOutcome::Stored)
    }

    async fn public_seed(
        &self,
        channel: ChannelId,
        epoch: u32,
    ) -> Result<Option<[u8; PUBLIC_SEED_LEN]>, StorageError> {
        Ok(self
            .inner
            .read()
            .await
            .public_seeds
            .get(&channel)
            .and_then(|seeds| seeds.get(&epoch))
            .map(|(envelope, _)| *envelope))
    }

    async fn prune_public_seeds_superseded_before(
        &self,
        cutoff_ms: u64,
    ) -> Result<u64, StorageError> {
        let mut guard = self.inner.write().await;
        let mut removed: u64 = 0;
        for seeds in guard.public_seeds.values_mut() {
            let before = seeds.len();
            seeds.retain(|_, (_, superseded)| !matches!(superseded, Some(at) if *at < cutoff_ms));
            removed = removed.saturating_add((before - seeds.len()) as u64);
        }
        Ok(removed)
    }

    async fn put_invite_key(
        &self,
        channel: ChannelId,
        invite_pk: [u8; INVITE_KEY_LEN],
        expiry_ms: u64,
        max_uses: u32,
        max_keys: usize,
        _now_ms: u64,
    ) -> Result<(), StorageError> {
        let mut guard = self.inner.write().await;
        if !guard.posts.contains_key(&channel) {
            return Err(StorageError::UnknownChannel);
        }
        let keys = guard.invite_keys.entry(channel).or_default();
        if let Some(row) = keys.get_mut(&invite_pk) {
            row.expiry_ms = expiry_ms;
            row.max_uses = max_uses;
            return Ok(());
        }
        if keys.len() >= max_keys {
            return Err(StorageError::TooManyInviteKeys);
        }
        keys.insert(
            invite_pk,
            InviteKeyRow {
                invite_pk,
                expiry_ms,
                max_uses,
                used_count: 0,
            },
        );
        Ok(())
    }

    async fn delete_invite_key(
        &self,
        channel: ChannelId,
        invite_pk: [u8; INVITE_KEY_LEN],
    ) -> Result<bool, StorageError> {
        Ok(self
            .inner
            .write()
            .await
            .invite_keys
            .get_mut(&channel)
            .is_some_and(|keys| keys.remove(&invite_pk).is_some()))
    }

    async fn invite_key(
        &self,
        channel: ChannelId,
        invite_pk: [u8; INVITE_KEY_LEN],
    ) -> Result<Option<InviteKeyRow>, StorageError> {
        Ok(self
            .inner
            .read()
            .await
            .invite_keys
            .get(&channel)
            .and_then(|keys| keys.get(&invite_pk))
            .copied())
    }

    async fn list_invite_keys(
        &self,
        channel: ChannelId,
    ) -> Result<Vec<InviteKeyRow>, StorageError> {
        let guard = self.inner.read().await;
        if !guard.posts.contains_key(&channel) {
            return Err(StorageError::UnknownChannel);
        }
        Ok(guard
            .invite_keys
            .get(&channel)
            .map(|keys| keys.values().copied().collect())
            .unwrap_or_default())
    }

    async fn claim_invite_use(
        &self,
        channel: ChannelId,
        invite_pk: [u8; INVITE_KEY_LEN],
        now_ms: u64,
    ) -> Result<bool, StorageError> {
        let mut guard = self.inner.write().await;
        let Some(row) = guard
            .invite_keys
            .get_mut(&channel)
            .and_then(|keys| keys.get_mut(&invite_pk))
        else {
            return Ok(false);
        };
        if !row.is_live(now_ms) {
            return Ok(false);
        }
        row.used_count = row.used_count.saturating_add(1);
        Ok(true)
    }

    async fn release_invite_use(
        &self,
        channel: ChannelId,
        invite_pk: [u8; INVITE_KEY_LEN],
    ) -> Result<(), StorageError> {
        let mut guard = self.inner.write().await;
        if let Some(row) = guard
            .invite_keys
            .get_mut(&channel)
            .and_then(|keys| keys.get_mut(&invite_pk))
        {
            row.used_count = row.used_count.saturating_sub(1);
        }
        Ok(())
    }

    async fn prune_invite_keys_expired_before(&self, cutoff_ms: u64) -> Result<u64, StorageError> {
        let mut guard = self.inner.write().await;
        let mut removed: u64 = 0;
        for keys in guard.invite_keys.values_mut() {
            let before = keys.len();
            keys.retain(|_, row| row.expiry_ms == 0 || row.expiry_ms >= cutoff_ms);
            removed = removed.saturating_add((before - keys.len()) as u64);
        }
        Ok(removed)
    }

    async fn list_subscriber_page(
        &self,
        channel: ChannelId,
        after_seq: u64,
        limit: usize,
    ) -> Result<Vec<SubscriberEntry>, StorageError> {
        use std::ops::Bound;
        let guard = self.inner.read().await;
        if !guard.posts.contains_key(&channel) {
            return Err(StorageError::UnknownChannel);
        }
        let Some(rows) = guard.subscriber_rows.get(&channel) else {
            return Ok(Vec::new());
        };
        Ok(rows
            .range((Bound::Excluded(after_seq), Bound::Unbounded))
            .take(limit)
            .map(|(seq, sub)| SubscriberEntry {
                seq: *seq,
                subscriber: *sub,
                sealed_ticket: guard
                    .subscriber_meta
                    .get(&(channel, *sub))
                    .and_then(|(_, ticket)| *ticket),
            })
            .collect())
    }

    async fn subscriber_count(&self, channel: ChannelId) -> Result<u64, StorageError> {
        let guard = self.inner.read().await;
        if !guard.posts.contains_key(&channel) {
            return Err(StorageError::UnknownChannel);
        }
        Ok(guard.subscribers.get(&channel).map_or(0, HashMap::len) as u64)
    }

    async fn remove_subscriber(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
        now_ms: u64,
    ) -> Result<bool, StorageError> {
        let mut guard = self.inner.write().await;
        if !guard.posts.contains_key(&channel) {
            return Err(StorageError::UnknownChannel);
        }
        if !guard.private.get(&channel).copied().unwrap_or(false) {
            return Err(StorageError::ChannelPublic);
        }
        let removed = guard.drop_subscriber(channel, subscriber);
        guard.removed.insert((channel, subscriber), now_ms);
        Ok(removed)
    }

    async fn is_removed(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
    ) -> Result<bool, StorageError> {
        Ok(self
            .inner
            .read()
            .await
            .removed
            .contains_key(&(channel, subscriber)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: [u8; OWNER_PUBKEY_LEN] = [7; OWNER_PUBKEY_LEN];

    fn cid(b: u8) -> ChannelId {
        ChannelId([b; 16])
    }

    #[tokio::test]
    async fn replace_owner_is_a_compare_and_swap() {
        let store = InMemoryChannelStore::new();
        let c = ChannelId::from_bytes(&[0x41; 16]).unwrap();
        assert!(matches!(
            store.replace_owner(c, [1; 32], [2; 32]).await,
            Err(StorageError::UnknownChannel)
        ));
        store.create(c, [1; 32]).await.unwrap();
        // Not the current key: nothing changes.
        assert!(!store.replace_owner(c, [9; 32], [2; 32]).await.unwrap());
        assert_eq!(store.owner_pubkey(c).await.unwrap(), Some([1; 32]));
        // The current key hands over, once.
        assert!(store.replace_owner(c, [1; 32], [2; 32]).await.unwrap());
        assert_eq!(store.owner_pubkey(c).await.unwrap(), Some([2; 32]));
        assert!(!store.replace_owner(c, [1; 32], [3; 32]).await.unwrap());
        assert_eq!(store.owner_pubkey(c).await.unwrap(), Some([2; 32]));
        // Re-creating under the old key is still refused.
        assert!(matches!(
            store.create(c, [1; 32]).await,
            Err(StorageError::OwnerMismatch)
        ));
    }

    #[tokio::test]
    async fn create_is_idempotent() {
        let s = InMemoryChannelStore::new();
        let c = cid(0xAA);
        s.create(c, OWNER).await.unwrap();
        s.create(c, OWNER).await.unwrap();
        assert!(s.exists(c).await);
    }

    #[tokio::test]
    async fn put_assigns_monotone_cursors() {
        let s = InMemoryChannelStore::new();
        let c = cid(0xBB);
        s.create(c, OWNER).await.unwrap();
        let a = s.put(c, vec![1, 2, 3], 1024, 1000).await.unwrap();
        let b = s.put(c, vec![4, 5], 1024, 2000).await.unwrap();
        assert_eq!(a.cursor, 1);
        assert_eq!(b.cursor, 2);
        assert!(b.server_ts_ms > a.server_ts_ms);
    }

    #[tokio::test]
    async fn put_rejects_unknown_channel() {
        let s = InMemoryChannelStore::new();
        let err = s.put(cid(0xCC), vec![1], 1024, 0).await.unwrap_err();
        assert!(matches!(err, StorageError::UnknownChannel));
    }

    #[tokio::test]
    async fn put_rejects_empty_payload() {
        let s = InMemoryChannelStore::new();
        let c = cid(0xDD);
        s.create(c, OWNER).await.unwrap();
        let err = s.put(c, vec![], 1024, 0).await.unwrap_err();
        assert!(matches!(err, StorageError::EmptyPost));
    }

    #[tokio::test]
    async fn put_rejects_oversize_payload() {
        let s = InMemoryChannelStore::new();
        let c = cid(0xEE);
        s.create(c, OWNER).await.unwrap();
        let err = s.put(c, vec![0u8; 100], 50, 0).await.unwrap_err();
        assert!(matches!(
            err,
            StorageError::PostTooLarge { max: 50, got: 100 }
        ));
    }

    #[tokio::test]
    async fn fetch_filters_by_cursor() {
        let s = InMemoryChannelStore::new();
        let c = cid(0xFF);
        s.create(c, OWNER).await.unwrap();
        for i in 0..5 {
            s.put(c, vec![i], 1024, u64::from(i)).await.unwrap();
        }
        let after_zero = s.fetch(c, 0, 100).await.unwrap();
        assert_eq!(after_zero.len(), 5);
        let after_two = s.fetch(c, 2, 100).await.unwrap();
        assert_eq!(after_two.len(), 3);
        assert_eq!(after_two[0].cursor, 3);
    }

    #[tokio::test]
    async fn fetch_respects_limit() {
        let s = InMemoryChannelStore::new();
        let c = cid(0x11);
        s.create(c, OWNER).await.unwrap();
        for i in 0..10 {
            s.put(c, vec![i], 1024, 0).await.unwrap();
        }
        let limited = s.fetch(c, 0, 3).await.unwrap();
        assert_eq!(limited.len(), 3);
        assert_eq!(limited[0].cursor, 1);
        assert_eq!(limited[2].cursor, 3);
    }

    #[test]
    fn channel_id_hex_round_trip() {
        let c = cid(0x42);
        let hex = c.to_hex();
        assert_eq!(hex.len(), 32);
        let back = ChannelId::from_hex(&hex).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn channel_id_rejects_wrong_length() {
        assert!(ChannelId::from_hex("abcdef").is_err());
        assert!(ChannelId::from_bytes(&[0u8; 8]).is_err());
    }

    #[test]
    fn channel_id_rejects_non_hex() {
        assert!(ChannelId::from_hex(&"z".repeat(32)).is_err());
    }

    fn sid(b: u8) -> SubscriberId {
        SubscriberId([b; 32])
    }

    #[tokio::test]
    async fn subscribe_round_trip() {
        let s = InMemoryChannelStore::new();
        let c = cid(0x10);
        let sub = sid(0x20);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, sub, 1_700_000_000).await.unwrap();
        assert!(s.is_subscribed(c, sub).await);
        let list = s.list_subscribers(c).await.unwrap();
        assert_eq!(list, vec![sub]);
        assert_eq!(s.total_subscribers().await, 1);
    }

    #[tokio::test]
    async fn subscribe_rejects_unknown_channel() {
        let s = InMemoryChannelStore::new();
        let err = s.subscribe(cid(0x10), sid(0x20), 0).await.unwrap_err();
        assert!(matches!(err, StorageError::UnknownChannel));
    }

    #[tokio::test]
    async fn subscribe_is_idempotent() {
        let s = InMemoryChannelStore::new();
        let c = cid(0x30);
        let sub = sid(0x40);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, sub, 1).await.unwrap();
        s.subscribe(c, sub, 2).await.unwrap();
        let list = s.list_subscribers(c).await.unwrap();
        assert_eq!(list.len(), 1);
    }

    #[tokio::test]
    async fn deliver_envelope_round_trip() {
        let s = InMemoryChannelStore::new();
        let c = cid(0x50);
        let sub = sid(0x60);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, sub, 0).await.unwrap();
        let envelope = vec![0xAAu8; 80]; // SEED_ENVELOPE_LEN
        s.deliver_envelope(c, sub, None, envelope.clone(), 80)
            .await
            .unwrap();
        let fetched = s.fetch_envelope(c, sub).await.unwrap().unwrap();
        assert_eq!(fetched, envelope);
    }

    #[tokio::test]
    async fn deliver_envelope_rejects_wrong_length() {
        let s = InMemoryChannelStore::new();
        let c = cid(0x70);
        let sub = sid(0x80);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, sub, 0).await.unwrap();
        let err = s
            .deliver_envelope(c, sub, None, vec![0u8; 79], 80)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            StorageError::BadEnvelopeLen {
                expected: 80,
                got: 79
            }
        ));
    }

    #[tokio::test]
    async fn deliver_envelope_rejects_unknown_subscriber() {
        let s = InMemoryChannelStore::new();
        let c = cid(0x90);
        s.create(c, OWNER).await.unwrap();
        let err = s
            .deliver_envelope(c, sid(0xA0), None, vec![0u8; 80], 80)
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::UnknownSubscriber));
    }

    #[tokio::test]
    async fn fetch_envelope_returns_none_before_delivery() {
        let s = InMemoryChannelStore::new();
        let c = cid(0xB0);
        let sub = sid(0xC0);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, sub, 0).await.unwrap();
        assert!(s.fetch_envelope(c, sub).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn envelope_latest_wins() {
        let s = InMemoryChannelStore::new();
        let c = cid(0xD0);
        let sub = sid(0xE0);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, sub, 0).await.unwrap();
        s.deliver_envelope(c, sub, None, vec![0x11; 80], 80)
            .await
            .unwrap();
        s.deliver_envelope(c, sub, None, vec![0x22; 80], 80)
            .await
            .unwrap();
        let fetched = s.fetch_envelope(c, sub).await.unwrap().unwrap();
        assert_eq!(fetched, vec![0x22; 80]);
    }

    #[test]
    fn subscriber_id_hex_round_trip() {
        let sub = sid(0xAB);
        let hex = sub.to_hex();
        assert_eq!(hex.len(), 64);
        assert_eq!(SubscriberId::from_hex(&hex).unwrap(), sub);
    }

    #[test]
    fn subscriber_id_rejects_wrong_length() {
        assert!(SubscriberId::from_hex("abc").is_err());
        assert!(SubscriberId::from_bytes(&[0u8; 31]).is_err());
        assert!(SubscriberId::from_bytes(&[0u8; 33]).is_err());
    }

    #[tokio::test]
    async fn genesis_epoch_is_zero() {
        let s = InMemoryChannelStore::new();
        let c = cid(0xF0);
        s.create(c, OWNER).await.unwrap();
        assert_eq!(s.current_epoch(c).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn current_epoch_rejects_unknown_channel() {
        let s = InMemoryChannelStore::new();
        let err = s.current_epoch(cid(0xF1)).await.unwrap_err();
        assert!(matches!(err, StorageError::UnknownChannel));
    }

    #[tokio::test]
    async fn rotate_bumps_epoch_and_replaces_envelopes() {
        let s = InMemoryChannelStore::new();
        let c = cid(0x01);
        let a = sid(0x02);
        let b = sid(0x03);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, a, 0).await.unwrap();
        s.subscribe(c, b, 0).await.unwrap();
        // Pre-rotation: deliver an "epoch 0" envelope for
        // each (so we can prove they get replaced).
        s.deliver_envelope(c, a, None, vec![0x00; 80], 80)
            .await
            .unwrap();
        s.deliver_envelope(c, b, None, vec![0x00; 80], 80)
            .await
            .unwrap();
        // Rotate.
        let new_epoch = s
            .rotate(c, vec![(a, vec![0xAA; 80]), (b, vec![0xBB; 80])], 80)
            .await
            .unwrap();
        assert_eq!(new_epoch, 1);
        assert_eq!(s.current_epoch(c).await.unwrap(), 1);
        assert_eq!(
            s.fetch_envelope(c, a).await.unwrap().unwrap(),
            vec![0xAA; 80]
        );
        assert_eq!(
            s.fetch_envelope(c, b).await.unwrap().unwrap(),
            vec![0xBB; 80]
        );
    }

    #[tokio::test]
    async fn rotate_is_atomic_on_wrong_length() {
        let s = InMemoryChannelStore::new();
        let c = cid(0x04);
        let a = sid(0x05);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, a, 0).await.unwrap();
        s.deliver_envelope(c, a, None, vec![0x00; 80], 80)
            .await
            .unwrap();
        // Second batch entry is wrong length — entire
        // rotation must abort without touching the
        // envelope or bumping the epoch.
        let err = s.rotate(c, vec![(a, vec![0u8; 79])], 80).await.unwrap_err();
        assert!(matches!(
            err,
            StorageError::BadEnvelopeLen {
                expected: 80,
                got: 79
            }
        ));
        assert_eq!(s.current_epoch(c).await.unwrap(), 0);
        assert_eq!(
            s.fetch_envelope(c, a).await.unwrap().unwrap(),
            vec![0x00; 80]
        );
    }

    #[tokio::test]
    async fn rotate_rejects_unsubscribed_pubkey() {
        let s = InMemoryChannelStore::new();
        let c = cid(0x06);
        s.create(c, OWNER).await.unwrap();
        // No subscribe; try to rotate to an unknown peer.
        let err = s
            .rotate(c, vec![(sid(0x07), vec![0u8; 80])], 80)
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::UnknownSubscriber));
        assert_eq!(s.current_epoch(c).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn rotate_empty_batch_still_bumps_epoch() {
        // A scheduled rotation on a channel with zero
        // subscribers should still increment the epoch —
        // future-self may subscribe and check the epoch
        // to decide whether to ask for a fresh envelope.
        let s = InMemoryChannelStore::new();
        let c = cid(0x08);
        s.create(c, OWNER).await.unwrap();
        let new_epoch = s.rotate(c, vec![], 80).await.unwrap();
        assert_eq!(new_epoch, 1);
    }

    // -----------------------------------------------------
    // prune_posts_older_than (G4-1-retention)
    // -----------------------------------------------------

    #[tokio::test]
    async fn prune_removes_old_posts_keeps_fresh() {
        let s = InMemoryChannelStore::new();
        let c = cid(0xA1);
        s.create(c, OWNER).await.unwrap();
        // Five posts at timestamps 100, 200, 300, 400, 500.
        for i in 1..=5 {
            s.put(c, vec![u8::try_from(i).unwrap()], 1024, i * 100)
                .await
                .unwrap();
        }
        // Cutoff at 350 — drops the three oldest (100,
        // 200, 300).
        let removed = s.prune_posts_older_than(350).await.unwrap();
        assert_eq!(removed, 3);
        // Two survivors remain (400, 500). Cursors are
        // preserved on the survivors — fetch starting at
        // 0 returns them in cursor order.
        let survivors = s.fetch(c, 0, 100).await.unwrap();
        assert_eq!(survivors.len(), 2);
        assert_eq!(survivors[0].server_ts_ms, 400);
        assert_eq!(survivors[1].server_ts_ms, 500);
    }

    #[tokio::test]
    async fn prune_with_future_cutoff_drops_everything() {
        let s = InMemoryChannelStore::new();
        let c = cid(0xA2);
        s.create(c, OWNER).await.unwrap();
        for i in 1..=3 {
            s.put(c, vec![u8::try_from(i).unwrap()], 1024, i * 100)
                .await
                .unwrap();
        }
        let removed = s.prune_posts_older_than(u64::MAX).await.unwrap();
        assert_eq!(removed, 3);
        assert_eq!(s.fetch(c, 0, 100).await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn prune_with_zero_cutoff_drops_nothing() {
        let s = InMemoryChannelStore::new();
        let c = cid(0xA3);
        s.create(c, OWNER).await.unwrap();
        s.put(c, vec![1], 1024, 100).await.unwrap();
        s.put(c, vec![2], 1024, 200).await.unwrap();
        let removed = s.prune_posts_older_than(0).await.unwrap();
        assert_eq!(removed, 0);
        assert_eq!(s.fetch(c, 0, 100).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn prune_preserves_cursor_monotonicity() {
        // A subscriber paginating with WHERE cursor > ?
        // must not see cursors reused after a prune. We
        // verify by checking that put-after-prune assigns
        // a strictly-greater cursor than the highest
        // pre-prune cursor (i.e. `next_cursor` was not
        // rolled back).
        let s = InMemoryChannelStore::new();
        let c = cid(0xA4);
        s.create(c, OWNER).await.unwrap();
        let a = s.put(c, vec![1], 1024, 100).await.unwrap();
        let b = s.put(c, vec![2], 1024, 200).await.unwrap();
        assert_eq!(a.cursor, 1);
        assert_eq!(b.cursor, 2);
        s.prune_posts_older_than(u64::MAX).await.unwrap();
        let c3 = s.put(c, vec![3], 1024, 300).await.unwrap();
        // Cursor 3 — not 1 — proves next_cursor survived.
        assert_eq!(c3.cursor, 3);
    }

    #[tokio::test]
    async fn prune_runs_across_multiple_channels() {
        let s = InMemoryChannelStore::new();
        let c1 = cid(0xB1);
        let c2 = cid(0xB2);
        s.create(c1, OWNER).await.unwrap();
        s.create(c2, OWNER).await.unwrap();
        s.put(c1, vec![1], 1024, 100).await.unwrap();
        s.put(c1, vec![2], 1024, 500).await.unwrap();
        s.put(c2, vec![3], 1024, 200).await.unwrap();
        s.put(c2, vec![4], 1024, 600).await.unwrap();
        // Cutoff 300 — drops the 100 and 200 timestamps,
        // one from each channel.
        let removed = s.prune_posts_older_than(300).await.unwrap();
        assert_eq!(removed, 2);
        assert_eq!(s.fetch(c1, 0, 100).await.unwrap().len(), 1);
        assert_eq!(s.fetch(c2, 0, 100).await.unwrap().len(), 1);
    }

    // -----------------------------------------------------
    // touch_subscriber_activity + prune_subscribers_older_than
    // (G4-1-subretention)
    // -----------------------------------------------------

    #[tokio::test]
    async fn touch_bumps_last_active_at() {
        let s = InMemoryChannelStore::new();
        let c = cid(0xC1);
        let sub = sid(0xC2);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, sub, 100).await.unwrap();
        // Touch with a later timestamp.
        s.touch_subscriber_activity(c, sub, 500).await.unwrap();
        // Prune at cutoff 200 — original 100 would have
        // been dropped, but the touch bumped to 500.
        let removed = s.prune_subscribers_older_than(200).await.unwrap();
        assert_eq!(removed, 0);
        assert!(s.is_subscribed(c, sub).await);
    }

    #[tokio::test]
    async fn touch_on_unregistered_is_noop() {
        // Touch on a (channel, subscriber) pair that
        // never registered is silently ignored.
        let s = InMemoryChannelStore::new();
        let c = cid(0xC3);
        s.create(c, OWNER).await.unwrap();
        // No subscribe — touch should not error.
        s.touch_subscriber_activity(c, sid(0xC4), 1_000)
            .await
            .unwrap();
        assert!(!s.is_subscribed(c, sid(0xC4)).await);
    }

    #[tokio::test]
    async fn prune_subscribers_removes_stale_keeps_active() {
        let s = InMemoryChannelStore::new();
        let c = cid(0xD1);
        let stale = sid(0xD2);
        let active = sid(0xD3);
        s.create(c, OWNER).await.unwrap();
        // Stale subscriber: registered at 100, never
        // touched.
        s.subscribe(c, stale, 100).await.unwrap();
        // Active subscriber: registered at 200, touched
        // at 800.
        s.subscribe(c, active, 200).await.unwrap();
        s.touch_subscriber_activity(c, active, 800).await.unwrap();
        // Cutoff 500 — drops `stale` (last_active=100),
        // keeps `active` (last_active=800).
        let removed = s.prune_subscribers_older_than(500).await.unwrap();
        assert_eq!(removed, 1);
        assert!(!s.is_subscribed(c, stale).await);
        assert!(s.is_subscribed(c, active).await);
    }

    #[tokio::test]
    async fn prune_subscribers_cascades_envelope() {
        // The SQL impl gets envelope-cascade for free via
        // FK ON DELETE CASCADE; the in-memory impl drops
        // the matching envelope entries explicitly.
        // Either way, fetch_envelope after a prune
        // returns UnknownSubscriber.
        let s = InMemoryChannelStore::new();
        let c = cid(0xE1);
        let sub = sid(0xE2);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, sub, 100).await.unwrap();
        s.deliver_envelope(c, sub, None, vec![0xAB; 80], 80)
            .await
            .unwrap();
        // Pre-prune: envelope is fetchable.
        assert!(s.fetch_envelope(c, sub).await.unwrap().is_some());
        // Cutoff 500 drops the subscriber + envelope.
        let removed = s.prune_subscribers_older_than(500).await.unwrap();
        assert_eq!(removed, 1);
        // Post-prune: subscriber is unknown, envelope is gone.
        let err = s.fetch_envelope(c, sub).await.unwrap_err();
        assert!(matches!(err, StorageError::UnknownSubscriber));
    }

    #[tokio::test]
    async fn prune_subscribers_zero_cutoff_drops_nothing() {
        let s = InMemoryChannelStore::new();
        let c = cid(0xE3);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, sid(0xE4), 100).await.unwrap();
        s.subscribe(c, sid(0xE5), 200).await.unwrap();
        let removed = s.prune_subscribers_older_than(0).await.unwrap();
        assert_eq!(removed, 0);
        assert_eq!(s.list_subscribers(c).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn prune_subscribers_across_multiple_channels() {
        let s = InMemoryChannelStore::new();
        let c1 = cid(0xF1);
        let c2 = cid(0xF2);
        s.create(c1, OWNER).await.unwrap();
        s.create(c2, OWNER).await.unwrap();
        s.subscribe(c1, sid(0xF3), 100).await.unwrap();
        s.subscribe(c1, sid(0xF4), 700).await.unwrap();
        s.subscribe(c2, sid(0xF5), 200).await.unwrap();
        s.subscribe(c2, sid(0xF6), 800).await.unwrap();
        // Cutoff 500 — drops the 100 and 200 entries
        // (one from each channel); keeps the 700 + 800.
        let removed = s.prune_subscribers_older_than(500).await.unwrap();
        assert_eq!(removed, 2);
        assert_eq!(s.list_subscribers(c1).await.unwrap().len(), 1);
        assert_eq!(s.list_subscribers(c2).await.unwrap().len(), 1);
    }

    // -----------------------------------------------------
    // stage_envelopes + commit_rotation (ADR-0030 — batched
    // rotation fanout; C-42 atomicity extended to the staged
    // path)
    // -----------------------------------------------------

    #[tokio::test]
    async fn stage_then_commit_promotes_and_bumps_epoch() {
        let s = InMemoryChannelStore::new();
        let c = cid(0x01);
        let a = sid(0x02);
        let b = sid(0x03);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, a, 0).await.unwrap();
        s.subscribe(c, b, 0).await.unwrap();
        // Pre-rotation "epoch 0" envelopes so we can prove
        // they get replaced only after commit.
        s.deliver_envelope(c, a, None, vec![0x00; 80], 80)
            .await
            .unwrap();
        s.deliver_envelope(c, b, None, vec![0x00; 80], 80)
            .await
            .unwrap();
        // Stage target epoch 1 for both.
        s.stage_envelopes(c, 1, vec![(a, vec![0xAA; 80]), (b, vec![0xBB; 80])], 80)
            .await
            .unwrap();
        // Commit.
        let new_epoch = s.commit_rotation(c, 1).await.unwrap();
        assert_eq!(new_epoch, 1);
        assert_eq!(s.current_epoch(c).await.unwrap(), 1);
        assert_eq!(
            s.fetch_envelope(c, a).await.unwrap().unwrap(),
            vec![0xAA; 80]
        );
        assert_eq!(
            s.fetch_envelope(c, b).await.unwrap().unwrap(),
            vec![0xBB; 80]
        );
    }

    #[tokio::test]
    async fn staged_envelopes_are_not_served_before_commit() {
        // C-42 core: the prepare phase is invisible to
        // subscribers — the read path never sees a staged row.
        let s = InMemoryChannelStore::new();
        let c = cid(0x04);
        let a = sid(0x05);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, a, 0).await.unwrap();
        s.deliver_envelope(c, a, None, vec![0x11; 80], 80)
            .await
            .unwrap();
        // Stage a new envelope but do NOT commit.
        s.stage_envelopes(c, 1, vec![(a, vec![0x22; 80])], 80)
            .await
            .unwrap();
        // Read path still returns the OLD (pre-stage)
        // envelope, and the epoch is untouched.
        assert_eq!(
            s.fetch_envelope(c, a).await.unwrap().unwrap(),
            vec![0x11; 80]
        );
        assert_eq!(s.current_epoch(c).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn stage_rejects_wrong_target_epoch() {
        let s = InMemoryChannelStore::new();
        let c = cid(0x06);
        let a = sid(0x07);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, a, 0).await.unwrap();
        // current_epoch is 0, so the only valid target is 1.
        let err = s
            .stage_envelopes(c, 2, vec![(a, vec![0xAA; 80])], 80)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            StorageError::StaleTargetEpoch {
                expected: 1,
                got: 2
            }
        ));
        // Nothing staged, epoch untouched.
        assert_eq!(s.current_epoch(c).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn commit_rejects_wrong_target_epoch() {
        let s = InMemoryChannelStore::new();
        let c = cid(0x08);
        let a = sid(0x09);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, a, 0).await.unwrap();
        s.stage_envelopes(c, 1, vec![(a, vec![0xAA; 80])], 80)
            .await
            .unwrap();
        // Commit with the wrong target (2, not 1).
        let err = s.commit_rotation(c, 2).await.unwrap_err();
        assert!(matches!(
            err,
            StorageError::StaleTargetEpoch {
                expected: 1,
                got: 2
            }
        ));
        // Epoch untouched — the staged rows survive for a
        // correct retry.
        assert_eq!(s.current_epoch(c).await.unwrap(), 0);
        assert_eq!(s.commit_rotation(c, 1).await.unwrap(), 1);
        assert_eq!(
            s.fetch_envelope(c, a).await.unwrap().unwrap(),
            vec![0xAA; 80]
        );
    }

    #[tokio::test]
    async fn abandoned_prepare_leaves_epoch_and_envelopes_untouched() {
        // A stage that is never committed must not alter the
        // live epoch or the live envelope set.
        let s = InMemoryChannelStore::new();
        let c = cid(0x0A);
        let a = sid(0x0B);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, a, 0).await.unwrap();
        s.deliver_envelope(c, a, None, vec![0x33; 80], 80)
            .await
            .unwrap();
        s.stage_envelopes(c, 1, vec![(a, vec![0x44; 80])], 80)
            .await
            .unwrap();
        // Never commit. Live state is exactly as before.
        assert_eq!(s.current_epoch(c).await.unwrap(), 0);
        assert_eq!(
            s.fetch_envelope(c, a).await.unwrap().unwrap(),
            vec![0x33; 80]
        );
    }

    #[tokio::test]
    async fn restaging_is_idempotent_and_merges_chunks() {
        // Re-sending a subscriber for the same target
        // overwrites (resumability); a second chunk with a
        // different subscriber merges into the same staged
        // set. Commit promotes the union.
        let s = InMemoryChannelStore::new();
        let c = cid(0x0C);
        let a = sid(0x0D);
        let b = sid(0x0E);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, a, 0).await.unwrap();
        s.subscribe(c, b, 0).await.unwrap();
        // Chunk 1: a with a first value.
        s.stage_envelopes(c, 1, vec![(a, vec![0x01; 80])], 80)
            .await
            .unwrap();
        // Chunk 1 re-sent with a corrected value (overwrite).
        s.stage_envelopes(c, 1, vec![(a, vec![0xAA; 80])], 80)
            .await
            .unwrap();
        // Chunk 2: b (merges — does not drop a).
        s.stage_envelopes(c, 1, vec![(b, vec![0xBB; 80])], 80)
            .await
            .unwrap();
        assert_eq!(s.commit_rotation(c, 1).await.unwrap(), 1);
        assert_eq!(
            s.fetch_envelope(c, a).await.unwrap().unwrap(),
            vec![0xAA; 80]
        );
        assert_eq!(
            s.fetch_envelope(c, b).await.unwrap().unwrap(),
            vec![0xBB; 80]
        );
    }

    #[tokio::test]
    async fn stage_rejects_unsubscribed_recipient() {
        let s = InMemoryChannelStore::new();
        let c = cid(0x0F);
        s.create(c, OWNER).await.unwrap();
        // No subscribe; try to stage to an unknown peer.
        let err = s
            .stage_envelopes(c, 1, vec![(sid(0x10), vec![0u8; 80])], 80)
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::UnknownSubscriber));
        assert_eq!(s.current_epoch(c).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn stage_is_atomic_on_wrong_length() {
        let s = InMemoryChannelStore::new();
        let c = cid(0x11);
        let a = sid(0x12);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, a, 0).await.unwrap();
        let err = s
            .stage_envelopes(c, 1, vec![(a, vec![0u8; 79])], 80)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            StorageError::BadEnvelopeLen {
                expected: 80,
                got: 79
            }
        ));
        // Nothing staged — a follow-up correct commit that
        // has no staged rows still bumps but promotes nothing.
        assert_eq!(s.current_epoch(c).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn commit_partial_coverage_keeps_prior_envelope() {
        // A registered subscriber NOT in the staged set keeps
        // their prior-epoch envelope after commit (partial
        // coverage, ADR-0030 open question 3).
        let s = InMemoryChannelStore::new();
        let c = cid(0x13);
        let staged_sub = sid(0x14);
        let untouched_sub = sid(0x15);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, staged_sub, 0).await.unwrap();
        s.subscribe(c, untouched_sub, 0).await.unwrap();
        s.deliver_envelope(c, staged_sub, None, vec![0x00; 80], 80)
            .await
            .unwrap();
        s.deliver_envelope(c, untouched_sub, None, vec![0x99; 80], 80)
            .await
            .unwrap();
        // Only stage the first subscriber.
        s.stage_envelopes(c, 1, vec![(staged_sub, vec![0xAA; 80])], 80)
            .await
            .unwrap();
        assert_eq!(s.commit_rotation(c, 1).await.unwrap(), 1);
        // Staged subscriber got the new envelope.
        assert_eq!(
            s.fetch_envelope(c, staged_sub).await.unwrap().unwrap(),
            vec![0xAA; 80]
        );
        // Untouched subscriber keeps their prior envelope.
        assert_eq!(
            s.fetch_envelope(c, untouched_sub).await.unwrap().unwrap(),
            vec![0x99; 80]
        );
    }

    #[tokio::test]
    async fn commit_with_no_staged_rows_still_bumps_epoch() {
        // A commit with an empty staged set advances the
        // epoch (parity with rotate_empty_batch_still_bumps).
        let s = InMemoryChannelStore::new();
        let c = cid(0x16);
        s.create(c, OWNER).await.unwrap();
        assert_eq!(s.commit_rotation(c, 1).await.unwrap(), 1);
        assert_eq!(s.current_epoch(c).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn commit_clears_staged_rows_so_next_rotation_starts_clean() {
        // After a commit, the staged rows for the channel are
        // gone — a subsequent stage targets current+1 anew.
        let s = InMemoryChannelStore::new();
        let c = cid(0x17);
        let a = sid(0x18);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, a, 0).await.unwrap();
        s.stage_envelopes(c, 1, vec![(a, vec![0xAA; 80])], 80)
            .await
            .unwrap();
        assert_eq!(s.commit_rotation(c, 1).await.unwrap(), 1);
        // Committing epoch 1 again is stale.
        let err = s.commit_rotation(c, 1).await.unwrap_err();
        assert!(matches!(err, StorageError::StaleTargetEpoch { .. }));
        // The next rotation stages epoch 2.
        s.stage_envelopes(c, 2, vec![(a, vec![0xCC; 80])], 80)
            .await
            .unwrap();
        assert_eq!(s.commit_rotation(c, 2).await.unwrap(), 2);
        assert_eq!(
            s.fetch_envelope(c, a).await.unwrap().unwrap(),
            vec![0xCC; 80]
        );
    }

    #[tokio::test]
    async fn stage_and_commit_reject_unknown_channel() {
        let s = InMemoryChannelStore::new();
        let c = cid(0x19);
        let stage_err = s.stage_envelopes(c, 1, vec![], 80).await.unwrap_err();
        assert!(matches!(stage_err, StorageError::UnknownChannel));
        let commit_err = s.commit_rotation(c, 1).await.unwrap_err();
        assert!(matches!(commit_err, StorageError::UnknownChannel));
    }

    // -----------------------------------------------------
    // Multi-epoch envelope retention (ADR-0031)
    // -----------------------------------------------------

    fn env80(byte: u8) -> Vec<u8> {
        vec![byte; 80]
    }

    #[tokio::test]
    async fn deliver_retains_highest_n_epochs_and_prunes_oldest() {
        // Default retention is 4. Deliver at epochs 1..=5
        // (driving `current_epoch` up with empty rotations so
        // each deliver files at the live epoch), then assert
        // only the highest 4 survive — epoch 1 is pruned.
        let s = InMemoryChannelStore::new();
        let c = cid(0x20);
        let sub = sid(0x21);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, sub, 0).await.unwrap();
        for e in 1u32..=5 {
            // Empty rotation bumps `current_epoch` to `e`.
            assert_eq!(s.rotate(c, vec![], 80).await.unwrap(), e);
            // Deliver at the live epoch (None -> current).
            let byte = u8::try_from(e).unwrap();
            s.deliver_envelope(c, sub, None, env80(byte), 80)
                .await
                .unwrap();
        }
        // The whole retained set: highest 4 epochs, ascending.
        let all = s.fetch_envelopes(c, sub).await.unwrap();
        let epochs: Vec<u32> = all.iter().map(|(e, _)| *e).collect();
        assert_eq!(epochs, vec![2, 3, 4, 5], "epoch 1 must be pruned");
        assert_eq!(all.len(), 4);
        // Singular fetch returns the highest retained epoch.
        assert_eq!(s.fetch_envelope(c, sub).await.unwrap().unwrap(), env80(5));
        // The pruned epoch-1 envelope is gone from the set.
        assert!(all.iter().all(|(e, _)| *e != 1));
    }

    #[tokio::test]
    async fn deliver_future_epoch_is_refused() {
        // A deliver at an epoch greater than `current_epoch`
        // is refused (C-42 — no future pre-seed).
        let s = InMemoryChannelStore::new();
        let c = cid(0x22);
        let sub = sid(0x23);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, sub, 0).await.unwrap();
        // current_epoch is 0; ask to file at epoch 3.
        let err = s
            .deliver_envelope(c, sub, Some(3), env80(0xAA), 80)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            StorageError::EnvelopeEpochInFuture { current: 0, got: 3 }
        ));
        // Nothing was filed.
        assert!(s.fetch_envelope(c, sub).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn deliver_at_explicit_past_epoch_is_accepted() {
        // A deliver at an explicit epoch <= current is filed
        // there (a re-syncing subscriber back-filling a gap).
        let s = InMemoryChannelStore::new();
        let c = cid(0x24);
        let sub = sid(0x25);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, sub, 0).await.unwrap();
        // Raise current_epoch to 3 with empty rotations.
        for _ in 0..3 {
            s.rotate(c, vec![], 80).await.unwrap();
        }
        assert_eq!(s.current_epoch(c).await.unwrap(), 3);
        // File explicitly at epoch 2 (< current).
        s.deliver_envelope(c, sub, Some(2), env80(0x2A), 80)
            .await
            .unwrap();
        let all = s.fetch_envelopes(c, sub).await.unwrap();
        assert_eq!(all, vec![(2u32, env80(0x2A))]);
    }

    #[tokio::test]
    async fn commit_rotation_retains_n_across_many_rotations() {
        // Drive six stage+commit rotations for one subscriber
        // and assert the retained set is the last N=4 epochs.
        let s = InMemoryChannelStore::new();
        let c = cid(0x26);
        let sub = sid(0x27);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, sub, 0).await.unwrap();
        for e in 1u32..=6 {
            let byte = u8::try_from(e).unwrap();
            s.stage_envelopes(c, e, vec![(sub, env80(byte))], 80)
                .await
                .unwrap();
            assert_eq!(s.commit_rotation(c, e).await.unwrap(), e);
        }
        let all = s.fetch_envelopes(c, sub).await.unwrap();
        let epochs: Vec<u32> = all.iter().map(|(e, _)| *e).collect();
        assert_eq!(epochs, vec![3, 4, 5, 6], "only the last N=4 remain");
        assert_eq!(s.fetch_envelope(c, sub).await.unwrap().unwrap(), env80(6));
    }

    #[tokio::test]
    async fn rotate_retains_n_across_many_rotations() {
        // Same as above but via the single-shot `rotate` path.
        let s = InMemoryChannelStore::new();
        let c = cid(0x28);
        let sub = sid(0x29);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, sub, 0).await.unwrap();
        for e in 1u32..=6 {
            let byte = u8::try_from(e).unwrap();
            assert_eq!(s.rotate(c, vec![(sub, env80(byte))], 80).await.unwrap(), e);
        }
        let epochs: Vec<u32> = s
            .fetch_envelopes(c, sub)
            .await
            .unwrap()
            .iter()
            .map(|(e, _)| *e)
            .collect();
        assert_eq!(epochs, vec![3, 4, 5, 6]);
    }

    #[tokio::test]
    async fn fetch_envelopes_empty_when_none_delivered() {
        let s = InMemoryChannelStore::new();
        let c = cid(0x2A);
        let sub = sid(0x2B);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, sub, 0).await.unwrap();
        assert_eq!(s.fetch_envelopes(c, sub).await.unwrap(), Vec::new());
    }

    #[tokio::test]
    async fn fetch_envelopes_gates_unknown_channel_and_subscriber() {
        let s = InMemoryChannelStore::new();
        let c = cid(0x2C);
        // Unknown channel.
        let err = s.fetch_envelopes(c, sid(0x2D)).await.unwrap_err();
        assert!(matches!(err, StorageError::UnknownChannel));
        // Known channel, unregistered subscriber.
        s.create(c, OWNER).await.unwrap();
        let err = s.fetch_envelopes(c, sid(0x2D)).await.unwrap_err();
        assert!(matches!(err, StorageError::UnknownSubscriber));
    }

    #[tokio::test]
    async fn with_envelope_retention_controls_window_and_floors_at_one() {
        // A custom retention of 2 keeps only the highest 2.
        let s = InMemoryChannelStore::new().with_envelope_retention(2);
        let c = cid(0x2E);
        let sub = sid(0x2F);
        s.create(c, OWNER).await.unwrap();
        s.subscribe(c, sub, 0).await.unwrap();
        for e in 1u32..=4 {
            let byte = u8::try_from(e).unwrap();
            s.rotate(c, vec![(sub, env80(byte))], 80).await.unwrap();
        }
        let epochs: Vec<u32> = s
            .fetch_envelopes(c, sub)
            .await
            .unwrap()
            .iter()
            .map(|(e, _)| *e)
            .collect();
        assert_eq!(epochs, vec![3, 4], "retention 2 keeps the highest 2");

        // A retention of 0 is clamped to 1 (keep only latest).
        let s1 = InMemoryChannelStore::new().with_envelope_retention(0);
        let c1 = cid(0x30);
        let sub1 = sid(0x31);
        s1.create(c1, OWNER).await.unwrap();
        s1.subscribe(c1, sub1, 0).await.unwrap();
        for e in 1u32..=3 {
            let byte = u8::try_from(e).unwrap();
            s1.rotate(c1, vec![(sub1, env80(byte))], 80).await.unwrap();
        }
        let epochs1: Vec<u32> = s1
            .fetch_envelopes(c1, sub1)
            .await
            .unwrap()
            .iter()
            .map(|(e, _)| *e)
            .collect();
        assert_eq!(epochs1, vec![3], "retention 0 floors to 1");
    }

    fn stored(name: &str, updated_at_ms: u64) -> StoredListing {
        StoredListing {
            topic: 1,
            language: "en".into(),
            handle: "abc".into(),
            name: name.into(),
            description: String::new(),
            bytes: name.as_bytes().to_vec(),
            updated_at_ms,
        }
    }

    #[tokio::test]
    async fn listing_rules_match_the_sql_store() {
        let s = InMemoryChannelStore::new();
        let c = cid(40);
        assert!(matches!(s.create(c, OWNER).await, Ok(())));
        assert!(matches!(
            s.create(c, [8; OWNER_PUBKEY_LEN]).await,
            Err(StorageError::OwnerMismatch)
        ));
        s.put_listing(c, stored("A", 5), 100).await.unwrap();
        assert!(matches!(
            s.put_listing(c, stored("B", 5), 200).await,
            Err(StorageError::StaleListing)
        ));
        s.withdraw_listing(c).await.unwrap();
        s.put_listing(c, stored("C", 1), 300).await.unwrap();
        let entry = s.listing(c, 400).await.unwrap().unwrap();
        assert_eq!(entry.first_listed_at_ms, 100);
        // A removal keeps a block in force; unblock lifts it.
        s.moderate(c, ModerationAction::Block, "spam", 1)
            .await
            .unwrap();
        s.moderate(c, ModerationAction::Remove, "dup", 2)
            .await
            .unwrap();
        assert!(matches!(
            s.put_listing(c, stored("D", 9), 500).await,
            Err(StorageError::ListingBlocked)
        ));
        s.moderate(c, ModerationAction::Unblock, "appeal", 3)
            .await
            .unwrap();
        s.put_listing(c, stored("D", 9), 500).await.unwrap();
        // Nonces are single use.
        assert!(s.consume_owner_nonce([1; 16], 10).await.unwrap());
        assert!(!s.consume_owner_nonce([1; 16], 11).await.unwrap());
        assert_eq!(s.prune_owner_nonces_older_than(11).await.unwrap(), 1);
    }

    // ADR-0060 - member cards.

    async fn member_store() -> InMemoryChannelStore {
        let s = InMemoryChannelStore::new();
        s.create(cid(0x70), OWNER).await.unwrap();
        for b in [1u8, 2, 3] {
            s.subscribe(cid(0x70), sid(b), 0).await.unwrap();
        }
        s
    }

    #[tokio::test]
    async fn member_cards_page_in_subscriber_order() {
        let s = member_store().await;
        for b in [3u8, 1, 2] {
            s.put_member_card(cid(0x70), sid(b), 0, vec![b; 40])
                .await
                .unwrap();
        }
        let first = s.list_member_cards(cid(0x70), None, 2).await.unwrap();
        assert_eq!(
            first.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            vec![sid(1), sid(2)]
        );
        let rest = s
            .list_member_cards(cid(0x70), Some(sid(2)), 2)
            .await
            .unwrap();
        assert_eq!(rest, vec![(sid(3), vec![3u8; 40])]);
        assert!(s
            .list_member_cards(cid(0x70), Some(sid(3)), 2)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn a_member_card_is_bounded_by_the_epochs() {
        let s = member_store().await;
        assert!(matches!(
            s.put_member_card(cid(0x70), sid(1), 1, vec![1; 40]).await,
            Err(StorageError::MemberCardEpochInFuture { current: 0, got: 1 })
        ));
        s.rotate(cid(0x70), vec![], 80).await.unwrap();
        s.rotate(cid(0x70), vec![], 80).await.unwrap();
        s.put_member_card(cid(0x70), sid(1), 2, vec![2; 40])
            .await
            .unwrap();
        assert!(matches!(
            s.put_member_card(cid(0x70), sid(1), 1, vec![1; 40]).await,
            Err(StorageError::MemberCardStale { stored: 2, got: 1 })
        ));
        // Same epoch replaces (a profile edit within the epoch).
        s.put_member_card(cid(0x70), sid(1), 2, vec![9; 40])
            .await
            .unwrap();
        assert_eq!(
            s.list_member_cards(cid(0x70), None, 10).await.unwrap(),
            vec![(sid(1), vec![9; 40])]
        );
        assert!(matches!(
            s.put_member_card(cid(0x70), sid(9), 0, vec![1; 40]).await,
            Err(StorageError::UnknownSubscriber)
        ));
        assert!(matches!(
            s.put_member_card(cid(0x71), sid(1), 0, vec![1; 40]).await,
            Err(StorageError::UnknownChannel)
        ));
    }

    #[tokio::test]
    async fn member_cards_go_with_delete_and_with_the_subscriber_prune() {
        let s = member_store().await;
        s.put_member_card(cid(0x70), sid(1), 0, vec![1; 40])
            .await
            .unwrap();
        s.put_member_card(cid(0x70), sid(2), 0, vec![2; 40])
            .await
            .unwrap();
        s.delete_member_card(cid(0x70), sid(1)).await.unwrap();
        s.delete_member_card(cid(0x70), sid(1)).await.unwrap();
        s.touch_subscriber_activity(cid(0x70), sid(2), 10)
            .await
            .unwrap();
        s.prune_subscribers_older_than(5).await.unwrap();
        // sid(2) was touched at 10 and survives; its card stays.
        assert_eq!(
            s.list_member_cards(cid(0x70), None, 10).await.unwrap(),
            vec![(sid(2), vec![2; 40])]
        );
        s.prune_subscribers_older_than(11).await.unwrap();
        assert!(s
            .list_member_cards(cid(0x70), None, 10)
            .await
            .unwrap()
            .is_empty());
    }
}
