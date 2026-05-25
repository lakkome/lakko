// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Per-call broadcast + per-peer relay.
//!
//! The router sits between the signaling layer (which owns
//! WebSocket frame I/O) and the peer registry (which owns
//! the call membership map). It is the **only** place that
//! decides "this frame goes to those peers" — every other
//! module either produces an inbound [`ClientFrame`] or
//! consumes an outbound [`ServerFrame`].
//!
//! ## Concurrency model
//!
//! Each peer's outbound side is an
//! `tokio::sync::mpsc::Sender<ServerFrame>` registered in
//! the [`crate::peer::PeerRegistry`] at admission time. The
//! signaling task's reader half forwards inbound frames to
//! [`Router::handle_client_frame`], which fans them out by
//! pushing onto the relevant outbound senders. The sender
//! side is the bounded channel — full queues surface as
//! [`RouterError::Backpressure`], at which point the
//! caller decides whether to drop the inbound frame or
//! disconnect the slow recipient.
//!
//! ## No payload inspection
//!
//! The router treats `payload_b64` as an opaque string and
//! never decodes it. This is the audit-pack invariant from
//! `docs/security/calls.md` §"What the SFU sees vs what the
//! client owns" — the SFU is a sealed envelope router.

use thiserror::Error;
use tracing::warn;

use crate::peer::{Peer, PeerId, PeerRegistry};
use crate::prom::names;
use crate::protocol::{ClientFrame, ErrorCode, PeerSnapshot, RelayTarget, ServerFrame};

/// Failure modes of a single relay attempt.
#[derive(Debug, Error)]
pub enum RouterError {
    /// Destination peer id was not in the same call (or has
    /// disconnected since the sender last saw the roster).
    #[error("unknown peer")]
    UnknownPeer,

    /// Recipient's outbound queue is full. The router did
    /// not deliver this frame; the caller decides whether
    /// to retry, drop, or close the slow recipient.
    #[error("recipient queue full")]
    Backpressure,
}

/// Map [`RouterError`] back to the wire [`ErrorCode`] the
/// SFU emits to the original sender.
impl RouterError {
    /// Translate the router-internal error code into the
    /// wire-level [`ErrorCode`] that the SFU emits to the
    /// peer that triggered the failed dispatch.
    pub fn to_wire_code(&self) -> ErrorCode {
        match self {
            Self::UnknownPeer => ErrorCode::UnknownPeer,
            Self::Backpressure => ErrorCode::BackpressureDrop,
        }
    }
}

/// Pseudonymise the sender's account id for inclusion in
/// `peer-joined` broadcasts. First 8 hex chars match the
/// audit-pack pseudonym shape used in log lines.
fn pseudonymise(account_id: &str) -> String {
    let n = account_id.len().min(8);
    account_id[..n].to_owned()
}

/// Router. Stateless aside from the [`PeerRegistry`] it
/// borrows; constructed per-request, not per-call.
pub struct Router<'a> {
    registry: &'a PeerRegistry,
}

impl<'a> Router<'a> {
    /// Borrow the registry for the duration of one routing
    /// decision.
    #[must_use]
    pub fn new(registry: &'a PeerRegistry) -> Self {
        Self { registry }
    }

    /// Build the welcome frame the SFU sends a peer right
    /// after admission. Reads the current call roster
    /// (excluding the just-admitted peer).
    pub async fn welcome_for(&self, peer: &Peer, server_ts: u64) -> ServerFrame {
        let roster = self
            .registry
            .peers_in_call(&peer.call_id)
            .await
            .into_iter()
            .filter(|p| p.id != peer.id)
            .map(|p| PeerSnapshot {
                peer_id: p.id.to_string(),
                account_pseudonym: pseudonymise(&p.account_id),
            })
            .collect();
        ServerFrame::Welcome {
            peer_id: peer.id.to_string(),
            call_id: peer.call_id.clone(),
            roster,
            server_ts,
        }
    }

