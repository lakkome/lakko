// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Peer registry — tracks every connected client across calls.
//!
//! Each WebSocket connection that survives JWT verification
//! is registered here under a fresh server-side [`PeerId`].
//! The registry is the join point between the signaling layer
//! (which observes WS frames) and — in S-3-B-routing — the
//! media-routing layer (which will hold `RTCPeerConnection`
//! handles per peer).
//!
//! ## Concurrency
//!
//! The map is wrapped in a `tokio::sync::RwLock`. Read paths
//! (per-call broadcasts) dominate; writes (peer join / leave)
//! are infrequent enough that the rwlock is cheaper than a
//! per-peer mutex pool.
//!
//! ## Lifecycle
//!
//! - `register` is called once per accepted connection.
//! - `unregister` is called from the WS task's drop / cleanup
//!   handler.
//! - `count` / `peer_ids_in_call` are read by the signaling
//!   and metrics layers.
//!
//! Per-peer state intentionally stays minimal here. The
//! routing sprint extends [`Peer`] with the RTP track set
//! and per-receiver layer selection.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, RwLock};

use crate::protocol::ServerFrame;

/// Opaque per-connection identifier, generated server-side.
///
/// Distinct from the JWT-carried `sub` (account_id) — multiple
/// peers from the same account (e.g. a desktop + a phone) get
/// distinct [`PeerId`]s so they appear as separate participants
/// in the call.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct PeerId(String);

impl PeerId {
    /// Generate a fresh peer id. The id is a monotone
    /// counter mixed with the low 64 bits of the system-
    /// clock nanos — long enough that the server-side log
    /// never sees a collision in practice and dependency-
    /// free (no `rand` needed for the SFU yet).
    #[must_use]
    pub(crate) fn fresh() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        // Casting the u128 nanos to u64 is intentional — we
        // only need uniqueness within the process lifetime,
        // not absolute time. The low 64 bits cycle every
        // ~584 years.
        #[allow(clippy::cast_possible_truncation)]
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map_or(0_u64, |d| d.as_nanos() as u64);
        Self(format!("p{n:016x}{nanos:016x}"))
    }

    /// Construct from a known string — used in tests to pin
    /// deterministic ids.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_str_for_test(s: &str) -> Self {
        Self(s.to_owned())
    }

    /// Construct from an arbitrary string — used by the
    /// router when matching a wire-supplied destination
    /// peer id back against the registry.
    #[must_use]
    pub(crate) fn from_string(s: String) -> Self {
        Self(s)
    }

    /// Borrow the raw string form. Used by the routing
    /// sprint when keying RTP fanout state by peer.
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PeerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Per-peer state held by the registry. Extended sprint-
/// by-sprint — `S-3-B-routing-media` adds RTP track sets,
/// `S-3-B-twcc` adds bandwidth counters, etc.
#[derive(Debug, Clone)]
pub(crate) struct Peer {
    /// Server-assigned id.
    pub id: PeerId,
    /// Call id this peer is part of, as carried by the
    /// admission JWT.
    pub call_id: String,
    /// Account id from the JWT `sub`. Stored for log
    /// pseudonymisation; never crosses into media frames.
    pub account_id: String,
    /// Outbound mpsc — frames the router pushes for the
    /// signaling task to write to the wire. `None` only in
    /// tests that bypass the registry. Holds a *clone* of
    /// the channel sender; the signaling task owns the
    /// receiver.
    pub outbound: Option<mpsc::Sender<ServerFrame>>,
}

/// Shared registry handle. Clone freely; the inner state is
/// behind an `Arc<RwLock<…>>`.
#[derive(Clone, Default)]
pub(crate) struct PeerRegistry {
    inner: Arc<RwLock<HashMap<PeerId, Peer>>>,
}

impl PeerRegistry {
    /// Create an empty registry.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Insert a freshly-admitted peer. Returns `false` if the
    /// id is already present (only possible in pathological
    /// PRNG collisions; we surface it so callers can
    /// regenerate the id).
    pub(crate) async fn register(&self, peer: Peer) -> bool {
        let mut guard = self.inner.write().await;
        if guard.contains_key(&peer.id) {
            return false;
        }
        guard.insert(peer.id.clone(), peer);
        true
    }

    /// Remove a peer by id. Returns the dropped record so the
    /// caller can fire a metrics counter.
    pub(crate) async fn unregister(&self, id: &PeerId) -> Option<Peer> {
        let mut guard = self.inner.write().await;
        guard.remove(id)
    }

    /// Total active peer count across every call. Read by
    /// the metrics layer once the routing sprint wires per-
    /// gauge updates.
    #[allow(dead_code)]
    pub(crate) async fn count(&self) -> usize {
        self.inner.read().await.len()
    }

    /// List the peers participating in a given call. Order is
    /// arbitrary; callers that need stable order must sort.
    /// Used by the routing sprint to broadcast a new track
    /// to every other peer in the call.
    #[allow(dead_code)]
    pub(crate) async fn peers_in_call(&self, call_id: &str) -> Vec<Peer> {
        let guard = self.inner.read().await;
        guard
            .values()
            .filter(|p| p.call_id == call_id)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_then_unregister() {
        let reg = PeerRegistry::new();
        let p = Peer {
            id: PeerId::from_str_for_test("p1"),
            call_id: "call-a".into(),
            account_id: "acct-a".into(),
            outbound: None,
        };
        assert!(reg.register(p.clone()).await);
        assert_eq!(reg.count().await, 1);
        let dropped = reg.unregister(&p.id).await;
        assert!(dropped.is_some());
        assert_eq!(reg.count().await, 0);
    }

    #[tokio::test]
    async fn duplicate_register_is_rejected() {
        let reg = PeerRegistry::new();
        let p = Peer {
            id: PeerId::from_str_for_test("dup"),
            call_id: "c".into(),
            account_id: "a".into(),
            outbound: None,
        };
        assert!(reg.register(p.clone()).await);
        assert!(!reg.register(p).await);
    }

    #[tokio::test]
    async fn peers_in_call_filters_correctly() {
        let reg = PeerRegistry::new();
        let p1 = Peer {
            id: PeerId::from_str_for_test("p1"),
            call_id: "call-a".into(),
            account_id: "a1".into(),
            outbound: None,
        };
        let p2 = Peer {
            id: PeerId::from_str_for_test("p2"),
            call_id: "call-a".into(),
            account_id: "a2".into(),
            outbound: None,
        };
        let p3 = Peer {
            id: PeerId::from_str_for_test("p3"),
            call_id: "call-b".into(),
            account_id: "a3".into(),
            outbound: None,
        };
        reg.register(p1).await;
        reg.register(p2).await;
        reg.register(p3).await;

        let in_a = reg.peers_in_call("call-a").await;
        assert_eq!(in_a.len(), 2);
        let in_b = reg.peers_in_call("call-b").await;
        assert_eq!(in_b.len(), 1);
    }

    #[test]
    fn peer_ids_are_unique_per_process() {
        let a = PeerId::fresh();
        let b = PeerId::fresh();
        assert_ne!(a, b);
    }
}
