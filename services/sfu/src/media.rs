// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Media-routing layer — `str0m`-backed RTP fanout for
//! Lakko's group calls (S-3-B-routing-media, step 1).
//!
//! ## Where this sits
//!
//! The SFU has two halves:
//!
//! - **Signaling** (already shipped — see
//!   [`crate::router`] + [`crate::signaling`]): a
//!   per-call WebSocket fabric that shuttles SDP /
//!   ICE / SFrame-key envelopes between peers without
//!   inspecting them. The wire is
//!   [`crate::protocol::ClientFrame`] /
//!   [`crate::protocol::ServerFrame`].
//! - **Media routing** (this module — landing
//!   incrementally): a `str0m::Rtc` instance per peer
//!   that terminates the peer's UDP+ICE+DTLS+SRTP
//!   path, then forwards encrypted RTP packets to the
//!   other peers in the same call.
//!
//! ## SFrame contract
//!
//! The audit-pack invariant from
//! [`docs/security/calls.md`](../../../docs/security/calls.md)
//! says the SFU is a sealed envelope router. With
//! SFrame (RFC 9605) the RTP payload is
//! end-to-end-encrypted under the MLS-derived call
//! key; the SFU sees ciphertext only. `str0m` does the
//! SRTP transport layer (per-peer hop), but the inner
//! SFrame ciphertext stays opaque from origin to
//! destination — the SFU forwards bytes without
//! decryption.
//!
//! ## What has shipped so far
//!
//! Step 1 — Foundation:
//!
//! - `str0m` dep pinned at `=0.19.0` with
//!   `default-features = false, features = ["rust-crypto"]`
//!   in `Cargo.toml` (see `UPSTREAM.md`).
//! - [`MediaPeer`] — wraps one `str0m::Rtc` instance
//!   per admitted peer, plus a stable [`MediaPeerId`]
//!   that matches [`crate::peer::PeerId`].
//! - [`MediaRouter`] — owns the per-call peer map.
//!   Stateless across calls; the registry-of-registries
//!   lives in `crate::peer::PeerRegistry` (the existing
//!   signaling-layer source of truth).
//! - Sync `tick` API: the caller (a future tokio task,
//!   not part of this step) feeds inbound UDP packets
//!   via [`MediaPeer::handle_input`] and drains
//!   `str0m::Rtc::poll_output` until it returns
//!   `Output::Timeout(..)`. `str0m`'s sync event loop
//!   is the property that made it the chosen stack
//!   (see [`SPIKE.md`](../SPIKE.md)).
//!
//! Step 2 — SDP/ICE wire frames + MediaPeer signaling
//! API:
//!
//! - New wire frames in [`crate::protocol`]:
//!   `ClientFrame::SdpOffer` + `ClientFrame::IceCandidate`
//!   (client → SFU), `ServerFrame::SdpAnswer` (SFU →
//!   client). New `ErrorCode::BadSdp` /
//!   `ErrorCode::BadIceCandidate` round-trip via kebab.
//! - [`MediaPeer::accept_offer`] wraps
//!   `str0m::Rtc::sdp_api().accept_offer(..)` with
//!   typed [`MediaError::BadSdp`] /
//!   [`MediaError::Str0m`] error mapping; returns the
//!   answer SDP string ready to ship as
//!   `ServerFrame::SdpAnswer`.
//! - [`MediaPeer::add_remote_candidate`] wraps
//!   `str0m::Candidate::from_sdp_string` +
//!   `str0m::Rtc::add_remote_candidate` with typed
//!   [`MediaError::BadIceCandidate`].
//! - Router stub: when `ClientFrame::SdpOffer` /
//!   `IceCandidate` arrives, [`crate::router::Router`]
//!   surfaces a typed
//!   [`crate::router::RouterError::MediaNotWired`] (maps
//!   to `ErrorCode::BadSdp` on the wire). The
//!   router-to-MediaPeer integration lands in step 3
//!   alongside the UDP socket loop.
//!
//! ## What does NOT land yet
//!
//! Steps 3-5 of S-3-B-routing-media:
//!
//! - Step 3 — open the SFU's UDP socket + spawn the
//!   tokio task that polls the registry, writes to the
//!   socket, and dispatches `ClientFrame::SdpOffer` /
//!   `IceCandidate` into the right `MediaPeer`.
//! - Step 4 — translate `str0m::Event::MediaData`
//!   into outgoing writes on every other peer's `Rtc`
//!   (RTP fanout).
//! - Step 5 — integration tests with synthetic
//!   `str0m::Rtc` clients in-process.
//!
//! Each step keeps the audit-pack invariant
//! ("payload-opaque router") intact.

