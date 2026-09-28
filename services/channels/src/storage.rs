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

/// 32-byte subscriber Ed25519 public key (ADR-0058), used
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
    /// A member card names an epoch greater than the channel's
    /// `current_epoch` (ADR-0058): a card is sealed under a seed a reader
    /// already holds, never a future one.
    #[error("member card epoch {got} is in the future (current epoch is {current})")]
    MemberCardEpochInFuture {
        /// The channel's live epoch at refusal time.
        current: u32,
        /// The epoch the card carries.
        got: u32,
    },
    /// A member card names an epoch lower than the card it would replace
    /// (ADR-0058): a card only ever moves forward, so a replayed old card
    /// cannot roll a subscriber's profile back to a retired seed.
    #[error("member card epoch {got} is older than the stored card's {stored}")]
    MemberCardStale {
        /// The stored card's epoch.
        stored: u32,
        /// The epoch the refused card carries.
        got: u32,
    },
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
    /// Register a fresh channel. Idempotent.
    async fn create(&self, channel: ChannelId) -> Result<(), StorageError>;

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
    ) -> Result<(), StorageError>;

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
    /// `cutoff_ms`. Returns the number of subscriber
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

    /// Store `(channel, subscriber)`'s member card (ADR-0058), replacing the
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
}

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
    /// Channel id → (subscriber → (epoch, opaque member card)), ADR-0058.
    /// A `BTreeMap` so a page in subscriber-key order is a range scan.
    member_cards: HashMap<ChannelId, BTreeMap<SubscriberId, (u32, Vec<u8>)>>,
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
    async fn create(&self, channel: ChannelId) -> Result<(), StorageError> {
        let mut guard = self.inner.write().await;
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
    async fn subscribe(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
        server_ts_ms: u64,
    ) -> Result<(), StorageError> {
        let mut guard = self.inner.write().await;
        if !guard.posts.contains_key(&channel) {
            return Err(StorageError::UnknownChannel);
        }
        guard
            .subscribers
            .entry(channel)
            .or_default()
            .insert(subscriber, server_ts_ms);
        Ok(())
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
            if let Some(set) = guard.subscribers.get_mut(&channel) {
                if set.remove(&sub).is_some() {
                    removed = removed.saturating_add(1);
                }
            }
            guard.envelopes.remove(&(channel, sub));
            if let Some(cards) = guard.member_cards.get_mut(&channel) {
                cards.remove(&sub);
            }
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cid(b: u8) -> ChannelId {
        ChannelId([b; 16])
    }

    #[tokio::test]
    async fn create_is_idempotent() {
        let s = InMemoryChannelStore::new();
        let c = cid(0xAA);
        s.create(c).await.unwrap();
        s.create(c).await.unwrap();
        assert!(s.exists(c).await);
    }

    #[tokio::test]
    async fn put_assigns_monotone_cursors() {
        let s = InMemoryChannelStore::new();
        let c = cid(0xBB);
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
        let err = s.put(c, vec![], 1024, 0).await.unwrap_err();
        assert!(matches!(err, StorageError::EmptyPost));
    }

    #[tokio::test]
    async fn put_rejects_oversize_payload() {
        let s = InMemoryChannelStore::new();
        let c = cid(0xEE);
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
        s.subscribe(c, sub, 0).await.unwrap();
        assert!(s.fetch_envelope(c, sub).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn envelope_latest_wins() {
        let s = InMemoryChannelStore::new();
        let c = cid(0xD0);
        let sub = sid(0xE0);
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c1).await.unwrap();
        s.create(c2).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c1).await.unwrap();
        s.create(c2).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
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
        s.create(c).await.unwrap();
        let err = s.fetch_envelopes(c, sid(0x2D)).await.unwrap_err();
        assert!(matches!(err, StorageError::UnknownSubscriber));
    }

    #[tokio::test]
    async fn with_envelope_retention_controls_window_and_floors_at_one() {
        // A custom retention of 2 keeps only the highest 2.
        let s = InMemoryChannelStore::new().with_envelope_retention(2);
        let c = cid(0x2E);
        let sub = sid(0x2F);
        s.create(c).await.unwrap();
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
        s1.create(c1).await.unwrap();
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

    // ADR-0058 - member cards.

    async fn member_store() -> InMemoryChannelStore {
        let s = InMemoryChannelStore::new();
        s.create(cid(0x70)).await.unwrap();
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
