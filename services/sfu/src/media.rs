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
//!   router-to-MediaPeer integration is [`spawn_media_task`]
//!   alongside the UDP socket loop.
//!
//! ## What does NOT land yet
//!
//! All five steps of S-3-B-routing-media landed. The UDP
//! socket and its tokio loop are [`spawn_media_task`] /
//! `run_media_loop`; RTP fanout is `fanout_media_data`,
//! which walks every non-origin peer and forwards the frame
//! bytes verbatim; simulcast layer selection is
//! `drive_layer_selection` over per-`(Mid, Rid)` bitrate
//! meters; and the in-process synthetic-client tests are
//! `services/sfu/tests/synthetic_client_e2e.rs`. All of it
//! keeps the audit-pack invariant ("payload-opaque router",
//! **C-39**) intact: the SFU never decodes a frame body, and
//! fanout refuses to cross a `call_id` boundary.
//!
//! What genuinely remains is a **runtime** gate rather than
//! code: a real RTP round-trip between two platforms over a
//! **deployed** SFU has never been run (**G-26**). The
//! headless half — six SFrame stacks pinning one golden wire
//! vector — is done.

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use std::collections::HashMap;
use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};

use str0m::bwe::{Bitrate, BweKind};
use str0m::media::{Direction, KeyframeRequestKind, MediaData, MediaKind, Mid, Rid};
use str0m::net::{Protocol, Receive, Transmit};
use str0m::{Candidate, Event, Input, Output, Rtc};
use thiserror::Error;

use crate::frame_descriptor;
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
    /// The media socket has no address the SFU can
    /// advertise as its ICE host candidate — see
    /// [`AdvertiseError`]. Raised by
    /// [`spawn_media_task_advertising`] before the socket is
    /// bound, so a misconfigured SFU never starts a media
    /// plane no client could reach.
    #[error("media advertise address: {0}")]
    AdvertiseAddress(#[from] AdvertiseError),
}

/// Why an address cannot be (or cannot resolve to) the
/// SFU's advertised ICE host candidate.
///
/// The SFU publishes exactly one local candidate per peer,
/// a `typ host` candidate built by `str0m::Candidate::host`.
/// str0m's ICE agent (`is` 0.9) refuses to build one on an
/// unspecified, multicast, broadcast or IPv4 link-local IP,
/// so any of those would leave every SDP answer without an
/// SFU candidate and no client could ever connect media.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AdvertiseError {
    /// The media socket binds an unspecified address
    /// (`0.0.0.0` / `::`) and no advertise IP was supplied.
    /// Binding every interface is fine; advertising "every
    /// interface" is not an address a client can dial.
    #[error(
        "media bind IP {bind} is unspecified and cannot be advertised as an ICE host \
         candidate; supply the IP clients reach this socket on"
    )]
    UnspecifiedBind {
        /// The unspecified bind IP.
        bind: IpAddr,
    },
    /// The bind IP (used as the advertised IP because no
    /// explicit one was supplied) is not a usable host
    /// candidate address.
    #[error("media bind IP {ip} cannot be advertised as an ICE host candidate: {reason}")]
    BindNotAdvertisable {
        /// The rejected IP.
        ip: IpAddr,
        /// Which rule rejected it.
        reason: &'static str,
    },
    /// The explicit advertise IP is not a usable host
    /// candidate address.
    #[error("advertise IP {ip} cannot be an ICE host candidate: {reason}")]
    AdvertiseNotAdvertisable {
        /// The rejected IP.
        ip: IpAddr,
        /// Which rule rejected it.
        reason: &'static str,
    },
    /// The advertise IP is a different address family from
    /// the bind IP. A candidate in one family can never pair
    /// with traffic on a socket of the other, and an
    /// IPv6-bound socket would hand str0m IPv4-mapped source
    /// addresses that match no remote candidate.
    #[error("advertise IP {advertise} is a different address family from media bind IP {bind}")]
    FamilyMismatch {
        /// The bind IP.
        bind: IpAddr,
        /// The advertise IP.
        advertise: IpAddr,
    },
}

/// `None` when `ip` can be an ICE host candidate address,
/// otherwise the rule that rejects it.
///
/// Mirrors the check str0m's ICE agent applies inside
/// `Candidate::host` (unspecified / multicast / broadcast /
/// IPv4 link-local) and additionally refuses IPv6 unicast
/// link-local (`fe80::/10`, which needs a scope id no SDP
/// candidate carries) and IPv4-mapped IPv6 forms of the
/// rejected IPv4 addresses. Loopback is allowed: it is the
/// dev and test configuration.
#[must_use]
pub fn advertisable_ip_problem(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_unspecified() {
                Some("unspecified address")
            } else if v4.is_multicast() {
                Some("multicast address")
            } else if v4.is_broadcast() {
                Some("broadcast address")
            } else if v4.is_link_local() {
                Some("link-local address")
            } else {
                None
            }
        },
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return advertisable_ip_problem(IpAddr::V4(mapped));
            }
            if v6.is_unspecified() {
                Some("unspecified address")
            } else if v6.is_multicast() {
                Some("multicast address")
            } else if (v6.segments()[0] & 0xffc0) == 0xfe80 {
                // fe80::/10 unicast link-local, spelled out
                // because `Ipv6Addr::is_unicast_link_local`
                // is newer than the workspace MSRV.
                Some("link-local address")
            } else {
                None
            }
        },
    }
}

/// Resolve the IP the SFU advertises as its ICE host
/// candidate from the media socket's bind IP and an
/// optional explicit advertise IP.
///
/// - An explicit advertise IP wins (it may differ from a
///   concrete bind IP, e.g. a private interface behind a
///   1:1 NAT), provided it is itself advertisable and the
///   same address family as the bind IP.
/// - Without one, a concrete bind IP is advertised as-is.
/// - Without one, an unspecified bind IP is refused: there
///   is nothing honest to advertise.
///
/// # Errors
///
/// [`AdvertiseError`] naming the rule that refused.
pub fn resolve_advertise_ip(
    bind_ip: IpAddr,
    advertise_ip: Option<IpAddr>,
) -> Result<IpAddr, AdvertiseError> {
    if let Some(ip) = advertise_ip {
        if let Some(reason) = advertisable_ip_problem(ip) {
            return Err(AdvertiseError::AdvertiseNotAdvertisable { ip, reason });
        }
        if ip.is_ipv4() != bind_ip.is_ipv4() {
            return Err(AdvertiseError::FamilyMismatch {
                bind: bind_ip,
                advertise: ip,
            });
        }
        return Ok(ip);
    }
    if bind_ip.is_unspecified() {
        return Err(AdvertiseError::UnspecifiedBind { bind: bind_ip });
    }
    if let Some(reason) = advertisable_ip_problem(bind_ip) {
        return Err(AdvertiseError::BindNotAdvertisable {
            ip: bind_ip,
            reason,
        });
    }
    Ok(bind_ip)
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
    /// Simulcast: the frame belongs to a layer this
    /// downstream track is not currently forwarding. The
    /// SFU sends exactly one simulcast layer per subscriber
    /// (the one selected by [`MediaPeer::select_layer`], or
    /// the first layer observed), so frames of the other
    /// layers are dropped here by design — not a fault.
    WrongLayer,
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
    /// Simulcast layer this downstream track forwards.
    ///
    /// - `None` — no layer chosen yet. A non-simulcast
    ///   source (every frame carries `rid == None`) forwards
    ///   verbatim. For a simulcast source, the FIRST layer
    ///   observed is auto-latched here (see
    ///   [`layer_gate_allows`]).
    /// - `Some(rid)` — forward only this layer's frames;
    ///   frames of every other layer are dropped
    ///   ([`MediaForward::WrongLayer`]). A bandwidth policy
    ///   overrides the latch via
    ///   [`MediaPeer::select_layer`].
    pub chosen_rid: Option<Rid>,
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
    /// The call this peer is admitted to. Set from the
    /// signaling-layer admission (`crate::peer::Peer::call_id`)
    /// when the peer's first SDP offer creates it. Media is
    /// fanned out ONLY between peers sharing a `call_id`
    /// (cross-call isolation — a hard SFU invariant); an
    /// empty value (a peer created by a stray pre-offer ICE
    /// candidate, or a bare test peer) matches no real call.
    call_id: String,
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
    /// Latest egress bandwidth estimate (bits/second) str0m produced for
    /// SENDING to this peer, from `Event::EgressBitrateEstimate`. `None` until
    /// BWE converges. Read by [`drive_layer_selection`] as this subscriber's
    /// per-receiver budget for choosing a simulcast layer.
    egress_estimate_bps: Option<u64>,
    /// Per-incoming-layer bitrate meters, keyed on `(source mid, rid)`. Only
    /// populated for simulcast tracks THIS peer publishes (frames carrying a
    /// rid). [`drive_layer_selection`] reads these to learn each layer's cost
    /// so it can fit a subscriber's budget.
    layer_meters: HashMap<(Mid, Rid), LayerBitrateMeter>,
    /// When the SFU last asked THIS (publishing) peer for a keyframe on each
    /// of its tracks, keyed `(mid, rid)`. Backs the rate limit in
    /// [`MediaPeer::request_keyframe_limited`]: a subscriber that has no
    /// reference frame asks for one about five times a second until it gets
    /// it, and relaying every one of those to the encoder would turn a
    /// missing keyframe into a keyframe storm.
    last_keyframe_request: HashMap<(Mid, Option<Rid>), Instant>,
}

/// Minimum gap between two keyframe requests the SFU relays to one publisher
/// track.
///
/// Chosen, not measured. The bound it has to satisfy from below: the live
/// receiver asked roughly every 200 ms, so anything under that relays every
/// request and limits nothing. From above: this is the extra latency a late
/// joiner can pay beyond the encoder's own, and a picture arriving half a
/// second late is still a picture, where five minutes was not. Two keyframe
/// requests per second per track under a storm is an acceptable worst case
/// for a video encoder that would otherwise be asked five times a second.
pub const KEYFRAME_REQUEST_MIN_INTERVAL: Duration = Duration::from_millis(500);

/// How many complete audio frames str0m may queue behind a missing packet
/// before it releases the stream again.
///
/// str0m hands the SFU whole frames in order. When a packet is missing it
/// holds every later frame until the packet turns up or this many frames
/// have queued behind the gap. Audio carries no retransmission stream, so
/// str0m never asks a publisher to resend an audio packet (`suppress_nack`
/// is set whenever the media line has no RTX): the missing packet never
/// turns up and the wait always runs to its end. With str0m's default of 15
/// that turned every lost 20 ms audio packet into a 300 ms stop followed by
/// fifteen frames at once, and a subscriber's jitter buffer grows to absorb
/// bursts of that size and keeps the delay (found 2026-10-02 on a call with
/// 1-3% packet loss on the publishers' Wi-Fi, heard as close to a second of
/// lag). At 1 the frame after a gap goes out at once: the loss stays a
/// 20 ms loss, which the receiver's concealment covers.
pub const REORDER_AUDIO_FRAMES: usize = 1;

/// The same bound for video: complete frames queued behind a missing packet
/// before str0m gives the frame up and releases what follows.
///
/// Video does have a retransmission stream, and str0m asks for a missing
/// packet again every 33 ms, so a short wait usually ends with the frame
/// whole. The default of 30 frames is a full second at 30 frames a second
/// (longer at the lower rates a publisher drops to after a loss burst), and
/// a packet that is not coming back stalls the picture for all of it. Ten
/// frames is a third of a second at 30 frames a second: room for two
/// resend round trips at the round-trip times seen on the test stack
/// (30-140 ms). Chosen, not measured. A frame released after a gap is not
/// decodable by the subscribers, so the SFU asks the publisher for a
/// keyframe at that moment ([`KeyframeReason::ForwardingGap`]).
pub const REORDER_VIDEO_FRAMES: usize = 10;

/// Whether a frame the SFU is about to forward calls for a keyframe from its
/// publisher: a video frame released with frames missing before it. The
/// subscribers cannot decode past the gap, and without this each of them
/// would first have to notice, wait out its own timeout and send a request
/// of its own.
#[must_use]
pub fn gap_needs_keyframe(kind: Option<MediaKind>, contiguous: bool) -> bool {
    !contiguous && kind == Some(MediaKind::Video)
}

/// Why the SFU is asking a publisher for a keyframe. Carried into the log
/// line so a run shows which path produced the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyframeReason {
    /// A downstream track just opened on a subscriber, which therefore holds
    /// no reference frame for it. Both the late-joiner backfill and the
    /// ordinary forward propagation reach the subscriber's answer the same
    /// way, and both need this.
    DownstreamOpened,
    /// A subscriber sent a PLI or FIR on a downstream track.
    SubscriberRequest,
    /// The publisher's own stream reached the SFU with a frame missing
    /// ([`gap_needs_keyframe`]).
    ForwardingGap,
}

impl KeyframeReason {
    /// Stable, low-cardinality label for logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DownstreamOpened => "downstream-opened",
            Self::SubscriberRequest => "subscriber-pli",
            Self::ForwardingGap => "forwarding-gap",
        }
    }
}

