// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Peer registry — tracks every connected client across calls.
//!
//! Each WebSocket connection that survives JWT verification
//! is registered here under the [`PeerId`] its admission token
//! carries: 16 random bytes the issuer draws per token
//! (ADR-0078 section 2). The registry holds no account id.
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

/// Opaque per-connection identifier: the random `peer_id` of the
/// connection's admission token (ADR-0078 section 2), 32 lower-case
/// hex characters. Every token draws a fresh one, so two devices of
/// one person are two peers and nothing ties a peer to an account.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PeerId(String);

impl PeerId {
    /// Construct from a known string — used in tests to pin
    /// deterministic ids.
    #[cfg(test)]
    #[must_use]
    pub fn from_str_for_test(s: &str) -> Self {
        Self(s.to_owned())
    }

    /// Construct from an arbitrary string — used by the
    /// router when matching a wire-supplied destination
    /// peer id back against the registry.
    #[must_use]
    pub fn from_string(s: String) -> Self {
        Self(s)
    }

    /// What a log line names this peer by (ADR-0078 section 11): six hex
    /// digits of a SipHash of the id under a key drawn at random once per
    /// process and never written anywhere. Lines of one connection still
    /// read together within a running SFU's log, but nothing at rest (no
    /// table, no other log, no wire capture) maps a tag back to the id, and
    /// a restart re-keys every tag. Collisions are possible (24 bits) and
    /// harmless: the tag is a reading aid, not a key.
    #[must_use]
    pub fn log_tag(&self) -> String {
        use std::collections::hash_map::RandomState;
        use std::hash::BuildHasher;
        use std::sync::OnceLock;
        static KEY: OnceLock<RandomState> = OnceLock::new();
        let hash = KEY.get_or_init(RandomState::new).hash_one(&self.0);
        format!("{:06x}", hash & 0x00FF_FFFF)
    }

    /// Borrow the raw string form. Used by the routing
    /// sprint when keying RTP fanout state by peer.
    #[must_use]
    #[allow(dead_code)]
    pub fn as_str(&self) -> &str {
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
pub struct Peer {
    /// Server-assigned id.
    pub id: PeerId,
    /// The room this peer is in, as its admission token names
    /// it (`room`, a one-way function of the call id).
    pub room: String,
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
pub struct PeerRegistry {
    inner: Arc<RwLock<HashMap<PeerId, Peer>>>,
}

impl PeerRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a freshly-admitted peer. Returns `false` if the
    /// id is already present (only possible in pathological
    /// PRNG collisions; we surface it so callers can
    /// regenerate the id).
    pub async fn register(&self, peer: Peer) -> bool {
        let mut guard = self.inner.write().await;
        if guard.contains_key(&peer.id) {
            return false;
        }
        guard.insert(peer.id.clone(), peer);
        true
    }

    /// Remove a peer by id. Returns the dropped record so the
    /// caller can fire a metrics counter.
    pub async fn unregister(&self, id: &PeerId) -> Option<Peer> {
        let mut guard = self.inner.write().await;
        guard.remove(id)
    }

    /// Total active peer count across every call. Read by
    /// the metrics layer once the routing sprint wires per-
    /// gauge updates.
    #[allow(dead_code)]
    pub async fn count(&self) -> usize {
        self.inner.read().await.len()
    }

    /// List the peers participating in a given call. Order is
    /// arbitrary; callers that need stable order must sort.
    /// Used by the routing sprint to broadcast a new track
    /// to every other peer in the call.
    #[allow(dead_code)]
    pub async fn peers_in_call(&self, room: &str) -> Vec<Peer> {
        let guard = self.inner.read().await;
        guard.values().filter(|p| p.room == room).cloned().collect()
    }

    /// Look up a single peer by id. Cloned out under the
    /// read lock so the caller can read `outbound` without
    /// holding the lock across an `await`.
    ///
    /// S-3-B-tracks step 2 — the media task uses this to
    /// look up the outbound mpsc of a peer it needs to
    /// ship a `ServerFrame::SdpOffer` to after a
    /// successful `MediaPeer::negotiate_if_needed`.
    pub async fn peer(&self, id: &PeerId) -> Option<Peer> {
        let guard = self.inner.read().await;
        guard.get(id).cloned()
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
            room: "call-a".into(),
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
            room: "c".into(),
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
            room: "call-a".into(),
            outbound: None,
        };
        let p2 = Peer {
            id: PeerId::from_str_for_test("p2"),
            room: "call-a".into(),
            outbound: None,
        };
        let p3 = Peer {
            id: PeerId::from_str_for_test("p3"),
            room: "call-b".into(),
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

    /// ADR-0078 section 2: the peer type carries no account. A
    /// peer is its id, its room and its outbound channel; this
    /// destructure fails to compile the day a field is added.
    #[test]
    fn the_peer_type_has_no_account_field() {
        let Peer { id, room, outbound } = Peer {
            id: PeerId::from_str_for_test("p"),
            room: "r".into(),
            outbound: None,
        };
        assert_eq!(id.as_str(), "p");
        assert_eq!(room, "r");
        assert!(outbound.is_none());
    }
}