    /// Broadcast a `peer-joined` to every other peer in the
    /// same call. Backpressure on individual recipients is
    /// logged and counted but never propagated — one slow
    /// peer must not stall the rest of the fanout.
    pub async fn announce_join(&self, peer: &Peer) {
        let frame = ServerFrame::PeerJoined {
            peer_id: peer.id.to_string(),
            account_pseudonym: pseudonymise(&peer.account_id),
        };
        self.broadcast_to_call(&peer.call_id, Some(&peer.id), frame)
            .await;
    }

    /// Broadcast a `peer-left` to every remaining peer in
    /// the same call.
    pub async fn announce_leave(&self, peer: &Peer) {
        let frame = ServerFrame::PeerLeft {
            peer_id: peer.id.to_string(),
        };
        self.broadcast_to_call(&peer.call_id, Some(&peer.id), frame)
            .await;
    }

    /// Dispatch a client-sent frame. The router translates
    /// it into one or more [`ServerFrame`]s and pushes them
    /// onto the relevant outbound senders.
    ///
    /// Returns either:
    ///
    /// - `Ok(None)` — the frame was an internal handshake
    ///   (currently only [`ClientFrame::Ping`]) and the
    ///   caller should send back a [`ServerFrame::Pong`].
    /// - `Ok(Some(pong))` — same as above but with the
    ///   prebuilt pong frame.
    /// - `Err(RouterError::UnknownPeer)` — destination
    ///   absent.
    /// - `Err(RouterError::Backpressure)` — the destination
    ///   queue was full.
    pub async fn handle_client_frame(
        &self,
        sender: &Peer,
        frame: ClientFrame,
    ) -> Result<Option<ServerFrame>, RouterError> {
        match frame {
            ClientFrame::Ping { nonce } => Ok(Some(ServerFrame::Pong { nonce })),
            ClientFrame::SignalRelay { to, payload_b64 } => {
                let relay = ServerFrame::SignalRelay {
                    from: sender.id.to_string(),
                    payload_b64,
                };
                match to {
                    RelayTarget::Peer { peer_id } => {
                        let target = PeerId::from_string(peer_id);
                        let peers = self.registry.peers_in_call(&sender.call_id).await;
                        let Some(dst) = peers.into_iter().find(|p| p.id == target) else {
                            return Err(RouterError::UnknownPeer);
                        };
                        deliver(&dst, relay)?;
                        Ok(None)
                    },
                    RelayTarget::Call => {
                        self.broadcast_to_call(&sender.call_id, Some(&sender.id), relay)
                            .await;
                        Ok(None)
                    },
                }
            },
        }
    }

    /// Send `frame` to every peer in `call_id`, excluding
    /// `except` (typically the originator). Per-recipient
    /// backpressure is logged + counted, never propagated.
    async fn broadcast_to_call(
        &self,
        call_id: &str,
        except: Option<&PeerId>,
        frame: ServerFrame,
    ) {
        let recipients = self.registry.peers_in_call(call_id).await;
        let mut delivered = 0usize;
        let mut dropped = 0usize;
        for p in recipients {
            if Some(&p.id) == except {
                continue;
            }
            match deliver(&p, frame.clone()) {
                Ok(()) => delivered += 1,
                Err(RouterError::Backpressure) => {
                    dropped += 1;
                    warn!(peer = %p.id, "router: broadcast drop due to backpressure");
                },
                Err(RouterError::UnknownPeer) => {
                    // Peer dropped between the registry read
                    // and the send — race, not a bug.
                },
            }
        }
        if delivered > 0 {
            metrics::counter!(names::SIGNALING_FRAMES_TOTAL).increment(delivered as u64);
        }
        if dropped > 0 {
            warn!(
                call = %call_id,
                dropped,
                delivered,
                "router: partial broadcast — some peers behind on their queue"
            );
        }
    }
}