/// One downstream track that a subscriber's renegotiation answer just
/// promoted to `Open`. Returned by [`MediaPeer::accept_renegotiation_answer`]
/// so the caller can ask the publisher for a keyframe at the one moment it
/// becomes useful — after the subscriber can receive it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedTrack {
    /// Publisher of the track.
    pub source_peer: MediaPeerId,
    /// Publisher-side mid.
    pub source_mid: Mid,
    /// Audio or video. Only video needs a keyframe.
    pub kind: MediaKind,
    /// Simulcast layer the subscriber is latched to, if any.
    pub rid: Option<Rid>,
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
        Self::with_stats_interval(id, now, None)
    }

    /// As [`Self::new`], additionally asking str0m for periodic
    /// per-stream statistics (`Event::MediaIngressStats` /
    /// `Event::MediaEgressStats` / `Event::PeerStats`) every
    /// `stats_interval`. `None` keeps str0m silent, which is the
    /// production default; the media loop logs the events when the
    /// operator turns diagnostics on (`LAKKO_SFU_MEDIA_DIAG_SECS`).
    #[must_use]
    pub fn with_stats_interval(
        id: MediaPeerId,
        now: Instant,
        stats_interval: Option<Duration>,
    ) -> Self {
        // Register the generic frame descriptor so it is negotiated in both
        // directions — echoed in the answer to a publisher's offer, and
        // offered in the SFU's own offer to a subscriber. Without it in this
        // session map str0m drops the extension three times over: it is not
        // echoed in the answer, not adopted from the offer, and not parsed
        // off the wire. The SFU carries the value without interpreting it;
        // see [`crate::frame_descriptor`] for what that costs in exposure.
        // Send-side bandwidth estimation is OFF, and with it str0m's pacer.
        // The SFU relays media it cannot re-encode: when the estimate toward a
        // subscriber collapses after a loss burst (60 kbps was seen live on
        // 2026-09-19 after a one-second blackout), the pacer holds a stream it
        // cannot thin far below its rate, the frames queue, the picture stops
        // for 15 s and then arrives in a burst. Unpaced, packets leave as
        // they arrive and a subscriber that is really short of bandwidth asks
        // its publisher for less through its own receiver feedback. The cost
        // is that `Event::EgressBitrateEstimate` never fires, so the
        // simulcast layer policy (`drive_layer_selection`) has no estimate to
        // act on and stays on the latched layer; nothing publishes simulcast
        // today. TWCC feedback toward publishers is a receive-side setting
        // and is unaffected.
        let rtc = Rtc::builder()
            .enable_bwe(None)
            .set_reordering_size_audio(REORDER_AUDIO_FRAMES)
            .set_reordering_size_video(REORDER_VIDEO_FRAMES)
            .set_extension(
                frame_descriptor::GENERIC_FRAME_DESCRIPTOR_ID,
                frame_descriptor::generic_frame_descriptor_extension(),
            )
            .set_stats_interval(stats_interval)
            .build(now);
        Self {
            id,
            call_id: String::new(),
            rtc,
            tracks_in: Vec::new(),
            tracks_out: Vec::new(),
            pending_offer: None,
            egress_estimate_bps: None,
            layer_meters: HashMap::new(),
            last_keyframe_request: HashMap::new(),
        }
    }

    /// The call this peer is admitted to (empty until the
    /// peer's first SDP offer sets it).
    #[must_use]
    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    /// Bind this peer to its call. Idempotent — the
    /// signaling layer sets the same `call_id` on every
    /// offer from the peer. Once a non-empty call is bound,
    /// a later empty value never clears it (defends against
    /// a stray pre-offer command that constructed the peer
    /// with no call).
    pub fn set_call_id(&mut self, call_id: String) {
        if !call_id.is_empty() {
            self.call_id = call_id;
        }
    }

    /// Immutable view of the upstream tracks this peer has
    /// announced via `Event::MediaAdded`. Read-only — the
    /// drain pass owns mutation.
    #[must_use]
    pub fn tracks_in(&self) -> &[TrackIn] {
        &self.tracks_in
    }

    /// The kind of the track this peer publishes on `mid`, when it has
    /// announced one.
    #[must_use]
    pub fn track_in_kind(&self, mid: Mid) -> Option<MediaKind> {
        self.tracks_in.iter().find(|t| t.mid == mid).map(|t| t.kind)
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
            chosen_rid: None,
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
    pub fn accept_renegotiation_answer(&mut self, sdp: &str) -> MediaResult<Vec<OpenedTrack>> {
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
        // answer. Report what opened so the caller can
        // ask each publisher for a keyframe now that this
        // subscriber can actually receive one.
        let mut opened = Vec::new();
        for entry in &mut self.tracks_out {
            if let TrackOutState::Negotiating(mid) = entry.state {
                entry.state = TrackOutState::Open(mid);
                opened.push(OpenedTrack {
                    source_peer: entry.source_peer.clone(),
                    source_mid: entry.source_mid,
                    kind: entry.kind,
                    rid: entry.chosen_rid,
                });
            }
        }
        Ok(opened)
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

    /// Mark the peer dead so the next prune drops it. Called when
    /// `poll_output` fails (str0m does not always flip `is_alive`
    /// itself on an error) and when the peer's signaling socket has
    /// closed, so a departed peer stops receiving fanout, being
    /// offered new tracks, and being backfilled onto joiners.
    pub fn disconnect(&mut self) {
        self.rtc.disconnect();
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
    /// address clients reach the SFU's UDP socket on (the
    /// advertised address, see
    /// [`spawn_media_task_advertising`]). The candidate is
    /// baked into the SDP answer the next call to
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
        // Find the matching Open entry MUTABLY — the simulcast gate latches
        // the chosen layer in place on the first frame it sees.
        let entry = self.tracks_out.iter_mut().find(|entry| {
            entry.source_peer == *source_peer
                && entry.source_mid == data.mid
                && matches!(entry.state, TrackOutState::Open(_))
        });
        let Some(entry) = entry else {
            return MediaForward::NoWriter;
        };
        // Simulcast layer selection: forward only this downstream track's chosen
        // layer (auto-latching the first layer observed; a bandwidth policy may
        // override via `select_layer`). A non-simulcast frame (`rid == None`)
        // always passes.
        if !layer_gate_allows(&mut entry.chosen_rid, data.rid) {
            return MediaForward::WrongLayer;
        }
        let TrackOutState::Open(local_mid) = entry.state else {
            // Unreachable — the `matches!` filter above kept only Open entries.
            return MediaForward::NoWriter;
        };
        // `entry` is no longer used, so its `&mut self.tracks_out` borrow ends
        // and we can take the writer off `self.rtc`.
        let Some(writer) = self.rtc.writer(local_mid) else {
            return MediaForward::NoWriter;
        };
        let Some(pt) = writer.match_params(data.params) else {
            return MediaForward::NoCodecMatch;
        };
        // Carry the publisher's frame descriptor onto the relayed frame. The
        // bytes are opaque to the SFU — it stores and re-emits them, and the
        // serializer recomputes only the two frame-boundary bits the
        // re-packetization invalidates. Without this the value is dropped and
        // the receiver has nothing left to recognise a key frame by, since
        // SFrame has replaced the bitstream header it would otherwise read.
        let writer = match data
            .ext_vals
            .user_values
            .get::<frame_descriptor::FrameDescriptor>()
        {
            Some(descriptor) => writer.user_extension_value(descriptor.clone()),
            None => writer,
        };
        if let Err(e) = writer.write(pt, data.network_time, data.time, data.data.clone()) {
            return MediaForward::WriteFailed(e.to_string());
        }
        MediaForward::Sent
    }

    /// Choose which simulcast layer this peer receives for the downstream track
    /// mirroring `(source_peer, source_mid)`. A bandwidth / quality policy calls
    /// this to override the auto-latched first layer — e.g. downgrade to a lower
    /// layer when the subscriber's egress estimate drops. `None` re-arms the
    /// auto-latch (the next observed layer is chosen). Returns `true` if a
    /// matching downstream track exists.
    pub fn select_layer(
        &mut self,
        source_peer: &MediaPeerId,
        source_mid: Mid,
        rid: Option<Rid>,
    ) -> bool {
        for entry in &mut self.tracks_out {
            if entry.source_peer == *source_peer && entry.source_mid == source_mid {
                entry.chosen_rid = rid;
                return true;
            }
        }
        false
    }

    /// Record one incoming simulcast frame into this (publishing) peer's
    /// per-layer bitrate meters. No-op for a non-simulcast frame (`rid == None`)
    /// — only simulcast layers need per-layer accounting.
    pub fn record_layer_frame(&mut self, data: &MediaData, now: Instant) {
        let Some(rid) = data.rid else {
            return;
        };
        self.layer_meters
            .entry((data.mid, rid))
            .or_insert_with(|| LayerBitrateMeter::new(now))
            .record(data.data.len(), now);
    }

    /// Update this peer's egress bandwidth estimate (bits/second) from a
    /// `Event::EgressBitrateEstimate`.
    pub fn set_egress_estimate_bps(&mut self, bps: u64) {
        self.egress_estimate_bps = Some(bps);
    }

    /// This peer's latest egress estimate (bits/second), if BWE has converged.
    #[must_use]
    pub fn egress_estimate_bps(&self) -> Option<u64> {
        self.egress_estimate_bps
    }

    /// The measured per-layer bitrates for the track `source_mid` this peer
    /// publishes, as `(rid, bits/second)` pairs sorted ASCENDING by bitrate —
    /// the shape [`choose_layer_for_budget`] consumes. Empty when the track is
    /// not simulcast or no window has completed yet.
    #[must_use]
    pub fn layer_bitrates_for(&self, source_mid: Mid) -> Vec<(Rid, u64)> {
        let mut out: Vec<(Rid, u64)> = self
            .layer_meters
            .iter()
            .filter(|((mid, _rid), _)| *mid == source_mid)
            .map(|((_mid, rid), meter)| (*rid, meter.bitrate_bps()))
            .collect();
        out.sort_by_key(|(_, bps)| *bps);
        out
    }

    /// Request a keyframe (PLI) from THIS (publishing) peer for the simulcast
    /// layer `(mid, rid)`. Called when a subscriber switches to this layer so
    /// the new layer starts on a decodable frame instead of showing corruption
    /// until the next natural keyframe. Best-effort: a not-yet-established
    /// stream (no `StreamRx` for the midrid) is a benign no-op. The SFU only
    /// asks for a keyframe on the plaintext RTP layer — it never touches the
    /// SFrame-encrypted payload (C-39).
    pub fn request_layer_keyframe(&mut self, mid: Mid, rid: Rid) {
        if let Some(stream_rx) = self.rtc.direct_api().stream_rx_by_mid(mid, Some(rid)) {
            stream_rx.request_keyframe(KeyframeRequestKind::Pli);
        }
    }

    /// Ask THIS (publishing) peer for a keyframe on `(mid, rid)`, at most once
    /// per [`KEYFRAME_REQUEST_MIN_INTERVAL`] per track. Returns whether a PLI
    /// actually went out, so the caller logs only real requests.
    ///
    /// `rid == None` on a simulcast source means the subscriber has not
    /// latched a layer yet (it latches on the first frame it sees, which is
    /// exactly what it is waiting for). There is no single stream to ask in
    /// that case, so every layer this publisher has been seen sending on is
    /// asked — the subscriber will latch whichever arrives first, and the
    /// others cost one keyframe each, once.
    ///
    /// The distinction from [`Self::request_layer_keyframe`] is deliberate:
    /// the simulcast layer switch is a single deliberate event and keeps its
    /// unlimited path untouched; this one sits behind a subscriber that may
    /// be asking five times a second.
    pub fn request_keyframe_limited(&mut self, mid: Mid, rid: Option<Rid>, now: Instant) -> bool {
        let key = (mid, rid);
        if !keyframe_request_allowed(self.last_keyframe_request.get(&key).copied(), now) {
            return false;
        }
        let mut api = self.rtc.direct_api();
        let sent = match rid {
            Some(r) => api.stream_rx_by_mid(mid, Some(r)).is_some_and(|s| {
                s.request_keyframe(KeyframeRequestKind::Pli);
                true
            }),
            None => {
                if let Some(s) = api.stream_rx_by_mid(mid, None) {
                    s.request_keyframe(KeyframeRequestKind::Pli);
                    true
                } else {
                    // Simulcast: no rid-less stream. Ask every metered layer.
                    let layers: Vec<Rid> = self
                        .layer_meters
                        .keys()
                        .filter(|(m, _)| *m == mid)
                        .map(|(_, r)| *r)
                        .collect();
                    let mut any = false;
                    for r in layers {
                        if let Some(s) = self.rtc.direct_api().stream_rx_by_mid(mid, Some(r)) {
                            s.request_keyframe(KeyframeRequestKind::Pli);
                            any = true;
                        }
                    }
                    any
                }
            },
        };
        if sent {
            self.last_keyframe_request.insert(key, now);
        }
        sent
    }

    /// Resolve one of THIS (subscribing) peer's downstream mids back to the
    /// publisher it mirrors. `None` unless the track is `Open` — a keyframe
    /// request on a track that is still negotiating has no stream behind it
    /// yet and would only reach the encoder before the subscriber can use it.
    #[must_use]
    pub fn origin_for_downstream_mid(&self, local_mid: Mid) -> Option<OpenedTrack> {
        self.tracks_out
            .iter()
            .find(|e| e.state == TrackOutState::Open(local_mid))
            .map(|e| OpenedTrack {
                source_peer: e.source_peer.clone(),
                source_mid: e.source_mid,
                kind: e.kind,
                rid: e.chosen_rid,
            })
    }
}

/// Simulcast layer gate for one downstream track.
///
/// Decides whether a frame with `frame_rid` should be forwarded, given the
/// track's currently-chosen layer `chosen` (mutated in place to auto-latch):
///
/// - `frame_rid == None` — the source is not simulcast (every frame carries no
///   rid), so always forward. `chosen` is left untouched.
/// - `frame_rid == Some(rid)` and `chosen == None` — first simulcast layer seen;
///   latch it (`*chosen = Some(rid)`) and forward.
/// - `frame_rid == Some(rid)` and `chosen == Some(sel)` — forward iff
///   `rid == sel`; otherwise this is a different layer and is dropped.
///
/// A policy overrides the latch by setting `chosen` via
/// [`MediaPeer::select_layer`]; the next non-matching frame is then dropped and
/// only the chosen layer flows.
fn layer_gate_allows(chosen: &mut Option<Rid>, frame_rid: Option<Rid>) -> bool {
    match frame_rid {
        None => true,
        Some(rid) => {
            if chosen.is_none() {
                *chosen = Some(rid);
            }
            *chosen == Some(rid)
        },
    }
}

/// Extract the estimate in bits/second from a str0m [`BweKind`]. Returns `None`
/// for an unrecognised (`#[non_exhaustive]`) future kind — the caller keeps its
/// last known estimate rather than treating the unknown as zero bandwidth.
fn bwe_kind_bps(kind: &BweKind) -> Option<u64> {
    match kind {
        BweKind::Twcc(b) => Some(bitrate_bps(*b)),
        BweKind::Remb(_mid, b) => Some(bitrate_bps(*b)),
        _ => None,
    }
}

/// str0m's `Bitrate` as bits/second (its internal unit; `Bitrate::bps` / `kbps`
/// / `mbps` constructors). Clamped `>= 0` so the policy arithmetic is total.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn bitrate_bps(bitrate: Bitrate) -> u64 {
    // The clamp rules out a negative estimate; a real bandwidth fits u64 with
    // room to spare, so the cast is an exact truncation to whole bits/second.
    let bps = bitrate.as_f64().max(0.0);
    bps as u64
}