use std::time::Instant;

use str0m::change::{SdpAnswer, SdpOffer};
use str0m::{Candidate, Output, Rtc};
use thiserror::Error;

use crate::peer::PeerId;

/// Stable identifier for a media peer. One-to-one with
/// [`crate::peer::PeerId`] — the signaling-layer
/// admission step assigns the id, and the media layer
/// reuses it so the two halves agree on identity.
pub type MediaPeerId = PeerId;

/// Failure modes the media layer surfaces. Variants are
/// stable; callers branch on the variant, never on the
/// message string.
#[derive(Debug, Error)]
pub enum MediaError {
    /// `str0m` itself surfaced an error while consuming
    /// an input or polling for output. The wrapped
    /// message is `str0m::RtcError`'s `Display`; we keep
    /// it opaque rather than re-exporting `RtcError`
    /// because rustc's typestate guarantees would force
    /// every caller to depend on the str0m version we
    /// happen to pin.
    #[error("str0m error: {0}")]
    Str0m(String),
    /// SDP-offer string failed to parse via
    /// `str0m::SdpOffer::from_sdp_string`. Maps to the
    /// wire-level [`crate::protocol::ErrorCode::BadSdp`].
    #[error("SDP parse failed: {0}")]
    BadSdp(String),
    /// ICE-candidate string failed to parse via
    /// `str0m::Candidate::from_sdp_string`. Maps to the
    /// wire-level [`crate::protocol::ErrorCode::BadIceCandidate`].
    #[error("ICE candidate parse failed: {0}")]
    BadIceCandidate(String),
}

/// Result alias.
pub type MediaResult<T> = std::result::Result<T, MediaError>;

/// One media-routing peer.
///
/// Wraps a single `str0m::Rtc` instance plus the
/// signaling-layer [`MediaPeerId`] that names the peer.
/// The `Rtc` owns the SRTP/DTLS/ICE state for one
/// client; outbound RTP for other peers is produced by
/// *their* `MediaPeer` instances, not by this one.
pub struct MediaPeer {
    id: MediaPeerId,
    rtc: Rtc,
}

impl MediaPeer {
    /// Construct a fresh media peer. The internal
    /// `str0m::Rtc` is initialised at the current
    /// `Instant`; the caller is expected to immediately
    /// hand it through the SDP offer-answer dance via a
    /// later step's API.
    ///
    /// `id` is the signaling-layer peer id — same value
    /// the websocket admission path stored in
    /// `crate::peer::PeerRegistry`.
    #[must_use]
    pub fn new(id: MediaPeerId, now: Instant) -> Self {
        Self {
            id,
            rtc: Rtc::new(now),
        }
    }

    /// Stable id for this peer.
    #[must_use]
    pub fn id(&self) -> &MediaPeerId {
        &self.id
    }

    /// Poll the underlying `str0m::Rtc` for its next
    /// action. Drives the sync event loop one step.
    ///
    /// The caller pumps this in a loop until the result
    /// is `Output::Timeout(..)`, at which point the
    /// caller should suspend on either the timeout or
    /// the next inbound socket read.
    ///
    /// # Errors
    ///
    /// Surfaces [`MediaError::Str0m`] on a state-machine
    /// failure inside `str0m`.
    pub fn poll_output(&mut self) -> MediaResult<Output> {
        self.rtc
            .poll_output()
            .map_err(|e| MediaError::Str0m(e.to_string()))
    }

    /// `true` if the underlying `str0m::Rtc` is still
    /// alive. `str0m::Rtc::is_alive` flips to `false` on
    /// DTLS / ICE failure or after explicit `disconnect`.
    #[must_use]
    pub fn is_alive(&self) -> bool {
        self.rtc.is_alive()
    }