/// Push one frame onto one peer's outbound channel. Maps
/// the mpsc send result back into the router error space.
fn deliver(peer: &Peer, frame: ServerFrame) -> Result<(), RouterError> {
    match &peer.outbound {
        None => Err(RouterError::UnknownPeer),
        Some(tx) => match tx.try_send(frame) {
            Ok(()) => Ok(()),
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                Err(RouterError::Backpressure)
            },
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                Err(RouterError::UnknownPeer)
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    /// Construct a fully-registered peer with an outbound
    /// channel hooked up. Returns both the peer record (for
    /// the registry) and the receiver end (for assertions).
    fn make_peer(
        id: &str,
        call_id: &str,
        account_id: &str,
    ) -> (Peer, mpsc::Receiver<ServerFrame>) {
        let (tx, rx) = mpsc::channel::<ServerFrame>(16);
        let peer = Peer {
            id: PeerId::from_str_for_test(id),
            call_id: call_id.into(),
            account_id: account_id.into(),
            outbound: Some(tx),
        };
        (peer, rx)
    }

    #[tokio::test]
    async fn welcome_excludes_self_and_pseudonymises() {
        let reg = PeerRegistry::new();
        let (alice, _alice_rx) = make_peer("p1", "call-a", "deadbeefdeadbeefdeadbeefdeadbeef");
        let (bob, _bob_rx) = make_peer("p2", "call-a", "cafebabecafebabecafebabecafebabe");
        reg.register(alice.clone()).await;
        reg.register(bob.clone()).await;

        let router = Router::new(&reg);
        let frame = router.welcome_for(&alice, 1_700_000_000).await;
        match frame {
            ServerFrame::Welcome {
                peer_id,
                call_id,
                roster,
                server_ts,
            } => {
                assert_eq!(peer_id, "p1");
                assert_eq!(call_id, "call-a");
                assert_eq!(server_ts, 1_700_000_000);
                assert_eq!(roster.len(), 1);
                assert_eq!(roster[0].peer_id, "p2");
                assert_eq!(roster[0].account_pseudonym, "cafebabe");
            },
            other => panic!("expected Welcome, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn join_broadcast_reaches_existing_peers_only() {
        let reg = PeerRegistry::new();
        let (alice, mut alice_rx) = make_peer("p1", "call-a", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let (bob, mut bob_rx) = make_peer("p2", "call-a", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        reg.register(alice.clone()).await;
        reg.register(bob.clone()).await;

        let router = Router::new(&reg);
        router.announce_join(&bob).await;
        // Alice should see bob-joined; bob should not see
        // his own join announcement.
        match alice_rx.try_recv().unwrap() {
            ServerFrame::PeerJoined {
                peer_id,
                account_pseudonym,
            } => {
                assert_eq!(peer_id, "p2");
                assert_eq!(account_pseudonym, "bbbbbbbb");
            },
            other => panic!("expected PeerJoined, got {other:?}"),
        }
        assert!(bob_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn leave_broadcast_reaches_remaining_peers() {
        let reg = PeerRegistry::new();
        let (alice, mut alice_rx) = make_peer("p1", "call-a", "aa");
        let (bob, _bob_rx) = make_peer("p2", "call-a", "bb");
        reg.register(alice.clone()).await;
        reg.register(bob.clone()).await;

        let router = Router::new(&reg);
        router.announce_leave(&bob).await;
        match alice_rx.try_recv().unwrap() {
            ServerFrame::PeerLeft { peer_id } => assert_eq!(peer_id, "p2"),
            other => panic!("expected PeerLeft, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn signal_relay_targets_named_peer() {
        let reg = PeerRegistry::new();
        let (alice, _alice_rx) = make_peer("p1", "call-a", "aa");
        let (bob, mut bob_rx) = make_peer("p2", "call-a", "bb");
        let (carol, mut carol_rx) = make_peer("p3", "call-a", "cc");
        reg.register(alice.clone()).await;
        reg.register(bob.clone()).await;
        reg.register(carol.clone()).await;

        let router = Router::new(&reg);
        let out = router
            .handle_client_frame(
                &alice,
                ClientFrame::SignalRelay {
                    to: RelayTarget::Peer {
                        peer_id: "p2".into(),
                    },
                    payload_b64: "AAAA".into(),
                },
            )
            .await
            .unwrap();
        assert!(out.is_none()); // no pong expected
        match bob_rx.try_recv().unwrap() {
            ServerFrame::SignalRelay { from, payload_b64 } => {
                assert_eq!(from, "p1");
                assert_eq!(payload_b64, "AAAA");
            },
            other => panic!("expected SignalRelay, got {other:?}"),
        }
        // Carol should not have received the unicast.
        assert!(carol_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn signal_relay_broadcast_skips_sender() {
        let reg = PeerRegistry::new();
        let (alice, mut alice_rx) = make_peer("p1", "call-a", "aa");
        let (bob, mut bob_rx) = make_peer("p2", "call-a", "bb");
        let (carol, mut carol_rx) = make_peer("p3", "call-a", "cc");
        reg.register(alice.clone()).await;
        reg.register(bob.clone()).await;
        reg.register(carol.clone()).await;

        let router = Router::new(&reg);
        router
            .handle_client_frame(
                &alice,
                ClientFrame::SignalRelay {
                    to: RelayTarget::Call,
                    payload_b64: "ZZZ".into(),
                },
            )
            .await
            .unwrap();
        // Bob + Carol receive; Alice (sender) does not.
        assert!(matches!(bob_rx.try_recv().unwrap(), ServerFrame::SignalRelay { .. }));
        assert!(matches!(carol_rx.try_recv().unwrap(), ServerFrame::SignalRelay { .. }));
        assert!(alice_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn signal_relay_to_unknown_peer_errors() {
        let reg = PeerRegistry::new();
        let (alice, _alice_rx) = make_peer("p1", "call-a", "aa");
        reg.register(alice.clone()).await;

        let router = Router::new(&reg);
        let err = router
            .handle_client_frame(
                &alice,
                ClientFrame::SignalRelay {
                    to: RelayTarget::Peer {
                        peer_id: "p-nonexistent".into(),
                    },
                    payload_b64: "AAAA".into(),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, RouterError::UnknownPeer));
    }

    #[tokio::test]
    async fn ping_yields_matching_pong() {
        let reg = PeerRegistry::new();
        let (alice, _alice_rx) = make_peer("p1", "call-a", "aa");
        reg.register(alice.clone()).await;

        let router = Router::new(&reg);
        let out = router
            .handle_client_frame(&alice, ClientFrame::Ping { nonce: 42 })
            .await
            .unwrap();
        match out.unwrap() {
            ServerFrame::Pong { nonce } => assert_eq!(nonce, 42),
            other => panic!("expected Pong, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cross_call_isolation() {
        let reg = PeerRegistry::new();
        let (alice, mut alice_rx) = make_peer("p1", "call-a", "aa");
        let (bob, mut bob_rx) = make_peer("p2", "call-a", "bb");
        let (eve, mut eve_rx) = make_peer("p9", "call-b", "ee");
        reg.register(alice.clone()).await;
        reg.register(bob.clone()).await;
        reg.register(eve.clone()).await;

        let router = Router::new(&reg);
        router.announce_join(&bob).await;
        // Alice (same call) sees the join; Eve (other call)
        // must not.
        assert!(matches!(alice_rx.try_recv().unwrap(), ServerFrame::PeerJoined { .. }));
        assert!(eve_rx.try_recv().is_err());
        // Sanity — bob still does not see his own join.
        assert!(bob_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn pseudonymise_truncates() {
        assert_eq!(pseudonymise("deadbeefxxxx"), "deadbeef");
        assert_eq!(pseudonymise("abc"), "abc");
        assert_eq!(pseudonymise(""), "");
    }
}