/// Windowed bitrate meter for one simulcast layer. Accumulates payload bytes
/// over a fixed window and, when the window rolls over, publishes the completed
/// window's rate in bits/second. Cheap + allocation-free; one instance lives
/// per `(source mid, rid)` on the upstream peer.
#[derive(Debug, Clone)]
struct LayerBitrateMeter {
    /// Measurement window length.
    window: Duration,
    /// Start of the in-progress window.
    window_start: Instant,
    /// Payload bytes seen in the in-progress window.
    bytes: u64,
    /// Rate of the last COMPLETED window (bits/second). `0` until the first
    /// window rolls over.
    last_bps: u64,
}

impl LayerBitrateMeter {
    /// One-second window — long enough to smooth per-frame jitter, short enough
    /// that a layer starting / stopping is reflected within a second.
    const WINDOW: Duration = Duration::from_secs(1);

    fn new(now: Instant) -> Self {
        Self {
            window: Self::WINDOW,
            window_start: now,
            bytes: 0,
            last_bps: 0,
        }
    }

    /// Record `len` payload bytes observed at `now`.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    fn record(&mut self, len: usize, now: Instant) {
        let elapsed = now.saturating_duration_since(self.window_start);
        if elapsed >= self.window {
            let secs = elapsed.as_secs_f64().max(1e-3);
            // Byte counts + bitrates are well within f64's exact-integer range
            // for any realistic media stream; the cast is a truncation to whole
            // bits/second.
            self.last_bps = ((self.bytes as f64 * 8.0) / secs) as u64;
            self.window_start = now;
            self.bytes = 0;
        }
        self.bytes = self.bytes.saturating_add(len as u64);
    }

    /// Rate of the most recently completed window (bits/second).
    fn bitrate_bps(&self) -> u64 {
        self.last_bps
    }
}

/// Fraction of headroom a HIGHER layer must fit under the budget before the
/// policy upgrades to it. Hysteresis: switching up needs margin (so a
/// borderline estimate does not flap), switching down is immediate.
const UPGRADE_HEADROOM_NUM: u64 = 1;
const UPGRADE_HEADROOM_DEN: u64 = 5; // 20 % headroom to upgrade