    /// Consume a client's SDP offer and produce the
    /// SFU-side SDP answer.
    ///
    /// `offer_sdp` is the plain-text offer the client
    /// sent over the signaling channel (carried as
    /// [`crate::protocol::ClientFrame::SdpOffer`]). On
    /// success the returned `String` is the answer the
    /// SFU should ship back via
    /// [`crate::protocol::ServerFrame::SdpAnswer`]; the
    /// SFU's local ICE candidates are baked in inline as
    /// `a=candidate:` lines (str0m does not emit them as
    /// separate events).
    ///
    /// # Errors
    ///
    /// - [`MediaError::BadSdp`] when the offer string
    ///   fails `str0m::SdpOffer::from_sdp_string`.
    /// - [`MediaError::Str0m`] when str0m's
    ///   `SdpApi::accept_offer` refuses the offer (e.g.
    ///   inconsistent bundle, codec mismatch).
    pub fn accept_offer(&mut self, offer_sdp: &str) -> MediaResult<String> {
        let offer =
            SdpOffer::from_sdp_string(offer_sdp).map_err(|e| MediaError::BadSdp(e.to_string()))?;
        let answer: SdpAnswer = self
            .rtc
            .sdp_api()
            .accept_offer(offer)
            .map_err(|e| MediaError::Str0m(e.to_string()))?;
        Ok(answer.to_sdp_string())
    }

    /// Hand a trickle-ICE candidate (the value of the
    /// SDP `a=candidate:…` attribute) to the underlying
    /// `str0m::Rtc`.
    ///
    /// `candidate_sdp` is the form
    /// `RTCIceCandidate.candidate` returns on the
    /// browser side — i.e. the full `candidate:…` line
    /// minus the leading `a=`. `str0m::Candidate::from_sdp_string`
    /// parses both with and without the `candidate:`
    /// prefix.
    ///
    /// # Errors
    ///
    /// [`MediaError::BadIceCandidate`] when the string
    /// fails `Candidate::from_sdp_string`.
    pub fn add_remote_candidate(&mut self, candidate_sdp: &str) -> MediaResult<()> {
        let candidate = Candidate::from_sdp_string(candidate_sdp)
            .map_err(|e| MediaError::BadIceCandidate(e.to_string()))?;
        self.rtc.add_remote_candidate(candidate);
        Ok(())
    }
}

impl std::fmt::Debug for MediaPeer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `str0m::Rtc` does not implement Debug, so we
        // print just the id + liveness.
        f.debug_struct("MediaPeer")
            .field("id", &self.id)
            .field("alive", &self.rtc.is_alive())
            .finish()
    }
}

/// Per-call media router — owns the map of admitted
/// peers. Cheap to construct, intentionally stateless
/// across calls; the SFU's process-wide registry
/// (see [`crate::peer::PeerRegistry`]) decides what
/// goes into each `MediaRouter`.
///
/// Step 1 ships only the construction surface; the
/// per-tick fanout logic lands in step 4 once the
/// signaling-driven SDP exchange (step 2) and the UDP
/// socket loop (step 3) are in place.
#[derive(Debug, Default)]
pub struct MediaRouter {
    peers: Vec<MediaPeer>,
}

impl MediaRouter {
    /// Construct an empty router. No call is bound at
    /// this point — the admission path adds peers via
    /// [`Self::add_peer`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a freshly-admitted peer to the router. The
    /// caller MUST have already validated admission
    /// (call JWT, call_id membership) via the existing
    /// `crate::router::Router` path — `MediaRouter`
    /// trusts the input.
    pub fn add_peer(&mut self, peer: MediaPeer) {
        self.peers.push(peer);
    }

    /// Remove a peer by id. Returns `true` if a peer
    /// was removed. Idempotent — repeated removals of
    /// the same id return `false`.
    pub fn remove_peer(&mut self, id: &MediaPeerId) -> bool {
        let before = self.peers.len();
        self.peers.retain(|p| p.id() != id);
        self.peers.len() != before
    }

    /// Borrow the peers in admission order.
    #[must_use]
    pub fn peers(&self) -> &[MediaPeer] {
        &self.peers
    }

    /// Mutably borrow the peers — needed by the
    /// per-tick caller to drive `poll_output` on each.
    pub fn peers_mut(&mut self) -> &mut [MediaPeer] {
        &mut self.peers
    }

    /// Number of admitted peers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    /// `true` if no peers are admitted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Mutable borrow of the peer with the given id, if
    /// present. Returns `None` if no peer matches.
    pub fn peer_mut(&mut self, id: &MediaPeerId) -> Option<&mut MediaPeer> {
        self.peers.iter_mut().find(|p| p.id() == id)
    }

