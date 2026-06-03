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

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::media::{Direction, MediaData, MediaKind, Mid};
use str0m::net::{Protocol, Receive, Transmit};
use str0m::{Candidate, Event, Input, Output, Rtc};
use thiserror::Error;

use crate::peer::{PeerId, PeerRegistry};
use crate::protocol::ServerFrame;

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

/// Outcome of a single [`MediaPeer::forward_media_data`]
/// call. Distinct skip reasons live on distinct variants
/// so a future metrics layer can count `NoWriter` /
/// `NoCodecMatch` separately — the former is the
/// expected state during early track negotiation and
/// must not look like a codec mismatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaForward {
    /// The frame was forwarded into the destination
    /// peer's outbound stream; str0m will SRTP-encrypt
    /// it on the next `poll_output` tick.
    Sent,
    /// Destination peer has no writer for the source
    /// `mid` — typically pre-renegotiation.
    NoWriter,
    /// Destination peer has no codec PT matching the
    /// source `PayloadParams`.
    NoCodecMatch,
    /// str0m's `Writer::write` returned an error.
    WriteFailed(String),
}

/// One incoming track on a [`MediaPeer`]. Produced from
/// `Event::MediaAdded` — str0m fires it after a
/// successful SDP offer-answer negotiates a new media
/// line.
///
/// The router uses `(peer_id, mid)` to identify the
/// upstream side of a forwarded RTP flow. Subscribed
/// peers carry a matching [`TrackOutEntry`] pointing
/// back through the same key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackIn {
    /// The `mid` str0m assigned to this incoming media
    /// line on the originating peer's `Rtc`.
    pub mid: Mid,
    /// Audio vs. video — needed when the router opens
    /// the matching outgoing track on every other peer.
    pub kind: MediaKind,
}

/// Lifecycle phase of a downstream peer's track that
/// mirrors an upstream [`TrackIn`].
///
/// Mirror of `TrackOutState` from str0m's `chat.rs`
/// example. Three states matter:
///
/// - `ToOpen` — the SFU has registered the upstream
///   TrackIn but has not yet initiated SDP
///   renegotiation with this downstream peer. The
///   media task picks this state up on its next
///   `negotiate_if_needed` pass.
/// - `Negotiating(local_mid)` — renegotiation is in
///   flight; the SFU has emitted a `ServerFrame::SdpOffer`
///   and is waiting on `ClientFrame::SdpAnswer`. The
///   `local_mid` is the destination-side mid the SDP
///   change allocated.
/// - `Open(local_mid)` — the SDP answer landed; the
///   destination peer now has a real writer for
///   `local_mid` and [`fanout_media_data`] can shovel
///   frames through.
///
/// The two `ToStop` / `NegotiatingStop` states mirror
/// chat.rs's cleanup path but are deferred — the
/// current SFU drops the entire `MediaPeer` on
/// disconnect, so per-track stop renegotiation is not
/// needed today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackOutState {
    /// Track registered; SDP renegotiation not started.
    ToOpen,
    /// SDP renegotiation in flight; the destination-side
    /// mid the change allocated is carried so we can
    /// match the answer to this entry.
    Negotiating(Mid),
    /// SDP answer applied; writer for `local_mid` is
    /// live and [`MediaPeer::forward_media_data`] can
    /// use it as the destination.
    Open(Mid),
}

