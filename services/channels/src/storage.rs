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

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

/// 32-byte subscriber X25519 public key, used as the
/// subscriber's identifier on the wire. Stored as 64-char
/// lowercase hex in URLs; this type is the parsed binary
/// form. The server NEVER does Diffie-Hellman against this
/// key — it only matches identities in the registry. The
/// publisher derives the per-subscriber HPKE envelope
/// using this same pubkey (see
/// [`lakko_channels::subscribe::seal_seed`]).
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
    /// X25519 public key, ready for
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
/// All 13 methods are async — even on the in-memory path
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

    /// Register a subscriber against the channel.
    async fn subscribe(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
        server_ts_ms: u64,
    ) -> Result<(), StorageError>;

    /// `true` if the subscriber is registered on the
    /// channel.
    async fn is_subscribed(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
    ) -> bool;

    /// List every subscriber registered on the channel.
    async fn list_subscribers(
        &self,
        channel: ChannelId,
    ) -> Result<Vec<SubscriberId>, StorageError>;

    /// Store the publisher-supplied HPKE seed envelope for
    /// `(channel, subscriber)`. Envelope is opaque.
    async fn deliver_envelope(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
        envelope: Vec<u8>,
        expected_len: usize,
    ) -> Result<(), StorageError>;

    /// Fetch the latest HPKE seed envelope for
    /// `(channel, subscriber)`. Returns `Ok(None)` when
    /// the subscriber is registered but no envelope has
    /// been delivered yet.
    async fn fetch_envelope(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
    ) -> Result<Option<Vec<u8>>, StorageError>;

    /// Total number of subscribers across every channel.
    async fn total_subscribers(&self) -> usize;

    /// Read the channel's current epoch id.
    async fn current_epoch(
        &self,
        channel: ChannelId,
    ) -> Result<u32, StorageError>;

    /// Atomic rotation: bump the current epoch + replace
    /// the per-subscriber envelopes for every entry in the
    /// batch. Either every step lands or none.
    async fn rotate(
        &self,
        channel: ChannelId,
        batch: Vec<(SubscriberId, Vec<u8>)>,
        expected_envelope_len: usize,
    ) -> Result<u32, StorageError>;
}

/// In-memory state container. Cheap to clone — the inner
/// state is behind an `Arc<RwLock<…>>`.
#[derive(Clone, Default)]
pub struct InMemoryChannelStore {
    inner: Arc<RwLock<Inner>>,
}

#[derive(Default)]
struct Inner {
    /// Channel id → ordered post list.
    posts: HashMap<ChannelId, Vec<StoredPost>>,
    /// Next cursor to hand out per channel (1-based; 0 is
    /// reserved as "before any post" in the fetch query).
    next_cursor: HashMap<ChannelId, u64>,
    /// Channel id → set of registered subscribers.
    /// `HashMap` rather than `HashSet` lets us iterate in
    /// insertion-stable order on the publisher's fan-out
    /// path; the value is the registration timestamp
    /// (unix-ms) to surface a "subscribed at" field if
    /// the future-self wants it.
    subscribers: HashMap<ChannelId, HashMap<SubscriberId, u64>>,
    /// (Channel, Subscriber) → latest HPKE-sealed seed
    /// envelope the publisher delivered. Latest-wins;
    /// the previous envelope is replaced on every
    /// publish, matching ADR-0010's "the most recent N
    /// seed envelopes per subscriber" retention model
    /// (N=1 in this MVP — multi-epoch retention is a
    /// follow-up).
    envelopes: HashMap<(ChannelId, SubscriberId), Vec<u8>>,
    /// Channel id → current epoch id. Server-side bookkeeping
    /// only; the authoritative epoch lives in each
    /// `ChannelPost`'s header on the client. Surfaced via
    /// `GET /v1/channels/:id/epoch` so a subscriber that
    /// comes back online after weeks can ask "did I miss a
    /// rotation?" before deciding to refresh its envelope.
    current_epoch: HashMap<ChannelId, u32>,
}

impl InMemoryChannelStore {
    /// Construct an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
        guard
            .next_cursor
            .insert(channel, cursor.saturating_add(1));
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
        self.inner
            .read()
            .await
            .posts
            .values()
            .map(Vec::len)
            .sum()
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
    async fn is_subscribed(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
    ) -> bool {
        self.inner
            .read()
            .await
            .subscribers
            .get(&channel)
            .is_some_and(|set| set.contains_key(&subscriber))
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
    /// a specific (channel, subscriber). The server never
    /// inspects `envelope` — it stores opaque bytes and
    /// hands them back unchanged on the matching GET.
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
    async fn deliver_envelope(
        &self,
        channel: ChannelId,
        subscriber: SubscriberId,
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
        guard.envelopes.insert((channel, subscriber), envelope);
        Ok(())
    }

    /// Fetch the most-recent HPKE seed envelope for the
    /// (channel, subscriber) pair. Returns `Ok(None)` if
    /// the subscriber is registered but no envelope has
    /// been delivered yet (e.g. the subscriber registered
    /// faster than the publisher's fan-out completed).
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
        Ok(guard.envelopes.get(&(channel, subscriber)).cloned())
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
    async fn current_epoch(
        &self,
        channel: ChannelId,
    ) -> Result<u32, StorageError> {
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
        // Validations passed — commit.
        for (sub, env) in batch {
            guard.envelopes.insert((channel, sub), env);
        }
        let next = guard
            .current_epoch
            .get(&channel)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        guard.current_epoch.insert(channel, next);
        Ok(next)
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
        let err = s
            .put(c, vec![0u8; 100], 50, 0)
            .await
            .unwrap_err();
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
        s.deliver_envelope(c, sub, envelope.clone(), 80)
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
            .deliver_envelope(c, sub, vec![0u8; 79], 80)
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
            .deliver_envelope(c, sid(0xA0), vec![0u8; 80], 80)
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
        s.deliver_envelope(c, sub, vec![0x11; 80], 80)
            .await
            .unwrap();
        s.deliver_envelope(c, sub, vec![0x22; 80], 80)
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
        s.deliver_envelope(c, a, vec![0x00; 80], 80)
            .await
            .unwrap();
        s.deliver_envelope(c, b, vec![0x00; 80], 80)
            .await
            .unwrap();
        // Rotate.
        let new_epoch = s
            .rotate(
                c,
                vec![(a, vec![0xAA; 80]), (b, vec![0xBB; 80])],
                80,
            )
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
        s.deliver_envelope(c, a, vec![0x00; 80], 80)
            .await
            .unwrap();
        // Second batch entry is wrong length — entire
        // rotation must abort without touching the
        // envelope or bumping the epoch.
        let err = s
            .rotate(c, vec![(a, vec![0u8; 79])], 80)
            .await
            .unwrap_err();
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
}