    /// Get the peer with the given id, or insert a fresh
    /// one (constructed at `now`) if it does not exist.
    /// Returns a mutable borrow of the (possibly new)
    /// peer.
    ///
    /// Used by the media task to lazily promote a
    /// signaling-admitted peer into the media layer on
    /// the first SDP-offer frame from that peer — there
    /// is no separate `MediaCommand::AddPeer` op because
    /// the offer itself signals that the peer wants
    /// media.
    pub fn peer_or_insert(&mut self, id: MediaPeerId, now: Instant) -> &mut MediaPeer {
        // Avoid the borrow-checker NLL gotcha by indexing
        // after a non-borrowed lookup.
        let existing_idx = self.peers.iter().position(|p| p.id() == &id);
        if let Some(idx) = existing_idx {
            &mut self.peers[idx]
        } else {
            self.peers.push(MediaPeer::new(id, now));
            self.peers
                .last_mut()
                .expect("just-pushed peer is always present")
        }
    }
}

// ============================================================
// Async/sync boundary — MediaCommand + spawn_media_task
// ============================================================

/// Commands the async signaling layer sends to the
/// sync-style media event loop.
///
/// Each variant carries a `oneshot::Sender` that the
/// task uses to send the result back. Dropping the
/// receiver before the task replies is harmless — the
/// task's `send` becomes a no-op and the originating
/// future just gets a `RecvError`, which the router maps
/// to a typed [`MediaCommandError::TaskGone`].
pub enum MediaCommand {
    /// Hand a client's SDP offer to the corresponding
    /// `MediaPeer` (created lazily if not yet present),
    /// reply with either the answer SDP string or a
    /// typed error.
    AcceptOffer {
        /// Originating peer id.
        peer_id: MediaPeerId,
        /// Client-supplied SDP offer text.
        offer_sdp: String,
        /// One-shot reply channel.
        reply: tokio::sync::oneshot::Sender<MediaResult<String>>,
    },
    /// Hand a trickle ICE candidate to the corresponding
    /// `MediaPeer`. No payload reply; the task either
    /// signals success or surfaces a typed error.
    AddRemoteCandidate {
        /// Originating peer id.
        peer_id: MediaPeerId,
        /// SDP-form candidate string.
        candidate_sdp: String,
        /// One-shot reply channel.
        reply: tokio::sync::oneshot::Sender<MediaResult<()>>,
    },
}

/// Bounded capacity for the media-command channel. A
/// stuck media task would surface as send-side
/// `try_send` errors at the router rather than
/// unbounded memory growth.
const MEDIA_COMMAND_CAPACITY: usize = 1024;

/// Handle the signaling layer uses to talk to the media
/// task. Cheap to clone — the underlying channel is
/// `Arc`-backed inside `tokio::sync::mpsc`.
#[derive(Clone)]
pub struct MediaTaskHandle {
    tx: tokio::sync::mpsc::Sender<MediaCommand>,
}

impl std::fmt::Debug for MediaTaskHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaTaskHandle")
            .field("capacity", &MEDIA_COMMAND_CAPACITY)
            .finish()
    }
}

/// Failure modes the router surfaces when talking to
/// the media task. Each variant maps to a wire-level
/// `ErrorCode`.
#[derive(Debug, Error)]
pub enum MediaCommandError {
    /// The media task is gone (either the task panicked
    /// or the handle's recipient was dropped). The
    /// service should be restarted; recoverable
    /// per-request handling is not possible because the
    /// underlying str0m state machine cannot resume
    /// from outside the task.
    #[error("media task is no longer running")]
    TaskGone,
    /// The media-command channel is full — the task is
    /// behind the signaling rate. The router drops the
    /// inbound frame rather than block the WS reader.
    #[error("media task command queue full")]
    Backpressure,
    /// The media task processed the command but returned
    /// a typed media-layer error (e.g. `BadSdp`). The
    /// wrapped error is the one the [`MediaPeer`] method
    /// surfaced.
    #[error("media operation failed: {0}")]
    Media(#[source] MediaError),
}

impl MediaTaskHandle {
    /// Send an [`AcceptOffer`](MediaCommand::AcceptOffer)
    /// command and await the answer.
    ///
    /// # Errors
    ///
    /// See [`MediaCommandError`]. Notably:
    /// - [`MediaCommandError::Backpressure`] when the
    ///   command channel is full (1024 entries).
    /// - [`MediaCommandError::TaskGone`] when the task
    ///   is no longer running.
    /// - [`MediaCommandError::Media`] wrapping the
    ///   per-peer media-layer error.
    pub async fn accept_offer(
        &self,
        peer_id: MediaPeerId,
        offer_sdp: String,
    ) -> std::result::Result<String, MediaCommandError> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.tx
            .try_send(MediaCommand::AcceptOffer {
                peer_id,
                offer_sdp,
                reply: reply_tx,
            })
            .map_err(|e| match e {
                tokio::sync::mpsc::error::TrySendError::Full(_) => MediaCommandError::Backpressure,
                tokio::sync::mpsc::error::TrySendError::Closed(_) => MediaCommandError::TaskGone,
            })?;
        let result = reply_rx.await.map_err(|_| MediaCommandError::TaskGone)?;
        result.map_err(MediaCommandError::Media)
    }