/// Choose the simulcast layer to send a subscriber, given the measured bitrate
/// of each available layer and the subscriber's egress bandwidth `budget_bps`.
///
/// `layers` MUST be sorted ascending by bitrate. Returns the highest layer that
/// fits the budget — but an UPGRADE past `current` requires the target layer to
/// fit with [`UPGRADE_HEADROOM_NUM`]/[`UPGRADE_HEADROOM_DEN`] headroom, while a
/// DOWNGRADE (or staying) applies immediately (congestion relief must not wait).
/// If no layer fits, the lowest is chosen (better a degraded stream than none).
/// With `layers` empty, `current` is preserved.
fn choose_layer_for_budget(
    layers: &[(Rid, u64)],
    budget_bps: u64,
    current: Option<Rid>,
) -> Option<Rid> {
    if layers.is_empty() {
        return current;
    }
    // Highest layer whose plain bitrate fits the budget (fall back to the
    // lowest layer if even that does not fit).
    let mut best = layers[0].0;
    let mut best_bps = layers[0].1;
    for &(rid, bps) in layers {
        if bps <= budget_bps {
            best = rid;
            best_bps = bps;
        }
    }
    let Some(cur) = current else {
        return Some(best);
    };
    let cur_bps = layers.iter().find(|(r, _)| *r == cur).map(|(_, b)| *b);
    match cur_bps {
        // Upgrading to a higher-bitrate layer than the current one: require
        // headroom so a marginal estimate does not oscillate.
        Some(cb) if best_bps > cb => {
            let needed = best_bps + best_bps * UPGRADE_HEADROOM_NUM / UPGRADE_HEADROOM_DEN;
            if needed <= budget_bps {
                Some(best)
            } else {
                Some(cur)
            }
        },
        // Downgrade or no change — apply immediately.
        _ => Some(best),
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
/// The per-tick fanout logic this type feeds is
/// `fanout_media_data`, driven from `run_media_loop`
/// over the UDP socket [`spawn_media_task`] owns.
#[derive(Debug, Default)]
pub struct MediaRouter {
    peers: Vec<MediaPeer>,
    /// str0m statistics cadence handed to every peer this router
    /// creates lazily; `None` (the default) keeps statistics off.
    stats_interval: Option<Duration>,
}

impl MediaRouter {
    /// Construct an empty router. No call is bound at
    /// this point — the admission path adds peers via
    /// [`Self::add_peer`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask str0m for per-stream statistics on every peer created
    /// from here on (see [`MediaPeer::with_stats_interval`]).
    pub fn set_stats_interval(&mut self, interval: Option<Duration>) {
        self.stats_interval = interval;
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
            self.peers
                .push(MediaPeer::with_stats_interval(id, now, self.stats_interval));
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
        /// The call this peer is admitted to — binds the
        /// media peer to its call so fanout stays
        /// call-scoped (cross-call isolation).
        call_id: String,
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
    /// The peer's signaling socket closed: drop its media state
    /// now rather than when its DTLS or ICE session happens to
    /// time out. Until this existed a peer whose socket closed
    /// before ICE ever connected stayed in the router for the
    /// life of the process: every later track was offered to it,
    /// every frame was fanned out to it, and every joiner was
    /// backfilled against it (five such ghosts sat in one live
    /// four-person call on 2026-09-19). No reply: the caller has
    /// nothing to do with a failure but log it.
    RemovePeer {
        /// The departed peer.
        peer_id: MediaPeerId,
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
        call_id: String,
        offer_sdp: String,
    ) -> std::result::Result<String, MediaCommandError> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.tx
            .try_send(MediaCommand::AcceptOffer {
                peer_id,
                call_id,
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

    /// Send a [`RemovePeer`](MediaCommand::RemovePeer). Fire and
    /// forget: the only failures are the command queue being full
    /// or the task being gone, and the caller can do nothing about
    /// either beyond logging.
    pub fn remove_peer(&self, peer_id: MediaPeerId) -> std::result::Result<(), MediaCommandError> {
        self.tx
            .try_send(MediaCommand::RemovePeer { peer_id })
            .map_err(|e| match e {
                tokio::sync::mpsc::error::TrySendError::Full(_) => MediaCommandError::Backpressure,
                tokio::sync::mpsc::error::TrySendError::Closed(_) => MediaCommandError::TaskGone,
            })
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

/// Spawn the media event-loop task, advertising the bind
/// IP as the SFU's ICE host candidate.
///
/// Equivalent to [`spawn_media_task_advertising`] with no
/// explicit advertise IP, so it refuses an unspecified
/// `bind_addr` (`0.0.0.0` / `::`) — use the advertising
/// variant for that.
///
/// # Errors
///
/// As [`spawn_media_task_advertising`].
pub async fn spawn_media_task(
    bind_addr: SocketAddr,
    peers: PeerRegistry,
) -> MediaResult<(MediaTaskHandle, SocketAddr)> {
    spawn_media_task_advertising(bind_addr, None, peers, None).await
}

/// Spawn the media event-loop task. Binds a
/// `tokio::net::UdpSocket` at `bind_addr` (typically
/// the value of `LAKKO_SFU_MEDIA_BIND`), resolves the
/// address the SFU advertises as its ICE host candidate,
/// and returns the handle the signaling layer uses to push
/// commands together with that **advertised** address.
///
/// The advertised address is `advertise_ip` (typically
/// `LAKKO_SFU_MEDIA_ADVERTISE_IP`) or, when that is `None`,
/// the bind IP — combined with the port the socket actually
/// bound (so `:0` resolves to the OS-picked port). For a
/// concrete bind IP without an advertise IP it equals the
/// socket's local address. See [`resolve_advertise_ip`] for
/// the refusal rules; resolution happens before the socket
/// is bound.
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
/// - [`MediaError::AdvertiseAddress`] when no advertisable
///   host-candidate address can be resolved (an unspecified
///   bind without an advertise IP, an unusable advertise
///   IP, or an address-family mismatch).
/// - [`MediaError::Str0m`] on UDP-bind failure (wrong
///   permission, port already in use, etc.).
pub async fn spawn_media_task_advertising(
    bind_addr: SocketAddr,
    advertise_ip: Option<IpAddr>,
    peers: PeerRegistry,
    diagnostics: Option<Duration>,
) -> MediaResult<(MediaTaskHandle, SocketAddr)> {
    let host_ip = resolve_advertise_ip(bind_addr.ip(), advertise_ip)?;
    let socket = tokio::net::UdpSocket::bind(bind_addr)
        .await
        .map_err(|e| MediaError::Str0m(format!("UDP bind {bind_addr}: {e}")))?;
    let local_addr = socket
        .local_addr()
        .map_err(|e| MediaError::Str0m(format!("UDP local_addr: {e}")))?;
    let advertise_addr = SocketAddr::new(host_ip, local_addr.port());

    let (tx, rx) = tokio::sync::mpsc::channel::<MediaCommand>(MEDIA_COMMAND_CAPACITY);
    tokio::spawn(run_media_loop(
        socket,
        advertise_addr,
        rx,
        peers,
        diagnostics,
    ));
    Ok((MediaTaskHandle { tx }, advertise_addr))
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
/// `advertise_addr` is `Some`, the per-peer host candidate
/// is registered before [`MediaPeer::accept_offer`] so
/// the answer's `a=candidate:` line points at the address
/// clients reach the SFU's socket on.
fn handle_command(router: &mut MediaRouter, cmd: MediaCommand, advertise_addr: Option<SocketAddr>) {
    match cmd {
        MediaCommand::AcceptOffer {
            peer_id,
            call_id,
            offer_sdp,
            reply,
        } => {
            let peer_id_for_backfill = peer_id.clone();
            let peer = router.peer_or_insert(peer_id, Instant::now());
            // Bind the peer to its call so fanout stays
            // call-scoped (cross-call isolation). Idempotent
            // across the peer's offers.
            peer.set_call_id(call_id);
            // The host candidate is the SFU's ONLY local
            // candidate, so an answer without it can never
            // connect media. Refuse the offer and log at
            // error level rather than shipping an answer
            // the client would only discover is dead after
            // an ICE timeout. `spawn_media_task_advertising`
            // resolves an advertisable address up front, so
            // reaching this branch means that validation and
            // str0m's candidate rules have drifted apart.
            if let Some(addr) = advertise_addr {
                if let Err(e) = peer.add_local_host_candidate(addr) {
                    tracing::error!(
                        peer = %peer.id(),
                        candidate_addr = %addr,
                        error = %e,
                        "media: SFU host candidate rejected; refusing SDP offer \
                         (an answer without it could never connect media)"
                    );
                    let _ = reply.send(Err(e));
                    return;
                }
            }
            let res = peer.accept_offer(&offer_sdp);
            // Lifecycle point 0. The four downstream lines describe how the
            // SFU routes tracks BETWEEN peers; they say nothing about the
            // answer to a peer's own offer — and that answer alone can hand
            // the peer a remote track. str0m sets its local direction to
            // `m.direction().invert()` (`change/sdp.rs:1285`), so a client
            // that offers a `sendrecv` m-line is answered `sendrecv` and its
            // libwebrtc raises `ontrack` for a receiver on its OWN send
            // m-line — one the SFU never points a downstream track at, so it
            // can never carry media. Logging the answered direction and msid
            // per m-line is what distinguishes that phantom from a real
            // subscription: a real downstream track's stream id is the source
            // peer's id, while this one is a random 30-char msid.
            if let Ok(answer) = &res {
                tracing::info!(
                    peer = %peer_id_for_backfill,
                    media = %media_line_summary(answer),
                    "media: answered peer's own offer"
                );
            }
            let ok = res.is_ok();
            // If the receiver dropped, the originating
            // future has bailed — ignore the send
            // failure.
            let _ = reply.send(res);
            // Give this peer the tracks the call already has. Announcement
            // only ever propagates FORWARD — see `backfill_existing_tracks`
            // for why that leaves a late joiner with nothing.
            if ok {
                backfill_existing_tracks(router, &peer_id_for_backfill);
            }
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
            // Lifecycle point 4 of 4, and the one that separates the two
            // failure shapes this logging exists for. Point 3 present and
            // this absent means the subscriber never answered — it refused
            // the offer, or never received it. Both present and still no
            // picture means the track is open and the fault is downstream,
            // in media rather than in negotiation.
            match &res {
                Ok(opened) => {
                    tracing::info!(
                        peer = %peer_id,
                        opened = opened.len(),
                        "media: subscriber answered the downstream-track offer"
                    );
                    // The subscriber can receive now, and holds no reference
                    // frame for anything that just opened. Ask each video
                    // publisher for a keyframe at this moment rather than at
                    // open time — asked earlier it lands before the
                    // subscriber exists to decode it, and the subscriber is
                    // left waiting for the publisher's next natural keyframe,
                    // which in a live call took five minutes.
                    let now = Instant::now();
                    for track in opened {
                        if track.kind != MediaKind::Video {
                            continue;
                        }
                        request_keyframe_from_publisher(
                            router,
                            track,
                            now,
                            KeyframeReason::DownstreamOpened,
                        );
                    }
                },
                Err(e) => tracing::warn!(
                    peer = %peer_id,
                    error = %e,
                    "media: subscriber's answer to the downstream-track offer was refused"
                ),
            }
            let _ = reply.send(res.map(|_| ()));
        },
        MediaCommand::RemovePeer { peer_id } => {
            let removed = router.remove_peer(&peer_id);
            tracing::info!(
                peer = %peer_id,
                removed,
                peers_left = router.len(),
                "media: peer removed after its signaling socket closed"
            );
        },
    }
}

/// The main event loop. Lives in its own function so it
/// can be unit-tested in isolation (future sub-sprint:
/// a synthetic-client integration test in step 5).
///
/// `advertise_addr` is the address published as every
/// peer's host candidate (see
/// [`spawn_media_task_advertising`]), NOT necessarily the
/// socket's local address.
async fn run_media_loop(
    socket: tokio::net::UdpSocket,
    advertise_addr: SocketAddr,
    mut rx: tokio::sync::mpsc::Receiver<MediaCommand>,
    peers: PeerRegistry,
    diagnostics: Option<Duration>,
) {
    let mut router = MediaRouter::new();
    router.set_stats_interval(diagnostics);
    // Per-destination fanout tallies, flushed to the log on the
    // diagnostics cadence. Off (no bookkeeping at all) unless the
    // operator asked for it.
    let mut ledger = diagnostics.map(|every| FanoutLedger::new(every, Instant::now()));
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
        fanout_media_data_into(&mut router, &summary.pending, ledger.as_mut());
        if let Some(l) = ledger.as_mut() {
            l.flush_if_due(now);
        }
        // Subscribers' PLIs, resolved during the drain to the publisher
        // track each mirrors, go to the publishers now — rate-limited per
        // track, since a subscriber with no reference frame asks about five
        // times a second until it has one.
        relay_keyframe_requests(&mut router, &summary.keyframe_requests, now);
        // Per-receiver simulcast layer selection: match each subscriber's
        // egress bandwidth estimate against the measured per-layer bitrates,
        // (re)select the best-fitting layer, and request a keyframe on any
        // switch for the next tick.
        let layer_switches = drive_layer_selection(&mut router);
        record_layer_selection_metrics(&layer_switches);
        let sleep_for = summary
            .earliest_deadline
            .saturating_duration_since(now)
            .min(MAX_TICK_INTERVAL);

        tokio::select! {
            biased; // prioritise commands so a fast signaling layer is not starved by busy traffic
            cmd = rx.recv() => {
                match cmd {
                    Some(cmd) => handle_command(&mut router, cmd, Some(advertise_addr)),
                    None => return, // all senders dropped — task is done
                }
            }
            recv = socket.recv_from(&mut buf) => {
                if let Ok((n, source)) = recv {
                    let Ok(contents) = (buf[..n]).try_into() else {
                        continue;
                    };
                    // `destination` MUST be the advertised
                    // host-candidate address, not the socket's
                    // local address. str0m's ICE agent (`is`
                    // 0.9, `stun_server_handle_request`) drops
                    // an inbound STUN binding request unless
                    // some Host/Relayed local candidate has
                    // `addr() == destination` — so with an
                    // unspecified bind (local addr
                    // `0.0.0.0:port`) or a NATed bind (local
                    // addr private, candidate public) every
                    // connectivity check would be discarded as
                    // "unknown interface" and ICE could never
                    // nominate a pair. This is one UDP socket
                    // carrying one candidate, so every datagram
                    // it receives is, by construction, traffic
                    // for that candidate. (`Transmit::source`
                    // is the mirror image and is ignored by
                    // `write_transmit`: `send_to` lets the OS
                    // pick the egress address.)
                    let input = Input::Receive(
                        Instant::now(),
                        Receive {
                            proto: Protocol::Udp,
                            source,
                            destination: advertise_addr,
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

/// One line per publisher stream per stats interval. The three
/// statistics events exist only when the peer was built with a stats
/// interval, i.e. when `LAKKO_SFU_MEDIA_DIAG_SECS` is set; the
/// counters are cumulative as str0m keeps them.
fn log_ingress_stats(peer: &MediaPeerId, st: &str0m::stats::MediaIngressStats) {
    tracing::info!(
        peer = %peer,
        mid = ?st.mid,
        rid = ?st.rid,
        bytes = st.bytes,
        packets = st.packets,
        plis = st.plis,
        nacks = st.nacks,
        loss = ?st.loss,
        rtt_ms = ?st.rtt.map(|d| d.as_millis()),
        "media: ingress stats"
    );
}

/// One line per downstream stream per stats interval, naming the
/// publisher track it mirrors so a reader can pair it with that
/// publisher's ingress line.
fn log_egress_stats(
    peer: &MediaPeerId,
    media_peer: &MediaPeer,
    st: &str0m::stats::MediaEgressStats,
) {
    let upstream = media_peer.origin_for_downstream_mid(st.mid);
    tracing::info!(
        peer = %peer,
        mid = ?st.mid,
        rid = ?st.rid,
        origin = ?upstream.as_ref().map(|t| t.source_peer.to_string()),
        origin_mid = ?upstream.as_ref().map(|t| t.source_mid),
        bytes = st.bytes,
        packets = st.packets,
        plis = st.plis,
        nacks = st.nacks,
        loss = ?st.loss,
        rtt_ms = ?st.rtt.map(|d| d.as_millis()),
        "media: egress stats"
    );
}

/// One line per peer per stats interval: the transport-level view,
/// including the egress bandwidth estimate the layer policy reads.
fn log_peer_stats(peer: &MediaPeerId, st: &str0m::stats::PeerStats) {
    tracing::info!(
        peer = %peer,
        bwe_tx = ?st.bwe_tx,
        bytes_rx = st.bytes_rx,
        bytes_tx = st.bytes_tx,
        loss_in = ?st.ingress_loss_fraction,
        loss_out = ?st.egress_loss_fraction,
        rtt_ms = ?st.rtt.map(|d| d.as_millis()),
        "media: peer stats"
    );
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
    /// Keyframe requests (PLI / FIR) subscribers sent on
    /// their downstream tracks, already resolved to the
    /// publisher track they mirror. Relayed to the
    /// publishers, rate-limited, after the drain.
    pub(crate) keyframe_requests: Vec<OpenedTrack>,
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
    let mut keyframe_requests: Vec<OpenedTrack> = Vec::new();
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
                        // Meter this layer's bitrate (simulcast frames only)
                        // before the frame moves into the fanout queue — the
                        // per-receiver policy reads these rates.
                        peer.record_layer_frame(&data, now);
                        // A video frame released with frames missing before
                        // it: ask this publisher for a keyframe now rather
                        // than after every subscriber has timed out.
                        if gap_needs_keyframe(peer.track_in_kind(data.mid), data.contiguous)
                            && peer.request_keyframe_limited(data.mid, data.rid, now)
                        {
                            tracing::info!(
                                origin = %origin,
                                mid = ?data.mid,
                                rid = ?data.rid,
                                reason = KeyframeReason::ForwardingGap.as_str(),
                                "media: keyframe requested from publisher"
                            );
                        }
                        pending.push(PendingFanout {
                            origin: origin.clone(),
                            data,
                        });
                    },
                    Event::EgressBitrateEstimate(kind) => {
                        // Per-receiver bandwidth signal for the simulcast layer
                        // policy — this is the estimate for SENDING to `peer`.
                        if let Some(bps) = bwe_kind_bps(&kind) {
                            peer.set_egress_estimate_bps(bps);
                        }
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
                        // Lifecycle point 1 of 4. A publisher announced a
                        // track. If a "no video" report has no line here, the
                        // publisher never got its media line negotiated and
                        // nothing downstream is worth looking at.
                        tracing::info!(
                            peer = %origin,
                            call = %peer.call_id(),
                            mid = ?added.mid,
                            kind = ?added.kind,
                            direction = ?added.direction,
                            "media: track announced by peer"
                        );
                        new_tracks.push(PendingTrackIn {
                            origin: origin.clone(),
                            track,
                        });
                    },
                    Event::KeyframeRequest(req) => {
                        // A subscriber has no decodable frame on one of ITS
                        // downstream tracks and is asking for one. The SFU
                        // holds no frames, so the only thing that can answer
                        // is the publisher — map the subscriber's local mid
                        // back to the publisher track it mirrors and relay.
                        // Resolved here, against this peer's `tracks_out`,
                        // because the relay itself needs a mutable borrow of
                        // a different peer.
                        if let Some(target) = peer.origin_for_downstream_mid(req.mid) {
                            keyframe_requests.push(target);
                        }
                    },
                    // The three statistics events exist only when the
                    // peer was built with a stats interval, i.e. when
                    // diagnostics are on; each is one line per stream
                    // per interval, cumulative counters as str0m keeps
                    // them.
                    // Transport state, always on: a subscriber that stops
                    // receiving for seconds at a time is either paced or
                    // disconnected, and only this line tells the two apart.
                    Event::IceConnectionStateChange(state) => {
                        tracing::info!(peer = %origin, state = ?state, "media: ICE connection state");
                    },
                    Event::Connected => {
                        tracing::info!(peer = %origin, "media: peer transport connected");
                    },
                    Event::MediaIngressStats(st) => log_ingress_stats(&origin, &st),
                    Event::MediaEgressStats(st) => log_egress_stats(&origin, peer, &st),
                    Event::PeerStats(st) => log_peer_stats(&origin, &st),
                    // Other events (ICE state, MediaChanged,
                    // ChannelOpen, ...) are not actionable for the
                    // SFrame-passthrough router yet.
                    _ => {},
                },
                Err(e) => {
                    // The message has always said "dropping"; the
                    // prune at the loop tail keys on `is_alive`, which
                    // str0m does not flip for every error, so flip it
                    // here or the peer lives on and every later write
                    // to it fails.
                    tracing::warn!(peer = %origin, error = %e, "media: poll_output failed; dropping peer");
                    peer.disconnect();
                    break;
                },
            }
        }
    }
    DrainSummary {
        earliest_deadline,
        pending,
        new_tracks,
        keyframe_requests,
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
    /// Simulcast: frames dropped because they belong to a
    /// layer this subscriber is not currently receiving. This
    /// is the SFU doing its job (one layer per subscriber),
    /// not a fault — but a spike is worth seeing (e.g. a
    /// publisher sending three layers to an audience that all
    /// picked the lowest).
    pub(crate) wrong_layer: u64,
}

impl FanoutOutcome {
    /// Total forward attempts this pass = sum of all
    /// outcome buckets = `frames × non-origin peers`.
    pub(crate) fn attempted(&self) -> u64 {
        self.sent + self.no_writer + self.no_codec_match + self.write_failed + self.wrong_layer
    }

    /// File one forward result into its bucket.
    pub(crate) fn tally(&mut self, forwarded: &MediaForward) {
        match forwarded {
            MediaForward::Sent => self.sent += 1,
            MediaForward::NoWriter => self.no_writer += 1,
            MediaForward::NoCodecMatch => self.no_codec_match += 1,
            MediaForward::WrongLayer => self.wrong_layer += 1,
            MediaForward::WriteFailed(_) => self.write_failed += 1,
        }
    }
}

/// What one publisher stream produced over a ledger interval.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OriginTally {
    /// Frames the SFU depacketized from this stream.
    pub(crate) frames: u64,
    /// Payload bytes across those frames.
    pub(crate) bytes: u64,
    /// Frames whose relayed descriptor marks them independent, i.e.
    /// key frames. Only a video stream carrying the generic frame
    /// descriptor can count here; audio and descriptor-less video
    /// stay at zero, which the log reader must not read as
    /// "no key frames were sent".
    pub(crate) key_frames: u64,
}

/// Per-destination fanout bookkeeping, logged and reset on a fixed
/// cadence. Exists because the Prometheus counters answer "how many
/// frames were dropped" and not "whose frames, to whom" — the
/// question a call with picture on one device and none on another
/// asks. Keys carry the origin's mid, so a publisher's audio and
/// video are told apart.
#[derive(Debug)]
pub(crate) struct FanoutLedger {
    every: Duration,
    last_flush: Instant,
    /// `(destination, origin, origin mid)` -> outcome buckets.
    per_target: HashMap<(MediaPeerId, MediaPeerId, Mid), FanoutOutcome>,
    /// `(origin, origin mid)` -> what the publisher produced.
    per_origin: HashMap<(MediaPeerId, Mid), OriginTally>,
}

impl FanoutLedger {
    /// A ledger that flushes every `every`, counting from `now`.
    pub(crate) fn new(every: Duration, now: Instant) -> Self {
        Self {
            every,
            last_flush: now,
            per_target: HashMap::new(),
            per_origin: HashMap::new(),
        }
    }

    /// File one depacketized frame against its publisher stream.
    pub(crate) fn record_origin(&mut self, origin: &MediaPeerId, data: &MediaData) {
        let tally = self
            .per_origin
            .entry((origin.clone(), data.mid))
            .or_default();
        tally.frames += 1;
        tally.bytes += data.data.len() as u64;
        let independent = data
            .ext_vals
            .user_values
            .get::<frame_descriptor::FrameDescriptor>()
            .and_then(|d| frame_descriptor::is_independent_frame(&d.0));
        if independent == Some(true) {
            tally.key_frames += 1;
        }
    }

    /// File one forward attempt.
    pub(crate) fn record(
        &mut self,
        target: &MediaPeerId,
        origin: &MediaPeerId,
        origin_mid: Mid,
        forwarded: &MediaForward,
    ) {
        self.per_target
            .entry((target.clone(), origin.clone(), origin_mid))
            .or_default()
            .tally(forwarded);
    }

    /// `true` once a full interval has passed since the last flush.
    pub(crate) fn is_due(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_flush) >= self.every
    }

    /// Log everything filed since the last flush and start over.
    /// Returns the number of lines written, `0` when not yet due.
    pub(crate) fn flush_if_due(&mut self, now: Instant) -> usize {
        if !self.is_due(now) {
            return 0;
        }
        self.last_flush = now;
        let mut lines = 0;
        let mut origins: Vec<_> = self.per_origin.drain().collect();
        origins.sort_by(|a, b| a.0.cmp(&b.0));
        for ((origin, mid), t) in origins {
            tracing::info!(
                origin = %origin,
                mid = ?mid,
                frames = t.frames,
                bytes = t.bytes,
                key_frames = t.key_frames,
                "media: publisher stream over the last interval"
            );
            lines += 1;
        }
        let mut targets: Vec<_> = self.per_target.drain().collect();
        targets.sort_by(|a, b| a.0.cmp(&b.0));
        for ((target, origin, mid), o) in targets {
            tracing::info!(
                target = %target,
                origin = %origin,
                origin_mid = ?mid,
                sent = o.sent,
                no_writer = o.no_writer,
                no_codec_match = o.no_codec_match,
                write_failed = o.write_failed,
                wrong_layer = o.wrong_layer,
                "media: fanout to subscriber over the last interval"
            );
            lines += 1;
        }
        lines
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
    if outcome.wrong_layer > 0 {
        metrics::counter!(MEDIA_FRAMES_DROPPED_TOTAL, "reason" => "wrong_layer")
            .increment(outcome.wrong_layer);
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
#[cfg(test)]
pub(crate) fn fanout_media_data(
    router: &mut MediaRouter,
    pending: &[PendingFanout],
) -> FanoutOutcome {
    fanout_media_data_into(router, pending, None)
}

/// As [`fanout_media_data`], additionally filing every attempt into
/// `ledger` (per destination, origin and origin mid) when one is
/// given. The ledger is the diagnostic view the aggregate counters
/// cannot give: which subscriber a publisher's frames do and do not
/// reach, and whether the publisher's video carries key frames.
pub(crate) fn fanout_media_data_into(
    router: &mut MediaRouter,
    pending: &[PendingFanout],
    mut ledger: Option<&mut FanoutLedger>,
) -> FanoutOutcome {
    let mut outcome = FanoutOutcome::default();
    if pending.is_empty() || router.len() < 2 {
        return outcome;
    }
    for event in pending {
        if let Some(l) = ledger.as_deref_mut() {
            l.record_origin(&event.origin, &event.data);
        }
        // Cross-call isolation: media is forwarded ONLY to
        // peers in the SAME call as the origin. Look up the
        // origin's call once; if the origin is gone (pruned)
        // or unbound (empty call), forward nothing. A peer in
        // a different call NEVER receives this frame.
        let origin_call = router
            .peers()
            .iter()
            .find(|p| p.id() == &event.origin)
            .map(|p| p.call_id().to_owned());
        let Some(origin_call) = origin_call.filter(|c| !c.is_empty()) else {
            continue;
        };
        // Walk the peer list manually so we can borrow
        // each non-origin peer mutably one at a time.
        let n = router.len();
        for idx in 0..n {
            // Borrow check: re-borrow per-iteration so we
            // can compare ids + call before touching the
            // writer. Skip the origin AND any peer not in the
            // origin's call.
            let skip = router.peers().get(idx).map_or(true, |p| {
                p.id() == &event.origin || p.call_id() != origin_call
            });
            if skip {
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
            let forwarded = peer.forward_media_data(&event.origin, &event.data);
            if let Some(l) = ledger.as_deref_mut() {
                l.record(peer.id(), &event.origin, event.data.mid, &forwarded);
            }
            match forwarded {
                MediaForward::Sent => outcome.sent += 1,
                MediaForward::NoWriter => outcome.no_writer += 1,
                MediaForward::NoCodecMatch => outcome.no_codec_match += 1,
                MediaForward::WrongLayer => outcome.wrong_layer += 1,
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

/// Open every track the call already has onto `joiner`.
///
/// ## The gap this closes
///
/// [`propagate_new_tracks_in`] is the only producer of downstream tracks,
/// and it runs off `Event::MediaAdded` — a track being **announced**. It
/// opens that track onto every peer present *at that moment*. Nothing ran
/// in the other direction, so a peer that joined a call already in progress
/// was given only what was published **after** it arrived. In a two-person
/// call where the first participant publishes and the second joins later,
/// the second received nothing at all, for the whole call.
///
/// The asymmetry hid for a long time because it is invisible whenever the
/// publisher is the one who arrives last, which is the shape every test in
/// this crate happened to have.
///
/// Idempotent: `observe_remote_track_in` de-dupes on
/// `(source_peer, source_mid)`, so calling this on every offer from a peer —
/// including its renegotiations — only ever adds what is genuinely missing.
/// Cross-call isolation is enforced here exactly as in the fanout: only
/// peers sharing a non-empty `call_id` with the joiner are considered.
fn backfill_existing_tracks(router: &mut MediaRouter, joiner: &MediaPeerId) {
    let Some(call) = router
        .peers()
        .iter()
        .find(|p| p.id() == joiner)
        .map(|p| p.call_id().to_owned())
        .filter(|c| !c.is_empty())
    else {
        return;
    };

    // Snapshot first: the write below needs a mutable borrow of the joiner,
    // so the sources cannot be read through `router` at the same time.
    let sources: Vec<(MediaPeerId, TrackIn)> = router
        .peers()
        .iter()
        .filter(|p| p.id() != joiner && p.call_id() == call)
        .flat_map(|p| {
            let id = p.id().clone();
            p.tracks_in().iter().map(move |t| (id.clone(), *t))
        })
        .collect();
    if sources.is_empty() {
        return;
    }

    let Some(peer) = router.peers_mut().iter_mut().find(|p| p.id() == joiner) else {
        return;
    };
    for (origin, track) in sources {
        if peer.observe_remote_track_in(origin.clone(), track) {
            // Same lifecycle point 2 as `propagate_new_tracks_in`, reached by
            // the other route. A joiner's subscriptions appear here rather
            // than there, which is the difference between "the call had
            // nothing to give it" and "the call had tracks and withheld them".
            tracing::info!(
                origin = %origin,
                target = %joiner,
                call = %call,
                mid = ?track.mid,
                kind = ?track.kind,
                "media: opened pre-existing track on joining subscriber"
            );
        }
    }
}

/// The rate limit's decision, kept pure so it can be pinned without a live
/// stream: a request is allowed when none has gone out for this track, or the
/// last one is at least [`KEYFRAME_REQUEST_MIN_INTERVAL`] old. A clock that
/// reads earlier than the last request (a monotonic clock cannot, but the
/// arithmetic should not care) saturates to zero elapsed and denies.
fn keyframe_request_allowed(last: Option<Instant>, now: Instant) -> bool {
    last.map_or(true, |l| {
        now.saturating_duration_since(l) >= KEYFRAME_REQUEST_MIN_INTERVAL
    })
}

/// Ask the publisher of `track` for a keyframe, rate-limited per publisher
/// track, and log it when one actually goes out. Returns whether it did.
///
/// The SFU holds no frames of its own, so a subscriber that lacks a reference
/// frame can only be helped by the publisher producing a new one. This is
/// the single path both reasons go through, so the rate limit is shared: a
/// track that was just asked because it opened is not asked again a moment
/// later because the subscriber's own PLI arrived before the keyframe did.
pub(crate) fn request_keyframe_from_publisher(
    router: &mut MediaRouter,
    track: &OpenedTrack,
    now: Instant,
    reason: KeyframeReason,
) -> bool {
    let Some(publisher) = router.peer_mut(&track.source_peer) else {
        return false;
    };
    let sent = publisher.request_keyframe_limited(track.source_mid, track.rid, now);
    if sent {
        // Lifecycle point 5. With the four negotiation lines showing a track
        // open and this showing the request go out, a "first frame" line on
        // the subscriber is expected within the encoder's keyframe latency —
        // not at the publisher's next natural keyframe, whenever that is.
        tracing::info!(
            origin = %track.source_peer,
            mid = ?track.source_mid,
            rid = ?track.rid,
            reason = reason.as_str(),
            "media: keyframe requested from publisher"
        );
    }
    sent
}

/// Relay every subscriber keyframe request captured during a drain to its
/// publisher. Duplicates within one tick — several subscribers asking for the
/// same track, or one subscriber asking repeatedly — collapse through the
/// per-track rate limit rather than being de-duplicated here, so the same
/// rule governs requests that arrive in one tick and across ticks.
pub(crate) fn relay_keyframe_requests(
    router: &mut MediaRouter,
    requests: &[OpenedTrack],
    now: Instant,
) -> usize {
    let mut relayed = 0;
    for req in requests {
        if req.kind != MediaKind::Video {
            continue;
        }
        if request_keyframe_from_publisher(router, req, now, KeyframeReason::SubscriberRequest) {
            relayed += 1;
        }
    }
    relayed
}

/// Condense an SDP's media lines into one field for a log line: per m-section
/// the kind, the direction, and the msid stream id.
///
/// The direction is the load-bearing part. A `sendrecv` answer to a client's
/// `sendrecv` offer gives that client a receiver on its own send m-line,
/// which looks to the client exactly like a subscription and is not one.
///
/// Diagnostic only. Nothing branches on this.
fn media_line_summary(sdp: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    let push = |current: &mut Option<String>, parts: &mut Vec<String>| {
        if let Some(c) = current.take() {
            parts.push(c);
        }
    };
    for line in sdp.lines() {
        if let Some(rest) = line.strip_prefix("m=") {
            push(&mut current, &mut parts);
            let kind = rest.split_whitespace().next().unwrap_or("?");
            current = Some(format!("[{kind}"));
        } else if let Some(c) = current.as_mut() {
            for dir in ["sendrecv", "sendonly", "recvonly", "inactive"] {
                if line == format!("a={dir}") {
                    c.push(' ');
                    c.push_str(dir);
                }
            }
            if let Some(msid) = line.strip_prefix("a=msid:") {
                if let Some(stream) = msid.split_whitespace().next() {
                    c.push_str(" msid=");
                    c.push_str(stream);
                }
            }
        }
    }
    push(&mut current, &mut parts);
    parts
        .into_iter()
        .map(|mut p| {
            p.push(']');
            p
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Condense an SDP's `a=extmap` lines into one field for a log line.
///
/// Renders `m=` sections as a separator and each extension as `id:name`,
/// where the name is the last path segment of the URI — `mid`,
/// `generic-frame-descriptor-00`, `abs-send-time`. Full URIs would make the
/// line unreadable and are not what a reader is checking: the question is
/// always which id a given extension landed on.
///
/// Diagnostic only. Nothing branches on this.
fn extmap_summary(sdp: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for line in sdp.lines() {
        if let Some(rest) = line.strip_prefix("m=") {
            let kind = rest.split_whitespace().next().unwrap_or("?");
            parts.push(format!("[{kind}]"));
        } else if let Some(rest) = line.strip_prefix("a=extmap:") {
            let mut fields = rest.split_whitespace();
            let Some(id) = fields.next() else {
                continue;
            };
            let name = fields.next().map_or("?", |uri| {
                uri.rsplit(['/', ':'])
                    .next()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(uri)
            });
            parts.push(format!("{id}:{name}"));
        }
    }
    parts.join(" ")
}

/// Tally of the simulcast layer switches [`drive_layer_selection`] made this
/// tick. Returned so the media task + tests observe the policy's decisions
/// without scraping the global metrics registry; the driver also mirrors it into
/// the `lakko_sfu_simulcast_layer_switches_total` counter. Only counts a genuine
/// bandwidth-driven flip (a track that already had a chosen layer) — the initial
/// latch of a track's first layer is not a "switch".
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LayerSelectionSummary {
    /// Switches to a higher-bitrate layer (recovery).
    pub(crate) upgrades: u64,
    /// Switches to a lower-bitrate layer (congestion).
    pub(crate) downgrades: u64,
}

/// Mirror a [`LayerSelectionSummary`] into the
/// `lakko_sfu_simulcast_layer_switches_total` counter, labelled `direction`.
/// Each bucket is only touched when non-zero so an idle tick creates no series.
fn record_layer_selection_metrics(summary: &LayerSelectionSummary) {
    use crate::prom::names::SIMULCAST_LAYER_SWITCHES_TOTAL;
    if summary.upgrades > 0 {
        metrics::counter!(SIMULCAST_LAYER_SWITCHES_TOTAL, "direction" => "up")
            .increment(summary.upgrades);
    }
    if summary.downgrades > 0 {
        metrics::counter!(SIMULCAST_LAYER_SWITCHES_TOTAL, "direction" => "down")
            .increment(summary.downgrades);
    }
}

/// Per-tick simulcast layer selection driver (G3-3).
///
/// For each subscriber peer with a converged egress bandwidth estimate, and for
/// each downstream simulcast track it receives, choose the best-fitting layer
/// via [`choose_layer_for_budget`] and apply it through
/// [`MediaPeer::select_layer`] when it changes — congestion downgrades
/// immediately, recovery upgrades with headroom hysteresis. A no-op for a peer
/// without an estimate yet, or a track with fewer than two measured layers
/// (audio / non-simulcast video). The two-pass shape (snapshot every
/// publisher's per-layer bitrates, then mutate each subscriber, then request a
/// keyframe from each publisher whose layer changed) avoids a cross-peer borrow
/// conflict. On every switch the SFU asks the publisher for a keyframe (PLI) for
/// the newly-selected layer so the switch starts on a decodable frame instead of
/// corruption until the next natural keyframe. Returns a
/// [`LayerSelectionSummary`] of the up/down switches made.
pub(crate) fn drive_layer_selection(router: &mut MediaRouter) -> LayerSelectionSummary {
    // Pass 1 (immutable): snapshot each publishing peer's per-`mid` layer
    // bitrates, keyed by the source peer id.
    let snapshot: HashMap<MediaPeerId, HashMap<Mid, Vec<(Rid, u64)>>> = router
        .peers()
        .iter()
        .map(|p| {
            let mut per_mid: HashMap<Mid, Vec<(Rid, u64)>> = HashMap::new();
            for ((mid, rid), meter) in &p.layer_meters {
                per_mid
                    .entry(*mid)
                    .or_default()
                    .push((*rid, meter.bitrate_bps()));
            }
            for layers in per_mid.values_mut() {
                layers.sort_by_key(|(_, bps)| *bps);
            }
            (p.id().clone(), per_mid)
        })
        .collect();

    // Pass 2 (mutable): choose + apply per subscriber; collect the keyframe
    // requests to issue in pass 3 (the publisher borrow is disjoint from this
    // subscriber borrow, so it cannot be done inline).
    let mut summary = LayerSelectionSummary::default();
    let mut keyframe_reqs: Vec<(MediaPeerId, Mid, Rid)> = Vec::new();
    for sub in router.peers_mut() {
        let Some(budget) = sub.egress_estimate_bps else {
            continue;
        };
        // Collect the decisions first so the `tracks_out` borrow is released
        // before `select_layer` mutates it.
        let decisions: Vec<(MediaPeerId, Mid, Option<Rid>)> = sub
            .tracks_out
            .iter()
            .map(|e| (e.source_peer.clone(), e.source_mid, e.chosen_rid))
            .collect();
        for (src_peer, src_mid, current) in decisions {
            let Some(layers) = snapshot.get(&src_peer).and_then(|m| m.get(&src_mid)) else {
                continue;
            };
            if layers.len() < 2 {
                continue; // not simulcast — nothing to choose
            }
            let chosen = choose_layer_for_budget(layers, budget, current);
            if chosen == current {
                continue;
            }
            sub.select_layer(&src_peer, src_mid, chosen);
            let Some(new_rid) = chosen else {
                continue;
            };
            // Queue a keyframe request for the newly-selected layer.
            keyframe_reqs.push((src_peer.clone(), src_mid, new_rid));
            // Count the switch direction — but only a genuine bandwidth-driven
            // flip (a track that already had a chosen layer); the initial latch
            // (`current == None`) is not a switch.
            if let Some(cur) = current {
                let bps_of = |rid: Rid| -> u64 {
                    layers
                        .iter()
                        .find(|(r, _)| *r == rid)
                        .map_or(0, |(_, b)| *b)
                };
                if bps_of(new_rid) > bps_of(cur) {
                    summary.upgrades += 1;
                } else {
                    summary.downgrades += 1;
                }
            }
        }
    }

    // Pass 3 (mutable, publishers): request a keyframe from each publisher for
    // the layer a subscriber just switched to, so the switch is clean.
    for (src_peer, mid, rid) in keyframe_reqs {
        if let Some(publisher) = router.peers_mut().iter_mut().find(|p| *p.id() == src_peer) {
            publisher.request_layer_keyframe(mid, rid);
        }
    }

    summary
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
        // Cross-call isolation: a peer's new upstream track is
        // only propagated to downstream peers in the SAME call,
        // so the SFU never opens a cross-call `TrackOut` (which
        // would later let `fanout_media_data` forward across
        // calls). Same origin-call lookup as the fanout path.
        let origin_call = router
            .peers()
            .iter()
            .find(|p| p.id() == &event.origin)
            .map(|p| p.call_id().to_owned());
        let Some(origin_call) = origin_call.filter(|c| !c.is_empty()) else {
            continue;
        };
        let n = router.len();
        for idx in 0..n {
            let skip = router.peers().get(idx).map_or(true, |p| {
                p.id() == &event.origin || p.call_id() != origin_call
            });
            if skip {
                continue;
            }
            let Some(peer) = router.peers_mut().get_mut(idx) else {
                continue;
            };
            let inserted = peer.observe_remote_track_in(event.origin.clone(), event.track);
            if inserted {
                // Lifecycle point 2 of 4. The SFU decided this subscriber
                // should receive that publisher's track. A missing line here
                // against a present point 1 means cross-call isolation or the
                // peer walk dropped it, not the negotiation.
                tracing::info!(
                    origin = %event.origin,
                    target = %peer.id(),
                    call = %origin_call,
                    mid = ?event.track.mid,
                    kind = ?event.track.kind,
                    "media: opened downstream track on subscriber"
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
        let extmaps = extmap_summary(&sdp);
        match tx.try_send(ServerFrame::SdpOffer { sdp }) {
            Ok(()) => {
                // Lifecycle point 3 of 4. The SFU offered the subscriber a
                // downstream track. The extmap summary is here because the id
                // an extension is offered on is peer-local — a peer that
                // never offered a video m-line has nothing pinning it — and a
                // mismatch against the peer's own numbering is invisible
                // without seeing the offer. It is a single compact field, not
                // a line per extension.
                tracing::info!(
                    peer = %peer_id,
                    extmaps = %extmaps,
                    "media: offered downstream track to subscriber"
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

    /// A media peer bound to `call` — used by the fanout
    /// tests, which (post cross-call-isolation) only forward
    /// between same-call peers.
    fn peer_in_call(name: &str, call: &str) -> MediaPeer {
        let mut p = MediaPeer::new(pid(name), Instant::now());
        p.set_call_id(call.to_owned());
        p
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
            .accept_offer(pid("alice"), "test-call".to_owned(), "garbage".into())
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
            .accept_offer(pid("alice"), "test-call".to_owned(), "garbage".into())
            .await
            .expect_err("malformed SDP must surface as BadSdp");
        assert!(matches!(
            err,
            MediaCommandError::Media(MediaError::BadSdp(_))
        ));
    }

    /// Without an advertise IP, a concrete bind IP is what
    /// the SFU advertises — today's behaviour for a bind
    /// such as `10.0.0.5:43210` or `127.0.0.1:0`.
    #[test]
    fn resolve_advertise_ip_uses_a_concrete_bind_ip() {
        let bind: IpAddr = "10.0.0.5".parse().unwrap();
        assert_eq!(resolve_advertise_ip(bind, None), Ok(bind));
        let v6: IpAddr = "2001:db8::5".parse().unwrap();
        assert_eq!(resolve_advertise_ip(v6, None), Ok(v6));
    }

    /// An unspecified bind (`0.0.0.0` / `::`) without an
    /// advertise IP is refused — the defect this guards is
    /// str0m silently refusing the `0.0.0.0` host candidate
    /// and every SDP answer shipping without one.
    #[test]
    fn resolve_advertise_ip_refuses_an_unspecified_bind_without_advertise() {
        for bind in ["0.0.0.0", "::"] {
            let bind: IpAddr = bind.parse().unwrap();
            assert_eq!(
                resolve_advertise_ip(bind, None),
                Err(AdvertiseError::UnspecifiedBind { bind }),
            );
        }
    }

    /// An explicit advertise IP wins over the bind IP —
    /// both for an unspecified bind and for a concrete one
    /// (a private interface behind a 1:1 NAT).
    #[test]
    fn resolve_advertise_ip_prefers_the_explicit_advertise_ip() {
        let public: IpAddr = "203.0.113.7".parse().unwrap();
        for bind in ["0.0.0.0", "10.0.0.5"] {
            let bind: IpAddr = bind.parse().unwrap();
            assert_eq!(resolve_advertise_ip(bind, Some(public)), Ok(public));
        }
        let v6: IpAddr = "2001:db8::7".parse().unwrap();
        assert_eq!(
            resolve_advertise_ip("::".parse().unwrap(), Some(v6)),
            Ok(v6)
        );
    }

    /// Every address str0m would refuse as a host candidate
    /// (plus the IPv6 link-local and IPv4-mapped forms it
    /// would not) is refused as an advertise IP.
    #[test]
    fn resolve_advertise_ip_refuses_unusable_advertise_ips() {
        let any_v4: IpAddr = "0.0.0.0".parse().unwrap();
        let any_v6: IpAddr = "::".parse().unwrap();
        for (bind, bad) in [
            (any_v4, "0.0.0.0"),
            (any_v4, "224.0.0.1"),
            (any_v4, "255.255.255.255"),
            (any_v4, "169.254.10.20"),
            (any_v6, "::"),
            (any_v6, "ff02::1"),
            (any_v6, "fe80::1"),
            (any_v6, "::ffff:0.0.0.0"),
        ] {
            let ip: IpAddr = bad.parse().unwrap();
            assert!(
                matches!(
                    resolve_advertise_ip(bind, Some(ip)),
                    Err(AdvertiseError::AdvertiseNotAdvertisable { ip: got, .. }) if got == ip
                ),
                "{bad} must be refused as an advertise IP"
            );
        }
    }

    /// A concrete but unusable bind IP is refused when it
    /// would be advertised as-is.
    #[test]
    fn resolve_advertise_ip_refuses_an_unusable_concrete_bind() {
        let bind: IpAddr = "169.254.1.1".parse().unwrap();
        assert!(matches!(
            resolve_advertise_ip(bind, None),
            Err(AdvertiseError::BindNotAdvertisable { .. })
        ));
    }

    /// The advertise IP must be the bind IP's family.
    #[test]
    fn resolve_advertise_ip_refuses_a_family_mismatch() {
        let cases = [("0.0.0.0", "2001:db8::7"), ("::", "203.0.113.7")];
        for (bind, advertise) in cases {
            let bind: IpAddr = bind.parse().unwrap();
            let advertise: IpAddr = advertise.parse().unwrap();
            assert_eq!(
                resolve_advertise_ip(bind, Some(advertise)),
                Err(AdvertiseError::FamilyMismatch { bind, advertise }),
            );
        }
    }

    /// The validation agrees with str0m on the addresses
    /// the SFU is expected to advertise: every IP this
    /// module accepts builds a `str0m::Candidate::host`,
    /// and every IP str0m refuses is refused here too. If a
    /// str0m bump tightens its rules, this fails before a
    /// deployment ships answers without a candidate.
    #[test]
    fn advertisable_ip_problem_agrees_with_str0m_host_candidate_rules() {
        for ip in [
            "127.0.0.1",
            "10.0.0.5",
            "203.0.113.7",
            "::1",
            "2001:db8::7",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "169.254.10.20",
            "::",
            "ff02::1",
        ] {
            let ip: IpAddr = ip.parse().unwrap();
            let str0m_accepts = Candidate::host(SocketAddr::new(ip, 5000), Protocol::Udp).is_ok();
            assert_eq!(
                advertisable_ip_problem(ip).is_none(),
                str0m_accepts,
                "validation and str0m disagree on {ip}"
            );
        }
    }

    /// `spawn_media_task` on an unspecified bind refuses
    /// to start instead of running a media plane whose SDP
    /// answers would carry no SFU candidate.
    #[tokio::test]
    async fn spawn_media_task_refuses_an_unspecified_bind_without_advertise() {
        let peers = crate::peer::PeerRegistry::new();
        let result = spawn_media_task("0.0.0.0:0".parse().unwrap(), peers).await;
        assert!(matches!(
            result,
            Err(MediaError::AdvertiseAddress(
                AdvertiseError::UnspecifiedBind { .. }
            ))
        ));
    }

    /// An unspecified bind plus an advertise IP starts, and
    /// the returned address is the advertise IP with the
    /// OS-picked port — the address clients must dial.
    #[tokio::test]
    async fn spawn_media_task_advertising_returns_the_advertised_addr() {
        let peers = crate::peer::PeerRegistry::new();
        let (_handle, addr) = spawn_media_task_advertising(
            "0.0.0.0:0".parse().unwrap(),
            Some("127.0.0.1".parse().unwrap()),
            peers,
            None,
        )
        .await
        .expect("unspecified bind with an advertise IP starts");
        assert_eq!(addr.ip(), "127.0.0.1".parse::<IpAddr>().unwrap());
        assert_ne!(addr.port(), 0, "OS-picked port must be non-zero");
    }

    /// A host candidate str0m refuses fails the offer out
    /// loud instead of shipping an answer without one (the
    /// `let _ =` this replaced). Driven through
    /// `handle_command` directly with an address that
    /// bypassed `resolve_advertise_ip`.
    #[tokio::test]
    async fn rejected_host_candidate_fails_the_offer() {
        let mut router = MediaRouter::new();
        let (reply, rx) = tokio::sync::oneshot::channel();
        handle_command(
            &mut router,
            MediaCommand::AcceptOffer {
                peer_id: pid("alice"),
                call_id: "test-call".to_owned(),
                offer_sdp: "never parsed".to_owned(),
                reply,
            },
            Some("0.0.0.0:43210".parse().unwrap()),
        );
        let res = rx.await.expect("reply sent");
        assert!(
            matches!(&res, Err(MediaError::Str0m(msg)) if msg.contains("host candidate")),
            "expected the host-candidate refusal, got {res:?}"
        );
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
    /// Look up a peer by test name. `MediaRouter` exposes the slice, not a
    /// by-id getter, and these tests only ever want one peer.
    fn find_peer<'a>(r: &'a MediaRouter, name: &str) -> &'a MediaPeer {
        let id = pid(name);
        r.peers()
            .iter()
            .find(|p| p.id() == &id)
            .unwrap_or_else(|| panic!("peer {name} must be in the router"))
    }

    /// The unit-level statement of the late-joiner repair: a peer that
    /// arrives after a track was announced still gets it, and the
    /// call boundary is respected while doing so.
    #[test]
    fn backfill_gives_a_joiner_the_calls_existing_tracks_but_not_another_calls() {
        let mut r = MediaRouter::new();

        let mut publisher = peer_in_call("publisher", "call-a");
        let video = publisher.register_local_track_in(Mid::new(), MediaKind::Video);
        let audio = publisher.register_local_track_in(Mid::new(), MediaKind::Audio);
        r.add_peer(publisher);

        let mut stranger = peer_in_call("stranger", "call-b");
        stranger.register_local_track_in(Mid::new(), MediaKind::Video);
        r.add_peer(stranger);

        // The joiner arrives last, so nothing ever propagated to it.
        r.add_peer(peer_in_call("joiner", "call-a"));
        assert!(
            find_peer(&r, "joiner").tracks_out().is_empty(),
            "a fresh peer starts with nothing"
        );

        backfill_existing_tracks(&mut r, &pid("joiner"));

        let joiner = find_peer(&r, "joiner");
        assert_eq!(
            joiner.tracks_out().len(),
            2,
            "both of the publisher's tracks"
        );
        assert!(
            joiner
                .tracks_out()
                .iter()
                .all(|t| t.source_peer == pid("publisher")),
            "the stranger is in another call and must never be a source"
        );
        let mids: Vec<Mid> = joiner.tracks_out().iter().map(|t| t.source_mid).collect();
        assert!(mids.contains(&video.mid) && mids.contains(&audio.mid));

        // Runs on every offer from a peer, renegotiations included, so it
        // must never accumulate duplicates.
        backfill_existing_tracks(&mut r, &pid("joiner"));
        assert_eq!(
            find_peer(&r, "joiner").tracks_out().len(),
            2,
            "backfill must be idempotent"
        );
    }

    #[test]
    fn backfill_is_a_no_op_for_a_peer_with_no_call_or_no_peers() {
        let mut r = MediaRouter::new();
        // Unknown peer.
        backfill_existing_tracks(&mut r, &pid("nobody"));

        // A peer whose call is unbound must not inherit anything — an empty
        // call id matches no real call, the same rule the fanout applies.
        let mut publisher = peer_in_call("publisher", "call-a");
        publisher.register_local_track_in(Mid::new(), MediaKind::Video);
        r.add_peer(publisher);
        r.add_peer(MediaPeer::new(pid("unbound"), Instant::now()));
        backfill_existing_tracks(&mut r, &pid("unbound"));
        assert!(
            find_peer(&r, "unbound").tracks_out().is_empty(),
            "an unbound peer must not inherit a call's tracks"
        );
    }

    #[test]
    fn keyframe_gate_allows_the_first_request_and_one_per_interval_after() {
        let t0 = Instant::now();
        assert!(keyframe_request_allowed(None, t0), "nothing sent yet");
        assert!(
            !keyframe_request_allowed(Some(t0), t0 + KEYFRAME_REQUEST_MIN_INTERVAL / 2),
            "inside the window is denied"
        );
        assert!(
            keyframe_request_allowed(Some(t0), t0 + KEYFRAME_REQUEST_MIN_INTERVAL),
            "exactly the interval later is allowed"
        );
        assert!(
            !keyframe_request_allowed(Some(t0 + Duration::from_secs(1)), t0),
            "a clock reading before the last request denies rather than underflows"
        );
    }

    #[test]
    fn keyframe_interval_sits_between_the_receivers_cadence_and_a_usable_wait() {
        // The live receiver asked about every 200 ms; below that the limit
        // relays everything and limits nothing. Above a second a late joiner
        // waits noticeably. The value is chosen, not measured — this pins the
        // reasoning so a later change has to argue with it.
        assert!(KEYFRAME_REQUEST_MIN_INTERVAL > Duration::from_millis(200));
        assert!(KEYFRAME_REQUEST_MIN_INTERVAL <= Duration::from_secs(1));
    }

    #[test]
    fn origin_for_downstream_mid_resolves_only_an_open_track() {
        let mut sub = peer_in_call("sub", "call-a");
        let source_mid = Mid::new();
        sub.observe_remote_track_in(
            pid("pub"),
            TrackIn {
                mid: source_mid,
                kind: MediaKind::Video,
            },
        );
        let local = Mid::new();

        // ToOpen: no local mid exists yet to ask on.
        assert!(sub.origin_for_downstream_mid(local).is_none());

        sub.tracks_out[0].state = TrackOutState::Negotiating(local);
        assert!(
            sub.origin_for_downstream_mid(local).is_none(),
            "a negotiating track has no stream behind it yet"
        );

        sub.tracks_out[0].state = TrackOutState::Open(local);
        let resolved = sub
            .origin_for_downstream_mid(local)
            .expect("open track resolves");
        assert_eq!(resolved.source_peer, pid("pub"));
        assert_eq!(resolved.source_mid, source_mid);
        assert_eq!(resolved.kind, MediaKind::Video);
        assert_eq!(resolved.rid, None, "no layer latched yet");

        assert!(
            sub.origin_for_downstream_mid(Mid::new()).is_none(),
            "an unrelated mid resolves to nothing"
        );
    }

    #[test]
    fn relay_skips_audio_and_unknown_publishers_without_panicking() {
        let mut r = MediaRouter::new();
        r.add_peer(peer_in_call("pub", "call-a"));
        let now = Instant::now();
        let reqs = vec![
            OpenedTrack {
                source_peer: pid("pub"),
                source_mid: Mid::new(),
                kind: MediaKind::Audio,
                rid: None,
            },
            OpenedTrack {
                source_peer: pid("nobody"),
                source_mid: Mid::new(),
                kind: MediaKind::Video,
                rid: None,
            },
            // A video track on a known publisher with no live StreamRx: the
            // PLI has nowhere to go, so it is reported as not sent — never
            // as sent-and-logged.
            OpenedTrack {
                source_peer: pid("pub"),
                source_mid: Mid::new(),
                kind: MediaKind::Video,
                rid: None,
            },
        ];
        assert_eq!(relay_keyframe_requests(&mut r, &reqs, now), 0);
    }

    #[test]
    fn keyframe_reason_labels_are_stable() {
        assert_eq!(
            KeyframeReason::DownstreamOpened.as_str(),
            "downstream-opened"
        );
        assert_eq!(KeyframeReason::SubscriberRequest.as_str(), "subscriber-pli");
        assert_eq!(KeyframeReason::ForwardingGap.as_str(), "forwarding-gap");
    }

    #[test]
    fn only_a_video_frame_with_a_gap_before_it_asks_for_a_keyframe() {
        assert!(gap_needs_keyframe(Some(MediaKind::Video), false));
        assert!(!gap_needs_keyframe(Some(MediaKind::Video), true));
        assert!(!gap_needs_keyframe(Some(MediaKind::Audio), false));
        assert!(!gap_needs_keyframe(Some(MediaKind::Audio), true));
        // A frame on a mid this peer never announced is not answered.
        assert!(!gap_needs_keyframe(None, false));
    }

    #[test]
    fn audio_is_never_held_behind_a_gap_and_video_for_less_than_str0ms_default() {
        // One complete frame queued behind the gap is enough to release it:
        // audio has no retransmission to wait for.
        assert_eq!(REORDER_AUDIO_FRAMES, 1);
        // str0m's default is 30 frames, a second at 30 frames a second.
        assert!((1..30).contains(&REORDER_VIDEO_FRAMES));
    }

    #[test]
    fn media_line_summary_shows_the_direction_and_msid_per_section() {
        let sdp = "v=0\r\n\
             m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
             a=recvonly\r\n\
             a=msid:aaaaaa bbbbbb\r\n\
             m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
             a=sendrecv\r\n\
             a=msid:cccccc dddddd\r\n";
        assert_eq!(
            media_line_summary(sdp),
            "[audio recvonly msid=aaaaaa] [video sendrecv msid=cccccc]"
        );
    }

    #[test]
    fn media_line_summary_survives_malformed_input() {
        assert_eq!(media_line_summary(""), "");
        assert_eq!(
            media_line_summary("a=sendrecv"),
            "",
            "no m= section to attach to"
        );
        assert_eq!(media_line_summary("m=video 9 x\r\n"), "[video]");
    }

    #[test]
    fn extmap_summary_names_the_id_each_extension_landed_on() {
        let sdp = "v=0\r\n\
             m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
             a=extmap:1 urn:ietf:params:rtp-hdrext:ssrc-audio-level\r\n\
             a=extmap:4 urn:ietf:params:rtp-hdrext:sdes:mid\r\n\
             m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
             a=extmap:4 urn:ietf:params:rtp-hdrext:sdes:mid\r\n\
             a=extmap:12 http://www.webrtc.org/experiments/rtp-hdrext/generic-frame-descriptor-00\r\n";
        let summary = extmap_summary(sdp);
        assert_eq!(
            summary,
            "[audio] 1:ssrc-audio-level 4:mid [video] 4:mid 12:generic-frame-descriptor-00"
        );
    }

    #[test]
    fn extmap_summary_survives_malformed_input() {
        // It runs on an SDP str0m produced, but it is a log helper: it must
        // never be the thing that panics during a call.
        assert_eq!(extmap_summary(""), "");
        assert_eq!(extmap_summary("a=extmap:"), "");
        assert_eq!(extmap_summary("a=extmap:7"), "7:?");
        assert_eq!(extmap_summary("m="), "[?]");
    }

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
                r.add_peer(peer_in_call(nm, "fanout-call"));
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
            r.add_peer(peer_in_call(nm, "fanout-call"));
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

    /// Cross-call isolation (a hard SFU invariant): a frame
    /// from a peer in call A is forwarded ONLY to the other
    /// peer in call A — NEVER to peers in call B, even though
    /// they share the one process-wide router. With one
    /// same-call subscriber + two cross-call peers, exactly
    /// ONE forward attempt is made (the same-call subscriber);
    /// the two foreign-call peers are never touched.
    #[test]
    fn remove_peer_command_drops_the_media_peer_from_the_router() {
        let mut r = MediaRouter::new();
        r.add_peer(peer_in_call("pub", "call-A"));
        r.add_peer(peer_in_call("sub", "call-A"));
        handle_command(
            &mut r,
            MediaCommand::RemovePeer {
                peer_id: pid("sub"),
            },
            None,
        );
        assert_eq!(r.len(), 1, "the departed peer is gone");
        assert!(r.peer_mut(&pid("sub")).is_none());
        // A frame from the remaining publisher is offered to nobody:
        // no ghost is left to fan out to.
        let pending = vec![PendingFanout {
            origin: pid("pub"),
            data: synthetic_media_data(),
        }];
        assert_eq!(fanout_media_data(&mut r, &pending).attempted(), 0);
        // Removing again is a no-op.
        handle_command(
            &mut r,
            MediaCommand::RemovePeer {
                peer_id: pid("sub"),
            },
            None,
        );
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn disconnect_makes_the_peer_prunable() {
        let mut p = MediaPeer::new(pid("alice"), Instant::now());
        assert!(p.is_alive());
        p.disconnect();
        assert!(!p.is_alive(), "disconnect flips is_alive");
        let mut r = MediaRouter::new();
        r.add_peer(p);
        r.retain_peers(MediaPeer::is_alive);
        assert!(r.is_empty(), "the loop-tail prune drops it");
    }

    #[test]
    fn fanout_ledger_files_per_target_and_flushes_only_when_due() {
        let t0 = Instant::now();
        let mut l = FanoutLedger::new(Duration::from_secs(10), t0);
        let data = synthetic_media_data();
        l.record_origin(&pid("pub"), &data);
        l.record(&pid("sub1"), &pid("pub"), data.mid, &MediaForward::Sent);
        l.record(&pid("sub1"), &pid("pub"), data.mid, &MediaForward::NoWriter);
        l.record(&pid("sub2"), &pid("pub"), data.mid, &MediaForward::Sent);
        let sub1 = &l.per_target[&(pid("sub1"), pid("pub"), data.mid)];
        assert_eq!((sub1.sent, sub1.no_writer), (1, 1));
        assert_eq!(l.per_target.len(), 2, "one row per destination");
        let origin = &l.per_origin[&(pid("pub"), data.mid)];
        assert_eq!(origin.frames, 1);
        assert_eq!(origin.bytes, data.data.len() as u64);
        assert_eq!(origin.key_frames, 0, "no descriptor on a synthetic frame");
        assert_eq!(
            l.flush_if_due(t0 + Duration::from_secs(9)),
            0,
            "not due yet"
        );
        assert_eq!(l.per_target.len(), 2, "kept until the flush");
        assert_eq!(
            l.flush_if_due(t0 + Duration::from_secs(10)),
            3,
            "one publisher line and two destination lines"
        );
        assert!(l.per_target.is_empty() && l.per_origin.is_empty());
        assert_eq!(
            l.flush_if_due(t0 + Duration::from_secs(10)),
            0,
            "a flush restarts the interval"
        );
    }

    #[test]
    fn fanout_never_crosses_calls() {
        let mut r = MediaRouter::new();
        r.add_peer(peer_in_call("a-pub", "call-A"));
        r.add_peer(peer_in_call("a-sub", "call-A"));
        r.add_peer(peer_in_call("b-sub1", "call-B"));
        r.add_peer(peer_in_call("b-sub2", "call-B"));
        let pending = vec![PendingFanout {
            origin: pid("a-pub"),
            data: synthetic_media_data(),
        }];

        let outcome = fanout_media_data(&mut r, &pending);

        // Exactly one non-origin SAME-call peer (a-sub) is
        // offered the frame; the two call-B peers are never
        // visited. (No writer is negotiated, so the single
        // attempt lands in `no_writer` — the count is what
        // proves the iteration reached exactly one peer.)
        assert_eq!(
            outcome.attempted(),
            1,
            "a frame from call A must reach exactly the one other call-A peer"
        );
        assert_eq!(outcome.no_writer, 1, "the single same-call attempt");
    }

    /// A frame whose origin has no bound call (an incomplete
    /// peer that never completed an SDP offer) is forwarded to
    /// NOBODY — fail-closed, so a half-set-up peer can never
    /// leak into any call.
    #[test]
    fn fanout_unbound_origin_forwards_to_nobody() {
        let mut r = MediaRouter::new();
        r.add_peer(MediaPeer::new(pid("ghost"), Instant::now())); // empty call
        r.add_peer(peer_in_call("a-sub", "call-A"));
        let pending = vec![PendingFanout {
            origin: pid("ghost"),
            data: synthetic_media_data(),
        }];

        let outcome = fanout_media_data(&mut r, &pending);
        assert_eq!(outcome.attempted(), 0, "an unbound origin forwards nothing");
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
        r.add_peer(peer_in_call("alice", "track-call"));
        r.add_peer(peer_in_call("bob", "track-call"));
        r.add_peer(peer_in_call("charlie", "track-call"));
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

    // ============================================================
    // Simulcast layer selection (G3-3)
    // ============================================================

    #[test]
    fn layer_gate_non_simulcast_frame_always_forwards_and_never_latches() {
        let mut chosen = None;
        assert!(layer_gate_allows(&mut chosen, None));
        assert!(layer_gate_allows(&mut chosen, None));
        assert_eq!(chosen, None, "a rid-less frame must not latch a layer");
    }

    #[test]
    fn layer_gate_latches_the_first_layer_and_drops_the_others() {
        let h = Rid::from("h");
        let l = Rid::from("l");
        let mut chosen = None;
        // First simulcast frame latches its layer and forwards.
        assert!(layer_gate_allows(&mut chosen, Some(h)));
        assert_eq!(chosen, Some(h));
        // Same layer keeps flowing.
        assert!(layer_gate_allows(&mut chosen, Some(h)));
        // A different layer is dropped — and does NOT move the latch.
        assert!(!layer_gate_allows(&mut chosen, Some(l)));
        assert_eq!(chosen, Some(h));
    }

    #[test]
    fn layer_gate_honours_an_explicit_policy_selection() {
        let h = Rid::from("h");
        let l = Rid::from("l");
        // A bandwidth policy pre-selected the low layer.
        let mut chosen = Some(l);
        assert!(
            !layer_gate_allows(&mut chosen, Some(h)),
            "high layer dropped"
        );
        assert!(
            layer_gate_allows(&mut chosen, Some(l)),
            "low layer forwarded"
        );
        assert_eq!(chosen, Some(l), "the selection is not overwritten");
    }

    #[test]
    fn select_layer_sets_chosen_rid_on_the_matching_track_only() {
        let mut p = MediaPeer::new(pid("alice"), Instant::now());
        let source = pid("bob");
        let src_mid = Mid::new();
        p.observe_remote_track_in(
            source.clone(),
            TrackIn {
                mid: src_mid,
                kind: MediaKind::Video,
            },
        );
        assert_eq!(p.tracks_out[0].chosen_rid, None, "starts un-latched");

        let h = Rid::from("h");
        assert!(
            p.select_layer(&source, src_mid, Some(h)),
            "matching track set"
        );
        assert_eq!(p.tracks_out[0].chosen_rid, Some(h));

        // A non-matching source is a no-op returning false.
        assert!(!p.select_layer(&pid("carol"), src_mid, Some(Rid::from("l"))));
        assert_eq!(p.tracks_out[0].chosen_rid, Some(h), "unchanged");

        // `None` re-arms the auto-latch.
        assert!(p.select_layer(&source, src_mid, None));
        assert_eq!(p.tracks_out[0].chosen_rid, None);
    }

    #[test]
    fn forward_drops_a_non_selected_simulcast_layer_as_wrong_layer() {
        let mut p = MediaPeer::new(pid("alice"), Instant::now());
        let source = pid("bob");
        let mut frame = synthetic_media_data();
        frame.rid = Some(Rid::from("h"));
        // Manually stage an Open downstream entry that has already selected the
        // LOW layer — the high-layer frame above must be dropped as WrongLayer
        // BEFORE any writer lookup (so no real str0m session is needed).
        p.tracks_out.push(TrackOutEntry {
            source_peer: source.clone(),
            source_mid: frame.mid,
            kind: MediaKind::Video,
            state: TrackOutState::Open(Mid::new()),
            chosen_rid: Some(Rid::from("l")),
        });
        assert_eq!(
            p.forward_media_data(&source, &frame),
            MediaForward::WrongLayer,
            "a frame for the non-selected layer is dropped as WrongLayer"
        );
    }

    // ============================================================
    // Simulcast BANDWIDTH policy (G3-3)
    // ============================================================

    #[test]
    fn layer_bitrate_meter_reports_the_completed_window_rate() {
        let t0 = Instant::now();
        let mut m = LayerBitrateMeter::new(t0);
        assert_eq!(m.bitrate_bps(), 0, "no completed window yet");
        // 100_000 payload bytes accumulate in the first ~1 s window.
        m.record(100_000, t0);
        // A record just past the window boundary publishes the window's rate.
        m.record(0, t0 + Duration::from_secs(1));
        // 100_000 bytes * 8 / 1.0 s ≈ 800 kbps.
        let bps = m.bitrate_bps();
        assert!((700_000..900_000).contains(&bps), "≈800 kbps, got {bps}");
    }

    #[test]
    fn choose_layer_empty_preserves_current() {
        let h = Rid::from("h");
        assert_eq!(choose_layer_for_budget(&[], 1_000_000, Some(h)), Some(h));
        assert_eq!(choose_layer_for_budget(&[], 1_000_000, None), None);
    }

    #[test]
    fn choose_layer_downgrades_immediately_under_congestion() {
        let l = Rid::from("l");
        let h = Rid::from("h");
        let layers = [(l, 300_000u64), (h, 2_000_000u64)];
        // On high; the budget collapses to 500 kbps → drop to low AT ONCE.
        assert_eq!(choose_layer_for_budget(&layers, 500_000, Some(h)), Some(l));
    }

    #[test]
    fn choose_layer_upgrades_only_with_headroom() {
        let l = Rid::from("l");
        let h = Rid::from("h");
        let layers = [(l, 300_000u64), (h, 1_000_000u64)];
        // On low; a 1.05 Mbps budget fits high's plain 1 Mbps but NOT the 20 %
        // upgrade headroom (needs 1.2 Mbps) → hold at low to avoid flapping.
        assert_eq!(
            choose_layer_for_budget(&layers, 1_050_000, Some(l)),
            Some(l)
        );
        // A 1.3 Mbps budget clears the headroom → upgrade to high.
        assert_eq!(
            choose_layer_for_budget(&layers, 1_300_000, Some(l)),
            Some(h)
        );
    }

    #[test]
    fn choose_layer_picks_lowest_when_none_fit() {
        let l = Rid::from("l");
        let h = Rid::from("h");
        let layers = [(l, 300_000u64), (h, 2_000_000u64)];
        // Budget below even the low layer → still choose the lowest (a degraded
        // stream beats no stream).
        assert_eq!(choose_layer_for_budget(&layers, 100_000, None), Some(l));
    }

    #[test]
    fn drive_layer_selection_downgrades_a_starved_subscriber() {
        let mid = Mid::new();
        let rid_l = Rid::from("l");
        let rid_h = Rid::from("h");

        // Publisher P with two measured layers on `mid`.
        let mut publisher = MediaPeer::new(pid("pub"), Instant::now());
        let pub_id = publisher.id().clone();
        publisher
            .layer_meters
            .insert((mid, rid_l), meter_with_bps(300_000));
        publisher
            .layer_meters
            .insert((mid, rid_h), meter_with_bps(2_000_000));

        // Subscriber S receiving P's track, currently on the HIGH layer, with a
        // 500 kbps egress estimate.
        let mut subscriber = MediaPeer::new(pid("sub"), Instant::now());
        subscriber.set_egress_estimate_bps(500_000);
        subscriber.tracks_out.push(TrackOutEntry {
            source_peer: pub_id.clone(),
            source_mid: mid,
            kind: MediaKind::Video,
            state: TrackOutState::Open(Mid::new()),
            chosen_rid: Some(rid_h),
        });

        let mut router = MediaRouter::new();
        router.add_peer(publisher);
        router.add_peer(subscriber);

        let summary = drive_layer_selection(&mut router);
        assert_eq!(
            summary,
            LayerSelectionSummary {
                upgrades: 0,
                downgrades: 1
            },
            "the high->low move is counted as exactly one downgrade"
        );

        let sub = router
            .peers()
            .iter()
            .find(|p| *p.id() == pid("sub"))
            .unwrap();
        assert_eq!(
            sub.tracks_out[0].chosen_rid,
            Some(rid_l),
            "a 500 kbps budget must downgrade the subscriber from high to low"
        );
    }

    #[test]
    fn drive_layer_selection_counts_a_recovery_as_an_upgrade() {
        let mid = Mid::new();
        let rid_l = Rid::from("l");
        let rid_h = Rid::from("h");

        let mut publisher = MediaPeer::new(pid("pub2"), Instant::now());
        let pub_id = publisher.id().clone();
        publisher
            .layer_meters
            .insert((mid, rid_l), meter_with_bps(300_000));
        publisher
            .layer_meters
            .insert((mid, rid_h), meter_with_bps(1_000_000));

        // On the LOW layer with a generous 2 Mbps estimate — clears the upgrade
        // headroom for the 1 Mbps high layer.
        let mut subscriber = MediaPeer::new(pid("sub2"), Instant::now());
        subscriber.set_egress_estimate_bps(2_000_000);
        subscriber.tracks_out.push(TrackOutEntry {
            source_peer: pub_id,
            source_mid: mid,
            kind: MediaKind::Video,
            state: TrackOutState::Open(Mid::new()),
            chosen_rid: Some(rid_l),
        });

        let mut router = MediaRouter::new();
        router.add_peer(publisher);
        router.add_peer(subscriber);

        let summary = drive_layer_selection(&mut router);
        assert_eq!(
            summary,
            LayerSelectionSummary {
                upgrades: 1,
                downgrades: 0
            }
        );
        let sub = router
            .peers()
            .iter()
            .find(|p| *p.id() == pid("sub2"))
            .unwrap();
        assert_eq!(sub.tracks_out[0].chosen_rid, Some(rid_h));
    }

    /// A [`LayerBitrateMeter`] whose completed-window rate is a fixed `bps`.
    fn meter_with_bps(bps: u64) -> LayerBitrateMeter {
        LayerBitrateMeter {
            window: LayerBitrateMeter::WINDOW,
            window_start: Instant::now(),
            bytes: 0,
            last_bps: bps,
        }
    }
}
