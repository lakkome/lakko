// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! In-memory storage for channels + posts.
//!
//! Phase 4a backend deliberately starts in-memory: a
//! `HashMap<ChannelId, Vec<StoredPost>>` behind a
//! [`tokio::sync::RwLock`]. MariaDB persistence lands as a
//! separate sub-sprint once the wire-API surface is
//! confirmed against real client traffic — premature
//! schema design before client behaviour is observed has
//! cost the project once already.
//!
//! ## Opacity contract
//!
//! The server treats every post body as **opaque bytes**.
//! It never decodes the ChannelPost wire layout, never
//! verifies the Ed25519 signature, never derives the
//! AEAD key. Subscribers do all of those steps client-
//! side after fetching. This module's API surface
//! reinforces the contract — there's no method called
//! `decode_post`, `verify_signature`, or
//! `decrypt_payload` here, only `put` and `fetch`.

use std::collections::HashMap;
use std::sync::Arc;

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
}

/// In-memory state container. Cheap to clone — the inner
/// state is behind an `Arc<RwLock<…>>`.
#[derive(Clone, Default)]
pub struct ChannelStore {
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
}

impl ChannelStore {
    /// Construct an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a fresh channel. Idempotent — a second call
    /// with the same id is a no-op.
    pub async fn create(&self, channel: ChannelId) {
        let mut guard = self.inner.write().await;
        guard.posts.entry(channel).or_default();
        guard.next_cursor.entry(channel).or_insert(1);
    }

    /// `true` if the channel is registered.
    pub async fn exists(&self, channel: ChannelId) -> bool {
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
    pub async fn put(
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
    pub async fn fetch(
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
    pub async fn total_posts(&self) -> usize {
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
    pub async fn subscribe(
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
    pub async fn is_subscribed(
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
    pub async fn list_subscribers(
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
    pub async fn deliver_envelope(
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
    pub async fn fetch_envelope(
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
    pub async fn total_subscribers(&self) -> usize {
        self.inner
            .read()
            .await
            .subscribers
            .values()
            .map(HashMap::len)
            .sum()
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
        let s = ChannelStore::new();
        let c = cid(0xAA);
        s.create(c).await;
        s.create(c).await;
        assert!(s.exists(c).await);
    }

    #[tokio::test]
    async fn put_assigns_monotone_cursors() {
        let s = ChannelStore::new();
        let c = cid(0xBB);
        s.create(c).await;
        let a = s.put(c, vec![1, 2, 3], 1024, 1000).await.unwrap();
        let b = s.put(c, vec![4, 5], 1024, 2000).await.unwrap();
        assert_eq!(a.cursor, 1);
        assert_eq!(b.cursor, 2);
        assert!(b.server_ts_ms > a.server_ts_ms);
    }

    #[tokio::test]
    async fn put_rejects_unknown_channel() {
        let s = ChannelStore::new();
        let err = s.put(cid(0xCC), vec![1], 1024, 0).await.unwrap_err();
        assert!(matches!(err, StorageError::UnknownChannel));
    }

    #[tokio::test]
    async fn put_rejects_empty_payload() {
        let s = ChannelStore::new();
        let c = cid(0xDD);
        s.create(c).await;
        let err = s.put(c, vec![], 1024, 0).await.unwrap_err();
        assert!(matches!(err, StorageError::EmptyPost));
    }

    #[tokio::test]
    async fn put_rejects_oversize_payload() {
        let s = ChannelStore::new();
        let c = cid(0xEE);
        s.create(c).await;
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
        let s = ChannelStore::new();
        let c = cid(0xFF);
        s.create(c).await;
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
        let s = ChannelStore::new();
        let c = cid(0x11);
        s.create(c).await;
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
        let s = ChannelStore::new();
        let c = cid(0x10);
        let sub = sid(0x20);
        s.create(c).await;
        s.subscribe(c, sub, 1_700_000_000).await.unwrap();
        assert!(s.is_subscribed(c, sub).await);
        let list = s.list_subscribers(c).await.unwrap();
        assert_eq!(list, vec![sub]);
        assert_eq!(s.total_subscribers().await, 1);
    }

    #[tokio::test]
    async fn subscribe_rejects_unknown_channel() {
        let s = ChannelStore::new();
        let err = s.subscribe(cid(0x10), sid(0x20), 0).await.unwrap_err();
        assert!(matches!(err, StorageError::UnknownChannel));
    }

    #[tokio::test]
    async fn subscribe_is_idempotent() {
        let s = ChannelStore::new();
        let c = cid(0x30);
        let sub = sid(0x40);
        s.create(c).await;
        s.subscribe(c, sub, 1).await.unwrap();
        s.subscribe(c, sub, 2).await.unwrap();
        let list = s.list_subscribers(c).await.unwrap();
        assert_eq!(list.len(), 1);
    }

    #[tokio::test]
    async fn deliver_envelope_round_trip() {
        let s = ChannelStore::new();
        let c = cid(0x50);
        let sub = sid(0x60);
        s.create(c).await;
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
        let s = ChannelStore::new();
        let c = cid(0x70);
        let sub = sid(0x80);
        s.create(c).await;
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
        let s = ChannelStore::new();
        let c = cid(0x90);
        s.create(c).await;
        let err = s
            .deliver_envelope(c, sid(0xA0), vec![0u8; 80], 80)
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::UnknownSubscriber));
    }

    #[tokio::test]
    async fn fetch_envelope_returns_none_before_delivery() {
        let s = ChannelStore::new();
        let c = cid(0xB0);
        let sub = sid(0xC0);
        s.create(c).await;
        s.subscribe(c, sub, 0).await.unwrap();
        assert!(s.fetch_envelope(c, sub).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn envelope_latest_wins() {
        let s = ChannelStore::new();
        let c = cid(0xD0);
        let sub = sid(0xE0);
        s.create(c).await;
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
}