    /// Send an [`AddRemoteCandidate`](MediaCommand::AddRemoteCandidate)
    /// command.
    ///
    /// # Errors
    ///
    /// Symmetric to [`Self::accept_offer`] — same
    /// variants for transport / task / media errors.
    pub async fn add_remote_candidate(
        &self,
        peer_id: MediaPeerId,
        candidate_sdp: String,
    ) -> std::result::Result<(), MediaCommandError> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.tx
            .try_send(MediaCommand::AddRemoteCandidate {
                peer_id,
                candidate_sdp,
                reply: reply_tx,
            })
            .map_err(|e| match e {
                tokio::sync::mpsc::error::TrySendError::Full(_) => MediaCommandError::Backpressure,
                tokio::sync::mpsc::error::TrySendError::Closed(_) => MediaCommandError::TaskGone,
            })?;
        let result = reply_rx.await.map_err(|_| MediaCommandError::TaskGone)?;
        result.map_err(MediaCommandError::Media)
    }
}

/// Spawn the media event-loop task. Returns the handle
/// signaling code uses to send commands.
///
/// The task is a single tokio future that owns the
/// [`MediaRouter`] and processes [`MediaCommand`]s one
/// at a time. Step 3 ships only the command channel; the
/// UDP socket + receive loop + `MediaData` fanout slot
/// into the same `select!` body in step 4.
#[must_use]
pub fn spawn_media_task() -> MediaTaskHandle {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<MediaCommand>(MEDIA_COMMAND_CAPACITY);
    tokio::spawn(async move {
        let mut router = MediaRouter::new();
        while let Some(cmd) = rx.recv().await {
            match cmd {
                MediaCommand::AcceptOffer {
                    peer_id,
                    offer_sdp,
                    reply,
                } => {
                    let peer = router.peer_or_insert(peer_id, Instant::now());
                    let res = peer.accept_offer(&offer_sdp);
                    // If the receiver dropped, the
                    // originating future has bailed —
                    // ignore the send failure.
                    let _ = reply.send(res);
                },
                MediaCommand::AddRemoteCandidate {
                    peer_id,
                    candidate_sdp,
                    reply,
                } => {
                    let peer = router.peer_or_insert(peer_id, Instant::now());
                    let res = peer.add_remote_candidate(&candidate_sdp);
                    let _ = reply.send(res);
                },
            }
        }
    });
    MediaTaskHandle { tx }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn pid(name: &str) -> MediaPeerId {
        // `PeerId` is defined in crate::peer; we go
        // through its test-only constructor so the
        // media tests cannot drift from the real shape.
        crate::peer::PeerId::from_str_for_test(name)
    }

    #[test]
    fn media_peer_constructs_and_reports_alive() {
        let p = MediaPeer::new(pid("alice"), Instant::now());
        assert!(p.is_alive(), "fresh str0m::Rtc must be alive");
        assert_eq!(p.id(), &pid("alice"));
    }

    #[test]
    fn media_peer_first_poll_returns_a_meaningful_output() {
        // str0m's event loop returns a Timeout on the
        // first poll of a freshly-constructed Rtc — we
        // do not yet have any pending input.
        let mut p = MediaPeer::new(pid("alice"), Instant::now());
        let out = p.poll_output().expect("first poll succeeds");
        // We accept any well-formed variant; the
        // specific timing is str0m's choice. Important:
        // it did not error and it did not panic.
        match out {
            Output::Timeout(_) | Output::Transmit(_) | Output::Event(_) => {},
        }
    }

    #[test]
    fn router_add_and_remove_round_trip() {
        let mut r = MediaRouter::new();
        assert!(r.is_empty());

        r.add_peer(MediaPeer::new(pid("alice"), Instant::now()));
        r.add_peer(MediaPeer::new(
            pid("bob"),
            Instant::now() + Duration::from_millis(1),
        ));
        assert_eq!(r.len(), 2);

        // Idempotent on missing id.
        assert!(!r.remove_peer(&pid("charlie")));
        assert_eq!(r.len(), 2);

        // Removes the targeted id only.
        assert!(r.remove_peer(&pid("alice")));
        assert_eq!(r.len(), 1);
        assert_eq!(r.peers()[0].id(), &pid("bob"));

        // Second remove of the same id is a no-op.
        assert!(!r.remove_peer(&pid("alice")));
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn router_peers_iteration_preserves_insertion_order() {
        let mut r = MediaRouter::new();
        let names = ["alice", "bob", "charlie", "dave"];
        for n in &names {
            r.add_peer(MediaPeer::new(pid(n), Instant::now()));
        }
        let observed: Vec<_> = r.peers().iter().map(|p| p.id().clone()).collect();
        let expected: Vec<_> = names.iter().map(|n| pid(n)).collect();
        assert_eq!(observed, expected);
    }

    /// Malformed offer string must fail at the str0m
    /// parser boundary with the typed
    /// [`MediaError::BadSdp`] — not panic, not surface
    /// as `MediaError::Str0m`. The audit-pack contract
    /// for variant-based error matching at the WebSocket
    /// layer (`router.rs` will key its
    /// `ErrorCode::BadSdp` reply off this).
    #[test]
    fn accept_offer_rejects_garbage_sdp() {
        let mut p = MediaPeer::new(pid("alice"), Instant::now());
        let err = p
            .accept_offer("v=0\r\n... obviously not a valid SDP ...")
            .expect_err("malformed SDP must be rejected");
        match err {
            MediaError::BadSdp(_) => {},
            other => panic!("expected BadSdp, got {other:?}"),
        }
    }

    /// Malformed ICE-candidate string must fail with
    /// [`MediaError::BadIceCandidate`] — symmetric
    /// contract to `accept_offer`.
    #[test]
    fn add_remote_candidate_rejects_garbage() {
        let mut p = MediaPeer::new(pid("alice"), Instant::now());
        let err = p
            .add_remote_candidate("not a candidate")
            .expect_err("garbage candidate must be rejected");
        match err {
            MediaError::BadIceCandidate(_) => {},
            other => panic!("expected BadIceCandidate, got {other:?}"),
        }
    }

    /// Empty offer string must also surface as
    /// `BadSdp` (defence against degenerate input from a
    /// buggy or hostile client).
    #[test]
    fn accept_offer_rejects_empty_string() {
        let mut p = MediaPeer::new(pid("alice"), Instant::now());
        let err = p.accept_offer("").expect_err("empty SDP must be rejected");
        match err {
            MediaError::BadSdp(_) => {},
            other => panic!("expected BadSdp, got {other:?}"),
        }
    }

    /// `peer_or_insert` returns the same peer instance on
    /// a second lookup with the same id — no duplicate
    /// rows in the router.
    #[test]
    fn peer_or_insert_is_idempotent_on_repeated_id() {
        let mut r = MediaRouter::new();
        let id = pid("alice");
        r.peer_or_insert(id.clone(), Instant::now());
        r.peer_or_insert(id.clone(), Instant::now());
        assert_eq!(r.len(), 1, "second insert must reuse the existing peer");
    }

    /// `spawn_media_task` → `accept_offer` round-trip on
    /// a malformed offer surfaces as
    /// `MediaCommandError::Media(MediaError::BadSdp)`
    /// through the channel. Exercises mpsc send-path,
    /// oneshot reply path, lazy `peer_or_insert` on
    /// first command, and typed-error propagation
    /// across the async/sync boundary.
    #[tokio::test]
    async fn task_accept_offer_surfaces_bad_sdp_through_channel() {
        let handle = spawn_media_task();
        let err = handle
            .accept_offer(pid("alice"), "garbage".into())
            .await
            .expect_err("malformed SDP must surface as BadSdp through the task");
        match err {
            MediaCommandError::Media(MediaError::BadSdp(_)) => {},
            other => panic!("expected Media(BadSdp), got {other:?}"),
        }
    }

    /// `add_remote_candidate` over the channel surfaces
    /// `BadIceCandidate` symmetrically.
    #[tokio::test]
    async fn task_add_remote_candidate_surfaces_bad_candidate_through_channel() {
        let handle = spawn_media_task();
        let err = handle
            .add_remote_candidate(pid("alice"), "not a candidate".into())
            .await
            .expect_err("garbage candidate must surface as BadIceCandidate through the task");
        match err {
            MediaCommandError::Media(MediaError::BadIceCandidate(_)) => {},
            other => panic!("expected Media(BadIceCandidate), got {other:?}"),
        }
    }
}