/// One outgoing track on a downstream peer that mirrors
/// an upstream [`TrackIn`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackOutEntry {
    /// Upstream peer that owns the source TrackIn.
    pub source_peer: MediaPeerId,
    /// Upstream side's mid (the source's `data.mid` on
    /// every `Event::MediaData` we forward through this
    /// entry).
    pub source_mid: Mid,
    /// Audio / video — pinned at registration so we do
    /// not need to chase the upstream peer at fanout
    /// time.
    pub kind: MediaKind,
    /// Lifecycle phase. See [`TrackOutState`].
    pub state: TrackOutState,
}

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
    /// Incoming tracks this peer announced via
    /// `Event::MediaAdded`. Populated in
    /// [`drain_outputs`]; consumed by
    /// [`MediaRouter::propagate_new_tracks_in`] when
    /// the router opens matching `TrackOutEntry`s on
    /// every other peer.
    tracks_in: Vec<TrackIn>,
    /// Outgoing tracks the SFU has opened (or is
    /// opening) on this peer so RTP from other peers'
    /// `tracks_in` can be forwarded here. Walked by
    /// [`MediaPeer::forward_media_data`] when it needs
    /// to resolve `(source_peer, source_mid)` to a
    /// local writer mid.
    tracks_out: Vec<TrackOutEntry>,
    /// In-flight SFU-initiated renegotiation. `Some`
    /// after [`MediaPeer::negotiate_if_needed`]
    /// produced an SDP offer that has been shipped to
    /// the peer but not yet answered. The next
    /// `ClientFrame::SdpAnswer` routes here via
    /// [`MediaPeer::accept_renegotiation_answer`],
    /// which hands the stored `SdpPendingOffer` +
    /// the inbound `SdpAnswer` to
    /// `Rtc::sdp_api().accept_answer(..)`.
    ///
    /// Only one renegotiation is in flight at a time —
    /// `negotiate_if_needed` short-circuits if
    /// `pending_offer.is_some()`. Adding a second
    /// concurrent change without applying the first
    /// would tangle str0m's internal SDP state.
    pending_offer: Option<SdpPendingOffer>,
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
            tracks_in: Vec::new(),
            tracks_out: Vec::new(),
            pending_offer: None,
        }
    }

    /// Immutable view of the upstream tracks this peer has
    /// announced via `Event::MediaAdded`. Read-only — the
    /// drain pass owns mutation.
    #[must_use]
    pub fn tracks_in(&self) -> &[TrackIn] {
        &self.tracks_in
    }

    /// Immutable view of the downstream tracks the SFU
    /// has opened (or is opening) on this peer so the
    /// fanout can shovel upstream RTP through. Read-only
    /// for the same reason.
    #[must_use]
    pub fn tracks_out(&self) -> &[TrackOutEntry] {
        &self.tracks_out
    }

    /// Record an upstream track this peer just announced
    /// via `Event::MediaAdded`. Returns the [`TrackIn`] so
    /// the caller (the router) can propagate matching
    /// `TrackOutEntry::ToOpen` rows to every other peer.
    ///
    /// Idempotent — repeated calls with the same `mid`
    /// return the existing entry rather than appending a
    /// duplicate. str0m occasionally fires `MediaAdded`
    /// twice during an SDP renegotiation that re-uses an
    /// existing m-line; this guard keeps the router's
    /// view consistent without leaning on str0m's
    /// internal de-dup.
    pub fn register_local_track_in(&mut self, mid: Mid, kind: MediaKind) -> TrackIn {
        if let Some(existing) = self.tracks_in.iter().find(|t| t.mid == mid).copied() {
            return existing;
        }
        let track = TrackIn { mid, kind };
        self.tracks_in.push(track);
        track
    }

    /// Record an upstream track from a *different* peer
    /// that this peer needs to receive. Adds a fresh
    /// [`TrackOutEntry`] in [`TrackOutState::ToOpen`]
    /// state; the SDP-renegotiation pass picks it up.
    ///
    /// Idempotent on `(source_peer, source_mid)` so the
    /// router's broadcast-to-everyone-else pass cannot
    /// accumulate duplicates if a `MediaAdded` event
    /// fires twice.
    pub fn observe_remote_track_in(&mut self, source_peer: MediaPeerId, source: TrackIn) -> bool {
        let already = self
            .tracks_out
            .iter()
            .any(|t| t.source_peer == source_peer && t.source_mid == source.mid);
        if already {
            return false;
        }
        self.tracks_out.push(TrackOutEntry {
            source_peer,
            source_mid: source.mid,
            kind: source.kind,
            state: TrackOutState::ToOpen,
        });
        true
    }

    /// `true` if the peer has an in-flight SFU-
    /// initiated renegotiation cycle. Used by the
    /// event loop to skip the
    /// [`negotiate_if_needed`](Self::negotiate_if_needed)
    /// call when it would be a no-op.
    #[must_use]
    pub fn has_pending_renegotiation(&self) -> bool {
        self.pending_offer.is_some()
    }

    /// Produce an SDP offer that promotes every
    /// `TrackOutState::ToOpen` entry to
    /// `TrackOutState::Negotiating(local_mid)`. Returns
    /// the SDP string the caller ships back to the peer
    /// as `ServerFrame::SdpOffer`.
    ///
    /// Returns `Ok(None)` when:
    ///
    /// - there are no `ToOpen` entries (nothing to
    ///   negotiate), or
    /// - a previous renegotiation is still in flight
    ///   (`pending_offer.is_some()`). The next call
    ///   after [`accept_renegotiation_answer`](Self::accept_renegotiation_answer)
    ///   picks up any `ToOpen` entries that landed
    ///   during the in-flight window.
    ///
    /// # Errors
    ///
    /// [`MediaError::Str0m`] if `Rtc::sdp_api()` /
    /// `apply()` surfaces an error.
    pub fn negotiate_if_needed(&mut self) -> MediaResult<Option<String>> {
        if self.pending_offer.is_some() {
            return Ok(None);
        }
        // Snapshot the indices of ToOpen entries — we
        // mutate `state` after the change-apply step
        // and a simultaneous iter+mut on the Vec would
        // fight the borrow checker.
        let to_open: Vec<usize> = self
            .tracks_out
            .iter()
            .enumerate()
            .filter(|(_, t)| matches!(t.state, TrackOutState::ToOpen))
            .map(|(i, _)| i)
            .collect();
        if to_open.is_empty() {
            return Ok(None);
        }

        let mut change = self.rtc.sdp_api();
        // Allocate one outgoing media line per ToOpen
        // entry. `Direction::SendOnly` — the SFU's job
        // is to forward RTP **to** this peer, not to
        // receive any back through these lines.
        //
        // The `stream_id` argument is the MSID stream
        // grouping; chat.rs uses the originating peer
        // id so the receiver can group all tracks from
        // one publisher into the same JS-side
        // `MediaStream`. We mirror that.
        let allocated: Vec<(usize, Mid)> = to_open
            .iter()
            .map(|&idx| {
                let entry = &self.tracks_out[idx];
                let mid = change.add_media(
                    entry.kind,
                    Direction::SendOnly,
                    Some(entry.source_peer.to_string()),
                    None,
                    None,
                );
                (idx, mid)
            })
            .collect();

        let Some((offer, pending)) = change.apply() else {
            // `apply` returns `None` when there are no
            // pending changes — should not happen given
            // we just queued `to_open.len()` add_media
            // calls, but defend against str0m API drift.
            return Ok(None);
        };

        for (idx, mid) in allocated {
            self.tracks_out[idx].state = TrackOutState::Negotiating(mid);
        }
        self.pending_offer = Some(pending);
        Ok(Some(offer.to_sdp_string()))
    }

    /// Apply a peer's `ClientFrame::SdpAnswer` to the
    /// in-flight renegotiation. Promotes every
    /// `TrackOutState::Negotiating(mid)` entry to
    /// `TrackOutState::Open(mid)` — fanout starts
    /// delivering through them on the next tick.
    ///
    /// # Errors
    ///
    /// - [`MediaError::Str0m`] when there is no
    ///   pending offer (the peer answered without us
    ///   asking) or `Rtc::sdp_api().accept_answer(..)`
    ///   refuses.
    /// - [`MediaError::BadSdp`] when `sdp` fails
    ///   `SdpAnswer::from_sdp_string`.
    pub fn accept_renegotiation_answer(&mut self, sdp: &str) -> MediaResult<()> {
        let Some(pending) = self.pending_offer.take() else {
            return Err(MediaError::Str0m(
                "received SdpAnswer with no pending offer".to_owned(),
            ));
        };
        let answer =
            SdpAnswer::from_sdp_string(sdp).map_err(|e| MediaError::BadSdp(e.to_string()))?;
        self.rtc
            .sdp_api()
            .accept_answer(pending, answer)
            .map_err(|e| MediaError::Str0m(e.to_string()))?;
        // Promote every Negotiating(..) entry. Multiple
        // tracks may have been added in the same
        // change-set; all of them flip on the same
        // answer.
        for entry in &mut self.tracks_out {
            if let TrackOutState::Negotiating(mid) = entry.state {
                entry.state = TrackOutState::Open(mid);
            }
        }
        Ok(())
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

    /// Register a host candidate that points at the
    /// SFU's bound UDP socket. The candidate is baked
    /// into the SDP answer the next call to
    /// [`Self::accept_offer`] produces.
    ///
    /// # Errors
    ///
    /// [`MediaError::Str0m`] on a `str0m::Candidate::host`
    /// failure (typically: address is not a valid
    /// host-candidate address).
    pub fn add_local_host_candidate(&mut self, addr: SocketAddr) -> MediaResult<()> {
        let candidate = Candidate::host(addr, Protocol::Udp)
            .map_err(|e| MediaError::Str0m(format!("host candidate: {e}")))?;
        self.rtc.add_local_candidate(candidate);
        Ok(())
    }

    /// `true` if the underlying `str0m::Rtc` would
    /// accept this UDP packet — exposed so the event
    /// loop can demux inbound datagrams.
    #[must_use]
    pub fn accepts(&self, input: &Input<'_>) -> bool {
        self.rtc.accepts(input)
    }

    /// Feed an `Input` into the str0m state machine.
    /// Mirrors `Rtc::handle_input` with typed-error
    /// mapping.
    ///
    /// # Errors
    ///
    /// [`MediaError::Str0m`] when `str0m::Rtc::handle_input`
    /// returns an error. Continue-on-error is up to the
    /// caller — typically the event loop logs and drops
    /// the peer.
    pub fn handle_input(&mut self, input: Input<'_>) -> MediaResult<()> {
        self.rtc
            .handle_input(input)
            .map_err(|e| MediaError::Str0m(e.to_string()))
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

    /// Best-effort forward of a [`MediaData`] frame
    /// captured from a *different* peer's `poll_output`.
    /// Returns the [`MediaForward`] outcome so the caller
    /// can keep observability counters per skip reason.
    ///
    /// The SFrame contract holds: `data.data` is the
    /// inner SFrame-encrypted payload bytes; we forward
    /// them verbatim into the destination peer's
    /// outbound stream without inspecting them. str0m
    /// re-encrypts at the SRTP layer for the per-hop
    /// destination — the inner SFrame ciphertext stays
    /// opaque from origin to destination (see audit-pack
    /// C-39).
    ///
    /// ## Mid resolution
    ///
    /// `data.mid` is the **source** peer's mid. The
    /// destination peer (this one) has its own mid for
    /// the corresponding outgoing track which the SDP
    /// renegotiation allocated. The mapping lives in
    /// [`tracks_out`](Self::tracks_out): walk for an
    /// entry whose `(source_peer, source_mid)` matches
    /// `(source_peer, data.mid)` and is in
    /// `TrackOutState::Open(local_mid)` — that's the mid
    /// we hand `Rtc::writer`.
    ///
    /// Until the renegotiation pass has flipped the
    /// entry to `Open`, the function returns
    /// `NoWriter` — the upstream RTP is dropped on the
    /// floor at this peer because there is no
    /// downstream surface to forward it through. The
    /// caller's metrics layer treats this as a
    /// transient pre-negotiation drop, not a fatal
    /// error.
    ///
    /// Skip conditions (each surfaces as a distinct
    /// [`MediaForward`] variant so a future metrics layer
    /// can count them without colliding):
    ///
    /// - `NoWriter`: no `tracks_out` entry in
    ///   `Open(local_mid)` state for the source pair, or
    ///   str0m no longer has a writer for that
    ///   `local_mid` (rare — typically a race against
    ///   peer teardown).
    /// - `NoCodecMatch`: destination's negotiated codec
    ///   set doesn't include the incoming PT.
    /// - `WriteFailed`: str0m refused the write (rare —
    ///   typically a state-machine inconsistency or an
    ///   unknown PT slipped past `match_params`).
    pub fn forward_media_data(
        &mut self,
        source_peer: &MediaPeerId,
        data: &MediaData,
    ) -> MediaForward {
        // Resolve the destination mid via the
        // `tracks_out` map. We only forward through an
        // entry that has reached the `Open(..)` state —
        // `ToOpen` / `Negotiating` mean the SDP
        // renegotiation is in flight and the destination
        // writer is not yet live.
        let local_mid = self.tracks_out.iter().find_map(|entry| {
            if entry.source_peer == *source_peer && entry.source_mid == data.mid {
                if let TrackOutState::Open(local) = entry.state {
                    return Some(local);
                }
            }
            None
        });
        let Some(local_mid) = local_mid else {
            return MediaForward::NoWriter;
        };
        let Some(writer) = self.rtc.writer(local_mid) else {
            return MediaForward::NoWriter;
        };
        let Some(pt) = writer.match_params(data.params) else {
            return MediaForward::NoCodecMatch;
        };
        if let Err(e) = writer.write(pt, data.network_time, data.time, data.data.clone()) {
            return MediaForward::WriteFailed(e.to_string());
        }
        MediaForward::Sent
    }
}

impl std::fmt::Debug for MediaPeer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `str0m::Rtc` does not implement Debug — print
        // the id + liveness + track-bookkeeping counts.
        // `finish_non_exhaustive` because the underlying
        // `rtc` field is intentionally elided.
        f.debug_struct("MediaPeer")
            .field("id", &self.id)
            .field("alive", &self.rtc.is_alive())
            .field("tracks_in_len", &self.tracks_in.len())
            .field("tracks_out_len", &self.tracks_out.len())
            .finish_non_exhaustive()
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

    /// Retain only peers for which `keep` returns `true`.
    /// Used by the event loop to drop peers whose
    /// `Rtc::is_alive` flipped to `false` (DTLS/ICE
    /// failure). O(n) in the number of peers.
    pub fn retain_peers<F: FnMut(&MediaPeer) -> bool>(&mut self, keep: F) {
        self.peers.retain(keep);
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
    /// Apply a peer's `ClientFrame::SdpAnswer` to the
    /// in-flight SFU-initiated renegotiation
    /// (S-3-B-tracks step 2). Promotes the matching
    /// `TrackOutEntry` from
    /// `TrackOutState::Negotiating(mid)` to
    /// `TrackOutState::Open(mid)`.
    AcceptRenegotiationAnswer {
        /// Originating peer id.
        peer_id: MediaPeerId,
        /// SDP answer the peer's RTCPeerConnection
        /// produced via `createAnswer`.
        answer_sdp: String,
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

    /// Send an
    /// [`AcceptRenegotiationAnswer`](MediaCommand::AcceptRenegotiationAnswer)
    /// command. Called by the router when it routes a
    /// `ClientFrame::SdpAnswer` back to the media task.
    ///
    /// # Errors
    ///
    /// Symmetric to [`Self::accept_offer`].
    pub async fn accept_renegotiation_answer(
        &self,
        peer_id: MediaPeerId,
        answer_sdp: String,
    ) -> std::result::Result<(), MediaCommandError> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.tx
            .try_send(MediaCommand::AcceptRenegotiationAnswer {
                peer_id,
                answer_sdp,
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

/// Spawn the media event-loop task. Binds a
/// `tokio::net::UdpSocket` at `bind_addr` (typically
/// the value of `LAKKO_SFU_MEDIA_BIND`), captures its
/// local address, and returns the handle the signaling
/// layer uses to push commands.
///
/// The task is a single tokio future driven by a
/// `select!` over three sources:
///
/// 1. Inbound UDP packets on the bound socket.
/// 2. [`MediaCommand`]s from the signaling layer.
/// 3. The earliest pending `str0m::Output::Timeout` of
///    any admitted [`MediaPeer`].
///
/// After every input, the task drains
/// `str0m::Rtc::poll_output` on every peer and writes
/// any `Output::Transmit` packets back to the socket.
/// Dead peers (those whose `Rtc::is_alive` flipped to
/// `false` after DTLS/ICE failure) are pruned at the
/// end of each tick.
///
/// `Event::MediaData` events are forwarded between
/// peers via `fanout_media_data` (S-3-B-routing-media
/// step 5). `Event::MediaAdded` events get registered
/// as `TrackIn` + propagated to every other peer as
/// `TrackOutEntry::ToOpen` (S-3-B-tracks step 1).
///
/// `peers` is the same [`PeerRegistry`] the signaling
/// layer owns. The media task uses it to push SFU-
/// initiated `ServerFrame::SdpOffer` frames back to
/// the right peer's outbound channel after a
/// successful [`MediaPeer::negotiate_if_needed`] call.
///
/// # Errors
///
/// [`MediaError::Str0m`] on UDP-bind failure (wrong
/// permission, port already in use, etc.).
pub async fn spawn_media_task(
    bind_addr: SocketAddr,
    peers: PeerRegistry,
) -> MediaResult<(MediaTaskHandle, SocketAddr)> {
    let socket = tokio::net::UdpSocket::bind(bind_addr)
        .await
        .map_err(|e| MediaError::Str0m(format!("UDP bind {bind_addr}: {e}")))?;
    let local_addr = socket
        .local_addr()
        .map_err(|e| MediaError::Str0m(format!("UDP local_addr: {e}")))?;

    let (tx, rx) = tokio::sync::mpsc::channel::<MediaCommand>(MEDIA_COMMAND_CAPACITY);
    tokio::spawn(run_media_loop(socket, local_addr, rx, peers));
    Ok((MediaTaskHandle { tx }, local_addr))
}

/// Spawn a media task **without** binding the UDP
/// socket. The task processes commands but never reads
/// or writes packets — useful for unit tests that
/// exercise the SDP signaling path without standing up
/// real ICE / DTLS. The test variant does not drive
/// renegotiations (no PeerRegistry is plumbed).
///
/// Production paths always go through
/// [`spawn_media_task`].
#[must_use]
pub fn spawn_media_task_no_socket() -> MediaTaskHandle {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<MediaCommand>(MEDIA_COMMAND_CAPACITY);
    tokio::spawn(async move {
        let mut router = MediaRouter::new();
        while let Some(cmd) = rx.recv().await {
            handle_command(&mut router, cmd, None);
        }
    });
    MediaTaskHandle { tx }
}

/// Process one command against `router`. If
/// `local_addr` is `Some`, the per-peer host candidate
/// is registered before [`MediaPeer::accept_offer`] so
/// the answer's `a=candidate:` line points at the SFU's
/// bound socket.
fn handle_command(router: &mut MediaRouter, cmd: MediaCommand, local_addr: Option<SocketAddr>) {
    match cmd {
        MediaCommand::AcceptOffer {
            peer_id,
            offer_sdp,
            reply,
        } => {
            let peer = router.peer_or_insert(peer_id, Instant::now());
            // Best-effort: if the host candidate fails
            // we still try to accept the offer; the
            // resulting answer just won't carry our
            // address, which is observable to the
            // client as a failed connectivity check.
            if let Some(addr) = local_addr {
                let _ = peer.add_local_host_candidate(addr);
            }
            let res = peer.accept_offer(&offer_sdp);
            // If the receiver dropped, the originating
            // future has bailed — ignore the send
            // failure.
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
        MediaCommand::AcceptRenegotiationAnswer {
            peer_id,
            answer_sdp,
            reply,
        } => {
            // A peer answering a renegotiation MUST
            // already have a media slot (the inbound
            // SdpOffer cycle created it). If not, the
            // peer is replying to a phantom offer.
            let res = match router.peer_mut(&peer_id) {
                Some(peer) => peer.accept_renegotiation_answer(&answer_sdp),
                None => Err(MediaError::Str0m(format!(
                    "SdpAnswer from unknown media peer {peer_id}"
                ))),
            };
            let _ = reply.send(res);
        },
    }
}

/// The main event loop. Lives in its own function so it
/// can be unit-tested in isolation (future sub-sprint:
/// a synthetic-client integration test in step 5).
async fn run_media_loop(
    socket: tokio::net::UdpSocket,
    local_addr: SocketAddr,
    mut rx: tokio::sync::mpsc::Receiver<MediaCommand>,
    peers: PeerRegistry,
) {
    let mut router = MediaRouter::new();
    let mut buf = vec![0u8; UDP_RECV_BUF];

    loop {
        // Compute the earliest pending peer timeout —
        // bounds the select's sleep so we drive the
        // str0m state machines forward even when no
        // network or command activity is happening.
        // Capture-then-fanout: drain_outputs collects
        // `Event::MediaData` events with their origin
        // peer ids; fanout_media_data forwards them into
        // every other peer's writer so the next drain
        // tick converts them to `Output::Transmit` UDP
        // sends.
        let now = Instant::now();
        let summary = drain_outputs(&mut router, &socket, now).await;
        // Propagate any new upstream tracks to every
        // other peer's `tracks_out` BEFORE fanout —
        // fanout depends on the `tracks_out` map to
        // resolve destination mids.
        propagate_new_tracks_in(&mut router, &summary.new_tracks);
        // S-3-B-tracks step 2: for every peer that
        // gained a fresh `ToOpen` entry, kick off the
        // SDP renegotiation. The produced offer rides
        // back to the peer's WS via the same
        // PeerRegistry the signaling layer owns.
        drive_renegotiations(&mut router, &peers).await;
        fanout_media_data(&mut router, &summary.pending);
        let sleep_for = summary
            .earliest_deadline
            .saturating_duration_since(now)
            .min(MAX_TICK_INTERVAL);

        tokio::select! {
            biased; // prioritise commands so a fast signaling layer is not starved by busy traffic
            cmd = rx.recv() => {
                match cmd {
                    Some(cmd) => handle_command(&mut router, cmd, Some(local_addr)),
                    None => return, // all senders dropped — task is done
                }
            }
            recv = socket.recv_from(&mut buf) => {
                if let Ok((n, source)) = recv {
                    let Ok(contents) = (buf[..n]).try_into() else {
                        continue;
                    };
                    let input = Input::Receive(
                        Instant::now(),
                        Receive {
                            proto: Protocol::Udp,
                            source,
                            destination: local_addr,
                            contents,
                        },
                    );
                    if let Some(peer) = router
                        .peers_mut()
                        .iter_mut()
                        .find(|p| p.accepts(&input))
                    {
                        // Errors here typically mean an
                        // ICE / DTLS bug in str0m or a
                        // spoofed packet that survived
                        // the accepts() check. Log and
                        // press on — `is_alive` cleanup
                        // at the loop tail catches a
                        // wedged peer.
                        if let Err(e) = peer.handle_input(input) {
                            tracing::warn!(error = %e, "media: handle_input failed");
                        }
                    }
                }
            }
            () = tokio::time::sleep(sleep_for) => {
                // The timeout branch is intentionally
                // empty — the next loop iteration's
                // `drain_outputs` call will tick the
                // peers and recompute the deadline.
            }
        }

        // Prune peers whose Rtc died (DTLS failure,
        // ICE timeout, etc.). The Vec retain compacts
        // in-place — O(n) on the worst case.
        router.retain_peers(MediaPeer::is_alive);
    }
}

/// One [`MediaData`] event captured during a drain pass,
/// tagged with the originating peer id so the fanout
/// step can skip the source when forwarding.
pub(crate) struct PendingFanout {
    /// Peer that emitted the [`MediaData`] event.
    pub(crate) origin: MediaPeerId,
    /// The frame to be forwarded.
    pub(crate) data: MediaData,
}

/// One `Event::MediaAdded` captured during a drain pass.
/// The router post-pass propagates each new track to
/// every other peer's `tracks_out` as
/// [`TrackOutState::ToOpen`].
pub(crate) struct PendingTrackIn {
    /// Peer that announced the new media line.
    pub(crate) origin: MediaPeerId,
    /// The track itself (mid + kind).
    pub(crate) track: TrackIn,
}

/// Result of one tick's [`drain_outputs`] pass.
pub(crate) struct DrainSummary {
    /// Earliest pending `Output::Timeout` across all peers.
    pub(crate) earliest_deadline: Instant,
    /// [`MediaData`] frames captured during the drain,
    /// waiting to be forwarded to non-origin peers.
    pub(crate) pending: Vec<PendingFanout>,
    /// New `MediaAdded` events captured during the drain.
    /// The router propagates them to every other peer's
    /// `tracks_out` before the next fanout pass.
    pub(crate) new_tracks: Vec<PendingTrackIn>,
}

/// Drain `poll_output` on every peer; write any
/// `Output::Transmit` packets back to `socket`; collect
/// the earliest `Output::Timeout(t)` so the caller can
/// `sleep_until` it; capture `Event::MediaData` frames
/// into a pending-fanout queue. Returns a
/// [`DrainSummary`].
///
/// The caller is responsible for invoking
/// [`fanout_media_data`] on `summary.pending` after this
/// returns — keeping capture and dispatch separate lets
/// each peer's `poll_output` loop hold an exclusive
/// borrow on a single peer without dropping it mid-walk
/// to write to other peers.
async fn drain_outputs(
    router: &mut MediaRouter,
    socket: &tokio::net::UdpSocket,
    now: Instant,
) -> DrainSummary {
    let mut earliest_deadline = now + MAX_TICK_INTERVAL;
    let mut pending: Vec<PendingFanout> = Vec::new();
    let mut new_tracks: Vec<PendingTrackIn> = Vec::new();
    for peer in router.peers_mut() {
        let origin = peer.id().clone();
        loop {
            match peer.poll_output() {
                Ok(Output::Transmit(t)) => {
                    write_transmit(socket, &t).await;
                },
                Ok(Output::Timeout(at)) => {
                    if at < earliest_deadline {
                        earliest_deadline = at;
                    }
                    break;
                },
                Ok(Output::Event(event)) => match event {
                    Event::MediaData(data) => {
                        pending.push(PendingFanout {
                            origin: origin.clone(),
                            data,
                        });
                    },
                    Event::MediaAdded(added) => {
                        // Register the track on the
                        // origin side; the router
                        // post-pass propagates it to
                        // every other peer's
                        // `tracks_out` as `ToOpen`.
                        // `register_local_track_in`
                        // is idempotent on `mid`.
                        let track = peer.register_local_track_in(added.mid, added.kind);
                        new_tracks.push(PendingTrackIn {
                            origin: origin.clone(),
                            track,
                        });
                    },
                    // Other events (ICE state,
                    // keyframe requests, stats,
                    // MediaChanged, ChannelOpen, ...)
                    // are not actionable for the
                    // SFrame-passthrough router yet.
                    // The per-event surface lands as
                    // follow-up sub-sprints.
                    _ => {},
                },
                Err(e) => {
                    tracing::warn!(error = %e, "media: poll_output failed; dropping peer");
                    // The peer's `is_alive` will likely
                    // flip false at the next tick; we
                    // rely on the loop-tail retain to
                    // clean up rather than break here.
                    break;
                },
            }
        }
    }
    DrainSummary {
        earliest_deadline,
        pending,
        new_tracks,
    }
}

/// Per-pass tally of [`fanout_media_data`] outcomes — one
/// increment per (frame, destination peer). Returned so the
/// media task + tests can observe fanout health without
/// scraping the global metrics registry;
/// [`fanout_media_data`] also mirrors it into the
/// `lakko_sfu_media_frames_*` Prometheus counters.
///
/// The **fairness** property the SFU upholds: for a single
/// origin frame in an N-peer call, [`Self::attempted`]
/// equals `N - 1` — every non-origin peer is visited exactly
/// once (none skipped, none double-served) and the origin is
/// never offered its own frame.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FanoutOutcome {
    /// Frames handed to a live downstream writer.
    pub(crate) sent: u64,
    /// Skips because the destination's `tracks_out` entry is
    /// not yet `Open` (renegotiation in flight) or str0m has
    /// no writer for the mid (peer teardown race).
    pub(crate) no_writer: u64,
    /// Skips because the destination codec set excludes the
    /// incoming PT.
    pub(crate) no_codec_match: u64,
    /// str0m refused the write.
    pub(crate) write_failed: u64,
}

impl FanoutOutcome {
    /// Total forward attempts this pass = sum of all
    /// outcome buckets = `frames × non-origin peers`.
    pub(crate) fn attempted(&self) -> u64 {
        self.sent + self.no_writer + self.no_codec_match + self.write_failed
    }
}

/// Mirror a [`FanoutOutcome`] into the
/// `lakko_sfu_media_frames_{forwarded,dropped}_total`
/// counters. Drops are labelled by `reason`. Each bucket is
/// only touched when non-zero so we don't create empty
/// label series on every idle tick (the no-op recorder used
/// in unit tests makes these calls free either way).
fn record_fanout_metrics(outcome: &FanoutOutcome) {
    use crate::prom::names::{MEDIA_FRAMES_DROPPED_TOTAL, MEDIA_FRAMES_FORWARDED_TOTAL};
    if outcome.sent > 0 {
        metrics::counter!(MEDIA_FRAMES_FORWARDED_TOTAL).increment(outcome.sent);
    }
    if outcome.no_writer > 0 {
        metrics::counter!(MEDIA_FRAMES_DROPPED_TOTAL, "reason" => "no_writer")
            .increment(outcome.no_writer);
    }
    if outcome.no_codec_match > 0 {
        metrics::counter!(MEDIA_FRAMES_DROPPED_TOTAL, "reason" => "no_codec_match")
            .increment(outcome.no_codec_match);
    }
    if outcome.write_failed > 0 {
        metrics::counter!(MEDIA_FRAMES_DROPPED_TOTAL, "reason" => "write_failed")
            .increment(outcome.write_failed);
    }
    // `no_writer` is the expected pre-negotiation transient, so it
    // is not worth a log line. A write failure or codec mismatch is
    // unusual — surface a per-pass summary (debug level, off in
    // production by default) carrying the full tally for diagnosis.
    if outcome.write_failed > 0 || outcome.no_codec_match > 0 {
        tracing::debug!(
            attempted = outcome.attempted(),
            sent = outcome.sent,
            no_writer = outcome.no_writer,
            no_codec_match = outcome.no_codec_match,
            write_failed = outcome.write_failed,
            "media: fanout pass had non-transient drops"
        );
    }
}

/// Walk `pending` and forward each frame to every peer
/// in `router` that is NOT the origin. The write
/// enqueues bytes into the destination peer's str0m
/// state; the next `drain_outputs` tick will turn those
/// into `Output::Transmit` events that hit the wire.
///
/// Best-effort: forwarding skips peers without a
/// matching writer or codec PT — see [`MediaForward`]
/// for the per-variant skip semantics and the deferred
/// TracksIn/TracksOut mapping note.
pub(crate) fn fanout_media_data(
    router: &mut MediaRouter,
    pending: &[PendingFanout],
) -> FanoutOutcome {
    let mut outcome = FanoutOutcome::default();
    if pending.is_empty() || router.len() < 2 {
        return outcome;
    }
    for event in pending {
        // Walk the peer list manually so we can borrow
        // each non-origin peer mutably one at a time.
        let n = router.len();
        for idx in 0..n {
            // Borrow check: re-borrow per-iteration so we
            // can compare ids before touching the writer.
            let target_is_origin = router
                .peers()
                .get(idx)
                .is_some_and(|p| p.id() == &event.origin);
            if target_is_origin {
                continue;
            }
            let Some(peer) = router.peers_mut().get_mut(idx) else {
                continue;
            };
            // Fairness: every non-origin peer is visited
            // exactly once per frame, and each outcome is
            // tallied so a starved subscriber (a spike in
            // `no_writer`) is observable in the metrics
            // rather than silently dropped.
            match peer.forward_media_data(&event.origin, &event.data) {
                MediaForward::Sent => outcome.sent += 1,
                MediaForward::NoWriter => outcome.no_writer += 1,
                MediaForward::NoCodecMatch => outcome.no_codec_match += 1,
                MediaForward::WriteFailed(detail) => {
                    outcome.write_failed += 1;
                    tracing::warn!(
                        target_peer = %peer.id(),
                        error = %detail,
                        "media: forward write failed"
                    );
                },
            }
        }
    }
    record_fanout_metrics(&outcome);
    outcome
}

/// Walk `new_tracks` and push a matching
/// [`TrackOutEntry::ToOpen`] onto every non-origin
/// peer. The next renegotiation pass (S-3-B-tracks step
/// 2) picks the entry up, calls
/// `Rtc::sdp_api().add_media(...).apply()` and ships
/// the resulting SDP offer back to the destination peer
/// over the signaling wire.
pub(crate) fn propagate_new_tracks_in(router: &mut MediaRouter, new_tracks: &[PendingTrackIn]) {
    if new_tracks.is_empty() || router.len() < 2 {
        return;
    }
    for event in new_tracks {
        let n = router.len();
        for idx in 0..n {
            let target_is_origin = router
                .peers()
                .get(idx)
                .is_some_and(|p| p.id() == &event.origin);
            if target_is_origin {
                continue;
            }
            let Some(peer) = router.peers_mut().get_mut(idx) else {
                continue;
            };
            let inserted = peer.observe_remote_track_in(event.origin.clone(), event.track);
            if inserted {
                tracing::debug!(
                    origin = %event.origin,
                    target = %peer.id(),
                    mid = ?event.track.mid,
                    kind = ?event.track.kind,
                    "media: registered remote TrackIn on downstream peer"
                );
            }
        }
    }
}

/// Walk every peer; if any have `tracks_out` entries
/// in `ToOpen`, call [`MediaPeer::negotiate_if_needed`]
/// and ship the resulting SDP offer to that peer's
/// outbound channel as a [`ServerFrame::SdpOffer`].
///
/// The peer's `RTCPeerConnection` answers with
/// `ClientFrame::SdpAnswer`, which the router routes
/// through [`MediaCommand::AcceptRenegotiationAnswer`]
/// back to [`MediaPeer::accept_renegotiation_answer`].
///
/// Peers that have an in-flight cycle
/// (`has_pending_renegotiation == true`) are skipped —
/// the next `drive_renegotiations` tick will pick them
/// up once their answer arrives.
async fn drive_renegotiations(router: &mut MediaRouter, peers: &PeerRegistry) {
    // Collect (peer_id, sdp) pairs while holding only
    // the borrow on `router`. The async dispatch loop
    // below runs after the borrow ends so we can
    // freely await on the PeerRegistry lock.
    let pending: Vec<(MediaPeerId, String)> = router
        .peers_mut()
        .iter_mut()
        .filter_map(|peer| {
            if peer.has_pending_renegotiation() {
                return None;
            }
            match peer.negotiate_if_needed() {
                Ok(Some(sdp)) => Some((peer.id().clone(), sdp)),
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!(
                        peer = %peer.id(),
                        error = %e,
                        "media: negotiate_if_needed failed"
                    );
                    None
                },
            }
        })
        .collect();

    for (peer_id, sdp) in pending {
        let Some(peer) = peers.peer(&peer_id).await else {
            tracing::warn!(
                peer = %peer_id,
                "media: cannot dispatch SdpOffer — peer not in registry"
            );
            continue;
        };
        let Some(tx) = peer.outbound.as_ref() else {
            tracing::warn!(
                peer = %peer_id,
                "media: cannot dispatch SdpOffer — peer has no outbound channel"
            );
            continue;
        };
        match tx.try_send(ServerFrame::SdpOffer { sdp }) {
            Ok(()) => {
                tracing::debug!(
                    peer = %peer_id,
                    "media: dispatched SFU-initiated SdpOffer"
                );
            },
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                // The peer's writer half is wedged —
                // the connection is likely about to
                // close. Log + move on; the next tick
                // retries.
                tracing::warn!(
                    peer = %peer_id,
                    "media: outbound channel full; renegotiation SdpOffer dropped"
                );
            },
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                tracing::debug!(
                    peer = %peer_id,
                    "media: outbound channel closed (peer disconnected mid-renegotiation)"
                );
            },
        }
    }
}

/// Send one `str0m::net::Transmit` out the bound
/// socket. Errors are logged but not surfaced — UDP is
/// already best-effort, and an EAGAIN here typically
/// resolves on the next tick.
async fn write_transmit(socket: &tokio::net::UdpSocket, t: &Transmit) {
    if let Err(e) = socket.send_to(&t.contents, t.destination).await {
        tracing::warn!(
            error = %e,
            destination = %t.destination,
            "media: UDP send failed"
        );
    }
}

/// UDP receive buffer — sized to the largest IPv4
/// fragment-free datagram (~1500 minus IP/UDP headers
/// + safety margin), matching str0m's example.
const UDP_RECV_BUF: usize = 2000;

/// Ceiling on how long the event loop sleeps between
/// ticks even when no peer has declared a deadline.
/// 100 ms matches str0m's chat-example heartbeat and is
/// far below the ICE keepalive interval.
const MAX_TICK_INTERVAL: Duration = Duration::from_millis(100);

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
        let handle = spawn_media_task_no_socket();
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
        let handle = spawn_media_task_no_socket();
        let err = handle
            .add_remote_candidate(pid("alice"), "not a candidate".into())
            .await
            .expect_err("garbage candidate must surface as BadIceCandidate through the task");
        match err {
            MediaCommandError::Media(MediaError::BadIceCandidate(_)) => {},
            other => panic!("expected Media(BadIceCandidate), got {other:?}"),
        }
    }

    /// `add_local_host_candidate` accepts a loopback
    /// IPv4 address — the smoke test for the event-loop
    /// host-candidate registration path.
    #[test]
    fn add_local_host_candidate_accepts_loopback() {
        let mut p = MediaPeer::new(pid("alice"), Instant::now());
        let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        p.add_local_host_candidate(addr)
            .expect("host candidate accepted");
    }

    /// `accepts` returns `true` for an `Input::Timeout`
    /// — the str0m contract is that timeouts are
    /// universally consumed. Validates the basic
    /// event-loop demux gate.
    #[test]
    fn accepts_timeout_input_unconditionally() {
        let p = MediaPeer::new(pid("alice"), Instant::now());
        let input = Input::Timeout(Instant::now());
        assert!(p.accepts(&input), "Rtc must accept Timeout inputs");
    }

    /// `spawn_media_task(addr)` succeeds on a free
    /// loopback port and returns a usable handle. End-
    /// to-end smoke that exercises:
    /// - real UDP bind on tokio's runtime,
    /// - local_addr capture,
    /// - command channel reachable,
    /// - event-loop spawned without panic.
    ///
    /// Loopback port 0 lets the OS pick a free port to
    /// dodge flakiness when the test runs in parallel.
    #[tokio::test]
    async fn spawn_media_task_binds_loopback() {
        let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let peers = crate::peer::PeerRegistry::new();
        let (handle, local_addr) = spawn_media_task(bind, peers)
            .await
            .expect("UDP bind on loopback:0 succeeds");
        assert!(local_addr.ip().is_loopback());
        assert_ne!(local_addr.port(), 0, "OS-picked port must be non-zero");
        // The task is alive; reuse the handle for a
        // round-trip command that fails at the SDP
        // parser to confirm the bound task is wired the
        // same way as the no-socket variant.
        let err = handle
            .accept_offer(pid("alice"), "garbage".into())
            .await
            .expect_err("malformed SDP must surface as BadSdp");
        assert!(matches!(
            err,
            MediaCommandError::Media(MediaError::BadSdp(_))
        ));
    }

    /// `forward_media_data` on a freshly-constructed
    /// `MediaPeer` returns [`MediaForward::NoWriter`] —
    /// the destination peer has no negotiated outgoing
    /// media-line yet, so the SFU has no writer to push
    /// into. This is the expected steady-state outcome
    /// during the brief pre-renegotiation window after a
    /// new peer joins and before its outbound track is
    /// added.
    ///
    /// The test guarantees the variant is `NoWriter` and
    /// NOT `NoCodecMatch` — a future codec-mismatch path
    /// must surface separately so observability counters
    /// stay disambiguated.
    #[test]
    fn forward_to_peer_without_writer_returns_no_writer() {
        let mut p = MediaPeer::new(pid("alice"), Instant::now());
        let synthetic = synthetic_media_data();
        // A fresh peer has no `tracks_out` entry for the
        // synthetic source, so `forward_media_data`
        // short-circuits to `NoWriter` before even
        // touching `Rtc::writer`. The source id is
        // arbitrary — the test guarantees the variant
        // mapping, not any particular topology.
        let source = pid("bob");
        let outcome = p.forward_media_data(&source, &synthetic);
        assert_eq!(
            outcome,
            MediaForward::NoWriter,
            "no tracks_out entry → NoWriter"
        );
    }

    /// `fanout_media_data` is a no-op when the router
    /// has a single peer — there is no non-origin peer
    /// to forward to, so the call must return without
    /// touching state.
    #[test]
    fn fanout_with_single_peer_is_noop() {
        let mut r = MediaRouter::new();
        r.add_peer(MediaPeer::new(pid("alice"), Instant::now()));
        let pending = vec![PendingFanout {
            origin: pid("alice"),
            data: synthetic_media_data(),
        }];
        // Smoke — must return without panic and leave
        // the router untouched.
        fanout_media_data(&mut r, &pending);
        assert_eq!(r.len(), 1);
    }

    /// `fanout_media_data` skips the origin peer when
    /// forwarding — the origin must not receive its own
    /// echo. Because none of the destination peers have
    /// a writer for the synthetic mid, the call is a
    /// no-op on each, but the iteration must NOT panic
    /// on borrow-check (the origin-skip path is what we
    /// are exercising).
    #[test]
    fn fanout_skips_origin_peer() {
        let mut r = MediaRouter::new();
        r.add_peer(MediaPeer::new(pid("alice"), Instant::now()));
        r.add_peer(MediaPeer::new(pid("bob"), Instant::now()));
        r.add_peer(MediaPeer::new(pid("charlie"), Instant::now()));
        let pending = vec![PendingFanout {
            origin: pid("bob"),
            data: synthetic_media_data(),
        }];
        fanout_media_data(&mut r, &pending);
        // All three peers still present, none died.
        assert_eq!(r.len(), 3);
        for peer in r.peers() {
            assert!(peer.is_alive());
        }
    }

    /// Empty `pending` slice is a no-op even with many
    /// peers — defensive against silent walking of the
    /// peer list.
    #[test]
    fn fanout_empty_pending_is_noop() {
        let mut r = MediaRouter::new();
        for n in &["a", "b", "c"] {
            r.add_peer(MediaPeer::new(pid(n), Instant::now()));
        }
        fanout_media_data(&mut r, &[]);
        assert_eq!(r.len(), 3);
    }

    /// Synthetic `MediaData` for tests — uses a
    /// fabricated mid + PT that will never match a
    /// freshly-constructed `Rtc`'s codec config, so
    /// `forward_media_data` always returns `NoWriter`
    /// here. The struct fields exercised by the fanout
    /// path (mid, params, network_time, time, data) all
    /// take values that pass str0m's API but never
    /// match a real session.
    fn synthetic_media_data() -> MediaData {
        use str0m::format::{Codec, CodecExtra, CodecSpec, FormatParams, PayloadParams};
        use str0m::media::{Frequency, MediaTime, Mid, Pt};
        use str0m::rtp::{ExtensionValues, SeqNo};
        let mid = Mid::new();
        let pt: Pt = 96.into();
        let params = PayloadParams::new(
            pt,
            None,
            CodecSpec {
                codec: Codec::Opus,
                channels: Some(2),
                clock_rate: Frequency::FORTY_EIGHT_KHZ,
                format: FormatParams::default(),
            },
        );
        let seq: SeqNo = 0_u64.into();
        MediaData {
            mid,
            pt,
            rid: None,
            params,
            time: MediaTime::new(0, Frequency::FORTY_EIGHT_KHZ),
            network_time: Instant::now(),
            seq_range: seq..=seq,
            contiguous: true,
            data: vec![0; 16],
            ext_vals: ExtensionValues::default(),
            codec_extra: CodecExtra::None,
            last_sender_info: None,
            audio_start_of_talk_spurt: false,
        }
    }

    // ============================================================
    // Fanout fairness (C-39 follow-up)
    // ============================================================

    /// A single origin frame in an N-peer call is offered to
    /// EVERY non-origin peer exactly once — never to the
    /// origin, never twice, never skipping one. Unit-test
    /// peers have no negotiated writer, so every attempt lands
    /// in `no_writer`; the tally proves the iteration touched
    /// each of the `N - 1` non-origin peers (the fairness
    /// invariant), independent of writer state. Verbatim
    /// delivery through an Open writer is covered by the
    /// two-client synthetic E2E.
    #[test]
    fn fanout_visits_every_non_origin_peer_for_three_plus() {
        for n_peers in [3usize, 4, 5] {
            let mut r = MediaRouter::new();
            let names: Vec<String> = (0..n_peers).map(|i| format!("peer{i}")).collect();
            for nm in &names {
                r.add_peer(MediaPeer::new(pid(nm), Instant::now()));
            }
            let pending = vec![PendingFanout {
                origin: pid(&names[0]),
                data: synthetic_media_data(),
            }];

            let outcome = fanout_media_data(&mut r, &pending);

            let expected = (n_peers - 1) as u64;
            assert_eq!(
                outcome.attempted(),
                expected,
                "{n_peers}-peer fanout must touch every non-origin peer exactly once"
            );
            assert_eq!(outcome.no_writer, expected, "{n_peers}-peer no_writer");
            assert_eq!(outcome.sent, 0, "{n_peers}-peer sent");
            assert_eq!(outcome.no_codec_match, 0, "{n_peers}-peer no_codec_match");
            assert_eq!(outcome.write_failed, 0, "{n_peers}-peer write_failed");
        }
    }

    /// The tally is per `(frame × non-origin peer)`: two
    /// frames from the same origin in a 3-peer call yield
    /// `2 × 2 = 4` attempts.
    #[test]
    fn fanout_tally_is_per_frame_times_non_origin_peers() {
        let mut r = MediaRouter::new();
        for nm in &["a", "b", "c"] {
            r.add_peer(MediaPeer::new(pid(nm), Instant::now()));
        }
        let pending = vec![
            PendingFanout {
                origin: pid("a"),
                data: synthetic_media_data(),
            },
            PendingFanout {
                origin: pid("a"),
                data: synthetic_media_data(),
            },
        ];

        let outcome = fanout_media_data(&mut r, &pending);

        assert_eq!(outcome.attempted(), 4);
        assert_eq!(outcome.no_writer, 4);
    }

    // ============================================================
    // S-3-B-tracks step 1 — TrackIn / TrackOut bookkeeping
    // ============================================================

    /// `register_local_track_in` appends a fresh
    /// [`TrackIn`] and returns it.
    #[test]
    fn register_local_track_in_appends() {
        let mut p = MediaPeer::new(pid("alice"), Instant::now());
        let mid = Mid::new();
        let track = p.register_local_track_in(mid, MediaKind::Audio);
        assert_eq!(track.mid, mid);
        assert_eq!(track.kind, MediaKind::Audio);
        assert_eq!(p.tracks_in().len(), 1);
    }

    /// Two calls with the same `mid` must return the
    /// same [`TrackIn`] without duplicating the entry —
    /// `MediaAdded` occasionally fires twice when an
    /// existing m-line is re-negotiated.
    #[test]
    fn register_local_track_in_is_idempotent_on_mid() {
        let mut p = MediaPeer::new(pid("alice"), Instant::now());
        let mid = Mid::new();
        let first = p.register_local_track_in(mid, MediaKind::Audio);
        let second = p.register_local_track_in(mid, MediaKind::Audio);
        assert_eq!(first, second);
        assert_eq!(p.tracks_in().len(), 1, "no duplicate entry");
    }

    /// `observe_remote_track_in` appends a fresh
    /// [`TrackOutEntry`] in [`TrackOutState::ToOpen`].
    #[test]
    fn observe_remote_track_in_appends_to_open() {
        let mut p = MediaPeer::new(pid("alice"), Instant::now());
        let mid = Mid::new();
        let source = TrackIn {
            mid,
            kind: MediaKind::Audio,
        };
        let inserted = p.observe_remote_track_in(pid("bob"), source);
        assert!(inserted);
        assert_eq!(p.tracks_out().len(), 1);
        let entry = &p.tracks_out()[0];
        assert_eq!(entry.source_peer, pid("bob"));
        assert_eq!(entry.source_mid, mid);
        assert_eq!(entry.kind, MediaKind::Audio);
        assert_eq!(entry.state, TrackOutState::ToOpen);
    }

    /// Repeated `observe_remote_track_in` calls on the
    /// same `(source_peer, source_mid)` are de-duped.
    /// The router's broadcast-to-everyone-else pass
    /// cannot accumulate phantom entries.
    #[test]
    fn observe_remote_track_in_is_idempotent_on_source_pair() {
        let mut p = MediaPeer::new(pid("alice"), Instant::now());
        let mid = Mid::new();
        let source = TrackIn {
            mid,
            kind: MediaKind::Audio,
        };
        assert!(p.observe_remote_track_in(pid("bob"), source));
        assert!(!p.observe_remote_track_in(pid("bob"), source));
        assert_eq!(p.tracks_out().len(), 1);
    }

    /// `propagate_new_tracks_in` opens a downstream
    /// entry on every non-origin peer, skips the origin
    /// itself, and is idempotent on a repeated call.
    #[test]
    fn propagate_new_tracks_in_skips_origin_and_is_idempotent() {
        let mut r = MediaRouter::new();
        r.add_peer(MediaPeer::new(pid("alice"), Instant::now()));
        r.add_peer(MediaPeer::new(pid("bob"), Instant::now()));
        r.add_peer(MediaPeer::new(pid("charlie"), Instant::now()));
        let mid = Mid::new();
        let pending = vec![PendingTrackIn {
            origin: pid("alice"),
            track: TrackIn {
                mid,
                kind: MediaKind::Video,
            },
        }];

        propagate_new_tracks_in(&mut r, &pending);

        // Alice (origin) has no tracks_out for her own
        // upstream track.
        let alice = r.peers().iter().find(|p| p.id() == &pid("alice")).unwrap();
        assert_eq!(alice.tracks_out().len(), 0);

        // Bob + charlie each have one entry pointing
        // back at alice's mid.
        for name in ["bob", "charlie"] {
            let peer = r.peers().iter().find(|p| p.id() == &pid(name)).unwrap();
            assert_eq!(peer.tracks_out().len(), 1, "{name}");
            assert_eq!(peer.tracks_out()[0].source_peer, pid("alice"));
            assert_eq!(peer.tracks_out()[0].source_mid, mid);
            assert_eq!(peer.tracks_out()[0].kind, MediaKind::Video);
            assert_eq!(peer.tracks_out()[0].state, TrackOutState::ToOpen);
        }

        // Second call must not duplicate.
        propagate_new_tracks_in(&mut r, &pending);
        for name in ["bob", "charlie"] {
            let peer = r.peers().iter().find(|p| p.id() == &pid(name)).unwrap();
            assert_eq!(peer.tracks_out().len(), 1, "{name} stays at 1");
        }
    }

    /// Empty `new_tracks` is a no-op; single-peer
    /// router is a no-op even with a non-empty list.
    #[test]
    fn propagate_new_tracks_in_edge_cases() {
        let mut r = MediaRouter::new();
        r.add_peer(MediaPeer::new(pid("alice"), Instant::now()));
        propagate_new_tracks_in(&mut r, &[]);
        let pending = vec![PendingTrackIn {
            origin: pid("alice"),
            track: TrackIn {
                mid: Mid::new(),
                kind: MediaKind::Audio,
            },
        }];
        // Single peer → no non-origin destinations.
        propagate_new_tracks_in(&mut r, &pending);
        assert_eq!(r.peers()[0].tracks_out().len(), 0);
    }

    /// Forwarding refuses to deliver through a
    /// `TrackOutEntry` that is still in
    /// `TrackOutState::ToOpen` — the renegotiation has
    /// not yet promoted it to `Open(local_mid)`. This
    /// pins the step-1 / step-2 boundary: step 1 wires
    /// the bookkeeping; step 2 is what makes fanout
    /// effective.
    #[test]
    fn forward_returns_no_writer_when_tracks_out_is_to_open() {
        let mut p = MediaPeer::new(pid("alice"), Instant::now());
        let synthetic = synthetic_media_data();
        let source = pid("bob");
        // Even with a matching `tracks_out` entry in
        // ToOpen state, fanout must surface NoWriter
        // (state machine isn't ready yet).
        p.observe_remote_track_in(
            source.clone(),
            TrackIn {
                mid: synthetic.mid,
                kind: MediaKind::Audio,
            },
        );
        assert_eq!(
            p.forward_media_data(&source, &synthetic),
            MediaForward::NoWriter,
        );
    }

    // ============================================================
    // S-3-B-tracks step 2 — renegotiation surface
    // ============================================================

    /// `negotiate_if_needed` returns `Ok(None)` when
    /// the peer has no `tracks_out` entries at all —
    /// the post-drain renegotiation pass must not
    /// produce a phantom offer.
    #[test]
    fn negotiate_if_needed_returns_none_when_no_tracks_out() {
        let mut p = MediaPeer::new(pid("alice"), Instant::now());
        let result = p.negotiate_if_needed().expect("no Rtc error");
        assert!(result.is_none(), "no tracks_out → no offer");
        assert!(!p.has_pending_renegotiation());
    }

    /// `negotiate_if_needed` with one ToOpen entry
    /// produces an SDP offer, promotes the entry to
    /// Negotiating(mid), and sets the pending flag.
    #[test]
    fn negotiate_if_needed_promotes_to_negotiating_and_sets_pending() {
        let mut p = MediaPeer::new(pid("alice"), Instant::now());
        let mid = Mid::new();
        p.observe_remote_track_in(
            pid("bob"),
            TrackIn {
                mid,
                kind: MediaKind::Audio,
            },
        );

        let sdp = p
            .negotiate_if_needed()
            .expect("Rtc::sdp_api succeeds")
            .expect("at least one ToOpen entry → Some(sdp)");
        // The produced string is real SDP — must be
        // non-empty and start with the v=0 line that
        // every SDP body begins with.
        assert!(
            sdp.starts_with("v="),
            "produced SDP starts with v=, got {sdp:.80?}"
        );
        assert!(p.has_pending_renegotiation());
        assert!(
            matches!(p.tracks_out()[0].state, TrackOutState::Negotiating(_)),
            "ToOpen → Negotiating(_)"
        );
    }

    /// A second `negotiate_if_needed` call with a
    /// pending offer in flight short-circuits to
    /// `Ok(None)` — preventing concurrent str0m
    /// changes from tangling the SDP state.
    #[test]
    fn negotiate_if_needed_skips_when_pending() {
        let mut p = MediaPeer::new(pid("alice"), Instant::now());
        p.observe_remote_track_in(
            pid("bob"),
            TrackIn {
                mid: Mid::new(),
                kind: MediaKind::Audio,
            },
        );
        let _ = p.negotiate_if_needed().unwrap().unwrap();
        // Second call without applying an answer.
        let result = p.negotiate_if_needed().expect("no Rtc error");
        assert!(result.is_none(), "pending offer → no second offer");
    }

    /// `accept_renegotiation_answer` with no pending
    /// offer must surface a typed error rather than
    /// silently no-op (the peer is replying to a
    /// phantom offer — almost certainly a protocol
    /// violation worth surfacing).
    #[test]
    fn accept_renegotiation_answer_errors_without_pending() {
        let mut p = MediaPeer::new(pid("alice"), Instant::now());
        let err = p
            .accept_renegotiation_answer("v=0\r\n")
            .expect_err("no pending offer → error");
        assert!(matches!(err, MediaError::Str0m(_)));
    }
}
