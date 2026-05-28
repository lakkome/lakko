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
//! ## What this step ships
//!
//! Step 1 (this file) is the foundation:
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
//! ## What does NOT land in step 1
//!
//! Steps 2-5 of S-3-B-routing-media:
//!
//! - Step 2 — wire SDP-offer / SDP-answer /
//!   ICE-candidate frames through
//!   [`crate::protocol::ClientFrame::SignalRelay`]
//!   onto each peer's `str0m::Rtc::sdp_api`.
//! - Step 3 — open the SFU's UDP socket + spawn the
//!   tokio task that polls the registry and writes to
//!   the socket.
//! - Step 4 — translate `str0m::Event::MediaData`
//!   into outgoing writes on every other peer's `Rtc`
//!   (RTP fanout).
//! - Step 5 — integration tests with synthetic
//!   `str0m::Rtc` clients in-process.
//!
//! Each step keeps the audit-pack invariant
//! ("payload-opaque router") intact.

use std::time::Instant;

use str0m::{Output, Rtc};
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
}
