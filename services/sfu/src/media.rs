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
//! ## Packets, not frames
//!
//! Every peer's `str0m::Rtc` runs in **RTP mode**
//! ([ADR-0066](../../../docs/adr/0066-sfu-forwards-rtp-packets.md)).
//! str0m hands the SFU each RTP packet as it arrives
//! (`Event::RtpPacket`) and the SFU writes that packet to
//! every other peer of the same call, on the downstream
//! track's send stream. Nothing is reassembled and nothing
//! is held:
//!
//! - the payload is forwarded untouched and the marker bit
//!   is kept;
//! - the sequence number and RTP timestamp are the
//!   publisher's plus a fixed offset
//!   ([`crate::stream_map::StreamMap`]), so a packet the SFU
//!   never received is a gap the subscriber sees, and a
//!   retransmission that arrives later fills it;
//! - the payload type is mapped by codec parameters from the
//!   publisher's negotiation to the subscriber's;
//! - with `LAKKO_SFU_VIDEO_FEC` on, a video packet sent inside
//!   RED (RFC 2198, one block) is handled as the media packet
//!   it carries and goes out RED-wrapped or unwrapped by what
//!   the subscriber negotiated ([`crate::video_fec`]): the one
//!   case where the first byte of a payload, cleartext framing
//!   in front of the sealed frame, is read and rewritten. A
//!   publisher's FEC packets (RFC 5109) end at the SFU: they
//!   are used to rebuild media packets lost on the publisher's
//!   leg ([`crate::fec_receive`]), which are then forwarded
//!   like any received packet, and are sent to no subscriber.
//!   In the slot each leaves on the stream of a subscriber
//!   that negotiated the pair, the SFU sends an FEC packet of
//!   its own, computed over the same media packets as str0m
//!   serialized them for that subscriber
//!   ([`crate::fec_send`]);
//! - the generic frame descriptor is carried verbatim
//!   ([`crate::frame_descriptor`]), with video
//!   orientation; `mid`, `rid`, the transport-wide
//!   sequence number and absolute send time belong to each
//!   leg and are set by str0m. The audio level (RFC 6464) is
//!   not negotiated with any peer and is carried nowhere
//!   (see [`session_extension_map`]).
//!
//! Loss is repaired where it is seen: str0m on the receive
//! side asks a publisher again for a missing video packet,
//! and a subscriber's own request for a packet the SFU did
//! forward is answered from str0m's send buffer.
//!
//! ## What is in place
//!
//! The UDP socket and its tokio loop are [`spawn_media_task`]
//! / `run_media_loop`; fanout is `fanout_packets_into`, which
//! walks every non-origin peer of the same call; a publisher
//! that sends a light copy of its camera on a second m-line and
//! says so in its offer reaches each subscriber as two
//! downstream tracks, the main one and a light companion
//! ([`TrackRole`], [`MediaPeer::declare_light_tracks`]); a
//! publisher that also declares a middle copy reaches each
//! subscriber as three, and the subscriber says which of the two
//! heavy ones it is sent ([`MediaPeer::set_video_wants`],
//! `drive_video_wants`, ADR-0070 step 5); a
//! simulcast video track does too (`drive_two_track_delivery`);
//! simulcast layer selection for
//! the main track is `drive_layer_selection` over
//! per-`(Mid, Rid)` bitrate meters; and the in-process
//! synthetic-client tests are
//! `services/sfu/tests/synthetic_client_e2e.rs`. All of it
//! keeps the audit-pack invariant ("payload-opaque router",
//! **C-39**) intact: the SFU never reads or alters a payload,
//! and fanout refuses to cross a `call_id` boundary.
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
use str0m::format::{Codec, CodecConfig, PayloadParams};
use str0m::media::{Direction, KeyframeRequestKind, MediaKind, MediaTime, Mid, Rid, Rids};
use str0m::net::{Protocol, Receive, Transmit};
use str0m::rtp::{
    Extension, ExtensionMap, ExtensionValues, RtpPacket, SeqNo, Ssrc, VideoOrientation,
};
use str0m::{Candidate, Event, Input, Output, Rtc};
use thiserror::Error;

use crate::burst_keyframe::{Arrived, BurstEnd, BurstEnds};
use crate::downlink_allocation::{self, DownlinkAllocation};
use crate::downlink_probe::{DownlinkProbe, ProbeOutcome, PROBE_DURATION, TOO_LARGE_WITHIN};
use crate::fec_receive::{self, FecReceiver, MutableExtensions};
use crate::fec_send::{FecSender, Generated};
use crate::frame_descriptor::{self, FrameDescriptor, PacketDescriptor};
use crate::peer::{PeerId, PeerRegistry};
use crate::playout_delay::{self, DownlinkBursts, PlayoutDelay, UplinkBursts};
use crate::protocol::{
    ServerFrame, VideoGiven, VideoQuality, VideoWant, MAX_VIDEO_GIVEN, MAX_VIDEO_WANTS,
};
use crate::publish_budget::{self, PublishBudget};
use crate::stream_map::StreamMap;
use crate::uplink_estimate::{
    ChangeReason, DetectorState, EstimatorConfig, PacketArrival, PathEstimator, SeqLossCounter,
    UplinkEstimator,
};
use crate::uplink_fallback::{self, UplinkFallback};
use crate::video_fec;
use crate::video_needed::{self, Needed, VideoNeeded, Wanted};

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

/// Outcome of offering one publisher packet to one
/// destination peer (`MediaPeer::forward_packet`). Distinct
/// skip reasons live on distinct variants so the metrics
/// layer counts them separately — `NoWriter` is the expected
/// state during early track negotiation and must not look
/// like a codec mismatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaForward {
    /// The packet was written to the destination peer's
    /// send stream; str0m SRTP-encrypts it and hands it to
    /// the socket on that peer's next poll.
    Sent,
    /// The destination has nowhere to send this packet yet:
    /// no downstream track is open for the source (typically
    /// pre-renegotiation), its transport has not connected,
    /// or str0m has no send stream for the track.
    NoWriter,
    /// The destination did not negotiate a payload type for
    /// the packet's codec.
    NoCodecMatch,
    /// str0m's `StreamTx::write_rtp` returned an error.
    WriteFailed(String),
    /// Simulcast: the packet belongs to a layer this
    /// downstream track is not currently forwarding. The
    /// SFU sends one simulcast layer per downstream track,
    /// so packets of the other layers are dropped here by
    /// design — not a fault.
    WrongLayer,
    /// The packet precedes everything the downstream stream
    /// can number: it is older than the packet a simulcast
    /// switch took effect at, or than the sequence-number
    /// cycle the stream started in. Its number downstream
    /// is already taken, so it is not forwarded.
    Stale,
    /// The subscriber does not show this track right now
    /// ([`MediaPeer::set_video_wants`], ADR-0070 step 5): it
    /// asked for another picture of this publisher, or the
    /// track is waiting for a key frame to resume at. Dropped
    /// by design, not a fault.
    NotWanted,
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
    /// The track is simulcast video: the publisher sends it
    /// on more than one rid. Known from the track's
    /// negotiation when it lists the rids, otherwise set the
    /// moment a second rid is seen on it
    /// ([`MediaPeer::note_simulcast_track_in`]). A subscriber
    /// of such a track gets two downstream tracks, see
    /// [`TrackRole`].
    pub simulcast: bool,
    /// The track is the **light copy** of this peer's camera: a
    /// second, ordinary video sender the peer declared in its
    /// offer ([`MediaPeer::declare_light_tracks`]). A subscriber
    /// gets it as the peer's light companion and never as a main
    /// track, see [`TrackRole`]. Never set together with
    /// `simulcast`.
    pub light: bool,
    /// The track is the **middle copy** of this peer's camera: a
    /// third, ordinary video sender the peer declared in its offer
    /// ([`MediaPeer::declare_middle_tracks`]). A subscriber gets it
    /// as the peer's middle track ([`TrackRole::Middle`]) and never
    /// as a main track. Never set together with `simulcast` or
    /// `light`.
    pub middle: bool,
}

impl TrackIn {
    /// What a subscriber gets this track as: the publisher's light
    /// companion, its middle track, or a main track.
    #[must_use]
    pub const fn role(&self) -> TrackRole {
        if self.light {
            TrackRole::LightCompanion
        } else if self.middle {
            TrackRole::Middle
        } else {
            TrackRole::Main
        }
    }
}

/// Suffix of the MSID stream id of a light companion track
/// ([`TrackRole::LightCompanion`]): the stream id is the
/// source peer id followed by this. The main track's stream
/// id is the source peer id alone. Clients tell the two
/// tracks of one publisher apart by it.
pub const LIGHT_STREAM_SUFFIX: &str = "~light";

/// Suffix of the MSID stream id of a middle track
/// ([`TrackRole::Middle`]): the stream id is the source peer id
/// followed by this.
pub const MIDDLE_STREAM_SUFFIX: &str = "~mid";

/// Which of the downstream tracks of one publisher video
/// track a [`TrackOutEntry`] is.
///
/// A subscriber receives a publisher's camera as **two**
/// tracks, both all the time:
///
/// - the **main** track, MSID stream id = the source peer id;
/// - the **light companion**, MSID stream id = the source
///   peer id + [`LIGHT_STREAM_SUFFIX`]: the same picture small
///   and cheap.
///
/// A subscriber decodes both and shows the light one the
/// moment the main one stands still; no key frame is needed
/// at that moment.
///
/// There are two ways a publisher produces the pair:
///
/// - **Two senders** (what the Lakko clients do). The
///   publisher sends the camera on two ordinary m-lines, one
///   encoding each, and names the light one in its offer
///   ([`MediaPeer::declare_light_tracks`], [`TrackIn::light`]).
///   The main m-line is forwarded as any single-encoding
///   track; the light m-line is forwarded, every packet of it,
///   to a companion entry and to nothing else. No layer is
///   ever selected.
/// - **Simulcast.** One m-line with several rids. The main
///   track is on the heaviest layer the subscriber-bandwidth
///   policy allows, and a second entry for the same source
///   track, the companion, forwards the light layer only and
///   never switches.
///
/// Audio, and video with neither, have a main track only.
///
/// ## The middle track (ADR-0070 step 5)
///
/// A publisher may send a third ordinary sender, the camera at
/// half size, and name it in its offer
/// ([`MediaPeer::declare_middle_tracks`], [`TrackIn::middle`]).
/// A subscriber then has **three** downstream tracks of that
/// publisher: main, **middle** (MSID stream id = the source peer
/// id + [`MIDDLE_STREAM_SUFFIX`]) and light. All three are
/// negotiated; which of the two heavy ones carries packets is the
/// subscriber's to say ([`MediaPeer::set_video_wants`]): the main
/// track unless it asks for the middle one or for neither. The
/// light companion is forwarded always.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TrackRole {
    /// The one downstream track every publisher track has.
    #[default]
    Main,
    /// The light picture of a publisher's camera: its
    /// declared light sender, or the light layer of its
    /// simulcast track, forever.
    LightCompanion,
    /// The middle picture of a publisher's camera: its declared
    /// middle sender, forwarded only to a subscriber that asks
    /// for it.
    Middle,
}

impl TrackRole {
    /// Stable, low-cardinality label for logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::LightCompanion => "light",
            Self::Middle => "middle",
        }
    }
}

/// What a publisher knows about the layers of one simulcast
/// video track it sends ([`MediaPeer::simulcast_layout`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SimulcastLayout {
    /// The light layer: what a light companion forwards.
    /// Fixed once known.
    pub light: Option<Rid>,
    /// The heaviest layer: what a main track is put on when
    /// nothing says otherwise. `None` until it can be told.
    pub heaviest: Option<Rid>,
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
///   destination peer now has a send stream for
///   `local_mid` and the fanout can write packets to it.
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
    /// SDP answer applied; the send stream for
    /// `local_mid` is live and the fanout uses it as the
    /// destination.
    Open(Mid),
}

/// One outgoing track on a downstream peer that mirrors
/// an upstream [`TrackIn`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackOutEntry {
    /// Upstream peer that owns the source TrackIn.
    pub source_peer: MediaPeerId,
    /// Upstream side's mid (the mid every packet forwarded
    /// through this entry arrived on).
    pub source_mid: Mid,
    /// Audio / video — pinned at registration so we do
    /// not need to chase the upstream peer at fanout
    /// time.
    pub kind: MediaKind,
    /// Lifecycle phase. See [`TrackOutState`].
    pub state: TrackOutState,
    /// Main track or light companion. See [`TrackRole`].
    pub role: TrackRole,
    /// Simulcast layer selected for this downstream track.
    ///
    /// - `None` — no layer selected yet. A non-simulcast
    ///   source (no packet carries a rid) forwards as is.
    ///   For a simulcast source, the FIRST layer observed is
    ///   auto-latched here (see `layer_gate_allows`).
    /// - `Some(rid)` — this is the layer the track should be
    ///   forwarding. A bandwidth policy overrides the latch
    ///   via [`MediaPeer::select_layer`].
    ///
    /// The selected layer and the layer on the wire differ
    /// while a switch is pending: the track goes on
    /// forwarding its current layer until a packet begins an
    /// independent frame on the selected one.
    pub chosen_rid: Option<Rid>,
    /// What the track is forwarding right now, and where on
    /// the downstream stream it is.
    forwarding: Forwarding,
}

/// Per-track forwarding state: which source stream is on the
/// wire and how its packets are numbered downstream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Forwarding {
    /// Simulcast layer currently forwarded. `None` for a
    /// non-simulcast source, and before the first simulcast
    /// packet is forwarded.
    active_rid: Option<Rid>,
    /// SSRC of the source stream the map below is fed from.
    /// A packet from a different SSRC — the first packet of
    /// a new layer, or a publisher that changed SSRC —
    /// re-bases the map.
    source_ssrc: Option<Ssrc>,
    /// Sequence-number and timestamp mapping, created with
    /// the first packet forwarded.
    map: Option<StreamMap>,
    /// The publisher's uplink fallback ([`crate::uplink_fallback`])
    /// is holding this track on the light layer.
    held_light: bool,
    /// The layer the track was selected to when the fallback took
    /// it over, if it had one: where it goes back to.
    before_fallback: Option<Rid>,
    /// Packets of the selected layer held while the switch to it is
    /// pending. See [`SwitchWait`].
    waiting: SwitchWait,
    /// The track is to begin at a whole key frame of its selected
    /// layer: nothing is on the wire yet, and the packets of the
    /// selected layer are held ([`SwitchWait`]) until one is whole.
    /// Set when the SFU selected the layer itself before the first
    /// packet (a light companion, and a main track put on the
    /// heaviest layer), cleared when the stream starts or when the
    /// wait is given up ([`START_KEYFRAME_REQUESTS`]).
    start_at_key_frame: bool,
    /// When the publisher was last asked for the key frame this
    /// track is waiting to begin at.
    start_requested: Option<Instant>,
    /// How many times it has been asked.
    start_requests: u8,
    /// Whether the subscriber is being sent this track at all
    /// ([`MediaPeer::set_video_wants`]). See [`Gate`].
    gate: Gate,
    /// The next packet forwarded continues the downstream stream at
    /// the next unused sequence number, whatever its own: set when a
    /// paused track resumes, so the packets that went by during the
    /// pause leave no gap the subscriber would ask to have resent.
    rebase_next: bool,
}

/// Whether a downstream video track is being sent to its subscriber
/// (ADR-0070 step 5).
///
/// A subscriber says which picture of each publisher it shows
/// ([`MediaPeer::set_video_wants`]); the main and the middle track of a
/// publisher it does not show are **paused**: every packet offered to
/// them is dropped ([`MediaForward::NotWanted`]) and nothing is held.
/// Pausing takes effect with the next packet.
///
/// A paused track that is wanted again is **resuming**: its publisher is
/// asked for a key frame at once and again every
/// [`START_KEYFRAME_RETRY`], and the track starts at the first packet of
/// a key frame (by the frame descriptor's flags, header only). After
/// [`START_KEYFRAME_REQUESTS`] requests with none it starts at the next
/// packet instead, as a track waiting to begin does: a stream without a
/// frame descriptor never shows the SFU a key frame.
///
/// The downstream stream is continuous across a pause: the first packet
/// forwarded after it takes the next unused sequence number
/// ([`Forwarding::rebase_next`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Gate {
    /// Packets are forwarded.
    #[default]
    Flowing,
    /// The subscriber does not show this track.
    Paused,
    /// Wanted again and waiting for a key frame to start at.
    Resuming {
        /// When the publisher was last asked for the key frame.
        asked: Option<Instant>,
        /// How many times it has been asked.
        requests: u8,
        /// The wait was given up: the next packet starts the track.
        any_packet: bool,
    },
}

/// One heavy camera sender (main or middle) of a publisher that is told
/// which of its senders are needed ([`crate::video_needed`]), by the mid
/// it arrives on.
#[derive(Debug, Clone, Copy, Default)]
struct HeavySender {
    /// The SSRC its packets last arrived on.
    ssrc: Option<Ssrc>,
    /// When its last packet arrived.
    last_arrival: Option<Instant>,
    /// The publisher was told this sender is not needed, and no packet
    /// has arrived on it since it was told it is needed again (or it has
    /// not been told so yet): silence on it is the SFU's own doing.
    expect_silence: bool,
    /// It has gone silent while silence was expected
    /// ([`video_needed::IDLE_QUIET`]): its loss books are closed. Cleared
    /// by its next packet.
    idle: bool,
    /// When it last came back from being idle.
    returned_at: Option<Instant>,
    /// The rate last measured on it while it was flowing, and when.
    last_rate: Option<(u64, Instant)>,
}

/// What a publisher has been told of its senders, and what follows from
/// it for the senders themselves ([`crate::video_needed`]).
#[derive(Debug, Default)]
struct SenderNeeds {
    /// What it was told, and since when each sender has been unneeded.
    tracker: VideoNeeded,
    /// Its heavy senders, by mid. At most [`MAX_TRACK_METERS`] entries.
    senders: HashMap<Mid, HeavySender>,
    /// The publisher's bitrate estimate when a sender of its was first
    /// told to stop: what the estimate is put back to when one is told
    /// to start again. `None` while every sender is needed.
    estimate_before_idle: Option<u64>,
}

/// What became of one pass of [`MediaPeer::drive_video_needed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NeededChange {
    /// What the publisher is to be told now.
    pub(crate) needed: Needed,
    /// How many subscribers want each of its heavy pictures.
    pub(crate) wanted: Wanted,
}

/// What became of one pass of [`MediaPeer::drive_publish_budget`]: a
/// budget the publisher is to be told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BudgetChange {
    /// The two ceilings.
    pub(crate) budget: publish_budget::Budget,
    /// Why it is told now.
    pub(crate) why: publish_budget::Why,
    /// What it was made from.
    pub(crate) inputs: publish_budget::Inputs,
}

/// How long a track that is waiting to begin at a key frame
/// ([`Forwarding::start_at_key_frame`]) waits before the publisher is
/// asked again. Chosen, as [`FALLBACK_KEYFRAME_RETRY`] is: a key frame
/// already on its way is given time to arrive.
const START_KEYFRAME_RETRY: Duration = Duration::from_secs(1);

/// After this many requests without a whole key frame, a track stops
/// waiting and begins at the next packet of its layer. Chosen. A stream
/// that carries no frame descriptor never shows the SFU a key frame, and
/// without the bound its publisher would be asked once a second for as
/// long as the track is open.
const START_KEYFRAME_REQUESTS: u8 = 5;

/// Most packets of a selected layer held for one downstream track while
/// its switch to that layer is pending.
///
/// Chosen, not measured: a key frame of a light layer and the frames
/// written behind it while it is being repaired, at the cost of at most
/// this many payloads per subscriber track for as long as a switch is
/// pending. A key frame of more packets than this is not waited for whole
/// (see [`SwitchWait::offer`]).
const SWITCH_WAIT_PACKETS: usize = 64;

/// A packet held longer than this is dropped from a [`SwitchWait`].
/// Chosen: a key frame that has not become whole in this time is given
/// up for the next one.
const SWITCH_WAIT_MAX_AGE: Duration = Duration::from_secs(1);

/// The packets of the selected layer a downstream track holds while its
/// switch to that layer is pending, and the rule for when the switch
/// happens.
///
/// ## The rule
///
/// A track switches layers at a key frame of the selected layer, and only
/// once **every packet of that key frame has reached the SFU**. Until then
/// the layer on the wire keeps being forwarded. When the key frame is
/// whole, its packets go out in stream order, followed by whatever of the
/// selected layer arrived behind it.
///
/// ## Why not at the key frame's first packet
///
/// A decoder cannot show a key frame before its last packet arrives, so
/// holding the earlier ones costs the viewer nothing. Switching at the
/// first packet does cost something when the publisher's uplink is losing
/// packets, which is exactly when the uplink fallback
/// ([`crate::uplink_fallback`]) selects the light layer: the rest of the
/// key frame is still being resent, and until it is whole the subscriber
/// has neither the new layer's picture nor the old layer's, which was
/// still making progress. Measured in
/// `the_uplink_fallback_carries_a_subscriber_through_a_dip`. And the
/// first packet is as likely to be lost and resent as any other: by the
/// time it arrives the rest of its frame has gone by, and a stream that
/// starts at it starts with a key frame that can never be completed.
///
/// ## What is read
///
/// Header fields only: the sequence number, the RTP timestamp, the marker
/// bit and the frame descriptor's independent-frame flag. The payloads are
/// held and sent, never read (invariant #2, C-39).
///
/// Two tracks compare equal whatever they hold here: the list is
/// transient, and payloads are not compared anywhere.
#[derive(Debug, Clone, Default)]
struct SwitchWait {
    /// Held media packets of one source stream, ascending by sequence
    /// number, no duplicates.
    packets: Vec<RelayPacket>,
    /// Sequence numbers on that stream taken by the publisher's FEC
    /// packets, which are never forwarded: not missing media.
    fec_seqs: Vec<u64>,
}

impl PartialEq for SwitchWait {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for SwitchWait {}

impl SwitchWait {
    fn clear(&mut self) {
        self.packets.clear();
        self.fec_seqs.clear();
    }

    /// Note that `seq_no` on the held stream is an FEC packet.
    fn note_fec(&mut self, seq_no: u64) {
        if self.fec_seqs.len() >= SWITCH_WAIT_PACKETS {
            self.fec_seqs.remove(0);
        }
        self.fec_seqs.push(seq_no);
    }

    /// Hold `packet`, and return the packets to send if the switch can
    /// happen now: a key frame from its first packet on, in stream order,
    /// with everything held behind it. `None` while it cannot.
    ///
    /// The switch happens at the newest key frame that is whole: its first
    /// packet (the frame descriptor says it begins an independent frame),
    /// then consecutive sequence numbers with the same RTP timestamp up to
    /// one with the marker bit. When more than [`SWITCH_WAIT_PACKETS`] are
    /// held and none is whole, the switch happens at the oldest first
    /// packet of a key frame held, whole or not (a key frame larger than
    /// the bound is never whole here, and the track must not be left
    /// waiting for one); with no such packet the oldest held packet is
    /// dropped.
    fn offer(&mut self, packet: &RelayPacket) -> Option<Vec<RelayPacket>> {
        if self.packets.first().is_some_and(|p| p.ssrc != packet.ssrc) {
            self.clear();
        }
        self.packets
            .retain(|p| packet.arrival.saturating_duration_since(p.arrival) <= SWITCH_WAIT_MAX_AGE);
        if let Err(at) = self
            .packets
            .binary_search_by_key(&*packet.seq_no, |p| *p.seq_no)
        {
            self.packets.insert(at, packet.clone());
        }
        let start = self.newest_whole_key_frame().or_else(|| {
            if self.packets.len() > SWITCH_WAIT_PACKETS {
                self.packets.iter().position(|p| p.starts_independent_frame)
            } else {
                None
            }
        });
        let Some(start) = start else {
            if self.packets.len() > SWITCH_WAIT_PACKETS {
                self.packets.remove(0);
            }
            return None;
        };
        let mut run = self.packets.split_off(start);
        self.clear();
        // They leave now, together.
        for held in &mut run {
            held.arrival = packet.arrival;
        }
        Some(run)
    }

    /// Index of the first packet of the newest key frame held whole.
    fn newest_whole_key_frame(&self) -> Option<usize> {
        (0..self.packets.len()).rev().find(|&start| {
            let first = &self.packets[start];
            if !first.starts_independent_frame {
                return false;
            }
            let mut expected = *first.seq_no;
            for held in &self.packets[start..] {
                while *held.seq_no > expected && self.fec_seqs.contains(&expected) {
                    expected += 1;
                }
                if *held.seq_no != expected || held.rtp_time != first.rtp_time {
                    return false;
                }
                if held.marker {
                    return true;
                }
                expected += 1;
            }
            false
        })
    }
}

impl Forwarding {
    /// Downstream position for `packet`, starting or
    /// re-basing the map as needed. `None` when the packet
    /// has no slot downstream (see [`MediaForward::Stale`]).
    fn position(&mut self, packet: &RelayPacket) -> Option<crate::stream_map::Mapped> {
        let seq_no = *packet.seq_no;
        let Some(map) = self.map.as_mut() else {
            let (map, first) = StreamMap::start(seq_no, packet.rtp_time, packet.arrival);
            self.map = Some(map);
            self.source_ssrc = Some(packet.ssrc);
            return Some(first);
        };
        let clock_hz = packet.params.spec().clock_rate.get();
        if self.source_ssrc == Some(packet.ssrc) && !self.rebase_next {
            return map.map(seq_no, packet.rtp_time, packet.arrival, clock_hz);
        }
        // A new source stream, or the same one after a pause: either
        // way the downstream stream continues at its next number.
        self.rebase_next = false;
        self.source_ssrc = Some(packet.ssrc);
        Some(map.rebase(seq_no, packet.rtp_time, packet.arrival, clock_hz))
    }
}

/// What a video packet that arrived under the `red` payload
/// type carries ([`crate::video_fec`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RedBody {
    /// A media packet: the body after the RED header byte is
    /// the media payload.
    Media,
    /// An FEC packet (RFC 5109). It ends at the SFU: the
    /// publisher's FEC receiver takes it and no subscriber is
    /// sent it.
    Fec,
}

/// One RTP packet received from a publisher, with what a
/// destination peer needs to relay it.
///
/// Built once per packet on the publishing peer
/// (`MediaPeer::relay_packet`) and offered to every other
/// peer of the call. The payload is opaque: nothing in the
/// SFU reads it.
#[derive(Debug, Clone)]
// Each flag is one independent fact read off the packet's header.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct RelayPacket {
    /// Publisher-side mid the packet arrived on.
    pub(crate) mid: Mid,
    /// Simulcast layer, when the source is simulcast.
    pub(crate) rid: Option<Rid>,
    /// Audio or video.
    pub(crate) kind: MediaKind,
    /// The source stream's main SSRC (a retransmission is
    /// already rewritten onto it by str0m).
    pub(crate) ssrc: Ssrc,
    /// Extended sequence number on the source stream.
    pub(crate) seq_no: SeqNo,
    /// RTP timestamp as carried in the packet header.
    pub(crate) rtp_time: u32,
    /// Marker bit: last packet of a video frame, start of a
    /// talk spurt for audio.
    pub(crate) marker: bool,
    /// The publisher's negotiated codec parameters for the
    /// packet's payload type. Matched against each
    /// subscriber's negotiation to find the payload type to
    /// send under. For a RED-wrapped video media packet these
    /// are the parameters of the media payload type the RED
    /// header names, so the packet is handled as the media
    /// packet it carries; for an FEC packet, the publisher's
    /// `red` parameters.
    pub(crate) params: PayloadParams,
    /// Video only: the packet arrived under the `red` payload
    /// type, and what its one block is. `payload` then still
    /// begins with the RED header byte.
    pub(crate) red: Option<RedBody>,
    /// The packet did not arrive: the SFU rebuilt it from the
    /// publisher's FEC ([`crate::fec_receive`]). Forwarded
    /// exactly like a packet that did; counted apart.
    pub(crate) recovered: bool,
    /// When the packet reached the SFU.
    pub(crate) arrival: Instant,
    /// Video orientation header extension, if carried.
    ///
    /// There is deliberately no field for the audio level or
    /// the voice-activity flag (RFC 6464): the SFU does not
    /// negotiate that extension ([`session_extension_map`]),
    /// and a packet in transit has nowhere to hold a value a
    /// peer sent anyway.
    pub(crate) video_orientation: Option<VideoOrientation>,
    /// The generic frame descriptor exactly as this packet
    /// carried it, relayed verbatim.
    pub(crate) descriptor: Option<PacketDescriptor>,
    /// The packet is the first of a frame with no
    /// dependencies, by the descriptor's own flags: the one
    /// place a simulcast switch may take effect.
    pub(crate) starts_independent_frame: bool,
    /// The packet carries no frame descriptor at all (and is not FEC):
    /// nothing in its header says where a frame or a key frame begins.
    pub(crate) blind: bool,
    /// RTP payload, without header. SFrame ciphertext; after
    /// one RED header byte when `red` is set.
    pub(crate) payload: Vec<u8>,
}

impl RelayPacket {
    /// Whether this packet completes a unit the per-origin
    /// ledger counts as a frame: every audio packet, and the
    /// marker-bit packet of a video frame.
    pub(crate) const fn ends_frame(&self) -> bool {
        match self.kind {
            MediaKind::Audio => true,
            MediaKind::Video => self.marker && !self.is_fec(),
        }
    }

    /// Whether this is a video FEC packet: not media, so not
    /// part of any frame.
    pub(crate) const fn is_fec(&self) -> bool {
        matches!(self.red, Some(RedBody::Fec))
    }

    /// Length of the media bytes: the payload without the RED
    /// header byte when there is one.
    pub(crate) fn media_len(&self) -> usize {
        match self.red {
            Some(_) => self.payload.len().saturating_sub(video_fec::RED_HEADER_LEN),
            None => self.payload.len(),
        }
    }

    /// Header extension values for the outgoing packet:
    /// what is the stream's own and travels with it. `mid`,
    /// `rid`, the transport-wide sequence number and
    /// absolute send time are deliberately left unset; str0m
    /// fills them in for the egress leg. The audio level and
    /// the voice-activity flag are never set: who is speaking,
    /// and how loudly, is not the server's to relay.
    fn egress_extensions(&self) -> ExtensionValues {
        let mut ext_vals = ExtensionValues {
            video_orientation: self.video_orientation,
            ..ExtensionValues::default()
        };
        if let Some(descriptor) = &self.descriptor {
            ext_vals.user_values.set(descriptor.clone());
        }
        ext_vals
    }
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
    /// `true` once str0m reported `Event::Connected` (ICE
    /// and DTLS are up, so SRTP keys exist). Packets are
    /// written to this peer only from then on: str0m's send
    /// queue in RTP mode is unbounded, and a peer whose
    /// transport never comes up would otherwise collect
    /// every packet of the call for as long as its
    /// signaling socket stayed open.
    connected: bool,
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
    /// `MediaPeer::forward_packet` when it needs to
    /// resolve `(source_peer, source_mid)` to the local
    /// mid of the send stream.
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
    /// populated for simulcast tracks THIS peer publishes (packets carrying a
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
    /// Keyframe requests that arrived inside the rate-limit window, keyed
    /// `(mid, rid)`. A request is never dropped: it is kept here and sent by
    /// [`MediaPeer::send_owed_keyframe_requests`] as soon as the window ends.
    /// A subscriber that asks once and then waits (seen on a live call: its
    /// one request fell inside the window opened by the track-open request,
    /// and its next came nine seconds later) is otherwise left without a
    /// picture for as long as it stays quiet.
    owed_keyframe_requests: std::collections::HashSet<(Mid, Option<Rid>)>,
    /// Requests sent to THIS (publishing) peer on each of its tracks
    /// since a key frame was last seen arriving on it
    /// ([`KEYFRAME_UNANSWERED_MAX`]).
    unanswered_keyframe_requests: HashMap<(Mid, Option<Rid>), u32>,
    /// THIS (subscribing) peer's own key frame requests, per downstream
    /// mid of its own ([`PLI_RELAY_FIRST_INTERVAL`]).
    pli_relays: HashMap<Mid, PliRelay>,
    /// Whether video FEC is switched on (`LAKKO_SFU_VIDEO_FEC`).
    /// Off, nothing below is ever touched.
    video_fec: bool,
    /// ULPFEC receive state for the video streams THIS peer
    /// publishes, by SSRC ([`crate::fec_receive`]). At most
    /// [`MAX_FEC_STREAMS`] entries.
    fec_receivers: HashMap<Ssrc, FecReceiver>,
    /// ULPFEC send state for the video tracks THIS peer
    /// subscribes to with `red` and `ulpfec` negotiated, by the
    /// peer's own mid ([`crate::fec_send`]). An entry appears
    /// with the first RED-wrapped media packet written to the
    /// track. At most [`MAX_FEC_LEGS`] entries.
    fec_senders: HashMap<Mid, LegFec>,
    /// One entry per FEC packet written to this peer since the
    /// media loop last collected them: the publisher and
    /// publisher mid of the track it protects, and the role of
    /// the downstream track. Drained every loop pass
    /// ([`MediaPeer::take_fec_sent`]).
    fec_sent: Vec<(MediaPeerId, Mid, TrackRole)>,
    /// Bitrate control for the media THIS peer publishes
    /// (ADR-0068): the estimate of what its path carries and the
    /// REMB bookkeeping. `None` under [`BitrateControl::Client`],
    /// and then nothing of it is ever touched. Boxed so a peer
    /// without it is no larger for the feature.
    uplink: Option<Box<Uplink>>,
    /// Whether the uplink fallback ([`crate::uplink_fallback`]) runs
    /// for the simulcast video THIS peer publishes. Off unless the
    /// operator switched it on (`LAKKO_SFU_UPLINK_FALLBACK`).
    uplink_fallback_on: bool,
    /// The fallback policy of each simulcast video track THIS peer
    /// publishes, by mid. An entry appears with the first video
    /// packet that carries a rid, so a publisher without simulcast
    /// has none. At most [`MAX_FALLBACK_TRACKS`] entries.
    uplink_fallbacks: HashMap<Mid, UplinkFallback<Rid>>,
    /// The highest bitrate measured so far on each simulcast layer THIS
    /// peer publishes, keyed like `layer_meters` and bounded by it.
    /// Which layer is the light one and which the heaviest is read from
    /// these, not from the current rates: a layer's current rate
    /// collapses while its packets are being lost, and the layers'
    /// first measurements do not complete in the same pass.
    layer_peak_bps: HashMap<(Mid, Rid), u64>,
    /// The light layer of each simulcast video track THIS peer
    /// publishes, by mid, from the moment it was first told
    /// ([`MediaPeer::simulcast_layout`]). Never changed afterwards: a
    /// light companion forwards one layer for as long as it lives.
    /// Bounded like `layer_meters`, whose mids are the only keys.
    light_rids: HashMap<Mid, Rid>,
    /// The mid of the video m-line THIS peer declared, in an SDP
    /// offer, to carry the light copy of its camera
    /// ([`MediaPeer::declare_light_tracks`]). At most one, the first
    /// that was accepted, and never changed afterwards.
    light_mid: Option<Mid>,
    /// The mid of the video m-line THIS peer declared to carry the
    /// middle copy of its camera
    /// ([`MediaPeer::declare_middle_tracks`]). At most one, the first
    /// that was accepted, and never changed afterwards.
    middle_mid: Option<Mid>,
    /// Which picture of each publisher THIS (subscribing) peer shows
    /// ([`MediaPeer::set_video_wants`]). A publisher without an entry
    /// is [`VideoQuality::Main`]. At most [`MAX_VIDEO_WANTS`] entries.
    video_wants: HashMap<MediaPeerId, VideoQuality>,
    /// What this peer does to carry video through a burst of loss
    /// (ADR-0070 steps 3 and 4).
    protection: LossProtection,
    /// SSRCs of the light video streams THIS peer publishes that have
    /// been put on the faster NACK schedule
    /// ([`LossProtection::light_nack_retry`]). At most
    /// [`MAX_FALLBACK_TRACKS`] entries.
    fast_nack_ssrcs: Vec<Ssrc>,
    /// Second copies of light companion packets waiting to be sent to
    /// THIS (subscribing) peer.
    light_duplicates: LightDuplicates,
    /// One entry per second copy sent to this peer since the media loop
    /// last collected them, as [`Self::fec_sent`] keeps FEC packets.
    duplicates_sent: Vec<(MediaPeerId, Mid, TrackRole)>,
    /// Bursts of loss on the uplink of the video THIS peer publishes.
    uplink_bursts: UplinkBursts,
    /// The burst each video stream THIS peer publishes is in, by
    /// `(mid, rid)`: what says when a burst has ended unrepaired and the
    /// publisher is to be asked for a key frame
    /// ([`crate::burst_keyframe`], [`drive_burst_keyframes`]).
    burst_ends: BurstEnds<VideoStream>,
    /// Bursts of loss on the downlink of THIS (subscribing) peer.
    downlink_bursts: DownlinkBursts,
    /// Video packets written to this peer (first transmissions), and how
    /// many receiver reports have been read per downstream track, for
    /// [`Self::poll_downlink_burst`].
    video_sent: u64,
    reports_read: HashMap<Mid, u64>,
    /// The playout delay of each publisher's video toward THIS
    /// (subscribing) peer. An entry exists for every publisher this peer
    /// has a video track of ([`drive_playout_delay`]).
    playout: HashMap<MediaPeerId, PairDelay>,
    /// Downlink control for THIS (subscribing) peer (ADR-0070 step 6):
    /// the allocation of what it is sent of each publisher's video, and
    /// its bookkeeping. `None` unless the peer was built with it
    /// ([`Self::with_downlink_control`]), and then nothing of it is ever
    /// touched. Boxed so a peer without it is no larger for the feature.
    downlink: Option<Box<Downlink>>,
    /// The rate arriving on each track THIS (publishing) peer sends, by
    /// mid: what a subscriber's allocation takes the track to cost. Only
    /// kept under downlink control. At most [`MAX_TRACK_METERS`] entries.
    track_meters: HashMap<Mid, LayerBitrateMeter>,
    /// Which of the camera senders THIS (publishing) peer has somebody is
    /// being sent, what it was told of that, and which of them have gone
    /// idle on the SFU's word ([`crate::video_needed`]).
    needs: SenderNeeds,
    /// What this publisher was told its camera senders may send
    /// ([`crate::publish_budget`]).
    budget: PublishBudget,
}

/// How many publishers a subscriber's video wants name at each quality
/// ([`MediaPeer::set_video_wants`]), for the log line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoWantsCount {
    /// Named at `main`.
    pub main: usize,
    /// Named at `middle`.
    pub middle: usize,
    /// Named at `light`.
    pub light: usize,
}

/// Why a mid named in an offer's `light_mids` or `middle_mids` was not
/// taken as this peer's light or middle track
/// ([`MediaPeer::declare_light_tracks`],
/// [`MediaPeer::declare_middle_tracks`]). The track, if there is one,
/// stays what it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightDeclarationRefused {
    /// The peer has no m-line with that mid.
    UnknownMid,
    /// The m-line is not video.
    NotVideo,
    /// The m-line is one the SFU sends on: a downstream track of this
    /// peer, or a line the peer does not send on.
    NotSentByPeer,
    /// The track was announced without the declaration and may already be
    /// open on subscribers as a main track.
    AlreadyMain,
    /// The peer already has a track of this kind (light, or middle) on
    /// another mid.
    AlreadyDeclared,
    /// The mid is already the peer's other declared copy: its light
    /// track when a middle one is being declared, and the reverse.
    DeclaredOtherwise,
}

impl LightDeclarationRefused {
    /// Stable, low-cardinality label for logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnknownMid => "unknown-mid",
            Self::NotVideo => "not-video",
            Self::NotSentByPeer => "not-sent-by-peer",
            Self::AlreadyMain => "already-a-main-track",
            Self::AlreadyDeclared => "another-track-of-this-kind-declared",
            Self::DeclaredOtherwise => "declared-as-the-other-copy",
        }
    }
}

/// Most simulcast video tracks of one publisher the SFU keeps an uplink
/// fallback policy for. A track beyond that starts the table over. Chosen:
/// camera and screen share with room to spare.
const MAX_FALLBACK_TRACKS: usize = 8;

/// The rid of the light simulcast layer by convention, used until the
/// layers' bitrates have been measured: the Lakko clients send `q` (light)
/// and `f` (full).
const LIGHT_RID: &str = "q";

/// Most downstream video tracks of one subscriber the SFU keeps FEC send
/// state for. A track beyond that starts the table over. Each entry can hold
/// up to [`crate::fec_send::MEDIA_WINDOW`] packets.
const MAX_FEC_LEGS: usize = 32;

/// Where an FEC packet the SFU generates for a subscriber goes on that
/// subscriber's stream: the slot the publisher's FEC packet has there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FecSlot {
    /// Extended downstream sequence number.
    seq_no: u64,
    /// Downstream RTP timestamp.
    rtp_time: u32,
    /// The wall clock that timestamp stands for.
    wallclock: Instant,
}

/// FEC send state for one downstream video track of a subscriber.
#[derive(Debug)]
struct LegFec {
    /// SSRC the track is sent on.
    ssrc: Ssrc,
    /// The subscriber's `red` payload type on this track.
    red_pt: str0m::media::Pt,
    /// The subscriber's `ulpfec` payload type on this track.
    ulpfec_pt: str0m::media::Pt,
    /// Publisher, publisher mid and role of the track, for the ledger.
    source: (MediaPeerId, Mid, TrackRole),
    sender: FecSender<FecSlot>,
}

/// The sequence number an extended sequence number has on the wire.
fn wire_seq(seq_no: u64) -> u16 {
    u16::try_from(seq_no & 0xffff).unwrap_or(0)
}

/// Most video streams of one publisher the SFU keeps FEC receive state for.
/// A stream beyond that starts the table over. Chosen to cover three
/// simulcast layers with room to spare; each entry can hold a few hundred
/// kilobytes (see [`crate::fec_receive`]).
const MAX_FEC_STREAMS: usize = 8;

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

/// After this many requests of the SFU's own (a track opening, starting
/// or resuming) on one publisher track with no key frame seen arriving on
/// it since, the SFU stops asking at the rate above. Chosen: the number a
/// track that waits to start is given (`START_KEYFRAME_REQUESTS`). A
/// publisher whose packets carry no frame descriptor (Firefox negotiates
/// none) never shows the SFU a key frame, and neither does a sender that
/// is switched off.
///
/// **A subscriber's own request is not counted against this.** Whether a
/// key frame arrived is something the SFU cannot see on such a stream,
/// and the subscriber can: one that decodes stops asking. Its requests
/// are bounded by their own rule ([`PLI_RELAY_FIRST_INTERVAL`]). (Seen on
/// a live call, 2026-10-07: a viewer of a Firefox publisher's main track
/// received packets it could not decode for more than half a minute,
/// asking five times a second, while this bound let one request through
/// in ten seconds.)
pub const KEYFRAME_UNANSWERED_MAX: u32 = 5;

/// And then asks at most once per this long, until a key frame is seen.
/// Chosen.
pub const KEYFRAME_UNANSWERED_RETRY: Duration = Duration::from_secs(10);

/// A subscriber's own key frame request (PLI) on one of its tracks is
/// relayed at once, and while it goes on asking, again after this long ...
pub const PLI_RELAY_FIRST_INTERVAL: Duration = Duration::from_secs(1);

/// ... doubling up to this: 1, 2, 4 and then every 8 s. A subscriber
/// that still asks was sent a key frame that did not help it, or the
/// request or the key frame was lost; the SFU cannot tell which, so it
/// goes on asking for it, less often. A receiver that cannot decode the
/// stream at all asks for ever (on a live call one such receiver cost
/// every publisher a key frame a second on two senders); it now costs
/// one in eight seconds. Both chosen.
pub const PLI_RELAY_MAX_INTERVAL: Duration = Duration::from_secs(8);

/// A subscriber that has sent no request on a track for this long has
/// what it asked for (a decoder that has a picture stops asking): its
/// next request is a new need and is relayed at once. Chosen: several
/// times the 200 ms to 1 s a browser leaves between two requests.
pub const PLI_RELAY_QUIET: Duration = Duration::from_secs(2);

/// One subscriber's key frame requests on one of its downstream tracks
/// ([`PLI_RELAY_FIRST_INTERVAL`]).
#[derive(Debug, Clone, Copy)]
struct PliRelay {
    /// When a request was last relayed.
    relayed: Instant,
    /// How long after it the next may be.
    wait: Duration,
    /// When the subscriber last asked, relayed or not.
    asked: Instant,
}

/// Whether a subscriber's key frame request at `now` is relayed, and the
/// state after it. Pure.
fn pli_relay_decision(state: Option<PliRelay>, now: Instant) -> (bool, PliRelay) {
    let fresh = PliRelay {
        relayed: now,
        wait: PLI_RELAY_FIRST_INTERVAL,
        asked: now,
    };
    let Some(state) = state else {
        return (true, fresh);
    };
    // Quiet since: it had what it asked for, and this is a new need.
    if now.saturating_duration_since(state.asked) >= PLI_RELAY_QUIET {
        return (true, fresh);
    }
    if now.saturating_duration_since(state.relayed) < state.wait {
        return (
            false,
            PliRelay {
                asked: now,
                ..state
            },
        );
    }
    (
        true,
        PliRelay {
            relayed: now,
            wait: (state.wait * 2).min(PLI_RELAY_MAX_INTERVAL),
            asked: now,
        },
    )
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
    /// A request that arrived inside the rate-limit window, sent when the
    /// window ended.
    Owed,
    /// The publisher's uplink is in trouble and its subscribers are being
    /// moved to its light layer ([`crate::uplink_fallback`]); also the
    /// repeats while that switch has not happened yet.
    UplinkFallback,
    /// The publisher's uplink is clean again and a subscriber is being
    /// moved back to the layer it is to have.
    UplinkRestore,
    /// A subscriber the SFU has no bandwidth estimate for is moved from
    /// the layer it happened to latch to the publisher's heaviest one.
    FullLayerDefault,
    /// A downstream track of a simulcast source is open, was given its
    /// layer by the SFU and is waiting for a whole key frame of it to
    /// begin at: a light companion, or a main track put on the heaviest
    /// layer. Repeated until the track has begun.
    TrackStart,
    /// A downstream track the subscriber did not show is shown again
    /// (ADR-0070 step 5) and waits for a key frame to resume at.
    /// Repeated until the track has started.
    TrackResume,
    /// A burst of loss on the publisher's uplink has ended and left
    /// packets of this stream that were not repaired
    /// ([`crate::burst_keyframe`]). Once per burst per stream.
    UplinkBurstEnd,
}

impl KeyframeReason {
    /// Stable, low-cardinality label for logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DownstreamOpened => "downstream-opened",
            Self::SubscriberRequest => "subscriber-pli",
            Self::Owed => "owed",
            Self::UplinkFallback => "uplink-fallback",
            Self::UplinkRestore => "uplink-restore",
            Self::FullLayerDefault => "full-layer-default",
            Self::TrackStart => "track-start",
            Self::TrackResume => "track-resume",
            Self::UplinkBurstEnd => "uplink-burst-end",
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
    /// Main track or light companion.
    pub role: TrackRole,
    /// Simulcast layer the subscriber is latched to, if any.
    pub rid: Option<Rid>,
}

/// Whether a RED packet (redundant audio, RFC 2198) with the publisher's
/// parameters `source` may be written to this peer's track `local_mid` under
/// the peer's own `red` parameters `matched`.
///
/// Two conditions, both read from the SDP negotiation and neither from the
/// packet's payload:
///
/// - the track's media line negotiated the `red` payload type. The codec
///   configuration `matched` comes from is per session, and a peer that
///   offered `red` on the track it publishes may still have answered the
///   SFU's offer for this track with Opus alone;
/// - both `red` entries name the same Opus payload type. A RED payload holds
///   that number inside block headers that SFrame has sealed, so the SFU
///   cannot renumber it and the subscriber must already know Opus by the
///   number the publisher used.
fn red_negotiated(
    rtc: &Rtc,
    local_mid: Mid,
    matched: &PayloadParams,
    source: &PayloadParams,
) -> bool {
    let on_track = rtc
        .media(local_mid)
        .is_some_and(|media| media.remote_pts().contains(&matched.pt()));
    let inner = |params: &PayloadParams| params.spec().format.red.map(|red| red.pt);
    on_track && inner(matched).is_some() && inner(matched) == inner(source)
}

/// The `red` and `ulpfec` payload types of this peer for the track
/// `local_mid`, when that track's media line negotiated both
/// ([`crate::video_fec`]). The codec configuration is per session; a peer
/// that offered the pair on the track it publishes may still have answered
/// the SFU's offer for this track without it, so the track's own payload
/// type list decides.
fn video_fec_negotiated(rtc: &Rtc, local_mid: Mid) -> Option<(str0m::media::Pt, str0m::media::Pt)> {
    let config = rtc.codec_config();
    let red = config.find(|p| p.spec().codec == Codec::RedVideo)?.pt();
    let ulpfec = config.find(|p| p.spec().codec == Codec::Ulpfec)?.pt();
    let pts = rtc.media(local_mid)?.remote_pts();
    (pts.contains(&red) && pts.contains(&ulpfec)).then_some((red, ulpfec))
}

/// How often the SFU asks a publisher again for a missing packet of its
/// light video: a declared light track, or the light layer of a simulcast
/// track ([ADR-0070](../../../docs/adr/0070-call-media-through-a-loss-burst.md)
/// step 3). Every other stream is asked every 60 ms (the vendored `str0m`'s
/// default). Chosen, not measured: at the light track's rate (tens of
/// packets a second) the extra requests cost little, and in a second that
/// loses seven datagrams in ten a packet needs several tries. A libwebrtc
/// sender ignores a request for a packet it resent less than one round trip
/// ago, so on a path slower than this some requests are spent for nothing.
pub const LIGHT_NACK_RETRY: Duration = Duration::from_millis(20);

/// How long after a packet of a light companion track was sent to a
/// subscriber it is sent a second time (ADR-0070 step 3). Chosen, not
/// measured: long enough that one short gap on the subscriber's downlink
/// does not take both copies, short enough that the copy is in time for a
/// viewer playing a few hundred milliseconds late.
pub const LIGHT_DUPLICATE_DELAY: Duration = Duration::from_millis(30);

/// Most second copies waiting to be sent to one subscriber. One more is
/// dropped, silently: the packet has been sent once and can still be asked
/// for. Chosen: several seconds of every light companion of a ten-person
/// call.
pub const MAX_LIGHT_DUPLICATES: usize = 2048;

/// The RTP header extension id the playout-delay extension is registered
/// under in a peer's session: 5, the id libwebrtc offers it on.
pub const PLAYOUT_DELAY_EXTENSION_ID: u8 = 5;

/// What the SFU does to carry video through a burst of loss
/// ([ADR-0070](../../../docs/adr/0070-call-media-through-a-loss-burst.md)
/// steps 3 and 4), and one switch of the same shape that is not about loss
/// ([`Self::video_needed`]). The default is everything on.
// Five independent on/off switches, each an environment variable.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LossProtection {
    /// Send every packet of a light companion track to a subscriber a
    /// second time, [`LIGHT_DUPLICATE_DELAY`] after the first
    /// (`LAKKO_SFU_LIGHT_DUPLICATE`, on unless set to `0`).
    pub light_duplicate: bool,
    /// How often a publisher is asked again for a missing packet of its
    /// light video. `None` leaves the light video on the schedule of every
    /// other stream; production uses [`LIGHT_NACK_RETRY`], and only the
    /// measurement that compares the two sets `None`.
    pub light_nack_retry: Option<Duration>,
    /// Stamp a minimum playout delay on the video sent to a subscriber
    /// after a burst of loss ([`crate::playout_delay`];
    /// `LAKKO_SFU_PLAYOUT_DELAY`, on unless set to `0`). Off, the
    /// extension is neither offered nor answered.
    pub playout_delay: bool,
    /// Ask a publisher for a key frame when a burst of loss on one of its
    /// video streams has ended unrepaired, and stop asking it for the
    /// packets of that burst ([`crate::burst_keyframe`];
    /// `LAKKO_SFU_BURST_KEYFRAME`, on unless set to `0`).
    pub burst_keyframe: bool,
    /// Tell a publisher which of its camera senders somebody is being
    /// sent, so it can stop the others ([`crate::video_needed`];
    /// `LAKKO_SFU_VIDEO_NEEDED`, on unless set to `0`). Not a matter of
    /// loss; it rides here because it is a switch of the same shape,
    /// decided per peer. Off, no `video-needed` frame is ever sent and
    /// nothing below it is touched.
    pub video_needed: bool,
    /// Tell a publisher how much its main and its middle camera sender
    /// may send, in a `publish-budget` frame made from the SFU's estimate
    /// of its uplink ([`crate::publish_budget`];
    /// `LAKKO_SFU_PUBLISH_BUDGET`, on unless set to `0`). Only under
    /// [`BitrateControl::Server`]: without the SFU's own estimate there
    /// is nothing to make a budget from, and the switch does nothing.
    /// Off, no such frame is ever sent; REMB is the same either way.
    pub publish_budget: bool,
}

impl Default for LossProtection {
    fn default() -> Self {
        Self {
            light_duplicate: true,
            light_nack_retry: Some(LIGHT_NACK_RETRY),
            playout_delay: true,
            burst_keyframe: true,
            video_needed: true,
            publish_budget: true,
        }
    }
}

impl LossProtection {
    /// Nothing of ADR-0070 steps 3 and 4, and no key frame asked for at
    /// the end of a burst: the behaviour before them.
    #[must_use]
    pub const fn off() -> Self {
        Self {
            light_duplicate: false,
            light_nack_retry: None,
            playout_delay: false,
            burst_keyframe: false,
            video_needed: false,
            publish_budget: false,
        }
    }
}

/// The second copies of light companion packets waiting to be sent to one
/// subscriber: when each is due, the subscriber's own mid of the track and
/// the downstream sequence number. Bounded by [`MAX_LIGHT_DUPLICATES`].
#[derive(Debug, Default)]
struct LightDuplicates {
    queue: std::collections::VecDeque<(Instant, Mid, u64)>,
}

impl LightDuplicates {
    /// Queue one copy. `false`, and nothing queued, when the queue is full.
    fn push(&mut self, due: Instant, local_mid: Mid, seq_no: u64) -> bool {
        if self.queue.len() >= MAX_LIGHT_DUPLICATES {
            return false;
        }
        self.queue.push_back((due, local_mid, seq_no));
        true
    }

    /// The oldest copy, if it is due at `now`. Each copy is handed out
    /// once. Copies are queued in the order their packets were sent, so a
    /// copy is never held behind one that is due much later.
    fn pop_due(&mut self, now: Instant) -> Option<(Mid, u64)> {
        let (due, _, _) = self.queue.front()?;
        if *due > now {
            return None;
        }
        self.queue.pop_front().map(|(_, mid, seq)| (mid, seq))
    }

    /// When the oldest copy is due.
    fn next_due(&self) -> Option<Instant> {
        self.queue.front().map(|(due, _, _)| *due)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.queue.len()
    }
}

/// The playout delay of one publisher's video toward one subscriber, and
/// what is being stamped right now.
#[derive(Debug, Default)]
struct PairDelay {
    policy: PlayoutDelay,
    /// The minimum playout delay stamped on the pair's video packets, in
    /// milliseconds; refreshed every pass ([`drive_playout_delay`]).
    stamped_ms: u16,
}

/// Whether the playout-delay header extension was negotiated on the media
/// line `local_mid` of this peer. The session map says what the SFU
/// offers; a peer that left the extension out of its answer for this line
/// did not negotiate it, and str0m then writes none.
fn playout_delay_negotiated(rtc: &Rtc, local_mid: Mid) -> bool {
    rtc.media(local_mid).is_some_and(|media| {
        media
            .remote_extmap()
            .id_of(Extension::PlayoutDelay)
            .is_some()
    })
}

/// Write the playout-delay header extension values onto a packet's
/// extensions: the given minimum and the fixed maximum
/// ([`playout_delay::MAX_PLAYOUT_DELAY_MS`]). str0m writes the extension
/// only when both are set.
fn stamp_playout_delay(ext_vals: &mut ExtensionValues, min_ms: u16) {
    ext_vals.play_delay_min = Some(MediaTime::from_millis(u64::from(min_ms)));
    ext_vals.play_delay_max = Some(MediaTime::from_millis(u64::from(
        playout_delay::MAX_PLAYOUT_DELAY_MS,
    )));
}

/// The ceiling of a subscriber's downlink estimate, and the most the SFU
/// counts on wanting to send one subscriber, bits per second
/// ([ADR-0070](../../../docs/adr/0070-call-media-through-a-loss-burst.md)
/// step 6). The estimate starts here: nothing is known of the path when a
/// subscriber joins, and an estimate that starts high is brought down by
/// the first sign of a queue. In use the ceiling follows what the SFU
/// would like to send ([`DOWNLINK_CEILING_OVER_DEMAND_NUM`]). Chosen.
pub const DOWNLINK_CEILING_BPS: u64 = 20_000_000;

/// The estimate's ceiling is this multiple of what the SFU would like to
/// send the subscriber (numerator; one and a half). An estimate far
/// above the demand tells the allocation nothing more, and the
/// estimator's blackout rule measures what arrives against the estimate.
const DOWNLINK_CEILING_OVER_DEMAND_NUM: u64 = 3;
/// See [`DOWNLINK_CEILING_OVER_DEMAND_NUM`].
const DOWNLINK_CEILING_OVER_DEMAND_DEN: u64 = 2;

/// How often the loss a subscriber's feedback reported is handed to its
/// estimator: the interval its loss rule is written against.
const DOWNLINK_LOSS_INTERVAL: Duration = Duration::from_secs(1);

/// For how long a probe that was carried vouches for its rate
/// ([`crate::downlink_probe`]): long enough for the allocation's next run
/// to give the set, short enough that it is not taken for the path as it
/// is some seconds later. Chosen.
const DOWNLINK_PROVEN_FOR: Duration = Duration::from_secs(2);

/// What is arriving at a subscriber vouches for its own rate only on a
/// quiet path: not while the estimator sees delay growing, and not within
/// this long of the estimate having been lowered. Chosen: the
/// estimator's own episode length.
const DOWNLINK_QUIET_AFTER_TROUBLE: Duration = Duration::from_secs(3);

/// A probe fills the path's queue on purpose, and the estimator reads
/// that as over-use. While a probe runs and for this long after it, a cut
/// of the estimate is taken back: a probe that fails changes when the
/// next one may start and nothing else. Chosen: longer than the 150 ms to
/// 500 ms a queue the probe filled takes to drain.
const DOWNLINK_PROBE_SHADOW: Duration = Duration::from_secs(1);

/// Feedback reports whose lag is remembered for the next probe
/// ([`crate::downlink_probe::PROBE_QUEUE_ALLOWANCE`]).
const DOWNLINK_FEEDBACK_LAGS_KEPT: usize = 16;

/// A cut always lands below what the set being sent needs, so on the
/// downlink a false alarm costs a picture. A cut is kept only when a
/// queue stands on the path ([`crate::uplink_estimate::QueueWatch`],
/// shared with a publisher's uplink): this deep ...
const DOWNLINK_STANDING_QUEUE: Duration = crate::uplink_estimate::STANDING_QUEUE;
/// ... for this long.
const DOWNLINK_QUEUE_STANDS_FOR: Duration = crate::uplink_estimate::QUEUE_STANDS_FOR;
/// A queue this deep beyond what the SFU's own sending explains, standing
/// for [`DOWNLINK_QUEUE_STANDS_FOR`], is cut on without the delay
/// detector having spoken: a path that carries a fraction of what is
/// sent fills its queue within the detector's first window and shows no
/// growth after (seen: a 2.35 Mbit/s link under 12 Mbit/s of demand, the
/// queue standing at 108 ms for 28 s and not one cut).
const DOWNLINK_DEEP_QUEUE: Duration = crate::uplink_estimate::DEEP_QUEUE;
/// The most the model of the SFU's own sending may explain of a queue
/// ([`Downlink::virtual_bits`]), as time at the estimate.
const DOWNLINK_VIRTUAL_QUEUE_MAX: Duration = Duration::from_secs(1);
/// What the path was carrying steadily just before a cut: the quiet
/// samples between this long ago ...
const DOWNLINK_STEADY_FROM: Duration = Duration::from_secs(4);
/// ... and this long ago (the last half second is the trouble itself),
const DOWNLINK_STEADY_UNTIL: Duration = Duration::from_millis(600);
/// with at least this many samples (one per allocation run).
const DOWNLINK_STEADY_MIN_SAMPLES: usize = 10;
/// A second cut within this long of one that was held is not held: the
/// path did not carry it after all.
const DOWNLINK_STEADY_ONCE_PER: Duration = Duration::from_secs(5);
/// A packet that the model of the SFU's own sending has waiting this
/// long was sent in a clump (a publisher whose frames arrived together):
/// twice [`DOWNLINK_STANDING_QUEUE`], and above what the frames of three
/// publishers written in the same instant come to on the 7.5 Mbit/s
/// link of the e2e test (28 ms).
const DOWNLINK_CLUMP: Duration = Duration::from_millis(50);
/// For this long after a clump was sent no queue is evidence against
/// the path: the model says how deep the clump's queue should be, but
/// not to the millisecond (in the e2e test, debug build, 36 ms of an
/// 84 ms queue stayed unexplained while the SFU was still writing the
/// clump out to four peers). A path that did get narrower in that second
/// is noticed when it is over.
const DOWNLINK_CLUMP_SHADOW: Duration = Duration::from_secs(1);
/// One clump's shadow per this long: a path on which clumps never stop
/// is still judged most of the time.
const DOWNLINK_CLUMP_ONCE_PER: Duration = Duration::from_secs(3);
/// The estimate's ceiling follows the largest demand of this long: a
/// moment's stall of the publishers must not lower it.
const DOWNLINK_DEMAND_PEAK_FOR: Duration = Duration::from_secs(10);

/// Most recently sent video packets remembered per subscriber, to copy as
/// padding. Chosen: more than a second of a main track.
const DOWNLINK_RECENT_PACKETS: usize = 512;

/// Padding is not sent for less than this many bytes owed, and at most
/// [`DOWNLINK_PAD_PACKETS_PER_PASS`] copies are queued in one pass of the
/// media loop.
const DOWNLINK_PAD_MIN_BYTES: usize = 200;
/// See [`DOWNLINK_PAD_MIN_BYTES`].
const DOWNLINK_PAD_PACKETS_PER_PASS: usize = 64;

/// A set that the path took away soon after it was given proved too
/// large only when the path still carries a good part of it: the cut
/// left the estimate at no less than one part in this many of what the
/// set put on the wire. Below that the path has collapsed under whatever
/// was being sent (a stall of a radio link: seen at 130 to 715 kbit/s
/// under sets of 1.5 to 2 Mbit/s on the live call of 2026-10-09), which
/// says nothing of the set, and the set is tried again as soon as the
/// path is quiet. Chosen: a set one step too large overshoots a path
/// that carried the set before it by less than twice.
const DOWNLINK_COLLAPSE_DEN: u64 = 2;

/// How often a subscriber's allocation is run. A downgrade therefore
/// follows a fallen estimate within this. Chosen.
const DOWNLINK_ALLOCATION_INTERVAL: Duration = Duration::from_millis(100);

/// The retransmission cache a paused track's send stream is given in
/// place of the one it had (which empties it): str0m's own defaults for a
/// video stream, 1000 packets and three seconds, with no cap on the share
/// of retransmissions (the vendored fork's eighth change).
const PAUSED_RTX_CACHE_PACKETS: usize = 1000;
/// See [`PAUSED_RTX_CACHE_PACKETS`].
const PAUSED_RTX_CACHE_AGE: Duration = Duration::from_secs(3);

/// Most tracks of one publisher the SFU keeps an arrival-rate meter for.
/// A track beyond that starts the table over. Chosen: audio, three camera
/// tracks and a screen share with room to spare.
const MAX_TRACK_METERS: usize = 16;

/// What the SFU keeps per subscriber to decide what it is sent of each
/// publisher's video (ADR-0070 step 6). Present only under downlink
/// control.
#[derive(Debug)]
struct Downlink {
    /// The rule ([`crate::downlink_allocation`]).
    allocation: DownlinkAllocation<MediaPeerId>,
    /// The estimate of what the path to the subscriber carries, on the
    /// rules of a publisher's uplink ([`crate::uplink_estimate`]), fed
    /// from the subscriber's transport-wide feedback.
    estimator: PathEstimator,
    /// Trying the next larger set on the path ([`crate::downlink_probe`]).
    probe: DownlinkProbe,
    /// The publishers the subscriber named in its last `video-wants`, in
    /// the order it named them: its order of importance. Publishers it
    /// did not name come after these, in the order they joined.
    importance: Vec<MediaPeerId>,
    /// What each publisher is given right now. A publisher without an
    /// entry is given what the subscriber asked for.
    given: HashMap<MediaPeerId, VideoQuality>,
    /// The `video-given` set the subscriber was last sent.
    told: Option<Vec<VideoGiven>>,
    /// The subscriber sent a `video-wants` since it was last told: it is
    /// told again, changed or not.
    tell: bool,
    /// When the allocation last ran.
    last_run: Option<Instant>,
    /// The subscriber's arrival clock against the SFU's: the SFU's time
    /// and the subscriber's reported arrival time of the first packet its
    /// feedback acknowledged. Every later arrival time is placed by its
    /// distance from that one.
    clock: Option<(Instant, Instant)>,
    /// Packets the feedback reported on, and reported missing, since the
    /// estimator was last told; and when it was.
    loss_expected: u64,
    loss_lost: u64,
    last_loss_feed: Instant,
    /// The estimator's count of cuts when it was last looked at, and when
    /// it last moved.
    seen_cuts: u64,
    last_trouble: Option<Instant>,
    /// The estimate when the last probe started, and until when a cut is
    /// the probe's doing ([`DOWNLINK_PROBE_SHADOW`]).
    probe_floor: Option<(u64, Instant)>,
    /// Cuts of the estimate since the media loop last collected them.
    cut_notes: Vec<EstimateCut>,
    /// The delay (arrival on the SFU's clock less send time, microseconds;
    /// its zero is arbitrary) of the packets acknowledged in the last
    /// [`crate::uplink_estimate::QUEUE_RECENT`], with their arrival times.
    queue: crate::uplink_estimate::QueueWatch,
    /// The queue the SFU's own sending would stand in on a path that
    /// carries exactly the estimate, bits: every packet the feedback
    /// speaks of adds its size, and the estimate drains it between
    /// their send times. A clump of packets (a publisher whose frames
    /// arrived together and were forwarded together) stands in the
    /// path's queue for a while on any path; that much of a queue says
    /// nothing against the estimate. And the send time of the last
    /// packet taken.
    virtual_bits: u64,
    virtual_last_sent: Option<Instant>,
    /// Since when a queue of [`DOWNLINK_STANDING_QUEUE`] has stood in the
    /// raw delay, whatever explains it: no probe starts into one.
    raw_queue_since: Option<Instant>,
    /// The rate acknowledged as arriving at each allocation run on a
    /// quiet path, newest last.
    steady: std::collections::VecDeque<(Instant, u64)>,
    /// When a cut was last held at the steady rate.
    held_at: Option<Instant>,
    /// The same model kept as the packets are written (not as the
    /// feedback speaks of them): media bits a path that carries the
    /// estimate would still be holding, and when the last was written.
    /// A probe adds no padding on top of a clump.
    written_bits: u64,
    written_last: Option<Instant>,
    /// When the last clump's shadow began ([`DOWNLINK_CLUMP_SHADOW`]),
    /// and when a cut was last kept.
    clump_at: Option<Instant>,
    kept_cut_at: Option<Instant>,
    /// The smallest delay in the current and in the previous
    /// [`crate::uplink_estimate::QUEUE_BASELINE`], and when the current one began.
    /// Since when a queue of [`DOWNLINK_STANDING_QUEUE`] has stood beyond
    /// what the SFU's own sending explains.
    queue_since: Option<Instant>,
    /// Where the queue stood when the running probe started, and in how
    /// many feedback reports running it has since stood deeper.
    probe_queue_base: Duration,
    probe_queue_hits: u8,
    /// What the path has said of itself: the estimate never stands
    /// above this. Set when the path cuts the estimate (to what was
    /// arriving then) and when a probe fails (to just under its rate);
    /// raised only by a probe that is carried. `None` until the path has
    /// spoken: then nothing is known and the estimate is its start
    /// value.
    limit_bps: Option<u64>,
    /// The ceiling the demand alone would set.
    demand_ceiling_bps: u64,
    /// How long the subscriber's feedback took to report on the oldest
    /// packet of each of its last reports, on a quiet path.
    feedback_lags: std::collections::VecDeque<Duration>,
    /// And to report on the newest packet of each.
    feedback_floors: std::collections::VecDeque<Duration>,
    /// A cut was taken back because the queue had not stood long enough
    /// yet. If it goes on standing, the cut is made after all.
    cut_pending: bool,
    /// The largest demand of the last [`DOWNLINK_DEMAND_PEAK_FOR`] and
    /// when it was seen.
    demand_peak: Option<(u64, Instant)>,
    /// The rate of the last probe that was carried, and when it ended.
    proven: Option<(u64, Instant)>,
    /// Probes that ended since the media loop last collected them.
    outcomes: Vec<ProbeOutcome>,
    /// Counts for the log line: probes started, carried, and bytes of
    /// padding queued.
    probes_started: u64,
    probes_carried: u64,
    padding_bytes: u64,
    /// The video packets most recently written to the subscriber, to copy
    /// as padding: its own mid of the track, the downstream sequence
    /// number, the payload length. Newest last.
    recent: std::collections::VecDeque<(Mid, u64, usize)>,
    /// What the SFU would like to send, headroom included, as of the
    /// last run.
    demand_bps: Option<u64>,
    /// The budget and the carried rate the allocation last ran with, for
    /// the log.
    budget_bps: Option<u64>,
    carried_bps: Option<u64>,
    /// Media bytes written to this subscriber (first transmissions), for
    /// the log: what is being sent.
    sent: LayerBitrateMeter,
    /// The path itself lowered the estimate since the allocation last
    /// ran ([`Self::cut_kept`]): what is being given may then stand above
    /// the estimate, and that is the path's word, not a cost that grew.
    lowered_since_run: bool,
    /// When something was last given (a publisher moved up), how large
    /// the set then was ([`downlink_allocation::set_level`]) and what it
    /// put on the wire: taken away again by the path within
    /// [`TOO_LARGE_WITHIN`], that set proved too large.
    last_up: Option<(Instant, u64, u64)>,
    /// Times the limit was raised to what the set being given had come
    /// to cost on a quiet path ([`Self::follow_what_is_carried`]), for
    /// the log.
    followed: u64,
}

impl Downlink {
    fn new(now: Instant) -> Self {
        Self {
            allocation: DownlinkAllocation::new(),
            estimator: PathEstimator::new(
                EstimatorConfig {
                    start_bps: DOWNLINK_CEILING_BPS,
                    max_bps: DOWNLINK_CEILING_BPS,
                },
                now,
            ),
            probe: DownlinkProbe::new(),
            importance: Vec::new(),
            given: HashMap::new(),
            // A subscriber with no video track of anyone is not told an
            // empty set it never asked about.
            told: Some(Vec::new()),
            tell: false,
            last_run: None,
            clock: None,
            loss_expected: 0,
            loss_lost: 0,
            last_loss_feed: now,
            seen_cuts: 0,
            last_trouble: None,
            probe_floor: None,
            cut_notes: Vec::new(),
            queue: crate::uplink_estimate::QueueWatch::new(now),
            queue_since: None,
            virtual_bits: 0,
            virtual_last_sent: None,
            raw_queue_since: None,
            steady: std::collections::VecDeque::new(),
            held_at: None,
            written_bits: 0,
            written_last: None,
            clump_at: None,
            kept_cut_at: None,
            probe_queue_base: Duration::ZERO,
            probe_queue_hits: 0,
            limit_bps: None,
            demand_ceiling_bps: DOWNLINK_CEILING_BPS,
            feedback_lags: std::collections::VecDeque::new(),
            feedback_floors: std::collections::VecDeque::new(),
            cut_pending: false,
            demand_peak: None,
            proven: None,
            outcomes: Vec::new(),
            probes_started: 0,
            probes_carried: 0,
            padding_bytes: 0,
            recent: std::collections::VecDeque::new(),
            demand_bps: None,
            budget_bps: None,
            carried_bps: None,
            sent: LayerBitrateMeter::new(now),
            lowered_since_run: false,
            last_up: None,
            followed: 0,
        }
    }

    /// A reported arrival time, on the subscriber's clock, as a time on
    /// the SFU's: the first one is `now`, every other one as far from it
    /// as the subscriber says.
    fn local_arrival(&mut self, remote: Instant, now: Instant) -> Instant {
        let (local0, remote0) = *self.clock.get_or_insert((now, remote));
        if remote >= remote0 {
            local0 + remote.duration_since(remote0)
        } else {
            local0
                .checked_sub(remote0.duration_since(remote))
                .unwrap_or(local0)
        }
    }

    /// Note what the SFU would like to send (`demand`, headroom
    /// included) and let the estimate's ceiling follow the largest
    /// demand of the last [`DOWNLINK_DEMAND_PEAK_FOR`].
    fn follow_demand(&mut self, demand: u64, now: Instant) {
        let demand = demand.min(DOWNLINK_CEILING_BPS);
        self.demand_bps = Some(demand);
        let peak = match self.demand_peak {
            Some((peak, at))
                if peak > demand
                    && now.saturating_duration_since(at) < DOWNLINK_DEMAND_PEAK_FOR =>
            {
                peak
            },
            _ => {
                self.demand_peak = Some((demand, now));
                demand
            },
        };
        if peak > 0 {
            self.demand_ceiling_bps = (peak / DOWNLINK_CEILING_OVER_DEMAND_DEN)
                .saturating_mul(DOWNLINK_CEILING_OVER_DEMAND_NUM)
                .min(DOWNLINK_CEILING_BPS);
            self.apply_ceiling();
        }
    }

    /// The estimate's ceiling: what the demand allows, and never above
    /// what the path has said of itself ([`Self::limit_bps`]).
    fn apply_ceiling(&mut self) {
        let ceiling = self.limit_bps.map_or(self.demand_ceiling_bps, |limit| {
            limit.min(self.demand_ceiling_bps)
        });
        self.estimator.set_ceiling(ceiling);
        // Until the path has spoken nothing is known of it, and the
        // estimate is its ceiling: a cut that was taken back must not
        // leave it at the ceiling of the moment, which in a call's first
        // second is the demand of the tracks measured so far.
        if self.limit_bps.is_none() {
            self.estimator.note_carried(ceiling);
        }
    }

    /// The feedback's usual lag: the longest of the last reports' on a
    /// quiet path; `None` before the first.
    fn feedback_lag(&self) -> Option<Duration> {
        self.feedback_lags.iter().copied().max()
    }

    /// The least a report takes to speak of a packet: the smallest lag
    /// of the newest packet of the last reports on a quiet path.
    fn feedback_floor(&self) -> Option<Duration> {
        self.feedback_floors.iter().copied().min()
    }

    /// Someone is given less than asked for: try the next larger set on
    /// the path, if a probe may start and there is a packet to copy. The
    /// most important publisher's next track first; when the path is
    /// known not to carry what that needs (a probe toward it failed and
    /// its interval is running), the first move that needs less. Never
    /// into a queue that is already standing or growing. Returns the
    /// rate of the probe started.
    fn start_probe(
        &mut self,
        publishers: &[downlink_allocation::Publisher<MediaPeerId>],
        given: &[VideoQuality],
        now: Instant,
    ) -> Option<u64> {
        // Never into a queue that is growing: not while the detector says
        // over-use, not while a doubted cut waits to be borne out, and
        // not into a queue that has stood for less than three seconds
        // (one that has stood longer without a cut is the path's own
        // delay having moved, which a carried probe then takes as the
        // baseline).
        let queue_is_young = self.raw_queue_since.is_some_and(|since| {
            now.saturating_duration_since(since) < DOWNLINK_QUIET_AFTER_TROUBLE
        });
        let calm = self.estimator.state() != DetectorState::Overuse
            && !self.cut_pending
            && !queue_is_young;
        if !calm || self.recent.is_empty() || self.probe.is_running() {
            return None;
        }
        // Nor into a clump of media that is going out this moment (a key
        // frame, a publisher's frames arriving together): the probe would
        // be given up at once and the next would wait
        // [`crate::downlink_probe::PROBE_AFTER_TROUBLE`] for nothing. The
        // next run of the allocation is a tenth of a second away. (Seen
        // on a live call, 2026-10-09: two probes of four after a dip
        // ended `disturbed` within 3 and 8 ms of starting.)
        if self.clump_being_written(now) {
            return None;
        }
        // The most important publisher's next track first. Once a probe
        // has failed the path is known to be short somewhere, and the
        // cheapest move that may be tried now goes first: otherwise a
        // move the path cannot carry would be retried for ever in front
        // of a cheaper one that failed once.
        // A set that proved too large a moment ago, and every larger one,
        // is not tried while its wait runs.
        let held = self.probe.held_from(now);
        let mut steps = downlink_allocation::next_steps(publishers, given)
            .into_iter()
            .filter(|step| held.map_or(true, |held| step.level < held))
            .filter(|step| self.probe.can_start_toward(step.need_bps, now));
        let step = if self.probe.failed_bps().is_some() {
            steps.min_by_key(|step| step.need_bps)?
        } else {
            steps.next()?
        };
        // The probe climbs from what the set being given puts on the
        // wire, and is held to the queue it may build by the feedback's
        // usual lag.
        let base = downlink_allocation::wire_bps(publishers, given);
        let lag = self
            .feedback_lag()
            .unwrap_or(crate::downlink_probe::PROBE_DEFAULT_FEEDBACK_LAG);
        // (The second copies of light packets are in both figures and
        // are reported to the probe as sent, like every media packet.
        // An earlier build took them out of the schedule as well, so
        // every probe ran short of its target by their rate: nothing at
        // 9 Mbit/s, and at 1.5 Mbit/s a third of the target, which no
        // probe from a small set could pass.)
        let floor = self.feedback_floor().unwrap_or(Duration::ZERO);
        if !self
            .probe
            .start_from(step.need_bps, base, (floor, lag), now)
        {
            return None;
        }
        self.probe_floor = Some((
            self.estimator.estimate_bps(),
            now + PROBE_DURATION + DOWNLINK_PROBE_SHADOW,
        ));
        // What the probe adds to the queue is measured from where the
        // queue stands now.
        self.probe_queue_base = self.standing_queue().unwrap_or(Duration::ZERO);
        self.probe_queue_hits = 0;
        self.probes_started = self.probes_started.saturating_add(1);
        self.probe.target_bps()
    }

    /// One acknowledged packet's delay into the standing-queue watch.
    fn note_delay(&mut self, sent: Instant, arrival: Instant, explained: Duration) {
        let micros = |d: Duration| i64::try_from(d.as_micros()).unwrap_or(i64::MAX);
        let delay = if arrival >= sent {
            micros(arrival.duration_since(sent))
        } else {
            -micros(sent.duration_since(arrival))
        };
        self.queue.note(arrival, delay, explained);
    }

    /// Take the delay of the packets acknowledged lately as the delay of
    /// the empty path: a probe has just shown that no queue stands.
    fn rebase_queue(&mut self) {
        if self.queue.rebase() {
            self.queue_since = None;
            self.raw_queue_since = None;
            self.cut_pending = false;
        }
    }

    /// The queue standing on the path ([`DOWNLINK_STANDING_QUEUE`]);
    /// `None` when too few packets were acknowledged lately to tell.
    fn standing_queue(&self) -> Option<Duration> {
        self.queue.standing()
    }

    /// The queue standing on the path beyond what the SFU's own sending
    /// explains on a path that carries the estimate
    /// ([`Self::virtual_bits`]): the evidence a cut needs.
    fn excess_queue(&self) -> Option<Duration> {
        self.queue.excess()
    }

    /// One packet the SFU sent at `sent` into the model of its own
    /// sending; returns how long it would wait on a path that carries
    /// `rate_bps`.
    fn explained_delay(&mut self, sent: Instant, size: usize, rate_bps: u64) -> Duration {
        let rate = rate_bps.max(1);
        let since = self
            .virtual_last_sent
            .map_or(Duration::ZERO, |last| sent.saturating_duration_since(last));
        if self.virtual_last_sent.map_or(true, |last| sent > last) {
            self.virtual_last_sent = Some(sent);
        }
        let micros = u128::from(u64::try_from(since.as_micros()).unwrap_or(u64::MAX));
        let drained = u64::try_from(u128::from(rate) * micros / 1_000_000).unwrap_or(u64::MAX);
        let most =
            u64::try_from(u128::from(rate) * DOWNLINK_VIRTUAL_QUEUE_MAX.as_micros() / 1_000_000)
                .unwrap_or(u64::MAX);
        // The packet waits behind what stood there when it was sent.
        let ahead = self.virtual_bits.saturating_sub(drained);
        self.virtual_bits = ahead
            .saturating_add(u64::try_from(size).unwrap_or(u64::MAX).saturating_mul(8))
            .min(most);
        Duration::from_micros(
            u64::try_from(u128::from(ahead) * 1_000_000 / u128::from(rate)).unwrap_or(u64::MAX),
        )
    }

    /// A packet the model has waiting [`DOWNLINK_CLUMP`] was sent in a
    /// clump: its shadow begins, unless a probe is the reason (a probe
    /// sends above the estimate on purpose and has its own shadow), a
    /// cut was just kept (then the estimate fell under what is still
    /// being sent, which is no clump), or the last shadow is too recent.
    fn note_clump(&mut self, explained: Duration, now: Instant) {
        if explained < DOWNLINK_CLUMP || self.probe.is_running() {
            return;
        }
        let within = |at: Option<Instant>, d: Duration| {
            at.is_some_and(|at| now.saturating_duration_since(at) < d)
        };
        if within(self.kept_cut_at, DOWNLINK_CLUMP_SHADOW)
            || within(self.clump_at, DOWNLINK_CLUMP_ONCE_PER)
            || self.probe_floor.is_some_and(|(_, until)| now <= until)
        {
            return;
        }
        // Only a path that has carried what is being sent steadily for
        // seconds gets the benefit of the doubt. The key frames a call
        // starts with are a clump too, and they go out before anything
        // is known of the path (seen: the first cut on the 7.5 Mbit/s
        // link a second late for it).
        if self.steady_bps(now).is_none() {
            return;
        }
        self.clump_at = Some(now);
    }

    /// The rate the model of the media written drains at: the estimate,
    /// and never less than what is steadily being written with a
    /// quarter to spare (an estimate below what cannot be given up, the
    /// light tracks, would make all of it a clump).
    ///
    /// While a probe runs it is the rate being tried, when that is more:
    /// the probe asks whether the path carries that rate, and a clump is
    /// what would stand in the queue of a path that does. (Seen on a
    /// live call, 2026-10-09: after a dip had put the estimate at
    /// 130 kbit/s with 0.5 Mbit/s of light pictures being written, the
    /// model drained at 0.6 Mbit/s and every key frame of a light
    /// picture was a clump: of four probes toward 1.4 to 1.7 Mbit/s
    /// three were given up as `disturbed`, one of them after its path
    /// had carried 1.46 of 1.74.)
    fn written_model_bps(&self, now: Instant) -> u64 {
        let written = self.sent.bitrate_bps_at(now);
        self.estimator
            .estimate_bps()
            .max(written.saturating_add(written / 4))
            .max(self.probe.target_bps().unwrap_or(0))
            .max(1)
    }

    /// One media packet written to the subscriber at `at` (first
    /// transmissions; a light packet's second copy is not counted, as it
    /// is not in [`Self::sent`]).
    fn note_written(&mut self, len: usize, at: Instant) {
        self.written_bits = self
            .written_backlog_bits(at)
            .saturating_add(u64::try_from(len).unwrap_or(u64::MAX).saturating_mul(8));
        if self.written_last.map_or(true, |last| at > last) {
            self.written_last = Some(at);
        }
    }

    /// What of the media written a path that carries the estimate would
    /// still be holding at `now`, bits.
    fn written_backlog_bits(&self, now: Instant) -> u64 {
        let since = self
            .written_last
            .map_or(Duration::ZERO, |last| now.saturating_duration_since(last));
        let micros = u128::from(u64::try_from(since.as_micros()).unwrap_or(u64::MAX));
        let drained = u64::try_from(u128::from(self.written_model_bps(now)) * micros / 1_000_000)
            .unwrap_or(u64::MAX);
        self.written_bits.saturating_sub(drained)
    }

    /// How long the media just written would wait on a path that carries
    /// [`Self::written_model_bps`].
    fn written_wait(&self, now: Instant) -> Duration {
        let rate = u128::from(self.written_model_bps(now));
        let micros = u128::from(self.written_backlog_bits(now)) * 1_000_000 / rate;
        Duration::from_micros(u64::try_from(micros).unwrap_or(u64::MAX))
    }

    /// Whether the media just written is a clump: more than
    /// [`DOWNLINK_STANDING_QUEUE`] of it would be waiting on a path that
    /// carries the estimate.
    fn clump_being_written(&self, now: Instant) -> bool {
        self.written_wait(now) >= DOWNLINK_STANDING_QUEUE
    }

    /// A clump of media ([`DOWNLINK_CLUMP`]) is going out while a probe
    /// runs: the probe is given up. The queue the two build together is
    /// not the path's answer to the probe's rate, and a probe judged by
    /// it would hold that rate back for the whole wait a failure costs
    /// (seen: a publisher's frames 126 ms late during the probe that
    /// would have settled the set, and the set settled 8 s later).
    fn give_probe_up_on_a_clump(&mut self, now: Instant) {
        if self.probe.is_running() && self.written_wait(now) >= DOWNLINK_CLUMP {
            if let Some(outcome) = self.probe.abandon(now) {
                self.outcomes.push(outcome);
            }
        }
    }

    /// Whether a clump's shadow lies on `now`.
    fn in_clump_shadow(&self, now: Instant) -> bool {
        self.clump_at
            .is_some_and(|at| now.saturating_duration_since(at) < DOWNLINK_CLUMP_SHADOW)
    }

    /// Note what is arriving on a quiet path, once per allocation run:
    /// what the path carries steadily.
    fn note_steady(&mut self, now: Instant) {
        while self
            .steady
            .front()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) > DOWNLINK_STEADY_FROM)
        {
            self.steady.pop_front();
        }
        let quiet = !self.probe.is_running()
            && self.queue_since.is_none()
            && self.probe_floor.map_or(true, |(_, until)| now > until);
        if !quiet {
            return;
        }
        if let Some(arriving) = self.estimator.arriving_latest_bps() {
            self.steady.push_back((now, arriving));
        }
    }

    /// The lowest rate the path was carrying steadily just before `now`;
    /// `None` without enough of a quiet stretch to tell.
    fn steady_bps(&self, now: Instant) -> Option<u64> {
        let samples = self.steady.iter().filter(|(at, _)| {
            let ago = now.saturating_duration_since(*at);
            ago >= DOWNLINK_STEADY_UNTIL && ago <= DOWNLINK_STEADY_FROM
        });
        if samples.clone().count() < DOWNLINK_STEADY_MIN_SAMPLES {
            return None;
        }
        samples.map(|(_, bps)| *bps).min()
    }

    /// The records themselves into the estimator, the loss count, the
    /// standing-queue watch and a running probe.
    fn feed_records(
        &mut self,
        records: &[(Instant, usize, Option<Instant>)],
        rate_bps: u64,
        now: Instant,
    ) {
        // How long this report took to speak of its oldest packet, kept
        // only from a quiet path: a queue (a probe's, or its aftermath)
        // is not the feedback's lag.
        let quiet = !self.probe.is_running()
            && self.raw_queue_since.is_none()
            && self.probe_floor.map_or(true, |(_, until)| now > until);
        if let (true, Some(oldest)) = (quiet, records.iter().map(|(sent, _, _)| *sent).min()) {
            if self.feedback_lags.len() >= DOWNLINK_FEEDBACK_LAGS_KEPT {
                self.feedback_lags.pop_front();
            }
            self.feedback_lags
                .push_back(now.saturating_duration_since(oldest));
            if let Some(newest) = records.iter().map(|(sent, _, _)| *sent).max() {
                if self.feedback_floors.len() >= DOWNLINK_FEEDBACK_LAGS_KEPT {
                    self.feedback_floors.pop_front();
                }
                self.feedback_floors
                    .push_back(now.saturating_duration_since(newest));
            }
        }
        for (sent, size, received) in records {
            self.loss_expected = self.loss_expected.saturating_add(1);
            self.probe.on_feedback(*sent, *size, received.is_some());
            let explained = self.explained_delay(*sent, *size, rate_bps);
            self.note_clump(explained, now);
            match received {
                Some(remote) => {
                    let arrival = self.local_arrival(*remote, now);
                    self.note_delay(*sent, arrival, explained);
                    self.estimator
                        .on_packet_sent_at(*sent, arrival, *size, false);
                },
                None => self.loss_lost = self.loss_lost.saturating_add(1),
            }
        }
        if !records.is_empty() {
            self.probe.on_report(now);
        }
        if now.saturating_duration_since(self.last_loss_feed) >= DOWNLINK_LOSS_INTERVAL {
            self.last_loss_feed = now;
            let (expected, lost) = (self.loss_expected, self.loss_lost);
            self.loss_expected = 0;
            self.loss_lost = 0;
            self.estimator.on_loss(now, expected, lost);
        }
    }

    /// The queue as the delay shows it (a probe is judged by that, and
    /// none starts into one), and the queue beyond what the SFU's own
    /// sending explains on a path that carries the estimate (a cut is
    /// judged by that: a clump the SFU forwarded stands in the queue of
    /// any path; in a clump's shadow no queue counts at all).
    fn watch_queues(&mut self, now: Instant) -> (Option<Duration>, Option<Duration>) {
        let raw_queue = self.standing_queue();
        match raw_queue {
            Some(queue) if queue >= DOWNLINK_STANDING_QUEUE => {
                self.raw_queue_since.get_or_insert(now);
            },
            Some(_) => self.raw_queue_since = None,
            None => {},
        }
        self.take_a_settled_delay_as_the_paths_own(raw_queue, now);
        let raw_queue = self.standing_queue();
        let queue = if self.in_clump_shadow(now) {
            self.excess_queue().map(|_| Duration::ZERO)
        } else {
            self.excess_queue()
        };
        (raw_queue, queue)
    }

    /// A delay that has stood above the path's smallest for
    /// [`DOWNLINK_QUIET_AFTER_TROUBLE`], not deep, with no cut kept in
    /// that time, no cut pending, no probe and no over-use, is not a
    /// queue that is filling: it is where the path's delay now is, and
    /// it is taken as the baseline.
    ///
    /// Seen in every run of the dip test: after a second of loss the
    /// delay computed from the subscriber's feedback stands 12 to 34 ms
    /// higher for good, on a loopback link with no queue ("Seen, not
    /// explained"). Left as a standing queue, it confirmed the next
    /// blip of the detector as congestion: a subscriber on an unlimited
    /// link lost a picture eight seconds after the dip, when one of the
    /// test's publishers wrote a frame late. The baseline would have
    /// caught up by itself after 15 to 30 s.
    fn take_a_settled_delay_as_the_paths_own(&mut self, raw: Option<Duration>, now: Instant) {
        let settled = self.raw_queue_since.is_some_and(|since| {
            now.saturating_duration_since(since) >= DOWNLINK_QUIET_AFTER_TROUBLE
        });
        let quiet = !self.cut_pending
            && !self.probe.is_running()
            && self.estimator.state() != DetectorState::Overuse
            && self.kept_cut_at.map_or(true, |at| {
                now.saturating_duration_since(at) >= DOWNLINK_QUIET_AFTER_TROUBLE
            });
        if settled && quiet && raw.is_some_and(|queue| queue < DOWNLINK_DEEP_QUEUE) {
            self.rebase_queue();
        }
    }

    /// A path that has just carried what is being sent for seconds on
    /// end does not lose a picture to the first cut: a cut is to a
    /// fraction of what arrived in the last half second, and after a gap
    /// in what was sent that is little. The estimate stays as it was
    /// (`before`); if the queue goes on standing for as long again, the
    /// path does not carry it any more, and that cut is kept.
    fn hold_first_cut(&mut self, note: &mut EstimateCut, before: u64, now: Instant) {
        let held_lately = self
            .held_at
            .is_some_and(|at| now.saturating_duration_since(at) < DOWNLINK_STEADY_ONCE_PER);
        if !held_lately && self.steady_bps(now).is_some() {
            self.estimator.note_carried(before);
            note.to_bps = self.estimator.estimate_bps();
            note.taken_back = true;
            note.held_at_steady = true;
            self.held_at = Some(now);
            self.cut_pending = true;
            self.queue_since = Some(now);
        }
        self.steady.clear();
    }

    /// A cut was kept: the path has spoken. What was arriving when it
    /// did is what it carries, and the estimate does not climb past that
    /// by rule; only a probe that is carried raises it. The model of the
    /// SFU's own sending starts again under the new estimate.
    fn cut_kept(&mut self, now: Instant) {
        let arriving = self.estimator.arriving_latest_bps().unwrap_or(0);
        self.limit_bps = Some(self.estimator.estimate_bps().max(arriving));
        self.apply_ceiling();
        self.kept_cut_at = Some(now);
        self.virtual_bits = 0;
        self.lowered_since_run = true;
    }

    /// Whether the path is quiet at `now`: the feedback of the last
    /// moments shows no queue standing, by the delay as it is and beyond
    /// what the SFU's own sending explains; no cut waits to be borne
    /// out; and the path has lowered nothing for
    /// [`DOWNLINK_QUIET_AFTER_TROUBLE`]. A path whose feedback says too
    /// little to tell is not quiet.
    fn path_is_quiet(&self, now: Instant) -> bool {
        !self.cut_pending
            && self.queue_since.is_none()
            && self.raw_queue_since.is_none()
            && self
                .standing_queue()
                .is_some_and(|queue| queue < DOWNLINK_STANDING_QUEUE)
            && self.last_trouble.map_or(true, |at| {
                now.saturating_duration_since(at) >= DOWNLINK_QUIET_AFTER_TROUBLE
            })
    }

    /// The set being given puts `wire_bps` on the wire, and that has
    /// come to be more than the estimate with nothing wrong on the path:
    /// the limit and the estimate follow it up (with a tenth to spare).
    ///
    /// The limit is what the path has said of itself, and a path that is
    /// carrying a set with no queue standing is saying that it carries
    /// it. The limit used to be raised by a carried probe alone, to a
    /// tenth over what the set tried then needed; a publisher's rate
    /// moves by more than that (its encoder climbing after a cut of its
    /// own, a key frame for a viewer that joined), and when the cost of
    /// the set passed the limit the allocation took a picture away "to
    /// fit the budget" on a path that was carrying it, and a probe gave
    /// it back a second or three later. (Seen on a live call,
    /// 2026-10-09: two of a subscriber's eleven real downgrades, with
    /// no cut of the estimate anywhere near.)
    ///
    /// This is the downlink's form of the uplink rule "the estimate
    /// rises whenever no queue stands" (`PathEstimator::confirming`):
    /// what may rise without a probe is bounded by what is in fact being
    /// sent and carried, never by arithmetic. A path that does not carry
    /// the grown set shows a queue, the cut is kept, and the picture
    /// goes at once as before. Not when the path itself has just lowered
    /// the estimate (`lowered`): then the estimate stands below the set
    /// on purpose.
    fn follow_what_is_carried(&mut self, wire_bps: u64, lowered: bool, now: Instant) {
        if lowered || self.limit_bps.is_none() || !self.estimator.has_arrivals() {
            return;
        }
        if wire_bps <= self.estimator.estimate_bps() || !self.path_is_quiet(now) {
            return;
        }
        let room = wire_bps.saturating_add(wire_bps / 10);
        self.limit_bps = self.limit_bps.map(|limit| limit.max(room));
        self.apply_ceiling();
        self.estimator.note_carried(room);
        self.followed = self.followed.saturating_add(1);
    }

    /// The log's note of a cut of the estimate made just now from
    /// `before`, with the queue as the delay shows it and beyond what
    /// the SFU's own sending explains.
    fn cut_note(
        &mut self,
        raw_queue: Option<Duration>,
        queue: Option<Duration>,
        before: u64,
        now: Instant,
    ) -> EstimateCut {
        let millis = |queue: Duration| u64::try_from(queue.as_millis()).unwrap_or(0);
        EstimateCut {
            reason: self.estimator.last_change().as_str(),
            to_bps: self.estimator.estimate_bps(),
            standing_queue_ms: raw_queue.map(millis),
            beyond_sending_ms: queue.map(millis),
            taken_back: false,
            held_at_steady: false,
            arriving_bps: self.estimator.arriving_latest_bps(),
            sending_bps: self.sent.bitrate_bps_at(now),
            from_bps: before,
        }
    }

    /// Take the subscriber's feedback on packets the SFU sent it: when
    /// each was sent, its size, and when the subscriber says it arrived
    /// (`None`: reported missing). Feeds the estimator, the loss count
    /// and a running probe. Returns whether the estimate was lowered by
    /// the path itself (not by a probe, and not without a standing
    /// queue).
    fn feed(&mut self, records: &[(Instant, usize, Option<Instant>)], now: Instant) -> bool {
        let before = self.estimator.estimate_bps();
        if self.probe.is_running() {
            if let Some((_, until)) = self.probe_floor.as_mut() {
                *until = now + DOWNLINK_PROBE_SHADOW;
            }
        }
        self.feed_records(records, before, now);
        // The estimate was lowered, or delay is growing while a rate is
        // being tried: a running probe ends here, and the next waits.
        let cuts = self.estimator.cuts();
        let lowered = cuts != self.seen_cuts;
        self.seen_cuts = cuts;
        let overuse = self.probe.is_running() && self.estimator.state() == DetectorState::Overuse;
        let (raw_queue, queue) = self.watch_queues(now);
        match queue {
            Some(queue) if queue >= DOWNLINK_STANDING_QUEUE => {
                self.queue_since.get_or_insert(now);
            },
            Some(_) => {
                self.queue_since = None;
                self.cut_pending = false;
            },
            None => {},
        }
        let stood = self
            .queue_since
            .is_some_and(|since| now.saturating_duration_since(since) >= DOWNLINK_QUEUE_STANDS_FOR);
        // A cut that was doubted, and the queue is still there: a path
        // whose queue is full shows no more growth of delay, so the
        // detector will not speak again. The cut is made now. So is one
        // the detector never asked for, when the queue is deep.
        let deep = queue.is_some_and(|queue| queue >= DOWNLINK_DEEP_QUEUE);
        let mut lowered = lowered;
        if (self.cut_pending || deep) && stood && !self.probe.is_running() {
            self.cut_pending = false;
            if self.estimator.confirm_overuse(now) {
                self.seen_cuts = self.estimator.cuts();
                lowered = true;
            }
            // Cut or not, the queue has to stand as long again before
            // the next: a cut takes that long to show. (Without this a
            // deep queue cut the estimate at every pass of the media
            // loop: 1089 cuts in a second, down to the floor.)
            self.queue_since = Some(now);
        }
        // A probe is filling the path on purpose: a queue of that depth
        // ends it, whatever the detector says and without waiting.
        // (Seen in two reports running: one reading of the queue is not
        // a queue, on a path with jitter. The probe cannot overflow the
        // path while it waits for the second: what it has sent ahead of
        // the feedback is bounded.)
        let deeper = self.probe.is_running()
            && raw_queue
                .is_some_and(|queue| queue >= self.probe_queue_base + DOWNLINK_STANDING_QUEUE);
        if deeper && !records.is_empty() {
            self.probe_queue_hits = self.probe_queue_hits.saturating_add(1);
        } else if !deeper {
            self.probe_queue_hits = 0;
        }
        let probe_queue = deeper && self.probe_queue_hits >= 2;
        if !(lowered || overuse || probe_queue) {
            return false;
        }
        // Lasting loss is trouble with or without a queue.
        let loss = lowered && self.estimator.last_change() == ChangeReason::Loss;
        let queued = if self.probe.is_running() {
            raw_queue.is_none() || probe_queue
        } else {
            queue.map_or(true, |queue| queue >= DOWNLINK_STANDING_QUEUE)
        };
        let stands = queue.is_none() || stood;
        let shadow = self
            .probe_floor
            .filter(|(_, until)| now <= *until)
            .map(|(floor, _)| floor);
        let real = if self.probe.is_running() {
            loss || queued
        } else {
            loss || stands
        };
        if real {
            self.last_trouble = Some(now);
            if let Some(outcome) = self.probe.on_trouble_while(now, shadow.is_some()) {
                self.outcomes.push(outcome);
            }
        }
        if !lowered {
            return false;
        }
        let mut note = self.cut_note(raw_queue, queue, before, now);
        if let Some(floor) = shadow {
            // The probe's doing: the estimate is as it was before it.
            self.estimator.note_carried(floor);
            note.taken_back = true;
        } else if !real {
            // No queue stands on the path: the detector was set off by
            // jitter, or by a queue too young to tell from a burst. The
            // estimate is as it was; a queue that goes on standing
            // brings the cut back.
            self.estimator.note_carried(before);
            note.taken_back = true;
            self.cut_pending = queued;
        }
        if !note.taken_back && !loss {
            self.hold_first_cut(&mut note, before, now);
        }
        if self.cut_notes.len() < 16 {
            self.cut_notes.push(note);
        }
        if !note.taken_back {
            self.cut_kept(now);
        }
        !note.taken_back
    }

    /// Judge the probes that ended since the last run: one the path
    /// carried raises the limit and the estimate to its rate and a tenth
    /// and vouches for that rate for [`DOWNLINK_PROVEN_FOR`]; one that
    /// failed puts the limit just under its rate; one given up for a
    /// clump changes nothing. Returns them.
    fn judge_probes(&mut self, now: Instant) -> Vec<ProbeOutcome> {
        let outcomes = std::mem::take(&mut self.outcomes);
        for outcome in &outcomes {
            if outcome.carried() {
                // The path carried more than is being sent and no queue
                // grew: whatever delay stands on it now is the path's
                // own, not a queue.
                self.rebase_queue();
                // And it carries at least this: the limit moves up to it
                // (with a tenth to spare, since what a set needs is
                // measured and moves from second to second).
                let room = outcome.target_bps.saturating_add(outcome.target_bps / 10);
                self.limit_bps = self.limit_bps.map(|limit| limit.max(room));
                self.apply_ceiling();
                // The estimate takes the same tenth: the set is given
                // when what it needs is within the budget, and what it
                // needs has moved by the time the probe ends. (Seen on a
                // live call, 2026-10-09: probes toward 872, 966 and
                // 1084 kbit/s carried one after the other, the set
                // needing a little more than the last probe's rate each
                // time, before a middle picture was given; of thirteen
                // carried probes two gave nothing and five gave half a
                // second late, when the estimate had risen by itself.)
                // What the set puts on the wire must still be within the
                // probe's rate.
                self.estimator.note_carried(room);
                // A cut in this probe's shadow is taken back to what the
                // probe has just shown, not to the estimate from before
                // it.
                if let Some((floor, _)) = self.probe_floor.as_mut() {
                    *floor = (*floor).max(self.estimator.estimate_bps());
                }
                self.proven = Some((outcome.target_bps, now));
                self.probes_carried = self.probes_carried.saturating_add(1);
            } else if outcome.failure == Some(crate::downlink_probe::ProbeFailure::Disturbed) {
                // Given up for a clump of media: nothing was learnt.
            } else {
                // The path does not carry this rate: the estimate stays
                // under it until a probe says otherwise.
                let under = outcome.target_bps.saturating_sub(1);
                self.limit_bps = Some(self.limit_bps.map_or(under, |l| l.min(under)));
                self.apply_ceiling();
            }
        }
        outcomes
    }

    /// A set that the path takes away again soon after it was given
    /// proved too large, and the next try at it waits longer each time
    /// ([`crate::downlink_probe::DownlinkProbe::on_given_taken_back`]);
    /// one that stays [`TOO_LARGE_WITHIN`] has proved it fits. Not when
    /// the path collapsed under it ([`DOWNLINK_COLLAPSE_DEN`]): that is
    /// not the set's doing. `changes` are this run's, `set` how large the
    /// set now given is and what it puts on the wire, `budget_bps` the
    /// estimate the run was made on. Returns the level that proved too
    /// large in this run and the wait it costs.
    fn note_given_and_taken(
        &mut self,
        changes: &[GivenChange],
        set: (u64, u64),
        budget_bps: Option<u64>,
        now: Instant,
    ) -> Option<(u64, Duration)> {
        let rank = downlink_allocation::rank;
        let up = changes.iter().any(|c| rank(c.to) > rank(c.from));
        let taken = changes
            .iter()
            .any(|c| matches!(c.why, "path-cut" | "over-budget"));
        let mut too_large = None;
        if up {
            self.last_up = Some((now, set.0, set.1));
        } else if taken {
            if let Some((at, level, wire)) = self.last_up.take() {
                let soon = now.saturating_duration_since(at) < TOO_LARGE_WITHIN;
                let collapsed =
                    budget_bps.map_or(true, |budget| budget < wire / DOWNLINK_COLLAPSE_DEN);
                if soon && !collapsed {
                    too_large = Some((level, self.probe.on_given_taken_back(level, now)));
                }
            }
        }
        if let Some((at, level, _)) = self.last_up {
            if now.saturating_duration_since(at) >= TOO_LARGE_WITHIN {
                self.probe.on_given_held(level);
                self.last_up = None;
            }
        }
        too_large
    }

    /// What the set being given now puts on the wire, as the allocation
    /// will take it: a publisher it has not met is on its light track.
    fn wire_now(&self, publishers: &[downlink_allocation::Publisher<MediaPeerId>]) -> u64 {
        let current: Vec<VideoQuality> = publishers
            .iter()
            .map(|p| {
                downlink_allocation::smaller(
                    p.wanted,
                    self.allocation
                        .given(&p.publisher)
                        .unwrap_or(VideoQuality::Light),
                )
            })
            .collect();
        downlink_allocation::wire_bps(publishers, &current)
    }

    /// What the path has been seen to carry, as of `now`: the rate of a
    /// probe that was just carried, or what is arriving on a quiet path.
    fn carried_bps(&mut self, now: Instant) -> Option<u64> {
        // Nothing vouches for a larger set while a queue stands on the
        // path or a cut waits to be borne out, not even a probe that has
        // just been carried: if the queue goes, the probe's word is
        // still good for [`DOWNLINK_PROVEN_FOR`]; if it stays, the set
        // would have been given into it. (Seen on a live call,
        // 2026-10-09: a middle picture given half a second after its
        // probe, with a queue of 98 ms standing, and taken away a second
        // later at 270 ms.)
        if self.raw_queue_since.is_some() || self.cut_pending {
            return None;
        }
        let proven = self
            .proven
            .filter(|(_, at)| now.saturating_duration_since(*at) <= DOWNLINK_PROVEN_FOR)
            .map(|(bps, _)| bps);
        // While a probe runs its padding is arriving too, and whether the
        // path carried that is the probe's verdict to give.
        let quiet = !self.probe.is_running()
            && self.estimator.state() != DetectorState::Overuse
            && self.last_trouble.map_or(true, |at| {
                now.saturating_duration_since(at) >= DOWNLINK_QUIET_AFTER_TROUBLE
            });
        let arriving = if quiet {
            self.estimator.arriving_latest_bps()
        } else {
            None
        };
        proven.max(arriving)
    }
}

/// What a publisher's tracks cost, as measured where its packets arrive
/// ([`MediaPeer::published_rates`]), bits per second.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PublishedRates {
    /// Its main video (every main video track it sends, summed; of a
    /// simulcast track, the heaviest layer).
    pub main_bps: u64,
    /// Its middle video track.
    pub middle_bps: u64,
    /// Its light video (a declared light track, or the light layer of a
    /// simulcast track).
    pub light_bps: u64,
    /// Its audio.
    pub audio_bps: u64,
}

/// One change of what a subscriber is given of a publisher's video, for
/// the log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GivenChange {
    /// The publisher.
    pub publisher: MediaPeerId,
    /// What the subscriber was being sent.
    pub from: VideoQuality,
    /// What it is sent now.
    pub to: VideoQuality,
    /// What the subscriber asks for.
    pub wanted: VideoQuality,
    /// Why, as a stable label for the log
    /// ([`MediaPeer::run_downlink`]). Down: `wants` (the subscriber asks
    /// for less), `path-cut` (the path lowered
    /// the estimate under what the set puts on the wire), `over-budget`
    /// (the set came to cost more than the estimate with no cut),
    /// `importance` (the subscriber changed its order), `first-listed`
    /// (a publisher met while the budget is known starts at its light
    /// picture: nothing was taken from the viewer). Up: `probe` (a probe
    /// was just carried), `quiet-path` (what is arriving covers the
    /// set).
    pub why: &'static str,
}

/// The change of what a publisher is given that `entry` amounts to, if
/// any, with why: `why_up` when it moved up; down, `first-listed` for a
/// publisher met for the first time (given what was asked for it is no
/// change; given less it is one, though nothing was taken from the
/// viewer), `wants` when the subscriber asks for less, else `why_down`.
fn given_change(
    wanted: VideoQuality,
    entry: &downlink_allocation::Given<MediaPeerId>,
    why_up: &'static str,
    why_down: &'static str,
) -> Option<GivenChange> {
    let rank = downlink_allocation::rank;
    let from = entry.previous.unwrap_or(wanted);
    if from == entry.given {
        return None;
    }
    let why = if rank(entry.given) > rank(from) {
        why_up
    } else if entry.previous.is_none() {
        "first-listed"
    } else if rank(wanted) < rank(from) {
        "wants"
    } else {
        why_down
    };
    Some(GivenChange {
        publisher: entry.publisher.clone(),
        from,
        to: entry.given,
        wanted,
        why,
    })
}

/// What one run of a subscriber's allocation came to
/// ([`MediaPeer::run_downlink`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DownlinkRun {
    /// The budget it ran with; `None` while nothing is known of the path.
    pub budget_bps: Option<u64>,
    /// What the path was seen to carry, as the allocation was told.
    pub carried_bps: Option<u64>,
    /// The rate of the probe this run started, if it started one.
    pub probe_started_bps: Option<u64>,
    /// The probes that ended since the last run.
    pub outcomes: Vec<ProbeOutcome>,
    /// The cuts of the estimate since the last run.
    pub cuts: Vec<EstimateCut>,
    /// Every publisher whose given quality changed.
    pub changes: Vec<GivenChange>,
    /// The `video-given` set to send the subscriber, when it is to be
    /// told: the set changed, or it sent a `video-wants`.
    pub tell: Option<Vec<VideoGiven>>,
    /// What the set given after this run puts on the wire, and what it
    /// needs with its headroom.
    pub wire_bps: u64,
    /// See `wire_bps`.
    pub need_bps: u64,
    /// The queue standing on the path at this run, milliseconds.
    pub queue_ms: Option<u64>,
    /// A set proved too large in this run (it was given and the path
    /// took it away again within
    /// [`crate::downlink_probe::TOO_LARGE_WITHIN`]): how large it was
    /// ([`downlink_allocation::set_level`]), and how long the next try
    /// at a set that large or larger waits.
    pub too_large: Option<(u64, Duration)>,
}

/// One cut of a subscriber's downlink estimate, for the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EstimateCut {
    /// Why ([`crate::uplink_estimate::ChangeReason::as_str`]).
    pub reason: &'static str,
    /// What the estimate was cut to, bits per second.
    pub to_bps: u64,
    /// The queue standing on the path at the cut, milliseconds; `None`
    /// when it could not be told.
    pub standing_queue_ms: Option<u64>,
    /// The cut was taken back: it came while a probe ran or just after
    /// one ([`DOWNLINK_PROBE_SHADOW`]), or with no queue standing on the
    /// path ([`DOWNLINK_STANDING_QUEUE`]).
    pub taken_back: bool,
    /// How much of that queue the SFU's own sending does not explain on
    /// a path that carries the estimate.
    pub beyond_sending_ms: Option<u64>,
    /// The cut was taken back because the path had carried what is being
    /// sent steadily until then; it is made if the queue goes on
    /// standing.
    pub held_at_steady: bool,
    /// What the feedback acknowledged as arriving over the last half
    /// second at the cut; `None` when too little did.
    pub arriving_bps: Option<u64>,
    /// The media the SFU was writing to the subscriber then (first
    /// transmissions). Beside `arriving_bps` it says whether the path
    /// was short of what was sent or the cut was made on something else.
    pub sending_bps: u64,
    /// The estimate before the cut.
    pub from_bps: u64,
}

/// One line of a subscriber's downlink for the diagnostics interval
/// ([`MediaPeer::downlink_snapshot`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DownlinkSnapshot {
    /// The budget; `None` while nothing is known of the path.
    pub budget_bps: Option<u64>,
    /// The estimate as it stands, the starting value included.
    pub estimate_bps: Option<u64>,
    /// What the path was last seen to carry.
    pub carried_bps: Option<u64>,
    /// The rate of the media being written to the subscriber.
    pub sending_bps: u64,
    /// What the SFU would like to send the subscriber, headroom included.
    pub demand_bps: Option<u64>,
    /// The rate being tried on the path right now, if a probe runs.
    pub probing_bps: Option<u64>,
    /// Probes started and carried since the subscriber joined, and the
    /// bytes of padding queued for them.
    pub probes_started: u64,
    /// See `probes_started`.
    pub probes_carried: u64,
    /// See `probes_started`.
    pub padding_bytes: u64,
    /// Times the estimate was lowered since the subscriber joined.
    pub cuts: u64,
    /// The queue standing on the path, milliseconds; `None` when it
    /// cannot be told.
    pub queue_ms: Option<u64>,
    /// The delay detector's state
    /// ([`crate::uplink_estimate::DetectorState::as_str`]); empty before
    /// the first snapshot.
    pub detector: &'static str,
    /// Publishers given their main track.
    pub main: usize,
    /// Publishers given their middle track.
    pub middle: usize,
    /// Publishers given their light track alone.
    pub light: usize,
    /// What the path has said of itself: the estimate's ceiling. `None`
    /// until it has.
    pub limit_bps: Option<u64>,
    /// While a set that proved too large is waited out: the level from
    /// which sets are held ([`downlink_allocation::set_level`]: one per
    /// middle picture, six per main one).
    pub held_from_level: Option<u64>,
    /// The wait the next set that proves too large will cost, seconds.
    pub too_large_wait_secs: u64,
    /// Times the limit followed the cost of the set being given up on a
    /// quiet path.
    pub followed: u64,
}

/// Who decides a publisher's bitrate
/// ([ADR-0068](../../../docs/adr/0068-server-side-bitrate-control.md)).
/// The operator switch is `LAKKO_SFU_BITRATE_CONTROL`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BitrateControl {
    /// The client's own send-side congestion controller, from the
    /// transport-wide feedback the SFU returns. The behaviour before
    /// ADR-0068, and the default.
    #[default]
    Client,
    /// The SFU: it negotiates no transport-wide congestion control,
    /// estimates what each publisher's path carries
    /// ([`crate::uplink_estimate`]) and tells the publisher with REMB.
    Server(EstimatorConfig),
}

impl BitrateControl {
    /// Stable label for the startup log line: the value of
    /// `LAKKO_SFU_BITRATE_CONTROL` that selects this mode.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Server(_) => "server",
        }
    }
}

/// How often the loss a publisher's streams saw is handed to its
/// estimator. One second: the interval the estimator's loss rule is written
/// against, and str0m's own receiver-report interval for video.
const UPLINK_LOSS_INTERVAL: Duration = Duration::from_secs(1);

/// A changed estimate is sent to the publisher no sooner than this after
/// the last REMB, unless it fell (see [`REMB_DROP_PERCENT`]). libwebrtc's
/// receive side sends REMB at most every 200 ms (`kRembSendIntervalMs`,
/// from memory of its source; not re-read for this change).
const REMB_MIN_INTERVAL: Duration = Duration::from_millis(200);

/// A REMB goes out at least this often, changed or not. Chosen, not
/// measured: the media loop passes at least every [`MAX_TICK_INTERVAL`],
/// so a publisher sees one within a second with room to spare, and a lost
/// REMB is replaced before a libwebrtc sender's start phase (two seconds
/// after its first receiver report) is over.
const REMB_MAX_INTERVAL: Duration = Duration::from_millis(500);

/// An estimate below this percentage of the last one sent goes out at
/// once. libwebrtc: `kSendThresholdPercent` = 97 (from memory of its
/// source; not re-read for this change).
const REMB_DROP_PERCENT: u64 = 97;

/// Most video receive streams of one publisher a REMB is sent on. A stream
/// beyond that starts the list over. Chosen: three simulcast layers with
/// room to spare.
const MAX_REMB_STREAMS: usize = 8;

/// RTP bytes str0m does not hand over with a packet's payload are counted
/// from the parsed header length; this is the fallback when a packet claims
/// none, the fixed RTP header.
const RTP_FIXED_HEADER_LEN: usize = 12;

/// What the SFU keeps per publisher to decide its bitrate. Present only
/// with [`BitrateControl::Server`].
#[derive(Debug)]
struct Uplink {
    estimator: UplinkEstimator,
    /// Loss and late packets from sequence numbers, all streams.
    loss: SeqLossCounter,
    /// A fixed instant the absolute send time is taken modulo 64 s
    /// against; see [`abs_send_time_24`].
    send_time_base: Instant,
    /// Main SSRCs of the video streams this publisher has sent on, in
    /// order of first packet. A REMB is requested on each.
    video_ssrcs: Vec<Ssrc>,
    last_loss_feed: Instant,
    /// When the last REMB was requested and the figure it carried.
    last_remb: Option<(Instant, u64)>,
}

/// What [`MediaPeer::uplink_seen`] reads off a packet before the packet is
/// consumed: its absolute send time and its size on the wire.
#[derive(Debug, Clone, Copy)]
pub(crate) struct UplinkSeen {
    abs_send_time: Option<Instant>,
    size: usize,
}

/// The 24-bit absolute send time a packet carried, recovered from the
/// `Instant` str0m turned it into.
///
/// str0m places the 24-bit value (6.18 fixed point seconds, wrapping every
/// 64 s) on its own clock by picking the 64-second cycle that puts it just
/// before the arrival time. Which cycle it picks can differ between two
/// packets when the publisher's clock runs slightly ahead of the SFU's, but
/// it only ever differs by a whole number of cycles, so the value modulo
/// 64 s against any fixed instant is the publisher's own value plus a
/// constant. A constant does not matter: the estimator uses differences.
fn abs_send_time_24(sent: Instant, base: Instant) -> u32 {
    const CYCLE_MICROS: u128 = 64_000_000;
    let micros = if sent >= base {
        sent.duration_since(base).as_micros() % CYCLE_MICROS
    } else {
        (CYCLE_MICROS - base.duration_since(sent).as_micros() % CYCLE_MICROS) % CYCLE_MICROS
    };
    // 2^18 units per second; below 2^24 by construction.
    let units = micros * (1 << 18) / 1_000_000;
    u32::try_from(units & 0x00ff_ffff).unwrap_or(0)
}

/// The session extension map every peer's `Rtc` is built from: str0m's
/// standard map without the audio level, and, when `transport_cc` is
/// false ([`BitrateControl::Server`] without downlink control), without the
/// transport-wide sequence number either; `abs-send-time` stays.
///
/// An extension that is not in the session map is neither offered nor
/// echoed in an answer, and its id on a received packet maps to nothing,
/// so no media line of the peer negotiates it and no value is parsed.
///
/// **The audio level (`urn:ietf:params:rtp-hdrext:ssrc-audio-level`,
/// RFC 6464) is never in it.** A header extension is outside the SFrame
/// seal, so negotiating this one would tell the server, in the clear and
/// on every audio packet, who is speaking and how loudly
/// (`docs/ux/group-call-grid.md` section 10, `docs/security/calls.md`
/// C-39). str0m's standard map carries it on id 1, which is why the map is
/// always replaced rather than extended. No client needs it: each one
/// measures the audio it decoded.
fn session_extension_map(transport_cc: bool) -> ExtensionMap {
    let mut map = ExtensionMap::empty();
    for (id, ext) in ExtensionMap::standard().iter() {
        let dropped = match ext {
            Extension::AudioLevel => true,
            Extension::TransportSequenceNumber => !transport_cc,
            _ => false,
        };
        if !dropped {
            map.set(id, ext.clone());
        }
    }
    map
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
        Self::with_options(id, now, None, false)
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
        Self::with_options(id, now, stats_interval, false)
    }

    /// As [`Self::with_stats_interval`], additionally choosing
    /// whether video FEC (`red/90000` + `ulpfec/90000`,
    /// [`crate::video_fec`]) may be negotiated with this peer.
    /// `false` is exactly the behaviour without the feature:
    /// neither payload type is offered or answered. The operator
    /// switch is `LAKKO_SFU_VIDEO_FEC`.
    #[must_use]
    pub fn with_options(
        id: MediaPeerId,
        now: Instant,
        stats_interval: Option<Duration>,
        video_fec: bool,
    ) -> Self {
        Self::with_bitrate_control(id, now, stats_interval, video_fec, BitrateControl::Client)
    }

    /// As [`Self::with_options`], additionally choosing who decides
    /// this peer's publishing bitrate (ADR-0068).
    /// [`BitrateControl::Client`] is exactly the behaviour without
    /// the feature. With [`BitrateControl::Server`] the peer's
    /// session carries no transport-wide sequence number extension
    /// and no `transport-cc` feedback on any media line, in the
    /// answers to its offers and in the SFU's offers to it alike,
    /// while `abs-send-time` and `goog-remb` stay; the SFU then
    /// estimates what the peer's path carries and sends it REMB
    /// ([`Self::drive_uplink`]).
    #[must_use]
    pub fn with_bitrate_control(
        id: MediaPeerId,
        now: Instant,
        stats_interval: Option<Duration>,
        video_fec: bool,
        bitrate: BitrateControl,
    ) -> Self {
        Self::with_protection(
            id,
            now,
            stats_interval,
            video_fec,
            bitrate,
            LossProtection::default(),
        )
    }

    /// As [`Self::with_downlink_control`] with downlink control off:
    /// exactly the behaviour without ADR-0070 step 6.
    #[must_use]
    pub fn with_protection(
        id: MediaPeerId,
        now: Instant,
        stats_interval: Option<Duration>,
        video_fec: bool,
        bitrate: BitrateControl,
        protection: LossProtection,
    ) -> Self {
        Self::with_downlink_control(
            id,
            now,
            stats_interval,
            video_fec,
            bitrate,
            protection,
            false,
        )
    }

    /// As [`Self::with_bitrate_control`], additionally choosing what the
    /// peer does to carry video through a burst of loss (ADR-0070 steps
    /// 3 and 4); [`LossProtection::default`] is everything on and is what
    /// every other constructor uses. With
    /// [`LossProtection::playout_delay`] the peer's session carries the
    /// playout-delay header extension under
    /// [`PLAYOUT_DELAY_EXTENSION_ID`], so it is offered to a subscriber
    /// and echoed to a peer that offers it.
    ///
    /// `downlink_control` switches on ADR-0070 step 6 for this peer, and
    /// only together with [`BitrateControl::Server`] (with
    /// [`BitrateControl::Client`] it is ignored). The peer's session then
    /// negotiates transport-wide congestion control **per direction**: the
    /// transport-wide sequence number extension and `transport-cc`
    /// feedback on the media lines the SFU offers (the ones it sends on),
    /// and neither on the media lines the peer offers (the ones it
    /// publishes on), which stay exactly as [`BitrateControl::Server`]
    /// has them. str0m keeps what the peer's transport-wide feedback says
    /// of each packet the SFU sent it, for the SFU's own estimate of the
    /// path ([`crate::uplink_estimate::PathEstimator`]); str0m's own
    /// send-side estimator and its pacer stay off.
    #[must_use]
    pub fn with_downlink_control(
        id: MediaPeerId,
        now: Instant,
        stats_interval: Option<Duration>,
        video_fec: bool,
        bitrate: BitrateControl,
        protection: LossProtection,
        downlink_control: bool,
    ) -> Self {
        let server = matches!(bitrate, BitrateControl::Server(_));
        let downlink_control = downlink_control && server;
        // RTP mode (ADR-0066): str0m hands over every RTP packet as it
        // arrives (`Event::RtpPacket`) and takes packets to send
        // (`StreamTx::write_rtp`) instead of assembling and re-cutting
        // frames. Nothing waits behind a missing packet, and the reordering
        // sizes of frame mode do not apply.
        //
        // Register the generic frame descriptor so it is negotiated in both
        // directions — echoed in the answer to a publisher's offer, and
        // offered in the SFU's own offer to a subscriber. Without it in this
        // session map str0m drops the extension three times over: it is not
        // echoed in the answer, not adopted from the offer, and not parsed
        // off the wire. See [`crate::frame_descriptor`] for what carrying it
        // costs in exposure.
        //
        // Send-side bandwidth estimation is OFF, and with it str0m's pacer.
        // The SFU relays media it cannot re-encode: when the estimate toward a
        // subscriber collapses after a loss burst (60 kbps was seen live on
        // 2026-09-19 after a one-second blackout), the pacer holds a stream it
        // cannot thin far below its rate, the packets queue, the picture stops
        // for 15 s and then arrives in a burst. Unpaced, packets leave as
        // they arrive and a subscriber that is really short of bandwidth asks
        // its publisher for less through its own receiver feedback. The cost
        // is that no send-side estimate (`BweKind::Twcc`) ever fires, so the
        // simulcast layer policy (`drive_layer_selection`) has no budget to
        // act on and a main track stays on the heaviest layer. A subscriber's
        // REMB still arrives as `Event::EgressBitrateEstimate`; it is not a
        // budget and is not taken for one (`bwe_kind_bps`). TWCC feedback
        // toward publishers is a receive-side setting and is unaffected.
        //
        // Redundant audio (RFC 2198, ADR-0067 step 3) is switched on: a
        // `red/48000/2` payload type listed before Opus, in the answer to a
        // publisher that offers it and in the SFU's own offers. A client
        // builds the RED container before SFrame seals the frame, so the SFU
        // forwards a RED payload as it forwards any other, unread, and only
        // to a subscriber whose track negotiated `red` (see
        // [`Self::forward_packet`]).
        //
        // That holds under downlink control (ADR-0070 step 6) too: the
        // estimate of a subscriber's path is the SFU's own, fed from the
        // feedback records str0m hands out, and what does not fit the path
        // is a track the allocation pauses, never a queue.
        //
        // The session map is always replaced, never str0m's default: the
        // default carries the audio level, which this SFU negotiates with
        // nobody ([`session_extension_map`]). ADR-0068: under server-side
        // bitrate control without downlink control the transport-wide
        // sequence number is left out too.
        let transport_cc = !server || downlink_control;
        let mut config = Rtc::builder()
            .set_rtp_mode(true)
            .enable_bwe(None)
            .set_extension_map(session_extension_map(transport_cc))
            .set_extension(
                frame_descriptor::GENERIC_FRAME_DESCRIPTOR_ID,
                frame_descriptor::generic_frame_descriptor_extension(),
            )
            .set_stats_interval(stats_interval);
        if protection.playout_delay {
            // ADR-0070 step 4. str0m writes the extension on a packet that
            // carries both values and whose media line negotiated it.
            config = config.set_extension(PLAYOUT_DELAY_EXTENSION_ID, Extension::PlayoutDelay);
        }
        config.codec_config().enable_red(true);
        // Video FEC (ADR-0067 step 4) is opt-in per deployment: with it, a
        // `red/90000` (with its RTX) and a `ulpfec/90000` payload type are
        // answered to a publisher that offers both and offered to every
        // subscriber. Unlike audio RED, the video RED header and the FEC
        // header are outside the SFrame-sealed frame, so the SFU can and
        // must map them per subscriber (see [`Self::forward_packet`]).
        config.codec_config().enable_video_fec(video_fec);
        if server && !downlink_control {
            // ...and no `a=rtcp-fb:<pt> transport-cc` on any payload type.
            // str0m writes the feedback lines of an answer or offer from
            // its own configured payload parameters; `goog-remb` (video)
            // and the rest stay as they are.
            let params: Vec<PayloadParams> = config
                .codec_config()
                .params()
                .iter()
                .map(|p| {
                    let mut p = *p;
                    p.set_fb_transport_cc(false);
                    p
                })
                .collect();
            *config.codec_config() = CodecConfig::new_from_payload_params(params);
        }
        let mut rtc = config.build(now);
        if downlink_control {
            // The extension and the feedback stay in the session, and the
            // vendored str0m leaves both out of every media line the peer
            // offers: what the peer publishes is still estimated by the
            // SFU and answered with REMB (ADR-0068), what the SFU sends it
            // is estimated from the peer's transport-wide feedback.
            let mut api = rtc.direct_api();
            api.set_twcc_on_local_media_only(true);
            api.keep_twcc_feedback(true);
        }
        if video_fec {
            // The FEC receiver needs each packet as the publisher sent it,
            // header extension block included; str0m otherwise keeps only
            // the parsed values.
            rtc.direct_api().keep_received_rtp(true);
        }
        Self {
            id,
            call_id: String::new(),
            rtc,
            connected: false,
            tracks_in: Vec::new(),
            tracks_out: Vec::new(),
            pending_offer: None,
            egress_estimate_bps: None,
            layer_meters: HashMap::new(),
            last_keyframe_request: HashMap::new(),
            unanswered_keyframe_requests: HashMap::new(),
            pli_relays: HashMap::new(),
            owed_keyframe_requests: std::collections::HashSet::new(),
            video_fec,
            fec_receivers: HashMap::new(),
            fec_senders: HashMap::new(),
            fec_sent: Vec::new(),
            uplink: match bitrate {
                BitrateControl::Client => None,
                BitrateControl::Server(estimator) => Some(Box::new(Uplink {
                    estimator: UplinkEstimator::new(estimator, now).confirming(),
                    loss: SeqLossCounter::new(),
                    send_time_base: now,
                    video_ssrcs: Vec::new(),
                    last_loss_feed: now,
                    last_remb: None,
                })),
            },
            uplink_fallback_on: false,
            uplink_fallbacks: HashMap::new(),
            layer_peak_bps: HashMap::new(),
            light_rids: HashMap::new(),
            light_mid: None,
            middle_mid: None,
            video_wants: HashMap::new(),
            protection,
            fast_nack_ssrcs: Vec::new(),
            light_duplicates: LightDuplicates::default(),
            duplicates_sent: Vec::new(),
            uplink_bursts: UplinkBursts::new(),
            burst_ends: BurstEnds::new(),
            downlink_bursts: DownlinkBursts::new(),
            video_sent: 0,
            reports_read: HashMap::new(),
            playout: HashMap::new(),
            downlink: downlink_control.then(|| Box::new(Downlink::new(now))),
            track_meters: HashMap::new(),
            needs: SenderNeeds::default(),
            budget: PublishBudget::new(),
        }
    }

    /// Whether downlink control (ADR-0070 step 6) runs for this peer.
    #[must_use]
    pub const fn downlink_control(&self) -> bool {
        self.downlink.is_some()
    }

    /// What this peer does to carry video through a burst of loss.
    #[must_use]
    pub const fn protection(&self) -> LossProtection {
        self.protection
    }

    /// Switch the uplink fallback for the simulcast video this peer
    /// publishes on or off ([`crate::uplink_fallback`]). Off by
    /// default.
    pub fn set_uplink_fallback(&mut self, on: bool) {
        self.uplink_fallback_on = on;
        if !on {
            self.uplink_fallbacks.clear();
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
    /// fanout can write upstream RTP to them. Read-only
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
        let track = TrackIn {
            mid,
            kind,
            simulcast: false,
            light: kind == MediaKind::Video && self.light_mid == Some(mid),
            middle: kind == MediaKind::Video && self.middle_mid == Some(mid),
        };
        self.tracks_in.push(track);
        // The negotiation may already list the track's rids.
        self.note_simulcast_track_in(mid).unwrap_or(track)
    }

    /// Take the `light_mids` of an SDP offer THIS peer sent and the
    /// SFU has just accepted: the mids of the offer's m-lines that
    /// carry a light copy of the peer's camera, sent as a second
    /// ordinary video sender beside the main one.
    ///
    /// The first mid that passes is remembered as the peer's light
    /// track, for good; the track is then announced with
    /// [`TrackIn::light`] set and reaches subscribers as the peer's
    /// light companion only ([`TrackRole`]). A mid is taken only when
    /// it names a **video** m-line of this peer that the peer sends
    /// on and that has not already been announced as an ordinary
    /// track (an offer names its new m-lines before str0m announces
    /// them, so a declaration made with the m-line always passes).
    /// Naming the mid that is already the light track again is a
    /// no-op, so a client may repeat the list in every offer.
    ///
    /// Returns what was refused and why, for the caller's log. A
    /// refused mid changes nothing: its track, if any, stays an
    /// ordinary track of its kind.
    pub fn declare_light_tracks(
        &mut self,
        light_mids: &[String],
    ) -> Vec<(Mid, LightDeclarationRefused)> {
        let mut refused = Vec::new();
        for raw in light_mids {
            let mid = Mid::from(raw.as_str());
            if self.light_mid == Some(mid) {
                continue;
            }
            let problem = self.declaration_problem(mid, self.light_mid, self.middle_mid);
            match problem {
                Some(why) => refused.push((mid, why)),
                None => self.light_mid = Some(mid),
            }
        }
        refused
    }

    /// Take the `middle_mids` of an SDP offer THIS peer sent and the
    /// SFU has just accepted: the mids of the offer's m-lines that
    /// carry the middle copy of the peer's camera (ADR-0070 step 5),
    /// a third ordinary video sender.
    ///
    /// The rules are those of [`Self::declare_light_tracks`]: the
    /// first mid that passes is the peer's middle track for good, a
    /// mid is taken only when it names a video m-line the peer sends
    /// on that has not been announced as an ordinary track, and
    /// naming the middle track again is a no-op. A mid that is the
    /// peer's light track is refused. The track is then announced
    /// with [`TrackIn::middle`] set and reaches subscribers as the
    /// peer's middle track only ([`TrackRole::Middle`]).
    ///
    /// Returns what was refused and why, for the caller's log.
    pub fn declare_middle_tracks(
        &mut self,
        middle_mids: &[String],
    ) -> Vec<(Mid, LightDeclarationRefused)> {
        let mut refused = Vec::new();
        for raw in middle_mids {
            let mid = Mid::from(raw.as_str());
            if self.middle_mid == Some(mid) {
                continue;
            }
            let problem = self.declaration_problem(mid, self.middle_mid, self.light_mid);
            match problem {
                Some(why) => refused.push((mid, why)),
                None => self.middle_mid = Some(mid),
            }
        }
        refused
    }

    /// Why `mid` cannot become this peer's light (or middle) track, if
    /// it cannot. `declared` is the track of that kind the peer already
    /// has, `other` its track of the other kind.
    fn declaration_problem(
        &self,
        mid: Mid,
        declared: Option<Mid>,
        other: Option<Mid>,
    ) -> Option<LightDeclarationRefused> {
        if other == Some(mid) {
            return Some(LightDeclarationRefused::DeclaredOtherwise);
        }
        if declared.is_some() {
            return Some(LightDeclarationRefused::AlreadyDeclared);
        }
        let Some(media) = self.rtc.media(mid) else {
            return Some(LightDeclarationRefused::UnknownMid);
        };
        if media.kind() != MediaKind::Video {
            return Some(LightDeclarationRefused::NotVideo);
        }
        // One of the SFU's own lines to this peer, or a line the peer
        // only receives on.
        let downstream = self.tracks_out.iter().any(|t| {
            matches!(t.state, TrackOutState::Negotiating(m) | TrackOutState::Open(m) if m == mid)
        });
        if downstream || !media.direction().is_receiving() {
            return Some(LightDeclarationRefused::NotSentByPeer);
        }
        if self.tracks_in.iter().any(|t| t.mid == mid) {
            return Some(LightDeclarationRefused::AlreadyMain);
        }
        None
    }

    /// The mid of the light copy of its camera THIS peer declared,
    /// if it declared one ([`Self::declare_light_tracks`]).
    #[must_use]
    pub const fn light_mid(&self) -> Option<Mid> {
        self.light_mid
    }

    /// The mid of the middle copy of its camera THIS peer declared,
    /// if it declared one ([`Self::declare_middle_tracks`]).
    #[must_use]
    pub const fn middle_mid(&self) -> Option<Mid> {
        self.middle_mid
    }

    /// The simulcast layers of the track THIS peer publishes on
    /// `mid`: the rids its negotiation lists, and every rid a
    /// packet has arrived on. Empty for a track without rids.
    fn simulcast_rids(&self, mid: Mid) -> Vec<Rid> {
        let mut rids: Vec<Rid> = match self.rtc.media(mid).map(str0m::media::Media::rids_rx) {
            Some(Rids::Specific(listed)) => listed.clone(),
            _ => Vec::new(),
        };
        for (m, rid) in self.layer_meters.keys() {
            if *m == mid && !rids.contains(rid) {
                rids.push(*rid);
            }
        }
        rids
    }

    /// Mark the video track THIS peer publishes on `mid` as
    /// simulcast if it has more than one layer and was not marked
    /// before. Returns the track, now marked, the one time that
    /// happens, so the caller can open the light companion on every
    /// subscriber; `None` otherwise (unknown mid, audio, a single
    /// layer, already marked, or the peer's declared light track,
    /// which is a companion as a whole).
    ///
    /// Called when the track is announced, where the negotiation
    /// usually lists the rids, and for every video packet that
    /// carries a rid, which covers a track whose negotiation did
    /// not.
    pub fn note_simulcast_track_in(&mut self, mid: Mid) -> Option<TrackIn> {
        let idx = self.tracks_in.iter().position(|t| t.mid == mid)?;
        let track = self.tracks_in[idx];
        if track.simulcast || track.light || track.middle || track.kind != MediaKind::Video {
            return None;
        }
        if self.simulcast_rids(mid).len() < 2 {
            return None;
        }
        self.tracks_in[idx].simulcast = true;
        Some(self.tracks_in[idx])
    }

    /// The layers of the simulcast video track THIS peer publishes
    /// on `mid`: which is the light one and which the heaviest.
    /// `None` when the track has fewer than two layers.
    ///
    /// - **Light.** The rid `q` when the track has one (the Lakko
    ///   clients name their light layer so); otherwise the layer
    ///   with the lowest measured bitrate, once every layer has
    ///   been measured. Fixed from the first time it is known.
    /// - **Heaviest.** With exactly two layers, the one that is
    ///   not the light one. With more, the layer with the highest
    ///   measured bitrate, once every layer has been measured.
    ///
    /// Bitrates are the highest each layer has shown so far, not
    /// the current ones, for the reason given at `layer_peak_bps`.
    pub fn simulcast_layout(&mut self, mid: Mid) -> Option<SimulcastLayout> {
        let rids = self.simulcast_rids(mid);
        if rids.len() < 2 {
            return None;
        }
        let peaks: Vec<(Rid, u64)> = rids
            .iter()
            .map(|rid| {
                let now = self
                    .layer_meters
                    .get(&(mid, *rid))
                    .map_or(0, LayerBitrateMeter::bitrate_bps);
                let peak = self.layer_peak_bps.get(&(mid, *rid)).copied().unwrap_or(0);
                (*rid, now.max(peak))
            })
            .collect();
        let named = Rid::from(LIGHT_RID);
        let light = self.light_rids.get(&mid).copied().or_else(|| {
            let found = if rids.contains(&named) {
                Some(named)
            } else {
                uplink_fallback::pick_light(&peaks, named)
            };
            if let Some(rid) = found {
                self.light_rids.insert(mid, rid);
            }
            found
        });
        let heaviest = match (light, rids.as_slice()) {
            (Some(light), [a, b]) => Some(if *a == light { *b } else { *a }),
            _ => uplink_fallback::pick_heaviest(&peaks),
        }
        .filter(|rid| Some(*rid) != light);
        Some(SimulcastLayout { light, heaviest })
    }

    /// Record an upstream track from a *different* peer
    /// that this peer needs to receive. Adds a fresh
    /// [`TrackOutEntry`] in [`TrackOutState::ToOpen`]
    /// state; the SDP-renegotiation pass picks it up.
    ///
    /// A simulcast video track (`source.simulcast`) gets a second
    /// entry, its light companion ([`TrackRole`]); the two are
    /// opened by the same renegotiation when they are added
    /// together. A track that is found to be simulcast only after
    /// its main track was added is passed here again, and the
    /// companion alone is added then.
    ///
    /// A publisher's declared light track (`source.light`) gets a
    /// companion entry **only**: it is never opened as a main
    /// track.
    ///
    /// Idempotent on `(source_peer, source_mid, role)` so the
    /// router's broadcast-to-everyone-else pass cannot
    /// accumulate duplicates if a `MediaAdded` event
    /// fires twice. Returns whether anything was added.
    pub fn observe_remote_track_in(&mut self, source_peer: MediaPeerId, source: TrackIn) -> bool {
        let mut added = false;
        let video = source.kind == MediaKind::Video;
        let roles: &[TrackRole] = if source.light && video {
            &[TrackRole::LightCompanion]
        } else if source.middle && video {
            &[TrackRole::Middle]
        } else if source.simulcast && video {
            &[TrackRole::Main, TrackRole::LightCompanion]
        } else {
            &[TrackRole::Main]
        };
        for &role in roles {
            let already = self.tracks_out.iter().any(|t| {
                t.source_peer == source_peer && t.source_mid == source.mid && t.role == role
            });
            if already {
                continue;
            }
            self.tracks_out.push(TrackOutEntry {
                source_peer: source_peer.clone(),
                source_mid: source.mid,
                kind: source.kind,
                state: TrackOutState::ToOpen,
                role,
                chosen_rid: None,
                forwarding: Forwarding::default(),
            });
            added = true;
        }
        if added {
            // A track the subscriber does not show starts paused.
            self.apply_video_wants();
        }
        added
    }

    /// The MSID stream id of a downstream track: the source peer
    /// id, followed by [`LIGHT_STREAM_SUFFIX`] for a light
    /// companion and by [`MIDDLE_STREAM_SUFFIX`] for a middle track.
    #[must_use]
    pub fn downstream_stream_id(source_peer: &MediaPeerId, role: TrackRole) -> String {
        match role {
            TrackRole::Main => source_peer.to_string(),
            TrackRole::LightCompanion => format!("{source_peer}{LIGHT_STREAM_SUFFIX}"),
            TrackRole::Middle => format!("{source_peer}{MIDDLE_STREAM_SUFFIX}"),
        }
    }

    /// Put the stream ids of this peer's light companions and middle
    /// tracks into an SDP str0m wrote, as the contract spells them.
    ///
    /// str0m filters a stream id down to the characters it takes
    /// for RFC 8830 token characters, and its range for the last
    /// block ends one short: `~` (0x7e), which the RFC allows, is
    /// dropped. A companion's stream id therefore leaves str0m
    /// without the `~` of [`LIGHT_STREAM_SUFFIX`], and a middle
    /// track's without that of [`MIDDLE_STREAM_SUFFIX`]. Every SDP this
    /// peer is sent (the SFU's offers and its answers alike, so
    /// the id never changes between them) passes through here,
    /// and each such id is replaced, as a whole token, by the id
    /// the contract names. Only `a=msid:`, `a=ssrc:.. msid:` and
    /// `a=msid-semantic:` lines are touched.
    fn restore_light_stream_ids(&self, sdp: String) -> String {
        // (as str0m writes it, as it is to be).
        let ids: Vec<(String, String)> = self
            .tracks_out
            .iter()
            .filter(|t| t.role != TrackRole::Main)
            .map(|t| {
                let wanted = Self::downstream_stream_id(&t.source_peer, t.role);
                (wanted.replace('~', ""), wanted)
            })
            .filter(|(written, wanted)| written != wanted)
            .collect();
        if ids.is_empty() {
            return sdp;
        }
        let mut out = String::with_capacity(sdp.len() + ids.len() * 4);
        for line in sdp.split_inclusive('\n') {
            let carries_msid = line.starts_with("a=msid")
                || (line.starts_with("a=ssrc:") && line.contains(" msid:"));
            if !carries_msid {
                out.push_str(line);
                continue;
            }
            // Split on the characters that end a token here, keeping
            // them.
            let mut token = String::new();
            let flush = |token: &mut String, out: &mut String| {
                match ids.iter().find(|(written, _)| written == token) {
                    Some((_, wanted)) => out.push_str(wanted),
                    None => out.push_str(token),
                }
                token.clear();
            };
            for c in line.chars() {
                if matches!(c, ' ' | ':' | '\r' | '\n') {
                    flush(&mut token, &mut out);
                    out.push(c);
                } else {
                    token.push(c);
                }
            }
            flush(&mut token, &mut out);
        }
        out
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
                // A light companion's stream id carries the suffix
                // that tells it from the main track.
                let mid = change.add_media(
                    entry.kind,
                    Direction::SendOnly,
                    Some(Self::downstream_stream_id(&entry.source_peer, entry.role)),
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
        Ok(Some(self.restore_light_stream_ids(offer.to_sdp_string())))
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
                    role: entry.role,
                    rid: entry.chosen_rid,
                });
            }
        }
        // A middle track that just opened may be the one the subscriber
        // asked for while it could not be had.
        self.apply_video_wants();
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

    /// `true` once this peer's transport is up (str0m reported
    /// `Event::Connected`). Packets are forwarded to a peer only
    /// from then on.
    #[must_use]
    pub const fn is_connected(&self) -> bool {
        self.connected
    }

    /// Record that str0m reported `Event::Connected` for this peer.
    pub fn mark_connected(&mut self) {
        self.connected = true;
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
        Ok(self.restore_light_stream_ids(answer.to_sdp_string()))
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

    /// Turn a packet str0m just received from THIS (publishing)
    /// peer into the form the fanout offers to the other peers.
    ///
    /// Resolves what the packet does not carry itself: the mid and
    /// simulcast layer of its stream, the media kind, and the
    /// codec parameters the publisher negotiated for its payload
    /// type. `None` when any of those is unknown, which str0m's
    /// own demultiplexing makes unreachable in practice (it emits
    /// a packet only for a stream and payload type it resolved);
    /// such a packet is dropped rather than forwarded half
    /// described.
    ///
    /// The payload is moved, never read.
    pub(crate) fn relay_packet(&mut self, packet: RtpPacket) -> Option<RelayPacket> {
        let ssrc = packet.header.ssrc;
        let (mid, rid) = {
            let mut api = self.rtc.direct_api();
            let stream = api.stream_rx(&ssrc)?;
            (stream.mid(), stream.rid())
        };
        let kind = self.rtc.media(mid)?.kind();
        if kind == MediaKind::Video {
            self.ask_faster_for_light(ssrc, mid, rid);
        }
        let params = *self
            .rtc
            .codec_config()
            .find(|p| p.pt() == packet.header.payload_type)?;
        let (params, red) = self.classify_video_red(params, &packet.payload)?;
        let ext_vals = &packet.header.ext_vals;
        let descriptor = ext_vals.user_values.get::<PacketDescriptor>().cloned();
        // An FEC packet is not part of a frame. libwebrtc builds its RTP
        // header as a copy of the last media packet's, header extensions
        // included (`ulpfec_generator.cc`, `GetFecPackets`), so it can carry
        // a frame descriptor that says "first packet of a key frame" while
        // being nothing of the kind. Its flags are not consulted.
        let starts_independent_frame = red != Some(RedBody::Fec)
            && ext_vals
                .user_values
                .get::<FrameDescriptor>()
                .and_then(|d| frame_descriptor::is_independent_frame(&d.0))
                == Some(true);
        if starts_independent_frame {
            self.settle_keyframe_requests(mid, rid);
        }
        // No descriptor in any form: a continuation packet carries one
        // too (kept as a `PacketDescriptor` only).
        let blind = kind == MediaKind::Video && red != Some(RedBody::Fec) && descriptor.is_none();
        Some(RelayPacket {
            mid,
            rid,
            kind,
            ssrc,
            seq_no: packet.seq_no,
            rtp_time: packet.header.timestamp,
            marker: packet.header.marker,
            params,
            red,
            recovered: false,
            arrival: packet.timestamp,
            video_orientation: ext_vals.video_orientation,
            descriptor,
            starts_independent_frame,
            blind,
            payload: packet.payload,
        })
    }

    /// Put the receive stream `ssrc` on the faster NACK schedule
    /// ([`LossProtection::light_nack_retry`]) if it carries THIS
    /// peer's light video: its declared light track, or the light
    /// layer of a simulcast track once that is known. Done once per
    /// stream.
    fn ask_faster_for_light(&mut self, ssrc: Ssrc, mid: Mid, rid: Option<Rid>) {
        let Some(retry) = self.protection.light_nack_retry else {
            return;
        };
        let light = self.light_mid == Some(mid)
            || rid.is_some_and(|rid| self.light_rids.get(&mid) == Some(&rid));
        if !light || self.fast_nack_ssrcs.contains(&ssrc) {
            return;
        }
        let mut api = self.rtc.direct_api();
        let Some(stream) = api.stream_rx(&ssrc) else {
            return;
        };
        stream.set_nack_retry_interval(Some(retry));
        if self.fast_nack_ssrcs.len() >= MAX_FALLBACK_TRACKS {
            self.fast_nack_ssrcs.clear();
        }
        self.fast_nack_ssrcs.push(ssrc);
    }

    /// For a packet that arrived under the video `red` payload
    /// type, find out what it carries and which codec
    /// parameters it is to be handled under; any other packet
    /// passes through with `None`.
    ///
    /// **Opacity (invariant #2, C-39).** This reads one byte,
    /// `payload[0]`: the RED header (RFC 2198), cleartext
    /// transport framing the sender puts in front of the
    /// already SFrame-sealed frame. It names the payload type
    /// of the block that follows. Nothing after it is read.
    ///
    /// - the byte names a video media payload type of this
    ///   publisher: a media packet, handled from here on under
    ///   that media codec's parameters;
    /// - it names the publisher's `ulpfec` payload type: an FEC
    ///   packet, kept under the `red` parameters;
    /// - the payload is empty (no header at all): passed on as
    ///   a bare `red` packet, which only a subscriber that
    ///   negotiated `red` is sent;
    /// - anything else (a further block announced, a payload
    ///   type the publisher did not negotiate for video): the
    ///   packet is dropped (`None`), as a libwebrtc receiver
    ///   would drop it.
    fn classify_video_red(
        &self,
        params: PayloadParams,
        payload: &[u8],
    ) -> Option<(PayloadParams, Option<RedBody>)> {
        if params.spec().codec != Codec::RedVideo {
            return Some((params, None));
        }
        let block_pt = match video_fec::red_block_pt(payload) {
            Ok(pt) => pt,
            Err(video_fec::RedError::Empty) => return Some((params, None)),
            Err(video_fec::RedError::MultipleBlocks) => return None,
        };
        let inner = *self.rtc.codec_config().find(|p| *p.pt() == block_pt)?;
        match inner.spec().codec {
            Codec::Ulpfec => Some((params, Some(RedBody::Fec))),
            Codec::RedVideo => None,
            codec if codec.is_video() => Some((inner, Some(RedBody::Media))),
            _ => None,
        }
    }

    /// The packet behind the `Event::RtpPacket` str0m just handed
    /// out, whole, as the publisher sent it. `None` unless video
    /// FEC is on, and for a packet that came as a retransmission.
    /// Must be called for every such event, before the next input
    /// to this peer, so a packet is never paired with another's
    /// bytes.
    pub(crate) fn take_received(&mut self) -> Option<Vec<u8>> {
        if !self.video_fec {
            return None;
        }
        self.rtc.direct_api().take_received_rtp()
    }

    /// The ids, on the media line `mid`, of the header extensions
    /// a libwebrtc sender fills in after it computed FEC.
    fn mutable_extensions(&self, mid: Mid) -> MutableExtensions {
        use str0m::rtp::Extension;
        let Some(media) = self.rtc.media(mid) else {
            return MutableExtensions::default();
        };
        let map = media.remote_extmap();
        MutableExtensions {
            abs_send_time: map.id_of(Extension::AbsoluteSendTime),
            transport_cc: map.id_of(Extension::TransportSequenceNumber),
            transmission_offset: map.id_of(Extension::TransmissionTimeOffset),
            video_timing: map.id_of(Extension::VideoTiming),
        }
    }

    /// Give one RED-wrapped video packet of THIS (publishing) peer
    /// to the stream's FEC receiver and return the media packets
    /// it could rebuild, ready for the fanout.
    ///
    /// `packet` is what [`Self::relay_packet`] made of the packet
    /// and `received` what [`Self::take_received`] returned for
    /// it. A media packet is taken only with its received bytes: a
    /// retransmitted one has none, and is left out as libwebrtc's
    /// receiver leaves it out (its extension block is the RTX
    /// packet's). An FEC packet is taken from its payload.
    ///
    /// A rebuilt packet is registered with str0m as received, so
    /// the publisher is no longer asked for it and it is not
    /// reported lost, and is then described exactly as
    /// [`Self::relay_packet`] describes a received RED media
    /// packet.
    ///
    /// **Opacity (invariant #2, C-39).** The receiver XORs packets
    /// together; a media payload is `SFrame` ciphertext going in and
    /// coming out and is never interpreted. What is read here is
    /// the RED header byte, already classified by
    /// [`Self::classify_video_red`].
    pub(crate) fn fec_receive(
        &mut self,
        packet: &RelayPacket,
        received: Option<Vec<u8>>,
        now: Instant,
    ) -> Vec<RelayPacket> {
        use crate::prom::names::{VIDEO_FEC_PACKETS_TOTAL, VIDEO_FEC_RECOVERED_TOTAL};
        let Some(red) = packet.red else {
            return Vec::new();
        };
        if !self.video_fec || packet.kind != MediaKind::Video {
            return Vec::new();
        }
        // The low 16 bits are the number on the wire.
        let seq = u16::try_from(*packet.seq_no & 0xffff).unwrap_or(0);
        let view = match red {
            RedBody::Media => {
                let Some(received) = received else {
                    return Vec::new();
                };
                let mutable = self.mutable_extensions(packet.mid);
                let Some(view) = fec_receive::media_from_red(&received, &mutable) else {
                    return Vec::new();
                };
                Some(view)
            },
            RedBody::Fec => None,
        };
        if !self.fec_receivers.contains_key(&packet.ssrc)
            && self.fec_receivers.len() >= MAX_FEC_STREAMS
        {
            self.fec_receivers.clear();
        }
        let stream = self
            .fec_receivers
            .entry(packet.ssrc)
            .or_insert_with(|| FecReceiver::new(*packet.ssrc));
        let rebuilt = if let Some(view) = view {
            stream.insert_media(seq, view)
        } else {
            metrics::counter!(VIDEO_FEC_PACKETS_TOTAL).increment(1);
            let Some(fec) = video_fec::red_body(&packet.payload) else {
                return Vec::new();
            };
            stream.insert_fec(seq, fec).unwrap_or_default()
        };
        let mut out = Vec::new();
        for recovered in rebuilt {
            let Some(packet) = self
                .rtc
                .direct_api()
                .handle_recovered_rtp(now, &recovered.packet)
            else {
                // Already received (a retransmission got here first), or
                // not a packet of this stream after all.
                continue;
            };
            if let Some(packet) = self.relay_recovered(packet) {
                metrics::counter!(VIDEO_FEC_RECOVERED_TOTAL).increment(1);
                out.push(packet);
            }
        }
        out
    }

    /// Describe a packet rebuilt from FEC for the fanout: as
    /// [`Self::relay_packet`] describes it, then put back into the
    /// RED-wrapped form a received media packet of this stream
    /// has, so every subscriber gets it the way it gets the
    /// others. `None` when what was rebuilt is not a video media
    /// packet of this publisher (its payload type is not one the
    /// publisher negotiated for video media): then the inputs were
    /// not what the FEC was computed over, and it is dropped.
    fn relay_recovered(&mut self, packet: RtpPacket) -> Option<RelayPacket> {
        let media_pt = *packet.header.payload_type;
        let mut packet = self.relay_packet(packet)?;
        let codec = packet.params.spec().codec;
        if packet.kind != MediaKind::Video
            || packet.red.is_some()
            || !codec.is_video()
            || matches!(codec, Codec::RedVideo | Codec::Ulpfec)
        {
            return None;
        }
        packet.payload.insert(0, media_pt);
        packet.red = Some(RedBody::Media);
        packet.recovered = true;
        Some(packet)
    }

    /// Offer one packet from a *different* peer to this
    /// (subscribing) peer. Returns the [`MediaForward`] outcome so
    /// the caller can keep a counter per skip reason.
    ///
    /// The SFrame contract holds: `packet.payload` is the RTP
    /// payload as the publisher sent it, SFrame ciphertext, and
    /// it is written to this peer's send stream without being
    /// read. str0m encrypts at the SRTP layer for the hop to this
    /// peer; the inner ciphertext stays opaque from origin to
    /// destination (audit-pack C-39).
    ///
    /// ## What is decided here, in order
    ///
    /// 1. **Which track.** `packet.mid` is the **source** peer's
    ///    mid. The mapping to this peer's own mid lives in
    ///    [`tracks_out`](Self::tracks_out): the entry whose
    ///    `(source_peer, source_mid)` matches and that is
    ///    `Open(local_mid)`. Until the renegotiation has opened
    ///    it the packet is dropped as `NoWriter`, the expected
    ///    pre-negotiation transient.
    /// 2. **Which simulcast layer** (`layer_gate_allows`): only
    ///    the track's current layer passes, and a switch takes
    ///    effect at a packet that begins an independent frame on
    ///    the selected layer, once that frame is whole at the SFU:
    ///    until then its packets are held and the old layer keeps
    ///    being forwarded ([`SwitchWait`]). Anything else is
    ///    `WrongLayer`, a held packet included.
    /// 3. **Whether the peer can be sent to at all**: its
    ///    transport must be connected, else `NoWriter`.
    /// 4. **Which payload type**: the publisher's codec
    ///    parameters matched against this peer's negotiation;
    ///    no match is `NoCodecMatch`. A RED packet (redundant
    ///    audio, RFC 2198) goes only to a peer whose track
    ///    negotiated `red` over the same Opus payload type
    ///    ([`red_negotiated`]); the SFU cannot unwrap it for
    ///    anyone else, so for them it is `NoCodecMatch` too.
    ///    Video RED is the opposite case ([`crate::video_fec`]):
    ///    its one header byte is outside the sealed frame, so a
    ///    RED-wrapped media packet goes to a peer whose track
    ///    negotiated `red` and `ulpfec` RED-wrapped (outer
    ///    payload type = the peer's `red`, header byte = the
    ///    peer's media payload type) and to any other peer
    ///    unwrapped, under its media payload type. The media
    ///    body is copied, never read. A publisher's FEC packet
    ///    is sent to nobody; the fanout does not offer one, and
    ///    one offered anyway is `NoCodecMatch`.
    /// 5. **Which sequence number and timestamp**
    ///    ([`StreamMap`]): the publisher's plus the track's fixed
    ///    offset, re-based when the source stream changes. A
    ///    packet with no slot downstream is `Stale`.
    ///
    /// The packet is then written with the marker bit kept, the
    /// publisher's frame descriptor and video orientation
    /// (never an audio level), `nackable` for video (so str0m keeps it for a
    /// subscriber's retransmission request) and not for audio,
    /// and its arrival time as the wall clock str0m builds sender
    /// reports from (a late packet gets the matching earlier
    /// moment instead, see [`crate::stream_map`]).
    ///
    /// This is the **main** track of the source track. A simulcast
    /// video track has a second downstream track, its light
    /// companion, which [`Self::forward_light`] feeds.
    pub(crate) fn forward_packet(
        &mut self,
        source_peer: &MediaPeerId,
        packet: &RelayPacket,
    ) -> MediaForward {
        let Some(idx) = self.open_track(source_peer, packet.mid, TrackRole::Main) else {
            return MediaForward::NoWriter;
        };
        if !self.gate_admits(idx, packet) {
            return MediaForward::NotWanted;
        }
        self.forward_at_pending_switch(idx, packet)
            .unwrap_or_else(|| self.forward_on_wire(idx, packet))
    }

    /// Offer one packet of a publisher's **declared middle track**
    /// ([`TrackIn::middle`]) to this (subscribing) peer: it goes to
    /// the middle track that mirrors it, through exactly the steps of
    /// [`Self::forward_packet`] on that track's own stream, when the
    /// subscriber shows the middle picture of this publisher
    /// ([`Self::set_video_wants`]) and [`MediaForward::NotWanted`]
    /// otherwise. No layer is selected: the source is one ordinary
    /// encoding.
    ///
    /// [`MediaForward::NoWriter`] while this peer has no open middle
    /// track for it.
    pub(crate) fn forward_middle_track(
        &mut self,
        source_peer: &MediaPeerId,
        packet: &RelayPacket,
    ) -> MediaForward {
        let Some(idx) = self.open_track(source_peer, packet.mid, TrackRole::Middle) else {
            return MediaForward::NoWriter;
        };
        if !self.gate_admits(idx, packet) {
            return MediaForward::NotWanted;
        }
        self.forward_at_pending_switch(idx, packet)
            .unwrap_or_else(|| self.forward_on_wire(idx, packet))
    }

    /// Whether the downstream track at `idx` takes `packet` now
    /// ([`Gate`]). A resuming track starts here: at the first packet
    /// of a key frame, or at any packet once the wait was given up.
    /// Header fields only.
    fn gate_admits(&mut self, idx: usize, packet: &RelayPacket) -> bool {
        let Some(entry) = self.tracks_out.get_mut(idx) else {
            return false;
        };
        let fw = &mut entry.forwarding;
        let any_packet = match fw.gate {
            Gate::Flowing => return true,
            Gate::Paused => return false,
            Gate::Resuming { any_packet, .. } => any_packet,
        };
        if packet.is_fec() {
            return false;
        }
        match (fw.active_rid, packet.rid) {
            // A simulcast source with a layer on the wire: only that
            // layer can continue the stream.
            (Some(active), rid) if rid != Some(active) => return false,
            // A simulcast source that never began: its own start rule
            // ([`Forwarding::start_at_key_frame`]) decides.
            (None, Some(_)) => {
                fw.gate = Gate::Flowing;
                return true;
            },
            _ => {},
        }
        // A packet with no frame descriptor (Firefox negotiates none)
        // can never show where a key frame begins: waiting for one would
        // only hold the track back for the five requests the wait is
        // given, and the subscriber asks for itself when it cannot
        // decode.
        if !(any_packet || packet.blind || packet.starts_independent_frame) {
            return false;
        }
        fw.gate = Gate::Flowing;
        fw.rebase_next = fw.map.is_some();
        true
    }

    /// Take the whole set of pictures THIS (subscribing) peer shows
    /// (`ClientFrame::VideoWants`, ADR-0070 step 5): for each
    /// publisher named, which of its video tracks. The set replaces
    /// the one before; a publisher it does not name is back at
    /// [`VideoQuality::Main`], and a peer named twice has its last
    /// entry. A name that is no publisher of this peer's call changes
    /// nothing now and applies if that peer's tracks appear later.
    /// More than [`MAX_VIDEO_WANTS`] entries (the protocol refuses
    /// such a frame) are cut at the bound.
    ///
    /// Takes effect at once: a track no longer shown is paused with
    /// the next packet, and a track now shown resumes at a key frame
    /// ([`Gate`]); its publisher is asked by
    /// [`drive_video_wants`]. Returns how many publishers are now at
    /// main (by default or by name among those named), middle and
    /// light, counting the names given.
    pub fn set_video_wants(&mut self, wants: &[VideoWant]) -> VideoWantsCount {
        self.video_wants.clear();
        for want in wants.iter().take(MAX_VIDEO_WANTS) {
            self.video_wants
                .insert(MediaPeerId::from_string(want.peer.clone()), want.quality);
        }
        if let Some(downlink) = self.downlink.as_mut() {
            // The order of the list is the subscriber's order of
            // importance (ADR-0070 step 6), and it is told what it is
            // given after every set, changed or not.
            downlink.importance.clear();
            for want in wants.iter().take(MAX_VIDEO_WANTS) {
                let id = MediaPeerId::from_string(want.peer.clone());
                if !downlink.importance.contains(&id) {
                    downlink.importance.push(id);
                }
            }
            downlink.tell = true;
            // The allocation runs at the next pass, whenever it last ran.
            downlink.last_run = None;
        }
        self.apply_video_wants();
        let mut count = VideoWantsCount::default();
        for quality in self.video_wants.values() {
            match quality {
                VideoQuality::Main => count.main += 1,
                VideoQuality::Middle => count.middle += 1,
                VideoQuality::Light => count.light += 1,
            }
        }
        count
    }

    /// The picture of `publisher` this peer is being sent: what it
    /// asked for ([`Self::set_video_wants`]), [`VideoQuality::Main`]
    /// when it asked for nothing, and `Main` too when it asked for
    /// the middle picture of a publisher that has no middle track
    /// open on this peer.
    #[must_use]
    pub fn video_quality_for(&self, publisher: &MediaPeerId) -> VideoQuality {
        match self.video_wants.get(publisher).copied().unwrap_or_default() {
            VideoQuality::Middle if !self.has_open_middle(publisher) => VideoQuality::Main,
            quality => quality,
        }
    }

    /// The picture of `publisher` this peer is being **sent**: what it
    /// asked for ([`Self::video_quality_for`]), or, under downlink
    /// control (ADR-0070 step 6), the smaller one its allocation gives it
    /// ([`Self::run_downlink`]). Never above what it asked for, and
    /// never the middle picture of a publisher that has no middle track
    /// open on this peer (the light track alone, then).
    #[must_use]
    pub fn video_quality_given(&self, publisher: &MediaPeerId) -> VideoQuality {
        let wanted = self.video_quality_for(publisher);
        let Some(given) = self
            .downlink
            .as_ref()
            .and_then(|downlink| downlink.given.get(publisher))
        else {
            return wanted;
        };
        match downlink_allocation::smaller(*given, wanted) {
            VideoQuality::Middle if !self.has_open_middle(publisher) => VideoQuality::Light,
            quality => quality,
        }
    }

    /// Whether this peer has an open middle track of `publisher`.
    fn has_open_middle(&self, publisher: &MediaPeerId) -> bool {
        self.tracks_out.iter().any(|t| {
            t.source_peer == *publisher
                && t.role == TrackRole::Middle
                && matches!(t.state, TrackOutState::Open(_))
        })
    }

    /// Bring every downstream video track's [`Gate`] in line with what
    /// this peer is sent ([`Self::video_quality_given`]): the main track
    /// of a publisher flows only at `Main`, its middle track only at
    /// `Middle`, its light companion always. A track that is to stop
    /// is paused here and now; one that is to start again begins to
    /// wait for a key frame. Called whenever the wants, the set of
    /// tracks or their state changes; does nothing when all agree.
    fn apply_video_wants(&mut self) {
        let flows: Vec<Option<bool>> = self
            .tracks_out
            .iter()
            .map(|t| {
                if t.kind != MediaKind::Video {
                    return None;
                }
                let quality = self.video_quality_given(&t.source_peer);
                match t.role {
                    TrackRole::LightCompanion => None,
                    TrackRole::Main => Some(quality == VideoQuality::Main),
                    TrackRole::Middle => Some(quality == VideoQuality::Middle),
                }
            })
            .collect();
        let downlink_control = self.downlink.is_some();
        let mut forget: Vec<Mid> = Vec::new();
        for (entry, flows) in self.tracks_out.iter_mut().zip(flows) {
            let Some(flows) = flows else {
                continue;
            };
            let fw = &mut entry.forwarding;
            match (flows, fw.gate) {
                (true, Gate::Paused) => {
                    fw.gate = Gate::Resuming {
                        asked: None,
                        requests: 0,
                        any_packet: false,
                    };
                },
                (false, Gate::Flowing | Gate::Resuming { .. }) => {
                    fw.gate = Gate::Paused;
                    // Nothing is held for a track nobody shows.
                    fw.waiting.clear();
                    if let (true, TrackOutState::Open(local_mid)) = (downlink_control, entry.state)
                    {
                        forget.push(local_mid);
                    }
                },
                _ => {},
            }
        }
        // Under downlink control a track is most often paused because the
        // subscriber's path is short, and the subscriber is then missing
        // packets of it and asking for them again. Answering would keep
        // sending the track that was just given up, on the path that does
        // not carry it: the packets kept for retransmission are dropped,
        // so str0m finds nothing to resend. The track resumes at a key
        // frame, which needs none of them.
        for local_mid in forget {
            if let Some(stream) = self.rtc.direct_api().stream_tx_by_mid(local_mid, None) {
                stream.set_rtx_cache(PAUSED_RTX_CACHE_PACKETS, PAUSED_RTX_CACHE_AGE, None);
            }
        }
    }

    /// The downstream video tracks of this (subscribing) peer that are
    /// open, were paused and are wanted again ([`Gate::Resuming`]),
    /// and whose publisher is due to be asked for a key frame (never
    /// asked, or [`START_KEYFRAME_RETRY`] ago). Each is marked as
    /// asked at `now`. A track that has been asked
    /// [`START_KEYFRAME_REQUESTS`] times stops waiting instead and
    /// starts at the next packet.
    ///
    /// A track whose source is in `idle` (publisher and publisher mid of
    /// a sender that is switched off on the SFU's word,
    /// [`crate::video_needed`]) is not asked for and uses up none of its
    /// requests: no packet can arrive until the publisher has switched
    /// the sender back on, and giving the wait up meanwhile would start
    /// the track at whatever packet comes first. It is asked for at once
    /// when the sender is back.
    pub(crate) fn due_resume_keyframes(
        &mut self,
        now: Instant,
        idle: &[(MediaPeerId, Mid)],
    ) -> Vec<OpenedTrack> {
        let mut due = Vec::new();
        for entry in &mut self.tracks_out {
            if !matches!(entry.state, TrackOutState::Open(_)) {
                continue;
            }
            if idle
                .iter()
                .any(|(peer, mid)| *peer == entry.source_peer && *mid == entry.source_mid)
            {
                if let Gate::Resuming {
                    asked, requests, ..
                } = &mut entry.forwarding.gate
                {
                    *asked = None;
                    *requests = 0;
                }
                continue;
            }
            let rid = entry.forwarding.active_rid.or(entry.chosen_rid);
            let Gate::Resuming {
                asked,
                requests,
                any_packet,
            } = &mut entry.forwarding.gate
            else {
                continue;
            };
            if *any_packet {
                continue;
            }
            let waited = asked.map_or(true, |at| {
                now.saturating_duration_since(at) >= START_KEYFRAME_RETRY
            });
            if !waited {
                continue;
            }
            if *requests >= START_KEYFRAME_REQUESTS {
                *any_packet = true;
                continue;
            }
            *asked = Some(now);
            *requests += 1;
            due.push(OpenedTrack {
                source_peer: entry.source_peer.clone(),
                source_mid: entry.source_mid,
                kind: entry.kind,
                role: entry.role,
                rid,
            });
        }
        due
    }

    /// Whether the downstream track of `role` mirroring
    /// `(source_peer, source_mid)` is paused or waiting to resume
    /// ([`Gate`]): the subscriber is being sent nothing on it.
    /// `false` when there is no such track.
    #[must_use]
    pub fn track_paused(
        &self,
        source_peer: &MediaPeerId,
        source_mid: Mid,
        role: TrackRole,
    ) -> bool {
        self.tracks_out.iter().any(|t| {
            t.source_peer == *source_peer
                && t.source_mid == source_mid
                && t.role == role
                && t.forwarding.gate != Gate::Flowing
        })
    }

    /// Whether the downstream track open on this peer's own mid
    /// `local_mid` is paused outright ([`Gate::Paused`]): a keyframe
    /// request the subscriber sends on it helps nobody.
    fn downstream_paused(&self, local_mid: Mid) -> bool {
        self.tracks_out
            .iter()
            .any(|t| t.state == TrackOutState::Open(local_mid) && t.forwarding.gate == Gate::Paused)
    }

    /// Offer one packet from a *different* peer to this
    /// (subscribing) peer's **light companion** of the packet's
    /// track ([`TrackRole::LightCompanion`]).
    ///
    /// `None`, and nothing counted, when the packet is not the
    /// companion's business: this peer has no open companion for
    /// the track, the companion's layer is not known yet
    /// ([`Self::apply_simulcast_layout`] sets it), or the packet
    /// is of any other layer. A packet of the companion's layer
    /// goes through exactly the steps of [`Self::forward_packet`]
    /// from the third on, on the companion's own stream: its own
    /// sequence-number map, its own send stream and its own FEC
    /// state. The companion begins at a whole key frame of its
    /// layer and never switches.
    pub(crate) fn forward_light(
        &mut self,
        source_peer: &MediaPeerId,
        packet: &RelayPacket,
    ) -> Option<MediaForward> {
        let idx = self.open_track(source_peer, packet.mid, TrackRole::LightCompanion)?;
        let light = self.tracks_out[idx].chosen_rid?;
        if packet.rid != Some(light) {
            return None;
        }
        Some(
            self.forward_at_pending_switch(idx, packet)
                .unwrap_or_else(|| self.forward_on_wire(idx, packet)),
        )
    }

    /// Offer one packet of a publisher's **declared light track**
    /// ([`TrackIn::light`]) to this (subscribing) peer: it goes to
    /// the light companion that mirrors the track, every packet of
    /// it, through exactly the steps of [`Self::forward_packet`] on
    /// the companion's own stream (its own sequence-number map,
    /// send stream, retransmission buffer and FEC state). No layer
    /// is selected and nothing waits: the source is one ordinary
    /// encoding.
    ///
    /// [`MediaForward::NoWriter`] while this peer has no open
    /// companion for the track.
    pub(crate) fn forward_light_track(
        &mut self,
        source_peer: &MediaPeerId,
        packet: &RelayPacket,
    ) -> MediaForward {
        let Some(idx) = self.open_track(source_peer, packet.mid, TrackRole::LightCompanion) else {
            return MediaForward::NoWriter;
        };
        self.forward_at_pending_switch(idx, packet)
            .unwrap_or_else(|| self.forward_on_wire(idx, packet))
    }

    /// Index in `tracks_out` of the open downstream track of the
    /// given role that mirrors `(source_peer, source_mid)`.
    fn open_track(
        &self,
        source_peer: &MediaPeerId,
        source_mid: Mid,
        role: TrackRole,
    ) -> Option<usize> {
        self.tracks_out.iter().position(|entry| {
            entry.source_peer == *source_peer
                && entry.source_mid == source_mid
                && entry.role == role
                && matches!(entry.state, TrackOutState::Open(_))
        })
    }

    /// Tell this (subscribing) peer's downstream tracks of the
    /// simulcast video track `(source_peer, source_mid)` which
    /// layer each is to carry, where nothing has decided yet:
    ///
    /// - the **light companion** is put on the light layer, once,
    ///   for good;
    /// - the **main** track, if no layer has been selected for it
    ///   and nothing is on the wire, is put on the heaviest layer
    ///   instead of being left to latch whichever layer arrives
    ///   first. A main track that has already latched is moved by
    ///   [`drive_uplink_fallback`]'s default-layer rule.
    ///
    /// A track given its layer here begins at a whole key frame of
    /// that layer ([`Forwarding::start_at_key_frame`]). Called for
    /// every simulcast video packet before it is forwarded, and
    /// once per pass by [`drive_two_track_delivery`], so it does
    /// nothing in the common case.
    pub(crate) fn apply_simulcast_layout(
        &mut self,
        source_peer: &MediaPeerId,
        source_mid: Mid,
        layout: SimulcastLayout,
    ) {
        for entry in &mut self.tracks_out {
            if entry.source_peer != *source_peer
                || entry.source_mid != source_mid
                || entry.kind != MediaKind::Video
                || entry.chosen_rid.is_some()
                || entry.forwarding.active_rid.is_some()
            {
                continue;
            }
            let target = match entry.role {
                TrackRole::LightCompanion => layout.light,
                TrackRole::Main => layout.heaviest,
                // A middle track mirrors one ordinary encoding.
                TrackRole::Middle => None,
            };
            if let Some(rid) = target {
                entry.chosen_rid = Some(rid);
                entry.forwarding.start_at_key_frame = true;
            }
        }
    }

    /// The downstream video tracks of this (subscribing) peer that
    /// are open, were given a layer by the SFU and are still
    /// waiting for a whole key frame to begin at, and whose
    /// publisher is due to be asked for one (never asked, or
    /// [`START_KEYFRAME_RETRY`] ago). Each is marked as asked at
    /// `now`. A track that has been asked
    /// [`START_KEYFRAME_REQUESTS`] times stops waiting instead and
    /// begins at the next packet of its layer.
    pub(crate) fn due_start_keyframes(&mut self, now: Instant) -> Vec<OpenedTrack> {
        let mut due = Vec::new();
        for entry in &mut self.tracks_out {
            let fw = &mut entry.forwarding;
            if !fw.start_at_key_frame
                || fw.active_rid.is_some()
                || entry.chosen_rid.is_none()
                || !matches!(entry.state, TrackOutState::Open(_))
                // A track the subscriber does not show needs no key frame.
                || fw.gate == Gate::Paused
            {
                continue;
            }
            let waited = fw.start_requested.map_or(true, |at| {
                now.saturating_duration_since(at) >= START_KEYFRAME_RETRY
            });
            if !waited {
                continue;
            }
            if fw.start_requests >= START_KEYFRAME_REQUESTS {
                fw.start_at_key_frame = false;
                fw.waiting.clear();
                continue;
            }
            fw.start_requested = Some(now);
            fw.start_requests += 1;
            due.push(OpenedTrack {
                source_peer: entry.source_peer.clone(),
                source_mid: entry.source_mid,
                kind: entry.kind,
                role: entry.role,
                rid: entry.chosen_rid,
            });
        }
        due
    }

    /// Whether the open downstream track of `role` mirroring
    /// `(source_peer, source_mid)` is waiting for a whole key frame
    /// to begin at.
    #[must_use]
    pub fn waits_to_start(
        &self,
        source_peer: &MediaPeerId,
        source_mid: Mid,
        role: TrackRole,
    ) -> bool {
        self.open_track(source_peer, source_mid, role)
            .is_some_and(|idx| {
                let entry = &self.tracks_out[idx];
                entry.forwarding.start_at_key_frame && entry.forwarding.active_rid.is_none()
            })
    }

    /// [`Self::forward_packet`] for a packet that is not waiting for a
    /// layer switch: every step of it but the holding of step 2, on the
    /// downstream track at `idx`.
    #[allow(clippy::too_many_lines)]
    fn forward_on_wire(&mut self, idx: usize, packet: &RelayPacket) -> MediaForward {
        // Mutable: the layer gate latches the selected layer and the map
        // advances in place.
        let Some(entry) = self.tracks_out.get_mut(idx) else {
            return MediaForward::NoWriter;
        };
        if packet.is_fec() {
            // A publisher's FEC packet ends at the SFU. It never selects a
            // layer or starts a downstream stream either.
            return MediaForward::NoCodecMatch;
        }
        if !layer_gate_allows(
            &mut entry.chosen_rid,
            &mut entry.forwarding.active_rid,
            packet.rid,
            packet.starts_independent_frame,
        ) {
            return MediaForward::WrongLayer;
        }
        let TrackOutState::Open(local_mid) = entry.state else {
            // Unreachable — the callers pass the index of an Open entry.
            return MediaForward::NoWriter;
        };
        if !self.connected {
            return MediaForward::NoWriter;
        }
        // The peer's `red` and `ulpfec` payload types for this track, when
        // the track negotiated the pair.
        let fec_pts = video_fec_negotiated(&self.rtc, local_mid);
        let Some(matched) = self.rtc.codec_config().match_params(packet.params).copied() else {
            return MediaForward::NoCodecMatch;
        };
        if packet.params.spec().codec == Codec::Red
            && !red_negotiated(&self.rtc, local_mid, &matched, &packet.params)
        {
            return MediaForward::NoCodecMatch;
        }
        let (pt, payload) = match packet.red {
            // Not RED-wrapped: the payload goes out as it came. A bare
            // (empty) video `red` packet has no media payload type to fall
            // back on, so it needs the pair.
            None => {
                if packet.params.spec().codec == Codec::RedVideo && fec_pts.is_none() {
                    return MediaForward::NoCodecMatch;
                }
                (matched.pt(), packet.payload.clone())
            },
            // `matched` is the peer's entry for the media codec inside.
            Some(RedBody::Media) => {
                if let Some((red_pt, _)) = fec_pts {
                    let Some(payload) = video_fec::red_rewrap(&packet.payload, *matched.pt())
                    else {
                        return MediaForward::NoCodecMatch;
                    };
                    (red_pt, payload)
                } else {
                    let Some(body) = video_fec::red_body(&packet.payload) else {
                        return MediaForward::NoCodecMatch;
                    };
                    (matched.pt(), body.to_vec())
                }
            },
            // Returned above.
            Some(RedBody::Fec) => return MediaForward::NoCodecMatch,
        };
        let Some(position) = entry.forwarding.position(packet) else {
            return MediaForward::Stale;
        };
        let mut ext_vals = packet.egress_extensions();
        if packet.kind == MediaKind::Video
            && self.protection.playout_delay
            && playout_delay_negotiated(&self.rtc, local_mid)
        {
            // ADR-0070 step 4: every video packet of this publisher to
            // this subscriber, main track and companion alike, carries the
            // pair's minimum playout delay; zero until a burst was seen.
            let min_ms = self
                .playout
                .get(&entry.source_peer)
                .map_or(0, |pair| pair.stamped_ms);
            stamp_playout_delay(&mut ext_vals, min_ms);
        }
        let mut api = self.rtc.direct_api();
        let Some(stream) = api.stream_tx_by_mid(local_mid, None) else {
            return MediaForward::NoWriter;
        };
        if let (Some((red_pt, ulpfec_pt)), Some(RedBody::Media)) = (fec_pts, packet.red) {
            // A RED-wrapped media packet to a subscriber with the pair: the
            // SFU will protect it with FEC of its own
            // ([`Self::offer_publisher_fec`]). That is computed over the
            // packet as str0m serializes it for this leg, so str0m keeps
            // those bytes for [`Self::send_leg_fec`].
            stream.keep_sent_rtp(true);
            let ssrc = stream.ssrc();
            if !self.fec_senders.contains_key(&local_mid) && self.fec_senders.len() >= MAX_FEC_LEGS
            {
                self.fec_senders.clear();
            }
            let leg = self.fec_senders.entry(local_mid).or_insert_with(|| LegFec {
                ssrc,
                red_pt,
                ulpfec_pt,
                source: (entry.source_peer.clone(), entry.source_mid, entry.role),
                sender: FecSender::new(),
            });
            if leg.ssrc != ssrc {
                // The stream was reset: what was sent under the old SSRC
                // protects nothing on the new one.
                leg.ssrc = ssrc;
                leg.sender = FecSender::new();
            }
            leg.red_pt = red_pt;
            leg.ulpfec_pt = ulpfec_pt;
        }
        // Audio has no retransmission stream; video does, and a packet
        // marked nackable is what str0m answers a subscriber's NACK from.
        let nackable = packet.kind == MediaKind::Video;
        let written = stream.write_rtp(
            pt,
            SeqNo::from(position.seq_no),
            position.rtp_time,
            position.wallclock,
            packet.marker,
            ext_vals,
            nackable,
            payload,
        );
        if let Err(e) = written {
            return MediaForward::WriteFailed(e.to_string());
        }
        let second_copy =
            nackable && entry.role == TrackRole::LightCompanion && self.protection.light_duplicate;
        if let Some(downlink) = self.downlink.as_mut() {
            let len = packet.payload.len();
            downlink
                .sent
                .record(len + RTP_FIXED_HEADER_LEN, packet.arrival);
            // A running probe counts what is sent beside its padding; a
            // light packet goes out twice.
            downlink.probe.on_sent(len, false, packet.arrival);
            downlink.note_written(len + RTP_FIXED_HEADER_LEN, packet.arrival);
            downlink.give_probe_up_on_a_clump(packet.arrival);
            if second_copy {
                downlink.probe.on_sent(len, false, packet.arrival);
            }
            if nackable {
                if downlink.recent.len() >= DOWNLINK_RECENT_PACKETS {
                    downlink.recent.pop_front();
                }
                downlink.recent.push_back((local_mid, position.seq_no, len));
            }
        }
        if nackable {
            self.video_sent = self.video_sent.saturating_add(1);
            if second_copy {
                // ADR-0070 step 3: a second copy a little later. A full
                // queue drops it; the packet itself has been sent.
                let _ = self.light_duplicates.push(
                    packet.arrival + LIGHT_DUPLICATE_DELAY,
                    local_mid,
                    position.seq_no,
                );
            }
        }
        MediaForward::Sent
    }

    /// Send the second copies of light companion packets that are due at
    /// `now` ([`LIGHT_DUPLICATE_DELAY`] after the packet reached the SFU)
    /// to this (subscribing) peer. Each goes out as a retransmission, on
    /// the track's RTX stream, exactly as a packet the subscriber asked
    /// for would, so a subscriber that has the first copy discards the
    /// second by its sequence number. A track without an RTX payload type
    /// gets none (str0m keeps no packet of it), and neither does a packet
    /// that has left the retransmission cache. Returns how many were
    /// queued for sending.
    pub(crate) fn send_light_duplicates(&mut self, now: Instant) -> u64 {
        let mut sent = 0u64;
        while let Some((local_mid, seq_no)) = self.light_duplicates.pop_due(now) {
            let source = self.tracks_out.iter().find_map(|entry| {
                (entry.state == TrackOutState::Open(local_mid))
                    .then(|| (entry.source_peer.clone(), entry.source_mid, entry.role))
            });
            let Some(source) = source else {
                continue;
            };
            let mut api = self.rtc.direct_api();
            let Some(stream) = api.stream_tx_by_mid(local_mid, None) else {
                continue;
            };
            if stream.resend(SeqNo::from(seq_no), now) {
                sent += 1;
                self.duplicates_sent.push(source);
            }
        }
        sent
    }

    /// When the next second copy for this peer is due.
    pub(crate) fn next_light_duplicate(&self) -> Option<Instant> {
        self.light_duplicates.next_due()
    }

    /// The second copies sent to this peer since the last call: the
    /// publisher, publisher mid and downstream role of each.
    pub(crate) fn take_duplicates_sent(&mut self) -> Vec<(MediaPeerId, Mid, TrackRole)> {
        std::mem::take(&mut self.duplicates_sent)
    }

    /// Hand one video packet THIS peer published to its uplink burst
    /// detector ([`crate::playout_delay::UplinkBursts`]): stream,
    /// extended sequence number, arrival. Header fields only. A no-op
    /// with the playout delay switched off, and for audio.
    ///
    /// An FEC packet is handed over like any other: under RED it takes
    /// a sequence number of the media stream it protects, and a
    /// detector that never saw it would hold that number missing for
    /// ever (it did until 2026-10-09: the uplink of every publisher
    /// that sends FEC read as in a burst all the time).
    pub(crate) fn observe_uplink_burst(&mut self, packet: &RelayPacket) {
        if !self.protection.playout_delay || packet.kind != MediaKind::Video {
            return;
        }
        self.uplink_bursts
            .on_packet(*packet.ssrc, *packet.seq_no, packet.arrival);
    }

    /// Hand one video packet THIS peer published to the bookkeeping
    /// that says when a burst on its stream has ended unrepaired
    /// ([`crate::burst_keyframe`]): stream, extended sequence number,
    /// arrival, whether its frame descriptor says it begins a key
    /// frame, and where in its frame the descriptor says it sits (the
    /// begin and end flags and, on a first packet, the frame id:
    /// [`frame_descriptor::packet_position`]). Header fields only. Every
    /// packet that takes a sequence number of the stream counts:
    /// retransmissions, FEC packets and packets rebuilt from FEC. A
    /// no-op for audio and with the feature switched off.
    ///
    /// An FEC packet's own descriptor is not read (it is a copy of the
    /// last media packet's). Its mask is, but only while packets are
    /// missing on its stream: the packets it names as protected are
    /// media ([`BurstEnds::fec_protects`]). The mask is the FEC header's;
    /// nothing of the protected bytes is read or copied (C-39).
    pub(crate) fn observe_burst_end(&mut self, packet: &RelayPacket) {
        if !self.protection.burst_keyframe || packet.kind != MediaKind::Video {
            return;
        }
        let stream = (packet.mid, packet.rid);
        let fec = packet.is_fec();
        let (what, part) = if fec {
            (Arrived::Fec, None)
        } else {
            let part = packet
                .descriptor
                .as_ref()
                .and_then(|d| frame_descriptor::packet_position(&d.0));
            if packet.starts_independent_frame {
                (Arrived::KeyFrameStart, part)
            } else {
                (Arrived::Media, part)
            }
        };
        self.burst_ends
            .on_described(stream, *packet.seq_no, packet.arrival, what, part);
        if fec && self.burst_ends.has_holes(stream) {
            if let Some(protected) = video_fec::red_body(&packet.payload)
                .and_then(|fec| fec_receive::fec_protected(fec).ok())
            {
                self.burst_ends
                    .fec_protects(stream, *packet.seq_no, &protected);
            }
        }
    }

    /// The bursts on the video streams THIS peer publishes that have
    /// ended with packets still missing since the last call, each
    /// once, by `(mid, rid)`: those that ended unrepaired
    /// ([`BurstEnd::asks`]) and those that ask for nothing.
    pub(crate) fn poll_burst_ends(&mut self, now: Instant) -> Vec<BurstEnd<VideoStream>> {
        if !self.protection.burst_keyframe {
            return Vec::new();
        }
        self.burst_ends.poll(now)
    }

    /// Whether THIS (subscribing) peer is being sent the stream
    /// `(mid, rid)` of `publisher` right now: a downstream track of it
    /// is open and not paused, and, for a simulcast source, is on or
    /// selected to that layer. A track that is still negotiating, one
    /// the subscriber does not show and one that waits for a key frame
    /// to resume at are not being sent.
    pub(crate) fn is_sent_stream(
        &self,
        publisher: &MediaPeerId,
        mid: Mid,
        rid: Option<Rid>,
    ) -> bool {
        self.tracks_out.iter().any(|t| {
            t.source_peer == *publisher
                && t.source_mid == mid
                && matches!(t.state, TrackOutState::Open(_))
                && t.forwarding.gate == Gate::Flowing
                && (rid.is_none() || t.forwarding.active_rid == rid || t.chosen_rid == rid)
        })
    }

    /// Stop asking THIS (publishing) peer to retransmit the packets of
    /// its stream `(mid, rid)` that are missing right now: a key frame
    /// has been asked for instead, or nobody is being sent the stream.
    /// Later losses are asked for as usual. Returns how many packets
    /// were given up; zero when there is no such receive stream.
    pub(crate) fn give_up_missing(&mut self, mid: Mid, rid: Option<Rid>) -> usize {
        self.rtc
            .direct_api()
            .stream_rx_by_mid(mid, rid)
            .map_or(0, str0m::rtp::StreamRx::give_up_missing)
    }

    /// Whether a key frame request on `(mid, rid)` of THIS (publishing)
    /// peer is held back by the rate limit and will go out when its
    /// window ends.
    #[must_use]
    pub fn keyframe_request_owed(&self, mid: Mid, rid: Option<Rid>) -> bool {
        self.owed_keyframe_requests.contains(&(mid, rid))
    }

    /// Whether the uplink of the video THIS peer publishes is in a burst
    /// of loss at `now`.
    pub(crate) fn poll_uplink_burst(&mut self, now: Instant) -> bool {
        self.protection.playout_delay && self.uplink_bursts.poll(now)
    }

    /// Whether the downlink of THIS (subscribing) peer is in a burst of
    /// loss at `now`: a receiver report on any of its downstream video
    /// tracks that says more than a tenth was lost, read once per report,
    /// or NACKs for more than a tenth of the video packets sent to it
    /// over the last second ([`crate::playout_delay::DownlinkBursts`]).
    pub(crate) fn poll_downlink_burst(&mut self, now: Instant) -> bool {
        if !self.protection.playout_delay {
            return false;
        }
        let mut burst = false;
        let mut nacked = 0u64;
        let mut api = self.rtc.direct_api();
        for entry in &self.tracks_out {
            let TrackOutState::Open(local_mid) = entry.state else {
                continue;
            };
            if entry.kind != MediaKind::Video {
                continue;
            }
            let Some(stream) = api.stream_tx_by_mid(local_mid, None) else {
                continue;
            };
            nacked = nacked.saturating_add(stream.nacked_packets());
            let reports = stream.receiver_reports();
            let read = self.reports_read.entry(local_mid).or_insert(0);
            if reports != *read {
                *read = reports;
                burst |= DownlinkBursts::on_receiver_report(stream.last_fraction_lost());
            }
        }
        burst |= self
            .downlink_bursts
            .on_interval(now, self.video_sent, nacked);
        burst
    }

    /// Bring the playout delay of every publisher's video toward THIS
    /// (subscribing) peer to `now`. `downlink_burst` is this peer's own
    /// ([`Self::poll_downlink_burst`]); `uplink_bursts` names the
    /// publishers of its call whose uplink is in a burst. A pair exists
    /// for every publisher this peer has a video track of, and is
    /// dropped with the last of them. Returns the pairs whose stamped
    /// value changed: publisher, from, to, why.
    pub(crate) fn update_playout_delays(
        &mut self,
        now: Instant,
        downlink_burst: bool,
        uplink_bursts: &[MediaPeerId],
    ) -> Vec<(MediaPeerId, u16, u16, playout_delay::Reason)> {
        let mut changes = Vec::new();
        if !self.protection.playout_delay {
            return changes;
        }
        let tracks = &self.tracks_out;
        self.playout.retain(|publisher, _| {
            tracks
                .iter()
                .any(|t| t.kind == MediaKind::Video && t.source_peer == *publisher)
        });
        for track in tracks {
            if track.kind == MediaKind::Video && !self.playout.contains_key(&track.source_peer) {
                self.playout
                    .insert(track.source_peer.clone(), PairDelay::default());
            }
        }
        for (publisher, pair) in &mut self.playout {
            if downlink_burst {
                pair.policy.on_downlink_burst(now);
            }
            if uplink_bursts.contains(publisher) {
                pair.policy.on_uplink_burst(now);
            }
            let (delay, reason) = pair.policy.delay_and_reason(now);
            if delay != pair.stamped_ms {
                changes.push((publisher.clone(), pair.stamped_ms, delay, reason));
                pair.stamped_ms = delay;
            }
        }
        changes
    }

    /// The minimum playout delay being stamped on `publisher`'s video
    /// toward this peer, in milliseconds. `None` when there is no such
    /// pair.
    #[must_use]
    pub fn playout_delay_ms(&self, publisher: &MediaPeerId) -> Option<u16> {
        self.playout.get(publisher).map(|pair| pair.stamped_ms)
    }

    /// The part of [`Self::forward_packet`] that applies while a switch
    /// to the packet's layer is pending on its track: another layer is on
    /// the wire and this one is selected. The packet is held, and the
    /// switch happens once a key frame of the selected layer is whole
    /// ([`SwitchWait`]); the held packets then go out through
    /// [`Self::forward_on_wire`] as packets of the layer now on the wire.
    ///
    /// The same holding applies to a track that has nothing on the wire
    /// yet and is to begin at a whole key frame
    /// ([`Forwarding::start_at_key_frame`]): its beginning is a switch
    /// from nothing.
    ///
    /// `None` when no switch is pending for this packet, and for an FEC
    /// packet: that is never held, but its sequence number is noted so it
    /// is not taken for a missing packet of a key frame.
    fn forward_at_pending_switch(
        &mut self,
        idx: usize,
        packet: &RelayPacket,
    ) -> Option<MediaForward> {
        packet.rid?;
        let entry = self.tracks_out.get_mut(idx)?;
        if packet.rid != entry.chosen_rid {
            return None;
        }
        let pending = match entry.forwarding.active_rid {
            Some(on_wire) => packet.rid != Some(on_wire),
            None => entry.forwarding.start_at_key_frame,
        };
        if !pending {
            return None;
        }
        if packet.is_fec() {
            entry.forwarding.waiting.note_fec(*packet.seq_no);
            return None;
        }
        let Some(run) = entry.forwarding.waiting.offer(packet) else {
            return Some(MediaForward::WrongLayer);
        };
        entry.forwarding.active_rid = packet.rid;
        entry.forwarding.start_at_key_frame = false;
        let mut outcome = MediaForward::WrongLayer;
        for held in &run {
            let forwarded = self.forward_on_wire(idx, held);
            if !matches!(outcome, MediaForward::Sent) {
                outcome = forwarded;
            }
        }
        Some(outcome)
    }

    /// Offer a publisher's FEC packet to this (subscribing) peer
    /// ([ADR-0067](../../../docs/adr/0067-loss-resilient-call-media.md)
    /// step 4, part 2).
    ///
    /// The packet itself is never sent: it covers the publisher's
    /// header extension block. What it leaves on this peer's stream
    /// is a free sequence number (the publisher numbers media and
    /// FEC in one space, and the stream keeps the publisher's
    /// numbering plus an offset) and the list of packets worth
    /// protecting together. So for a track that negotiated `red` and
    /// `ulpfec`, the SFU asks its own FEC sender for a packet in that
    /// slot over the same media packets as this peer was sent them.
    /// It is built once every one of them has gone on the wire here
    /// ([`Self::send_leg_fec`]), at once if they already have.
    ///
    /// Nothing is asked for, and the slot stays a gap as before,
    /// when: the track did not negotiate the pair; no RED-wrapped
    /// media was sent on it yet; the FEC packet belongs to another
    /// simulcast layer or source stream than the one on the wire
    /// here; its header does not parse, or names a packet that is
    /// not before it or has no slot on this stream.
    ///
    /// **Opacity (invariant #2, C-39).** Read here: the FEC header of
    /// the publisher's packet (its mask). No media byte.
    ///
    /// Both downstream tracks of a simulcast track are offered the
    /// packet; only the one whose layer is on the wire has a slot
    /// for it.
    pub(crate) fn offer_publisher_fec(&mut self, source_peer: &MediaPeerId, packet: &RelayPacket) {
        if !packet.is_fec() {
            return;
        }
        for role in [
            TrackRole::Main,
            TrackRole::LightCompanion,
            TrackRole::Middle,
        ] {
            if let Some(idx) = self.open_track(source_peer, packet.mid, role) {
                // A track the subscriber is not being sent has no slot
                // for anything: taking one would leave a gap in its
                // stream across the pause.
                if self.tracks_out[idx].forwarding.gate != Gate::Flowing {
                    continue;
                }
                // A track waiting for a whole key frame of this layer
                // must not take the FEC packet's number for a lost
                // media packet.
                let _ = self.forward_at_pending_switch(idx, packet);
                self.offer_publisher_fec_to(idx, packet);
            }
        }
    }

    /// [`Self::offer_publisher_fec`] for the downstream track at `idx`.
    fn offer_publisher_fec_to(&mut self, idx: usize, packet: &RelayPacket) {
        if !self.video_fec || !self.connected {
            return;
        }
        let Some(entry) = self.tracks_out.get_mut(idx) else {
            return;
        };
        let TrackOutState::Open(local_mid) = entry.state else {
            return;
        };
        if video_fec_negotiated(&self.rtc, local_mid).is_none() {
            return;
        }
        let Some(leg) = self.fec_senders.get_mut(&local_mid) else {
            return;
        };
        // Only the stream on the wire has slots here. This never latches a
        // layer or starts a stream: media does that.
        let forwarding = &mut entry.forwarding;
        if forwarding.active_rid != packet.rid || forwarding.source_ssrc != Some(packet.ssrc) {
            return;
        }
        let Some(map) = forwarding.map.as_mut() else {
            return;
        };
        let Some(protected) = video_fec::red_body(&packet.payload)
            .and_then(|fec| fec_receive::fec_protected(fec).ok())
        else {
            return;
        };
        // The mask names 16-bit numbers; each is a packet sent before the
        // FEC packet, so its extended number is the FEC packet's minus the
        // distance.
        let fec_seq = *packet.seq_no;
        let mut leg_seqs = Vec::with_capacity(protected.len());
        for seq in protected {
            let behind = wire_seq(fec_seq).wrapping_sub(seq);
            if behind == 0 || behind >= 0x8000 {
                return;
            }
            let Some(downstream) = fec_seq
                .checked_sub(u64::from(behind))
                .and_then(|source| map.peek(source))
            else {
                return;
            };
            leg_seqs.push(wire_seq(downstream));
        }
        if crate::fec_send::mask_for(&leg_seqs).is_none() {
            return;
        }
        // Taking the slot moves the stream's newest number onto it, so a
        // later change of source stream continues after it.
        let clock_hz = packet.params.spec().clock_rate.get();
        let Some(slot) = map.map(fec_seq, packet.rtp_time, packet.arrival, clock_hz) else {
            return;
        };
        let tag = FecSlot {
            seq_no: slot.seq_no,
            rtp_time: slot.rtp_time,
            wallclock: slot.wallclock,
        };
        let Ok(generated) = leg.sender.request(wire_seq(slot.seq_no), leg_seqs, tag) else {
            return;
        };
        self.write_leg_fec(local_mid, generated);
    }

    /// Take the packets str0m serialized for this (subscribing)
    /// peer since the last call and give each RED-wrapped media
    /// packet to its track's FEC sender; write every FEC packet
    /// that completes.
    ///
    /// The bytes are each packet as it went on the wire at its
    /// first transmission, before SRTP: this leg's header extension
    /// block, with `mid` present or not as str0m decided at that
    /// moment. They are put into the form a libwebrtc receiver
    /// feeds its FEC decoder ([`fec_receive::media_from_red`]: RED
    /// header byte removed, media payload type, the extensions a
    /// pacer fills in zeroed, payload and padding as sent), which
    /// is what the FEC has to be computed over. A retransmission is
    /// never among them: its extension block is its own.
    ///
    /// **Opacity (invariant #2, C-39).** The payload is `SFrame`
    /// ciphertext and is only XORed ([`crate::fec_send::encode`]).
    /// What is read: the fixed header, the extension framing and
    /// the RED header byte.
    pub(crate) fn send_leg_fec(&mut self) {
        if !self.video_fec || self.fec_senders.is_empty() {
            return;
        }
        while let Some(wire) = self.rtc.direct_api().take_sent_rtp() {
            let (Some(ssrc), Some(seq), Some(pt)) = (
                wire.get(8..12)
                    .and_then(|b| <[u8; 4]>::try_from(b).ok())
                    .map(u32::from_be_bytes),
                wire.get(2..4)
                    .and_then(|b| <[u8; 2]>::try_from(b).ok())
                    .map(u16::from_be_bytes),
                wire.get(1).map(|b| b & 0x7f),
            ) else {
                continue;
            };
            let Some(local_mid) = self
                .fec_senders
                .iter()
                .find(|(_, leg)| *leg.ssrc == ssrc && *leg.red_pt == pt)
                .map(|(mid, _)| *mid)
            else {
                continue;
            };
            let mutable = self.mutable_extensions(local_mid);
            let Some(view) = fec_receive::media_from_red(&wire, &mutable) else {
                continue;
            };
            let Some(leg) = self.fec_senders.get_mut(&local_mid) else {
                continue;
            };
            if view.get(1).map(|b| b & 0x7f) == Some(*leg.ulpfec_pt) {
                // An FEC packet of the SFU's own: not media.
                continue;
            }
            let generated = leg.sender.sent_media(seq, view);
            self.write_leg_fec(local_mid, generated);
        }
    }

    /// Write FEC packets built for the track `local_mid` to its
    /// send stream: RED-wrapped (outer payload type the peer's
    /// `red`, header byte its `ulpfec`), at the sequence number,
    /// timestamp and wall clock of the slot, without the marker
    /// bit. Not nackable: like libwebrtc, the SFU does not
    /// retransmit FEC. From there it is a packet like any other
    /// to str0m: it leaves on the next poll, after the media it
    /// protects, and takes a transport-wide sequence number.
    fn write_leg_fec(&mut self, local_mid: Mid, generated: Vec<Generated<FecSlot>>) {
        use crate::prom::names::VIDEO_FEC_SENT_TOTAL;
        if generated.is_empty() {
            return;
        }
        let Some(leg) = self.fec_senders.get(&local_mid) else {
            return;
        };
        let mut api = self.rtc.direct_api();
        let Some(stream) = api.stream_tx_by_mid(local_mid, None) else {
            return;
        };
        for packet in generated {
            let mut payload = Vec::with_capacity(video_fec::RED_HEADER_LEN + packet.fec.len());
            payload.push(*leg.ulpfec_pt);
            payload.extend_from_slice(&packet.fec);
            let written = stream.write_rtp(
                leg.red_pt,
                SeqNo::from(packet.tag.seq_no),
                packet.tag.rtp_time,
                packet.tag.wallclock,
                false,
                ExtensionValues::default(),
                false,
                payload,
            );
            if written.is_ok() {
                metrics::counter!(VIDEO_FEC_SENT_TOTAL).increment(1);
                self.fec_sent.push(leg.source.clone());
            }
        }
    }

    /// The FEC packets written to this peer since the last call,
    /// as the publisher track each protects and the role of the
    /// downstream track it was written to.
    pub(crate) fn take_fec_sent(&mut self) -> Vec<(MediaPeerId, Mid, TrackRole)> {
        std::mem::take(&mut self.fec_sent)
    }

    /// Choose which simulcast layer this peer receives for the downstream track
    /// mirroring `(source_peer, source_mid)`. A bandwidth / quality policy calls
    /// this to override the auto-latched first layer — e.g. downgrade to a lower
    /// layer when the subscriber's egress estimate drops. `None` re-arms the
    /// auto-latch (the next observed layer is chosen). Returns `true` if a
    /// matching downstream track exists. Only the main track is selected: a
    /// light companion ([`TrackRole::LightCompanion`]) never changes layer.
    ///
    /// The selection is not the switch. A track that is already forwarding a
    /// layer keeps forwarding it until a key frame of the selected one is
    /// whole at the SFU ([`SwitchWait`]), so the subscriber never receives a
    /// delta frame it has no reference for, nor a key frame it cannot
    /// complete.
    pub fn select_layer(
        &mut self,
        source_peer: &MediaPeerId,
        source_mid: Mid,
        rid: Option<Rid>,
    ) -> bool {
        for entry in &mut self.tracks_out {
            if entry.source_peer == *source_peer
                && entry.source_mid == source_mid
                && entry.role == TrackRole::Main
            {
                if entry.chosen_rid != rid {
                    entry.forwarding.waiting.clear();
                }
                entry.chosen_rid = rid;
                return true;
            }
        }
        false
    }

    /// What bitrate control needs of a packet THIS peer published,
    /// read before [`Self::relay_packet`] consumes the packet: its
    /// absolute send time and its size (RTP header and payload).
    /// Header fields only; the payload is not read.
    pub(crate) fn uplink_seen(packet: &RtpPacket) -> UplinkSeen {
        let header_len = packet.header.header_len.max(RTP_FIXED_HEADER_LEN);
        UplinkSeen {
            abs_send_time: packet.header.ext_vals.abs_send_time,
            size: header_len.saturating_add(packet.payload.len()),
        }
    }

    /// Hand one packet THIS peer published to its bitrate estimator
    /// (ADR-0068). A no-op under [`BitrateControl::Client`].
    ///
    /// `seen` is what [`Self::uplink_seen`] read off the packet and
    /// `packet` what [`Self::relay_packet`] made of it. Every stream
    /// of the publisher counts, audio included: they share one path.
    /// A packet whose sequence number is behind the highest of its
    /// stream is a retransmission (str0m hands a packet from the
    /// retransmission stream over already rewritten as the packet it
    /// repairs) or was reordered.
    pub(crate) fn observe_uplink(&mut self, seen: UplinkSeen, packet: &RelayPacket) {
        let Some(uplink) = self.uplink.as_mut() else {
            return;
        };
        if packet.kind == MediaKind::Video && !uplink.video_ssrcs.contains(&packet.ssrc) {
            if uplink.video_ssrcs.len() >= MAX_REMB_STREAMS {
                uplink.video_ssrcs.clear();
            }
            uplink.video_ssrcs.push(packet.ssrc);
        }
        let late = uplink.loss.on_packet(*packet.ssrc, *packet.seq_no);
        let base = uplink.send_time_base;
        uplink.estimator.on_packet(PacketArrival {
            send_time_24: seen.abs_send_time.map(|sent| abs_send_time_24(sent, base)),
            arrival: packet.arrival,
            size: seen.size,
            retransmission: late,
        });
    }

    /// Run bitrate control for THIS (publishing) peer: hand the loss
    /// of the last interval to the estimator when an interval has
    /// passed, and ask str0m to send the publisher a REMB when one is
    /// due. Returns `true` when a REMB was requested. A no-op under
    /// [`BitrateControl::Client`] and until the peer has sent video.
    ///
    /// **Which SSRCs the REMB names.** str0m builds a REMB per
    /// receive stream, naming that stream's main SSRC
    /// (`StreamRx::request_remb`). One is requested on every video
    /// receive stream of the publisher, each carrying the same figure:
    /// the estimate for the publisher's whole path. A libwebrtc sender
    /// takes the bitrate of any REMB it receives as its transport-wide
    /// receiver limit and does not consult the SSRC list
    /// (`RTCPReceiver::HandlePsfbApp` stores the bitrate alone, M125),
    /// so the list only has to name streams the publisher owns. Audio
    /// streams are not named: `goog-remb` is negotiated on video
    /// payload types only.
    ///
    /// A REMB goes out at once when the estimate fell below
    /// [`REMB_DROP_PERCENT`] of the last one sent, otherwise no sooner
    /// than [`REMB_MIN_INTERVAL`] after the last when the figure
    /// changed, and at least every [`REMB_MAX_INTERVAL`]. The request
    /// is picked up by str0m's next timeout input, which the media
    /// loop feeds every pass.
    pub fn drive_uplink(&mut self, now: Instant) -> bool {
        let Some(uplink) = self.uplink.as_mut() else {
            return false;
        };
        if now.saturating_duration_since(uplink.last_loss_feed) >= UPLINK_LOSS_INTERVAL {
            uplink.last_loss_feed = now;
            let (expected, lost) = uplink.loss.take_interval();
            uplink.estimator.on_loss(now, expected, lost);
        }
        for cut in uplink.estimator.take_cut_notes() {
            tracing::info!(
                origin = %self.id,
                rule = cut.rule,
                from_kbps = cut.from_bps / 1000,
                to_kbps = cut.to_bps / 1000,
                arriving_kbps = cut.arriving_bps / 1000,
                queue_ms = ?cut.queue_ms,
                trend = cut.trend,
                threshold = cut.threshold,
                confirmed = cut.confirmed,
                "media: a publisher's bitrate estimate was cut for over-use"
            );
        }
        if !self.connected || uplink.video_ssrcs.is_empty() {
            return false;
        }
        let bps = uplink.estimator.estimate_bps();
        let due = uplink.last_remb.map_or(true, |(at, last)| {
            let since = now.saturating_duration_since(at);
            bps.saturating_mul(100) < last.saturating_mul(REMB_DROP_PERCENT)
                || (bps != last && since >= REMB_MIN_INTERVAL)
                || since >= REMB_MAX_INTERVAL
        });
        if !due {
            return false;
        }
        let mut requested = false;
        let mut api = self.rtc.direct_api();
        for ssrc in &uplink.video_ssrcs {
            if let Some(stream) = api.stream_rx(ssrc) {
                stream.request_remb(Bitrate::bps(bps));
                requested = true;
            }
        }
        if requested {
            uplink.last_remb = Some((now, bps));
            metrics::counter!(crate::prom::names::BITRATE_REMB_SENT_TOTAL).increment(1);
        }
        requested
    }

    /// The figures of this publisher's bitrate estimate for a log
    /// line; `None` under [`BitrateControl::Client`].
    pub fn uplink_snapshot(&mut self, now: Instant) -> Option<crate::uplink_estimate::Snapshot> {
        self.uplink.as_mut().map(|u| u.estimator.snapshot(now))
    }

    /// Record one incoming simulcast packet into this (publishing) peer's
    /// per-layer bitrate meters. No-op for a non-simulcast packet
    /// (`rid == None`) — only simulcast layers need per-layer accounting.
    pub(crate) fn record_layer_packet(&mut self, packet: &RelayPacket, now: Instant) {
        let Some(rid) = packet.rid else {
            return;
        };
        self.layer_meters
            .entry((packet.mid, rid))
            .or_insert_with(|| LayerBitrateMeter::new(now))
            .record(packet.payload.len(), now);
        // FEC packets and the RED header byte are counted: they are part of
        // what the layer costs a subscriber that takes them.
    }

    /// Record one incoming packet into the arrival-rate meter of its
    /// track (ADR-0070 step 6): what the track costs a subscriber that is
    /// sent it. Counted by size alone, header and payload. A no-op unless
    /// downlink control is on. A publisher's FEC packets are not counted:
    /// no subscriber is sent them.
    pub(crate) fn record_track_packet(&mut self, packet: &RelayPacket, now: Instant) {
        if self.downlink.is_none() || packet.is_fec() {
            return;
        }
        if !self.track_meters.contains_key(&packet.mid)
            && self.track_meters.len() >= MAX_TRACK_METERS
        {
            self.track_meters.clear();
        }
        let meter = self
            .track_meters
            .entry(packet.mid)
            .or_insert_with(|| LayerBitrateMeter::new(now));
        meter.record(packet.payload.len() + RTP_FIXED_HEADER_LEN, now);
        // What a heavy sender costs while it flows is remembered for the
        // time it is switched off ([`Self::heavy_rate_bps`]). Not right
        // after it came back: the meter's window still holds the silence.
        let rate = meter.bitrate_bps_cost_at(now);
        if let Some(sender) = self.needs.senders.get_mut(&packet.mid) {
            let settled = sender.returned_at.map_or(true, |at| {
                now.saturating_duration_since(at) >= video_needed::RATE_SETTLE
            });
            if rate > 0 && settled {
                sender.last_rate = Some((rate, now));
            }
        }
    }

    /// The rate arriving on the track `mid` this peer publishes, bits per
    /// second; zero before its first full second and once it has been
    /// silent for two.
    fn track_rate_bps(&self, mid: Mid, now: Instant) -> u64 {
        self.track_meters
            .get(&mid)
            .map_or(0, |meter| meter.bitrate_bps_cost_at(now))
    }

    /// What the heavy camera sender on `mid` (main or middle, by `role`)
    /// costs a subscriber that is given it, bits per second
    /// ([`crate::video_needed`]).
    ///
    /// While the sender flows that is the rate arriving on it
    /// ([`Self::track_rate_bps`]). A sender that is switched off on the
    /// SFU's word, or has just been switched back on, has no rate to
    /// measure, and "costs nothing" would make giving it look free. It
    /// is then taken to cost what was last measured on it, for
    /// [`video_needed::RATE_MEMORY`], and after that the nominal rate of
    /// its role ([`video_needed::NOMINAL_MAIN_BPS`],
    /// [`video_needed::NOMINAL_MIDDLE_BPS`]); never zero, and never less
    /// than what is arriving. A sender the publisher was never told to
    /// stop is measured as before: zero until its first packets.
    fn heavy_rate_bps(&self, mid: Mid, role: TrackRole, now: Instant) -> u64 {
        let measured = self.track_rate_bps(mid, now);
        let Some(sender) = self.needs.senders.get(&mid) else {
            return measured;
        };
        let settling = sender
            .returned_at
            .is_some_and(|at| now.saturating_duration_since(at) < video_needed::RATE_SETTLE);
        let off = sender.expect_silence || sender.idle || settling;
        if !off || self.downlink.is_none() {
            return measured;
        }
        if measured > 0 && !sender.idle && !settling {
            // Told to stop and still sending: it costs what arrives.
            return measured;
        }
        let nominal = match role {
            TrackRole::Middle => video_needed::NOMINAL_MIDDLE_BPS,
            TrackRole::Main | TrackRole::LightCompanion => video_needed::NOMINAL_MAIN_BPS,
        };
        let remembered = sender
            .last_rate
            .filter(|(_, at)| now.saturating_duration_since(*at) <= video_needed::RATE_MEMORY)
            .map_or(nominal, |(bps, _)| bps);
        remembered.max(measured)
    }

    /// What the tracks this peer publishes cost at `now`
    /// ([`PublishedRates`]). All zero unless downlink control is on.
    #[must_use]
    pub fn published_rates(&self, now: Instant) -> PublishedRates {
        let mut rates = PublishedRates::default();
        for track in &self.tracks_in {
            let rate = self.track_rate_bps(track.mid, now);
            match (track.kind, track.role()) {
                (MediaKind::Audio, _) => rates.audio_bps = rates.audio_bps.saturating_add(rate),
                (MediaKind::Video, TrackRole::LightCompanion) => {
                    rates.light_bps = rates.light_bps.saturating_add(rate);
                },
                (MediaKind::Video, TrackRole::Middle) => {
                    let rate = self.heavy_rate_bps(track.mid, TrackRole::Middle, now);
                    rates.middle_bps = rates.middle_bps.saturating_add(rate);
                },
                (MediaKind::Video, TrackRole::Main) => {
                    // A simulcast track reaches a subscriber as one layer
                    // on the main track and the light layer on the
                    // companion; its meter counts every layer.
                    let layers = self.layer_bitrates_for(track.mid);
                    match (track.simulcast, layers.first(), layers.last()) {
                        (true, Some((_, light)), Some((_, heaviest))) if layers.len() > 1 => {
                            rates.light_bps = rates.light_bps.saturating_add(*light);
                            rates.main_bps = rates.main_bps.saturating_add(*heaviest);
                        },
                        _ => {
                            let rate = self.heavy_rate_bps(track.mid, TrackRole::Main, now);
                            rates.main_bps = rates.main_bps.saturating_add(rate);
                        },
                    }
                },
            }
        }
        rates
    }

    /// Whether THIS (subscribing) peer wants the main and the middle
    /// picture of `publisher`, for the count that decides which of the
    /// publisher's senders are needed ([`crate::video_needed`]).
    /// `publisher_has_middle` says whether the publisher has a middle
    /// sender at all.
    ///
    /// Only by what the peer **said**: the entry for the publisher in its
    /// latest `video-wants`. Not by what it is being forwarded at the
    /// moment (which this does not change) and not by what downlink
    /// control gives. A peer that holds no video track of the publisher
    /// wants nothing. Otherwise:
    ///
    /// - **`middle`**: it wants middle and never main, also while its
    ///   middle track is still being negotiated or is not open: the
    ///   middle picture is what it will be sent. Only of a publisher
    ///   that has no middle sender at all does it want main (that is
    ///   what such a subscriber is sent, for good).
    /// - **`main`**: it wants main, when it has a main track of the
    ///   publisher in any state; and, under downlink control, middle
    ///   too, which is what its allocation falls back to and has to know
    ///   the cost of.
    /// - **`light`**: neither.
    /// - **not named, or no `video-wants` ever**: neither. Such a peer is
    ///   still *forwarded* the main track when it flows (the forwarding
    ///   default is unchanged), but it switches nobody's sender on:
    ///   somebody joining a call must not make every publisher start its
    ///   main sender before it has said what it shows.
    pub(crate) fn heavy_wanted_of(
        &self,
        publisher: &MediaPeerId,
        publisher_has_middle: bool,
    ) -> (bool, bool) {
        let has = |role: Option<TrackRole>| {
            self.tracks_out.iter().any(|t| {
                t.source_peer == *publisher
                    && t.kind == MediaKind::Video
                    && role.map_or(true, |role| t.role == role)
            })
        };
        if !has(None) {
            return (false, false);
        }
        match self.video_wants.get(publisher).copied() {
            Some(VideoQuality::Middle) if publisher_has_middle => (false, true),
            Some(VideoQuality::Middle) => (has(Some(TrackRole::Main)), false),
            Some(VideoQuality::Main) => (
                has(Some(TrackRole::Main)),
                has(Some(TrackRole::Middle)) && self.downlink.is_some(),
            ),
            Some(VideoQuality::Light) | None => (false, false),
        }
    }

    /// Whether THIS (publishing) peer is told which of its camera senders
    /// are needed: the switch is on and it declared a light track in an
    /// offer, which is what a client that sends its camera on separate
    /// senders does. A publisher of one video track, and one that sends
    /// simulcast, is never told anything.
    #[must_use]
    pub fn told_video_needed(&self) -> bool {
        self.protection.video_needed && self.light_mid.is_some()
    }

    /// What THIS (publishing) peer was last told of its senders; `None`
    /// before the first frame.
    #[must_use]
    pub fn video_needed_told(&self) -> Option<Needed> {
        self.needs.tracker.told()
    }

    /// The last `video-needed` frame could not be queued for THIS peer:
    /// it is produced again at the next pass.
    pub(crate) fn video_needed_not_delivered(&mut self) {
        self.needs.tracker.forget_told();
    }

    /// Whether THIS (publishing) peer is told how much its camera senders
    /// may send: the switch is on and the SFU decides its bitrate
    /// ([`BitrateControl::Server`]).
    #[must_use]
    pub fn told_publish_budget(&self) -> bool {
        self.protection.publish_budget && self.uplink.is_some()
    }

    /// What THIS (publishing) peer was last told its senders may send;
    /// `None` before the first frame.
    #[must_use]
    pub fn publish_budget_told(&self) -> Option<publish_budget::Budget> {
        self.budget.told()
    }

    /// The last `publish-budget` frame could not be queued for THIS peer:
    /// it is produced again at the next pass.
    pub(crate) fn publish_budget_not_delivered(&mut self) {
        self.budget.forget_told();
    }

    /// One pass of the publish budget for THIS (publishing) peer
    /// ([`crate::publish_budget`]): make the two ceilings from the
    /// current estimate of its uplink and return them when the publisher
    /// is to be told.
    ///
    /// Nothing until the peer is connected and has sent video: that is
    /// when its estimate first goes out as REMB too
    /// ([`Self::drive_uplink`]), and before it the figure is only the
    /// configured start value. What the rule is made from:
    ///
    /// - the estimate, as the REMB carries it;
    /// - audio: the rate arriving on the peer's audio tracks when
    ///   downlink control keeps its track meters
    ///   ([`Self::published_rates`]), otherwise the module's constant;
    /// - whether the peer declared a light and a middle sender;
    /// - which of its senders it was last told are needed
    ///   ([`crate::video_needed`]); a peer that was told nothing is
    ///   sending everything.
    ///
    /// `None`, and nothing done, for a peer that is not told
    /// ([`Self::told_publish_budget`]).
    pub(crate) fn drive_publish_budget(&mut self, now: Instant) -> Option<BudgetChange> {
        if !self.told_publish_budget() || !self.connected {
            return None;
        }
        let estimate_bps = {
            let uplink = self.uplink.as_ref()?;
            if uplink.video_ssrcs.is_empty() {
                return None;
            }
            uplink.estimator.estimate_bps()
        };
        let audio_bps = self
            .downlink
            .is_some()
            .then(|| self.published_rates(now).audio_bps);
        let inputs = publish_budget::Inputs {
            estimate_bps,
            audio_bps,
            has_light: self.light_mid.is_some(),
            has_middle: self.middle_mid.is_some(),
            main_needed: self.told_needed(TrackRole::Main),
            middle_needed: self.told_needed(TrackRole::Middle),
        };
        let (budget, why) = self.budget.update(inputs, now)?;
        Some(BudgetChange {
            budget,
            why,
            inputs,
        })
    }

    /// The role of the heavy camera sender THIS peer publishes on `mid`:
    /// `Middle` for its declared middle track, `Main` for any other
    /// video track of one encoding that is not its light track. `None`
    /// for audio, the light track, a simulcast track and an unknown mid.
    fn heavy_sender_role(&self, mid: Mid) -> Option<TrackRole> {
        let track = self.tracks_in.iter().find(|t| t.mid == mid)?;
        if track.kind != MediaKind::Video || track.simulcast || track.light {
            return None;
        }
        Some(track.role())
    }

    /// Whether THIS peer was last told that its sender of `role` is
    /// needed. A peer that was told nothing is sending everything.
    fn told_needed(&self, role: TrackRole) -> bool {
        self.needs.tracker.told().map_or(true, |told| match role {
            TrackRole::Middle => told.middle,
            TrackRole::Main | TrackRole::LightCompanion => told.main,
        })
    }

    /// One pass of "senders nobody watches" for THIS (publishing) peer
    /// ([`crate::video_needed`]): `wanted` is how many subscribers want
    /// each of its heavy pictures right now.
    ///
    /// - A sender the peer was told to stop that has sent nothing for
    ///   [`video_needed::IDLE_QUIET`] is taken for **idle**: what was
    ///   missing on it is no longer asked for, and its stream is
    ///   forgotten by the two burst detectors and the loss counter of
    ///   the bitrate estimate, so neither the stop nor the return reads
    ///   as loss.
    /// - When what the peer is to be told has changed, it is returned
    ///   for the caller to send. A sender told to stop is marked as
    ///   expected to go silent and the bitrate estimate of that moment
    ///   is remembered; a sender told to start again has the estimate
    ///   put back to it at once
    ///   ([`crate::uplink_estimate::PathEstimator::restore_after_idle`])
    ///   and a REMB sent with the next clock tick.
    ///
    /// `None`, and nothing done, for a peer that is not told
    /// ([`Self::told_video_needed`]).
    pub(crate) fn drive_video_needed(
        &mut self,
        wanted: Wanted,
        now: Instant,
    ) -> Option<NeededChange> {
        if !self.told_video_needed() {
            return None;
        }
        self.mark_idle_senders(now);
        let before = self.needs.tracker.told();
        let needed = self
            .needs
            .tracker
            .update(wanted, self.middle_mid.is_some(), now)?;
        let heavy: Vec<(Mid, TrackRole)> = self
            .tracks_in
            .iter()
            .filter_map(|t| self.heavy_sender_role(t.mid).map(|role| (t.mid, role)))
            .collect();
        let on = |role: TrackRole| match role {
            TrackRole::Middle => needed.middle,
            TrackRole::Main | TrackRole::LightCompanion => needed.main,
        };
        let mut stopped = false;
        for (mid, role) in &heavy {
            if on(*role) {
                continue;
            }
            stopped = true;
            if !self.needs.senders.contains_key(mid) && self.needs.senders.len() >= MAX_TRACK_METERS
            {
                self.needs.senders.clear();
            }
            self.needs.senders.entry(*mid).or_default().expect_silence = true;
        }
        let started =
            before.is_some_and(|was| (!was.main && needed.main) || (!was.middle && needed.middle));
        if let Some(uplink) = self.uplink.as_mut() {
            if started {
                if let Some(bps) = self.needs.estimate_before_idle {
                    if uplink.estimator.restore_after_idle(bps, now) {
                        tracing::info!(
                            origin = %self.id,
                            estimate_kbps = uplink.estimator.estimate_bps() / 1000,
                            "media: a publisher's bitrate estimate was put back as a sender is switched on again"
                        );
                    }
                }
                // Whatever the figure, the publisher hears it now.
                uplink.last_remb = None;
            }
            if stopped {
                let estimate = uplink.estimator.estimate_bps();
                let kept = self.needs.estimate_before_idle.unwrap_or(0);
                self.needs.estimate_before_idle = Some(kept.max(estimate));
            } else {
                self.needs.estimate_before_idle = None;
            }
        }
        Some(NeededChange { needed, wanted })
    }

    /// Take the heavy senders that were told to stop and have sent
    /// nothing for [`video_needed::IDLE_QUIET`] for idle
    /// ([`Self::drive_video_needed`]).
    fn mark_idle_senders(&mut self, now: Instant) {
        let gone: Vec<(Mid, Option<Ssrc>)> = self
            .needs
            .senders
            .iter()
            .filter(|(_, sender)| {
                sender.expect_silence
                    && !sender.idle
                    && sender.last_arrival.map_or(true, |at| {
                        now.saturating_duration_since(at) >= video_needed::IDLE_QUIET
                    })
            })
            .map(|(mid, sender)| (*mid, sender.ssrc))
            .collect();
        for (mid, ssrc) in gone {
            if let Some(sender) = self.needs.senders.get_mut(&mid) {
                sender.idle = true;
            }
            self.burst_ends.forget((mid, None));
            if let Some(ssrc) = ssrc {
                self.uplink_bursts.forget(*ssrc);
                if let Some(uplink) = self.uplink.as_mut() {
                    uplink.loss.forget(*ssrc);
                }
            }
            let given_up = self.give_up_missing(mid, None);
            tracing::info!(
                origin = %self.id,
                mid = ?mid,
                given_up,
                "media: a sender nobody is being sent has stopped"
            );
        }
    }

    /// The mids of the heavy senders of THIS (publishing) peer that are
    /// idle: switched off on the SFU's word, nothing arriving.
    pub(crate) fn idle_senders(&self) -> Vec<Mid> {
        self.needs
            .senders
            .iter()
            .filter(|(_, sender)| sender.idle)
            .map(|(mid, _)| *mid)
            .collect()
    }

    /// Whether the heavy sender on `mid` of THIS peer is idle.
    #[must_use]
    pub fn sender_idle(&self, mid: Mid) -> bool {
        self.needs.senders.get(&mid).is_some_and(|s| s.idle)
    }

    /// One video packet THIS peer published has arrived: note it against
    /// its sender ([`crate::video_needed`]). Called before the packet is
    /// handed to the loss counter and the burst detectors.
    ///
    /// The first packet of a sender that was idle is its **return**. The
    /// packets its sequence numbers skipped are not asked for (a sender
    /// that was off sent none), the count of key frame requests that got
    /// no answer starts again (they were asked of a sender that was
    /// off), and unless the packet itself begins a key frame the
    /// publisher is asked for one, so every subscriber of the track has
    /// a picture to start from. Header fields only.
    pub(crate) fn note_sender_arrival(&mut self, packet: &RelayPacket, now: Instant) {
        if packet.kind != MediaKind::Video || packet.rid.is_some() || !self.told_video_needed() {
            return;
        }
        let Some(role) = self.heavy_sender_role(packet.mid) else {
            return;
        };
        let told_on = self.told_needed(role);
        if !self.needs.senders.contains_key(&packet.mid)
            && self.needs.senders.len() >= MAX_TRACK_METERS
        {
            self.needs.senders.clear();
        }
        let sender = self.needs.senders.entry(packet.mid).or_default();
        let returned = sender.idle;
        let moved = sender.ssrc.is_some_and(|ssrc| ssrc != packet.ssrc);
        sender.ssrc = Some(packet.ssrc);
        sender.last_arrival = Some(now);
        sender.idle = false;
        if told_on {
            sender.expect_silence = false;
        }
        if !returned {
            return;
        }
        sender.returned_at = Some(now);
        if moved {
            // A sender that came back on another SSRC is a new stream to
            // every book already.
            return;
        }
        let key = (packet.mid, None);
        let given_up = self.give_up_missing(packet.mid, None);
        self.unanswered_keyframe_requests.remove(&key);
        let key_frame = packet.starts_independent_frame;
        let asked = told_on
            && !key_frame
            && !packet.blind
            && !packet.is_fec()
            && self.request_keyframe_limited(packet.mid, None, now);
        tracing::info!(
            origin = %self.id,
            mid = ?packet.mid,
            role = role.as_str(),
            needed = told_on,
            first_packet_starts_key_frame = key_frame,
            key_frame_asked = asked,
            given_up,
            "media: a sender that had stopped is sending again"
        );
    }

    /// The publishers this (subscribing) peer has an open downstream
    /// track of, each once, in the order their first track was opened.
    #[must_use]
    pub fn open_publishers(&self) -> Vec<MediaPeerId> {
        let mut out: Vec<MediaPeerId> = Vec::new();
        for track in &self.tracks_out {
            if matches!(track.state, TrackOutState::Open(_)) && !out.contains(&track.source_peer) {
                out.push(track.source_peer.clone());
            }
        }
        out
    }

    /// Whether this peer has an open downstream track of `publisher` of
    /// the given kind and role.
    #[must_use]
    pub fn has_open_track_of(
        &self,
        publisher: &MediaPeerId,
        kind: MediaKind,
        role: TrackRole,
    ) -> bool {
        self.tracks_out.iter().any(|t| {
            t.source_peer == *publisher
                && t.kind == kind
                && t.role == role
                && matches!(t.state, TrackOutState::Open(_))
        })
    }

    /// The budget of this (subscribing) peer's downlink, bits per second:
    /// the SFU's own estimate of what the path to it carries. `None`
    /// without downlink control and until the peer's transport-wide
    /// feedback has acknowledged a first packet: before that nothing is
    /// known of the path (a peer that negotiated no transport-wide
    /// feedback never sends any), and an unknown budget gives everyone
    /// what they asked for.
    #[must_use]
    pub fn downlink_budget_bps(&self) -> Option<u64> {
        let downlink = self.downlink.as_ref()?;
        downlink
            .estimator
            .has_arrivals()
            .then(|| downlink.estimator.estimate_bps())
    }

    /// Hand this (subscribing) peer's estimator the feedback records
    /// (packet send time, size, reported arrival time or `None`). What
    /// [`Self::pump_downlink`] does with the records str0m kept; a test
    /// calls it directly. A no-op without downlink control.
    pub(crate) fn downlink_feed(
        &mut self,
        records: &[(Instant, usize, Option<Instant>)],
        now: Instant,
    ) {
        let lowered = self
            .downlink
            .as_mut()
            .is_some_and(|downlink| downlink.feed(records, now));
        if lowered {
            self.drop_heavy_retransmissions();
        }
    }

    /// The path to this (subscribing) peer was found over-used: the
    /// packets of its main and middle video tracks kept for
    /// retransmission are dropped. The subscriber is missing packets of
    /// them and asking again, and answering puts megabits of late video
    /// on a path that has just overflowed (measured on the 7.5 Mbit/s
    /// link of the e2e test: about 1500 retransmissions lost at the full
    /// queue in the second after the first cut). A picture that cannot be
    /// repaired is asked for again as a key frame by the subscriber. The
    /// light tracks keep theirs.
    fn drop_heavy_retransmissions(&mut self) {
        let heavy: Vec<Mid> = self
            .tracks_out
            .iter()
            .filter(|t| t.kind == MediaKind::Video && t.role != TrackRole::LightCompanion)
            .filter_map(|t| match t.state {
                TrackOutState::Open(local_mid) => Some(local_mid),
                _ => None,
            })
            .collect();
        let mut api = self.rtc.direct_api();
        for local_mid in heavy {
            if let Some(stream) = api.stream_tx_by_mid(local_mid, None) {
                stream.set_rtx_cache(PAUSED_RTX_CACHE_PACKETS, PAUSED_RTX_CACHE_AGE, None);
            }
        }
    }

    /// One pass of this (subscribing) peer's downlink in the media loop,
    /// under downlink control: take the transport-wide feedback str0m
    /// kept and feed the estimator, and, while a probe runs, queue the
    /// padding it owes as copies of packets already sent to this peer
    /// (`StreamTx::resend`, on the retransmission stream; never new
    /// media). Returns the bytes of padding queued. A no-op, returning
    /// zero, without downlink control.
    pub fn pump_downlink(&mut self, now: Instant) -> usize {
        if self.downlink.is_none() {
            return 0;
        }
        let records = self.rtc.direct_api().take_twcc_feedback();
        self.downlink_feed(&records, now);
        let Some(downlink) = self.downlink.as_mut() else {
            return 0;
        };
        // No padding on top of a clump of media: the two together are
        // what overflows a queue.
        let mut owed = if downlink.clump_being_written(now) {
            0
        } else {
            downlink.probe.padding_owed(now)
        };
        let mut queued = 0usize;
        let mut copies = 0usize;
        let mut api = self.rtc.direct_api();
        while owed >= DOWNLINK_PAD_MIN_BYTES
            && copies < DOWNLINK_PAD_PACKETS_PER_PASS
            && !downlink.recent.is_empty()
        {
            // The oldest remembered packet goes to the back once copied,
            // so the copies go round the packets held.
            let Some((local_mid, seq_no, len)) = downlink.recent.pop_front() else {
                break;
            };
            let resent = api
                .stream_tx_by_mid(local_mid, None)
                .is_some_and(|stream| stream.resend(SeqNo::from(seq_no), now));
            if !resent {
                // No longer kept for retransmission: forgotten.
                continue;
            }
            downlink.recent.push_back((local_mid, seq_no, len));
            downlink.probe.on_sent(len, true, now);
            owed = owed.saturating_sub(len);
            queued += len;
            copies += 1;
        }
        downlink.padding_bytes = downlink
            .padding_bytes
            .saturating_add(u64::try_from(queued).unwrap_or(u64::MAX));
        queued
    }

    /// When this peer's downlink next needs a pass of the media loop: the
    /// next padding step of a running probe. `None` when none runs.
    #[must_use]
    pub fn next_downlink_tick(&self, now: Instant) -> Option<Instant> {
        self.downlink
            .as_ref()
            .and_then(|downlink| downlink.probe.next_tick(now))
    }

    /// The subscriber's order of importance for the publishers in
    /// `joined` (given in the order they joined the call): those it named
    /// in its last `video-wants`, in the order it named them, then the
    /// rest in the order they joined.
    #[must_use]
    pub fn importance_order(&self, joined: &[MediaPeerId]) -> Vec<MediaPeerId> {
        let named: &[MediaPeerId] = self
            .downlink
            .as_ref()
            .map_or(&[], |downlink| downlink.importance.as_slice());
        let mut out: Vec<MediaPeerId> = named
            .iter()
            .filter(|id| joined.contains(id))
            .cloned()
            .collect();
        for id in joined {
            if !out.contains(id) {
                out.push(id.clone());
            }
        }
        out
    }

    /// Whether this peer's allocation is due at `now`: downlink control
    /// is on and it last ran [`DOWNLINK_ALLOCATION_INTERVAL`] ago, never,
    /// or before the peer's last `video-wants`.
    #[must_use]
    pub fn downlink_due(&self, now: Instant) -> bool {
        self.downlink.as_ref().is_some_and(|downlink| {
            !downlink.outcomes.is_empty()
                || downlink.last_run.map_or(true, |at| {
                    now.saturating_duration_since(at) >= DOWNLINK_ALLOCATION_INTERVAL
                })
        })
    }

    /// Run this (subscribing) peer's allocation at `now` (ADR-0070 step
    /// 6) over `publishers`, which the caller lists in the peer's order
    /// of importance ([`Self::importance_order`]) with what each asked-for
    /// quality and track costs.
    ///
    /// - The estimate's ceiling follows what the SFU would like to send.
    /// - A probe that has ended is judged: one the path carried raises
    ///   the estimate to its rate and vouches for that rate for
    ///   [`DOWNLINK_PROVEN_FOR`].
    /// - The allocation runs on the estimate and on what the path has
    ///   been seen to carry ([`crate::downlink_allocation`]). What each
    ///   publisher is given replaces what the peer asked for in the
    ///   per-track gate at once ([`Self::video_quality_given`]): a track
    ///   given up is paused with the next packet, a track given back
    ///   resumes at a key frame its publisher is asked for by
    ///   [`drive_video_wants`].
    /// - Before that, the limit follows what the set being given has come
    ///   to cost on a quiet path ([`Downlink::follow_what_is_carried`]);
    ///   after it, a set that the path took away again soon after it was
    ///   given is noted as too large
    ///   ([`Downlink::note_given_and_taken`]).
    /// - If someone is still given less than asked for, the next larger
    ///   set is tried on the path: a probe starts toward what that set
    ///   needs, when one may ([`crate::downlink_probe`]).
    /// - The set to tell the peer lists every publisher it has a video
    ///   track of, at most [`MAX_VIDEO_GIVEN`], most important first.
    ///
    /// A no-op, returning an empty run, without downlink control.
    pub fn run_downlink(
        &mut self,
        publishers: &[downlink_allocation::Publisher<MediaPeerId>],
        now: Instant,
    ) -> DownlinkRun {
        let Some(downlink) = self.downlink.as_mut() else {
            return DownlinkRun::default();
        };
        downlink.last_run = Some(now);

        downlink.follow_demand(downlink_allocation::demand_bps(publishers), now);

        // A probe that can be judged now. (One ended by trouble was filed
        // when the trouble was seen.)
        if let Some(outcome) = downlink.probe.poll(now) {
            downlink.outcomes.push(outcome);
        }
        let outcomes = downlink.judge_probes(now);

        // What is being given now, as the allocation will take it: a
        // publisher it has not met starts at its light track.
        let lowered = std::mem::take(&mut downlink.lowered_since_run);
        downlink.follow_what_is_carried(downlink.wire_now(publishers), lowered, now);

        let budget_bps = downlink
            .estimator
            .has_arrivals()
            .then(|| downlink.estimator.estimate_bps());
        let carried_bps = downlink.carried_bps(now);
        downlink.note_steady(now);
        downlink.budget_bps = budget_bps;
        downlink.carried_bps = carried_bps;
        let proven = downlink
            .proven
            .is_some_and(|(_, at)| now.saturating_duration_since(at) <= DOWNLINK_PROVEN_FOR);

        let given = downlink.allocation.allocate_within(
            budget_bps,
            carried_bps,
            downlink.probe.held_from(now),
            publishers,
        );
        let why_up = if proven { "probe" } else { "quiet-path" };
        let why_down = if lowered {
            "path-cut"
        } else if downlink.allocation.over_budget() {
            "over-budget"
        } else if downlink.allocation.reordered() {
            "importance"
        } else {
            "other"
        };
        let mut changes = Vec::new();
        let mut set: Vec<VideoGiven> = Vec::new();
        let mut regate = false;
        for (publisher, entry) in publishers.iter().zip(&given) {
            let before = downlink
                .given
                .insert(entry.publisher.clone(), entry.given)
                .unwrap_or(publisher.wanted);
            regate |= before != entry.given;
            changes.extend(given_change(publisher.wanted, entry, why_up, why_down));
            let has_video = publisher.main_bps.is_some()
                || publisher.middle_bps.is_some()
                || publisher.light_bps.is_some();
            if has_video
                && set.len() < MAX_VIDEO_GIVEN
                && !set.iter().any(|g| g.peer == entry.publisher.as_str())
            {
                set.push(VideoGiven {
                    peer: entry.publisher.to_string(),
                    quality: entry.given,
                });
            }
        }
        downlink
            .given
            .retain(|id, _| given.iter().any(|g| g.publisher == *id));

        let qualities: Vec<VideoQuality> = given.iter().map(|g| g.given).collect();
        let wire_bps = downlink_allocation::wire_bps(publishers, &qualities);
        let need_bps = downlink_allocation::need_bps(publishers, &qualities);

        let level = downlink_allocation::set_level(&qualities);
        let too_large = downlink.note_given_and_taken(&changes, (level, wire_bps), budget_bps, now);
        let queue_ms = downlink
            .standing_queue()
            .map(|queue| u64::try_from(queue.as_millis()).unwrap_or(u64::MAX));

        let probe_started_bps = if budget_bps.is_some() {
            downlink.start_probe(publishers, &qualities, now)
        } else {
            None
        };

        let tell = (downlink.tell || downlink.told.as_ref() != Some(&set)).then(|| {
            downlink.tell = false;
            downlink.told = Some(set.clone());
            set
        });
        let cuts = std::mem::take(&mut downlink.cut_notes);
        if regate {
            self.apply_video_wants();
        }
        DownlinkRun {
            budget_bps,
            carried_bps,
            probe_started_bps,
            outcomes,
            cuts,
            changes,
            tell,
            wire_bps,
            need_bps,
            queue_ms,
            too_large,
        }
    }

    /// This (subscribing) peer's downlink for the diagnostics line;
    /// `None` without downlink control.
    #[must_use]
    pub fn downlink_snapshot(&self, now: Instant) -> Option<DownlinkSnapshot> {
        let downlink = self.downlink.as_ref()?;
        let mut snap = DownlinkSnapshot {
            budget_bps: downlink.budget_bps,
            estimate_bps: Some(downlink.estimator.estimate_bps()),
            carried_bps: downlink.carried_bps,
            sending_bps: downlink.sent.bitrate_bps_at(now),
            demand_bps: downlink.demand_bps,
            probing_bps: downlink.probe.target_bps(),
            probes_started: downlink.probes_started,
            probes_carried: downlink.probes_carried,
            padding_bytes: downlink.padding_bytes,
            cuts: downlink.estimator.cuts(),
            queue_ms: downlink
                .standing_queue()
                .map(|queue| u64::try_from(queue.as_millis()).unwrap_or(u64::MAX)),
            detector: downlink.estimator.state().as_str(),
            limit_bps: downlink.limit_bps,
            held_from_level: downlink.probe.held_from(now),
            too_large_wait_secs: downlink.probe.too_large_wait().as_secs(),
            followed: downlink.followed,
            ..DownlinkSnapshot::default()
        };
        for quality in downlink.given.values() {
            match quality {
                VideoQuality::Main => snap.main += 1,
                VideoQuality::Middle => snap.middle += 1,
                VideoQuality::Light => snap.light += 1,
            }
        }
        Some(snap)
    }

    /// Hand one incoming simulcast video packet to the uplink fallback
    /// policy of its track ([`crate::uplink_fallback`]): its layer, its
    /// extended sequence number and when it arrived. Header fields only.
    ///
    /// A no-op for audio and for a packet without a rid, so a publisher
    /// that does not send simulcast never gets a policy. Retransmissions
    /// (str0m hands them over as the packet they repair), packets rebuilt
    /// from FEC and FEC packets themselves are all fed: each takes a
    /// sequence number of the layer's stream, and the first two are what
    /// closes a hole.
    pub(crate) fn observe_uplink_holes(&mut self, packet: &RelayPacket) {
        if !self.uplink_fallback_on || packet.kind != MediaKind::Video {
            return;
        }
        let Some(rid) = packet.rid else {
            return;
        };
        if !self.uplink_fallbacks.contains_key(&packet.mid)
            && self.uplink_fallbacks.len() >= MAX_FALLBACK_TRACKS
        {
            self.uplink_fallbacks.clear();
        }
        self.uplink_fallbacks
            .entry(packet.mid)
            .or_default()
            .on_packet(rid, *packet.seq_no, packet.arrival);
    }

    /// The uplink fallback state of the track `mid` this peer publishes,
    /// as a label for a log line: `full` or `fallback`. `None` when the
    /// track has no policy (not simulcast, or the feature is off).
    #[must_use]
    pub fn uplink_fallback_state(&self, mid: Mid) -> Option<&'static str> {
        self.uplink_fallbacks.get(&mid).map(|fb| match fb.state() {
            uplink_fallback::State::Full => "full",
            uplink_fallback::State::Fallback { .. } => "fallback",
        })
    }

    /// The only layer subscribers of the track `mid` this peer publishes
    /// may be on right now, when its uplink fallback is in effect.
    #[must_use]
    pub fn uplink_fallback_layer(&self, mid: Mid) -> Option<Rid> {
        self.uplink_fallbacks
            .get(&mid)
            .and_then(UplinkFallback::restricted_to)
    }

    /// Take a str0m `Event::EgressBitrateEstimate` for THIS (subscribing)
    /// peer. Only a send-side estimate becomes the peer's budget for
    /// simulcast layer selection; a REMB from the subscriber is not one and
    /// changes nothing (see [`bwe_kind_bps`]).
    pub fn note_egress_estimate(&mut self, kind: &BweKind) {
        if let Some(bps) = bwe_kind_bps(kind) {
            self.set_egress_estimate_bps(bps);
        }
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
    /// layer `(mid, rid)`. Called when a layer is selected for a subscriber:
    /// the switch waits for an independent frame on that layer, and this is
    /// what makes one arrive now rather than at the next natural keyframe.
    /// Best-effort: a not-yet-established stream (no `StreamRx` for the
    /// midrid) is a benign no-op. The SFU only asks for a keyframe on the
    /// plaintext RTP layer — it never touches the SFrame-encrypted payload
    /// (C-39).
    pub fn request_layer_keyframe(&mut self, mid: Mid, rid: Rid) {
        if let Some(stream_rx) = self.rtc.direct_api().stream_rx_by_mid(mid, Some(rid)) {
            stream_rx.request_keyframe(KeyframeRequestKind::Pli);
        }
    }

    /// As [`Self::request_layer_keyframe`], additionally entering the
    /// request in the rate limit's books at `now`. The request itself is
    /// not limited: it goes out whatever was asked before. What the entry
    /// buys is that a repeat through [`Self::request_keyframe_limited`]
    /// (the uplink fallback asks again while a switch is still pending)
    /// waits a full [`KEYFRAME_REQUEST_MIN_INTERVAL`] from this one.
    /// Returns whether there was a stream to ask.
    pub fn request_layer_keyframe_at(&mut self, mid: Mid, rid: Rid, now: Instant) -> bool {
        let asked = self
            .rtc
            .direct_api()
            .stream_rx_by_mid(mid, Some(rid))
            .is_some_and(|stream_rx| {
                stream_rx.request_keyframe(KeyframeRequestKind::Pli);
                true
            });
        if !asked {
            return false;
        }
        self.last_keyframe_request.insert((mid, Some(rid)), now);
        self.owed_keyframe_requests.remove(&(mid, Some(rid)));
        true
    }

    /// Ask THIS (publishing) peer for a keyframe on `(mid, rid)`, at most once
    /// per [`KEYFRAME_REQUEST_MIN_INTERVAL`] per track. Returns whether a PLI
    /// actually went out, so the caller logs only real requests.
    ///
    /// `rid == None` on a simulcast source means the subscriber has not
    /// latched a layer yet (it latches on the first packet it sees, which is
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
        self.request_keyframe(mid, rid, now, false)
    }

    /// As [`Self::request_keyframe_limited`]; `for_subscriber` says the
    /// request is a subscriber's own, relayed or owed, which the bound
    /// on unanswered requests does not apply to
    /// ([`KEYFRAME_UNANSWERED_MAX`]).
    fn request_keyframe(
        &mut self,
        mid: Mid,
        rid: Option<Rid>,
        now: Instant,
        for_subscriber: bool,
    ) -> bool {
        let key = (mid, rid);
        let last = self.last_keyframe_request.get(&key).copied();
        // A track that has been asked and asked and shows no key frame
        // (no frame descriptor to see one by, or a sender that sends
        // nothing) is asked rarely, and what comes in between is dropped:
        // owing it would only move the same request to the end of the
        // wait.
        let unanswered = if for_subscriber {
            0
        } else {
            self.unanswered_keyframe_requests
                .get(&key)
                .copied()
                .unwrap_or(0)
        };
        match keyframe_ask(last, unanswered, now) {
            KeyframeAsk::Drop => {
                self.owed_keyframe_requests.remove(&key);
                return false;
            },
            KeyframeAsk::Owe => {
                // Inside the window: owed, not dropped.
                self.owed_keyframe_requests.insert(key);
                return false;
            },
            KeyframeAsk::Send => {},
        }
        self.owed_keyframe_requests.remove(&key);
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
            let count = self.unanswered_keyframe_requests.entry(key).or_insert(0);
            *count = count.saturating_add(1);
        }
        sent
    }

    /// A key frame was seen arriving on `(mid, rid)` of THIS
    /// (publishing) peer: every request for one on that track is
    /// answered. What was owed is dropped (the key frame goes to every
    /// subscriber the track is being sent to, the one that asked
    /// included), and the count of unanswered requests starts again.
    fn settle_keyframe_requests(&mut self, mid: Mid, rid: Option<Rid>) {
        if self.owed_keyframe_requests.is_empty() && self.unanswered_keyframe_requests.is_empty() {
            return;
        }
        for key in [(mid, rid), (mid, None)] {
            self.owed_keyframe_requests.remove(&key);
            self.unanswered_keyframe_requests.remove(&key);
        }
    }

    /// THIS (subscribing) peer has no decodable frame on its downstream
    /// track `local_mid` and is asking for one. The SFU holds no media,
    /// so only the publisher can answer: the publisher track to ask, or
    /// `None` when the request is not relayed. It is not when the track
    /// is not open, when the subscriber is not being sent it (ADR-0070
    /// step 5: a key frame of it would reach nobody), and when the
    /// subscriber goes on asking although key frames are being sent to
    /// it ([`PLI_RELAY_FIRST_INTERVAL`]).
    pub(crate) fn subscriber_keyframe_request(
        &mut self,
        local_mid: Mid,
        now: Instant,
    ) -> Option<OpenedTrack> {
        let target = self.origin_for_downstream_mid(local_mid)?;
        if self.downstream_paused(local_mid) || !self.subscriber_pli_due(local_mid, now) {
            return None;
        }
        Some(target)
    }

    /// Whether THIS (subscribing) peer's key frame request on its own
    /// downstream mid `local_mid` at `now` is relayed to the publisher
    /// ([`PLI_RELAY_FIRST_INTERVAL`]); the request is counted when it is.
    pub(crate) fn subscriber_pli_due(&mut self, local_mid: Mid, now: Instant) -> bool {
        let (due, state) = pli_relay_decision(self.pli_relays.get(&local_mid).copied(), now);
        self.pli_relays.insert(local_mid, state);
        due
    }

    /// Requests sent on `(mid, rid)` of THIS (publishing) peer since a
    /// key frame was last seen arriving on it.
    #[must_use]
    pub fn unanswered_keyframe_requests(&self, mid: Mid, rid: Option<Rid>) -> u32 {
        self.unanswered_keyframe_requests
            .get(&(mid, rid))
            .copied()
            .unwrap_or(0)
    }

    /// Send every keyframe request that was held back by the rate limit and
    /// whose window has now ended. Returns the `(mid, rid)` of each request
    /// that went out, for the caller's log line.
    pub fn send_owed_keyframe_requests(&mut self, now: Instant) -> Vec<(Mid, Option<Rid>)> {
        if self.owed_keyframe_requests.is_empty() {
            return Vec::new();
        }
        let due: Vec<(Mid, Option<Rid>)> = self
            .owed_keyframe_requests
            .iter()
            .copied()
            .filter(|key| {
                keyframe_request_allowed(self.last_keyframe_request.get(key).copied(), now)
            })
            .collect();
        let mut sent = Vec::new();
        for (mid, rid) in due {
            // Removed whether or not it goes out: a stream that is gone has
            // nothing to ask.
            self.owed_keyframe_requests.remove(&(mid, rid));
            // What is owed passed the bound when it was filed.
            if self.request_keyframe(mid, rid, now, true) {
                sent.push((mid, rid));
            }
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
                role: e.role,
                rid: e.chosen_rid,
            })
    }
}

/// Simulcast layer gate for one downstream track.
///
/// Decides whether a packet of layer `packet_rid` is forwarded. `chosen` is
/// the layer selected for the track and `active` the layer on the wire; both
/// are updated in place.
///
/// - `packet_rid == None` — the source is not simulcast, so always forward.
///   Neither value is touched.
/// - Nothing selected yet (`chosen == None`) — the packet's layer is latched
///   as the selection. This is the first-layer latch: a subscriber gets
///   whichever layer reaches the SFU first.
/// - Nothing on the wire yet (`active == None`) — a packet of the selected
///   layer starts the stream at once; any other layer is dropped. There is
///   no earlier picture to protect, so there is nothing to wait for (the
///   publisher was asked for a keyframe when the track opened).
/// - The packet is of the layer on the wire — forward, even while another
///   layer is selected: the old layer keeps flowing until the switch.
/// - The packet is of the selected layer and that is not yet the layer on
///   the wire — the switch happens here if and only if the packet begins an
///   independent frame (`starts_independent_frame`: the first-packet bit and
///   the no-dependencies flag of the generic frame descriptor). Otherwise it
///   is dropped. A stream that carries no frame descriptor therefore never
///   becomes a switch target; no Lakko client can decode such a stream
///   anyway, since SFrame has replaced the bitstream bytes the key-frame flag
///   would otherwise be read from. [`MediaPeer::forward_packet`] does not
///   bring such a packet here: it holds the selected layer's packets and
///   switches once a key frame is whole ([`SwitchWait`]), which is this
///   rule with the further condition that the rest of the frame has
///   arrived. The arm stays as the rule for a caller without that state.
/// - Any other layer — dropped.
///
/// A policy selects a layer through [`MediaPeer::select_layer`].
fn layer_gate_allows(
    chosen: &mut Option<Rid>,
    active: &mut Option<Rid>,
    packet_rid: Option<Rid>,
    starts_independent_frame: bool,
) -> bool {
    let Some(rid) = packet_rid else {
        return true;
    };
    let selected = *chosen.get_or_insert(rid);
    match *active {
        Some(on_wire) if on_wire == rid => true,
        Some(_) if selected == rid && starts_independent_frame => {
            *active = Some(rid);
            true
        },
        None if selected == rid => {
            *active = Some(rid);
            true
        },
        Some(_) | None => false,
    }
}

/// Extract, from a str0m [`BweKind`], an estimate in bits/second of what the
/// path TO a subscriber carries. `None` when the event is not one, and the
/// caller then keeps what it had.
///
/// Only the send-side estimate ([`BweKind::Twcc`]) is one. A **REMB** is
/// not, and is ignored: it is the subscriber's receive-side figure, and a
/// libwebrtc receiver does not let it rise far above the rate that is
/// actually arriving (about one and a half times it, from memory of its
/// AIMD rate control; not re-read for this change). Taken as the budget for
/// simulcast layer selection it confirms itself: once a subscriber receives
/// little, the figure says little fits, the main track is selected to the
/// light layer, and from then on the figure can never grow past what the
/// light layer brings, so the track stays there while the heavy layer
/// arrives at the SFU unused. That was seen on a live call on 2026-10-04
/// (`role="main" sent=161 wrong_layer=1427` per five seconds), under
/// server-side bitrate control, where no transport-wide feedback is
/// negotiated and a browser therefore sends REMB.
///
/// str0m raises the REMB event whether or not its own estimator is enabled,
/// so "send-side estimation is off" never meant "no estimate arrives".
/// An unrecognised (`#[non_exhaustive]`) future kind is ignored likewise.
fn bwe_kind_bps(kind: &BweKind) -> Option<u64> {
    match kind {
        BweKind::Twcc(b) => Some(bitrate_bps(*b)),
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
    /// Rate of the completed window before that one.
    before_bps: u64,
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
            before_bps: 0,
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
            self.before_bps = self.last_bps;
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

    /// As [`Self::bitrate_bps_at`], but a meter that has not completed a
    /// window yet answers with the rate of what it has seen so far, once
    /// that is a quarter of a window: a track's cost is known a quarter
    /// of a second after it starts, not a second.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    fn bitrate_bps_early_at(&self, now: Instant) -> u64 {
        let elapsed = now.saturating_duration_since(self.window_start);
        if self.last_bps == 0 && elapsed >= self.window / 4 && elapsed <= self.window * 2 {
            return ((self.bytes as f64 * 8.0) / elapsed.as_secs_f64()) as u64;
        }
        self.bitrate_bps_at(now)
    }

    /// What a track costs, for a rule that gives and takes tracks by it:
    /// the mean of the last two completed windows. A publisher whose
    /// frames stall and then arrive together has one window short of
    /// them and the next one over; either alone would make a larger set
    /// look affordable for a second, or the set being given look too
    /// dear (seen in the e2e clump test: a probe toward two mains at
    /// 7.1 Mbit/s instead of 9.0, started in the stall, and its padding
    /// on top of the clump overflowed the link's queue by two packets).
    fn bitrate_bps_cost_at(&self, now: Instant) -> u64 {
        if self.last_bps == 0 || self.before_bps == 0 {
            return self.bitrate_bps_early_at(now);
        }
        if now.saturating_duration_since(self.window_start) > self.window * 2 {
            return 0;
        }
        self.last_bps / 2 + self.before_bps / 2
    }

    /// As [`Self::bitrate_bps`], and zero once nothing has been recorded
    /// for two windows: a meter only moves when it is fed, so a stream
    /// that stopped would otherwise keep its last rate for good.
    fn bitrate_bps_at(&self, now: Instant) -> u64 {
        if now.saturating_duration_since(self.window_start) > self.window * 2 {
            0
        } else {
            self.last_bps
        }
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
/// `fanout_packets_into`, driven from `run_media_loop`
/// over the UDP socket [`spawn_media_task`] owns.
#[derive(Debug, Default)]
pub struct MediaRouter {
    peers: Vec<MediaPeer>,
    /// str0m statistics cadence handed to every peer this router
    /// creates lazily; `None` (the default) keeps statistics off.
    stats_interval: Option<Duration>,
    /// Whether peers this router creates may negotiate video FEC
    /// (`LAKKO_SFU_VIDEO_FEC`); `false` by default.
    video_fec: bool,
    /// Who decides the publishing bitrate of peers this router creates
    /// (`LAKKO_SFU_BITRATE_CONTROL`); the client by default.
    bitrate_control: BitrateControl,
    /// Whether peers this router creates run the uplink fallback
    /// (`LAKKO_SFU_UPLINK_FALLBACK`); `false` by default.
    uplink_fallback: bool,
    /// What peers this router creates do to carry video through a burst
    /// of loss (ADR-0070 steps 3 and 4); everything on by default.
    protection: LossProtection,
    /// Whether peers this router creates run downlink control (ADR-0070
    /// step 6, `LAKKO_SFU_DOWNLINK_CONTROL`); `false` until set, and
    /// without effect unless the SFU also decides publishers' bitrate.
    downlink_control: bool,
    /// When [`drive_video_needed`] last ran; `None` makes it run at the
    /// next pass whatever the time.
    video_needed_ran: Option<Instant>,
    /// When [`drive_publish_budget`] last ran; `None` makes it run at the
    /// next pass whatever the time.
    publish_budget_ran: Option<Instant>,
}

impl MediaRouter {
    /// Construct an empty router. No call is bound at
    /// this point — the admission path adds peers via
    /// [`Self::add_peer`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty router whose peers are created as the media task was
    /// switched on: `options` and who decides a publisher's bitrate.
    #[must_use]
    pub fn configured(options: MediaTaskOptions, bitrate: BitrateControl) -> Self {
        let mut router = Self::new();
        router.set_stats_interval(options.diagnostics);
        router.set_video_fec(options.video_fec);
        router.set_uplink_fallback(options.uplink_fallback);
        router.set_protection(options.protection);
        router.set_bitrate_control(bitrate);
        router.set_downlink_control(options.downlink_control);
        router
    }

    /// Ask str0m for per-stream statistics on every peer created
    /// from here on (see [`MediaPeer::with_stats_interval`]).
    pub fn set_stats_interval(&mut self, interval: Option<Duration>) {
        self.stats_interval = interval;
    }

    /// Let every peer created from here on negotiate video FEC
    /// (see [`MediaPeer::with_options`]).
    pub fn set_video_fec(&mut self, enabled: bool) {
        self.video_fec = enabled;
    }

    /// Choose who decides the publishing bitrate of every peer
    /// created from here on (see [`MediaPeer::with_bitrate_control`]).
    pub fn set_bitrate_control(&mut self, bitrate: BitrateControl) {
        self.bitrate_control = bitrate;
    }

    /// Switch the uplink fallback on (or back off) for every peer
    /// created from here on (see [`MediaPeer::set_uplink_fallback`]).
    pub fn set_uplink_fallback(&mut self, on: bool) {
        self.uplink_fallback = on;
    }

    /// Choose what every peer created from here on does to carry video
    /// through a burst of loss (see [`MediaPeer::with_protection`]).
    pub fn set_protection(&mut self, protection: LossProtection) {
        self.protection = protection;
    }

    /// Switch downlink control on (or back off) for every peer created
    /// from here on (see [`MediaPeer::with_downlink_control`]).
    pub fn set_downlink_control(&mut self, on: bool) {
        self.downlink_control = on;
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
            let mut peer = MediaPeer::with_downlink_control(
                id,
                now,
                self.stats_interval,
                self.video_fec,
                self.bitrate_control,
                self.protection,
                self.downlink_control,
            );
            peer.set_uplink_fallback(self.uplink_fallback);
            self.peers.push(peer);
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
        /// The mids the offer declares as carrying a light copy of
        /// the peer's camera (`ClientFrame::SdpOffer.light_mids`).
        light_mids: Vec<String>,
        /// The mids the offer declares as carrying the middle copy
        /// (`ClientFrame::SdpOffer.middle_mids`).
        middle_mids: Vec<String>,
        /// One-shot reply channel.
        reply: tokio::sync::oneshot::Sender<MediaResult<String>>,
    },
    /// The pictures a subscriber shows (`ClientFrame::VideoWants`,
    /// ADR-0070 step 5): the whole set, replacing the one before.
    /// No reply: a set for a peer without media state is dropped.
    VideoWants {
        /// The subscribing peer.
        peer_id: MediaPeerId,
        /// For each publisher named, which of its video tracks.
        wants: Vec<VideoWant>,
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
    /// every packet was fanned out to it, and every joiner was
    /// backfilled against it (five such ghosts sat in one live
    /// four-person call on 2026-09-19). No reply: the caller has
    /// nothing to do with a failure but log it.
    RemovePeer {
        /// The departed peer.
        peer_id: MediaPeerId,
    },
    /// Read one subscriber's downlink as the diagnostics line shows it
    /// ([`MediaPeer::downlink_snapshot`], ADR-0070 step 6). Replies
    /// `None` for a peer without media state or without downlink control.
    /// Changes nothing.
    DownlinkSnapshot {
        /// The subscribing peer.
        peer_id: MediaPeerId,
        /// One-shot reply channel.
        reply: tokio::sync::oneshot::Sender<Option<DownlinkSnapshot>>,
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
        self.accept_offer_with_light(peer_id, call_id, offer_sdp, Vec::new())
            .await
    }

    /// As [`Self::accept_offer`], for an offer that declares some of
    /// its m-lines to carry a light copy of the peer's camera
    /// (`ClientFrame::SdpOffer.light_mids`, see
    /// [`MediaPeer::declare_light_tracks`]). An empty list is exactly
    /// [`Self::accept_offer`].
    ///
    /// # Errors
    ///
    /// As [`Self::accept_offer`]. A declaration the SFU does not take
    /// is not an error: it is ignored and logged.
    pub async fn accept_offer_with_light(
        &self,
        peer_id: MediaPeerId,
        call_id: String,
        offer_sdp: String,
        light_mids: Vec<String>,
    ) -> std::result::Result<String, MediaCommandError> {
        self.accept_offer_with_copies(peer_id, call_id, offer_sdp, light_mids, Vec::new())
            .await
    }

    /// As [`Self::accept_offer_with_light`], for an offer that also
    /// declares the m-line carrying the middle copy of the peer's
    /// camera (`ClientFrame::SdpOffer.middle_mids`, see
    /// [`MediaPeer::declare_middle_tracks`]).
    ///
    /// # Errors
    ///
    /// As [`Self::accept_offer`]. A declaration the SFU does not take
    /// is not an error: it is ignored and logged.
    pub async fn accept_offer_with_copies(
        &self,
        peer_id: MediaPeerId,
        call_id: String,
        offer_sdp: String,
        light_mids: Vec<String>,
        middle_mids: Vec<String>,
    ) -> std::result::Result<String, MediaCommandError> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.tx
            .try_send(MediaCommand::AcceptOffer {
                peer_id,
                call_id,
                offer_sdp,
                light_mids,
                middle_mids,
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

    /// Send a [`VideoWants`](MediaCommand::VideoWants): the pictures
    /// `peer_id` shows, replacing the set before. Fire and forget, as
    /// [`Self::remove_peer`] is.
    ///
    /// # Errors
    ///
    /// [`MediaCommandError::Backpressure`] or
    /// [`MediaCommandError::TaskGone`].
    pub fn set_video_wants(
        &self,
        peer_id: MediaPeerId,
        wants: Vec<VideoWant>,
    ) -> std::result::Result<(), MediaCommandError> {
        self.tx
            .try_send(MediaCommand::VideoWants { peer_id, wants })
            .map_err(|e| match e {
                tokio::sync::mpsc::error::TrySendError::Full(_) => MediaCommandError::Backpressure,
                tokio::sync::mpsc::error::TrySendError::Closed(_) => MediaCommandError::TaskGone,
            })
    }

    /// Read `peer_id`'s downlink
    /// ([`DownlinkSnapshot`](MediaCommand::DownlinkSnapshot)): its budget,
    /// what it is being sent and how many publishers are given each
    /// quality. `None` for a peer without media state or without downlink
    /// control.
    ///
    /// # Errors
    ///
    /// [`MediaCommandError::Backpressure`] or
    /// [`MediaCommandError::TaskGone`].
    pub async fn downlink_snapshot(
        &self,
        peer_id: MediaPeerId,
    ) -> std::result::Result<Option<DownlinkSnapshot>, MediaCommandError> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.tx
            .try_send(MediaCommand::DownlinkSnapshot {
                peer_id,
                reply: reply_tx,
            })
            .map_err(|e| match e {
                tokio::sync::mpsc::error::TrySendError::Full(_) => MediaCommandError::Backpressure,
                tokio::sync::mpsc::error::TrySendError::Closed(_) => MediaCommandError::TaskGone,
            })?;
        reply_rx.await.map_err(|_| MediaCommandError::TaskGone)
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
/// On every pass the task drives each peer's clock
/// (`Input::Timeout`), drains `str0m::Rtc::poll_output`
/// on every peer and writes any `Output::Transmit`
/// packets back to the socket. Dead peers (those whose
/// `Rtc::is_alive` flipped to `false` after DTLS/ICE
/// failure) are pruned at the end of each tick.
///
/// `Event::RtpPacket` events are forwarded between
/// peers via `fanout_packets_into` (ADR-0066).
/// `Event::MediaAdded` events get registered
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
    let options = MediaTaskOptions {
        diagnostics,
        video_fec: false,
        uplink_fallback: false,
        protection: LossProtection::default(),
        downlink_control: false,
    };
    spawn_media_task_with_options(bind_addr, advertise_ip, peers, options).await
}

/// What the media task is switched on with, beyond its addresses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MediaTaskOptions {
    /// Diagnostics cadence (`LAKKO_SFU_MEDIA_DIAG_SECS`); `None` is off.
    pub diagnostics: Option<Duration>,
    /// Whether video FEC (`red/90000` + `ulpfec/90000`) is negotiated and
    /// forwarded (`LAKKO_SFU_VIDEO_FEC`). `false` is the behaviour without
    /// the feature. See [`crate::video_fec`].
    pub video_fec: bool,
    /// Switch the uplink fallback on (`LAKKO_SFU_UPLINK_FALLBACK`).
    /// `false`, the default, leaves it off. On, the main track of every
    /// subscriber of a simulcast video track is moved to the light layer
    /// while the publisher's uplink is in trouble. It costs resolution
    /// and was measured not to shorten the stall; a subscriber has the
    /// light layer all the time on the light companion track instead
    /// ([`TrackRole`]). See [`crate::uplink_fallback`].
    pub uplink_fallback: bool,
    /// What the SFU does to carry video through a burst of loss
    /// (ADR-0070 steps 3 and 4: `LAKKO_SFU_LIGHT_DUPLICATE`,
    /// `LAKKO_SFU_PLAYOUT_DELAY`). The default is everything on.
    pub protection: LossProtection,
    /// Estimate each subscriber's downlink and give up the larger tracks
    /// first when it is short (ADR-0070 step 6,
    /// `LAKKO_SFU_DOWNLINK_CONTROL`). `false` is the behaviour without
    /// the feature. It takes effect only with
    /// [`BitrateControl::Server`]; see
    /// [`MediaPeer::with_downlink_control`].
    pub downlink_control: bool,
}

/// As [`spawn_media_task_advertising`], with the options of the media
/// task given explicitly and the client deciding its own bitrate.
///
/// # Errors
///
/// As [`spawn_media_task_advertising`].
pub async fn spawn_media_task_with_options(
    bind_addr: SocketAddr,
    advertise_ip: Option<IpAddr>,
    peers: PeerRegistry,
    options: MediaTaskOptions,
) -> MediaResult<(MediaTaskHandle, SocketAddr)> {
    spawn_media_task_with_bitrate_control(
        bind_addr,
        advertise_ip,
        peers,
        options,
        BitrateControl::Client,
    )
    .await
}

/// As [`spawn_media_task_with_options`], additionally choosing who decides
/// a publisher's bitrate (`LAKKO_SFU_BITRATE_CONTROL`, ADR-0068). The
/// production entry point; [`BitrateControl::Client`] is exactly
/// [`spawn_media_task_with_options`].
///
/// # Errors
///
/// As [`spawn_media_task_advertising`].
pub async fn spawn_media_task_with_bitrate_control(
    bind_addr: SocketAddr,
    advertise_ip: Option<IpAddr>,
    peers: PeerRegistry,
    options: MediaTaskOptions,
    bitrate: BitrateControl,
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
        options,
        bitrate,
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
            light_mids,
            middle_mids,
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
            // The offer's m-lines exist now and none of the new ones has
            // been announced yet (str0m announces a track at the next
            // poll), so this is the moment a declared light track is
            // told from a main one.
            if res.is_ok() {
                declare_light_tracks(peer, &light_mids);
                declare_middle_tracks(peer, &middle_mids);
            }
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
                    request_keyframes_for_opened(router, &peer_id, opened, Instant::now());
                },
                Err(e) => tracing::warn!(
                    peer = %peer_id,
                    error = %e,
                    "media: subscriber's answer to the downstream-track offer was refused"
                ),
            }
            let _ = reply.send(res.map(|_| ()));
        },
        MediaCommand::VideoWants { peer_id, wants } => {
            take_video_wants(router, &peer_id, &wants);
        },
        MediaCommand::DownlinkSnapshot { peer_id, reply } => {
            let snapshot = router
                .peer_mut(&peer_id)
                .and_then(|peer| peer.downlink_snapshot(Instant::now()));
            let _ = reply.send(snapshot);
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

/// Hand the `light_mids` of an offer `peer` sent, and the SFU accepted, to
/// the peer ([`MediaPeer::declare_light_tracks`]), and log what came of it:
/// one warning per mid that was ignored, with the reason, and one line
/// naming the peer's light track.
fn declare_light_tracks(peer: &mut MediaPeer, light_mids: &[String]) {
    if light_mids.is_empty() {
        return;
    }
    for (mid, why) in peer.declare_light_tracks(light_mids) {
        tracing::warn!(
            peer = %peer.id(),
            mid = ?mid,
            reason = why.as_str(),
            "media: light-track declaration ignored"
        );
    }
    tracing::info!(
        peer = %peer.id(),
        declared = light_mids.len(),
        light_mid = ?peer.light_mid(),
        "media: offer declared light tracks"
    );
}

/// Hand a subscriber's video wants to its peer
/// ([`MediaPeer::set_video_wants`]), log the change in one line, and ask
/// the publisher of every track that is wanted again for a key frame now
/// rather than at the next pass. A set from a peer without media state is
/// dropped.
fn take_video_wants(router: &mut MediaRouter, subscriber: &MediaPeerId, wants: &[VideoWant]) {
    let Some(peer) = router.peer_mut(subscriber) else {
        tracing::debug!(
            peer = %subscriber,
            "media: video wants from a peer without media state"
        );
        return;
    };
    let count = peer.set_video_wants(wants);
    tracing::info!(
        subscriber = %subscriber,
        named = wants.len(),
        main = count.main,
        middle = count.middle,
        light = count.light,
        "media: subscriber's video wants changed"
    );
    // A sender that is wanted again is told so at the next pass.
    router.video_needed_ran = None;
    drive_video_wants(router, Instant::now());
}

/// As [`declare_light_tracks`], for the `middle_mids` of the offer
/// ([`MediaPeer::declare_middle_tracks`]).
fn declare_middle_tracks(peer: &mut MediaPeer, middle_mids: &[String]) {
    if middle_mids.is_empty() {
        return;
    }
    for (mid, why) in peer.declare_middle_tracks(middle_mids) {
        tracing::warn!(
            peer = %peer.id(),
            mid = ?mid,
            reason = why.as_str(),
            "media: middle-track declaration ignored"
        );
    }
    tracing::info!(
        peer = %peer.id(),
        declared = middle_mids.len(),
        middle_mid = ?peer.middle_mid(),
        "media: offer declared middle tracks"
    );
}

/// Ask the publisher of every downstream track that was paused and is
/// wanted again for a key frame ([`MediaPeer::due_resume_keyframes`],
/// ADR-0070 step 5): at once when the wait starts, and again every
/// [`START_KEYFRAME_RETRY`] until the track has started. The request goes
/// through the per-track rate limit; one that falls inside the window is
/// owed and goes out when the window ends. Returns how many requests went
/// out in this pass.
pub(crate) fn drive_video_wants(router: &mut MediaRouter, now: Instant) -> usize {
    // Senders that are switched off on the SFU's word: a track that
    // resumes from one waits without being asked for.
    let idle: Vec<(MediaPeerId, Mid)> = router
        .peers()
        .iter()
        .flat_map(|peer| {
            peer.idle_senders()
                .into_iter()
                .map(|mid| (peer.id().clone(), mid))
        })
        .collect();
    let mut due: Vec<OpenedTrack> = Vec::new();
    for sub in router.peers_mut() {
        due.extend(sub.due_resume_keyframes(now, &idle));
    }
    due.iter()
        .filter(|track| {
            request_keyframe_from_publisher(router, track, now, KeyframeReason::TrackResume)
        })
        .count()
}

/// Ask the publishers of the video tracks that just opened on
/// `subscriber` for a key frame each.
///
/// A track of a simulcast source is first told which layer it carries
/// ([`MediaPeer::apply_simulcast_layout`]). One that then waits for a whole
/// key frame of that layer is asked for by [`drive_two_track_delivery`], in
/// this same pass and again until it has begun, so it is not asked here.
/// Neither is a track the subscriber is not being sent (ADR-0070 step 5):
/// a paused one needs no key frame, and a resuming one is asked for by
/// [`drive_video_wants`].
/// Nor is a track whose sender is idle on the SFU's word
/// ([`crate::video_needed`]).
fn request_keyframes_for_opened(
    router: &mut MediaRouter,
    subscriber: &MediaPeerId,
    opened: &[OpenedTrack],
    now: Instant,
) {
    for track in opened {
        if track.kind != MediaKind::Video {
            continue;
        }
        let not_sent = router
            .peer_mut(subscriber)
            .is_some_and(|sub| sub.track_paused(&track.source_peer, track.source_mid, track.role));
        if not_sent {
            continue;
        }
        // A sender that is switched off on the SFU's word cannot answer;
        // it is asked at its return unless it comes back with a key frame
        // ([`MediaPeer::note_sender_arrival`]).
        let idle = router
            .peer_mut(&track.source_peer)
            .is_some_and(|publisher| publisher.sender_idle(track.source_mid));
        if idle {
            continue;
        }
        let layout = router
            .peer_mut(&track.source_peer)
            .and_then(|p| p.simulcast_layout(track.source_mid));
        if let (Some(layout), Some(sub)) = (layout, router.peer_mut(subscriber)) {
            sub.apply_simulcast_layout(&track.source_peer, track.source_mid, layout);
            if sub.waits_to_start(&track.source_peer, track.source_mid, track.role) {
                continue;
            }
        }
        request_keyframe_from_publisher(router, track, now, KeyframeReason::DownstreamOpened);
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
/// How often [`IngressTap`] writes its counts.
const INGRESS_TAP_INTERVAL: Duration = Duration::from_secs(1);

/// Per-source count of UDP datagrams the media socket received, split by
/// whether any peer's `Rtc` accepted them, written once a second while the
/// diagnostics are on.
///
/// A datagram no peer accepts is dropped without a trace otherwise, so a gap
/// in a publisher's stream cannot be told apart from datagrams that never
/// reached the socket. One line per source address per second answers that:
/// a source whose count falls while it is still listed lost packets before
/// the SFU; a source with `unaccepted` datagrams, or a new source address
/// appearing for the same peer, lost them inside it.
///
/// Counts only; no datagram byte is read here.
struct IngressTap {
    since: Instant,
    counts: HashMap<SocketAddr, (u32, u32)>,
}

impl IngressTap {
    fn new(now: Instant) -> Self {
        Self {
            since: now,
            counts: HashMap::new(),
        }
    }

    fn record(&mut self, source: SocketAddr, accepted: bool, now: Instant) {
        if now.saturating_duration_since(self.since) >= INGRESS_TAP_INTERVAL {
            self.flush(now);
        }
        let entry = self.counts.entry(source).or_insert((0, 0));
        if accepted {
            entry.0 = entry.0.saturating_add(1);
        } else {
            entry.1 = entry.1.saturating_add(1);
        }
    }

    fn flush(&mut self, now: Instant) {
        for (source, (accepted, unaccepted)) in self.counts.drain() {
            tracing::info!(
                %source,
                accepted,
                unaccepted,
                "media: udp datagrams over the last second"
            );
        }
        self.since = now;
    }
}

async fn run_media_loop(
    socket: tokio::net::UdpSocket,
    advertise_addr: SocketAddr,
    mut rx: tokio::sync::mpsc::Receiver<MediaCommand>,
    peers: PeerRegistry,
    options: MediaTaskOptions,
    bitrate: BitrateControl,
) {
    let diagnostics = options.diagnostics;
    let mut router = MediaRouter::configured(options, bitrate);
    // Per-destination fanout tallies, flushed to the log on the
    // diagnostics cadence. Off (no bookkeeping at all) unless the
    // operator asked for it.
    let mut ledger = diagnostics.map(|every| FanoutLedger::new(every, Instant::now()));
    let mut buf = vec![0u8; UDP_RECV_BUF];
    let mut ingress_tap = diagnostics.map(|_| IngressTap::new(Instant::now()));

    loop {
        let now = Instant::now();
        // Bitrate control (ADR-0068, a no-op unless switched on): a REMB
        // that is due is requested before the clock tick below, which is
        // what makes str0m build it, so it leaves in this pass.
        drive_bitrate_control(&mut router, now);
        // Playout delay (ADR-0070 step 4, a no-op when switched off):
        // bursts seen since the last pass raise the delay stamped on the
        // packets forwarded below.
        drive_playout_delay(&mut router, now);
        // A burst of loss on a publisher's video stream that has ended
        // unrepaired: the publisher is asked for a key frame now, before
        // the clock tick below, so the request leaves in this pass.
        drive_burst_keyframes(&mut router, now);
        // Second copies of light companion packets that are due (ADR-0070
        // step 3) are queued as retransmissions before the clock tick, so
        // they leave in this pass.
        let duplicates = send_light_duplicates(&mut router, ledger.as_mut(), now);
        // Drive every peer's clock. str0m is sans-IO: it only moves time
        // when it is handed an input, and it stamps a packet written with
        // `write_rtp` (and builds RTCP, retransmission requests and ICE
        // checks) in the timeout handler. Without this a packet forwarded
        // to a peer would wait in that peer's send queue until the peer
        // itself next sent the SFU a datagram.
        drive_time(&mut router, now);
        // Keyframe requests held back by the rate limit go out as soon as
        // their window has ended; the loop passes at least every
        // `MAX_TICK_INTERVAL`, so "as soon as" is within that.
        send_owed_keyframe_requests(&mut router, now);
        // Capture-then-fanout: drain_outputs sends what is ready and
        // collects `Event::RtpPacket`s with their origin peer ids;
        // fanout_packets_into writes each to every other peer of the call.
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
        let forwarded = fanout_packets_into(&mut router, &summary.pending, ledger.as_mut());
        // FEC packets the SFU built for subscribers in this pass (video FEC
        // only): written, like forwarded media, to send queues.
        let fec_sent = collect_fec_sent(&mut router, ledger.as_mut());
        if let Some(l) = ledger.as_mut() {
            log_diagnostics_if_due(&mut router, l, now);
        }
        // Subscribers' PLIs, resolved during the drain to the publisher
        // track each mirrors, go to the publishers now — rate-limited per
        // track, since a subscriber with no reference frame asks about five
        // times a second until it has one.
        let relayed = relay_keyframe_requests(&mut router, &summary.keyframe_requests, now);
        // Per-receiver simulcast layer selection: match each subscriber's
        // egress bandwidth estimate against the measured per-layer bitrates,
        // (re)select the best-fitting layer, and request a keyframe on any
        // switch for the next tick.
        //
        // The uplink fallback runs first: a publisher whose uplink is in
        // trouble has its subscribers held on its light layer, and the
        // bandwidth policy below leaves those tracks alone.
        let fallback = drive_uplink_fallback(&mut router, now);
        let layer_switches = drive_layer_selection(&mut router);
        record_layer_selection_metrics(&layer_switches);
        // Two-track delivery of simulcast video: tracks that have no
        // layer yet are given theirs, and a track still waiting for a
        // key frame to begin at has its publisher asked for one.
        //
        // Downlink control (ADR-0070 step 6, a no-op when switched off)
        // runs before the two: a track it gives back is asked for its key
        // frame by `drive_video_wants` in this same pass.
        let padding = run_downlink_control(&mut router, &peers, now).await;
        // Senders nobody watches: each publisher is told which of its
        // camera senders somebody is being sent, and a sender that went
        // idle on that word is marked before the resume requests below
        // look at it.
        run_video_needed(&mut router, &peers, now).await;
        // And how much its main and middle sender may send, from the
        // estimate of its uplink (ADR-0068, amendment of 2026-10-11; a
        // no-op unless the SFU decides publishers' bitrate). After the
        // pass above, so a budget follows a change of need at once.
        run_publish_budget(&mut router, &peers, now).await;
        let start_requests =
            drive_two_track_delivery(&mut router, now) + drive_video_wants(&mut router, now);
        // A packet written to a peer, or a keyframe request queued on one,
        // leaves on that peer's next clock tick and poll. Go straight round
        // rather than waiting for the next datagram: nothing is held at the
        // SFU. The second pass receives no input, so it captures no packet
        // and falls through to the wait below.
        if forwarded.sent > 0
            || relayed > 0
            || fec_sent > 0
            || fallback.keyframes_requested > 0
            || start_requests > 0
            || padding > 0
        {
            continue;
        }
        // `duplicates` left with this pass's drain and needs nothing more.
        let _ = duplicates;
        // A second copy that falls due before str0m's own next deadline
        // wakes the loop for it.
        // And so does the next padding step of a probe that is running.
        let next_duplicate = router
            .peers()
            .iter()
            .filter_map(|peer| {
                peer.next_light_duplicate()
                    .into_iter()
                    .chain(peer.next_downlink_tick(now))
                    .min()
            })
            .min();
        let deadline = next_duplicate.map_or(summary.earliest_deadline, |due| {
            due.min(summary.earliest_deadline)
        });
        let sleep_for = deadline
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
                    let taker = router
                        .peers_mut()
                        .iter_mut()
                        .find(|p| p.accepts(&input));
                    if let Some(tap) = ingress_tap.as_mut() {
                        tap.record(source, taker.is_some(), Instant::now());
                    }
                    if let Some(peer) = taker {
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
                // empty — the next loop iteration drives
                // every peer's clock, drains it and
                // recomputes the deadline.
            }
        }

        // Prune peers whose Rtc died (DTLS failure,
        // ICE timeout, etc.). The Vec retain compacts
        // in-place — O(n) on the worst case.
        router.retain_peers(MediaPeer::is_alive);
    }
}

/// Move every peer's str0m clock to `now`.
///
/// A timeout input is what makes str0m act on anything that is due:
/// stamping packets queued by `write_rtp` so the next poll sends them,
/// building sender and receiver reports, retransmission requests and
/// transport feedback, and running the ICE agent. A peer whose state
/// machine refuses the input is marked dead, the same way a failed poll
/// is, and the prune at the loop tail drops it.
fn drive_time(router: &mut MediaRouter, now: Instant) {
    for peer in router.peers_mut() {
        if let Err(e) = peer.handle_input(Input::Timeout(now)) {
            tracing::warn!(
                peer = %peer.id(),
                error = %e,
                "media: timeout input failed; dropping peer"
            );
            peer.disconnect();
        }
    }
}

/// Send every subscriber the second copies of light companion packets that
/// are due ([`MediaPeer::send_light_duplicates`]), file them in `ledger`
/// when one is given, and return how many there were.
pub(crate) fn send_light_duplicates(
    router: &mut MediaRouter,
    mut ledger: Option<&mut FanoutLedger>,
    now: Instant,
) -> u64 {
    let mut count = 0u64;
    for peer in router.peers_mut() {
        if peer.send_light_duplicates(now) == 0 {
            continue;
        }
        let sent = peer.take_duplicates_sent();
        count += sent.len() as u64;
        if let Some(l) = ledger.as_deref_mut() {
            for (origin, origin_mid, role) in &sent {
                l.record_duplicate_sent(peer.id(), origin, *origin_mid, *role);
            }
        }
    }
    if count > 0 {
        metrics::counter!(crate::prom::names::LIGHT_DUPLICATES_SENT_TOTAL).increment(count);
    }
    count
}

/// Run the playout delay for every (subscriber, publisher) pair
/// ([`crate::playout_delay`], ADR-0070 step 4): ask every peer whether its
/// uplink (as a publisher) and its downlink (as a subscriber) are in a
/// burst of loss, raise the delay of the pairs that touches, let the others
/// come down with time, and log every change. A publisher's uplink burst
/// reaches only subscribers of its own call (the pairs are made from a
/// subscriber's own downstream tracks). Returns how many pairs changed.
pub(crate) fn drive_playout_delay(router: &mut MediaRouter, now: Instant) -> usize {
    let mut uplink: Vec<MediaPeerId> = Vec::new();
    let mut downlink: Vec<MediaPeerId> = Vec::new();
    for peer in router.peers_mut() {
        if peer.poll_uplink_burst(now) {
            uplink.push(peer.id().clone());
        }
        if peer.poll_downlink_burst(now) {
            downlink.push(peer.id().clone());
        }
    }
    let mut changed = 0usize;
    for peer in router.peers_mut() {
        let down = downlink.contains(peer.id());
        let subscriber = peer.id().clone();
        for (publisher, from_ms, to_ms, reason) in peer.update_playout_delays(now, down, &uplink) {
            changed += 1;
            if to_ms > from_ms {
                metrics::counter!(crate::prom::names::PLAYOUT_DELAY_RAISES_TOTAL).increment(1);
            }
            tracing::info!(
                subscriber = %subscriber,
                publisher = %publisher,
                from_ms,
                to_ms,
                reason = reason.as_str(),
                "media: minimum playout delay changed"
            );
        }
    }
    changed
}

/// One video stream of a publisher: its media line and, for a simulcast
/// source, the layer.
type VideoStream = (Mid, Option<Rid>);

/// What became of one burst that ended unrepaired
/// ([`drive_burst_keyframes`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BurstKeyframe {
    /// The publisher was asked for a key frame.
    Asked,
    /// The request fell inside the rate limit's window: it is owed and
    /// goes out when the window ends.
    Owed,
    /// The track has been asked and has shown no key frame
    /// ([`KEYFRAME_UNANSWERED_MAX`]), or the publisher has no such
    /// receive stream: nothing was asked.
    NotAsked,
    /// No subscriber is being sent the stream: no key frame is needed.
    NobodyWatching,
}

impl BurstKeyframe {
    /// Stable, low-cardinality label for logs.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Asked => "asked",
            Self::Owed => "owed",
            Self::NotAsked => "not-asked",
            Self::NobodyWatching => "nobody-watching",
        }
    }
}

/// Ask the publisher of every video stream on which a burst of loss has
/// just ended unrepaired for a key frame
/// ([`crate::burst_keyframe`], ADR-0070 note of 2026-10-09), instead of
/// leaving it to a subscriber's decoder to give up seconds later.
///
/// - Per stream: main, middle and light, and each simulcast layer, on its
///   own.
/// - Only a stream some subscriber is being sent
///   ([`MediaPeer::is_sent_stream`]). A key frame of a stream nobody is
///   shown would reach nobody; a track that is shown again later is asked
///   for by [`drive_video_wants`].
/// - Through the one gate every request goes through
///   ([`MediaPeer::request_keyframe_limited`]): the per-track rate limit
///   (a request inside the window is owed) and the bound on unanswered
///   requests both hold.
/// - Once per burst: the bookkeeping hands each burst out once.
/// - A burst that ended with packets missing of which none is taken for
///   media somebody needs (known not media, or no more of unknown kind
///   than lost FEC packets explain) asks for nothing, changes nothing and
///   is only logged.
///
/// **The packets of the burst are no longer asked for.** Whenever the key
/// frame is asked for or owed, and whenever nobody is being sent the
/// stream, the publisher is no longer asked to retransmit what is missing
/// on it ([`MediaPeer::give_up_missing`]). By then every such packet has
/// been asked for at least twice on a path that carries packets again and
/// has not come; a key frame makes it useless; and each further request is
/// a retransmission the publisher sends on a path that has just recovered,
/// ahead of the key frame. Packets that go missing afterwards, the key
/// frame's own included, are asked for as usual. When nothing is asked
/// (the unanswered bound) the requests go on.
///
/// Returns what became of each burst, for tests and the log.
pub(crate) fn drive_burst_keyframes(
    router: &mut MediaRouter,
    now: Instant,
) -> Vec<(MediaPeerId, Mid, Option<Rid>, BurstKeyframe)> {
    let mut ended: Vec<(MediaPeerId, BurstEnd<VideoStream>)> = Vec::new();
    for peer in router.peers_mut() {
        let id = peer.id().clone();
        ended.extend(
            peer.poll_burst_ends(now)
                .into_iter()
                .map(|end| (id.clone(), end)),
        );
    }
    let mut outcomes = Vec::with_capacity(ended.len());
    for (publisher, end) in ended {
        let (mid, rid) = end.stream;
        if !end.asks() {
            // Packets stayed missing and none of them is taken for media
            // somebody needs: said, so the log shows every burst.
            tracing::info!(
                origin = %publisher,
                mid = ?mid,
                rid = ?rid,
                lost = end.lost,
                still_missing = end.unrepaired,
                known_media = end.media,
                known_not_media = end.not_media,
                unknown = end.unknown,
                beyond_repair = end.beyond_repair,
                lasted_ms = end.lasted.as_millis(),
                why = end.why.as_str(),
                "media: a burst of loss on a publisher's stream ended with packets missing and asked for nothing"
            );
            continue;
        }
        let watched = router
            .peers()
            .iter()
            .any(|peer| peer.id() != &publisher && peer.is_sent_stream(&publisher, mid, rid));
        let outcome = if watched {
            let track = OpenedTrack {
                source_peer: publisher.clone(),
                source_mid: mid,
                kind: MediaKind::Video,
                role: TrackRole::Main,
                rid,
            };
            if request_keyframe_from_publisher(router, &track, now, KeyframeReason::UplinkBurstEnd)
            {
                BurstKeyframe::Asked
            } else if router
                .peer_mut(&publisher)
                .is_some_and(|p| p.keyframe_request_owed(mid, rid))
            {
                BurstKeyframe::Owed
            } else {
                BurstKeyframe::NotAsked
            }
        } else {
            BurstKeyframe::NobodyWatching
        };
        let given_up = if outcome == BurstKeyframe::NotAsked {
            0
        } else {
            router
                .peer_mut(&publisher)
                .map_or(0, |p| p.give_up_missing(mid, rid))
        };
        tracing::info!(
            origin = %publisher,
            mid = ?mid,
            rid = ?rid,
            lost = end.lost,
            still_missing = end.unrepaired,
            known_media = end.media,
            known_not_media = end.not_media,
            unknown = end.unknown,
            beyond_repair = end.beyond_repair,
            lasted_ms = end.lasted.as_millis(),
            why = end.why.as_str(),
            key_frame = outcome.as_str(),
            given_up,
            "media: a burst of loss on a publisher's stream ended unrepaired"
        );
        outcomes.push((publisher, mid, rid, outcome));
    }
    outcomes
}

/// Note, for each fanout line due at this flush, the playout delay being
/// stamped on that publisher's video toward that subscriber, and whether
/// the subscriber is being sent nothing on the track (ADR-0070 step 5).
fn note_playout_delays(router: &MediaRouter, ledger: &mut FanoutLedger) {
    for peer in router.peers() {
        for track in peer.tracks_out() {
            if track.forwarding.gate != Gate::Flowing {
                ledger.note_paused(peer.id(), &track.source_peer, track.source_mid, track.role);
            }
            if let Some(ms) = peer.playout_delay_ms(&track.source_peer) {
                ledger.note_playout_delay(
                    peer.id(),
                    &track.source_peer,
                    track.source_mid,
                    track.role,
                    ms,
                );
            }
        }
    }
}

/// Note, for each heavy camera sender of a publisher that is told which
/// of its senders are needed, what it was last told
/// ([`crate::video_needed`]).
fn note_video_needed(router: &MediaRouter, ledger: &mut FanoutLedger) {
    for peer in router.peers() {
        if !peer.told_video_needed() || peer.video_needed_told().is_none() {
            continue;
        }
        for track in peer.tracks_in() {
            if let Some(role) = peer.heavy_sender_role(track.mid) {
                ledger.note_needed(peer.id(), track.mid, peer.told_needed(role));
            }
        }
    }
}

/// Run bitrate control for every publisher: loss intervals and REMB (see
/// [`MediaPeer::drive_uplink`]). Returns how many peers a REMB was
/// requested for. Does nothing under [`BitrateControl::Client`].
pub(crate) fn drive_bitrate_control(router: &mut MediaRouter, now: Instant) -> usize {
    router
        .peers_mut()
        .iter_mut()
        .map(|peer| usize::from(peer.drive_uplink(now)))
        .sum()
}

/// The diagnostics lines of the interval, when it is over.
fn log_diagnostics_if_due(router: &mut MediaRouter, ledger: &mut FanoutLedger, now: Instant) {
    if ledger.is_due(now) {
        log_uplink_estimates(router, now);
        log_downlinks(router, now);
        note_uplink_fallback_states(router, ledger);
        note_playout_delays(router, ledger);
        note_video_needed(router, ledger);
    }
    ledger.flush_if_due(now);
}

/// Log and count what one run of a subscriber's downlink did.
fn log_downlink_run(subscriber: &MediaPeerId, run: &DownlinkRun) {
    if let Some(target) = run.probe_started_bps {
        tracing::info!(
            %subscriber,
            target_kbps = target / 1000,
            budget_kbps = ?run.budget_bps.map(|bps| bps / 1000),
            "media: trying the next larger set on a subscriber's downlink"
        );
    }
    for cut in &run.cuts {
        tracing::info!(
            %subscriber,
            reason = cut.reason,
            to_kbps = cut.to_bps / 1000,
            standing_queue_ms = ?cut.standing_queue_ms,
            taken_back = cut.taken_back,
            beyond_sending_ms = ?cut.beyond_sending_ms,
            held_at_steady = cut.held_at_steady,
            from_kbps = cut.from_bps / 1000,
            arriving_kbps = ?cut.arriving_bps.map(|bps| bps / 1000),
            sending_kbps = cut.sending_bps / 1000,
            "media: the estimate of a subscriber's downlink was lowered"
        );
    }
    for outcome in &run.outcomes {
        let label = outcome
            .failure
            .map_or("carried", |failure| failure.as_str());
        metrics::counter!(
            crate::prom::names::DOWNLINK_PROBES_TOTAL,
            "outcome" => label
        )
        .increment(1);
        metrics::counter!(crate::prom::names::DOWNLINK_PROBE_PADDING_BYTES_TOTAL)
            .increment(outcome.padding_bytes);
        tracing::info!(
            %subscriber,
            outcome = label,
            target_kbps = outcome.target_bps / 1000,
            carried_kbps = outcome.carried_bps / 1000,
            padding_bytes = outcome.padding_bytes,
            held_back_bytes = outcome.held_back_bytes,
            standing_queue_ms = ?run.queue_ms,
            "media: a probe of a subscriber's downlink ended"
        );
    }
    if let Some((level, wait)) = run.too_large {
        tracing::info!(
            %subscriber,
            level,
            need_kbps = run.need_bps / 1000,
            wait_secs = wait.as_secs(),
            "media: a set given to a subscriber's downlink proved too large"
        );
    }
    for change in &run.changes {
        let direction = if downlink_allocation::smaller(change.from, change.to) == change.to {
            "down"
        } else {
            "up"
        };
        metrics::counter!(
            crate::prom::names::DOWNLINK_GIVEN_CHANGES_TOTAL,
            "direction" => direction
        )
        .increment(1);
        tracing::info!(
            %subscriber,
            publisher = %change.publisher,
            from = change.from.as_str(),
            to = change.to.as_str(),
            wanted = change.wanted.as_str(),
            budget_kbps = ?run.budget_bps.map(|bps| bps / 1000),
            carried_kbps = ?run.carried_bps.map(|bps| bps / 1000),
            why = change.why,
            wire_kbps = run.wire_bps / 1000,
            need_kbps = run.need_bps / 1000,
            standing_queue_ms = ?run.queue_ms,
            "media: what a subscriber is given of a publisher's video changed"
        );
    }
}

/// Run downlink control for every subscriber whose allocation is due
/// (ADR-0070 step 6; [`MediaPeer::run_downlink`]): gather what each
/// publisher's tracks cost, list each subscriber's publishers of its own
/// call in its order of importance, run its allocation, log every change
/// of a given quality, and return the `video-given` frames to send, each
/// with the subscriber it is for. Does nothing, and returns nothing,
/// without downlink control.
pub(crate) fn drive_downlink_control(
    router: &mut MediaRouter,
    now: Instant,
) -> Vec<(MediaPeerId, ServerFrame)> {
    if !router.peers().iter().any(|peer| peer.downlink_due(now)) {
        return Vec::new();
    }
    // Every peer as a publisher, in the order they joined: the router's
    // own order.
    let published: Vec<(MediaPeerId, String, PublishedRates)> = router
        .peers()
        .iter()
        .map(|peer| {
            (
                peer.id().clone(),
                peer.call_id().to_owned(),
                peer.published_rates(now),
            )
        })
        .collect();
    let mut frames = Vec::new();
    for sub in router.peers_mut() {
        if !sub.downlink_due(now) {
            continue;
        }
        let open = sub.open_publishers();
        let light_copies = if sub.protection().light_duplicate {
            2
        } else {
            1
        };
        let joined: Vec<MediaPeerId> = published
            .iter()
            .filter(|(id, call, _)| {
                id != sub.id() && call == sub.call_id() && !call.is_empty() && open.contains(id)
            })
            .map(|(id, _, _)| id.clone())
            .collect();
        let publishers: Vec<downlink_allocation::Publisher<MediaPeerId>> = sub
            .importance_order(&joined)
            .into_iter()
            .filter_map(|id| {
                let (_, _, rates) = published.iter().find(|(p, _, _)| *p == id)?;
                let video = |role, bps: u64| {
                    sub.has_open_track_of(&id, MediaKind::Video, role)
                        .then_some(bps)
                };
                let audio = sub.has_open_track_of(&id, MediaKind::Audio, TrackRole::Main);
                Some(downlink_allocation::Publisher {
                    wanted: sub.video_quality_for(&id),
                    main_bps: video(TrackRole::Main, rates.main_bps),
                    middle_bps: video(TrackRole::Middle, rates.middle_bps),
                    // A light companion packet is sent twice (ADR-0070
                    // step 3) when that is switched on: twice the cost.
                    light_bps: video(
                        TrackRole::LightCompanion,
                        rates.light_bps.saturating_mul(light_copies),
                    ),
                    audio_bps: if audio { rates.audio_bps } else { 0 },
                    publisher: id,
                })
            })
            .collect();
        let run = sub.run_downlink(&publishers, now);
        log_downlink_run(sub.id(), &run);
        if let Some(given) = run.tell {
            frames.push((sub.id().clone(), ServerFrame::VideoGiven { given }));
        }
    }
    frames
}

/// One pass of downlink control in the media loop: take every
/// subscriber's transport-wide feedback and queue the padding a running
/// probe owes ([`MediaPeer::pump_downlink`]), run the allocations that
/// are due ([`drive_downlink_control`]) and send the `video-given` frames
/// that came of it. Returns the bytes of padding queued in this pass.
/// Does nothing without downlink control.
async fn run_downlink_control(
    router: &mut MediaRouter,
    peers: &PeerRegistry,
    now: Instant,
) -> usize {
    let padding: usize = router
        .peers_mut()
        .iter_mut()
        .map(|peer| peer.pump_downlink(now))
        .sum();
    let told = drive_downlink_control(router, now);
    send_video_given(peers, told).await;
    padding
}

/// Send each subscriber its `video-given` frame on its signaling socket.
/// A frame that cannot be queued is dropped and logged: the subscriber is
/// told the whole set again at the next change or `video-wants`.
async fn send_video_given(peers: &PeerRegistry, frames: Vec<(MediaPeerId, ServerFrame)>) {
    for (peer_id, frame) in frames {
        let Some(peer) = peers.peer(&peer_id).await else {
            continue;
        };
        let Some(tx) = peer.outbound.as_ref() else {
            continue;
        };
        if let Err(e) = tx.try_send(frame) {
            tracing::debug!(
                peer = %peer_id,
                error = %e,
                "media: video-given frame not queued"
            );
        }
    }
}

/// How often [`drive_video_needed`] runs at most, unless a subscriber's
/// wants just changed. Chosen: the media loop passes once per datagram,
/// and counting every publisher's subscribers that often buys nothing; a
/// sender that is needed again is still told within one frame interval.
const VIDEO_NEEDED_INTERVAL: Duration = Duration::from_millis(20);

/// Tell every publisher which of its camera senders somebody is being
/// sent ([`crate::video_needed`], [`MediaPeer::drive_video_needed`]):
/// count, per publisher that is told at all, the subscribers that want
/// its main and its middle picture ([`MediaPeer::heavy_wanted_of`]), mark
/// the senders that went idle, and return the `video-needed` frames to
/// send, each with the publisher it is for. One line is logged per
/// change. Does nothing, and returns nothing, when it last ran less than
/// [`VIDEO_NEEDED_INTERVAL`] ago, and for publishers with the switch off
/// (`LAKKO_SFU_VIDEO_NEEDED=0`).
pub(crate) fn drive_video_needed(
    router: &mut MediaRouter,
    now: Instant,
) -> Vec<(MediaPeerId, ServerFrame)> {
    let due = router.video_needed_ran.map_or(true, |at| {
        now.saturating_duration_since(at) >= VIDEO_NEEDED_INTERVAL
    });
    if !due {
        return Vec::new();
    }
    router.video_needed_ran = Some(now);
    if !router.peers().iter().any(MediaPeer::told_video_needed) {
        return Vec::new();
    }
    let counted: Vec<(MediaPeerId, Wanted)> = router
        .peers()
        .iter()
        .filter(|publisher| publisher.told_video_needed())
        .map(|publisher| {
            let mut wanted = Wanted::default();
            for sub in router.peers() {
                if sub.id() == publisher.id() {
                    continue;
                }
                let (main, middle) =
                    sub.heavy_wanted_of(publisher.id(), publisher.middle_mid().is_some());
                wanted.main += usize::from(main);
                wanted.middle += usize::from(middle);
            }
            (publisher.id().clone(), wanted)
        })
        .collect();
    let mut frames = Vec::new();
    for (id, wanted) in counted {
        let Some(change) = router
            .peer_mut(&id)
            .and_then(|publisher| publisher.drive_video_needed(wanted, now))
        else {
            continue;
        };
        tracing::info!(
            publisher = %id,
            main = change.needed.main,
            middle = change.needed.middle,
            want_main = change.wanted.main,
            want_middle = change.wanted.middle,
            "media: which of a publisher's camera senders are needed changed"
        );
        frames.push((
            id,
            ServerFrame::VideoNeeded {
                main: change.needed.main,
                middle: change.needed.middle,
            },
        ));
    }
    if !frames.is_empty() {
        // Which senders take from a publisher's budget changed: the
        // budget follows in this same pass.
        router.publish_budget_ran = None;
    }
    frames
}

/// How often the publish budget is looked at. Chosen: what a publisher is
/// told changes with its bitrate estimate, which moves on the scale of the
/// REMB interval; a lower budget is therefore at most this late.
const PUBLISH_BUDGET_INTERVAL: Duration = Duration::from_millis(100);

/// Decide, for every publisher that is told how much its camera senders
/// may send, whether it is to be told now ([`crate::publish_budget`]), and
/// return the frames to send. Runs when [`PUBLISH_BUDGET_INTERVAL`] has
/// passed since the last run, and in the pass after what a publisher was
/// told of its senders changed. Returns nothing sooner, under
/// [`BitrateControl::Client`] and with the switch off
/// ([`LossProtection::publish_budget`]).
pub(crate) fn drive_publish_budget(
    router: &mut MediaRouter,
    now: Instant,
) -> Vec<(MediaPeerId, ServerFrame)> {
    let due = router.publish_budget_ran.map_or(true, |at| {
        now.saturating_duration_since(at) >= PUBLISH_BUDGET_INTERVAL
    });
    if !due {
        return Vec::new();
    }
    router.publish_budget_ran = Some(now);
    let mut frames = Vec::new();
    for publisher in router.peers_mut() {
        let Some(change) = publisher.drive_publish_budget(now) else {
            continue;
        };
        metrics::counter!(
            crate::prom::names::PUBLISH_BUDGET_SENT_TOTAL,
            "why" => change.why.as_str()
        )
        .increment(1);
        tracing::info!(
            publisher = %publisher.id(),
            estimate_kbps = change.inputs.estimate_bps / 1000,
            main_kbps = change.budget.main_kbps,
            middle_kbps = change.budget.middle_kbps,
            main_needed = change.inputs.main_needed,
            middle_needed = change.inputs.has_middle && change.inputs.middle_needed,
            why = change.why.as_str(),
            "media: a publisher was told how much its camera senders may send"
        );
        frames.push((
            publisher.id().clone(),
            ServerFrame::PublishBudget {
                main_kbps: change.budget.main_kbps,
                middle_kbps: change.budget.middle_kbps,
            },
        ));
    }
    frames
}

/// One pass of the publish budget in the media loop:
/// [`drive_publish_budget`], and each frame that came of it sent to its
/// publisher on its signaling socket. A frame that cannot be queued is
/// produced again at the next run.
async fn run_publish_budget(router: &mut MediaRouter, peers: &PeerRegistry, now: Instant) {
    for (peer_id, frame) in drive_publish_budget(router, now) {
        let queued = match peers.peer(&peer_id).await {
            Some(peer) => peer
                .outbound
                .as_ref()
                .is_some_and(|tx| tx.try_send(frame).is_ok()),
            None => false,
        };
        if !queued {
            // Tried again at the next run, [`PUBLISH_BUDGET_INTERVAL`] on.
            tracing::trace!(peer = %peer_id, "media: publish-budget frame not queued");
            if let Some(publisher) = router.peer_mut(&peer_id) {
                publisher.publish_budget_not_delivered();
            }
        }
    }
}

/// One pass of "senders nobody watches" in the media loop:
/// [`drive_video_needed`], and each frame that came of it sent to its
/// publisher on its signaling socket. A frame that cannot be queued is
/// produced again at the next run.
async fn run_video_needed(router: &mut MediaRouter, peers: &PeerRegistry, now: Instant) {
    for (peer_id, frame) in drive_video_needed(router, now) {
        let queued = match peers.peer(&peer_id).await {
            Some(peer) => peer
                .outbound
                .as_ref()
                .is_some_and(|tx| tx.try_send(frame).is_ok()),
            None => false,
        };
        if !queued {
            // Tried again at the next run, [`VIDEO_NEEDED_INTERVAL`] on.
            tracing::trace!(peer = %peer_id, "media: video-needed frame not queued");
            if let Some(publisher) = router.peer_mut(&peer_id) {
                publisher.video_needed_not_delivered();
            }
        }
    }
}

/// One line per subscriber per diagnostics interval under downlink
/// control (ADR-0070 step 6): the budget, what the path was seen to
/// carry, what is being sent and what the SFU would like to send, how
/// many publishers are given each quality, and the probes so far.
fn log_downlinks(router: &MediaRouter, now: Instant) {
    for peer in router.peers() {
        let Some(snap) = peer.downlink_snapshot(now) else {
            continue;
        };
        tracing::info!(
            subscriber = %peer.id(),
            budget_kbps = ?snap.budget_bps.map(|bps| bps / 1000),
            estimate_kbps = ?snap.estimate_bps.map(|bps| bps / 1000),
            carried_kbps = ?snap.carried_bps.map(|bps| bps / 1000),
            sending_kbps = snap.sending_bps / 1000,
            demand_kbps = ?snap.demand_bps.map(|bps| bps / 1000),
            given_main = snap.main,
            given_middle = snap.middle,
            given_light = snap.light,
            probing_kbps = ?snap.probing_bps.map(|bps| bps / 1000),
            probes_started = snap.probes_started,
            probes_carried = snap.probes_carried,
            padding_bytes = snap.padding_bytes,
            cuts = snap.cuts,
            standing_queue_ms = ?snap.queue_ms,
            detector = snap.detector,
            limit_kbps = ?snap.limit_bps.map(|bps| bps / 1000),
            held_from_level = ?snap.held_from_level,
            too_large_wait_secs = snap.too_large_wait_secs,
            followed = snap.followed,
            "media: subscriber downlink over the last interval"
        );
    }
}

/// One line per publisher per diagnostics interval under
/// [`BitrateControl::Server`]: the estimate the SFU sends the publisher,
/// the rate arriving from it, the delay detector's state and why the
/// estimate last changed (ADR-0068 section 6).
fn log_uplink_estimates(router: &mut MediaRouter, now: Instant) {
    for peer in router.peers_mut() {
        let Some(snap) = peer.uplink_snapshot(now) else {
            continue;
        };
        tracing::info!(
            origin = %peer.id(),
            estimate_kbps = snap.estimate_bps / 1000,
            arriving_kbps = snap.arriving_bps / 1000,
            state = snap.state.as_str(),
            last_change = snap.last_change.as_str(),
            retransmitted = snap.retransmitted,
            queue_ms = ?snap.queue_ms,
            doubted = snap.doubted,
            "media: publisher bitrate estimate over the last interval"
        );
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
        role = ?upstream.as_ref().map(|t| t.role.as_str()),
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

/// One publisher packet captured during a drain pass,
/// tagged with the originating peer id so the fanout
/// step can skip the source when forwarding.
pub(crate) struct PendingFanout {
    /// Peer the packet arrived from.
    pub(crate) origin: MediaPeerId,
    /// The packet to be forwarded.
    pub(crate) packet: RelayPacket,
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
    /// Publisher packets captured during the drain,
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
/// `sleep_until` it; capture `Event::RtpPacket` packets
/// into a pending-fanout queue. Returns a
/// [`DrainSummary`].
///
/// The caller is responsible for invoking
/// `fanout_packets_into` on `summary.pending` after this
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
                    Event::RtpPacket(packet) => {
                        // One RTP packet from this publisher, handed over
                        // as it arrived: str0m in RTP mode neither orders
                        // nor waits. A retransmission the publisher sent
                        // on its RTX stream arrives here already rewritten
                        // as the packet it repairs.
                        //
                        // With video FEC on, the packet's own bytes are
                        // taken first (they are paired with this event),
                        // and after the packet is queued the publisher's
                        // FEC receiver sees it: whatever it can now rebuild
                        // is queued right behind, as packets of the same
                        // stream.
                        let received = peer.take_received();
                        // Bitrate control reads the send time and the
                        // size before the packet is consumed (ADR-0068).
                        let seen = MediaPeer::uplink_seen(&packet);
                        if let Some(packet) = peer.relay_packet(packet) {
                            // A sender that was switched off on the SFU's
                            // word and is back is noted first: its
                            // return is not loss to anything below.
                            peer.note_sender_arrival(&packet, now);
                            peer.observe_uplink(seen, &packet);
                            let rebuilt = peer.fec_receive(&packet, received, now);
                            for packet in std::iter::once(packet).chain(rebuilt) {
                                // Meter this layer's bitrate (simulcast only)
                                // before the packet moves into the fanout
                                // queue — the per-receiver policy reads
                                // these rates.
                                peer.record_layer_packet(&packet, now);
                                // And every track's arrival rate, for the
                                // subscribers' allocations (ADR-0070 step
                                // 6; a no-op without downlink control).
                                peer.record_track_packet(&packet, now);
                                // And its sequence number goes to the
                                // track's uplink fallback policy.
                                peer.observe_uplink_holes(&packet);
                                // And to the publisher's burst detector
                                // (playout delay, ADR-0070 step 4).
                                peer.observe_uplink_burst(&packet);
                                // And to the bookkeeping that says when
                                // a burst on its stream has ended
                                // unrepaired.
                                peer.observe_burst_end(&packet);
                                if let Some(track) = late_simulcast_track(peer, &packet) {
                                    new_tracks.push(PendingTrackIn {
                                        origin: origin.clone(),
                                        track,
                                    });
                                }
                                pending.push(PendingFanout {
                                    origin: origin.clone(),
                                    packet,
                                });
                            }
                        }
                    },
                    Event::EgressBitrateEstimate(kind) => {
                        // Per-receiver bandwidth signal for the simulcast layer
                        // policy: the send-side estimate for SENDING to
                        // `peer`. A subscriber's REMB is not taken.
                        peer.note_egress_estimate(&kind);
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
                            simulcast = track.simulcast,
                            role = track.role().as_str(),
                            "media: track announced by peer"
                        );
                        new_tracks.push(PendingTrackIn {
                            origin: origin.clone(),
                            track,
                        });
                    },
                    Event::KeyframeRequest(req) => {
                        // Resolved here, against this peer's `tracks_out`,
                        // because the relay itself needs a mutable borrow of
                        // a different peer.
                        keyframe_requests.extend(peer.subscriber_keyframe_request(req.mid, now));
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
                        // From here on the fanout may write to this peer.
                        peer.mark_connected();
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
        // What was just serialized for this peer may complete FEC packets
        // for it (video FEC only; a no-op otherwise). They are written now
        // and leave on the next pass.
        peer.send_leg_fec();
    }
    DrainSummary {
        earliest_deadline,
        pending,
        new_tracks,
        keyframe_requests,
    }
}

/// The track `packet` arrived on, if this packet is what shows it to be
/// simulcast: a video track on which a second rid has just been seen. Its
/// subscribers get the light companion now. `Some` once per track
/// ([`MediaPeer::note_simulcast_track_in`]).
fn late_simulcast_track(peer: &mut MediaPeer, packet: &RelayPacket) -> Option<TrackIn> {
    if packet.kind != MediaKind::Video || packet.rid.is_none() {
        return None;
    }
    let track = peer.note_simulcast_track_in(packet.mid)?;
    tracing::info!(
        peer = %peer.id(),
        mid = ?track.mid,
        "media: track found to be simulcast from its packets"
    );
    Some(track)
}

/// Per-pass tally of fanout outcomes: one increment per
/// (packet, destination peer). Returned so the media task and
/// tests can observe fanout health without scraping the
/// global metrics registry; `fanout_packets_into` also
/// mirrors it into the `lakko_sfu_media_frames_*`
/// Prometheus counters. Those names date from when the SFU
/// forwarded whole frames and are kept so dashboards and
/// alert rules keep working; the unit they count is now an
/// RTP packet.
///
/// The **fairness** property the SFU upholds: for a single
/// origin packet in an N-peer call, [`Self::attempted`]
/// equals `N - 1` — every non-origin peer is visited exactly
/// once (none skipped, none double-served) and the origin is
/// never offered its own packet. A packet of the light layer
/// of a simulcast video track counts once more for every peer
/// that has the track's light companion open
/// ([`TrackRole::LightCompanion`]): it is offered to that
/// track too. A packet of a publisher's declared light track
/// ([`TrackIn::light`]) counts once per non-origin peer, like
/// any packet: it is offered to the companion alone.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FanoutOutcome {
    /// Packets written to a live downstream send stream.
    pub(crate) sent: u64,
    /// Skips because the destination's `tracks_out` entry is
    /// not yet `Open` (renegotiation in flight), its transport
    /// is not connected, or str0m has no send stream for the
    /// mid (peer teardown race).
    pub(crate) no_writer: u64,
    /// Skips because the destination did not negotiate the
    /// packet's codec.
    pub(crate) no_codec_match: u64,
    /// str0m refused the write.
    pub(crate) write_failed: u64,
    /// Simulcast: packets dropped because they belong to a
    /// layer this subscriber is not currently receiving. This
    /// is the SFU doing its job (one layer per subscriber),
    /// not a fault — but a spike is worth seeing (e.g. a
    /// publisher sending three layers to an audience that all
    /// picked the lowest).
    pub(crate) wrong_layer: u64,
    /// Packets with no slot on the downstream stream: older
    /// than a simulcast switch point or than the cycle the
    /// stream started in. Expected in ones and twos around a
    /// layer switch; anything steady is worth a look.
    pub(crate) stale: u64,
    /// Packets of a main or middle track the subscriber does not
    /// show (ADR-0070 step 5), or of one waiting for a key frame
    /// to resume at. The SFU doing what it was asked, not a fault.
    pub(crate) not_wanted: u64,
    /// Video FEC packets the SFU computed for and wrote to the
    /// destination ([`crate::fec_send`]). Not a forward attempt
    /// and not part of [`Self::attempted`]: filed by the media
    /// loop per destination, in the ledger only.
    pub(crate) fec_sent: u64,
    /// Second copies of light companion packets sent to the
    /// destination (ADR-0070 step 3). Not a forward attempt, not
    /// part of [`Self::attempted`] and not a repair: nobody asked
    /// for them. In the ledger only.
    pub(crate) duplicates: u64,
    /// The minimum playout delay being stamped on this
    /// publisher's video toward the destination at the flush, in
    /// milliseconds; `None` for audio and with the feature off.
    /// In the ledger only.
    pub(crate) playout_delay_ms: Option<u16>,
    /// Whether the subscriber was being sent nothing on this track
    /// at the flush: paused, or waiting to resume. In the ledger
    /// only.
    pub(crate) paused: bool,
}

impl FanoutOutcome {
    /// Total forward attempts this pass = sum of all
    /// outcome buckets = `packets × non-origin peers`.
    pub(crate) const fn attempted(&self) -> u64 {
        self.sent
            + self.no_writer
            + self.no_codec_match
            + self.write_failed
            + self.wrong_layer
            + self.stale
            + self.not_wanted
    }

    /// File one forward result into its bucket.
    pub(crate) fn tally(&mut self, forwarded: &MediaForward) {
        match forwarded {
            MediaForward::Sent => self.sent += 1,
            MediaForward::NoWriter => self.no_writer += 1,
            MediaForward::NoCodecMatch => self.no_codec_match += 1,
            MediaForward::WrongLayer => self.wrong_layer += 1,
            MediaForward::Stale => self.stale += 1,
            MediaForward::NotWanted => self.not_wanted += 1,
            MediaForward::WriteFailed(_) => self.write_failed += 1,
        }
    }
}

/// What one publisher stream produced over a ledger interval.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OriginTally {
    /// RTP packets received on this stream.
    pub(crate) packets: u64,
    /// Frames, counted without reassembling any: every audio
    /// packet is one, and a video frame is counted at its
    /// marker-bit packet. A video frame whose last packet was
    /// lost is therefore not counted.
    pub(crate) frames: u64,
    /// Payload bytes across those packets.
    pub(crate) bytes: u64,
    /// Packets whose frame descriptor marks the start of an
    /// independent frame, i.e. key frames. Only a video stream
    /// carrying the generic frame descriptor can count here;
    /// audio and descriptor-less video stay at zero, which the
    /// log reader must not read as "no key frames were sent".
    pub(crate) key_frames: u64,
    /// Video FEC packets received on this stream. Counted
    /// here and nowhere above: an FEC packet is not media, so
    /// it is not a packet, a frame, payload bytes or a key
    /// frame of the stream. None is forwarded.
    pub(crate) fec_packets: u64,
    /// Media packets that did not arrive and were rebuilt from
    /// the stream's FEC. Each is also one of `packets` (and of
    /// `frames`, `bytes`, `key_frames` where it applies): to a
    /// subscriber it is a packet like the others.
    pub(crate) recovered: u64,
    /// The uplink fallback state of the track at the end of the
    /// interval (`full` / `fallback`); `None` for a track without
    /// simulcast layers.
    pub(crate) uplink: Option<&'static str>,
    /// Whether the publisher was last told that somebody is being sent
    /// this sender ([`crate::video_needed`]); `None` for a stream its
    /// publisher is told nothing of (audio, the light track, simulcast,
    /// a publisher that declared no senders, the switch off).
    pub(crate) needed: Option<bool>,
}

/// Per-destination fanout bookkeeping, logged and reset on a fixed
/// cadence. Exists because the Prometheus counters answer "how many
/// packets were dropped" and not "whose packets, to whom" — the
/// question a call with picture on one device and none on another
/// asks. Keys carry the origin's mid, so a publisher's audio and
/// video are told apart.
#[derive(Debug)]
pub(crate) struct FanoutLedger {
    every: Duration,
    last_flush: Instant,
    /// `(destination, origin, origin mid, downstream track role)` ->
    /// outcome buckets. A simulcast video track has two lines per
    /// destination: its main track and its light companion.
    per_target: HashMap<(MediaPeerId, MediaPeerId, Mid, TrackRole), FanoutOutcome>,
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

    /// File one received packet against its publisher stream.
    pub(crate) fn record_origin(&mut self, origin: &MediaPeerId, packet: &RelayPacket) {
        let tally = self
            .per_origin
            .entry((origin.clone(), packet.mid))
            .or_default();
        if packet.is_fec() {
            tally.fec_packets += 1;
            return;
        }
        tally.packets += 1;
        if packet.recovered {
            tally.recovered += 1;
        }
        tally.bytes += packet.media_len() as u64;
        if packet.ends_frame() {
            tally.frames += 1;
        }
        if packet.starts_independent_frame {
            tally.key_frames += 1;
        }
    }

    /// File one forward attempt.
    pub(crate) fn record(
        &mut self,
        target: &MediaPeerId,
        origin: &MediaPeerId,
        origin_mid: Mid,
        role: TrackRole,
        forwarded: &MediaForward,
    ) {
        self.per_target
            .entry((target.clone(), origin.clone(), origin_mid, role))
            .or_default()
            .tally(forwarded);
    }

    /// File one FEC packet the SFU wrote to `target` on its
    /// downstream track of `role` for the track `(origin, origin_mid)`.
    pub(crate) fn record_fec_sent(
        &mut self,
        target: &MediaPeerId,
        origin: &MediaPeerId,
        origin_mid: Mid,
        role: TrackRole,
    ) {
        self.per_target
            .entry((target.clone(), origin.clone(), origin_mid, role))
            .or_default()
            .fec_sent += 1;
    }

    /// File one second copy of a light companion packet sent to
    /// `target` for the track `(origin, origin_mid)`.
    pub(crate) fn record_duplicate_sent(
        &mut self,
        target: &MediaPeerId,
        origin: &MediaPeerId,
        origin_mid: Mid,
        role: TrackRole,
    ) {
        self.per_target
            .entry((target.clone(), origin.clone(), origin_mid, role))
            .or_default()
            .duplicates += 1;
    }

    /// Note the playout delay being stamped on a line that has
    /// something filed this interval, for its line at the flush.
    pub(crate) fn note_playout_delay(
        &mut self,
        target: &MediaPeerId,
        origin: &MediaPeerId,
        origin_mid: Mid,
        role: TrackRole,
        ms: u16,
    ) {
        if let Some(o) =
            self.per_target
                .get_mut(&(target.clone(), origin.clone(), origin_mid, role))
        {
            o.playout_delay_ms = Some(ms);
        }
    }

    /// Note that a line that has something filed this interval is of
    /// a track the subscriber is being sent nothing on, for its line
    /// at the flush.
    pub(crate) fn note_paused(
        &mut self,
        target: &MediaPeerId,
        origin: &MediaPeerId,
        origin_mid: Mid,
        role: TrackRole,
    ) {
        if let Some(o) =
            self.per_target
                .get_mut(&(target.clone(), origin.clone(), origin_mid, role))
        {
            o.paused = true;
        }
    }

    /// Note the uplink fallback state of a publisher track that has
    /// something filed this interval, for its line at the flush.
    pub(crate) fn note_uplink_state(
        &mut self,
        origin: &MediaPeerId,
        mid: Mid,
        state: &'static str,
    ) {
        if let Some(tally) = self.per_origin.get_mut(&(origin.clone(), mid)) {
            tally.uplink = Some(state);
        }
    }

    /// Note whether the publisher was told that the heavy sender
    /// `(origin, mid)` is needed, for its line at the flush. A sender
    /// that is not needed gets its line even when nothing arrived on it
    /// in the interval: that silence is the point.
    pub(crate) fn note_needed(&mut self, origin: &MediaPeerId, mid: Mid, needed: bool) {
        if needed {
            if let Some(tally) = self.per_origin.get_mut(&(origin.clone(), mid)) {
                tally.needed = Some(true);
            }
        } else {
            self.per_origin
                .entry((origin.clone(), mid))
                .or_default()
                .needed = Some(false);
        }
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
                packets = t.packets,
                frames = t.frames,
                bytes = t.bytes,
                key_frames = t.key_frames,
                fec_packets = t.fec_packets,
                recovered = t.recovered,
                uplink = t.uplink.unwrap_or("not-simulcast"),
                needed = t.needed.map_or("not-told", |needed| if needed { "yes" } else { "no" }),
                "media: publisher stream over the last interval"
            );
            lines += 1;
        }
        let mut targets: Vec<_> = self.per_target.drain().collect();
        targets.sort_by(|a, b| a.0.cmp(&b.0));
        for ((target, origin, mid, role), o) in targets {
            tracing::info!(
                target = %target,
                origin = %origin,
                origin_mid = ?mid,
                role = role.as_str(),
                sent = o.sent,
                no_writer = o.no_writer,
                no_codec_match = o.no_codec_match,
                write_failed = o.write_failed,
                wrong_layer = o.wrong_layer,
                stale = o.stale,
                not_wanted = o.not_wanted,
                paused = o.paused,
                fec_sent = o.fec_sent,
                duplicates = o.duplicates,
                playout_delay_ms = ?o.playout_delay_ms,
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
    if outcome.stale > 0 {
        metrics::counter!(MEDIA_FRAMES_DROPPED_TOTAL, "reason" => "stale").increment(outcome.stale);
    }
    if outcome.not_wanted > 0 {
        metrics::counter!(MEDIA_FRAMES_DROPPED_TOTAL, "reason" => "not_wanted")
            .increment(outcome.not_wanted);
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

/// Walk `pending` and offer each packet to every peer in
/// `router` that is NOT the origin and shares its call. A
/// write queues the packet on the destination peer's send
/// stream; that peer's next clock tick and poll put it on
/// the wire.
///
/// Best-effort: a destination without an open track, a
/// connected transport or a matching codec is skipped — see
/// [`MediaForward`] for the per-variant semantics.
#[cfg(test)]
pub(crate) fn fanout_packets(router: &mut MediaRouter, pending: &[PendingFanout]) -> FanoutOutcome {
    fanout_packets_into(router, pending, None)
}

/// As `fanout_packets`, additionally filing every attempt into
/// `ledger` (per destination, origin and origin mid) when one is
/// given. The ledger is the diagnostic view the aggregate counters
/// cannot give: which subscriber a publisher's packets do and do not
/// reach, and whether the publisher's video carries key frames.
pub(crate) fn fanout_packets_into(
    router: &mut MediaRouter,
    pending: &[PendingFanout],
    mut ledger: Option<&mut FanoutLedger>,
) -> FanoutOutcome {
    let mut outcome = FanoutOutcome::default();
    if pending.is_empty() || router.len() < 2 {
        return outcome;
    }
    // What each publisher knows of its simulcast tracks' layers, looked
    // up once per track per pass.
    let mut layouts: Vec<((MediaPeerId, Mid), Option<SimulcastLayout>)> = Vec::new();
    // Packets written to light companion tracks, for their own counter.
    let mut light_sent = 0u64;
    for event in pending {
        if let Some(l) = ledger.as_deref_mut() {
            l.record_origin(&event.origin, &event.packet);
        }
        // A simulcast video packet: which layer the light companion
        // carries and which the main track starts on.
        let layout = if event.packet.kind == MediaKind::Video && event.packet.rid.is_some() {
            let key = (event.origin.clone(), event.packet.mid);
            if let Some((_, known)) = layouts.iter().find(|(k, _)| *k == key) {
                *known
            } else {
                let found = router
                    .peer_mut(&event.origin)
                    .and_then(|p| p.simulcast_layout(event.packet.mid));
                layouts.push((key, found));
                found
            }
        } else {
            None
        };
        // Cross-call isolation: media is forwarded ONLY to
        // peers in the SAME call as the origin. Look up the
        // origin's call once; if the origin is gone (pruned)
        // or unbound (empty call), forward nothing. A peer in
        // a different call NEVER receives this packet.
        let origin = router.peers().iter().find(|p| p.id() == &event.origin);
        // A packet of the origin's declared light track goes to its
        // subscribers' light companions and to nothing else.
        let light_source = event.packet.kind == MediaKind::Video
            && origin.is_some_and(|p| p.light_mid() == Some(event.packet.mid));
        // A packet of its declared middle track goes to their middle
        // tracks and to nothing else.
        let middle_source = event.packet.kind == MediaKind::Video
            && origin.is_some_and(|p| p.middle_mid() == Some(event.packet.mid));
        let origin_call = origin.map(|p| p.call_id().to_owned());
        let Some(origin_call) = origin_call.filter(|c| !c.is_empty()) else {
            continue;
        };
        // Walk the peer list manually so we can borrow
        // each non-origin peer mutably one at a time.
        let n = router.len();
        for idx in 0..n {
            // Borrow check: re-borrow per-iteration so we
            // can compare ids + call before touching the
            // send stream. Skip the origin AND any peer not in
            // the origin's call.
            let skip = router.peers().get(idx).map_or(true, |p| {
                p.id() == &event.origin || p.call_id() != origin_call
            });
            if skip {
                continue;
            }
            let Some(peer) = router.peers_mut().get_mut(idx) else {
                continue;
            };
            if event.packet.is_fec() {
                // A publisher's FEC packet is forwarded to nobody and is
                // not a forward attempt. A subscriber with the pair is
                // told about it, so the SFU's own FEC packet for that
                // subscriber takes its place.
                peer.offer_publisher_fec(&event.origin, &event.packet);
                continue;
            }
            // Fairness: every non-origin peer is visited
            // exactly once per packet, and each outcome is
            // tallied so a starved subscriber (a spike in
            // `no_writer`) is observable in the metrics
            // rather than silently dropped.
            if let Some(layout) = layout {
                peer.apply_simulcast_layout(&event.origin, event.packet.mid, layout);
            }
            let attempts: [Option<(TrackRole, MediaForward)>; 2] = if light_source {
                let forwarded = peer.forward_light_track(&event.origin, &event.packet);
                [Some((TrackRole::LightCompanion, forwarded)), None]
            } else if middle_source {
                let forwarded = peer.forward_middle_track(&event.origin, &event.packet);
                [Some((TrackRole::Middle, forwarded)), None]
            } else {
                let forwarded = peer.forward_packet(&event.origin, &event.packet);
                // The light companion of a simulcast track takes the
                // light layer's packets as well; a packet that is not its
                // business is not an attempt.
                let light = peer.forward_light(&event.origin, &event.packet);
                [
                    Some((TrackRole::Main, forwarded)),
                    light.map(|f| (TrackRole::LightCompanion, f)),
                ]
            };
            for (role, forwarded) in attempts.into_iter().flatten() {
                if let Some(l) = ledger.as_deref_mut() {
                    l.record(peer.id(), &event.origin, event.packet.mid, role, &forwarded);
                }
                if role == TrackRole::LightCompanion && forwarded == MediaForward::Sent {
                    light_sent += 1;
                }
                if let MediaForward::WriteFailed(detail) = &forwarded {
                    tracing::warn!(
                        target_peer = %peer.id(),
                        role = role.as_str(),
                        error = %detail,
                        "media: forward write failed"
                    );
                }
                outcome.tally(&forwarded);
            }
        }
    }
    record_fanout_metrics(&outcome);
    if light_sent > 0 {
        metrics::counter!(crate::prom::names::LIGHT_COMPANION_PACKETS_FORWARDED_TOTAL)
            .increment(light_sent);
    }
    outcome
}

/// Collect the FEC packets written to subscribers since the last call
/// (during the drain and during the fanout), file them in `ledger` when one
/// is given, and return how many there were. A non-zero count means packets
/// are waiting in send queues, like a non-zero fanout.
fn collect_fec_sent(router: &mut MediaRouter, mut ledger: Option<&mut FanoutLedger>) -> u64 {
    let mut count = 0u64;
    for peer in router.peers_mut() {
        let sent = peer.take_fec_sent();
        if sent.is_empty() {
            continue;
        }
        count += sent.len() as u64;
        if let Some(l) = ledger.as_deref_mut() {
            for (origin, origin_mid, role) in &sent {
                l.record_fec_sent(peer.id(), origin, *origin_mid, *role);
            }
        }
    }
    count
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

/// What becomes of a request for a key frame on one publisher track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyframeAsk {
    /// It goes out now.
    Send,
    /// It is inside the rate limit's window: kept, and sent when the
    /// window ends.
    Owe,
    /// The track has been asked [`KEYFRAME_UNANSWERED_MAX`] times with no
    /// key frame seen since, and less than
    /// [`KEYFRAME_UNANSWERED_RETRY`] ago: not sent and not kept.
    Drop,
}

/// The whole decision for a request at `now` on a track last asked at
/// `last`, with `unanswered` requests sent since a key frame was last
/// seen arriving on it. Pure.
fn keyframe_ask(last: Option<Instant>, unanswered: u32, now: Instant) -> KeyframeAsk {
    if unanswered >= KEYFRAME_UNANSWERED_MAX
        && last.is_some_and(|l| now.saturating_duration_since(l) < KEYFRAME_UNANSWERED_RETRY)
    {
        return KeyframeAsk::Drop;
    }
    if keyframe_request_allowed(last, now) {
        KeyframeAsk::Send
    } else {
        KeyframeAsk::Owe
    }
}

/// Ask the publisher of `track` for a keyframe, rate-limited per publisher
/// track, and log it when one actually goes out. Returns whether it did.
///
/// The SFU holds no media of its own, so a subscriber that lacks a reference
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
    let sent = publisher.request_keyframe(
        track.source_mid,
        track.rid,
        now,
        reason == KeyframeReason::SubscriberRequest,
    );
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

/// Send, for every publisher, the keyframe requests the rate limit held back
/// and whose window has ended, and log each one that goes out.
pub(crate) fn send_owed_keyframe_requests(router: &mut MediaRouter, now: Instant) -> usize {
    let mut sent = 0;
    for peer in router.peers_mut() {
        let requests = peer.send_owed_keyframe_requests(now);
        for (mid, rid) in &requests {
            tracing::info!(
                origin = %peer.id(),
                mid = ?mid,
                rid = ?rid,
                reason = KeyframeReason::Owed.as_str(),
                "media: keyframe requested from publisher"
            );
        }
        sent += requests.len();
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
/// immediately, recovery upgrades with headroom hysteresis. The estimate is
/// the send-side one only; a subscriber's REMB is never a budget
/// ([`bwe_kind_bps`]). A no-op for a peer
/// without an estimate yet, or a track with fewer than two measured layers
/// (audio / non-simulcast video). The two-pass shape (snapshot every
/// publisher's per-layer bitrates, then mutate each subscriber, then request a
/// keyframe from each publisher whose layer changed) avoids a cross-peer borrow
/// conflict. On every selection the SFU asks the publisher for a keyframe (PLI)
/// for the newly-selected layer; the subscriber's track goes on forwarding its
/// current layer and switches at that keyframe once it is whole
/// ([`SwitchWait`]), so the switch starts on a decodable frame. Returns a
/// [`LayerSelectionSummary`] of the up/down selections made.
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

    // Publisher tracks whose uplink fallback is in effect.
    let held: std::collections::HashSet<(MediaPeerId, Mid)> = router
        .peers()
        .iter()
        .flat_map(|p| {
            p.uplink_fallbacks
                .iter()
                .filter(|(_, fb)| fb.restricted_to().is_some())
                .map(|(mid, _)| (p.id().clone(), *mid))
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
        // A light companion is exempt: it carries one layer for good.
        let decisions: Vec<(MediaPeerId, Mid, Option<Rid>)> = sub
            .tracks_out
            .iter()
            .filter(|e| e.role == TrackRole::Main)
            .map(|e| (e.source_peer.clone(), e.source_mid, e.chosen_rid))
            .collect();
        for (src_peer, src_mid, current) in decisions {
            let Some(layers) = snapshot.get(&src_peer).and_then(|m| m.get(&src_mid)) else {
                continue;
            };
            if layers.len() < 2 {
                continue; // not simulcast — nothing to choose
            }
            if held.contains(&(src_peer.clone(), src_mid)) {
                // The publisher's uplink fallback holds its subscribers on
                // the light layer; there is nothing lower to choose and
                // nothing higher is allowed. The lower of the two wins.
                continue;
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

/// How long the uplink fallback waits before it asks a publisher again
/// for a key frame on the light layer, while a subscriber's switch to it
/// has not happened.
///
/// Chosen, not measured. Each request makes the encoder start a new key
/// frame, and the one already on its way is still being repaired over the
/// uplink that is in trouble; asking sooner than a key frame takes to get
/// through throws that work away.
const FALLBACK_KEYFRAME_RETRY: Duration = Duration::from_secs(1);

/// What [`drive_uplink_fallback`] did in one pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UplinkFallbackSummary {
    /// Publisher tracks that went from full to fallback.
    pub(crate) to_fallback: u64,
    /// Publisher tracks that went from fallback to full.
    pub(crate) to_full: u64,
    /// Keyframe requests that went out to publishers. Non-zero means
    /// something is waiting in a publisher's send queue.
    pub(crate) keyframes_requested: u64,
}

/// One simulcast publisher track as [`drive_uplink_fallback`] found it.
struct FallbackTrack {
    origin: MediaPeerId,
    mid: Mid,
    /// The layers' measured bitrates, ascending.
    layers: Vec<(Rid, u64)>,
    /// The heaviest layer, by the highest bitrate each layer has shown;
    /// `None` until every layer has been measured.
    heaviest: Option<Rid>,
    /// The light layer its subscribers are held on, while in fallback.
    held: Option<Rid>,
}

/// The uplink fallback, once per pass of the media loop
/// ([`crate::uplink_fallback`]).
///
/// For every simulcast video track of every publisher the track's policy is
/// moved to `now`. Then, per subscriber of such a track:
///
/// - **The track is in fallback.** A subscriber selected to any other layer
///   is selected to the light one, and the publisher is asked for a keyframe
///   on the light layer at once, outside the rate limit. The layer the
///   subscriber was selected to is remembered. As with every selection the
///   switch itself happens at a key frame of the light layer, once it is
///   whole at the SFU ([`SwitchWait`]), and until then the old layer keeps
///   being forwarded. For as long as a subscriber's switch has
///   not happened, the request is repeated every
///   [`FALLBACK_KEYFRAME_RETRY`]: the keyframe, or the request for it,
///   travels the path that is in trouble.
/// - **The track has just left fallback.** Each subscriber it held goes to
///   what the subscriber-bandwidth policy wants: with an estimate for the
///   subscriber, [`choose_layer_for_budget`] starting from the remembered
///   layer; without one, the heaviest layer. The publisher is asked for a
///   keyframe on that layer.
/// - **The track is full and was not held.** A subscriber the SFU has no
///   estimate for and that sits on a layer other than the heaviest is
///   selected to the heaviest. Without this a subscriber stays on whichever
///   layer reached the SFU first, which for a publisher that lists its light
///   layer first is the light one; and send-side estimation is off in
///   production, so nothing else would ever move it.
///
/// [`drive_layer_selection`] leaves a track in fallback alone, so the two
/// policies combine as the lower of the two.
///
/// With the fallback switched off, which is the default
/// (`LAKKO_SFU_UPLINK_FALLBACK` turns it on), no track has a policy, so none
/// is ever held; the last rule still applies.
///
/// Only main tracks are selected here. A light companion
/// ([`TrackRole::LightCompanion`]) carries the light layer for good.
///
/// A publisher without simulcast has sent no packet with a rid and is not
/// looked at; with none in the router this returns before touching any
/// subscriber.
pub(crate) fn drive_uplink_fallback(
    router: &mut MediaRouter,
    now: Instant,
) -> UplinkFallbackSummary {
    let mut summary = UplinkFallbackSummary::default();
    let tracks = poll_uplink_fallbacks(router, now, &mut summary);
    if tracks.is_empty() {
        return summary;
    }
    let requests = select_for_uplink_fallback(router, &tracks);
    summary.keyframes_requested = send_fallback_keyframe_requests(router, requests, now);
    summary
}

/// Pass 1 of [`drive_uplink_fallback`] (publishers): move every policy to
/// `now`, log and count each change of state, and return every simulcast
/// track found.
fn poll_uplink_fallbacks(
    router: &mut MediaRouter,
    now: Instant,
    summary: &mut UplinkFallbackSummary,
) -> Vec<FallbackTrack> {
    use crate::prom::names::UPLINK_FALLBACK_SWITCHES_TOTAL;
    let mut tracks: Vec<FallbackTrack> = Vec::new();
    for peer in router.peers_mut() {
        if peer.layer_meters.is_empty() {
            continue;
        }
        let origin = peer.id.clone();
        // Every track this peer has sent a packet with a rid on.
        let mut mids: Vec<Mid> = peer.layer_meters.keys().map(|(mid, _)| *mid).collect();
        mids.sort_unstable();
        mids.dedup();
        for mid in mids {
            let layers = peer.layer_bitrates_for(mid);
            let mut peaks = Vec::with_capacity(layers.len());
            for (rid, bps) in &layers {
                let peak = peer.layer_peak_bps.entry((mid, *rid)).or_insert(0);
                *peak = (*peak).max(*bps);
                peaks.push((*rid, *peak));
            }
            let light = uplink_fallback::pick_light(&peaks, Rid::from(LIGHT_RID));
            // No policy when the fallback is switched off: the track is
            // then never held, and only the default layer applies.
            let transition = peer
                .uplink_fallbacks
                .get_mut(&mid)
                .and_then(|policy| policy.poll(now, light));
            if let Some(t) = transition {
                match t.direction {
                    uplink_fallback::Direction::ToFallback => summary.to_fallback += 1,
                    uplink_fallback::Direction::ToFull => summary.to_full += 1,
                }
                metrics::counter!(
                    UPLINK_FALLBACK_SWITCHES_TOTAL,
                    "direction" => t.direction.to_str()
                )
                .increment(1);
                tracing::info!(
                    origin = %origin,
                    mid = ?mid,
                    from = t.direction.from_str(),
                    to = t.direction.to_str(),
                    light = ?t.light,
                    reason = t.reason.as_str(),
                    oldest_hole_ms = ?t.oldest_hole.map(|d| d.as_millis()),
                    "media: publisher uplink fallback changed state"
                );
            }
            tracks.push(FallbackTrack {
                origin: origin.clone(),
                mid,
                layers,
                heaviest: peer
                    .simulcast_layout(mid)
                    .and_then(|layout| layout.heaviest)
                    .or_else(|| uplink_fallback::pick_heaviest(&peaks)),
                held: peer.uplink_fallback_layer(mid),
            });
        }
    }
    tracks
}

/// A keyframe request [`drive_uplink_fallback`] wants made: publisher, mid,
/// layer, why, and whether it goes out at once (a first request) or only
/// when [`FALLBACK_KEYFRAME_RETRY`] has passed since the last (a repeat).
type FallbackKeyframeRequest = (MediaPeerId, Mid, Rid, KeyframeReason, bool);

/// Pass 2 of [`drive_uplink_fallback`] (subscribers): select layers and
/// return the keyframe requests that follow from it.
fn select_for_uplink_fallback(
    router: &mut MediaRouter,
    tracks: &[FallbackTrack],
) -> Vec<FallbackKeyframeRequest> {
    let mut requests: Vec<FallbackKeyframeRequest> = Vec::new();
    for track in tracks {
        let heaviest = track.heaviest;
        for sub in router.peers_mut() {
            if sub.id == track.origin {
                continue;
            }
            let budget = sub.egress_estimate_bps;
            for entry in &mut sub.tracks_out {
                // The main track only: a light companion never changes
                // layer.
                if entry.source_peer != track.origin
                    || entry.source_mid != track.mid
                    || entry.role != TrackRole::Main
                {
                    continue;
                }
                if let Some(light) = track.held {
                    if !entry.forwarding.held_light {
                        entry.forwarding.held_light = true;
                        entry.forwarding.before_fallback = entry.chosen_rid;
                    }
                    if entry.chosen_rid != Some(light) {
                        entry.forwarding.waiting.clear();
                        entry.chosen_rid = Some(light);
                        requests.push((
                            track.origin.clone(),
                            track.mid,
                            light,
                            KeyframeReason::UplinkFallback,
                            true,
                        ));
                    } else if entry
                        .forwarding
                        .active_rid
                        .is_some_and(|on_wire| on_wire != light)
                    {
                        // Selected, not switched yet: ask again, inside
                        // the rate limit.
                        requests.push((
                            track.origin.clone(),
                            track.mid,
                            light,
                            KeyframeReason::UplinkFallback,
                            false,
                        ));
                    }
                    continue;
                }
                let (target, reason) = if entry.forwarding.held_light {
                    entry.forwarding.held_light = false;
                    let before = entry.forwarding.before_fallback.take();
                    let target = budget.map_or(heaviest.or(before), |budget| {
                        choose_layer_for_budget(&track.layers, budget, before.or(heaviest))
                    });
                    (target, KeyframeReason::UplinkRestore)
                } else if budget.is_none() && entry.chosen_rid.is_some() {
                    (heaviest, KeyframeReason::FullLayerDefault)
                } else {
                    continue;
                };
                let Some(target) = target else {
                    continue;
                };
                if entry.chosen_rid != Some(target) {
                    entry.forwarding.waiting.clear();
                    entry.chosen_rid = Some(target);
                    requests.push((track.origin.clone(), track.mid, target, reason, true));
                }
            }
        }
    }

    requests
}

/// Pass 3 of [`drive_uplink_fallback`] (publishers): make the keyframe
/// requests, each layer once; a first request wins over a repeat for the
/// same layer. Returns how many went out.
fn send_fallback_keyframe_requests(
    router: &mut MediaRouter,
    mut requests: Vec<FallbackKeyframeRequest>,
    now: Instant,
) -> u64 {
    let mut sent_count = 0;
    requests.sort_by_key(|(_, _, _, _, at_once)| !*at_once);
    let mut asked: Vec<(MediaPeerId, Mid, Rid)> = Vec::new();
    for (origin, mid, rid, reason, at_once) in requests {
        let key = (origin, mid, rid);
        if asked.contains(&key) {
            continue;
        }
        let Some(publisher) = router.peer_mut(&key.0) else {
            continue;
        };
        // A repeat is not owed when it is too early: the next pass asks
        // again if the switch is still pending.
        let due = at_once
            || publisher
                .last_keyframe_request
                .get(&(mid, Some(rid)))
                .map_or(true, |last| {
                    now.saturating_duration_since(*last) >= FALLBACK_KEYFRAME_RETRY
                });
        let sent = due && publisher.request_layer_keyframe_at(mid, rid, now);
        if sent {
            sent_count += 1;
            tracing::info!(
                origin = %key.0,
                mid = ?mid,
                rid = ?rid,
                reason = reason.as_str(),
                "media: keyframe requested from publisher"
            );
        }
        asked.push(key);
    }
    sent_count
}

/// Two-track delivery of simulcast video, once per pass of the media loop
/// ([`TrackRole`]).
///
/// For every simulcast video track of every publisher, each subscriber's
/// downstream tracks that have no layer yet are given theirs
/// ([`MediaPeer::apply_simulcast_layout`]): the light companion the light
/// layer, the main track the heaviest. The same is done per packet in the
/// fanout; this covers a track that opened while no packet was flowing.
///
/// Then every track that is waiting for a whole key frame to begin at has
/// its publisher asked for one on its layer
/// ([`MediaPeer::due_start_keyframes`]): at once when the wait starts, and
/// again every [`START_KEYFRAME_RETRY`] until the track has begun. The
/// request goes through the per-track rate limit; one that falls inside
/// the window is owed and goes out when the window ends, so it is never
/// dropped. Returns how many requests went out in this pass.
///
/// With no simulcast publisher in the router this returns before touching
/// any subscriber.
pub(crate) fn drive_two_track_delivery(router: &mut MediaRouter, now: Instant) -> usize {
    let mut layouts: Vec<(MediaPeerId, Mid, SimulcastLayout)> = Vec::new();
    for peer in router.peers_mut() {
        let mut mids: Vec<Mid> = peer
            .tracks_in
            .iter()
            .filter(|t| t.simulcast)
            .map(|t| t.mid)
            .chain(peer.layer_meters.keys().map(|(mid, _)| *mid))
            .collect();
        mids.sort_unstable();
        mids.dedup();
        for mid in mids {
            if let Some(layout) = peer.simulcast_layout(mid) {
                layouts.push((peer.id.clone(), mid, layout));
            }
        }
    }
    if layouts.is_empty() {
        return 0;
    }
    let mut due: Vec<OpenedTrack> = Vec::new();
    for sub in router.peers_mut() {
        for (origin, mid, layout) in &layouts {
            if sub.id != *origin {
                sub.apply_simulcast_layout(origin, *mid, *layout);
            }
        }
        due.extend(sub.due_start_keyframes(now));
    }
    due.iter()
        .filter(|track| {
            request_keyframe_from_publisher(router, track, now, KeyframeReason::TrackStart)
        })
        .count()
}

/// File the uplink fallback state of every simulcast publisher track in
/// `ledger`, for the per-interval publisher line.
fn note_uplink_fallback_states(router: &MediaRouter, ledger: &mut FanoutLedger) {
    for peer in router.peers() {
        for mid in peer.uplink_fallbacks.keys() {
            if let Some(state) = peer.uplink_fallback_state(*mid) {
                ledger.note_uplink_state(peer.id(), *mid, state);
            }
        }
    }
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
        // would later let the fanout forward across calls).
        // Same origin-call lookup as the fanout path.
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

    /// The 24-bit send time survives the round trip through the `Instant`
    /// str0m makes of it, whichever 64-second cycle str0m placed it in and
    /// whether that is before or after the fixed base.
    #[test]
    fn abs_send_time_24_is_the_same_modulo_64_seconds() {
        let base = Instant::now() + Duration::from_secs(1000);
        let units =
            |micros: u64| u32::try_from(u128::from(micros) * (1 << 18) / 1_000_000).unwrap();
        for micros in [0u64, 1, 3_815, 1_000_000, 31_999_999, 63_999_990] {
            let want = units(micros);
            for cycles in [0u64, 1, 5] {
                let after = base + Duration::from_micros(micros + cycles * 64_000_000);
                assert_eq!(abs_send_time_24(after, base), want, "{micros} +{cycles}");
                let before = base
                    .checked_sub(Duration::from_micros((cycles + 1) * 64_000_000 - micros))
                    .unwrap();
                let got = abs_send_time_24(before, base);
                // Rounding of the complement may differ by one unit.
                assert!(
                    got.abs_diff(want) <= 1,
                    "{micros} -{cycles}: {got} vs {want}"
                );
            }
        }
        assert!(abs_send_time_24(base, base) == 0);
    }

    #[test]
    fn server_bitrate_control_drops_the_transport_wide_sequence_number_too() {
        let map = session_extension_map(false);
        let with = session_extension_map(true);
        let standard = ExtensionMap::standard();
        assert!(map.id_of(Extension::TransportSequenceNumber).is_none());
        assert_eq!(
            with.id_of(Extension::TransportSequenceNumber),
            standard.id_of(Extension::TransportSequenceNumber)
        );
        assert!(standard.id_of(Extension::TransportSequenceNumber).is_some());
        // The standard map less the audio level, and less transport-cc
        // where that is asked for; nothing else goes.
        assert_eq!(with.iter().count() + 1, standard.iter().count());
        assert_eq!(map.iter().count() + 2, standard.iter().count());
        for ext in [
            Extension::AbsoluteSendTime,
            Extension::RtpMid,
            Extension::RtpStreamId,
            Extension::RepairedRtpStreamId,
            Extension::VideoOrientation,
        ] {
            assert_eq!(map.id_of(ext.clone()), standard.id_of(ext.clone()));
            assert_eq!(with.id_of(ext.clone()), standard.id_of(ext));
        }
    }

    const AUDIO_LEVEL_URI: &str = "urn:ietf:params:rtp-hdrext:ssrc-audio-level";

    /// One peer per way the SFU builds an `Rtc`: client-side bitrate
    /// control, server-side, and server-side with downlink control; with
    /// and without the optional extensions.
    fn peers_of_every_configuration() -> Vec<MediaPeer> {
        let now = Instant::now();
        let server = || BitrateControl::Server(EstimatorConfig::default());
        let mut peers = Vec::new();
        for video_fec in [false, true] {
            for protection in [LossProtection::default(), LossProtection::off()] {
                for (bitrate, downlink) in [
                    (BitrateControl::Client, false),
                    (server(), false),
                    (server(), true),
                ] {
                    peers.push(MediaPeer::with_downlink_control(
                        pid("p"),
                        now,
                        None,
                        video_fec,
                        bitrate,
                        protection,
                        downlink,
                    ));
                }
            }
        }
        peers
    }

    /// The audio level (RFC 6464) is in no session map the SFU builds,
    /// although str0m's own default has it.
    #[test]
    fn no_session_extension_map_holds_the_audio_level() {
        assert!(
            ExtensionMap::standard()
                .id_of(Extension::AudioLevel)
                .is_some(),
            "str0m's default map has it: the reason the SFU replaces the map"
        );
        for transport_cc in [false, true] {
            let map = session_extension_map(transport_cc);
            assert!(map.id_of(Extension::AudioLevel).is_none());
            assert!(map.iter().all(|(_, ext)| *ext != Extension::AudioLevel));
        }
        for peer in peers_of_every_configuration() {
            assert!(
                peer.rtc
                    .codec_config()
                    .params()
                    .iter()
                    .any(|p| p.spec().codec == Codec::Opus),
                "opus is still configured"
            );
        }
    }

    /// A publisher that offers the audio level, as every libwebrtc client
    /// does, is answered without it; so it does not send it.
    #[test]
    fn the_answer_to_a_publisher_does_not_accept_the_audio_level() {
        for mut peer in peers_of_every_configuration() {
            let mut client = Rtc::builder().build(Instant::now());
            let mut change = client.sdp_api();
            change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
            change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
            let (offer, _pending) = change.apply().expect("an offer");
            let offer = offer.to_sdp_string();
            assert!(
                offer.contains(AUDIO_LEVEL_URI),
                "the client offers it:\n{offer}"
            );
            let answer = peer.accept_offer(&offer).expect("the SFU answers");
            assert!(
                !answer.contains("audio-level"),
                "not echoed: {}",
                extmap_summary(&answer)
            );
            assert!(
                answer.contains("m=audio") && answer.contains("opus/48000/2"),
                "audio itself is accepted:\n{answer}"
            );
            assert!(answer.contains("urn:ietf:params:rtp-hdrext:sdes:mid"));
            // And the line negotiated no id for it: a value sent anyway
            // would be parsed into nothing.
            let mids: Vec<&str> = answer
                .lines()
                .filter_map(|l| l.strip_prefix("a=mid:"))
                .collect();
            assert_eq!(mids.len(), 2, "{answer}");
            for mid in mids {
                let media = peer.rtc.media(Mid::from(mid)).expect("the line exists");
                assert!(media.remote_extmap().id_of(Extension::AudioLevel).is_none());
            }
        }
    }

    /// The SFU's own offer of an audio track to a subscriber carries no
    /// audio-level extension, whatever the configuration.
    #[test]
    fn the_offer_to_a_subscriber_does_not_carry_the_audio_level() {
        for mut peer in peers_of_every_configuration() {
            for kind in [MediaKind::Audio, MediaKind::Video] {
                peer.observe_remote_track_in(
                    pid("pub"),
                    TrackIn {
                        mid: Mid::new(),
                        kind,
                        simulcast: false,
                        light: false,
                        middle: false,
                    },
                );
            }
            let offer = peer
                .negotiate_if_needed()
                .expect("negotiates")
                .expect("an offer");
            assert!(
                offer.contains("m=audio") && offer.contains("opus/48000/2"),
                "an audio line is offered:\n{offer}"
            );
            assert!(
                !offer.contains("audio-level"),
                "not offered: {}",
                extmap_summary(&offer)
            );
        }
    }

    /// Under server-side bitrate control no payload type keeps
    /// `transport-cc` feedback and every other feedback flag is as it is
    /// under client-side control; the codec list is otherwise identical.
    #[test]
    fn server_bitrate_control_changes_nothing_of_the_codecs_but_transport_cc() {
        let server = BitrateControl::Server(EstimatorConfig::default());
        for video_fec in [false, true] {
            let now = Instant::now();
            let client_peer = MediaPeer::with_options(pid("c"), now, None, video_fec);
            let server_peer =
                MediaPeer::with_bitrate_control(pid("s"), now, None, video_fec, server);
            let client_params = client_peer.rtc.codec_config().params();
            let server_params = server_peer.rtc.codec_config().params();
            assert_eq!(client_params.len(), server_params.len());
            assert!(client_params.iter().any(PayloadParams::fb_transport_cc));
            for (c, s) in client_params.iter().zip(server_params) {
                assert!(!s.fb_transport_cc(), "{s:?}");
                assert_eq!(c.pt(), s.pt());
                assert_eq!(c.resend(), s.resend());
                assert_eq!(c.spec(), s.spec());
                assert_eq!(c.fb_remb(), s.fb_remb());
                assert_eq!(c.fb_nack(), s.fb_nack());
                assert_eq!(c.fb_pli(), s.fb_pli());
                assert_eq!(c.fb_fir(), s.fb_fir());
            }
            assert!(client_peer.uplink.is_none());
            assert!(server_peer.uplink.is_some());
        }
    }

    /// Bitrate control does nothing for a peer under client-side control,
    /// and nothing for a publisher that has sent no video or whose
    /// transport is not up.
    #[test]
    fn drive_uplink_is_a_no_op_without_a_connected_video_publisher() {
        let now = Instant::now();
        let mut client = MediaPeer::new(pid("c"), now);
        assert!(!client.drive_uplink(now + Duration::from_secs(5)));
        assert!(client.uplink_snapshot(now).is_none());
        client.observe_uplink(
            UplinkSeen {
                abs_send_time: Some(now),
                size: 1200,
            },
            &synthetic_video_packet(),
        );
        assert!(client.uplink.is_none());

        let server = BitrateControl::Server(EstimatorConfig::default());
        let mut peer = MediaPeer::with_bitrate_control(pid("s"), now, None, false, server);
        assert!(
            !peer.drive_uplink(now + Duration::from_secs(5)),
            "no video yet"
        );
        let snap = peer
            .uplink_snapshot(now)
            .expect("an estimate exists from the start");
        assert_eq!(snap.estimate_bps, crate::uplink_estimate::DEFAULT_START_BPS);
        assert_eq!(snap.last_change.as_str(), "start");
        peer.observe_uplink(
            UplinkSeen {
                abs_send_time: Some(now),
                size: 1200,
            },
            &synthetic_video_packet(),
        );
        assert_eq!(peer.uplink.as_ref().map(|u| u.video_ssrcs.len()), Some(1));
        // An audio packet is counted and names no REMB stream.
        peer.observe_uplink(
            UplinkSeen {
                abs_send_time: None,
                size: 100,
            },
            &synthetic_packet(),
        );
        assert_eq!(peer.uplink.as_ref().map(|u| u.video_ssrcs.len()), Some(1));
        assert!(
            !peer.drive_uplink(now + Duration::from_secs(6)),
            "the transport is not connected"
        );
        let mut router = MediaRouter::new();
        router.add_peer(peer);
        assert_eq!(
            drive_bitrate_control(&mut router, now + Duration::from_secs(7)),
            0
        );
    }

    #[test]
    fn the_router_hands_its_bitrate_control_to_the_peers_it_creates() {
        let now = Instant::now();
        let mut router = MediaRouter::new();
        assert!(router.peer_or_insert(pid("before"), now).uplink.is_none());
        router.set_bitrate_control(BitrateControl::Server(EstimatorConfig::default()));
        assert!(router.peer_or_insert(pid("after"), now).uplink.is_some());
        assert!(router.peer_or_insert(pid("before"), now).uplink.is_none());
        assert_eq!(BitrateControl::Client.as_str(), "client");
        assert_eq!(
            BitrateControl::Server(EstimatorConfig::default()).as_str(),
            "server"
        );
        assert_eq!(BitrateControl::default(), BitrateControl::Client);
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
                light_mids: Vec::new(),
                middle_mids: Vec::new(),
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

    /// `forward_packet` on a freshly-constructed
    /// `MediaPeer` returns [`MediaForward::NoWriter`] —
    /// the destination peer has no negotiated outgoing
    /// media-line yet, so the SFU has no send stream to
    /// write to. This is the expected steady-state outcome
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
        let synthetic = synthetic_packet();
        // A fresh peer has no `tracks_out` entry for the
        // synthetic source, so `forward_packet`
        // short-circuits to `NoWriter` before even
        // looking for a send stream. The source id is
        // arbitrary — the test guarantees the variant
        // mapping, not any particular topology.
        let source = pid("bob");
        let outcome = p.forward_packet(&source, &synthetic);
        assert_eq!(
            outcome,
            MediaForward::NoWriter,
            "no tracks_out entry → NoWriter"
        );
    }

    /// `fanout_packets` is a no-op when the router
    /// has a single peer — there is no non-origin peer
    /// to forward to, so the call must return without
    /// touching state.
    #[test]
    fn fanout_with_single_peer_is_noop() {
        let mut r = MediaRouter::new();
        r.add_peer(MediaPeer::new(pid("alice"), Instant::now()));
        let pending = vec![PendingFanout {
            origin: pid("alice"),
            packet: synthetic_packet(),
        }];
        // Smoke — must return without panic and leave
        // the router untouched.
        fanout_packets(&mut r, &pending);
        assert_eq!(r.len(), 1);
    }

    /// `fanout_packets` skips the origin peer when
    /// forwarding — the origin must not receive its own
    /// echo. Because none of the destination peers have
    /// a send stream for the synthetic mid, the call is a
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
            packet: synthetic_packet(),
        }];
        fanout_packets(&mut r, &pending);
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
        fanout_packets(&mut r, &[]);
        assert_eq!(r.len(), 3);
    }

    /// Synthetic publisher packet for tests — a fabricated
    /// mid and Opus payload parameters that match no real
    /// session, so `forward_packet` on a bare peer always
    /// stops at `NoWriter`. The fields the fanout path reads
    /// all take values str0m's API accepts.
    fn synthetic_packet() -> RelayPacket {
        use str0m::format::{Codec, CodecSpec, FormatParams};
        use str0m::media::{Frequency, Pt};
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
        RelayPacket {
            mid: Mid::new(),
            rid: None,
            kind: MediaKind::Audio,
            ssrc: 0x1234_5678.into(),
            seq_no: 0_u64.into(),
            rtp_time: 0,
            marker: false,
            params,
            red: None,
            recovered: false,
            arrival: Instant::now(),
            video_orientation: None,
            descriptor: None,
            starts_independent_frame: false,
            blind: false,
            payload: vec![0; 16],
        }
    }

    /// A synthetic VP8 video packet on the same footing.
    fn synthetic_video_packet() -> RelayPacket {
        use str0m::format::{Codec, CodecSpec, FormatParams};
        use str0m::media::{Frequency, Pt};
        let pt: Pt = 97.into();
        RelayPacket {
            kind: MediaKind::Video,
            params: PayloadParams::new(
                pt,
                None,
                CodecSpec {
                    codec: Codec::Vp8,
                    channels: None,
                    clock_rate: Frequency::NINETY_KHZ,
                    format: FormatParams::default(),
                },
            ),
            ..synthetic_packet()
        }
    }

    // ============================================================
    // Fanout fairness (C-39 follow-up)
    // ============================================================

    /// A single origin packet in an N-peer call is offered to
    /// EVERY non-origin peer exactly once — never to the
    /// origin, never twice, never skipping one. Unit-test
    /// peers have no negotiated send stream, so every attempt lands
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

    /// A track that shows no key frame is asked a bounded number of
    /// times at the rate limit, then once per long wait; what comes in
    /// between is dropped, not owed. A track that answers is never
    /// bounded.
    #[test]
    fn a_track_that_shows_no_key_frame_is_asked_a_bounded_number_of_times() {
        let t0 = Instant::now();
        // Asked every 100 ms for a minute, never answered.
        let mut last: Option<Instant> = None;
        let mut unanswered = 0u32;
        let mut sent = Vec::new();
        for step in 0..600u64 {
            let now = t0 + Duration::from_millis(100 * step);
            if keyframe_ask(last, unanswered, now) == KeyframeAsk::Send {
                last = Some(now);
                unanswered += 1;
                sent.push(now.duration_since(t0));
            }
        }
        // Five at the rate limit, then one every ten seconds.
        let burst = KEYFRAME_UNANSWERED_MAX as usize;
        assert_eq!(sent.len(), burst + 5, "{sent:?}");
        assert!(sent[burst - 1] <= KEYFRAME_REQUEST_MIN_INTERVAL * KEYFRAME_UNANSWERED_MAX);
        for pair in sent[burst - 1..].windows(2) {
            assert!(
                pair[1].saturating_sub(pair[0]) >= KEYFRAME_UNANSWERED_RETRY,
                "{sent:?}"
            );
        }
        // Inside the long wait a request is dropped, not owed.
        let asked = t0;
        assert_eq!(
            keyframe_ask(
                Some(asked),
                KEYFRAME_UNANSWERED_MAX,
                asked + Duration::from_secs(1)
            ),
            KeyframeAsk::Drop
        );
        assert_eq!(
            keyframe_ask(
                Some(asked),
                KEYFRAME_UNANSWERED_MAX,
                asked + KEYFRAME_UNANSWERED_RETRY
            ),
            KeyframeAsk::Send
        );
        // Below the bound the rate limit alone decides, and owes.
        assert_eq!(
            keyframe_ask(Some(asked), 1, asked + Duration::from_millis(100)),
            KeyframeAsk::Owe
        );
        assert_eq!(
            keyframe_ask(Some(asked), 0, asked + KEYFRAME_REQUEST_MIN_INTERVAL),
            KeyframeAsk::Send
        );
        assert_eq!(keyframe_ask(None, 0, t0), KeyframeAsk::Send);
    }

    /// A key frame seen arriving on a track settles what was owed on it
    /// and starts the count of unanswered requests again; another track's
    /// debts are untouched.
    #[test]
    fn a_key_frame_settles_the_debts_of_its_track() {
        let mut publisher = peer_in_call("publisher", "call");
        let (light, middle) = (Mid::new(), Mid::new());
        for mid in [light, middle] {
            publisher.owed_keyframe_requests.insert((mid, None));
            publisher
                .unanswered_keyframe_requests
                .insert((mid, None), KEYFRAME_UNANSWERED_MAX);
        }
        publisher.settle_keyframe_requests(light, None);
        assert!(!publisher.owed_keyframe_requests.contains(&(light, None)));
        assert_eq!(publisher.unanswered_keyframe_requests(light, None), 0);
        assert!(publisher.owed_keyframe_requests.contains(&(middle, None)));
        assert_eq!(
            publisher.unanswered_keyframe_requests(middle, None),
            KEYFRAME_UNANSWERED_MAX
        );
        // Nothing is owed any more: nothing goes out when the window ends.
        publisher.owed_keyframe_requests.clear();
        assert!(publisher
            .send_owed_keyframe_requests(Instant::now() + Duration::from_secs(60))
            .is_empty());
    }

    /// A subscriber that goes on asking although it is being answered is
    /// relayed less and less often; one that asks again after a quiet
    /// stretch is relayed at once and starts over. Each of a subscriber's
    /// tracks is counted on its own.
    #[test]
    fn a_subscriber_that_keeps_asking_is_relayed_less_and_less_often() {
        let t0 = Instant::now();
        let mut sub = peer_in_call("sub", "call");
        let track = Mid::new();
        let other = Mid::new();
        // Asking every 200 ms for two minutes, as a receiver that cannot
        // decode the stream does.
        let mut relayed = Vec::new();
        for step in 0..600u64 {
            let now = t0 + Duration::from_millis(200 * step);
            if sub.subscriber_pli_due(track, now) {
                relayed.push(now.duration_since(t0).as_secs());
            }
        }
        // At once, then after 1, 2 and 4 s, then every 8 s.
        assert_eq!(relayed[..5], [0, 1, 3, 7, 15], "{relayed:?}");
        for pair in relayed[4..].windows(2) {
            assert_eq!(pair[1] - pair[0], 8, "{relayed:?}");
        }
        assert_eq!(relayed.len(), 18, "{relayed:?}");
        // The other track of the same subscriber has its own count.
        assert!(sub.subscriber_pli_due(other, t0 + Duration::from_secs(100)));
    }

    /// A subscriber that stops asking has what it asked for. Its next
    /// request, after a quiet stretch, is a new need: relayed at once,
    /// and again after the first interval while it keeps asking.
    #[test]
    fn a_subscriber_that_went_quiet_is_relayed_at_once_when_it_asks_again() {
        let t0 = Instant::now();
        let mut sub = peer_in_call("sub", "call");
        let track = Mid::new();
        let ms = Duration::from_millis;
        // Asks for twenty seconds (its wait is at the longest), stops.
        for step in 0..100u64 {
            let _ = sub.subscriber_pli_due(track, t0 + ms(200 * step));
        }
        let stopped = t0 + ms(200 * 99);
        // Still inside the long wait and not yet quiet: not relayed.
        assert!(!sub.subscriber_pli_due(track, stopped + ms(300)));
        // Quiet for the stated time, then it needs a key frame again.
        let again = stopped + ms(300) + PLI_RELAY_QUIET;
        assert!(sub.subscriber_pli_due(track, again), "relayed at once");
        assert!(!sub.subscriber_pli_due(track, again + ms(200)));
        assert!(!sub.subscriber_pli_due(track, again + ms(800)));
        assert!(
            sub.subscriber_pli_due(track, again + PLI_RELAY_FIRST_INTERVAL),
            "and again a second later while it keeps asking"
        );
    }

    /// A subscriber's own request is never refused for the track having
    /// shown no key frame: on a stream without a frame descriptor the SFU
    /// sees none, and the subscriber is the one that knows. The SFU's own
    /// requests on such a track are bounded as before.
    #[test]
    fn a_subscribers_request_is_not_counted_against_the_unanswered_bound() {
        let mut publisher = peer_in_call("publisher", "call");
        let (mid, t0) = (Mid::new(), Instant::now());
        publisher
            .unanswered_keyframe_requests
            .insert((mid, None), KEYFRAME_UNANSWERED_MAX);
        publisher.last_keyframe_request.insert((mid, None), t0);
        let soon = t0 + Duration::from_millis(100);
        let later = t0 + Duration::from_secs(2);
        // The SFU's own: dropped, neither sent nor owed.
        assert!(!publisher.request_keyframe(mid, None, soon, false));
        assert!(!publisher.owed_keyframe_requests.contains(&(mid, None)));
        assert!(!publisher.request_keyframe(mid, None, later, false));
        assert!(!publisher.owed_keyframe_requests.contains(&(mid, None)));
        // A subscriber's: inside the rate limit's window it is owed, and
        // after it nothing but the missing stream keeps it from going
        // out (this peer has no receive stream to ask).
        assert!(!publisher.request_keyframe(mid, None, soon, true));
        assert!(publisher.owed_keyframe_requests.contains(&(mid, None)));
        assert_eq!(
            keyframe_ask(Some(t0), 0, later),
            KeyframeAsk::Send,
            "what a subscriber's request is judged by"
        );
    }

    /// A resuming track whose publisher sends no frame descriptor starts
    /// at the next packet: nothing in such a packet says where a key
    /// frame begins, so there is nothing to wait for.
    #[test]
    fn a_resuming_track_without_frame_descriptors_starts_at_the_next_packet() {
        let t0 = Instant::now();
        let source = pid("pub");
        let mut sub = three_track_subscriber("pub");
        sub.set_video_wants(&[want("pub", VideoQuality::Light)]);
        sub.set_video_wants(&[]);
        assert_eq!(sub.due_resume_keyframes(t0, &[]).len(), 1, "asked once");
        assert!(sub.track_paused(&source, Mid::from(MAIN_MID), TrackRole::Main));
        let blind = RelayPacket {
            blind: true,
            ..video_packet_on(MAIN_MID, 150, false)
        };
        assert_ne!(
            sub.forward_packet(&source, &blind),
            MediaForward::NotWanted,
            "not held back"
        );
        assert!(!sub.track_paused(&source, Mid::from(MAIN_MID), TrackRole::Main));
        // Flowing: the publisher is not asked again for the resume.
        assert!(sub
            .due_resume_keyframes(t0 + Duration::from_secs(10), &[])
            .is_empty());
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
                simulcast: false,
                light: false,
                middle: false,
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
                role: TrackRole::Main,
                rid: None,
            },
            OpenedTrack {
                source_peer: pid("nobody"),
                source_mid: Mid::new(),
                kind: MediaKind::Video,
                role: TrackRole::Main,
                rid: None,
            },
            // A video track on a known publisher with no live StreamRx: the
            // PLI has nowhere to go, so it is reported as not sent — never
            // as sent-and-logged.
            OpenedTrack {
                source_peer: pid("pub"),
                source_mid: Mid::new(),
                kind: MediaKind::Video,
                role: TrackRole::Main,
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
        assert_eq!(KeyframeReason::UplinkBurstEnd.as_str(), "uplink-burst-end");
        assert_eq!(KeyframeReason::UplinkFallback.as_str(), "uplink-fallback");
        assert_eq!(KeyframeReason::UplinkRestore.as_str(), "uplink-restore");
        assert_eq!(
            KeyframeReason::FullLayerDefault.as_str(),
            "full-layer-default"
        );
    }

    #[test]
    fn every_peer_runs_str0m_in_rtp_mode_and_starts_unconnected() {
        // RTP mode is what makes str0m hand packets over as they arrive
        // (ADR-0066). The frame-level writer refuses to exist in that mode,
        // so a peer that could still be given one was not built that way.
        // The assertion goes through the one observable str0m offers
        // without a session: `Rtc::writer` panics in RTP mode.
        let mut p = MediaPeer::new(pid("alice"), Instant::now());
        assert!(!p.is_connected(), "no transport before Event::Connected");
        let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = p.rtc.writer(Mid::new());
        }));
        assert!(refused.is_err(), "the peer must be in RTP mode");
        p.mark_connected();
        assert!(p.is_connected());
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
             a=extmap:2 http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time\r\n\
             a=extmap:4 urn:ietf:params:rtp-hdrext:sdes:mid\r\n\
             m=video 9 UDP/TLS/RTP/SAVPF 96\r\n\
             a=extmap:4 urn:ietf:params:rtp-hdrext:sdes:mid\r\n\
             a=extmap:12 http://www.webrtc.org/experiments/rtp-hdrext/generic-frame-descriptor-00\r\n";
        let summary = extmap_summary(sdp);
        assert_eq!(
            summary,
            "[audio] 2:abs-send-time 4:mid [video] 4:mid 12:generic-frame-descriptor-00"
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
    /// invariant), independent of send-stream state. Verbatim
    /// delivery through an Open track is covered by the
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
                packet: synthetic_packet(),
            }];

            let outcome = fanout_packets(&mut r, &pending);

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

    /// The tally is per `(packet × non-origin peer)`: two
    /// packets from the same origin in a 3-peer call yield
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
                packet: synthetic_packet(),
            },
            PendingFanout {
                origin: pid("a"),
                packet: synthetic_packet(),
            },
        ];

        let outcome = fanout_packets(&mut r, &pending);

        assert_eq!(outcome.attempted(), 4);
        assert_eq!(outcome.no_writer, 4);
    }

    /// Cross-call isolation (a hard SFU invariant): a packet
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
        // A packet from the remaining publisher is offered to nobody:
        // no ghost is left to fan out to.
        let pending = vec![PendingFanout {
            origin: pid("pub"),
            packet: synthetic_packet(),
        }];
        assert_eq!(fanout_packets(&mut r, &pending).attempted(), 0);
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
        let packet = synthetic_packet();
        l.record_origin(&pid("pub"), &packet);
        l.record(
            &pid("sub1"),
            &pid("pub"),
            packet.mid,
            TrackRole::Main,
            &MediaForward::Sent,
        );
        l.record(
            &pid("sub1"),
            &pid("pub"),
            packet.mid,
            TrackRole::Main,
            &MediaForward::NoWriter,
        );
        l.record(
            &pid("sub2"),
            &pid("pub"),
            packet.mid,
            TrackRole::Main,
            &MediaForward::Sent,
        );
        let sub1 = &l.per_target[&(pid("sub1"), pid("pub"), packet.mid, TrackRole::Main)];
        assert_eq!((sub1.sent, sub1.no_writer), (1, 1));
        assert_eq!(l.per_target.len(), 2, "one row per destination");
        let origin = &l.per_origin[&(pid("pub"), packet.mid)];
        assert_eq!(origin.frames, 1);
        assert_eq!(origin.bytes, packet.payload.len() as u64);
        assert_eq!(origin.key_frames, 0, "no descriptor on a synthetic packet");
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

    /// The ledger counts frames without assembling any: an audio packet is
    /// a frame, a video frame is counted at its marker-bit packet, and a
    /// key frame at the packet whose descriptor begins an independent one.
    #[test]
    fn fanout_ledger_counts_frames_and_key_frames_from_packet_flags() {
        let mut l = FanoutLedger::new(Duration::from_secs(10), Instant::now());
        let video = synthetic_video_packet();
        // A three-packet key frame followed by a one-packet delta frame.
        let first = RelayPacket {
            starts_independent_frame: true,
            blind: false,
            payload: vec![0; 100],
            ..video.clone()
        };
        let middle = RelayPacket {
            payload: vec![0; 100],
            ..video.clone()
        };
        let last = RelayPacket {
            marker: true,
            payload: vec![0; 40],
            ..video.clone()
        };
        let delta = RelayPacket {
            marker: true,
            payload: vec![0; 10],
            ..video.clone()
        };
        for p in [&first, &middle, &last, &delta] {
            l.record_origin(&pid("pub"), p);
        }
        let tally = l.per_origin[&(pid("pub"), video.mid)];
        assert_eq!(
            tally,
            OriginTally {
                packets: 4,
                frames: 2,
                bytes: 250,
                key_frames: 1,
                fec_packets: 0,
                recovered: 0,
                uplink: None,
                needed: None,
            }
        );

        // Audio: every packet is a frame, marker or not.
        let audio = synthetic_packet();
        l.record_origin(&pid("pub"), &audio);
        l.record_origin(&pid("pub"), &audio);
        let tally = l.per_origin[&(pid("pub"), audio.mid)];
        assert_eq!((tally.packets, tally.frames, tally.key_frames), (2, 2, 0));
    }

    #[test]
    fn every_forward_outcome_has_its_own_bucket() {
        let mut o = FanoutOutcome::default();
        for f in [
            MediaForward::Sent,
            MediaForward::NoWriter,
            MediaForward::NoCodecMatch,
            MediaForward::WriteFailed("x".to_owned()),
            MediaForward::WrongLayer,
            MediaForward::Stale,
            MediaForward::NotWanted,
        ] {
            o.tally(&f);
        }
        assert_eq!(
            o,
            FanoutOutcome {
                sent: 1,
                no_writer: 1,
                no_codec_match: 1,
                write_failed: 1,
                wrong_layer: 1,
                stale: 1,
                not_wanted: 1,
                fec_sent: 0,
                duplicates: 0,
                playout_delay_ms: None,
                paused: false,
            }
        );
        assert_eq!(o.attempted(), 7);
    }

    // ============================================================
    // Video FEC (ADR-0067 step 4)
    // ============================================================

    /// A peer with video FEC switched on, and its default payload types.
    fn fec_peer(name: &str) -> MediaPeer {
        MediaPeer::with_options(pid(name), Instant::now(), None, true)
    }

    fn params_of(peer: &MediaPeer, codec: Codec) -> PayloadParams {
        *peer
            .rtc
            .codec_config()
            .find(|p| p.spec().codec == codec)
            .expect("codec configured")
    }

    #[test]
    fn video_fec_is_off_unless_asked_for() {
        let off = MediaPeer::new(pid("off"), Instant::now());
        for codec in [Codec::RedVideo, Codec::Ulpfec] {
            assert!(off
                .rtc
                .codec_config()
                .find(|p| p.spec().codec == codec)
                .is_none());
        }
        let on = fec_peer("on");
        assert_eq!(*params_of(&on, Codec::RedVideo).pt(), 116);
        assert_eq!(
            params_of(&on, Codec::RedVideo).resend().map(|p| *p),
            Some(117)
        );
        assert_eq!(*params_of(&on, Codec::Ulpfec).pt(), 118);
        // The router hands the switch to the peers it creates.
        let mut router = MediaRouter::new();
        router.set_video_fec(true);
        let peer = router.peer_or_insert(pid("lazy"), Instant::now());
        assert!(peer
            .rtc
            .codec_config()
            .find(|p| p.spec().codec == Codec::Ulpfec)
            .is_some());
    }

    #[test]
    fn a_red_wrapped_packet_is_classified_by_its_header_byte() {
        let peer = fec_peer("pub");
        let red = params_of(&peer, Codec::RedVideo);
        let vp8 = params_of(&peer, Codec::Vp8);
        let ulpfec = params_of(&peer, Codec::Ulpfec);
        let opus = params_of(&peer, Codec::Opus);

        // Media: handled under the media codec's parameters.
        let (params, body) = peer
            .classify_video_red(red, &[*vp8.pt(), 0xaa, 0xbb])
            .expect("media");
        assert_eq!(params.pt(), vp8.pt());
        assert_eq!(body, Some(RedBody::Media));

        // FEC: kept under the red parameters.
        let (params, body) = peer
            .classify_video_red(red, &[*ulpfec.pt(), 0, 0])
            .expect("fec");
        assert_eq!(params.pt(), red.pt());
        assert_eq!(body, Some(RedBody::Fec));

        // No header at all: a bare red packet.
        let (params, body) = peer.classify_video_red(red, &[]).expect("empty");
        assert_eq!((params.pt(), body), (red.pt(), None));

        // A further block, a payload type nobody negotiated, an audio
        // payload type, red inside red, the red RTX number: dropped.
        assert!(peer
            .classify_video_red(red, &[0x80 | *vp8.pt(), 0, 0, 0, *vp8.pt()])
            .is_none());
        assert!(peer.classify_video_red(red, &[77, 1]).is_none());
        assert!(peer.classify_video_red(red, &[*opus.pt(), 1]).is_none());
        assert!(peer.classify_video_red(red, &[*red.pt(), 1]).is_none());
        assert!(peer.classify_video_red(red, &[117, 1]).is_none());

        // A packet under any other payload type is not looked into.
        let (params, body) = peer
            .classify_video_red(vp8, &[*ulpfec.pt(), 9])
            .expect("plain");
        assert_eq!((params.pt(), body), (vp8.pt(), None));
    }

    /// A RED media or FEC packet as `relay_packet` would describe it.
    fn red_packet(peer: &MediaPeer, body: RedBody, payload: Vec<u8>) -> RelayPacket {
        RelayPacket {
            kind: MediaKind::Video,
            params: match body {
                RedBody::Media => params_of(peer, Codec::Vp8),
                RedBody::Fec => params_of(peer, Codec::RedVideo),
            },
            red: Some(body),
            payload,
            ..synthetic_packet()
        }
    }

    fn open_video_track(peer: &mut MediaPeer, source: &MediaPeerId, mid: Mid) {
        peer.tracks_out.push(TrackOutEntry {
            source_peer: source.clone(),
            source_mid: mid,
            kind: MediaKind::Video,
            state: TrackOutState::Open(Mid::new()),
            role: TrackRole::Main,
            chosen_rid: None,
            forwarding: Forwarding::default(),
        });
    }

    /// No track is sent a publisher's FEC packet, whether the peer's session
    /// knows the payload types or not; the media inside RED gets as far as
    /// the stream lookup.
    #[test]
    fn a_publishers_fec_packet_is_forwarded_to_nobody() {
        let source = pid("pub");
        let publisher = fec_peer("pub");
        for mut subscriber in [
            Box::new(fec_peer("sub")),
            Box::new(MediaPeer::new(pid("sub"), Instant::now())),
        ] {
            subscriber.mark_connected();
            let fec = red_packet(&publisher, RedBody::Fec, vec![118; 20]);
            let media = RelayPacket {
                mid: fec.mid,
                ..red_packet(&publisher, RedBody::Media, vec![96, 1, 2, 3])
            };
            open_video_track(&mut subscriber, &source, fec.mid);
            assert_eq!(
                subscriber.forward_packet(&source, &fec),
                MediaForward::NoCodecMatch
            );
            assert!(
                subscriber.tracks_out[0].forwarding.map.is_none(),
                "an FEC packet does not start the downstream stream"
            );
            assert_eq!(
                subscriber.forward_packet(&source, &media),
                MediaForward::NoWriter
            );
            // The media packet did.
            assert!(subscriber.tracks_out[0].forwarding.map.is_some());
        }
    }

    /// A Chrome-shaped video offer: VP8 with RTX, then `red` with RTX and
    /// `ulpfec`, under libwebrtc's payload type numbers and extension ids.
    const CHROME_VIDEO_OFFER: &str = "v=0\r\n\
        o=- 5058682828002148772 2 IN IP4 127.0.0.1\r\n\
        s=-\r\n\
        t=0 0\r\n\
        a=group:BUNDLE 0\r\n\
        a=msid-semantic: WMS\r\n\
        m=video 9 UDP/TLS/RTP/SAVPF 96 97 116 117 118\r\n\
        c=IN IP4 0.0.0.0\r\n\
        a=rtcp:9 IN IP4 0.0.0.0\r\n\
        a=ice-ufrag:S5hk\r\n\
        a=ice-pwd:0zV/Yu3y8aDzbHgqWhnVQhqP\r\n\
        a=ice-options:trickle\r\n\
        a=fingerprint:sha-256 8C:64:ED:03:76:D0:3D:B4:88:08:91:64:08:80:A8:C6:5A:BF:8B:4E:38:27:96:CA:08:49:25:73:46:60:20:DC\r\n\
        a=setup:actpass\r\n\
        a=mid:0\r\n\
        a=extmap:2 http://www.webrtc.org/experiments/rtp-hdrext/abs-send-time\r\n\
        a=extmap:3 urn:3gpp:video-orientation\r\n\
        a=extmap:4 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01\r\n\
        a=extmap:9 urn:ietf:params:rtp-hdrext:sdes:mid\r\n\
        a=extmap:12 http://www.webrtc.org/experiments/rtp-hdrext/generic-frame-descriptor-00\r\n\
        a=sendonly\r\n\
        a=msid:- 7a9f4f0e-6f0e-4f43-8d53-1d7c3c8b2a11\r\n\
        a=rtcp-mux\r\n\
        a=rtcp-rsize\r\n\
        a=rtpmap:96 VP8/90000\r\n\
        a=rtcp-fb:96 goog-remb\r\n\
        a=rtcp-fb:96 transport-cc\r\n\
        a=rtcp-fb:96 ccm fir\r\n\
        a=rtcp-fb:96 nack\r\n\
        a=rtcp-fb:96 nack pli\r\n\
        a=rtpmap:97 rtx/90000\r\n\
        a=fmtp:97 apt=96\r\n\
        a=rtpmap:116 red/90000\r\n\
        a=rtpmap:117 rtx/90000\r\n\
        a=fmtp:117 apt=116\r\n\
        a=rtpmap:118 ulpfec/90000\r\n\
        a=ssrc-group:FID 1111111111 2222222222\r\n\
        a=ssrc:1111111111 cname:xeXs3aE9AOBn00yJ\r\n\
        a=ssrc:2222222222 cname:xeXs3aE9AOBn00yJ\r\n";

    /// The answer to a Chrome-shaped offer: with the switch on it keeps the
    /// pair and the red RTX mapping under the offerer's numbers, with the
    /// switch off it has VP8 and its RTX alone.
    #[test]
    fn a_chrome_shaped_video_offer_is_answered_with_the_pair_only_when_switched_on() {
        let video = |sdp: &str| -> Vec<String> {
            sdp.lines()
                .skip_while(|l| !l.starts_with("m=video"))
                .map(str::to_owned)
                .collect()
        };
        let mut on = fec_peer("on");
        let answer = on.accept_offer(CHROME_VIDEO_OFFER).expect("answer");
        let lines = video(&answer);
        println!("{}", lines.join("\n"));
        assert_eq!(lines[0], "m=video 9 UDP/TLS/RTP/SAVPF 96 97 116 117 118");
        for line in [
            "a=rtpmap:116 red/90000",
            "a=rtpmap:117 rtx/90000",
            "a=fmtp:117 apt=116",
            "a=rtpmap:118 ulpfec/90000",
            "a=fmtp:97 apt=96",
        ] {
            assert!(lines.iter().any(|l| l == line), "{line}");
        }

        let mut off = MediaPeer::new(pid("off"), Instant::now());
        let answer = off.accept_offer(CHROME_VIDEO_OFFER).expect("answer");
        let lines = video(&answer);
        assert_eq!(lines[0], "m=video 9 UDP/TLS/RTP/SAVPF 96 97");
        assert!(!lines
            .iter()
            .any(|l| l.contains("red/") || l.contains("ulpfec")));
    }

    /// An FEC packet of a simulcast layer never latches or starts that
    /// layer on a track; the layer's media does.
    #[test]
    fn a_simulcast_fec_packet_does_not_latch_a_layer() {
        let source = pid("pub");
        let publisher = fec_peer("pub");
        let mut subscriber = fec_peer("sub");
        subscriber.mark_connected();
        let fec = RelayPacket {
            rid: Some(Rid::from("h")),
            ..red_packet(&publisher, RedBody::Fec, vec![118; 20])
        };
        open_video_track(&mut subscriber, &source, fec.mid);
        assert_eq!(
            subscriber.forward_packet(&source, &fec),
            MediaForward::NoCodecMatch
        );
        let entry = &subscriber.tracks_out[0];
        assert_eq!(entry.chosen_rid, None, "nothing was selected");
        assert_eq!(entry.forwarding.active_rid, None, "nothing was started");
    }

    #[test]
    fn the_ledger_and_the_frame_flags_ignore_fec_packets() {
        let publisher = fec_peer("pub");
        let media = RelayPacket {
            marker: true,
            ..red_packet(&publisher, RedBody::Media, vec![96; 101])
        };
        // An FEC packet whose header was copied from a marker-bit packet.
        let fec = RelayPacket {
            mid: media.mid,
            marker: true,
            ..red_packet(&publisher, RedBody::Fec, vec![118; 60])
        };
        assert!(media.ends_frame());
        assert!(!fec.ends_frame());
        assert_eq!(media.media_len(), 100, "the RED header byte is not media");

        let mut l = FanoutLedger::new(Duration::from_secs(10), Instant::now());
        l.record_origin(&pid("pub"), &media);
        l.record_origin(&pid("pub"), &fec);
        assert_eq!(
            l.per_origin[&(pid("pub"), media.mid)],
            OriginTally {
                packets: 1,
                frames: 1,
                bytes: 100,
                key_frames: 0,
                fec_packets: 1,
                recovered: 0,
                uplink: None,
                needed: None,
            }
        );

        // A packet rebuilt from FEC is a packet like the others, and is
        // counted as recovered besides.
        let rebuilt = RelayPacket {
            recovered: true,
            ..media.clone()
        };
        l.record_origin(&pid("pub"), &rebuilt);
        let tally = l.per_origin[&(pid("pub"), media.mid)];
        assert_eq!((tally.packets, tally.frames, tally.recovered), (2, 2, 1));
    }

    /// An FEC packet the SFU wrote for a subscriber is filed per target in
    /// the ledger and is not a forward attempt.
    #[test]
    fn the_ledger_files_sent_fec_apart_from_forward_attempts() {
        let now = Instant::now();
        let mut l = FanoutLedger::new(Duration::from_secs(10), now);
        let mid = Mid::from("v");
        l.record(
            &pid("sub"),
            &pid("pub"),
            mid,
            TrackRole::Main,
            &MediaForward::Sent,
        );
        l.record_fec_sent(&pid("sub"), &pid("pub"), mid, TrackRole::Main);
        l.record_fec_sent(&pid("sub"), &pid("pub"), mid, TrackRole::Main);
        let o = l.per_target[&(pid("sub"), pid("pub"), mid, TrackRole::Main)];
        assert_eq!((o.sent, o.fec_sent, o.attempted()), (1, 2, 1));
    }

    /// A publisher's FEC packet offered to a subscriber that has been sent
    /// no RED-wrapped media on the track asks for nothing and writes nothing.
    #[test]
    fn a_publishers_fec_packet_asks_nothing_of_a_track_without_fec_state() {
        let source = pid("pub");
        let publisher = fec_peer("pub");
        let mut subscriber = fec_peer("sub");
        let fec = red_packet(&publisher, RedBody::Fec, vec![118; 20]);
        open_video_track(&mut subscriber, &source, fec.mid);
        subscriber.offer_publisher_fec(&source, &fec);
        subscriber.send_leg_fec();
        assert!(subscriber.fec_senders.is_empty());
        assert!(subscriber.take_fec_sent().is_empty());
    }

    /// The fanout offers a publisher's FEC packet to no subscriber: it is
    /// filed in the ledger and nothing is attempted for it.
    #[test]
    fn the_fanout_skips_fec_packets() {
        let mut router = MediaRouter::new();
        router.set_video_fec(true);
        let now = Instant::now();
        for name in ["pub", "sub"] {
            router
                .peer_or_insert(pid(name), now)
                .set_call_id("c".to_owned());
        }
        let publisher = fec_peer("pub");
        let fec = red_packet(&publisher, RedBody::Fec, vec![118; 60]);
        let media = RelayPacket {
            mid: fec.mid,
            ..red_packet(&publisher, RedBody::Media, vec![96, 1])
        };
        let mut ledger = FanoutLedger::new(Duration::from_secs(10), now);
        let outcome = fanout_packets_into(
            &mut router,
            &[
                PendingFanout {
                    origin: pid("pub"),
                    packet: fec.clone(),
                },
                PendingFanout {
                    origin: pid("pub"),
                    packet: media,
                },
            ],
            Some(&mut ledger),
        );
        assert_eq!(outcome.attempted(), 1, "the media packet alone");
        let tally = ledger.per_origin[&(pid("pub"), fec.mid)];
        assert_eq!((tally.packets, tally.fec_packets), (1, 1));
    }

    // ============================================================
    // RTP relay (ADR-0066)
    // ============================================================

    /// What travels with the stream is carried; what belongs to the egress
    /// leg is left for str0m to set.
    #[test]
    fn egress_extensions_carry_the_streams_own_values_and_nothing_of_the_leg() {
        use str0m::rtp::VideoOrientation;
        let descriptor = PacketDescriptor(vec![0xb0, 0x01, 0x2a, 0x00]);
        let packet = RelayPacket {
            video_orientation: Some(VideoOrientation::Deg90),
            descriptor: Some(descriptor.clone()),
            ..synthetic_video_packet()
        };
        let ev = packet.egress_extensions();
        assert!(
            ev.audio_level.is_none() && ev.voice_activity.is_none(),
            "who speaks, and how loudly, is not relayed"
        );
        assert_eq!(ev.video_orientation, Some(VideoOrientation::Deg90));
        assert_eq!(
            ev.user_values.get::<PacketDescriptor>(),
            Some(&descriptor),
            "the frame descriptor is relayed as the packet carried it"
        );
        assert_eq!(
            ev.frame_position, None,
            "no packetizer position: the serializer writes the value verbatim"
        );
        assert!(ev.mid.is_none() && ev.rid.is_none() && ev.rid_repair.is_none());
        assert!(ev.transport_cc.is_none() && ev.abs_send_time.is_none());

        // Nothing to carry: nothing is invented.
        let bare = synthetic_packet().egress_extensions();
        assert!(bare.audio_level.is_none() && bare.voice_activity.is_none());
        assert!(bare.video_orientation.is_none());
        assert!(bare.user_values.get::<PacketDescriptor>().is_none());
    }

    /// The per-track position: the publisher's numbers, gaps kept, a late
    /// packet in its own slot.
    #[test]
    fn forwarding_position_follows_the_publishers_numbering() {
        let mut f = Forwarding::default();
        let at = |seq: u64, time: u32| RelayPacket {
            seq_no: seq.into(),
            rtp_time: time,
            ..synthetic_video_packet()
        };
        let first = f.position(&at(1_000, 3_000)).expect("first packet");
        assert_eq!((first.seq_no, first.rtp_time), (1_000, 3_000));
        assert_eq!(f.source_ssrc, Some(synthetic_packet().ssrc));

        // 1001 is lost on the way to the SFU; 1002 keeps its distance.
        let after_gap = f.position(&at(1_002, 6_000)).expect("in range");
        assert_eq!(after_gap.seq_no, 1_002, "the gap survives");
        // The retransmission arrives late and fills exactly that slot.
        let repaired = f.position(&at(1_001, 3_000)).expect("in range");
        assert_eq!(repaired.seq_no, 1_001);
    }

    /// A different source stream under the same downstream track — a new
    /// simulcast layer — continues the numbering instead of jumping.
    #[test]
    fn forwarding_position_rebases_when_the_source_stream_changes() {
        let mut f = Forwarding::default();
        let t0 = Instant::now();
        let low = RelayPacket {
            seq_no: 200_u64.into(),
            rtp_time: 9_000,
            arrival: t0,
            ..synthetic_video_packet()
        };
        let last_low = f.position(&low).expect("first packet");

        let high = RelayPacket {
            ssrc: 0x0bad_cafe.into(),
            seq_no: 77_000_u64.into(),
            rtp_time: 4_000_000_000,
            arrival: t0 + Duration::from_millis(100),
            ..synthetic_video_packet()
        };
        let first_high = f.position(&high).expect("the switch packet");
        assert_eq!(first_high.seq_no, last_low.seq_no + 1, "continuous");
        assert_eq!(
            first_high.rtp_time,
            last_low.rtp_time + 9_000,
            "100 ms at 90 kHz"
        );
        assert_eq!(f.source_ssrc, Some(high.ssrc));

        // A packet of the new layer from before the switch has no slot.
        let earlier_high = RelayPacket {
            seq_no: 76_999_u64.into(),
            ..high.clone()
        };
        assert_eq!(f.position(&earlier_high), None);
    }

    /// The whole decision chain up to the write, on a peer with an open
    /// track but no transport: nothing is queued for a peer that could not
    /// send it.
    #[test]
    fn forward_to_an_unconnected_peer_is_no_writer_and_starts_no_stream() {
        let mut p = MediaPeer::new(pid("alice"), Instant::now());
        let source = pid("bob");
        let packet = synthetic_packet();
        p.tracks_out.push(TrackOutEntry {
            source_peer: source.clone(),
            source_mid: packet.mid,
            kind: MediaKind::Audio,
            state: TrackOutState::Open(Mid::new()),
            role: TrackRole::Main,
            chosen_rid: None,
            forwarding: Forwarding::default(),
        });
        assert_eq!(p.forward_packet(&source, &packet), MediaForward::NoWriter);
        assert_eq!(
            p.tracks_out[0].forwarding.map, None,
            "the downstream numbering starts with the first packet actually sent"
        );
    }

    /// A connected peer that did not negotiate the packet's codec reports
    /// that, not a missing writer.
    #[test]
    fn forward_of_a_codec_the_peer_did_not_negotiate_is_no_codec_match() {
        use str0m::format::{Codec, CodecSpec, FormatParams};
        use str0m::media::Frequency;
        let mut p = MediaPeer::new(pid("alice"), Instant::now());
        p.mark_connected();
        let source = pid("bob");
        // PCMU is not in str0m's default codec set (Opus, VP8, H.264, VP9).
        let packet = RelayPacket {
            params: PayloadParams::new(
                0.into(),
                None,
                CodecSpec {
                    codec: Codec::PCMU,
                    channels: None,
                    clock_rate: Frequency::EIGHT_KHZ,
                    format: FormatParams::default(),
                },
            ),
            ..synthetic_packet()
        };
        p.tracks_out.push(TrackOutEntry {
            source_peer: source.clone(),
            source_mid: packet.mid,
            kind: MediaKind::Audio,
            state: TrackOutState::Open(Mid::new()),
            role: TrackRole::Main,
            chosen_rid: None,
            forwarding: Forwarding::default(),
        });
        assert_eq!(
            p.forward_packet(&source, &packet),
            MediaForward::NoCodecMatch
        );
    }

    /// A RED packet is not written to a track that did not negotiate `red`,
    /// even though the peer's session-wide codec configuration has a `red`
    /// entry: the SFU cannot unwrap the container, so the packet is counted
    /// as a codec mismatch. The same track takes a plain Opus packet as far
    /// as the stream lookup.
    #[test]
    fn red_packet_to_a_track_without_red_is_no_codec_match() {
        use str0m::format::{CodecSpec, FormatParams, RedParams};
        use str0m::media::Frequency;
        let mut p = MediaPeer::new(pid("alice"), Instant::now());
        p.mark_connected();
        let source = pid("bob");
        let red = RelayPacket {
            params: PayloadParams::new(
                63.into(),
                None,
                CodecSpec {
                    codec: Codec::Red,
                    channels: Some(2),
                    clock_rate: Frequency::FORTY_EIGHT_KHZ,
                    format: FormatParams {
                        red: Some(RedParams {
                            pt: 111.into(),
                            count: 2,
                        }),
                        ..FormatParams::default()
                    },
                },
            ),
            ..synthetic_packet()
        };
        let opus = RelayPacket {
            mid: red.mid,
            ..synthetic_packet()
        };
        // The session does know `red`; what is missing is the track's own
        // negotiation of it.
        assert!(p.rtc.codec_config().match_params(red.params).is_some());
        p.tracks_out.push(TrackOutEntry {
            source_peer: source.clone(),
            source_mid: red.mid,
            kind: MediaKind::Audio,
            state: TrackOutState::Open(Mid::new()),
            role: TrackRole::Main,
            chosen_rid: None,
            forwarding: Forwarding::default(),
        });
        assert_eq!(p.forward_packet(&source, &red), MediaForward::NoCodecMatch);
        assert_eq!(p.forward_packet(&source, &opus), MediaForward::NoWriter);
    }

    /// A connected peer with a matching codec but no send stream behind the
    /// track (the teardown race) is `NoWriter`, and a packet older than the
    /// stream's switch point is `Stale` before any stream is looked up.
    #[test]
    fn forward_reports_stale_and_missing_stream_separately() {
        let mut p = MediaPeer::new(pid("alice"), Instant::now());
        p.mark_connected();
        let source = pid("bob");
        let packet = RelayPacket {
            seq_no: 500_u64.into(),
            ..synthetic_packet()
        };
        p.tracks_out.push(TrackOutEntry {
            source_peer: source.clone(),
            source_mid: packet.mid,
            kind: MediaKind::Audio,
            state: TrackOutState::Open(Mid::new()),
            role: TrackRole::Main,
            chosen_rid: None,
            forwarding: Forwarding::default(),
        });
        // Opus is in str0m's default codec set, so the codec matches; the
        // made-up mid has no send stream.
        assert_eq!(p.forward_packet(&source, &packet), MediaForward::NoWriter);

        // Move the track onto another source stream at packet 9000 ...
        let switched = RelayPacket {
            ssrc: 0x0bad_cafe.into(),
            seq_no: 9_000_u64.into(),
            ..packet.clone()
        };
        assert_eq!(p.forward_packet(&source, &switched), MediaForward::NoWriter);
        // ... and a packet of that stream from before the switch is stale.
        let before_switch = RelayPacket {
            seq_no: 8_999_u64.into(),
            ..switched
        };
        assert_eq!(
            p.forward_packet(&source, &before_switch),
            MediaForward::Stale
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
            packet: synthetic_packet(),
        }];

        let outcome = fanout_packets(&mut r, &pending);

        // Exactly one non-origin SAME-call peer (a-sub) is
        // offered the packet; the two call-B peers are never
        // visited. (No writer is negotiated, so the single
        // attempt lands in `no_writer` — the count is what
        // proves the iteration reached exactly one peer.)
        assert_eq!(
            outcome.attempted(),
            1,
            "a packet from call A must reach exactly the one other call-A peer"
        );
        assert_eq!(outcome.no_writer, 1, "the single same-call attempt");
    }

    /// A packet whose origin has no bound call (an incomplete
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
            packet: synthetic_packet(),
        }];

        let outcome = fanout_packets(&mut r, &pending);
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
            simulcast: false,
            light: false,
            middle: false,
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
            simulcast: false,
            light: false,
            middle: false,
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
                simulcast: false,
                light: false,
                middle: false,
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
                simulcast: false,
                light: false,
                middle: false,
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
        let synthetic = synthetic_packet();
        let source = pid("bob");
        // Even with a matching `tracks_out` entry in
        // ToOpen state, fanout must surface NoWriter
        // (state machine isn't ready yet).
        p.observe_remote_track_in(
            source.clone(),
            TrackIn {
                mid: synthetic.mid,
                kind: MediaKind::Audio,
                simulcast: false,
                light: false,
                middle: false,
            },
        );
        assert_eq!(
            p.forward_packet(&source, &synthetic),
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
                simulcast: false,
                light: false,
                middle: false,
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
                simulcast: false,
                light: false,
                middle: false,
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
    fn layer_gate_non_simulcast_packet_always_forwards_and_never_latches() {
        let mut chosen = None;
        let mut active = None;
        assert!(layer_gate_allows(&mut chosen, &mut active, None, false));
        assert!(layer_gate_allows(&mut chosen, &mut active, None, true));
        assert_eq!(chosen, None, "a rid-less packet must not latch a layer");
        assert_eq!(active, None);
    }

    #[test]
    fn layer_gate_latches_the_first_layer_and_drops_the_others() {
        let h = Rid::from("h");
        let l = Rid::from("l");
        let mut chosen = None;
        let mut active = None;
        // First simulcast packet latches its layer and forwards — no
        // keyframe is needed to start a stream nobody is watching yet.
        assert!(layer_gate_allows(&mut chosen, &mut active, Some(h), false));
        assert_eq!(chosen, Some(h));
        assert_eq!(active, Some(h));
        // Same layer keeps flowing.
        assert!(layer_gate_allows(&mut chosen, &mut active, Some(h), false));
        // A different layer is dropped — and does NOT move the latch, even
        // when it begins a key frame: nobody selected it.
        assert!(!layer_gate_allows(&mut chosen, &mut active, Some(l), false));
        assert!(!layer_gate_allows(&mut chosen, &mut active, Some(l), true));
        assert_eq!(chosen, Some(h));
        assert_eq!(active, Some(h));
    }

    #[test]
    fn layer_gate_honours_an_explicit_policy_selection() {
        let h = Rid::from("h");
        let l = Rid::from("l");
        // A bandwidth policy pre-selected the low layer.
        let mut chosen = Some(l);
        let mut active = None;
        assert!(
            !layer_gate_allows(&mut chosen, &mut active, Some(h), true),
            "high layer dropped"
        );
        assert!(
            layer_gate_allows(&mut chosen, &mut active, Some(l), false),
            "low layer forwarded"
        );
        assert_eq!(chosen, Some(l), "the selection is not overwritten");
        assert_eq!(active, Some(l));
    }

    /// ADR-0066 point 6: a selection is not the switch. The old layer keeps
    /// flowing until a packet begins an independent frame on the new one.
    #[test]
    fn layer_gate_switches_only_at_an_independent_frame_of_the_selected_layer() {
        let h = Rid::from("h");
        let l = Rid::from("l");
        // Forwarding high; the policy has just selected low.
        let mut chosen = Some(l);
        let mut active = Some(h);

        // Delta-frame packets of the selected layer cannot be decoded by a
        // subscriber that holds no reference on that layer: dropped.
        assert!(!layer_gate_allows(&mut chosen, &mut active, Some(l), false));
        assert_eq!(active, Some(h), "not switched yet");
        // The layer on the wire keeps flowing meanwhile.
        assert!(layer_gate_allows(&mut chosen, &mut active, Some(h), false));
        assert!(layer_gate_allows(&mut chosen, &mut active, Some(h), true));
        assert_eq!(
            active,
            Some(h),
            "its own key frame does not cancel the switch"
        );

        // The first packet of a key frame on the selected layer is the
        // switch point.
        assert!(layer_gate_allows(&mut chosen, &mut active, Some(l), true));
        assert_eq!(active, Some(l));
        // From there the new layer flows, deltas included, and the old one
        // is dropped.
        assert!(layer_gate_allows(&mut chosen, &mut active, Some(l), false));
        assert!(!layer_gate_allows(&mut chosen, &mut active, Some(h), false));
        assert!(!layer_gate_allows(&mut chosen, &mut active, Some(h), true));
        assert_eq!(chosen, Some(l));
    }

    /// A third layer is never forwarded, whatever it carries.
    #[test]
    fn layer_gate_drops_a_layer_that_is_neither_selected_nor_on_the_wire() {
        let (h, m, l) = (Rid::from("h"), Rid::from("m"), Rid::from("l"));
        let mut chosen = Some(l);
        let mut active = Some(h);
        assert!(!layer_gate_allows(&mut chosen, &mut active, Some(m), true));
        assert_eq!((chosen, active), (Some(l), Some(h)));
    }

    /// Re-arming the latch while a layer is flowing selects whatever comes
    /// next, and still switches only at a key frame.
    #[test]
    fn layer_gate_rearmed_latch_selects_the_next_layer_seen() {
        let h = Rid::from("h");
        let l = Rid::from("l");
        let mut chosen = None;
        let mut active = Some(h);
        assert!(!layer_gate_allows(&mut chosen, &mut active, Some(l), false));
        assert_eq!(chosen, Some(l), "latched as the selection");
        assert_eq!(active, Some(h), "but not on the wire before a key frame");
        assert!(layer_gate_allows(&mut chosen, &mut active, Some(l), true));
        assert_eq!(active, Some(l));
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
                simulcast: false,
                light: false,
                middle: false,
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
        let mut packet = synthetic_video_packet();
        packet.rid = Some(Rid::from("h"));
        // Manually stage an Open downstream entry that has already selected the
        // LOW layer — the high-layer packet above must be dropped as WrongLayer
        // BEFORE any send-stream lookup (so no real str0m session is needed).
        p.tracks_out.push(TrackOutEntry {
            source_peer: source.clone(),
            source_mid: packet.mid,
            kind: MediaKind::Video,
            state: TrackOutState::Open(Mid::new()),
            role: TrackRole::Main,
            chosen_rid: Some(Rid::from("l")),
            forwarding: Forwarding::default(),
        });
        assert_eq!(
            p.forward_packet(&source, &packet),
            MediaForward::WrongLayer,
            "a packet for the non-selected layer is dropped as WrongLayer"
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
            role: TrackRole::Main,
            chosen_rid: Some(rid_h),
            forwarding: Forwarding::default(),
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
            role: TrackRole::Main,
            chosen_rid: Some(rid_l),
            forwarding: Forwarding::default(),
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

    // ============================================================
    // Uplink fallback wiring
    // ============================================================

    /// A video packet of layer `rid` with sequence number `seq`, arrived at
    /// `at`.
    fn layer_packet(mid: Mid, rid: &str, seq: u64, at: Instant) -> RelayPacket {
        RelayPacket {
            mid,
            rid: Some(Rid::from(rid)),
            seq_no: SeqNo::from(seq),
            arrival: at,
            ..synthetic_video_packet()
        }
    }

    /// A router with a publisher of two measured layers (`q` light, `f`
    /// full) that has received packet 1 on each at `t0`, and a subscriber
    /// selected to `start` with `budget` as its egress estimate. The uplink
    /// fallback is off by default and is switched on here.
    fn fallback_router(
        mid: Mid,
        t0: Instant,
        start: Option<&str>,
        budget: Option<u64>,
    ) -> MediaRouter {
        let mut publisher = MediaPeer::new(pid("pub"), t0);
        publisher.set_uplink_fallback(true);
        publisher
            .layer_meters
            .insert((mid, Rid::from("q")), meter_with_bps(150_000));
        publisher
            .layer_meters
            .insert((mid, Rid::from("f")), meter_with_bps(1_500_000));
        publisher.observe_uplink_holes(&layer_packet(mid, "q", 1, t0));
        publisher.observe_uplink_holes(&layer_packet(mid, "f", 1, t0));
        let mut subscriber = MediaPeer::new(pid("sub"), t0);
        if let Some(bps) = budget {
            subscriber.set_egress_estimate_bps(bps);
        }
        subscriber.tracks_out.push(TrackOutEntry {
            source_peer: pid("pub"),
            source_mid: mid,
            kind: MediaKind::Video,
            state: TrackOutState::Open(Mid::new()),
            role: TrackRole::Main,
            chosen_rid: start.map(Rid::from),
            forwarding: Forwarding {
                active_rid: start.map(Rid::from),
                ..Forwarding::default()
            },
        });
        let mut router = MediaRouter::new();
        router.add_peer(publisher);
        router.add_peer(subscriber);
        router
    }

    fn chosen(router: &MediaRouter) -> Option<Rid> {
        find_peer(router, "sub").tracks_out[0].chosen_rid
    }

    #[test]
    fn an_old_hole_on_the_full_layer_moves_subscribers_to_the_light_one_and_back() {
        let mid = Mid::new();
        let t0 = Instant::now();
        let (q, f) = (Rid::from("q"), Rid::from("f"));
        // The subscriber has room for the full layer and is on it.
        let mut router = fallback_router(mid, t0, Some("f"), Some(10_000_000));
        assert_eq!(
            drive_uplink_fallback(&mut router, t0),
            UplinkFallbackSummary::default()
        );
        assert_eq!(
            find_peer(&router, "pub").uplink_fallback_state(mid),
            Some("full")
        );

        // Packet 2 of the full layer is skipped.
        router
            .peer_mut(&pid("pub"))
            .unwrap()
            .observe_uplink_holes(&layer_packet(mid, "f", 3, t0));
        let young = t0 + uplink_fallback::HOLE_AGE;
        assert_eq!(drive_uplink_fallback(&mut router, young).to_fallback, 0);
        assert_eq!(chosen(&router), Some(f));

        let old = young + Duration::from_millis(1);
        let summary = drive_uplink_fallback(&mut router, old);
        assert_eq!(summary.to_fallback, 1);
        assert_eq!(summary.to_full, 0);
        assert_eq!(chosen(&router), Some(q), "selected to the light layer");
        let publisher = find_peer(&router, "pub");
        assert_eq!(publisher.uplink_fallback_state(mid), Some("fallback"));
        assert_eq!(publisher.uplink_fallback_layer(mid), Some(q));
        // The old layer stays on the wire until the light layer's keyframe.
        assert_eq!(
            find_peer(&router, "sub").tracks_out[0]
                .forwarding
                .active_rid,
            Some(f)
        );

        // The bandwidth policy has room for the full layer and may not use
        // it: the lower of the two wins.
        assert_eq!(
            drive_layer_selection(&mut router),
            LayerSelectionSummary::default()
        );
        assert_eq!(chosen(&router), Some(q));

        // The hole is filled; three seconds later the track is full again
        // and the subscriber is back on the full layer.
        router
            .peer_mut(&pid("pub"))
            .unwrap()
            .observe_uplink_holes(&layer_packet(mid, "f", 2, old));
        let almost = old + uplink_fallback::RESTORE_AFTER.saturating_sub(Duration::from_millis(1));
        assert_eq!(drive_uplink_fallback(&mut router, almost).to_full, 0);
        assert_eq!(chosen(&router), Some(q));
        let summary = drive_uplink_fallback(&mut router, old + uplink_fallback::RESTORE_AFTER);
        assert_eq!(summary.to_full, 1);
        assert_eq!(chosen(&router), Some(f));
        assert_eq!(
            find_peer(&router, "pub").uplink_fallback_state(mid),
            Some("full")
        );
    }

    #[test]
    fn a_subscriber_short_of_bandwidth_is_not_raised_by_the_restore() {
        let mid = Mid::new();
        let t0 = Instant::now();
        let q = Rid::from("q");
        // 400 kbit/s: the light layer fits, the full one does not. The
        // subscriber is on the light layer before, during and after.
        let mut router = fallback_router(mid, t0, Some("q"), Some(400_000));
        router
            .peer_mut(&pid("pub"))
            .unwrap()
            .observe_uplink_holes(&layer_packet(mid, "f", 3, t0));
        let old = t0 + uplink_fallback::HOLE_AGE + Duration::from_millis(1);
        assert_eq!(drive_uplink_fallback(&mut router, old).to_fallback, 1);
        assert_eq!(chosen(&router), Some(q));
        router
            .peer_mut(&pid("pub"))
            .unwrap()
            .observe_uplink_holes(&layer_packet(mid, "f", 2, old));
        let restored = old + uplink_fallback::RESTORE_AFTER;
        assert_eq!(drive_uplink_fallback(&mut router, restored).to_full, 1);
        assert_eq!(chosen(&router), Some(q));
    }

    #[test]
    fn a_subscriber_without_an_estimate_is_put_on_the_heaviest_layer() {
        let mid = Mid::new();
        let t0 = Instant::now();
        // Latched to the light layer because it reached the SFU first.
        let mut router = fallback_router(mid, t0, Some("q"), None);
        let summary = drive_uplink_fallback(&mut router, t0);
        assert_eq!(summary.to_fallback + summary.to_full, 0, "not a fallback");
        assert_eq!(chosen(&router), Some(Rid::from("f")));
        // Nothing latched yet: left to the latch.
        let mut router = fallback_router(mid, t0, None, None);
        drive_uplink_fallback(&mut router, t0);
        assert_eq!(chosen(&router), None);
    }

    #[test]
    fn the_heaviest_layer_default_does_not_need_the_fallback_switched_on() {
        let mid = Mid::new();
        let t0 = Instant::now();
        let mut router = fallback_router(mid, t0, Some("q"), None);
        router
            .peer_mut(&pid("pub"))
            .unwrap()
            .set_uplink_fallback(false);
        assert_eq!(find_peer(&router, "pub").uplink_fallback_state(mid), None);
        let summary = drive_uplink_fallback(&mut router, t0);
        assert_eq!(summary.to_fallback + summary.to_full, 0);
        assert_eq!(chosen(&router), Some(Rid::from("f")));
    }

    #[test]
    fn a_publisher_without_simulcast_has_no_fallback_policy() {
        let mid = Mid::new();
        let t0 = Instant::now();
        let mut publisher = MediaPeer::new(pid("pub"), t0);
        // Video without a rid, with a hole; and simulcast audio-kind noise.
        for seq in [1_u64, 2, 9] {
            let packet = RelayPacket {
                mid,
                seq_no: SeqNo::from(seq),
                arrival: t0,
                ..synthetic_video_packet()
            };
            assert_eq!(packet.rid, None);
            publisher.observe_uplink_holes(&packet);
        }
        publisher.observe_uplink_holes(&RelayPacket {
            mid,
            rid: Some(Rid::from("q")),
            ..synthetic_packet()
        });
        assert!(publisher.uplink_fallbacks.is_empty());
        assert_eq!(publisher.uplink_fallback_state(mid), None);
        let mut router = MediaRouter::new();
        router.add_peer(publisher);
        router.add_peer(MediaPeer::new(pid("sub"), t0));
        assert_eq!(
            drive_uplink_fallback(&mut router, t0 + Duration::from_secs(1)),
            UplinkFallbackSummary::default()
        );
    }

    #[test]
    fn the_fallback_is_off_unless_switched_on() {
        let mid = Mid::new();
        let t0 = Instant::now();
        // As constructed, and as a router constructs its peers.
        let mut publisher = MediaPeer::new(pid("pub"), t0);
        let mut router = MediaRouter::new();
        assert!(!router.peer_or_insert(pid("x"), t0).uplink_fallback_on);
        router.set_uplink_fallback(true);
        assert!(router.peer_or_insert(pid("y"), t0).uplink_fallback_on);
        publisher.observe_uplink_holes(&layer_packet(mid, "q", 1, t0));
        publisher.observe_uplink_holes(&layer_packet(mid, "f", 1, t0));
        publisher.observe_uplink_holes(&layer_packet(mid, "f", 5, t0));
        assert!(publisher.uplink_fallbacks.is_empty());
    }

    // ============================================================
    // Layer switch at a whole key frame
    // ============================================================

    /// Packet `seq` of the light layer: `key` marks the first packet of a
    /// key frame, `marker` the last packet of a frame, `time` its frame.
    fn light(seq: u64, time: u32, key: bool, marker: bool, at: Instant) -> RelayPacket {
        RelayPacket {
            rid: Some(Rid::from("q")),
            seq_no: SeqNo::from(seq),
            rtp_time: time,
            starts_independent_frame: key,
            blind: false,
            marker,
            arrival: at,
            ..synthetic_video_packet()
        }
    }

    fn seqs(run: &[RelayPacket]) -> Vec<u64> {
        run.iter().map(|p| *p.seq_no).collect()
    }

    #[test]
    fn a_switch_waits_for_the_whole_key_frame_and_sends_it_in_order() {
        let t0 = Instant::now();
        let mut wait = SwitchWait::default();
        // Delta packets ahead of any key frame are held, not sent.
        assert!(wait.offer(&light(8, 100, false, true, t0)).is_none());
        // The key frame is packets 10..=12; 11 and 12 arrive first, then a
        // delta frame behind it, and its first packet last (it was resent).
        assert!(wait.offer(&light(11, 200, false, false, t0)).is_none());
        assert!(wait.offer(&light(12, 200, false, true, t0)).is_none());
        assert!(wait.offer(&light(13, 300, false, true, t0)).is_none());
        // A duplicate changes nothing.
        assert!(wait.offer(&light(12, 200, false, true, t0)).is_none());
        let later = t0 + Duration::from_millis(300);
        let run = wait
            .offer(&light(10, 200, true, false, later))
            .expect("the key frame is whole");
        assert_eq!(seqs(&run), vec![10, 11, 12, 13]);
        assert!(
            run.iter().all(|p| p.arrival == later),
            "they leave together"
        );
        assert!(wait.packets.is_empty(), "nothing is kept after the switch");
    }

    #[test]
    fn a_key_frame_with_a_missing_packet_is_not_switched_at() {
        let t0 = Instant::now();
        let mut wait = SwitchWait::default();
        assert!(wait.offer(&light(10, 200, true, false, t0)).is_none());
        // 11 is missing; the frame's last packet and later frames are here.
        assert!(wait.offer(&light(12, 200, false, true, t0)).is_none());
        assert!(wait.offer(&light(13, 300, false, true, t0)).is_none());
        // A first packet with the marker of another frame behind it is not
        // a whole frame either.
        assert!(wait.newest_whole_key_frame().is_none());
        let run = wait
            .offer(&light(11, 200, false, false, t0))
            .expect("whole now");
        assert_eq!(seqs(&run), vec![10, 11, 12, 13]);
    }

    #[test]
    fn the_newest_whole_key_frame_is_the_one_switched_at() {
        let t0 = Instant::now();
        let mut wait = SwitchWait::default();
        // An older key frame that never became whole (11 missing)...
        assert!(wait.offer(&light(10, 200, true, false, t0)).is_none());
        assert!(wait.offer(&light(12, 200, false, true, t0)).is_none());
        // ...and a newer one in a single packet.
        let run = wait
            .offer(&light(20, 900, true, true, t0))
            .expect("a one-packet key frame is whole");
        assert_eq!(seqs(&run), vec![20]);
    }

    #[test]
    fn an_fec_packet_inside_a_key_frame_is_not_a_missing_packet() {
        let t0 = Instant::now();
        let mut wait = SwitchWait::default();
        assert!(wait.offer(&light(10, 200, true, false, t0)).is_none());
        // 11 is the publisher's FEC packet.
        assert!(wait.offer(&light(12, 200, false, true, t0)).is_none());
        wait.note_fec(11);
        let run = wait
            .offer(&light(13, 300, false, true, t0))
            .expect("whole: 11 is not media");
        assert_eq!(seqs(&run), vec![10, 12, 13]);
    }

    #[test]
    fn what_is_held_is_bounded_and_a_large_key_frame_still_switches() {
        let t0 = Instant::now();
        // No key frame at all: the oldest is dropped.
        let mut wait = SwitchWait::default();
        for seq in 0..1000_u64 {
            assert!(wait.offer(&light(seq, 7, false, false, t0)).is_none());
            assert!(wait.packets.len() <= SWITCH_WAIT_PACKETS);
        }
        // A key frame of more packets than are held: the switch happens at
        // its first packet once the bound is reached.
        let mut wait = SwitchWait::default();
        assert!(wait.offer(&light(100, 7, true, false, t0)).is_none());
        let mut run = None;
        for seq in 101..400_u64 {
            run = wait.offer(&light(seq, 7, false, false, t0));
            if run.is_some() {
                break;
            }
        }
        let run = run.expect("switched at the bound");
        assert_eq!(run.len(), SWITCH_WAIT_PACKETS + 1);
        assert_eq!(*run[0].seq_no, 100);
        // FEC notes are bounded too.
        for seq in 0..1000_u64 {
            wait.note_fec(seq);
        }
        assert!(wait.fec_seqs.len() <= SWITCH_WAIT_PACKETS);
    }

    #[test]
    fn old_packets_and_another_source_stream_are_not_kept() {
        let t0 = Instant::now();
        let mut wait = SwitchWait::default();
        assert!(wait.offer(&light(10, 200, true, false, t0)).is_none());
        // More than SWITCH_WAIT_MAX_AGE later the first packet is gone, so
        // the rest of its frame does not complete it.
        let late = t0 + SWITCH_WAIT_MAX_AGE + Duration::from_millis(1);
        assert!(wait.offer(&light(11, 200, false, true, late)).is_none());
        assert_eq!(seqs(&wait.packets), vec![11]);
        // A packet of another SSRC starts the list over.
        let other = RelayPacket {
            ssrc: 0x9999.into(),
            ..light(500, 1, false, false, late)
        };
        assert!(wait.offer(&other).is_none());
        assert_eq!(seqs(&wait.packets), vec![500]);
    }

    #[test]
    fn forward_keeps_the_old_layer_on_the_wire_until_the_key_frame_is_whole() {
        let source = pid("pub");
        let src_mid = Mid::new();
        let t0 = Instant::now();
        let (q, f) = (Rid::from("q"), Rid::from("f"));
        let mut p = MediaPeer::new(pid("sub"), t0);
        p.tracks_out.push(TrackOutEntry {
            source_peer: source.clone(),
            source_mid: src_mid,
            kind: MediaKind::Video,
            state: TrackOutState::Open(Mid::new()),
            role: TrackRole::Main,
            chosen_rid: Some(q),
            forwarding: Forwarding {
                active_rid: Some(f),
                ..Forwarding::default()
            },
        });
        let on_track = |packet: RelayPacket| RelayPacket {
            mid: src_mid,
            ..packet
        };
        // The key frame's first packet alone does not switch.
        let first = on_track(light(10, 200, true, false, t0));
        assert!(matches!(
            p.forward_packet(&source, &first),
            MediaForward::WrongLayer
        ));
        assert_eq!(p.tracks_out[0].forwarding.active_rid, Some(f));
        // The old layer still passes the gate (this unit peer has no
        // transport, so it stops at `NoWriter`, past the gate).
        let old = RelayPacket {
            rid: Some(f),
            ..on_track(light(77, 5, false, false, t0))
        };
        assert!(matches!(
            p.forward_packet(&source, &old),
            MediaForward::NoWriter
        ));
        // The last packet makes it whole: the light layer is on the wire.
        let last = on_track(light(11, 200, false, true, t0));
        assert!(matches!(
            p.forward_packet(&source, &last),
            MediaForward::NoWriter
        ));
        assert_eq!(p.tracks_out[0].forwarding.active_rid, Some(q));
        assert!(matches!(
            p.forward_packet(&source, &old),
            MediaForward::WrongLayer
        ));
    }

    // ============================================================
    // Two-track delivery of simulcast video
    // ============================================================

    /// A publisher that has received a packet on each of `rids` of `mid`,
    /// which is how a bare unit peer comes to know a track's layers.
    fn simulcast_publisher(name: &str, mid: Mid, rids: &[&str], t0: Instant) -> MediaPeer {
        let mut publisher = peer_in_call(name, "call");
        publisher.register_local_track_in(mid, MediaKind::Video);
        for (i, rid) in rids.iter().enumerate() {
            publisher.record_layer_packet(&layer_packet(mid, rid, i as u64, t0), t0);
        }
        publisher
    }

    fn roles(peer: &MediaPeer) -> Vec<TrackRole> {
        peer.tracks_out.iter().map(|t| t.role).collect()
    }

    fn open_all(peer: &mut MediaPeer) {
        for entry in &mut peer.tracks_out {
            entry.state = TrackOutState::Open(Mid::new());
        }
    }

    fn entry(peer: &MediaPeer, role: TrackRole) -> &TrackOutEntry {
        peer.tracks_out
            .iter()
            .find(|t| t.role == role)
            .expect("the track exists")
    }

    #[test]
    fn a_simulcast_source_yields_two_tracks_and_a_single_encoding_one() {
        let mut sub = MediaPeer::new(pid("sub"), Instant::now());
        let simulcast = TrackIn {
            mid: Mid::new(),
            kind: MediaKind::Video,
            simulcast: true,
            light: false,
            middle: false,
        };
        assert!(sub.observe_remote_track_in(pid("pub"), simulcast));
        assert_eq!(
            roles(&sub),
            vec![TrackRole::Main, TrackRole::LightCompanion]
        );
        assert!(sub
            .tracks_out
            .iter()
            .all(|t| t.state == TrackOutState::ToOpen && t.chosen_rid.is_none()));
        // Idempotent on (source, mid, role).
        assert!(!sub.observe_remote_track_in(pid("pub"), simulcast));
        assert_eq!(sub.tracks_out.len(), 2);

        // A single encoding: the main track alone.
        let mut plain = MediaPeer::new(pid("plain"), Instant::now());
        let single = TrackIn {
            mid: Mid::new(),
            kind: MediaKind::Video,
            simulcast: false,
            light: false,
            middle: false,
        };
        assert!(plain.observe_remote_track_in(pid("pub"), single));
        assert_eq!(roles(&plain), vec![TrackRole::Main]);
        // Audio never has a companion, whatever the flag says.
        let audio = TrackIn {
            mid: Mid::new(),
            kind: MediaKind::Audio,
            simulcast: true,
            light: false,
            middle: false,
        };
        assert!(plain.observe_remote_track_in(pid("pub"), audio));
        assert_eq!(roles(&plain), vec![TrackRole::Main, TrackRole::Main]);
    }

    #[test]
    fn a_track_is_simulcast_from_its_second_rid_and_is_reported_once() {
        let mid = Mid::new();
        let t0 = Instant::now();
        let mut publisher = peer_in_call("pub", "call");
        let announced = publisher.register_local_track_in(mid, MediaKind::Video);
        assert!(!announced.simulcast, "nothing says so yet");
        assert_eq!(publisher.simulcast_layout(mid), None);
        // One rid is a single encoding that happens to be named.
        publisher.record_layer_packet(&layer_packet(mid, "q", 1, t0), t0);
        assert_eq!(publisher.note_simulcast_track_in(mid), None);
        assert_eq!(publisher.simulcast_layout(mid), None);
        // The second rid makes it simulcast, once.
        publisher.record_layer_packet(&layer_packet(mid, "f", 1, t0), t0);
        let found = publisher
            .note_simulcast_track_in(mid)
            .expect("found to be simulcast");
        assert!(found.simulcast && found.mid == mid);
        assert!(publisher.tracks_in()[0].simulcast);
        assert_eq!(publisher.note_simulcast_track_in(mid), None, "only once");
        // An unknown mid and an audio track are never simulcast.
        assert_eq!(publisher.note_simulcast_track_in(Mid::new()), None);
    }

    #[test]
    fn a_late_discovered_simulcast_opens_the_companion_once() {
        let mid = Mid::new();
        let t0 = Instant::now();
        let mut router = MediaRouter::new();
        let mut publisher = peer_in_call("pub", "call");
        let plain = publisher.register_local_track_in(mid, MediaKind::Video);
        router.add_peer(publisher);
        router.add_peer(peer_in_call("sub", "call"));
        // Announced as a single encoding: the main track alone.
        propagate_new_tracks_in(
            &mut router,
            &[PendingTrackIn {
                origin: pid("pub"),
                track: plain,
            }],
        );
        assert_eq!(roles(find_peer(&router, "sub")), vec![TrackRole::Main]);
        // The main track is open and forwarding when the second rid shows.
        let publisher = router.peer_mut(&pid("pub")).unwrap();
        publisher.record_layer_packet(&layer_packet(mid, "q", 1, t0), t0);
        publisher.record_layer_packet(&layer_packet(mid, "f", 1, t0), t0);
        let found = publisher.note_simulcast_track_in(mid).expect("simulcast");
        let again = [PendingTrackIn {
            origin: pid("pub"),
            track: found,
        }];
        propagate_new_tracks_in(&mut router, &again);
        propagate_new_tracks_in(&mut router, &again);
        let sub = find_peer(&router, "sub");
        assert_eq!(
            roles(sub),
            vec![TrackRole::Main, TrackRole::LightCompanion],
            "the companion is added once, the main track is left alone"
        );
        // A joiner after that is backfilled with both.
        router.add_peer(peer_in_call("late", "call"));
        backfill_existing_tracks(&mut router, &pid("late"));
        assert_eq!(
            roles(find_peer(&router, "late")),
            vec![TrackRole::Main, TrackRole::LightCompanion]
        );
    }

    #[test]
    fn the_layout_names_q_light_and_the_other_layer_heaviest() {
        let mid = Mid::new();
        let t0 = Instant::now();
        let (q, f) = (Rid::from("q"), Rid::from("f"));
        let mut publisher = simulcast_publisher("pub", mid, &["q", "f"], t0);
        // Known by name, with nothing measured.
        assert_eq!(
            publisher.simulcast_layout(mid),
            Some(SimulcastLayout {
                light: Some(q),
                heaviest: Some(f)
            })
        );

        // Without a `q`: nothing until every layer is measured, then the
        // lowest and the highest, and the light one is fixed from then on.
        let other = Mid::new();
        let (l, h) = (Rid::from("l"), Rid::from("h"));
        let mut unnamed = simulcast_publisher("pub2", other, &["l", "h"], t0);
        assert_eq!(
            unnamed.simulcast_layout(other),
            Some(SimulcastLayout::default())
        );
        unnamed
            .layer_meters
            .insert((other, l), meter_with_bps(100_000));
        unnamed
            .layer_meters
            .insert((other, h), meter_with_bps(900_000));
        let measured = SimulcastLayout {
            light: Some(l),
            heaviest: Some(h),
        };
        assert_eq!(unnamed.simulcast_layout(other), Some(measured));
        // The light layer's rate later exceeds the other's current rate
        // (the heavy one is being lost): the companion's layer stays.
        unnamed
            .layer_meters
            .insert((other, h), meter_with_bps(10_000));
        assert_eq!(unnamed.simulcast_layout(other).unwrap().light, Some(l));
    }

    #[test]
    fn the_companion_forwards_the_light_layer_only_and_the_main_track_the_heavy_one() {
        let mid = Mid::new();
        let t0 = Instant::now();
        let (q, f) = (Rid::from("q"), Rid::from("f"));
        let mut publisher = simulcast_publisher("pub", mid, &["q", "f"], t0);
        let layout = publisher.simulcast_layout(mid).unwrap();
        let track = publisher.note_simulcast_track_in(mid).unwrap();
        let source = pid("pub");
        let mut sub = MediaPeer::new(pid("sub"), t0);
        sub.observe_remote_track_in(source.clone(), track);
        open_all(&mut sub);

        // Before the layout is applied the companion takes nothing.
        let q_delta = light(10, 100, false, true, t0);
        let q_delta = RelayPacket { mid, ..q_delta };
        assert_eq!(sub.forward_light(&source, &q_delta), None);

        sub.apply_simulcast_layout(&source, mid, layout);
        assert_eq!(entry(&sub, TrackRole::Main).chosen_rid, Some(f));
        assert_eq!(entry(&sub, TrackRole::LightCompanion).chosen_rid, Some(q));
        assert!(sub.waits_to_start(&source, mid, TrackRole::Main));
        assert!(sub.waits_to_start(&source, mid, TrackRole::LightCompanion));

        // A heavy packet is not the companion's business at all, and a
        // light packet is the wrong layer for the main track: the first
        // layer to arrive does not latch it.
        let f_delta = RelayPacket {
            rid: Some(f),
            ..q_delta.clone()
        };
        assert_eq!(sub.forward_light(&source, &f_delta), None);
        assert_eq!(
            sub.forward_packet(&source, &q_delta),
            MediaForward::WrongLayer
        );
        assert_eq!(entry(&sub, TrackRole::Main).chosen_rid, Some(f));

        // Each begins at a whole key frame of its own layer: a delta
        // frame is held back, the key frame starts the stream. (This unit
        // peer has no transport, so a packet past the gate is `NoWriter`.)
        assert_eq!(
            sub.forward_light(&source, &q_delta),
            Some(MediaForward::WrongLayer)
        );
        assert_eq!(
            entry(&sub, TrackRole::LightCompanion).forwarding.active_rid,
            None
        );
        let q_key = RelayPacket {
            mid,
            ..light(11, 200, true, true, t0)
        };
        assert_eq!(
            sub.forward_light(&source, &q_key),
            Some(MediaForward::NoWriter)
        );
        assert_eq!(
            entry(&sub, TrackRole::LightCompanion).forwarding.active_rid,
            Some(q)
        );
        assert!(!sub.waits_to_start(&source, mid, TrackRole::LightCompanion));
        let f_key = RelayPacket {
            mid,
            rid: Some(f),
            ..light(500, 200, true, true, t0)
        };
        assert_eq!(sub.forward_packet(&source, &f_key), MediaForward::NoWriter);
        assert_eq!(entry(&sub, TrackRole::Main).forwarding.active_rid, Some(f));

        // From here on, and whatever is selected for the main track, the
        // companion carries `q` and nothing else.
        assert!(sub.select_layer(&source, mid, Some(q)));
        assert_eq!(entry(&sub, TrackRole::Main).chosen_rid, Some(q));
        assert_eq!(entry(&sub, TrackRole::LightCompanion).chosen_rid, Some(q));
        assert!(sub.select_layer(&source, mid, Some(f)));
        assert_eq!(entry(&sub, TrackRole::LightCompanion).chosen_rid, Some(q));
        for seq in 600..640 {
            let f_more = RelayPacket {
                mid,
                rid: Some(f),
                ..light(seq, 300, seq == 600, true, t0)
            };
            assert_eq!(sub.forward_light(&source, &f_more), None);
        }
        assert_eq!(
            entry(&sub, TrackRole::LightCompanion).forwarding.active_rid,
            Some(q)
        );
        // The layout is applied again on every packet: nothing changes.
        sub.apply_simulcast_layout(
            &source,
            mid,
            SimulcastLayout {
                light: Some(f),
                heaviest: Some(q),
            },
        );
        assert_eq!(entry(&sub, TrackRole::LightCompanion).chosen_rid, Some(q));
    }

    #[test]
    fn the_policies_leave_the_companion_alone_and_put_the_main_track_on_the_heavy_layer() {
        let mid = Mid::new();
        let t0 = Instant::now();
        let (q, f) = (Rid::from("q"), Rid::from("f"));
        // The main track latched the light layer (it arrived first), with
        // no bandwidth estimate; a companion sits beside it.
        let mut router = fallback_router(mid, t0, Some("q"), None);
        let sub = router.peer_mut(&pid("sub")).unwrap();
        sub.tracks_out.push(TrackOutEntry {
            source_peer: pid("pub"),
            source_mid: mid,
            kind: MediaKind::Video,
            state: TrackOutState::Open(Mid::new()),
            role: TrackRole::LightCompanion,
            chosen_rid: Some(q),
            forwarding: Forwarding {
                active_rid: Some(q),
                ..Forwarding::default()
            },
        });
        drive_uplink_fallback(&mut router, t0);
        let sub = find_peer(&router, "sub");
        assert_eq!(entry(sub, TrackRole::Main).chosen_rid, Some(f));
        assert_eq!(entry(sub, TrackRole::LightCompanion).chosen_rid, Some(q));

        // A hole on the full layer: the fallback (switched on in this
        // router) moves the main track, and only the main track.
        router
            .peer_mut(&pid("pub"))
            .unwrap()
            .observe_uplink_holes(&layer_packet(mid, "f", 3, t0));
        let old = t0 + uplink_fallback::HOLE_AGE + Duration::from_millis(1);
        assert_eq!(drive_uplink_fallback(&mut router, old).to_fallback, 1);
        let sub = find_peer(&router, "sub");
        assert_eq!(entry(sub, TrackRole::Main).chosen_rid, Some(q));
        let companion = entry(sub, TrackRole::LightCompanion);
        assert_eq!(companion.chosen_rid, Some(q));
        assert!(!companion.forwarding.held_light);

        // The bandwidth policy, with room for the full layer only on
        // paper for the companion: it is not looked at.
        let mut router = fallback_router(mid, t0, Some("f"), Some(200_000));
        let sub = router.peer_mut(&pid("sub")).unwrap();
        sub.tracks_out.insert(
            0,
            TrackOutEntry {
                source_peer: pid("pub"),
                source_mid: mid,
                kind: MediaKind::Video,
                state: TrackOutState::Open(Mid::new()),
                role: TrackRole::LightCompanion,
                chosen_rid: Some(f),
                forwarding: Forwarding::default(),
            },
        );
        let summary = drive_layer_selection(&mut router);
        assert_eq!(summary.downgrades, 1, "the main track alone");
        let sub = find_peer(&router, "sub");
        assert_eq!(entry(sub, TrackRole::Main).chosen_rid, Some(q));
        assert_eq!(entry(sub, TrackRole::LightCompanion).chosen_rid, Some(f));
    }

    #[test]
    fn a_waiting_track_is_asked_for_until_it_begins_and_not_for_ever() {
        let mid = Mid::new();
        let t0 = Instant::now();
        let mut router = MediaRouter::new();
        let mut publisher = simulcast_publisher("pub", mid, &["q", "f"], t0);
        let track = publisher.note_simulcast_track_in(mid).unwrap();
        router.add_peer(publisher);
        let mut sub = peer_in_call("sub", "call");
        sub.observe_remote_track_in(pid("pub"), track);
        router.add_peer(sub);

        // Not open yet: given their layers, asked for nothing.
        assert_eq!(drive_two_track_delivery(&mut router, t0), 0);
        let sub = router.peer_mut(&pid("sub")).unwrap();
        assert_eq!(
            entry(sub, TrackRole::LightCompanion).chosen_rid,
            Some(Rid::from("q"))
        );
        assert_eq!(entry(sub, TrackRole::Main).chosen_rid, Some(Rid::from("f")));
        assert!(sub.due_start_keyframes(t0).is_empty());

        // Open: both are due at once, each on its own layer, then not
        // again before the retry interval.
        open_all(sub);
        let due = sub.due_start_keyframes(t0);
        let asked: Vec<(TrackRole, Option<Rid>)> = due.iter().map(|t| (t.role, t.rid)).collect();
        assert_eq!(
            asked,
            vec![
                (TrackRole::Main, Some(Rid::from("f"))),
                (TrackRole::LightCompanion, Some(Rid::from("q")))
            ]
        );
        let almost = t0 + START_KEYFRAME_RETRY.saturating_sub(Duration::from_millis(1));
        assert!(sub.due_start_keyframes(almost).is_empty());
        assert_eq!(sub.due_start_keyframes(t0 + START_KEYFRAME_RETRY).len(), 2);

        // The companion begins: only the main track is still asked for.
        let q_key = RelayPacket {
            mid,
            ..light(11, 200, true, true, t0)
        };
        sub.forward_light(&pid("pub"), &q_key);
        let mut at = t0 + START_KEYFRAME_RETRY;
        let mut asked = 2;
        loop {
            at += START_KEYFRAME_RETRY;
            let due = sub.due_start_keyframes(at);
            if due.is_empty() {
                break;
            }
            assert_eq!(due.len(), 1);
            assert_eq!(due[0].role, TrackRole::Main);
            asked += 1;
            assert!(asked <= START_KEYFRAME_REQUESTS, "bounded");
        }
        assert_eq!(asked, START_KEYFRAME_REQUESTS);
        // Given up: it begins at the next packet of its layer.
        assert!(!sub.waits_to_start(&pid("pub"), mid, TrackRole::Main));
        let f_delta = RelayPacket {
            mid,
            rid: Some(Rid::from("f")),
            ..light(900, 300, false, false, at)
        };
        assert_eq!(
            sub.forward_packet(&pid("pub"), &f_delta),
            MediaForward::NoWriter
        );
        assert_eq!(
            entry(sub, TrackRole::Main).forwarding.active_rid,
            Some(Rid::from("f"))
        );
    }

    #[test]
    fn the_fanout_and_the_ledger_tell_the_two_tracks_apart() {
        let mid = Mid::new();
        let t0 = Instant::now();
        let mut router = MediaRouter::new();
        let mut publisher = simulcast_publisher("pub", mid, &["q", "f"], t0);
        let track = publisher.note_simulcast_track_in(mid).unwrap();
        router.add_peer(publisher);
        let mut sub = peer_in_call("sub", "call");
        sub.observe_remote_track_in(pid("pub"), track);
        open_all(&mut sub);
        router.add_peer(sub);
        // A single-encoding subscriber-side view for comparison: a third
        // peer with the main track only.
        let mut plain = peer_in_call("plain", "call");
        plain.observe_remote_track_in(
            pid("pub"),
            TrackIn {
                simulcast: false,
                light: false,
                middle: false,
                ..track
            },
        );
        open_all(&mut plain);
        router.add_peer(plain);

        let key = |rid: &str, seq: u64| PendingFanout {
            origin: pid("pub"),
            packet: RelayPacket {
                mid,
                rid: Some(Rid::from(rid)),
                ..light(seq, 200, true, true, t0)
            },
        };
        let mut ledger = FanoutLedger::new(Duration::from_secs(10), t0);
        let outcome = fanout_packets_into(
            &mut router,
            &[key("q", 10), key("f", 500)],
            Some(&mut ledger),
        );
        // Two packets to two peers, and the light one once more to the
        // peer with a companion. No transport: each is `NoWriter` or, for
        // a layer the main track does not carry, `WrongLayer`.
        assert_eq!(outcome.attempted(), 5);
        let of = |target: &str, role: TrackRole| {
            ledger
                .per_target
                .get(&(pid(target), pid("pub"), mid, role))
                .copied()
        };
        let main = of("sub", TrackRole::Main).expect("the main track has a line");
        assert_eq!((main.wrong_layer, main.no_writer), (1, 1));
        let companion = of("sub", TrackRole::LightCompanion).expect("and the companion its own");
        assert_eq!(companion.attempted(), 1);
        assert_eq!(companion.no_writer, 1);
        assert_eq!(of("plain", TrackRole::LightCompanion), None);
        assert_eq!(of("plain", TrackRole::Main).unwrap().attempted(), 2);
        assert_eq!(ledger.flush_if_due(t0 + Duration::from_secs(10)), 4);

        let sub = find_peer(&router, "sub");
        assert_eq!(
            entry(sub, TrackRole::Main).forwarding.active_rid,
            Some(Rid::from("f"))
        );
        assert_eq!(
            entry(sub, TrackRole::LightCompanion).forwarding.active_rid,
            Some(Rid::from("q"))
        );
    }

    #[test]
    fn the_companions_stream_id_carries_the_suffix_through_strs_filter() {
        let t0 = Instant::now();
        let mut sub = MediaPeer::new(pid("sub"), t0);
        let source = crate::peer::PeerId::fresh();
        assert_eq!(
            MediaPeer::downstream_stream_id(&source, TrackRole::Main),
            source.to_string()
        );
        let light_id = format!("{source}{LIGHT_STREAM_SUFFIX}");
        assert_eq!(
            MediaPeer::downstream_stream_id(&source, TrackRole::LightCompanion),
            light_id
        );
        assert_eq!(LIGHT_STREAM_SUFFIX, "~light");
        sub.observe_remote_track_in(
            source.clone(),
            TrackIn {
                mid: Mid::new(),
                kind: MediaKind::Video,
                simulcast: true,
                light: false,
                middle: false,
            },
        );
        let offer = sub
            .negotiate_if_needed()
            .expect("negotiates")
            .expect("an offer");
        let msids: Vec<&str> = offer
            .lines()
            .filter_map(|l| l.strip_prefix("a=msid:"))
            .filter_map(|l| l.split_whitespace().next())
            .collect();
        assert_eq!(msids, vec![source.to_string().as_str(), light_id.as_str()]);
        // One offer opened both, and no line still carries the id as str0m
        // wrote it.
        assert!(sub
            .tracks_out
            .iter()
            .all(|t| matches!(t.state, TrackOutState::Negotiating(_))));
        let written = light_id.replace('~', "");
        assert!(
            !offer
                .split(|c: char| c.is_whitespace() || c == ':')
                .any(|token| token == written),
            "{offer}"
        );
        // A session without a companion is passed through untouched.
        let plain = MediaPeer::new(pid("plain"), t0);
        assert_eq!(plain.restore_light_stream_ids(offer.clone()), offer);
    }

    /// Why [`MediaPeer::restore_light_stream_ids`] exists: str0m drops the
    /// `~` from a stream id it is given. If this test fails, str0m has
    /// started keeping it and the rewrite can go.
    #[test]
    fn str0m_drops_the_tilde_from_a_stream_id() {
        let mut rtc = Rtc::builder().build(Instant::now());
        let mut change = rtc.sdp_api();
        change.add_media(
            MediaKind::Video,
            Direction::SendOnly,
            Some("peer~light".to_owned()),
            None,
            None,
        );
        let (offer, _pending) = change.apply().expect("an offer");
        let sdp = offer.to_sdp_string();
        assert!(sdp.contains("a=msid:peerlight "), "{sdp}");
        assert!(!sdp.contains('~'), "{sdp}");
    }

    // ============================================================
    // A subscriber's REMB is not a budget for layer selection
    // ============================================================

    /// A simulcast publisher (`q` 150 kbit/s, `f` 1.5 Mbit/s) and a
    /// subscriber whose main track is selected to `main_on`.
    fn remb_router(main_on: &str) -> (MediaRouter, Mid) {
        let t0 = Instant::now();
        let mid = Mid::new();
        let mut publisher = simulcast_publisher("pub", mid, &["q", "f"], t0);
        publisher
            .layer_meters
            .insert((mid, Rid::from("q")), meter_with_bps(150_000));
        publisher
            .layer_meters
            .insert((mid, Rid::from("f")), meter_with_bps(1_500_000));
        let mut sub = peer_in_call("sub", "call");
        for (role, rid) in [(TrackRole::Main, main_on), (TrackRole::LightCompanion, "q")] {
            sub.tracks_out.push(TrackOutEntry {
                source_peer: pid("pub"),
                source_mid: mid,
                kind: MediaKind::Video,
                state: TrackOutState::Open(Mid::new()),
                role,
                chosen_rid: Some(Rid::from(rid)),
                forwarding: Forwarding::default(),
            });
        }
        let mut router = MediaRouter::new();
        router.add_peer(publisher);
        router.add_peer(sub);
        (router, mid)
    }

    fn main_layer(router: &MediaRouter) -> Option<Rid> {
        entry(find_peer(router, "sub"), TrackRole::Main).chosen_rid
    }

    /// The live finding of 2026-10-04. Under server-side bitrate control a
    /// browser subscriber sends REMB, and str0m hands it over as an egress
    /// estimate whether or not its own estimator is on. A REMB follows what
    /// the subscriber is being sent, so the moment the publisher's heavy
    /// layer paused, the figure fell below the heavy layer's rate; taken as
    /// the budget it moved the main track to the light layer, where the
    /// figure could never grow back.
    #[test]
    fn a_subscribers_remb_does_not_move_its_main_track_to_the_light_layer() {
        let (mut router, mid) = remb_router("f");
        let sub = router.peer_mut(&pid("sub")).expect("the subscriber");
        // What a subscriber receiving the light layer alone reports.
        sub.note_egress_estimate(&BweKind::Remb(mid, Bitrate::kbps(300)));
        assert_eq!(sub.egress_estimate_bps(), None, "a REMB is not a budget");

        let summary = drive_layer_selection(&mut router);
        assert_eq!(summary, LayerSelectionSummary::default());
        assert_eq!(main_layer(&router), Some(Rid::from("f")));
        assert_eq!(
            entry(find_peer(&router, "sub"), TrackRole::LightCompanion).chosen_rid,
            Some(Rid::from("q"))
        );

        // A send-side estimate still is one, and still downgrades.
        let sub = router.peer_mut(&pid("sub")).expect("the subscriber");
        sub.note_egress_estimate(&BweKind::Twcc(Bitrate::kbps(300)));
        assert_eq!(sub.egress_estimate_bps(), Some(300_000));
        let summary = drive_layer_selection(&mut router);
        assert_eq!(summary.downgrades, 1);
        assert_eq!(main_layer(&router), Some(Rid::from("q")));
    }

    /// The other half of the same finding: a main track that sits on the
    /// light layer is put back on the heaviest by the default-layer rule,
    /// which applies to a subscriber without an estimate. With a REMB taken
    /// as one, the rule never applied and the track stayed.
    #[test]
    fn a_main_track_on_the_light_layer_goes_back_to_the_heaviest_despite_remb() {
        let (mut router, mid) = remb_router("q");
        router
            .peer_mut(&pid("sub"))
            .expect("the subscriber")
            .note_egress_estimate(&BweKind::Remb(mid, Bitrate::kbps(450)));
        let _ = drive_uplink_fallback(&mut router, Instant::now());
        let _ = drive_layer_selection(&mut router);
        assert_eq!(main_layer(&router), Some(Rid::from("f")));
    }

    // ============================================================
    // Two-track delivery from two senders (a declared light track)
    // ============================================================

    /// The SDP offer of a client with one audio sender, two video senders
    /// and one video receiver, and the four mids in that order.
    fn two_sender_offer() -> (String, [Mid; 4]) {
        let mut rtc = Rtc::builder().build(Instant::now());
        let mut change = rtc.sdp_api();
        let mut add = |kind, dir| change.add_media(kind, dir, None, None, None);
        let mids = [
            add(MediaKind::Audio, Direction::SendOnly),
            add(MediaKind::Video, Direction::SendOnly),
            add(MediaKind::Video, Direction::SendOnly),
            add(MediaKind::Video, Direction::RecvOnly),
        ];
        let (offer, _pending) = change.apply().expect("an offer");
        (offer.to_sdp_string(), mids)
    }

    /// Register every m-line of [`two_sender_offer`] as the media loop
    /// does when str0m announces it (`Event::MediaAdded`, which str0m
    /// holds back until the transport is up, so a unit peer never sees
    /// one).
    fn announce_tracks(peer: &mut MediaPeer, mids: [Mid; 4]) {
        let kinds = [
            MediaKind::Audio,
            MediaKind::Video,
            MediaKind::Video,
            MediaKind::Video,
        ];
        for (mid, kind) in mids.into_iter().zip(kinds) {
            peer.register_local_track_in(mid, kind);
        }
    }

    fn mids_of(mids: &[Mid]) -> Vec<String> {
        mids.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn a_declared_light_mid_is_taken_only_for_a_video_line_the_peer_sends_on() {
        let (offer, [audio, main, light, receiving]) = two_sender_offer();
        let mut peer = MediaPeer::new(pid("pub"), Instant::now());
        peer.accept_offer(&offer).expect("the offer is accepted");

        // An audio line, an unknown mid and a line the peer only receives
        // on: each ignored, with its reason.
        let refused = peer.declare_light_tracks(&[
            audio.to_string(),
            "nope".to_owned(),
            receiving.to_string(),
        ]);
        assert_eq!(
            refused,
            vec![
                (audio, LightDeclarationRefused::NotVideo),
                (Mid::from("nope"), LightDeclarationRefused::UnknownMid),
                (receiving, LightDeclarationRefused::NotSentByPeer),
            ]
        );
        assert_eq!(peer.light_mid(), None);

        // The second video sender is taken; the first, named after it, is
        // not: one light track per peer, the first.
        let refused = peer.declare_light_tracks(&mids_of(&[light, main]));
        assert_eq!(
            refused,
            vec![(main, LightDeclarationRefused::AlreadyDeclared)]
        );
        assert_eq!(peer.light_mid(), Some(light));
        // Named again, as a client that repeats the list does: a no-op.
        assert!(peer.declare_light_tracks(&mids_of(&[light])).is_empty());

        announce_tracks(&mut peer, [audio, main, light, receiving]);
        let track = |mid: Mid| {
            *peer
                .tracks_in()
                .iter()
                .find(|t| t.mid == mid)
                .expect("the track was announced")
        };
        assert!(track(light).light, "the declared sender is the light track");
        assert!(!track(main).light && !track(audio).light);
        assert_eq!(track(audio).kind, MediaKind::Audio);
        // A light track is never taken for simulcast.
        assert_eq!(peer.note_simulcast_track_in(light), None);
    }

    #[test]
    fn a_track_announced_without_the_declaration_stays_a_main_track() {
        let (offer, mids) = two_sender_offer();
        let light = mids[2];
        let mut peer = MediaPeer::new(pid("pub"), Instant::now());
        peer.accept_offer(&offer).expect("the offer is accepted");
        announce_tracks(&mut peer, mids);
        let refused = peer.declare_light_tracks(&mids_of(&[light]));
        assert_eq!(refused, vec![(light, LightDeclarationRefused::AlreadyMain)]);
        assert_eq!(peer.light_mid(), None);
        assert!(peer.tracks_in().iter().all(|t| !t.light));
    }

    #[test]
    fn a_declared_light_track_opens_as_a_companion_and_never_as_a_main_track() {
        let (main_mid, light_mid) = (Mid::from("1"), Mid::from("2"));
        let light = TrackIn {
            mid: light_mid,
            kind: MediaKind::Video,
            simulcast: false,
            light: true,
            middle: false,
        };
        let main = TrackIn {
            mid: main_mid,
            kind: MediaKind::Video,
            simulcast: false,
            light: false,
            middle: false,
        };
        let mut sub = MediaPeer::new(pid("sub"), Instant::now());
        assert!(sub.observe_remote_track_in(pid("pub"), main));
        assert!(sub.observe_remote_track_in(pid("pub"), light));
        assert!(
            !sub.observe_remote_track_in(pid("pub"), light),
            "idempotent"
        );
        let got: Vec<(Mid, TrackRole)> = sub
            .tracks_out
            .iter()
            .map(|t| (t.source_mid, t.role))
            .collect();
        assert_eq!(
            got,
            vec![
                (main_mid, TrackRole::Main),
                (light_mid, TrackRole::LightCompanion)
            ]
        );
        // The offer names the two stream ids of the contract.
        let offer = sub
            .negotiate_if_needed()
            .expect("negotiates")
            .expect("an offer");
        assert!(offer.contains("a=msid:pub "), "{offer}");
        assert!(offer.contains("a=msid:pub~light "), "{offer}");
    }

    #[test]
    fn a_packet_of_a_declared_light_track_is_offered_to_the_companion_alone() {
        let t0 = Instant::now();
        let light_mid = Mid::from("2");
        let mut publisher = peer_in_call("pub", "call");
        publisher.light_mid = Some(light_mid);
        let track = publisher.register_local_track_in(light_mid, MediaKind::Video);
        assert!(track.light);
        let mut sub = peer_in_call("sub", "call");
        assert!(sub.observe_remote_track_in(pid("pub"), track));
        open_all(&mut sub);
        let mut router = MediaRouter::new();
        router.add_peer(publisher);
        router.add_peer(sub);

        let mut packet = synthetic_video_packet();
        packet.mid = light_mid;
        let pending = [PendingFanout {
            origin: pid("pub"),
            packet,
        }];
        let mut ledger = FanoutLedger::new(Duration::from_secs(5), t0);
        let outcome = fanout_packets_into(&mut router, &pending, Some(&mut ledger));
        // One attempt, on the companion (the unit peer has no transport,
        // so it ends as `no_writer`); nothing is filed for a main track.
        assert_eq!(outcome.attempted(), 1);
        assert_eq!(outcome.no_writer, 1);
        let roles: Vec<TrackRole> = ledger.per_target.keys().map(|k| k.3).collect();
        assert_eq!(roles, vec![TrackRole::LightCompanion]);
        // The companion carries a whole stream: no layer is selected.
        let sub = find_peer(&router, "sub");
        assert_eq!(entry(sub, TrackRole::LightCompanion).chosen_rid, None);
        assert_eq!(drive_two_track_delivery(&mut router, t0), 0);
    }

    // ============================================================
    // ADR-0070 step 5: a middle sender, and what a subscriber shows
    // ============================================================

    #[test]
    fn a_middle_declaration_follows_the_rules_of_a_light_one() {
        let (offer, [audio, first, second, receiving]) = two_sender_offer();
        let mut peer = MediaPeer::new(pid("pub"), Instant::now());
        peer.accept_offer(&offer).expect("the offer is accepted");

        // An audio line, an unknown mid and a line the peer only receives
        // on: each ignored, with its reason.
        let refused = peer.declare_middle_tracks(&[
            audio.to_string(),
            "nope".to_owned(),
            receiving.to_string(),
        ]);
        assert_eq!(
            refused,
            vec![
                (audio, LightDeclarationRefused::NotVideo),
                (Mid::from("nope"), LightDeclarationRefused::UnknownMid),
                (receiving, LightDeclarationRefused::NotSentByPeer),
            ]
        );
        assert_eq!(peer.middle_mid(), None);

        // The light track is declared first, as the media task does. The
        // same mid cannot be the middle track as well; the other can, and
        // then no further one: at most one middle track per peer.
        assert!(peer.declare_light_tracks(&mids_of(&[second])).is_empty());
        let refused = peer.declare_middle_tracks(&mids_of(&[second, first, audio]));
        assert_eq!(
            refused,
            vec![
                (second, LightDeclarationRefused::DeclaredOtherwise),
                (audio, LightDeclarationRefused::AlreadyDeclared),
            ]
        );
        assert_eq!(peer.middle_mid(), Some(first));
        assert_eq!(peer.light_mid(), Some(second));
        // Named again: a no-op. And the middle mid is no light track.
        assert!(peer.declare_middle_tracks(&mids_of(&[first])).is_empty());
        assert_eq!(
            peer.declare_light_tracks(&mids_of(&[first])),
            vec![(first, LightDeclarationRefused::DeclaredOtherwise)]
        );

        announce_tracks(&mut peer, [audio, first, second, receiving]);
        let track = |mid: Mid| {
            *peer
                .tracks_in()
                .iter()
                .find(|t| t.mid == mid)
                .expect("the track was announced")
        };
        assert!(track(first).middle && !track(first).light);
        assert!(track(second).light && !track(second).middle);
        assert!(!track(audio).middle);
        assert_eq!(track(first).role(), TrackRole::Middle);
        assert_eq!(track(second).role(), TrackRole::LightCompanion);
        assert_eq!(track(audio).role(), TrackRole::Main);
        // A middle track is never taken for simulcast.
        assert_eq!(peer.note_simulcast_track_in(first), None);
    }

    #[test]
    fn a_middle_track_announced_without_the_declaration_stays_a_main_track() {
        let (offer, mids) = two_sender_offer();
        let mut peer = MediaPeer::new(pid("pub"), Instant::now());
        peer.accept_offer(&offer).expect("the offer is accepted");
        announce_tracks(&mut peer, mids);
        let refused = peer.declare_middle_tracks(&mids_of(&[mids[1]]));
        assert_eq!(
            refused,
            vec![(mids[1], LightDeclarationRefused::AlreadyMain)]
        );
        assert_eq!(peer.middle_mid(), None);
        assert!(peer.tracks_in().iter().all(|t| !t.middle));
    }

    const MAIN_MID: &str = "1";
    const LIGHT_MID: &str = "2";
    const MIDDLE_MID: &str = "3";

    fn video_track_in(mid: &str, role: TrackRole) -> TrackIn {
        TrackIn {
            mid: Mid::from(mid),
            kind: MediaKind::Video,
            simulcast: false,
            light: role == TrackRole::LightCompanion,
            middle: role == TrackRole::Middle,
        }
    }

    /// A subscriber with the three downstream tracks of `publisher`
    /// open: main, light and middle.
    fn three_track_subscriber(publisher: &str) -> MediaPeer {
        let mut sub = peer_in_call("sub", "call");
        add_three_tracks(&mut sub, publisher);
        sub
    }

    fn add_three_tracks(sub: &mut MediaPeer, publisher: &str) {
        for (mid, role) in [
            (MAIN_MID, TrackRole::Main),
            (LIGHT_MID, TrackRole::LightCompanion),
            (MIDDLE_MID, TrackRole::Middle),
        ] {
            assert!(sub.observe_remote_track_in(pid(publisher), video_track_in(mid, role)));
        }
        open_all(sub);
        // What the answer to the renegotiation does.
        sub.apply_video_wants();
    }

    fn want(peer: &str, quality: VideoQuality) -> VideoWant {
        VideoWant {
            peer: peer.to_owned(),
            quality,
        }
    }

    fn video_packet_on(mid: &str, seq_no: u64, key_frame: bool) -> RelayPacket {
        RelayPacket {
            mid: Mid::from(mid),
            seq_no: seq_no.into(),
            starts_independent_frame: key_frame,
            blind: false,
            ..synthetic_video_packet()
        }
    }

    /// Offer one packet of each of `publisher`'s three tracks to `sub`
    /// and say which were taken: `(main, middle, light)`. A unit peer
    /// has no transport, so a packet that passed the gate ends as
    /// `NoWriter`, and one the subscriber does not show as `NotWanted`.
    fn taken(sub: &mut MediaPeer, publisher: &str, key_frame: bool) -> (bool, bool, bool) {
        let source = pid(publisher);
        let main = sub.forward_packet(&source, &video_packet_on(MAIN_MID, 10, key_frame));
        let middle = sub.forward_middle_track(&source, &video_packet_on(MIDDLE_MID, 10, key_frame));
        let light = sub.forward_light_track(&source, &video_packet_on(LIGHT_MID, 10, key_frame));
        let passed = |f: MediaForward| match f {
            MediaForward::NoWriter => true,
            MediaForward::NotWanted => false,
            other => panic!("unexpected outcome {other:?}"),
        };
        (passed(main), passed(middle), passed(light))
    }

    #[test]
    fn a_declared_middle_track_opens_as_a_middle_track_with_its_own_stream_id() {
        let mut sub = MediaPeer::new(pid("sub"), Instant::now());
        for (mid, role) in [
            (MAIN_MID, TrackRole::Main),
            (LIGHT_MID, TrackRole::LightCompanion),
            (MIDDLE_MID, TrackRole::Middle),
        ] {
            let track = video_track_in(mid, role);
            assert!(sub.observe_remote_track_in(pid("pub"), track));
            assert!(
                !sub.observe_remote_track_in(pid("pub"), track),
                "idempotent"
            );
        }
        assert_eq!(
            roles(&sub),
            vec![
                TrackRole::Main,
                TrackRole::LightCompanion,
                TrackRole::Middle
            ]
        );
        let offer = sub
            .negotiate_if_needed()
            .expect("negotiates")
            .expect("an offer");
        assert!(offer.contains("a=msid:pub "), "{offer}");
        assert!(offer.contains("a=msid:pub~light "), "{offer}");
        assert!(offer.contains("a=msid:pub~mid "), "{offer}");
        assert_eq!(
            MediaPeer::downstream_stream_id(&pid("pub"), TrackRole::Middle),
            "pub~mid"
        );
        assert_eq!(TrackRole::Middle.as_str(), "middle");
    }

    #[test]
    fn with_no_wants_a_subscriber_gets_the_main_and_the_light_track() {
        let mut sub = three_track_subscriber("pub");
        assert_eq!(sub.video_quality_for(&pid("pub")), VideoQuality::Main);
        assert_eq!(taken(&mut sub, "pub", false), (true, false, true));
        assert!(!sub.track_paused(&pid("pub"), Mid::from(MAIN_MID), TrackRole::Main));
        assert!(sub.track_paused(&pid("pub"), Mid::from(MIDDLE_MID), TrackRole::Middle));
        // Nothing was paused and wanted again: nobody is asked for a key
        // frame, and an empty set changes nothing.
        assert!(sub.due_resume_keyframes(Instant::now(), &[]).is_empty());
        assert_eq!(sub.set_video_wants(&[]), VideoWantsCount::default());
        assert_eq!(taken(&mut sub, "pub", false), (true, false, true));
    }

    #[test]
    fn each_quality_forwards_exactly_its_tracks() {
        let t0 = Instant::now();
        // Middle: the middle and the light track. The middle one was
        // paused, so it starts at a key frame.
        let mut sub = three_track_subscriber("pub");
        let count = sub.set_video_wants(&[want("pub", VideoQuality::Middle)]);
        assert_eq!(
            count,
            VideoWantsCount {
                main: 0,
                middle: 1,
                light: 0
            }
        );
        assert_eq!(sub.video_quality_for(&pid("pub")), VideoQuality::Middle);
        assert_eq!(
            taken(&mut sub, "pub", false),
            (false, false, true),
            "the main track stops at once; the middle one waits for a key frame"
        );
        assert_eq!(taken(&mut sub, "pub", true), (false, true, true));
        assert_eq!(taken(&mut sub, "pub", false), (false, true, true));

        // Light: neither heavy track.
        let mut sub = three_track_subscriber("pub");
        sub.set_video_wants(&[want("pub", VideoQuality::Light)]);
        assert_eq!(sub.video_quality_for(&pid("pub")), VideoQuality::Light);
        assert_eq!(taken(&mut sub, "pub", true), (false, false, true));
        assert!(sub.due_resume_keyframes(t0, &[]).is_empty());

        // Main, named: as with no wants.
        let mut sub = three_track_subscriber("pub");
        sub.set_video_wants(&[want("pub", VideoQuality::Main)]);
        assert_eq!(taken(&mut sub, "pub", false), (true, false, true));

        // Named twice: the last entry counts.
        let mut sub = three_track_subscriber("pub");
        sub.set_video_wants(&[
            want("pub", VideoQuality::Middle),
            want("pub", VideoQuality::Light),
        ]);
        assert_eq!(sub.video_quality_for(&pid("pub")), VideoQuality::Light);
    }

    #[test]
    fn middle_asked_of_a_publisher_without_a_middle_track_is_main() {
        let mut sub = peer_in_call("sub", "call");
        assert!(sub.observe_remote_track_in(pid("pub"), video_track_in(MAIN_MID, TrackRole::Main)));
        assert!(sub.observe_remote_track_in(
            pid("pub"),
            video_track_in(LIGHT_MID, TrackRole::LightCompanion)
        ));
        open_all(&mut sub);
        sub.set_video_wants(&[want("pub", VideoQuality::Middle)]);
        assert_eq!(sub.video_quality_for(&pid("pub")), VideoQuality::Main);
        let source = pid("pub");
        assert_eq!(
            sub.forward_packet(&source, &video_packet_on(MAIN_MID, 1, false)),
            MediaForward::NoWriter,
            "the main track goes on"
        );

        // The middle track arrives later and is still being negotiated:
        // main goes on. Once it is open the earlier want applies.
        assert!(
            sub.observe_remote_track_in(pid("pub"), video_track_in(MIDDLE_MID, TrackRole::Middle))
        );
        assert_eq!(sub.video_quality_for(&pid("pub")), VideoQuality::Main);
        open_all(&mut sub);
        sub.apply_video_wants();
        assert_eq!(sub.video_quality_for(&pid("pub")), VideoQuality::Middle);
        assert_eq!(taken(&mut sub, "pub", true), (false, true, true));
    }

    #[test]
    fn pausing_is_immediate_and_resuming_asks_for_a_key_frame_and_starts_at_one() {
        let t0 = Instant::now();
        let source = pid("pub");
        let mut sub = three_track_subscriber("pub");
        // The main track is flowing and has numbered some packets.
        let (map, _) = StreamMap::start(100, 9_000, t0);
        sub.tracks_out[0].forwarding.map = Some(map);
        sub.tracks_out[0].forwarding.source_ssrc = Some(synthetic_video_packet().ssrc);

        sub.set_video_wants(&[want("pub", VideoQuality::Light)]);
        // The very next packet is dropped, key frame or not.
        assert_eq!(
            sub.forward_packet(&source, &video_packet_on(MAIN_MID, 101, true)),
            MediaForward::NotWanted
        );
        assert!(
            sub.due_resume_keyframes(t0, &[]).is_empty(),
            "paused, not resuming"
        );

        // Wanted again: the publisher is asked at once, on the main track.
        sub.set_video_wants(&[]);
        let due = sub.due_resume_keyframes(t0, &[]);
        assert_eq!(due.len(), 1);
        assert_eq!(
            (due[0].source_peer.clone(), due[0].source_mid, due[0].role),
            (source.clone(), Mid::from(MAIN_MID), TrackRole::Main)
        );
        // Not again before the retry interval, then again after it.
        assert!(sub
            .due_resume_keyframes(t0 + Duration::from_millis(500), &[])
            .is_empty());
        assert_eq!(
            sub.due_resume_keyframes(t0 + START_KEYFRAME_RETRY, &[])
                .len(),
            1
        );

        // Packets that do not begin a key frame are still dropped.
        assert_eq!(
            sub.forward_packet(&source, &video_packet_on(MAIN_MID, 150, false)),
            MediaForward::NotWanted
        );
        assert!(sub.track_paused(&source, Mid::from(MAIN_MID), TrackRole::Main));
        // The first packet of a key frame starts the track, and the
        // stream is to continue at its next number.
        assert_eq!(
            sub.forward_packet(&source, &video_packet_on(MAIN_MID, 160, true)),
            MediaForward::NoWriter
        );
        assert!(!sub.track_paused(&source, Mid::from(MAIN_MID), TrackRole::Main));
        assert!(sub.tracks_out[0].forwarding.rebase_next);
        assert!(sub
            .due_resume_keyframes(t0 + Duration::from_secs(10), &[])
            .is_empty());
    }

    #[test]
    fn a_resumed_stream_continues_at_its_next_sequence_number() {
        let t0 = Instant::now();
        let packet = |seq: u64, at: Instant| RelayPacket {
            seq_no: seq.into(),
            rtp_time: u32::try_from(seq).unwrap() * 3_000,
            arrival: at,
            ..synthetic_video_packet()
        };
        let mut fw = Forwarding::default();
        let first = fw.position(&packet(100, t0)).expect("a slot");
        let second = fw
            .position(&packet(101, t0 + Duration::from_millis(33)))
            .expect("a slot");
        assert_eq!(second.seq_no, first.seq_no + 1);
        // A pause during which the publisher sent 200 packets, then the
        // first packet forwarded after it.
        fw.rebase_next = true;
        let resumed = fw
            .position(&packet(302, t0 + Duration::from_secs(7)))
            .expect("a slot");
        assert_eq!(resumed.seq_no, second.seq_no + 1, "no gap downstream");
        assert!(!fw.rebase_next);
        // The timestamp moved on by the seven seconds that passed.
        let moved = resumed.rtp_time.wrapping_sub(second.rtp_time);
        assert!((600_000..=640_000).contains(&moved), "{moved}");
        // And the stream goes on from there, a gap staying a gap.
        let next = fw
            .position(&packet(304, t0 + Duration::from_secs(7)))
            .expect("a slot");
        assert_eq!(next.seq_no, resumed.seq_no + 2);
        // A packet from before the resume point has no slot.
        assert!(fw
            .position(&packet(301, t0 + Duration::from_secs(7)))
            .is_none());
    }

    #[test]
    fn a_track_that_gets_no_key_frame_resumes_at_the_next_packet_after_five_requests() {
        let t0 = Instant::now();
        let source = pid("pub");
        let mut sub = three_track_subscriber("pub");
        sub.set_video_wants(&[want("pub", VideoQuality::Middle)]);
        let mut asked = 0;
        let mut at = t0;
        for _ in 0..u32::from(START_KEYFRAME_REQUESTS) + 3 {
            asked += sub.due_resume_keyframes(at, &[]).len();
            at += START_KEYFRAME_RETRY;
        }
        assert_eq!(asked, usize::from(START_KEYFRAME_REQUESTS));
        assert_eq!(
            sub.forward_middle_track(&source, &video_packet_on(MIDDLE_MID, 5, false)),
            MediaForward::NoWriter,
            "given up waiting: the next packet starts the track"
        );
        assert!(!sub.track_paused(&source, Mid::from(MIDDLE_MID), TrackRole::Middle));
    }

    #[test]
    fn wants_for_an_unknown_peer_change_nothing() {
        let mut sub = three_track_subscriber("pub");
        let count = sub.set_video_wants(&[
            want("nobody", VideoQuality::Light),
            want("someone-else", VideoQuality::Middle),
        ]);
        assert_eq!((count.middle, count.light), (1, 1));
        assert_eq!(sub.video_quality_for(&pid("pub")), VideoQuality::Main);
        assert_eq!(taken(&mut sub, "pub", false), (true, false, true));
        assert!(sub.due_resume_keyframes(Instant::now(), &[]).is_empty());
        // Asked of a peer it has no middle track of: main, as documented.
        assert_eq!(
            sub.video_quality_for(&pid("someone-else")),
            VideoQuality::Main
        );
        // More entries than the protocol allows are cut at the bound.
        let many: Vec<VideoWant> = (0..MAX_VIDEO_WANTS + 10)
            .map(|i| want(&format!("p{i}"), VideoQuality::Light))
            .collect();
        assert_eq!(sub.set_video_wants(&many).light, MAX_VIDEO_WANTS);
    }

    #[test]
    fn a_replaced_set_drops_what_the_new_one_omits() {
        let mut sub = three_track_subscriber("a");
        add_three_tracks(&mut sub, "b");
        sub.set_video_wants(&[
            want("a", VideoQuality::Light),
            want("b", VideoQuality::Middle),
        ]);
        assert_eq!(taken(&mut sub, "a", true), (false, false, true));
        assert_eq!(taken(&mut sub, "b", true), (false, true, true));

        // The new set names `b` alone, at light: `a` is back at main (and
        // starts at a key frame), `b`'s middle track stops at once.
        sub.set_video_wants(&[want("b", VideoQuality::Light)]);
        assert_eq!(sub.video_quality_for(&pid("a")), VideoQuality::Main);
        assert_eq!(taken(&mut sub, "b", true), (false, false, true));
        assert_eq!(taken(&mut sub, "a", false), (false, false, true));
        assert_eq!(taken(&mut sub, "a", true), (true, false, true));
        let due = sub.due_resume_keyframes(Instant::now(), &[]);
        assert!(due.is_empty(), "the track has started: {due:?}");
    }

    #[test]
    fn the_fanout_offers_a_middle_packet_to_the_middle_track_alone() {
        let t0 = Instant::now();
        let middle_mid = Mid::from(MIDDLE_MID);
        let mut publisher = peer_in_call("pub", "call");
        publisher.middle_mid = Some(middle_mid);
        let track = publisher.register_local_track_in(middle_mid, MediaKind::Video);
        assert!(track.middle);
        let mut sub = peer_in_call("sub", "call");
        assert!(sub.observe_remote_track_in(pid("pub"), track));
        open_all(&mut sub);
        sub.apply_video_wants();
        let mut router = MediaRouter::new();
        router.add_peer(publisher);
        router.add_peer(sub);

        let pending = [PendingFanout {
            origin: pid("pub"),
            packet: video_packet_on(MIDDLE_MID, 1, true),
        }];
        let mut ledger = FanoutLedger::new(Duration::from_secs(5), t0);
        // Not asked for: one attempt, on the middle track, not wanted.
        let outcome = fanout_packets_into(&mut router, &pending, Some(&mut ledger));
        assert_eq!((outcome.attempted(), outcome.not_wanted), (1, 1));
        let roles: Vec<TrackRole> = ledger.per_target.keys().map(|k| k.3).collect();
        assert_eq!(roles, vec![TrackRole::Middle]);
        note_playout_delays(&router, &mut ledger);
        assert!(ledger.per_target.values().all(|o| o.paused));

        // Asked for: the publisher is asked for a key frame (the unit
        // publisher has no stream to ask, so none goes out), and the key
        // frame packet passes the gate.
        router
            .peer_mut(&pid("sub"))
            .expect("the subscriber")
            .set_video_wants(&[want("pub", VideoQuality::Middle)]);
        assert_eq!(drive_video_wants(&mut router, t0), 0);
        let outcome = fanout_packets_into(&mut router, &pending, None);
        assert_eq!((outcome.attempted(), outcome.no_writer), (1, 1));
    }

    #[test]
    fn a_subscribers_keyframe_request_on_a_paused_track_is_not_relayed() {
        let sub = three_track_subscriber("pub");
        let local = match sub.tracks_out[2].state {
            TrackOutState::Open(mid) => mid,
            other => panic!("the middle track is open, got {other:?}"),
        };
        assert_eq!(sub.tracks_out[2].role, TrackRole::Middle);
        // The subscriber's own keyframe request on the paused middle track
        // is not relayed; one on the flowing main track is.
        assert!(sub.downstream_paused(local));
        let main_local = match sub.tracks_out[0].state {
            TrackOutState::Open(mid) => mid,
            other => panic!("the main track is open, got {other:?}"),
        };
        assert!(!sub.downstream_paused(main_local));
    }

    // ============================================================
    // ADR-0070 steps 3 and 4
    // ============================================================

    #[test]
    fn loss_protection_is_on_by_default_and_off_is_nothing() {
        let on = LossProtection::default();
        assert!(on.light_duplicate && on.playout_delay);
        assert_eq!(on.light_nack_retry, Some(LIGHT_NACK_RETRY));
        assert_eq!(LIGHT_NACK_RETRY, Duration::from_millis(20));
        assert_eq!(LIGHT_DUPLICATE_DELAY, Duration::from_millis(30));
        let off = LossProtection::off();
        assert!(!off.light_duplicate && !off.playout_delay);
        assert_eq!(off.light_nack_retry, None);
        assert_eq!(MediaTaskOptions::default().protection, on);
    }

    #[test]
    fn the_router_hands_its_protection_to_the_peers_it_creates() {
        let now = Instant::now();
        let mut router = MediaRouter::new();
        assert_eq!(
            router.peer_or_insert(pid("before"), now).protection(),
            LossProtection::default()
        );
        router.set_protection(LossProtection::off());
        assert_eq!(
            router.peer_or_insert(pid("after"), now).protection(),
            LossProtection::off()
        );
    }

    #[test]
    fn a_second_copy_is_handed_out_once_when_due_and_the_queue_is_bounded() {
        let t0 = Instant::now();
        let mid = Mid::new();
        let mut queue = LightDuplicates::default();
        assert_eq!(queue.next_due(), None);
        assert!(queue.push(t0 + LIGHT_DUPLICATE_DELAY, mid, 7));
        assert!(queue.push(t0 + LIGHT_DUPLICATE_DELAY * 2, mid, 8));
        assert_eq!(queue.next_due(), Some(t0 + LIGHT_DUPLICATE_DELAY));
        assert_eq!(queue.pop_due(t0), None, "not before it is due");
        assert_eq!(queue.pop_due(t0 + Duration::from_millis(29)), None);
        assert_eq!(queue.pop_due(t0 + LIGHT_DUPLICATE_DELAY), Some((mid, 7)));
        assert_eq!(
            queue.pop_due(t0 + LIGHT_DUPLICATE_DELAY),
            None,
            "once, and the next is not due yet"
        );
        assert_eq!(queue.pop_due(t0 + Duration::from_secs(1)), Some((mid, 8)));
        assert_eq!(queue.pop_due(t0 + Duration::from_secs(1)), None);

        // The cap: one more than fits is dropped, and nothing else is.
        for seq in 0..MAX_LIGHT_DUPLICATES as u64 {
            assert!(queue.push(t0, mid, seq));
        }
        assert!(!queue.push(t0, mid, 999_999), "dropped when full");
        assert_eq!(queue.len(), MAX_LIGHT_DUPLICATES);
        assert_eq!(queue.pop_due(t0), Some((mid, 0)));
        assert!(queue.push(t0, mid, 1_000_000), "room again");
    }

    #[test]
    fn a_peer_without_a_transport_queues_no_second_copy_and_sends_none() {
        // A unit peer has no transport: a light packet is `NoWriter`, so
        // nothing was sent and there is nothing to send twice.
        let t0 = Instant::now();
        let source = pid("pub");
        let mid = Mid::new();
        let mut sub = MediaPeer::new(pid("sub"), t0);
        sub.observe_remote_track_in(
            source.clone(),
            TrackIn {
                mid,
                kind: MediaKind::Video,
                simulcast: false,
                light: true,
                middle: false,
            },
        );
        open_all(&mut sub);
        let packet = RelayPacket {
            mid,
            ..synthetic_video_packet()
        };
        assert_eq!(
            sub.forward_light_track(&source, &packet),
            MediaForward::NoWriter
        );
        assert_eq!(sub.next_light_duplicate(), None);
        assert_eq!(sub.send_light_duplicates(t0 + Duration::from_secs(1)), 0);
        assert!(sub.take_duplicates_sent().is_empty());
    }

    /// The SDP of the offer `peer` makes for one video track of `source`.
    fn offer_for_one_video_track(peer: &mut MediaPeer, source: &MediaPeerId) -> String {
        peer.observe_remote_track_in(
            source.clone(),
            TrackIn {
                mid: Mid::new(),
                kind: MediaKind::Video,
                simulcast: false,
                light: false,
                middle: false,
            },
        );
        peer.negotiate_if_needed()
            .expect("an offer")
            .expect("one track to open")
    }

    #[test]
    fn the_playout_delay_extension_is_offered_only_when_switched_on() {
        const URI: &str = "http://www.webrtc.org/experiments/rtp-hdrext/playout-delay";
        let now = Instant::now();
        let mut on = MediaPeer::new(pid("on"), now);
        let sdp = offer_for_one_video_track(&mut on, &pid("pub"));
        assert!(
            sdp.contains(&format!("a=extmap:{PLAYOUT_DELAY_EXTENSION_ID} {URI}")),
            "offered under libwebrtc's own id: {}",
            extmap_summary(&sdp)
        );
        let mut off = MediaPeer::with_protection(
            pid("off"),
            now,
            None,
            false,
            BitrateControl::Client,
            LossProtection::off(),
        );
        let sdp = offer_for_one_video_track(&mut off, &pid("pub"));
        assert!(!sdp.contains(URI), "{}", extmap_summary(&sdp));
        // Nothing negotiated yet on either: an offer is not an answer.
        assert!(!playout_delay_negotiated(&on.rtc, Mid::new()));
    }

    #[test]
    fn a_pair_exists_per_publisher_with_video_and_follows_both_burst_sources() {
        let t0 = Instant::now();
        let (alice, bob) = (pid("alice"), pid("bob"));
        let mut sub = MediaPeer::new(pid("sub"), t0);
        assert!(sub.update_playout_delays(t0, true, &[]).is_empty());
        assert_eq!(sub.playout_delay_ms(&alice), None, "no track, no pair");

        open_video_track(&mut sub, &alice, Mid::new());
        open_video_track(&mut sub, &bob, Mid::new());
        // An audio track makes no pair.
        sub.observe_remote_track_in(
            pid("carol"),
            TrackIn {
                mid: Mid::new(),
                kind: MediaKind::Audio,
                simulcast: false,
                light: false,
                middle: false,
            },
        );
        assert!(sub.update_playout_delays(t0, false, &[]).is_empty());
        assert_eq!(sub.playout_delay_ms(&alice), Some(0));
        assert_eq!(sub.playout_delay_ms(&bob), Some(0));
        assert_eq!(sub.playout_delay_ms(&pid("carol")), None);

        // A burst on Alice's uplink raises Alice's pair alone.
        let changes = sub.update_playout_delays(t0, false, std::slice::from_ref(&alice));
        assert_eq!(
            changes,
            vec![(alice.clone(), 0, 300, playout_delay::Reason::UplinkBurst)]
        );
        assert_eq!(sub.playout_delay_ms(&alice), Some(300));
        assert_eq!(sub.playout_delay_ms(&bob), Some(0));
        // Still in the burst: nothing changes, nothing is reported.
        assert!(sub
            .update_playout_delays(
                t0 + Duration::from_secs(1),
                false,
                std::slice::from_ref(&alice)
            )
            .is_empty());

        // A burst on the subscriber's own downlink raises every pair.
        let t1 = t0 + Duration::from_secs(10);
        let changes = sub.update_playout_delays(t1, true, &[]);
        assert_eq!(
            changes,
            vec![(bob.clone(), 0, 300, playout_delay::Reason::DownlinkBurst)]
        );

        // Quiet: both come down in steps, each change reported once.
        let t2 = t1 + playout_delay::QUIET_BEFORE_LOWERING;
        let changes = sub.update_playout_delays(t2, false, &[]);
        assert_eq!(changes.len(), 2);
        assert!(changes
            .iter()
            .all(|c| c.1 == 300 && c.2 == 200 && c.3 == playout_delay::Reason::Quiet));
        assert!(sub.update_playout_delays(t2, false, &[]).is_empty());

        // The publisher's last video track goes: so does the pair.
        sub.tracks_out.retain(|t| t.source_peer != bob);
        let _ = sub.update_playout_delays(t2, false, &[]);
        assert_eq!(sub.playout_delay_ms(&bob), None);
        assert_eq!(sub.playout_delay_ms(&alice), Some(200));
    }

    #[test]
    fn switched_off_nothing_is_detected_and_no_pair_is_kept() {
        let t0 = Instant::now();
        let mut peer = MediaPeer::with_protection(
            pid("p"),
            t0,
            None,
            false,
            BitrateControl::Client,
            LossProtection::off(),
        );
        open_video_track(&mut peer, &pid("pub"), Mid::new());
        // A stream with a hole that grows old.
        for seq in [10u64, 12, 13] {
            peer.observe_uplink_burst(&RelayPacket {
                seq_no: seq.into(),
                arrival: t0,
                ..synthetic_video_packet()
            });
        }
        let later = t0 + Duration::from_secs(1);
        assert!(!peer.poll_uplink_burst(later));
        assert!(!peer.poll_downlink_burst(later));
        assert!(peer
            .update_playout_delays(later, true, &[pid("pub")])
            .is_empty());
        assert_eq!(peer.playout_delay_ms(&pid("pub")), None);
    }

    #[test]
    fn a_publishers_old_hole_is_an_uplink_burst_and_audio_is_not_counted() {
        let t0 = Instant::now();
        let mut publisher = MediaPeer::new(pid("pub"), t0);
        // Audio with a hole: not video, not counted.
        for seq in [10u64, 12] {
            publisher.observe_uplink_burst(&RelayPacket {
                seq_no: seq.into(),
                arrival: t0,
                ..synthetic_packet()
            });
        }
        assert!(!publisher.poll_uplink_burst(t0 + Duration::from_millis(500)));
        // Video with a hole.
        for seq in [10u64, 12] {
            publisher.observe_uplink_burst(&RelayPacket {
                seq_no: seq.into(),
                arrival: t0,
                ..synthetic_video_packet()
            });
        }
        assert!(!publisher.poll_uplink_burst(t0 + Duration::from_millis(100)));
        assert!(publisher.poll_uplink_burst(t0 + Duration::from_millis(200)));
        // The retransmission closes it.
        publisher.observe_uplink_burst(&RelayPacket {
            seq_no: 11u64.into(),
            arrival: t0 + Duration::from_millis(210),
            ..synthetic_video_packet()
        });
        assert!(!publisher.poll_uplink_burst(t0 + Duration::from_millis(220)));
    }

    #[test]
    fn fec_packets_in_sequence_are_not_holes_for_the_uplink_burst_detector() {
        let t0 = Instant::now();
        let mut publisher = MediaPeer::new(pid("pub"), t0);
        // Media with an FEC packet in every third slot, as a libwebrtc
        // sender numbers them: nothing is missing.
        for i in 0..300u64 {
            let red = (i % 3 == 2).then_some(RedBody::Fec);
            publisher.observe_uplink_burst(&RelayPacket {
                seq_no: (100 + i).into(),
                arrival: t0 + Duration::from_millis(4 * i),
                red,
                ..synthetic_video_packet()
            });
        }
        for ms in (0..3000).step_by(50) {
            assert!(
                !publisher.poll_uplink_burst(t0 + Duration::from_millis(ms)),
                "in a burst at {ms} ms with nothing lost"
            );
        }
    }

    // ---- A key frame at the end of an unrepaired burst ----

    /// A publisher in `call` with a video receive stream on each of
    /// `mids`, so a key frame request has somewhere to go.
    fn publisher_with_video(name: &str, call: &str, mids: &[&str]) -> MediaPeer {
        let mut publisher = peer_in_call(name, call);
        for (i, mid) in mids.iter().enumerate() {
            let mid = Mid::from(*mid);
            let ssrc: Ssrc = (0x1000 + u32::try_from(i).expect("fits")).into();
            let mut api = publisher.rtc.direct_api();
            api.declare_media(mid, MediaKind::Video);
            api.expect_stream_rx(ssrc, None, mid, None);
        }
        publisher
    }

    /// One second on `mid` in which only every fifth packet arrives,
    /// 4 ms apart from `t0`, then the stream in sequence again for
    /// `after` packets. Returns when it was in sequence again.
    fn burst_then_in_sequence(
        publisher: &mut MediaPeer,
        mid: &str,
        t0: Instant,
        after: u64,
    ) -> Instant {
        for i in 0..250u64 {
            if i % 5 != 0 {
                continue;
            }
            publisher.observe_burst_end(&RelayPacket {
                arrival: t0 + Duration::from_millis(4 * i),
                ..video_packet_on(mid, 1000 + i, false)
            });
        }
        let resumed = t0 + Duration::from_secs(1);
        for i in 0..after {
            publisher.observe_burst_end(&RelayPacket {
                arrival: resumed + Duration::from_millis(4 * i),
                ..video_packet_on(mid, 1250 + i, false)
            });
        }
        resumed
    }

    /// Run [`drive_burst_keyframes`] every 10 ms over `[from, until]`.
    fn drive_bursts_over(
        router: &mut MediaRouter,
        from: Instant,
        until: Instant,
    ) -> Vec<(Duration, Mid, BurstKeyframe)> {
        let mut out = Vec::new();
        let mut at = from;
        while at <= until {
            for (_, mid, _, outcome) in drive_burst_keyframes(router, at) {
                out.push((at.duration_since(from), mid, outcome));
            }
            let _ = send_owed_keyframe_requests(router, at);
            at += Duration::from_millis(10);
        }
        out
    }

    /// One packet of a publisher's stream as a libwebrtc sender with ULPFEC
    /// numbers it: frame `seq / 4` is two media packets, then two FEC
    /// packets that protect them. `described`: the media packets carry
    /// the generic frame descriptor. An FEC packet carries a real FEC
    /// header (short mask, the frame's two packets, no protected bytes)
    /// after its RED header byte.
    fn fec_stream_packet(seq: u64, described: bool, arrival: Instant) -> RelayPacket {
        let frame = seq / 4;
        let base = RelayPacket {
            arrival,
            ..video_packet_on(MAIN_MID, seq, false)
        };
        if seq % 4 >= 2 {
            let sn_base = u16::try_from((4 * frame) & 0xffff).expect("fits");
            let mut payload = vec![118u8, 0, 0];
            payload.extend_from_slice(&sn_base.to_be_bytes());
            payload.extend_from_slice(&[0; 8]);
            payload.extend_from_slice(&[0xc0, 0x00]);
            return RelayPacket {
                red: Some(RedBody::Fec),
                payload,
                ..base
            };
        }
        let first = seq % 4 == 0;
        let descriptor = described.then(|| {
            let id = u16::try_from(frame & 0xffff).expect("fits").to_le_bytes();
            PacketDescriptor(if first {
                vec![0xb8, 0x01, id[0], id[1], 0x01]
            } else {
                vec![0x70]
            })
        });
        RelayPacket {
            blind: !described,
            descriptor,
            ..base
        }
    }

    /// The measured case of 2026-10-11 through the peer: half the stream
    /// FEC, a burst that loses 21 packets (402..=422), of which the media
    /// packets come again but for `never`. Returns every burst judged.
    fn fec_stream_burst(described: bool, never: &[u64]) -> Vec<BurstEnd<VideoStream>> {
        let t0 = Instant::now();
        let mut publisher = publisher_with_video("pub", "call", &[MAIN_MID]);
        let mut judged = Vec::new();
        let mut at = t0;
        for seq in 0..480u64 {
            at = t0 + Duration::from_millis(4 * seq);
            if !(402..423).contains(&seq) {
                publisher.observe_burst_end(&fec_stream_packet(seq, described, at));
            }
            if seq == 437 {
                // The retransmissions that came: media only.
                for late in (402..423u64).filter(|s| s % 4 < 2 && !never.contains(s)) {
                    publisher.observe_burst_end(&fec_stream_packet(late, described, at));
                }
            }
            judged.extend(publisher.poll_burst_ends(at));
        }
        // The loop runs every pass: polled as time goes by, not once.
        for ms in (10..=1000u64).step_by(10) {
            judged.extend(publisher.poll_burst_ends(at + Duration::from_millis(ms)));
        }
        judged
    }

    #[test]
    fn media_left_missing_on_a_stream_that_is_half_fec_is_seen_through_masks_and_descriptors() {
        use crate::burst_keyframe::Why;
        // The end of frame 101, the begin of frame 102, the end of frame
        // 105 never come; eleven FEC packets do not either.
        let never = [405, 408, 421];
        // FEC masks alone: the FEC packet 423 arrives and names 420 and
        // 421; 421 is still missing when the burst is judged.
        let judged = fec_stream_burst(false, &never);
        assert_eq!(judged.len(), 1, "{judged:?}");
        assert_eq!((judged[0].lost, judged[0].unrepaired), (21, 14));
        assert_eq!(judged[0].why, Why::MediaMissing);
        assert_eq!((judged[0].media, judged[0].not_media), (1, 0));
        // By number alone this burst asked for nothing: 14 - 2 is not
        // more than 1.5 * 0.5 * 21. With 421 back, the masks that arrived
        // name nothing missing and it still does not.
        let judged = fec_stream_burst(false, &[405, 408]);
        assert_eq!(judged.len(), 1, "{judged:?}");
        assert_eq!(judged[0].why, Why::WithinFecShare);
        assert!(!judged[0].asks());
        // With the frame descriptors all three are found, and six of the
        // lost FEC packets are known for what they are.
        let judged = fec_stream_burst(true, &never);
        assert_eq!(judged.len(), 1, "{judged:?}");
        assert_eq!(judged[0].why, Why::MediaMissing);
        assert_eq!((judged[0].media, judged[0].not_media), (3, 6));
        // Every media packet back: the eleven missing are all known not
        // to be media.
        let judged = fec_stream_burst(true, &[]);
        assert_eq!(judged.len(), 1, "{judged:?}");
        assert_eq!(judged[0].why, Why::NotMedia);
        assert_eq!((judged[0].media, judged[0].not_media), (0, 11));
    }

    #[test]
    fn a_burst_that_left_only_packets_nobody_needs_asks_the_publisher_for_nothing() {
        let t0 = Instant::now();
        let mut router = MediaRouter::new();
        let mut publisher = publisher_with_video("pub", "call", &[MAIN_MID]);
        let mut sub = peer_in_call("sub", "call");
        open_video_track(&mut sub, &pid("pub"), Mid::from(MAIN_MID));
        // Every FEC packet of thirty frames lost, no media packet: sixty
        // sequence numbers that stay missing.
        for seq in 0..800u64 {
            if (400..520).contains(&seq) && seq % 4 >= 2 {
                continue;
            }
            publisher.observe_burst_end(&fec_stream_packet(
                seq,
                true,
                t0 + Duration::from_millis(4 * seq),
            ));
        }
        assert!(publisher.burst_ends.in_burst((Mid::from(MAIN_MID), None)));
        router.add_peer(publisher);
        router.add_peer(sub);
        let from = t0 + Duration::from_millis(4 * 520);
        let asked = drive_bursts_over(&mut router, from, from + Duration::from_secs(5));
        assert!(asked.is_empty(), "{asked:?}");
        let publisher = find_peer(&router, "pub");
        assert!(!publisher.burst_ends.in_burst((Mid::from(MAIN_MID), None)));
        assert_eq!(
            publisher.unanswered_keyframe_requests(Mid::from(MAIN_MID), None),
            0
        );
    }

    #[test]
    fn an_unrepaired_burst_asks_the_publisher_once_after_the_stated_time() {
        let t0 = Instant::now();
        let mut router = MediaRouter::new();
        let mut publisher = publisher_with_video("pub", "call", &[MAIN_MID]);
        let mut sub = peer_in_call("sub", "call");
        open_video_track(&mut sub, &pid("pub"), Mid::from(MAIN_MID));
        let resumed = burst_then_in_sequence(&mut publisher, MAIN_MID, t0, 40);
        router.add_peer(publisher);
        router.add_peer(sub);

        let asked = drive_bursts_over(&mut router, resumed, resumed + Duration::from_secs(4));
        assert_eq!(asked.len(), 1, "one request per burst: {asked:?}");
        let (after, mid, outcome) = asked[0];
        assert_eq!(mid, Mid::from(MAIN_MID));
        assert_eq!(outcome, BurstKeyframe::Asked);
        assert!(
            after >= crate::burst_keyframe::BURST_END_QUIET
                && after <= crate::burst_keyframe::BURST_END_QUIET + Duration::from_millis(20),
            "asked {after:?} after the stream was in sequence again"
        );
        assert_eq!(
            find_peer(&router, "pub").unanswered_keyframe_requests(Mid::from(MAIN_MID), None),
            1,
            "through the one gate, and counted there"
        );
    }

    #[test]
    fn a_burst_that_retransmission_repaired_asks_the_publisher_for_nothing() {
        let t0 = Instant::now();
        let mut router = MediaRouter::new();
        let mut publisher = publisher_with_video("pub", "call", &[MAIN_MID]);
        let mut sub = peer_in_call("sub", "call");
        open_video_track(&mut sub, &pid("pub"), Mid::from(MAIN_MID));
        let resumed = burst_then_in_sequence(&mut publisher, MAIN_MID, t0, 40);
        // Every missing packet arrives 60 ms after the burst.
        for i in (0..250u64).filter(|i| i % 5 != 0) {
            publisher.observe_burst_end(&RelayPacket {
                arrival: resumed + Duration::from_millis(60),
                ..video_packet_on(MAIN_MID, 1000 + i, false)
            });
        }
        router.add_peer(publisher);
        router.add_peer(sub);
        assert!(
            drive_bursts_over(&mut router, resumed, resumed + Duration::from_secs(4)).is_empty()
        );
        assert_eq!(
            find_peer(&router, "pub").unanswered_keyframe_requests(Mid::from(MAIN_MID), None),
            0
        );
    }

    #[test]
    fn a_stream_no_subscriber_is_being_sent_is_not_asked_for_a_key_frame() {
        let t0 = Instant::now();
        let source = pid("pub");
        // Nobody else in the call; a subscriber of another publisher; a
        // subscriber whose track is still negotiating; one that does not
        // show the main picture.
        let nobody: Option<Box<MediaPeer>> = None;
        let mut elsewhere = peer_in_call("sub", "call");
        open_video_track(&mut elsewhere, &pid("other"), Mid::from(MAIN_MID));
        let mut negotiating = peer_in_call("sub", "call");
        open_video_track(&mut negotiating, &source, Mid::from(MAIN_MID));
        negotiating.tracks_out[0].state = TrackOutState::Negotiating(Mid::new());
        let mut paused = three_track_subscriber("pub");
        paused.set_video_wants(&[want("pub", VideoQuality::Light)]);
        assert!(paused.track_paused(&source, Mid::from(MAIN_MID), TrackRole::Main));
        assert!(!paused.is_sent_stream(&source, Mid::from(MAIN_MID), None));
        assert!(paused.is_sent_stream(&source, Mid::from(LIGHT_MID), None));

        for sub in [
            nobody,
            Some(Box::new(elsewhere)),
            Some(Box::new(negotiating)),
            Some(Box::new(paused)),
        ] {
            let mut router = MediaRouter::new();
            let mut publisher = publisher_with_video("pub", "call", &[MAIN_MID]);
            let resumed = burst_then_in_sequence(&mut publisher, MAIN_MID, t0, 40);
            router.add_peer(publisher);
            if let Some(sub) = sub {
                router.add_peer(*sub);
            }
            let ended = drive_bursts_over(&mut router, resumed, resumed + Duration::from_secs(4));
            assert_eq!(ended.len(), 1, "{ended:?}");
            assert_eq!(ended[0].2, BurstKeyframe::NobodyWatching);
            let publisher = find_peer(&router, "pub");
            assert_eq!(
                publisher.unanswered_keyframe_requests(Mid::from(MAIN_MID), None),
                0,
                "nothing went through the gate"
            );
            assert!(!publisher.keyframe_request_owed(Mid::from(MAIN_MID), None));
        }
    }

    #[test]
    fn a_burst_end_request_respects_the_rate_limit_and_the_unanswered_bound() {
        let t0 = Instant::now();
        let main = Mid::from(MAIN_MID);

        // Asked 100 ms before the burst ended: owed, sent when the window
        // ends, and once.
        let mut router = MediaRouter::new();
        let mut publisher = publisher_with_video("pub", "call", &[MAIN_MID]);
        let mut sub = peer_in_call("sub", "call");
        open_video_track(&mut sub, &pid("pub"), main);
        let resumed = burst_then_in_sequence(&mut publisher, MAIN_MID, t0, 40);
        let asked_before = resumed + Duration::from_millis(20);
        assert!(publisher.request_keyframe_limited(main, None, asked_before));
        router.add_peer(publisher);
        router.add_peer(sub);
        let ended = drive_bursts_over(&mut router, resumed, resumed + Duration::from_secs(4));
        assert_eq!(ended.len(), 1, "{ended:?}");
        assert_eq!(ended[0].2, BurstKeyframe::Owed);
        let publisher = find_peer(&router, "pub");
        assert!(!publisher.keyframe_request_owed(main, None), "sent since");
        assert_eq!(
            publisher.unanswered_keyframe_requests(main, None),
            2,
            "the earlier request and the owed one, no more"
        );
        assert_eq!(
            publisher.last_keyframe_request.get(&(main, None)).copied(),
            Some(asked_before + KEYFRAME_REQUEST_MIN_INTERVAL),
            "at the end of the window, not before"
        );

        // A track that has been asked and asked and shows no key frame:
        // nothing is sent and nothing is owed.
        let mut router = MediaRouter::new();
        let mut publisher = publisher_with_video("pub", "call", &[MAIN_MID]);
        let mut sub = peer_in_call("sub", "call");
        open_video_track(&mut sub, &pid("pub"), main);
        let resumed = burst_then_in_sequence(&mut publisher, MAIN_MID, t0, 40);
        publisher
            .unanswered_keyframe_requests
            .insert((main, None), KEYFRAME_UNANSWERED_MAX);
        publisher
            .last_keyframe_request
            .insert((main, None), resumed);
        router.add_peer(publisher);
        router.add_peer(sub);
        let ended = drive_bursts_over(&mut router, resumed, resumed + Duration::from_secs(4));
        assert_eq!(ended.len(), 1, "{ended:?}");
        assert_eq!(ended[0].2, BurstKeyframe::NotAsked);
        let publisher = find_peer(&router, "pub");
        assert_eq!(
            publisher.unanswered_keyframe_requests(main, None),
            KEYFRAME_UNANSWERED_MAX
        );
        assert_eq!(
            publisher.last_keyframe_request.get(&(main, None)).copied(),
            Some(resumed)
        );
    }

    #[test]
    fn each_stream_of_a_publisher_is_asked_for_on_its_own() {
        let t0 = Instant::now();
        let mut router = MediaRouter::new();
        let mut publisher = publisher_with_video("pub", "call", &[MAIN_MID, LIGHT_MID, MIDDLE_MID]);
        // The light picture loses a second and repairs nothing; the middle
        // one runs clean; the main one loses the same second, and its
        // subscriber does not show it.
        let resumed = burst_then_in_sequence(&mut publisher, LIGHT_MID, t0, 40);
        let _ = burst_then_in_sequence(&mut publisher, MAIN_MID, t0, 40);
        for i in 0..1500u64 {
            publisher.observe_burst_end(&RelayPacket {
                arrival: t0 + Duration::from_millis(4 * i),
                ..video_packet_on(MIDDLE_MID, 7000 + i, false)
            });
        }
        let mut sub = three_track_subscriber("pub");
        sub.set_video_wants(&[want("pub", VideoQuality::Middle)]);
        router.add_peer(publisher);
        router.add_peer(sub);
        let mut ended = drive_bursts_over(&mut router, resumed, resumed + Duration::from_secs(4));
        ended.sort_by_key(|(_, mid, _)| mid.to_string());
        let ended: Vec<(Mid, BurstKeyframe)> = ended.into_iter().map(|(_, m, o)| (m, o)).collect();
        assert_eq!(
            ended,
            vec![
                (Mid::from(MAIN_MID), BurstKeyframe::NobodyWatching),
                (Mid::from(LIGHT_MID), BurstKeyframe::Asked),
            ]
        );
        let publisher = find_peer(&router, "pub");
        assert_eq!(
            publisher.unanswered_keyframe_requests(Mid::from(MIDDLE_MID), None),
            0
        );
    }

    #[test]
    fn a_key_frame_that_arrived_since_and_the_switch_off_ask_for_nothing() {
        let t0 = Instant::now();
        // A key frame of the publisher's own starts arriving 40 ms after
        // the burst: what was missing before it is missing for nobody.
        let mut router = MediaRouter::new();
        let mut publisher = publisher_with_video("pub", "call", &[MAIN_MID]);
        let mut sub = peer_in_call("sub", "call");
        open_video_track(&mut sub, &pid("pub"), Mid::from(MAIN_MID));
        let resumed = burst_then_in_sequence(&mut publisher, MAIN_MID, t0, 10);
        for i in 0..1000u64 {
            publisher.observe_burst_end(&RelayPacket {
                arrival: resumed + Duration::from_millis(40 + 4 * i),
                ..video_packet_on(MAIN_MID, 1260 + i, i == 0)
            });
        }
        router.add_peer(publisher);
        router.add_peer(sub);
        assert!(
            drive_bursts_over(&mut router, resumed, resumed + Duration::from_secs(4)).is_empty()
        );

        // Switched off: nothing is kept and nothing is asked.
        let mut router = MediaRouter::new();
        let mut publisher = MediaPeer::with_protection(
            pid("pub"),
            t0,
            None,
            false,
            BitrateControl::Client,
            LossProtection {
                burst_keyframe: false,
                ..LossProtection::default()
            },
        );
        publisher.set_call_id("call".to_owned());
        let mut sub = peer_in_call("sub", "call");
        open_video_track(&mut sub, &pid("pub"), Mid::from(MAIN_MID));
        let resumed = burst_then_in_sequence(&mut publisher, MAIN_MID, t0, 40);
        assert_eq!(publisher.burst_ends.open_holes(), 0);
        router.add_peer(publisher);
        router.add_peer(sub);
        assert!(
            drive_bursts_over(&mut router, resumed, resumed + Duration::from_secs(4)).is_empty()
        );
    }

    #[test]
    fn a_publishers_burst_raises_the_delay_of_its_own_calls_subscribers_only() {
        let t0 = Instant::now();
        let mut router = MediaRouter::new();
        let mut publisher = peer_in_call("pub", "call-a");
        let mut same = peer_in_call("same", "call-a");
        let mut other = peer_in_call("other", "call-b");
        // The subscriber of another call has a video track of a publisher
        // of its own call.
        open_video_track(&mut same, &pid("pub"), Mid::new());
        open_video_track(&mut other, &pid("elsewhere"), Mid::new());
        for seq in [10u64, 12] {
            publisher.observe_uplink_burst(&RelayPacket {
                seq_no: seq.into(),
                arrival: t0,
                ..synthetic_video_packet()
            });
        }
        router.add_peer(publisher);
        router.add_peer(same);
        router.add_peer(other);

        assert_eq!(drive_playout_delay(&mut router, t0), 0);
        let later = t0 + Duration::from_millis(300);
        assert_eq!(drive_playout_delay(&mut router, later), 1);
        assert_eq!(
            find_peer(&router, "same").playout_delay_ms(&pid("pub")),
            Some(300)
        );
        assert_eq!(
            find_peer(&router, "other").playout_delay_ms(&pid("elsewhere")),
            Some(0),
            "a burst in one call changes nothing in another"
        );
        // Filed for the fanout line of the pair.
        let mut ledger = FanoutLedger::new(Duration::from_secs(1), t0);
        let mid = find_peer(&router, "same").tracks_out()[0].source_mid;
        ledger.record(
            &pid("same"),
            &pid("pub"),
            mid,
            TrackRole::Main,
            &MediaForward::Sent,
        );
        ledger.record_duplicate_sent(&pid("same"), &pid("pub"), mid, TrackRole::Main);
        note_playout_delays(&router, &mut ledger);
        let line = ledger
            .per_target
            .get(&(pid("same"), pid("pub"), mid, TrackRole::Main))
            .expect("the line");
        assert_eq!(line.playout_delay_ms, Some(300));
        assert_eq!(line.duplicates, 1);
        assert_eq!(
            line.attempted(),
            1,
            "a second copy is not a forward attempt"
        );
    }

    /// A [`LayerBitrateMeter`] whose completed-window rate is a fixed `bps`.
    fn meter_with_bps(bps: u64) -> LayerBitrateMeter {
        LayerBitrateMeter {
            window: LayerBitrateMeter::WINDOW,
            window_start: Instant::now(),
            bytes: 0,
            last_bps: bps,
            before_bps: bps,
        }
    }

    // ---- ADR-0070 step 6: downlink control ----

    const DL_MAIN_BPS: u64 = 2_000_000;
    const DL_MIDDLE_BPS: u64 = 500_000;
    const DL_LIGHT_BPS: u64 = 150_000;

    /// A peer under server-side bitrate control with downlink control on.
    fn downlink_peer(name: &str, call: &str) -> MediaPeer {
        let mut peer = MediaPeer::with_downlink_control(
            pid(name),
            Instant::now(),
            None,
            false,
            BitrateControl::Server(EstimatorConfig::default()),
            LossProtection::default(),
            true,
        );
        peer.set_call_id(call.to_owned());
        peer
    }

    /// A publisher of three declared video tracks whose arrival rates
    /// are the three constants above.
    fn downlink_publisher(name: &str, call: &str) -> MediaPeer {
        let mut peer = downlink_peer(name, call);
        peer.light_mid = Some(Mid::from(LIGHT_MID));
        peer.middle_mid = Some(Mid::from(MIDDLE_MID));
        for (mid, bps) in [
            (MAIN_MID, DL_MAIN_BPS),
            (LIGHT_MID, DL_LIGHT_BPS),
            (MIDDLE_MID, DL_MIDDLE_BPS),
        ] {
            peer.register_local_track_in(Mid::from(mid), MediaKind::Video);
            peer.track_meters
                .insert(Mid::from(mid), meter_with_bps(bps));
        }
        peer
    }

    /// What the allocation is told `publisher` costs a subscriber that
    /// has its three tracks and asks for `wanted`.
    fn downlink_input(
        publisher: &str,
        wanted: VideoQuality,
    ) -> downlink_allocation::Publisher<MediaPeerId> {
        downlink_allocation::Publisher {
            publisher: pid(publisher),
            wanted,
            main_bps: Some(DL_MAIN_BPS),
            middle_bps: Some(DL_MIDDLE_BPS),
            light_bps: Some(DL_LIGHT_BPS),
            audio_bps: 0,
        }
    }

    /// The SDP sections of `sdp`, one per media line.
    fn media_sections(sdp: &str) -> Vec<String> {
        sdp.split("\r\nm=")
            .skip(1)
            .map(|section| format!("m={section}"))
            .collect()
    }

    /// Downlink control negotiates transport-wide congestion control per
    /// direction: on the lines the SFU offers, and not on the lines a
    /// peer offers, which stay as server-side bitrate control has them.
    #[test]
    fn downlink_control_negotiates_transport_cc_only_on_the_lines_the_sfu_offers() {
        let now = Instant::now();
        let mut client = Rtc::builder().build(now);
        let mut change = client.sdp_api();
        change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
        change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
        let (offer, _pending) = change.apply().expect("an offer");
        let offer = offer.to_sdp_string();
        assert!(
            offer.contains("transport-cc") && offer.contains("transport-wide-cc"),
            "the client offers it, as a browser does"
        );

        let mut peer = downlink_peer("p", "call");
        assert!(peer.downlink_control());
        let answer = peer.accept_offer(&offer).expect("the SFU answers");
        assert!(
            !answer.contains("transport-cc") && !answer.contains("transport-wide-cc"),
            "the answer to the peer's own offer names none:\n{answer}"
        );
        assert!(answer.contains("goog-remb") && answer.contains("abs-send-time"));

        // A downstream track: the SFU's own offer.
        assert!(peer.observe_remote_track_in(pid("pub"), video_track_in(MAIN_MID, TrackRole::Main)));
        let sfu_offer = peer
            .negotiate_if_needed()
            .expect("negotiates")
            .expect("an offer");
        let sections = media_sections(&sfu_offer);
        assert_eq!(sections.len(), 3, "{sfu_offer}");
        for section in &sections[..2] {
            assert!(
                !section.contains("transport-cc") && !section.contains("transport-wide-cc"),
                "a line the peer offered stays without it:\n{section}"
            );
        }
        let sent = &sections[2];
        assert!(sent.contains("a=sendonly") && sent.contains("a=msid:pub "));
        assert!(
            sent.contains("transport-wide-cc") && sent.contains(" transport-cc"),
            "the line the SFU sends on carries the extension and the feedback:\n{sent}"
        );
        assert!(sent.contains("goog-remb") && sent.contains("abs-send-time"));

        // Switched off, and with the client deciding its bitrate, the
        // peer is what it was before step 6.
        let server = BitrateControl::Server(EstimatorConfig::default());
        let mut off = MediaPeer::with_downlink_control(
            pid("off"),
            now,
            None,
            false,
            server,
            LossProtection::default(),
            false,
        );
        assert!(!off.downlink_control());
        let answer = off.accept_offer(&offer).expect("the SFU answers");
        assert!(off.observe_remote_track_in(pid("pub"), video_track_in(MAIN_MID, TrackRole::Main)));
        let sfu_offer = off.negotiate_if_needed().expect("ok").expect("an offer");
        for sdp in [&answer, &sfu_offer] {
            assert!(!sdp.contains("transport-cc") && !sdp.contains("transport-wide-cc"));
        }
        let mut client_mode = MediaPeer::with_downlink_control(
            pid("client"),
            now,
            None,
            false,
            BitrateControl::Client,
            LossProtection::default(),
            true,
        );
        assert!(
            !client_mode.downlink_control(),
            "downlink control needs server-side bitrate control"
        );
        let answer = client_mode.accept_offer(&offer).expect("the SFU answers");
        assert!(answer.contains("transport-cc") && answer.contains("transport-wide-cc"));
        assert!(client_mode.downlink_budget_bps().is_none());
        assert!(client_mode.downlink_snapshot(now).is_none());
        assert_eq!(
            client_mode.run_downlink(&[downlink_input("pub", VideoQuality::Main)], now),
            DownlinkRun::default()
        );
    }

    /// Feedback for `n` packets of 1200 bytes sent 5 ms apart from
    /// `from`, each arriving `delay_ms(i)` after it was sent, on an
    /// arrival clock an hour ahead of the SFU's.
    fn feedback(
        from: Instant,
        n: u64,
        delay_ms: impl Fn(u64) -> Option<u64>,
    ) -> Vec<(Instant, usize, Option<Instant>)> {
        (0..n)
            .map(|i| {
                let sent = from + Duration::from_millis(5 * i);
                let received = delay_ms(i)
                    .map(|d| sent + Duration::from_secs(3600) + Duration::from_millis(d));
                (sent, 1200, received)
            })
            .collect()
    }

    /// Make `sub`'s budget known and equal to `bps`: one acknowledged
    /// packet, and the estimate brought down to the figure.
    fn known_budget(sub: &mut MediaPeer, bps: u64, now: Instant) {
        sub.downlink_feed(&feedback(now, 1, |_| Some(20)), now);
        let downlink = sub.downlink.as_mut().expect("downlink control is on");
        // The path has spoken (until it has, the estimate is its
        // ceiling), without a limit that would bind in these tests.
        downlink.limit_bps = Some(DOWNLINK_CEILING_BPS);
        downlink.estimator.force_estimate(bps);
        // Brought down as the path brings it down.
        downlink.lowered_since_run = true;
    }

    /// Say that `sub`'s path was just seen to carry `bps`.
    fn carried(sub: &mut MediaPeer, bps: u64, now: Instant) {
        sub.downlink.as_mut().expect("on").proven = Some((bps, now));
    }

    /// The budget is unknown until the subscriber's feedback has
    /// acknowledged a packet; then it is the SFU's own estimate.
    #[test]
    fn the_downlink_budget_is_unknown_until_feedback_has_come() {
        let now = Instant::now();
        let mut sub = downlink_peer("sub", "call");
        assert_eq!(sub.downlink_budget_bps(), None);
        // What str0m's own estimator would say is not a budget here.
        sub.note_egress_estimate(&BweKind::Twcc(Bitrate::kbps(900)));
        assert_eq!(sub.downlink_budget_bps(), None);
        // Packets reported missing say nothing either.
        sub.downlink_feed(&feedback(now, 5, |_| None), now);
        assert_eq!(sub.downlink_budget_bps(), None);
        sub.downlink_feed(&feedback(now, 1, |_| Some(20)), now);
        assert_eq!(sub.downlink_budget_bps(), Some(DOWNLINK_CEILING_BPS));
        // Without downlink control there is none, whatever is fed.
        let mut plain = peer_in_call("plain", "call");
        plain.downlink_feed(&feedback(now, 1, |_| Some(20)), now);
        assert_eq!(plain.downlink_budget_bps(), None);
        assert_eq!(plain.pump_downlink(now), 0);
        assert_eq!(plain.next_downlink_tick(now), None);
    }

    /// The estimator fed from feedback follows the rules of the uplink
    /// one: a steady path changes nothing, a growing queue cuts the
    /// estimate to a fraction of what was acknowledged, and the cut is
    /// what the probe and the allocation hear of.
    #[test]
    fn the_downlink_estimate_is_cut_when_the_feedback_shows_a_queue_growing() {
        let t0 = Instant::now();
        let mut sub = downlink_peer("sub", "call");
        // The SFU would like to send 2 Mbit/s: the ceiling is 3.
        let ceiling = 3_000_000;
        sub.downlink
            .as_mut()
            .expect("on")
            .estimator
            .set_ceiling(ceiling);
        // Two seconds at a constant 20 ms: 1200 bytes every 5 ms is
        // 1.92 Mbit/s, the estimate stays at its ceiling.
        sub.downlink_feed(
            &feedback(t0, 400, |_| Some(20)),
            t0 + Duration::from_secs(2),
        );
        assert_eq!(sub.downlink_budget_bps(), Some(ceiling));
        let snap = sub.downlink_snapshot(t0).expect("on");
        assert_eq!(snap.cuts, 0);
        // Then every packet is 3 ms later than the one before: a queue,
        // and it stands. (The feedback comes as it does on a call, a
        // report every 100 ms.)
        let later = t0 + Duration::from_secs(2);
        feed_in_reports(&mut sub, later, 200, |i| Some(20 + 3 * i));
        let budget = sub.downlink_budget_bps().expect("known");
        assert!(
            budget < 1_920_000,
            "cut below what was being acknowledged, got {budget}"
        );
        let downlink = sub.downlink.as_ref().expect("on");
        assert!(downlink.estimator.cuts() >= 1);
        let trouble = downlink.last_trouble.expect("the path has spoken");
        assert!(trouble > later && trouble <= later + Duration::from_secs(1));
        assert!(
            !downlink.probe.can_start(trouble + Duration::from_secs(1)),
            "no probe right after the path has spoken"
        );
    }

    /// Feedback for `n` packets as [`feedback`] makes it, handed over in
    /// reports of twenty packets (100 ms), each at the time its last
    /// packet was sent plus 100 ms.
    fn feed_in_reports(
        sub: &mut MediaPeer,
        from: Instant,
        n: u64,
        delay_ms: impl Fn(u64) -> Option<u64>,
    ) {
        let all = feedback(from, n, delay_ms);
        for (k, report) in all.chunks(20).enumerate() {
            let k = u64::try_from(k).expect("small");
            sub.downlink_feed(report, from + Duration::from_millis(100 * (k + 1)));
        }
    }

    /// The detector set off with no queue standing on the path (jitter
    /// alone, or a queue that drained by itself) costs nothing: the cut
    /// is taken back, counted, and nothing hears of trouble.
    #[test]
    fn a_cut_with_no_standing_queue_is_taken_back() {
        let t0 = Instant::now();
        let mut sub = downlink_peer("sub", "call");
        let ceiling = 3_000_000;
        sub.downlink
            .as_mut()
            .expect("on")
            .estimator
            .set_ceiling(ceiling);
        feed_in_reports(&mut sub, t0, 400, |_| Some(20));
        // A queue grows for 150 ms and is gone again: thirty packets each
        // 3 ms later than the one before, then back to 20 ms.
        let later = t0 + Duration::from_secs(2);
        feed_in_reports(&mut sub, later, 400, |i| {
            Some(if i < 30 { 20 + 3 * i } else { 20 })
        });
        let downlink = sub.downlink.as_ref().expect("on");
        assert_eq!(sub.downlink_budget_bps(), Some(ceiling), "as it was");
        assert_eq!(downlink.last_trouble, None, "nothing heard of trouble");
        assert!(downlink.probe.can_start(later + Duration::from_secs(2)));
        if downlink.estimator.cuts() > 0 {
            assert!(downlink.cut_notes.iter().all(|note| note.taken_back));
        }
        // Jitter: every other packet 8 ms late, for four seconds.
        let jitter = later + Duration::from_secs(2);
        feed_in_reports(&mut sub, jitter, 800, |i| Some(20 + 8 * (i % 2)));
        let downlink = sub.downlink.as_ref().expect("on");
        assert_eq!(sub.downlink_budget_bps(), Some(ceiling));
        assert_eq!(downlink.last_trouble, None);
    }

    /// The records of `seconds` of steady sending at `send_bps` through
    /// a path that carries `path_bps(t)` behind a queue, in reports
    /// every 100 ms. With `clump_at`, nothing is sent for 200 ms from
    /// then and everything withheld goes out at once after it: a
    /// publisher whose frames arrived together. Feeds them to `sub` and
    /// runs nothing else.
    fn feed_through_a_path(
        sub: &mut MediaPeer,
        t0: Instant,
        seconds: u64,
        send_bps: u64,
        path_bps: impl Fn(Duration) -> u64,
        clump_at: Option<Duration>,
    ) {
        const PACKET: usize = 1100;
        let every = Duration::from_micros(PACKET as u64 * 8 * 1_000_000 / send_bps);
        let mut free_at = t0;
        let mut report: Vec<(Instant, usize, Option<Instant>)> = Vec::new();
        let mut next_report = t0 + Duration::from_millis(100);
        let mut due = Duration::ZERO;
        while due < Duration::from_secs(seconds) {
            // A withheld packet is sent when the stall ends.
            let sent = match clump_at {
                Some(at) if due >= at && due < at + Duration::from_millis(200) => {
                    t0 + at + Duration::from_millis(200)
                },
                _ => t0 + due,
            };
            while sent >= next_report {
                let (told, kept): (Vec<_>, Vec<_>) =
                    report.drain(..).partition(|(_, _, arrival)| {
                        arrival.is_some_and(|a| a <= next_report + Duration::from_secs(3600))
                    });
                report = kept;
                sub.downlink_feed(&told, next_report);
                next_report += Duration::from_millis(100);
            }
            let rate = path_bps(sent.duration_since(t0)).max(1);
            let serve = Duration::from_micros(PACKET as u64 * 8 * 1_000_000 / rate);
            free_at = free_at.max(sent) + serve;
            // The arrival clock an hour ahead, as in `feedback`.
            report.push((
                sent,
                PACKET,
                Some(free_at + Duration::from_secs(3600) + Duration::from_millis(20)),
            ));
            due += every;
        }
    }

    /// A clump the SFU forwarded stands in the queue of any path. That
    /// much of a queue is not evidence that the path is short: no cut is
    /// kept and nothing hears of trouble. The same queue with steady
    /// sending (the path itself got narrower) is cut on.
    #[test]
    fn a_queue_the_sfus_own_clump_built_is_not_cut_on() {
        let t0 = Instant::now();
        let estimate = 7_000_000;
        // The clump is 200 ms of this: 160 ms of queue on the path
        // below, which takes more than 400 ms to fall under the depth a
        // cut needs no detector for.
        let sending = 6_000_000;
        let mut sub = downlink_peer("sub", "call");
        known_budget(&mut sub, estimate, t0);
        // The path has said this much of itself and no more.
        let downlink = sub.downlink.as_mut().expect("on");
        downlink.limit_bps = Some(estimate);
        downlink.apply_ceiling();
        feed_through_a_path(
            &mut sub,
            t0,
            12,
            sending,
            |_| 7_500_000,
            Some(Duration::from_secs(6)),
        );
        let downlink = sub.downlink.as_ref().expect("on");
        assert!(
            downlink.cut_notes.iter().all(|note| note.taken_back),
            "{:?}",
            downlink.cut_notes
        );
        assert_eq!(downlink.last_trouble, None, "nothing heard of trouble");
        assert_eq!(sub.downlink_budget_bps(), Some(estimate));

        // The path gets narrower than what is sent: a queue the sending
        // does not explain. That is cut on.
        let mut sub = downlink_peer("sub", "call");
        known_budget(&mut sub, estimate, t0);
        // The path has said this much of itself and no more.
        let downlink = sub.downlink.as_mut().expect("on");
        downlink.limit_bps = Some(estimate);
        downlink.apply_ceiling();
        feed_through_a_path(
            &mut sub,
            t0,
            12,
            sending,
            |at| {
                if at >= Duration::from_secs(6) {
                    4_000_000
                } else {
                    7_500_000
                }
            },
            None,
        );
        let budget = sub.downlink_budget_bps().expect("known");
        let downlink = sub.downlink.as_ref().expect("on");
        assert!(
            budget < sending,
            "cut to {budget}: {:?} queue {:?} excess {:?} since {:?} pending {}",
            downlink.cut_notes,
            downlink.standing_queue(),
            downlink.excess_queue(),
            downlink.queue_since.map(|at| at.duration_since(t0)),
            downlink.cut_pending
        );
        // Once per time the queue has to stand, not at every pass: the
        // estimate is a fraction of what the path delivers, not the
        // floor.
        assert!(budget > 3_000_000, "cut to {budget}");
        assert!(sub.downlink.as_ref().expect("on").last_trouble.is_some());
    }

    /// A delay that steps up and stays, with nothing else wrong, is the
    /// path's own after three seconds: it no longer reads as a queue, so
    /// a later blip of the detector is not confirmed by it. A deep one
    /// is never taken so.
    #[test]
    fn a_delay_that_settles_higher_becomes_the_baseline() {
        let t0 = Instant::now();
        let mut sub = downlink_peer("sub", "call");
        feed_in_reports(&mut sub, t0, 400, |_| Some(20));
        // 30 ms more from here on, steady.
        let stepped = t0 + Duration::from_secs(2);
        feed_in_reports(&mut sub, stepped, 400, |_| Some(50));
        let downlink = sub.downlink.as_ref().expect("on");
        assert!(
            downlink
                .standing_queue()
                .is_some_and(|q| q >= Duration::from_millis(25)),
            "two seconds in it still reads as a queue: {:?}",
            downlink.standing_queue()
        );
        feed_in_reports(&mut sub, stepped + Duration::from_secs(2), 400, |_| {
            Some(50)
        });
        let downlink = sub.downlink.as_ref().expect("on");
        assert_eq!(
            downlink.standing_queue(),
            Some(Duration::ZERO),
            "taken as the path's own"
        );
        assert_eq!(downlink.raw_queue_since, None);

        // A deep one stays a queue however long it stands.
        let mut deep = downlink_peer("deep", "call");
        feed_in_reports(&mut deep, t0, 400, |_| Some(20));
        feed_in_reports(&mut deep, stepped, 1200, |_| Some(120));
        let downlink = deep.downlink.as_ref().expect("on");
        assert!(downlink
            .standing_queue()
            .is_some_and(|q| q >= Duration::from_millis(75)));
    }

    /// What a probe does to the path is the probe's doing: a cut while
    /// it runs or just after it ends the probe and leaves the estimate
    /// as it was before the probe.
    #[test]
    fn a_cut_during_a_probe_fails_the_probe_and_leaves_the_estimate() {
        let t0 = Instant::now();
        let mut sub = downlink_peer("sub", "call");
        add_three_tracks(&mut sub, "pub");
        let input = [downlink_input("pub", VideoQuality::Main)];
        let _ = sub.run_downlink(&input, t0);
        feed_in_reports(&mut sub, t0, 400, |_| Some(20));
        known_budget(&mut sub, DL_LIGHT_BPS, t0 + Duration::from_secs(2));
        sub.downlink
            .as_mut()
            .expect("on")
            .recent
            .push_back((Mid::from("x"), 1, 400));
        let start = t0 + Duration::from_secs(3);
        let run = sub.run_downlink(&input, start);
        assert!(run.probe_started_bps.is_some(), "a probe starts");
        let before = sub.downlink_budget_bps().expect("known");
        // The queue the probe fills: every packet 3 ms later than the
        // one before.
        feed_in_reports(&mut sub, start, 200, |i| Some(20 + 3 * i));
        let downlink = sub.downlink.as_ref().expect("on");
        assert!(!downlink.probe.is_running(), "the probe ended");
        assert_eq!(
            downlink.outcomes.first().and_then(|o| o.failure),
            Some(crate::downlink_probe::ProbeFailure::Trouble)
        );
        assert!(downlink.cut_notes.iter().all(|note| note.taken_back));
        assert!(
            sub.downlink_budget_bps().expect("known") >= before,
            "the estimate is not below what it was before the probe"
        );
        assert_eq!(
            sub.video_quality_given(&pid("pub")),
            VideoQuality::Light,
            "and nothing is given on a failed probe"
        );
    }

    /// Once the path has spoken the estimate never stands above what it
    /// said: what was arriving when it cut the estimate, and just under
    /// the rate of a probe that failed. Only a probe that is carried
    /// raises it.
    #[test]
    fn the_estimate_never_stands_above_what_the_path_has_said() {
        use crate::downlink_probe::ProbeFailure;
        let t0 = Instant::now();
        let mut sub = downlink_peer("sub", "call");
        add_three_tracks(&mut sub, "pub");
        let input = [downlink_input("pub", VideoQuality::Main)];
        let _ = sub.run_downlink(&input, t0);
        // Nothing said yet: the estimate is its ceiling.
        feed_in_reports(&mut sub, t0, 400, |_| Some(20));
        let run = sub.run_downlink(&input, t0 + Duration::from_secs(2));
        let open = run.budget_bps.expect("known");
        assert!(sub.downlink.as_ref().expect("on").limit_bps.is_none());
        // A queue grows and stands: the path cuts the estimate.
        let later = t0 + Duration::from_secs(2);
        feed_in_reports(&mut sub, later, 200, |i| Some(20 + 3 * i));
        let limit = sub
            .downlink
            .as_ref()
            .expect("on")
            .limit_bps
            .expect("the path has spoken");
        assert!(limit < open, "{limit} < {open}");
        // A long quiet time, in which the estimator climbs by rule.
        let quiet = later + Duration::from_secs(1);
        feed_in_reports(&mut sub, quiet, 4000, |_| Some(620));
        let run = sub.run_downlink(&input, quiet + Duration::from_secs(20));
        assert!(run.budget_bps.expect("known") <= limit);

        // A probe that fails puts the limit just under its rate; one that
        // is carried lifts it to that rate and a tenth.
        let outcome = |target_bps: u64, failure: Option<ProbeFailure>| ProbeOutcome {
            target_bps,
            carried_bps: 0,
            padding_bytes: 0,
            held_back_bytes: 0,
            failure,
            next_allowed: quiet,
        };
        let at = quiet + Duration::from_secs(21);
        let failed = limit / 2;
        sub.downlink
            .as_mut()
            .expect("on")
            .outcomes
            .push(outcome(failed, Some(ProbeFailure::Short)));
        let run = sub.run_downlink(&input, at);
        assert!(run.budget_bps.expect("known") < failed);
        sub.downlink
            .as_mut()
            .expect("on")
            .outcomes
            .push(outcome(failed, None));
        let run = sub.run_downlink(&input, at + Duration::from_millis(100));
        let budget = run.budget_bps.expect("known");
        assert_eq!(
            budget,
            failed + failed / 10,
            "with the tenth: the set is given when what it needs is within the budget, and \
             what it needs has moved by the time the probe ends"
        );
    }

    /// The estimate's ceiling follows the largest demand of the last ten
    /// seconds: a publisher whose packets stop for a moment does not
    /// bring the estimate down with them.
    #[test]
    fn a_dip_in_demand_does_not_lower_the_estimate() {
        let t0 = Instant::now();
        let mut sub = downlink_peer("sub", "call");
        add_three_tracks(&mut sub, "pub");
        let full = [downlink_input("pub", VideoQuality::Main)];
        let mut stalled = downlink_input("pub", VideoQuality::Main);
        stalled.main_bps = Some(10_000);
        stalled.middle_bps = Some(10_000);
        stalled.light_bps = Some(10_000);
        sub.downlink_feed(&feedback(t0, 1, |_| Some(20)), t0);
        let run = sub.run_downlink(&full, t0);
        let budget = run.budget_bps.expect("known");
        let run = sub.run_downlink(&[stalled.clone()], t0 + Duration::from_secs(1));
        assert_eq!(run.budget_bps, Some(budget), "the peak holds");
        let run = sub.run_downlink(&full, t0 + Duration::from_secs(2));
        assert_eq!(run.budget_bps, Some(budget));
        assert!(run.changes.is_empty());
        // Once the demand has been gone for ten seconds the ceiling
        // follows it down.
        let run = sub.run_downlink(&[stalled], t0 + Duration::from_secs(13));
        assert!(run.budget_bps.expect("known") < budget);
    }

    /// A burst of loss with no delay growth changes nothing; loss that
    /// lasts steps the estimate down.
    #[test]
    fn a_burst_of_loss_changes_nothing_and_lasting_loss_does() {
        let t0 = Instant::now();
        let mut sub = downlink_peer("sub", "call");
        let second = |n: u64| t0 + Duration::from_secs(n);
        sub.downlink_feed(&feedback(second(0), 200, |_| Some(20)), second(1));
        // One second in which seven packets in ten are missing.
        let lossy = |i: u64| (i % 10 < 3).then_some(20);
        sub.downlink_feed(&feedback(second(1), 200, lossy), second(2));
        sub.downlink_feed(&feedback(second(2), 200, |_| Some(20)), second(3));
        assert_eq!(sub.downlink_budget_bps(), Some(DOWNLINK_CEILING_BPS));
        assert_eq!(sub.downlink.as_ref().expect("on").estimator.cuts(), 0);
        // Five seconds of it.
        for n in 3..8 {
            sub.downlink_feed(&feedback(second(n), 200, lossy), second(n + 1));
        }
        let downlink = sub.downlink.as_ref().expect("on");
        assert!(downlink.estimator.cuts() >= 1, "loss that lasts is a cut");
        assert!(sub.downlink_budget_bps().expect("known") < DOWNLINK_CEILING_BPS);
    }

    /// What the allocation gives replaces what the subscriber asked for
    /// in the per-track gate: a track given up stops with the next
    /// packet; a track is given back only when the path was seen to
    /// carry it, and then starts at a key frame; nothing is ever given
    /// above what was asked for.
    #[test]
    fn what_the_allocation_gives_replaces_the_wanted_quality_in_the_gate() {
        let t0 = Instant::now();
        let mut sub = downlink_peer("sub", "call");
        add_three_tracks(&mut sub, "pub");
        let input = [downlink_input("pub", VideoQuality::Main)];
        let all = DL_LIGHT_BPS + downlink_allocation::with_headroom(DL_MAIN_BPS);
        let middle = DL_LIGHT_BPS + downlink_allocation::with_headroom(DL_MIDDLE_BPS);

        // Nothing known of the path: what was asked for, and the
        // subscriber is told once.
        let run = sub.run_downlink(&input, t0);
        assert_eq!(run.budget_bps, None);
        assert!(run.changes.is_empty() && run.probe_started_bps.is_none());
        assert_eq!(
            run.tell,
            Some(vec![VideoGiven {
                peer: "pub".to_owned(),
                quality: VideoQuality::Main,
            }])
        );
        assert_eq!(sub.video_quality_given(&pid("pub")), VideoQuality::Main);
        assert_eq!(taken(&mut sub, "pub", false), (true, false, true));
        assert_eq!(sub.run_downlink(&input, t0).tell, None, "nothing changed");

        // A budget for the light track alone: the main track stops at
        // once, and the subscriber is told.
        known_budget(&mut sub, DL_LIGHT_BPS, t0);
        let run = sub.run_downlink(&input, t0 + Duration::from_secs(1));
        assert_eq!(run.budget_bps, Some(DL_LIGHT_BPS));
        assert_eq!(
            run.changes,
            vec![GivenChange {
                publisher: pid("pub"),
                from: VideoQuality::Main,
                to: VideoQuality::Light,
                wanted: VideoQuality::Main,
                why: "path-cut",
            }]
        );
        assert_eq!(
            run.tell.expect("told")[0].quality,
            VideoQuality::Light,
            "the frame says what is sent"
        );
        assert_eq!(sub.video_quality_for(&pid("pub")), VideoQuality::Main);
        assert_eq!(sub.video_quality_given(&pid("pub")), VideoQuality::Light);
        assert_eq!(taken(&mut sub, "pub", true), (false, false, true));
        let snap = sub.downlink_snapshot(t0).expect("downlink control is on");
        assert_eq!((snap.main, snap.middle, snap.light), (0, 0, 1));
        assert_eq!(snap.budget_bps, Some(DL_LIGHT_BPS));
        assert_eq!(snap.demand_bps, Some(all));

        // The budget is back, and more: on its word alone nothing is
        // given back, however long it lasts.
        let estimator = &mut sub.downlink.as_mut().expect("on").estimator;
        estimator.note_carried(all * 2);
        for secs in [2, 10, 60] {
            let run = sub.run_downlink(&input, t0 + Duration::from_secs(secs));
            assert!(run.changes.is_empty(), "after {secs} s");
            assert!(run.budget_bps.expect("known") >= all);
        }
        assert_eq!(taken(&mut sub, "pub", true), (false, false, true));

        // The path was seen to carry the middle track: it is given, and
        // starts at a key frame its publisher is asked for.
        let later = t0 + Duration::from_secs(61);
        carried(&mut sub, middle, later);
        let run = sub.run_downlink(&input, later);
        assert_eq!(run.carried_bps, Some(middle));
        assert_eq!(run.changes.len(), 1);
        assert_eq!(run.changes[0].to, VideoQuality::Middle);
        assert_eq!(sub.video_quality_given(&pid("pub")), VideoQuality::Middle);
        assert_eq!(taken(&mut sub, "pub", false), (false, false, true));
        let due = sub.due_resume_keyframes(later, &[]);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].role, TrackRole::Middle);
        assert_eq!(taken(&mut sub, "pub", true), (false, true, true));

        // The subscriber asks for the light picture: proof of any rate
        // gives it no more than that.
        sub.set_video_wants(&[want("pub", VideoQuality::Light)]);
        assert_eq!(sub.video_quality_given(&pid("pub")), VideoQuality::Light);
        let input_light = [downlink_input("pub", VideoQuality::Light)];
        let after = later + Duration::from_secs(60);
        carried(&mut sub, all * 10, after);
        let run = sub.run_downlink(&input_light, after);
        assert_eq!(
            run.tell.expect("told after a video-wants")[0].quality,
            VideoQuality::Light
        );
        assert_eq!(taken(&mut sub, "pub", true), (false, false, true));

        // And told again after the next video-wants, changed or not.
        sub.set_video_wants(&[want("pub", VideoQuality::Light)]);
        let run = sub.run_downlink(&input_light, after + Duration::from_secs(1));
        assert!(run.tell.is_some() && run.changes.is_empty());
    }

    /// While someone is given less than asked for, the next larger set
    /// is tried on the path: a probe toward what it needs, padded with
    /// copies of packets already sent; carried, the set is given.
    #[test]
    fn the_next_larger_set_is_tried_on_the_path_before_it_is_given() {
        use crate::downlink_probe::{PROBE_DURATION, PROBE_FIRST_RETRY};

        let t0 = Instant::now();
        let mut sub = downlink_peer("sub", "call");
        add_three_tracks(&mut sub, "pub");
        let input = [downlink_input("pub", VideoQuality::Main)];
        let _ = sub.run_downlink(&input, t0);
        known_budget(&mut sub, DL_LIGHT_BPS, t0);
        let run = sub.run_downlink(&input, t0 + Duration::from_millis(100));
        assert_eq!(sub.video_quality_given(&pid("pub")), VideoQuality::Light);
        assert_eq!(
            run.probe_started_bps, None,
            "nothing to copy as padding yet: no probe"
        );

        // A packet has been sent on the light track.
        sub.downlink
            .as_mut()
            .expect("on")
            .recent
            .push_back((Mid::from("7"), 100, 1200));
        let start = t0 + Duration::from_millis(200);
        let run = sub.run_downlink(&input, start);
        let middle = DL_LIGHT_BPS + downlink_allocation::with_headroom(DL_MIDDLE_BPS);
        assert_eq!(
            run.probe_started_bps,
            Some(middle),
            "one track up: what the light and the middle track need"
        );
        assert!(run.changes.is_empty(), "not given before it was carried");
        assert!(sub.next_downlink_tick(start).is_some());
        let snap = sub.downlink_snapshot(start).expect("on");
        assert_eq!((snap.probing_bps, snap.probes_started), (Some(middle), 1));
        // No second probe while one runs.
        assert_eq!(
            sub.run_downlink(&input, start + Duration::from_millis(100))
                .probe_started_bps,
            None
        );
        // A unit peer has no send stream to copy on, so the packet is
        // forgotten and no padding is queued.
        assert_eq!(sub.pump_downlink(start + Duration::from_millis(100)), 0);
        assert!(sub.downlink.as_ref().expect("on").recent.is_empty());

        // The subscriber's feedback acknowledges more than the target
        // rate over the probe (1.92 Mbit/s), and a packet sent after it.
        let end = start + PROBE_DURATION;
        // (The 200 packets it reports on were sent: a probe that sent
        // less than its schedule asked for did not try its rate.)
        for i in 0..200u64 {
            sub.downlink.as_mut().expect("on").probe.on_sent(
                1200,
                true,
                start + Duration::from_millis(5 * i),
            );
        }
        let mut records = feedback(start, 200, |_| Some(20));
        records.extend(feedback(end, 1, |_| Some(20)));
        sub.downlink_feed(&records, end + Duration::from_millis(50));
        let done = end + Duration::from_millis(100);
        let run = sub.run_downlink(&input, done);
        assert_eq!(run.outcomes.len(), 1);
        assert!(run.outcomes[0].carried(), "{:?}", run.outcomes[0]);
        assert!(run.carried_bps >= Some(middle));
        assert_eq!(run.changes.len(), 1);
        assert_eq!(run.changes[0].to, VideoQuality::Middle);
        assert!(
            run.budget_bps.expect("known") >= middle,
            "the estimate follows"
        );
        assert_eq!(sub.downlink_snapshot(done).expect("on").probes_carried, 1);

        // The next step (main) is tried next; this time the path answers
        // with a growing queue: the probe ends, nothing is given, and the
        // next one waits.
        sub.downlink
            .as_mut()
            .expect("on")
            .recent
            .push_back((Mid::from("7"), 101, 1200));
        let again = done + Duration::from_secs(1);
        let run = sub.run_downlink(&input, again);
        let main = DL_LIGHT_BPS + downlink_allocation::with_headroom(DL_MAIN_BPS);
        assert_eq!(run.probe_started_bps, Some(main));
        // (In reports of 100 ms: one reading of a queue does not end a
        // probe, two running do.)
        feed_in_reports(&mut sub, again, 150, |i| Some(20 + 4 * i));
        let trouble = again + Duration::from_millis(800);
        let run = sub.run_downlink(&input, trouble);
        assert_eq!(run.outcomes.len(), 1);
        assert!(!run.outcomes[0].carried());
        assert!(run.changes.iter().all(|c| c.to != VideoQuality::Main));
        assert_ne!(sub.video_quality_given(&pid("pub")), VideoQuality::Main);
        assert_eq!(sub.next_downlink_tick(trouble), None, "padding has stopped");
        sub.downlink
            .as_mut()
            .expect("on")
            .recent
            .push_back((Mid::from("7"), 102, 1200));
        let soon = trouble + PROBE_FIRST_RETRY.saturating_sub(Duration::from_millis(1));
        assert_eq!(sub.run_downlink(&input, soon).probe_started_bps, None);
    }

    /// A set that comes to cost more than the estimate on a quiet path
    /// is kept: the path is carrying it, and the limit follows. With a
    /// queue standing the same growth takes the picture away, as before.
    /// (Live call, 2026-10-09: a middle picture taken away with no cut
    /// of the estimate anywhere near, the budget standing at a tenth
    /// over what a probe had tried a minute earlier.)
    #[test]
    fn a_set_that_comes_to_cost_more_on_a_quiet_path_is_kept() {
        let t0 = Instant::now();
        let mut sub = downlink_peer("sub", "call");
        add_three_tracks(&mut sub, "pub");
        let mut input = [downlink_input("pub", VideoQuality::Middle)];
        let _ = sub.run_downlink(&input, t0);
        feed_in_reports(&mut sub, t0, 400, |_| Some(20));
        let at = t0 + Duration::from_secs(2);
        let wire = DL_LIGHT_BPS + DL_MIDDLE_BPS;
        {
            let downlink = sub.downlink.as_mut().expect("on");
            downlink.limit_bps = Some(wire + 10_000);
            downlink.apply_ceiling();
        }
        let run = sub.run_downlink(&input, at);
        assert_eq!(run.budget_bps, Some(wire + 10_000));
        assert!(run.changes.is_empty());

        // The publisher's encoder climbs by a third.
        input[0].middle_bps = Some(DL_MIDDLE_BPS * 4 / 3);
        let grown = DL_LIGHT_BPS + DL_MIDDLE_BPS * 4 / 3;
        let run = sub.run_downlink(&input, at + Duration::from_millis(100));
        assert!(run.changes.is_empty(), "nothing is taken away: {run:?}");
        assert_eq!(run.wire_bps, grown);
        assert_eq!(run.budget_bps, Some(grown + grown / 10));
        let snap = sub.downlink_snapshot(at).expect("on");
        assert_eq!(snap.limit_bps, Some(grown + grown / 10));
        assert_eq!(snap.followed, 1);
        // Once it fits, nothing follows any further.
        let run = sub.run_downlink(&input, at + Duration::from_millis(200));
        assert_eq!(run.budget_bps, Some(grown + grown / 10));
        assert_eq!(sub.downlink_snapshot(at).expect("on").followed, 1);

        // It doubles while a queue stands on the path: that is not a
        // path carrying it.
        input[0].middle_bps = Some(DL_MIDDLE_BPS * 2);
        sub.downlink.as_mut().expect("on").raw_queue_since = Some(at);
        let run = sub.run_downlink(&input, at + Duration::from_millis(300));
        assert_eq!(run.changes.len(), 1, "{run:?}");
        assert_eq!(run.changes[0].to, VideoQuality::Light);
        assert_eq!(run.changes[0].why, "over-budget");
        assert_eq!(sub.downlink_snapshot(at).expect("on").followed, 1);
    }

    /// The limit does not follow a set the path itself has just put the
    /// estimate under, nor a path whose feedback says nothing.
    #[test]
    fn the_limit_does_not_follow_a_set_the_path_has_just_cut_under() {
        let t0 = Instant::now();
        let mut sub = downlink_peer("sub", "call");
        add_three_tracks(&mut sub, "pub");
        let input = [downlink_input("pub", VideoQuality::Middle)];
        let _ = sub.run_downlink(&input, t0);
        feed_in_reports(&mut sub, t0, 400, |_| Some(20));
        let at = t0 + Duration::from_secs(2);
        let _ = sub.run_downlink(&input, at);
        // The path lowers the estimate to the light picture alone (the
        // helper says so as a kept cut does), on feedback that shows no
        // queue.
        {
            let downlink = sub.downlink.as_mut().expect("on");
            downlink.limit_bps = Some(DOWNLINK_CEILING_BPS);
            downlink.estimator.force_estimate(DL_LIGHT_BPS);
            downlink.lowered_since_run = true;
            assert!(downlink.path_is_quiet(at));
        }
        let run = sub.run_downlink(&input, at + Duration::from_millis(100));
        assert_eq!(run.changes.len(), 1);
        assert_eq!(run.changes[0].why, "path-cut");
        assert_eq!(run.budget_bps, Some(DL_LIGHT_BPS));
        assert_eq!(sub.downlink_snapshot(at).expect("on").followed, 0);

        // A path that says nothing (no feedback at all) is not quiet.
        let mut silent = downlink_peer("silent", "call");
        assert!(!silent
            .downlink
            .as_mut()
            .expect("on")
            .path_is_quiet(t0 + Duration::from_secs(60)));
    }

    /// Nothing is given into a queue: while one stands, or a cut waits
    /// to be borne out, not even a probe that was just carried vouches
    /// for a larger set; when the queue has gone, it does.
    #[test]
    fn nothing_is_given_while_a_queue_stands() {
        let t0 = Instant::now();
        let mut sub = downlink_peer("sub", "call");
        add_three_tracks(&mut sub, "pub");
        let input = [downlink_input("pub", VideoQuality::Middle)];
        let middle = DL_LIGHT_BPS + downlink_allocation::with_headroom(DL_MIDDLE_BPS);
        let _ = sub.run_downlink(&input, t0);
        known_budget(&mut sub, DL_LIGHT_BPS, t0);
        let _ = sub.run_downlink(&input, t0 + Duration::from_secs(1));
        assert_eq!(sub.video_quality_given(&pid("pub")), VideoQuality::Light);
        sub.downlink
            .as_mut()
            .expect("on")
            .estimator
            .note_carried(middle * 2);

        let later = t0 + Duration::from_secs(5);
        carried(&mut sub, middle, later);
        sub.downlink.as_mut().expect("on").raw_queue_since = Some(later);
        let run = sub.run_downlink(&input, later);
        assert_eq!(run.carried_bps, None);
        assert!(run.changes.is_empty(), "a queue stands");
        {
            let downlink = sub.downlink.as_mut().expect("on");
            downlink.raw_queue_since = None;
            downlink.cut_pending = true;
        }
        let run = sub.run_downlink(&input, later + Duration::from_millis(100));
        assert!(run.changes.is_empty(), "a cut is pending");
        sub.downlink.as_mut().expect("on").cut_pending = false;
        let run = sub.run_downlink(&input, later + Duration::from_millis(200));
        assert_eq!(run.carried_bps, Some(middle));
        assert_eq!(run.changes.len(), 1);
        assert_eq!(
            (run.changes[0].to, run.changes[0].why),
            (VideoQuality::Middle, "probe")
        );
    }

    /// A set that the path takes away again soon after it was given
    /// proved too large: the next try at it waits, longer each time, and
    /// no proof gives it in the meantime; a set that stays puts the wait
    /// back to the first.
    #[test]
    fn a_set_taken_away_soon_after_it_was_given_waits_longer_each_time() {
        use crate::downlink_probe::{TOO_LARGE_FIRST_WAIT, TOO_LARGE_WITHIN};

        let t0 = Instant::now();
        let mut sub = downlink_peer("sub", "call");
        add_three_tracks(&mut sub, "pub");
        let input = [downlink_input("pub", VideoQuality::Middle)];
        let middle = DL_LIGHT_BPS + downlink_allocation::with_headroom(DL_MIDDLE_BPS);
        let _ = sub.run_downlink(&input, t0);
        known_budget(&mut sub, DL_LIGHT_BPS, t0);
        let _ = sub.run_downlink(&input, t0 + Duration::from_secs(1));

        // Given on proof, and taken away by the path `after` later.
        let give = |sub: &mut MediaPeer, at: Instant| {
            sub.downlink
                .as_mut()
                .expect("on")
                .estimator
                .note_carried(middle * 2);
            carried(sub, middle, at);
            sub.run_downlink(&input, at)
        };
        // The path lowers the estimate to `bps`, under what the light and
        // the middle picture put on the wire (650 kbit/s).
        let take_to = |sub: &mut MediaPeer, bps: u64, at: Instant| {
            known_budget(sub, bps, at);
            sub.run_downlink(&input, at)
        };
        let take = |sub: &mut MediaPeer, at: Instant| take_to(sub, 400_000, at);
        let mut at = t0 + Duration::from_secs(5);
        for wait in [10u64, 20, 40, 60, 60] {
            let run = give(&mut sub, at);
            assert_eq!(run.changes.len(), 1, "given after the wait: {run:?}");
            assert_eq!(run.changes[0].to, VideoQuality::Middle);
            at += Duration::from_secs(3);
            let run = take(&mut sub, at);
            assert_eq!(run.changes[0].why, "path-cut");
            assert_eq!(
                run.too_large,
                Some((1, Duration::from_secs(wait))),
                "taken away three seconds after it was given"
            );
            let snap = sub.downlink_snapshot(at).expect("on");
            assert_eq!(snap.held_from_level, Some(1), "one middle picture");
            // No proof gives it while the wait runs, and no probe starts
            // toward it.
            sub.downlink
                .as_mut()
                .expect("on")
                .recent
                .push_back((Mid::from("7"), 100, 1200));
            let soon = at + Duration::from_millis(wait * 1000 - 100);
            let run = give(&mut sub, soon);
            assert!(run.changes.is_empty(), "held for {wait} s: {run:?}");
            assert_eq!(run.probe_started_bps, None);
            at += Duration::from_secs(wait);
        }

        // Given once more, and this time it stays: the wait is the first
        // again, and a path that takes it away a minute later proved
        // nothing against it.
        let run = give(&mut sub, at);
        assert_eq!(run.changes.len(), 1);
        let held = at + TOO_LARGE_WITHIN;
        let _ = sub.run_downlink(&input, held);
        let snap = sub.downlink_snapshot(held).expect("on");
        assert_eq!(snap.too_large_wait_secs, TOO_LARGE_FIRST_WAIT.as_secs());
        // (Nothing to copy as padding from here on: no probe starts, and
        // the rest is about what is given on proof.)
        sub.downlink.as_mut().expect("on").recent.clear();
        let run = take(&mut sub, held + Duration::from_secs(40));
        assert_eq!(run.changes.len(), 1);
        assert_eq!(run.too_large, None, "a set that had stayed");

        // And a path that collapses under a set three seconds after it
        // was given (the estimate at the light picture alone, less than
        // half of what the set put on the wire) says nothing of the set.
        let again = held + Duration::from_secs(60);
        let run = give(&mut sub, again);
        assert_eq!(run.changes.len(), 1);
        let run = take_to(&mut sub, DL_LIGHT_BPS, again + Duration::from_secs(3));
        assert_eq!(run.changes[0].why, "path-cut");
        assert_eq!(run.too_large, None, "a stall of the path");
        let snap = sub.downlink_snapshot(again).expect("on");
        assert_eq!(snap.held_from_level, None);
    }

    /// A probe does not start into a clump of media, and while it runs a
    /// clump is measured against the rate being tried, not against an
    /// estimate that a dip left far below the path.
    #[test]
    fn a_clump_is_measured_against_the_rate_a_probe_tries() {
        use crate::downlink_probe::ProbeFailure;

        let t0 = Instant::now();
        let mut sub = downlink_peer("sub", "call");
        add_three_tracks(&mut sub, "pub");
        let input = [downlink_input("pub", VideoQuality::Middle)];
        let middle = DL_LIGHT_BPS + downlink_allocation::with_headroom(DL_MIDDLE_BPS);
        let _ = sub.run_downlink(&input, t0);
        known_budget(&mut sub, DL_LIGHT_BPS, t0);
        let _ = sub.run_downlink(&input, t0 + Duration::from_secs(1));
        let at = t0 + Duration::from_secs(4);
        {
            let downlink = sub.downlink.as_mut().expect("on");
            downlink.recent.push_back((Mid::from("7"), 100, 1200));
            // A key frame of the light picture is going out: 1200 bytes
            // are 64 ms at the estimate of 150 kbit/s.
            downlink.note_written(1200, at);
            assert!(downlink.clump_being_written(at));
        }
        let run = sub.run_downlink(&input, at);
        assert_eq!(run.probe_started_bps, None, "not into a clump");
        let start = at + Duration::from_millis(200);
        let run = sub.run_downlink(&input, start);
        assert_eq!(run.probe_started_bps, Some(middle), "once it has gone");

        let downlink = sub.downlink.as_mut().expect("on");
        // 4000 bytes at once: 213 ms at the estimate, 43 ms at the
        // 750 kbit/s the probe tries.
        let key = start + Duration::from_millis(300);
        downlink.note_written(4000, key);
        downlink.give_probe_up_on_a_clump(key);
        assert!(
            downlink.probe.is_running(),
            "not a clump at the probe's rate"
        );
        // And 8000 more on top: 128 ms at that rate.
        downlink.note_written(8000, key);
        downlink.give_probe_up_on_a_clump(key);
        assert!(!downlink.probe.is_running());
        assert_eq!(
            downlink.outcomes.last().and_then(|o| o.failure),
            Some(ProbeFailure::Disturbed)
        );
    }

    /// The middle picture is never sent in place of one that is not
    /// there, and the estimate's ceiling follows what the SFU would like
    /// to send.
    #[test]
    fn a_given_middle_without_a_middle_track_is_the_light_track_alone() {
        let now = Instant::now();
        let mut sub = downlink_peer("sub", "call");
        assert!(sub.observe_remote_track_in(pid("pub"), video_track_in(MAIN_MID, TrackRole::Main)));
        assert!(sub.observe_remote_track_in(
            pid("pub"),
            video_track_in(LIGHT_MID, TrackRole::LightCompanion)
        ));
        open_all(&mut sub);
        // A stale answer of the allocation naming a middle track.
        sub.downlink
            .as_mut()
            .expect("on")
            .given
            .insert(pid("pub"), VideoQuality::Middle);
        assert_eq!(sub.video_quality_given(&pid("pub")), VideoQuality::Light);

        let mut input = downlink_input("pub", VideoQuality::Main);
        input.middle_bps = None;
        sub.downlink_feed(&feedback(now, 1, |_| Some(20)), now);
        let _ = sub.run_downlink(&[input], now);
        let demand = DL_LIGHT_BPS + downlink_allocation::with_headroom(DL_MAIN_BPS);
        let snap = sub.downlink_snapshot(now).expect("on");
        assert_eq!(snap.demand_bps, Some(demand));
        assert_eq!(
            snap.estimate_bps,
            Some(demand / 2 * 3),
            "one and a half times"
        );
        // The figure is capped.
        let mut huge = downlink_input("pub", VideoQuality::Main);
        huge.main_bps = Some(1_000_000_000);
        let _ = sub.run_downlink(&[huge], now + Duration::from_secs(1));
        assert_eq!(
            sub.downlink_snapshot(now).expect("on").demand_bps,
            Some(DOWNLINK_CEILING_BPS)
        );
    }

    /// The order of importance is the order of the subscriber's last
    /// `video-wants`, then the order of joining.
    #[test]
    fn importance_is_the_order_of_the_wants_then_of_joining() {
        let mut sub = downlink_peer("sub", "call");
        let joined = [pid("a"), pid("b"), pid("c"), pid("d")];
        assert_eq!(sub.importance_order(&joined), joined.to_vec());
        sub.set_video_wants(&[
            want("c", VideoQuality::Main),
            want("gone", VideoQuality::Main),
            want("a", VideoQuality::Light),
            want("c", VideoQuality::Middle),
        ]);
        assert_eq!(
            sub.importance_order(&joined),
            vec![pid("c"), pid("a"), pid("b"), pid("d")]
        );
        // The next set replaces the order.
        sub.set_video_wants(&[]);
        assert_eq!(sub.importance_order(&joined), joined.to_vec());
        // Without downlink control there is only the order of joining.
        let mut plain = peer_in_call("plain", "call");
        plain.set_video_wants(&[want("c", VideoQuality::Main)]);
        assert_eq!(plain.importance_order(&joined), joined.to_vec());
    }

    /// What a publisher's tracks cost is the rate arriving on each, and a
    /// track that has gone silent costs nothing.
    #[test]
    fn published_rates_are_the_arrival_rates_of_the_tracks() {
        let now = Instant::now();
        let mut publisher = downlink_publisher("pub", "call");
        let audio = Mid::from("9");
        publisher.register_local_track_in(audio, MediaKind::Audio);
        publisher.track_meters.insert(audio, meter_with_bps(32_000));
        assert_eq!(
            publisher.published_rates(now),
            PublishedRates {
                main_bps: DL_MAIN_BPS,
                middle_bps: DL_MIDDLE_BPS,
                light_bps: DL_LIGHT_BPS,
                audio_bps: 32_000,
            }
        );
        assert_eq!(
            publisher.published_rates(now + Duration::from_secs(3)),
            PublishedRates::default(),
            "nothing has arrived for more than two seconds"
        );

        // The meter is fed by size, header included, and only under
        // downlink control.
        let mut publisher = downlink_peer("pub", "call");
        let packet = video_packet_on(MAIN_MID, 1, false);
        publisher.register_local_track_in(packet.mid, MediaKind::Video);
        for ms in [0u64, 500, 1000] {
            publisher.record_track_packet(&packet, now + Duration::from_millis(ms));
        }
        let bits = 2 * 8 * (packet.payload.len() + RTP_FIXED_HEADER_LEN) as u64;
        assert_eq!(
            publisher
                .published_rates(now + Duration::from_millis(1100))
                .main_bps,
            bits
        );
        let mut plain = peer_in_call("plain", "call");
        plain.record_track_packet(&packet, now);
        assert!(plain.track_meters.is_empty());
    }

    /// The driver lists each subscriber's publishers of its own call in
    /// its order of importance, and the frame it returns says so.
    #[test]
    fn the_driver_gives_the_main_picture_to_the_most_important_publisher() {
        use VideoQuality::{Main, Middle};
        let now = Instant::now();
        let mut router = MediaRouter::new();
        let mut sub = downlink_peer("sub", "call");
        for publisher in ["a", "b", "c"] {
            add_three_tracks(&mut sub, publisher);
        }
        // A publisher of another call, whose tracks this subscriber
        // somehow holds, is never counted.
        add_three_tracks(&mut sub, "elsewhere");
        // Lights (each sent twice), one main track and two middle ones.
        let budget = 3 * 2 * DL_LIGHT_BPS
            + downlink_allocation::with_headroom(DL_MAIN_BPS)
            + 2 * downlink_allocation::with_headroom(DL_MIDDLE_BPS);
        router.add_peer(sub);
        for publisher in ["a", "b", "c"] {
            router.add_peer(downlink_publisher(publisher, "call"));
        }
        router.add_peer(downlink_publisher("elsewhere", "other-call"));

        let given = |frames: Vec<(MediaPeerId, ServerFrame)>| -> Vec<(String, VideoQuality)> {
            assert_eq!(frames.len(), 1, "one subscriber has tracks");
            assert_eq!(frames[0].0, pid("sub"));
            match &frames[0].1 {
                ServerFrame::VideoGiven { given } => {
                    given.iter().map(|g| (g.peer.clone(), g.quality)).collect()
                },
                other => panic!("expected video-given, got {other:?}"),
            }
        };
        // Nothing known of the path: everyone on main, in the order of
        // joining.
        assert_eq!(
            given(drive_downlink_control(&mut router, now)),
            vec![
                ("a".to_owned(), Main),
                ("b".to_owned(), Main),
                ("c".to_owned(), Main)
            ]
        );
        // The budget becomes known and is short: the least important
        // step down, at once.
        let sub = router.peer_mut(&pid("sub")).expect("there");
        known_budget(sub, budget, now);
        let now = now + DOWNLINK_ALLOCATION_INTERVAL;
        assert_eq!(
            given(drive_downlink_control(&mut router, now)),
            vec![
                ("a".to_owned(), Main),
                ("b".to_owned(), Middle),
                ("c".to_owned(), Middle)
            ]
        );
        let sub = router.peer_mut(&pid("sub")).expect("there");
        assert_eq!(taken(sub, "a", false), (true, false, true));
        assert!(!taken(sub, "b", false).0, "b's main track is paused");
        // Nothing is due again at once, and nothing changed.
        assert!(drive_downlink_control(&mut router, now).is_empty());
        assert!(
            drive_downlink_control(&mut router, now + DOWNLINK_ALLOCATION_INTERVAL).is_empty(),
            "run again, nothing to tell"
        );

        // The subscriber names c first: c is the most important now, and
        // the set is told again in that order. The rule answers main for
        // c and middle for a from now on: a steps down at once, c steps
        // up when the path was seen to carry the set.
        let sub = router.peer_mut(&pid("sub")).expect("there");
        sub.set_video_wants(&[want("c", Main), want("a", Main), want("b", Main)]);
        let later = now + Duration::from_millis(500);
        assert_eq!(
            given(drive_downlink_control(&mut router, later)),
            vec![
                ("c".to_owned(), Middle),
                ("a".to_owned(), Middle),
                ("b".to_owned(), Middle)
            ],
            "c is first in line; its main track needs the path to have carried it"
        );
        let after = later + Duration::from_millis(500);
        let sub = router.peer_mut(&pid("sub")).expect("there");
        carried(
            sub,
            3 * 2 * DL_LIGHT_BPS + DL_MAIN_BPS + 2 * DL_MIDDLE_BPS,
            after,
        );
        assert_eq!(
            given(drive_downlink_control(&mut router, after)),
            vec![
                ("c".to_owned(), Main),
                ("a".to_owned(), Middle),
                ("b".to_owned(), Middle)
            ]
        );
        // c's main track waits for a key frame and starts at one; its
        // middle track has stopped.
        let sub = router.peer_mut(&pid("sub")).expect("there");
        assert_eq!(taken(sub, "c", false), (false, false, true));
        let due = sub.due_resume_keyframes(after, &[]);
        assert!(due
            .iter()
            .any(|track| track.source_peer == pid("c") && track.role == TrackRole::Main));
        assert_eq!(taken(sub, "c", true), (true, false, true));
    }

    /// Without downlink control the driver does nothing at all.
    #[test]
    fn the_driver_does_nothing_without_downlink_control() {
        let now = Instant::now();
        let mut router = MediaRouter::new();
        let mut sub = peer_in_call("sub", "call");
        add_three_tracks(&mut sub, "pub");
        sub.downlink_feed(&feedback(now, 1, |_| Some(20)), now);
        router.add_peer(sub);
        router.add_peer(peer_in_call("pub", "call"));
        assert!(drive_downlink_control(&mut router, now).is_empty());
        let sub = router.peer_mut(&pid("sub")).expect("there");
        assert_eq!(sub.video_quality_given(&pid("pub")), VideoQuality::Main);
        assert_eq!(taken(sub, "pub", false), (true, false, true));
    }

    /// The router hands its downlink switch to the peers it creates.
    #[test]
    fn the_router_hands_its_downlink_control_to_the_peers_it_creates() {
        let now = Instant::now();
        let mut router = MediaRouter::new();
        router.set_bitrate_control(BitrateControl::Server(EstimatorConfig::default()));
        assert!(!router.peer_or_insert(pid("a"), now).downlink_control());
        router.set_downlink_control(true);
        assert!(router.peer_or_insert(pid("b"), now).downlink_control());
        let mut client = MediaRouter::new();
        client.set_downlink_control(true);
        assert!(
            !client.peer_or_insert(pid("c"), now).downlink_control(),
            "not when the client decides its bitrate"
        );
    }
    // ---- Senders nobody watches (`video-needed`) ----

    /// A publisher that declared a light sender, and a middle one when
    /// `middle`, with a receive stream on each of its video mids.
    fn needed_publisher(name: &str, middle: bool, mut peer: MediaPeer) -> MediaPeer {
        peer.set_call_id("call".to_owned());
        let mids: &[&str] = if middle {
            &[MAIN_MID, LIGHT_MID, MIDDLE_MID]
        } else {
            &[MAIN_MID, LIGHT_MID]
        };
        peer.light_mid = Some(Mid::from(LIGHT_MID));
        if middle {
            peer.middle_mid = Some(Mid::from(MIDDLE_MID));
        }
        for (i, mid) in mids.iter().enumerate() {
            let mid = Mid::from(*mid);
            let ssrc: Ssrc = (0x1000 + u32::try_from(i).expect("fits")).into();
            {
                let mut api = peer.rtc.direct_api();
                api.declare_media(mid, MediaKind::Video);
                api.expect_stream_rx(ssrc, None, mid, None);
            }
            peer.register_local_track_in(mid, MediaKind::Video);
        }
        assert_eq!(peer.id(), &pid(name));
        peer
    }

    /// A subscriber named `name` with `publisher`'s tracks open.
    fn needed_subscriber(name: &str, publisher: &str, middle: bool) -> MediaPeer {
        let mut sub = peer_in_call(name, "call");
        if middle {
            add_three_tracks(&mut sub, publisher);
        } else {
            for (mid, role) in [
                (MAIN_MID, TrackRole::Main),
                (LIGHT_MID, TrackRole::LightCompanion),
            ] {
                assert!(sub.observe_remote_track_in(pid(publisher), video_track_in(mid, role)));
            }
            open_all(&mut sub);
            sub.apply_video_wants();
        }
        sub
    }

    /// Run [`drive_video_needed`] at `at`, whatever the time since it
    /// last ran, and return what each publisher is told.
    fn needed_at(router: &mut MediaRouter, at: Instant) -> Vec<(MediaPeerId, bool, bool)> {
        router.video_needed_ran = None;
        drive_video_needed(router, at)
            .into_iter()
            .map(|(id, frame)| match frame {
                ServerFrame::VideoNeeded { main, middle } => (id, main, middle),
                other => panic!("expected video-needed, got {other:?}"),
            })
            .collect()
    }

    fn set_wants(router: &mut MediaRouter, sub: &str, wants: &[VideoWant]) {
        take_video_wants(router, &pid(sub), wants);
    }

    #[test]
    fn the_need_rule_counts_only_what_a_subscriber_named() {
        let source = pid("pub");
        // (what the subscriber asked for, downlink control) -> (main, middle)
        let cases = [
            // Nothing said: nothing heavy, with or without downlink control.
            (None, false, (false, false)),
            (None, true, (false, false)),
            (Some(VideoQuality::Main), false, (true, false)),
            (Some(VideoQuality::Middle), false, (false, true)),
            (Some(VideoQuality::Light), false, (false, false)),
            // Under downlink control the middle picture is what a main
            // one falls back to: it stays needed.
            (Some(VideoQuality::Main), true, (true, true)),
            (Some(VideoQuality::Middle), true, (false, true)),
            (Some(VideoQuality::Light), true, (false, false)),
        ];
        for (asked, downlink, expected) in cases {
            let mut sub = if downlink {
                downlink_peer("sub", "call")
            } else {
                peer_in_call("sub", "call")
            };
            add_three_tracks(&mut sub, "pub");
            if let Some(quality) = asked {
                sub.set_video_wants(&[want("pub", quality)]);
            }
            assert_eq!(
                sub.heavy_wanted_of(&source, true),
                expected,
                "asked {asked:?}, downlink control {downlink}"
            );
            // Another publisher's senders are not this one's business.
            assert_eq!(sub.heavy_wanted_of(&pid("other"), true), (false, false));
        }

        // A set that does not name the publisher is silence about it.
        let mut sub = three_track_subscriber("pub");
        sub.set_video_wants(&[want("pub", VideoQuality::Main)]);
        assert_eq!(sub.heavy_wanted_of(&source, true), (true, false));
        sub.set_video_wants(&[want("someone-else", VideoQuality::Main)]);
        assert_eq!(sub.heavy_wanted_of(&source, true), (false, false));
        // The forwarding default is what it was: main, when it flows.
        assert_eq!(sub.video_quality_for(&source), VideoQuality::Main);
        assert_eq!(taken(&mut sub, "pub", false), (true, false, true));

        // The wish, not what downlink control gives: a subscriber given
        // the light picture alone still wants what it asked for.
        let mut sub = downlink_peer("sub", "call");
        add_three_tracks(&mut sub, "pub");
        sub.set_video_wants(&[want("pub", VideoQuality::Main)]);
        sub.downlink
            .as_mut()
            .expect("on")
            .given
            .insert(source.clone(), VideoQuality::Light);
        sub.apply_video_wants();
        assert_eq!(sub.video_quality_given(&source), VideoQuality::Light);
        assert_eq!(sub.heavy_wanted_of(&source, true), (true, true));

        // A publisher without a middle sender: asking for middle is asking
        // for main, for good, and nothing wants a sender that is not there.
        let mut sub = needed_subscriber("sub", "pub", false);
        sub.set_video_wants(&[want("pub", VideoQuality::Middle)]);
        assert_eq!(sub.heavy_wanted_of(&source, false), (true, false));
    }

    #[test]
    fn a_subscriber_that_named_middle_counts_for_middle_only_while_it_negotiates() {
        let source = pid("pub");
        let mut sub = peer_in_call("sub", "call");
        for (mid, role) in [
            (MAIN_MID, TrackRole::Main),
            (LIGHT_MID, TrackRole::LightCompanion),
            (MIDDLE_MID, TrackRole::Middle),
        ] {
            assert!(sub.observe_remote_track_in(source.clone(), video_track_in(mid, role)));
        }
        assert_eq!(sub.heavy_wanted_of(&source, true), (false, false));

        // It names middle before any of its tracks is open: middle, and
        // not main, though main is what it would be forwarded meanwhile.
        sub.set_video_wants(&[want("pub", VideoQuality::Middle)]);
        assert_eq!(sub.video_quality_for(&source), VideoQuality::Main);
        assert_eq!(sub.heavy_wanted_of(&source, true), (false, true));
        // The main and the light track open first, the middle one is
        // still being negotiated: the same.
        for entry in &mut sub.tracks_out {
            entry.state = if entry.role == TrackRole::Middle {
                TrackOutState::Negotiating(Mid::new())
            } else {
                TrackOutState::Open(Mid::new())
            };
        }
        sub.apply_video_wants();
        assert_eq!(sub.video_quality_for(&source), VideoQuality::Main);
        assert_eq!(sub.heavy_wanted_of(&source, true), (false, true));
        // What it is forwarded in the meantime is unchanged: main.
        assert_eq!(
            sub.forward_packet(&source, &video_packet_on(MAIN_MID, 1, false)),
            MediaForward::NoWriter
        );
        open_all(&mut sub);
        sub.apply_video_wants();
        assert_eq!(sub.heavy_wanted_of(&source, true), (false, true));
        // The middle track's entry has not even arrived on the subscriber.
        let mut early = peer_in_call("early", "call");
        assert!(early.observe_remote_track_in(
            source.clone(),
            video_track_in(LIGHT_MID, TrackRole::LightCompanion)
        ));
        early.set_video_wants(&[want("pub", VideoQuality::Middle)]);
        assert_eq!(early.heavy_wanted_of(&source, true), (false, true));
    }

    #[test]
    fn a_joiner_that_says_light_then_middle_never_switches_main_on() {
        let t0 = Instant::now();
        let mut router = idle_router(t0);
        let _ = main_goes_idle(&mut router, t0);
        let ms = Duration::from_millis;
        // Somebody joins: the tracks are in its list, then offered.
        let arrival = t0 + video_needed::UNNEEDED_AFTER + Duration::from_secs(5);
        let mut joiner = peer_in_call("joiner", "call");
        for (mid, role) in [
            (MAIN_MID, TrackRole::Main),
            (LIGHT_MID, TrackRole::LightCompanion),
            (MIDDLE_MID, TrackRole::Middle),
        ] {
            assert!(joiner.observe_remote_track_in(pid("pub"), video_track_in(mid, role)));
        }
        router.add_peer(joiner);
        assert!(needed_at(&mut router, arrival).is_empty());
        for entry in &mut router.peer_mut(&pid("joiner")).expect("there").tracks_out {
            entry.state = TrackOutState::Negotiating(Mid::new());
        }
        // In the millisecond of the offer it says light for everyone, and
        // 230 ms later middle; its tracks open at some point in between.
        set_wants(&mut router, "joiner", &[want("pub", VideoQuality::Light)]);
        assert!(needed_at(&mut router, arrival).is_empty());
        open_all(router.peer_mut(&pid("joiner")).expect("there"));
        assert!(needed_at(&mut router, arrival + ms(100)).is_empty());
        set_wants(&mut router, "joiner", &[want("pub", VideoQuality::Middle)]);
        let mut at = arrival + ms(230);
        while at < arrival + Duration::from_secs(30) {
            assert!(
                needed_at(&mut router, at).is_empty(),
                "the publisher is told nothing: main stays off, middle was on"
            );
            at += ms(250);
        }
        assert_eq!(
            find_peer(&router, "pub").video_needed_told(),
            Some(Needed {
                main: false,
                middle: true
            })
        );
        // Naming main is what switches it on, in the same pass.
        set_wants(&mut router, "joiner", &[want("pub", VideoQuality::Main)]);
        assert_eq!(needed_at(&mut router, at), vec![(pid("pub"), true, true)]);
    }

    #[test]
    fn a_silent_subscriber_never_switches_a_sender_on() {
        let t0 = Instant::now();
        let mut router = MediaRouter::new();
        router.add_peer(needed_publisher(
            "pub",
            true,
            MediaPeer::new(pid("pub"), t0),
        ));
        // A subscriber that never sends `video-wants`, from the start.
        router.add_peer(needed_subscriber("silent", "pub", true));
        assert_eq!(needed_at(&mut router, t0), vec![(pid("pub"), true, true)]);
        let off = t0 + video_needed::UNNEEDED_AFTER;
        assert_eq!(
            needed_at(&mut router, off),
            vec![(pid("pub"), false, false)]
        );
        // Another one joins, under downlink control, and stays silent too.
        let mut second = downlink_peer("second", "call");
        add_three_tracks(&mut second, "pub");
        router.add_peer(second);
        let mut at = off;
        while at < off + Duration::from_secs(120) {
            assert!(
                needed_at(&mut router, at).is_empty(),
                "silence switches nothing on"
            );
            at += Duration::from_secs(1);
        }
        // What it is forwarded is what there is: its main track is not
        // paused, so main the moment main flows, and the light track.
        let silent = router.peer_mut(&pid("silent")).expect("there");
        assert_eq!(taken(silent, "pub", false), (true, false, true));
        // Its first word switches the sender on at once.
        set_wants(&mut router, "silent", &[want("pub", VideoQuality::Middle)]);
        assert_eq!(needed_at(&mut router, at), vec![(pid("pub"), false, true)]);
    }

    #[test]
    fn a_publisher_is_told_true_first_false_after_the_delay_and_true_at_once() {
        let t0 = Instant::now();
        let mut router = MediaRouter::new();
        router.add_peer(needed_publisher(
            "pub",
            true,
            MediaPeer::new(pid("pub"), t0),
        ));
        router.add_peer(needed_subscriber("s1", "pub", true));
        router.add_peer(needed_subscriber("s2", "pub", true));
        set_wants(&mut router, "s1", &[want("pub", VideoQuality::Middle)]);
        set_wants(&mut router, "s2", &[want("pub", VideoQuality::Middle)]);

        // The first word: everything on, though nobody wants main.
        assert_eq!(needed_at(&mut router, t0), vec![(pid("pub"), true, true)]);
        assert_eq!(
            find_peer(&router, "pub").video_needed_told(),
            Some(Needed {
                main: true,
                middle: true
            })
        );
        // Nothing more until the delay is over.
        let almost = t0 + video_needed::UNNEEDED_AFTER.saturating_sub(Duration::from_millis(1));
        assert!(needed_at(&mut router, almost).is_empty());
        let off = t0 + video_needed::UNNEEDED_AFTER;
        assert_eq!(needed_at(&mut router, off), vec![(pid("pub"), false, true)]);
        assert!(needed_at(&mut router, off + Duration::from_secs(60)).is_empty());

        // One subscriber goes to the speaker view: told in the same pass.
        let back = off + Duration::from_secs(90);
        set_wants(&mut router, "s1", &[want("pub", VideoQuality::Main)]);
        assert_eq!(router.video_needed_ran, None, "the next pass runs it");
        assert_eq!(
            drive_video_needed(&mut router, back)
                .into_iter()
                .collect::<Vec<_>>(),
            vec![(
                pid("pub"),
                ServerFrame::VideoNeeded {
                    main: true,
                    middle: true
                }
            )]
        );
        // Both go to the light picture: both senders off, after the delay.
        set_wants(&mut router, "s1", &[want("pub", VideoQuality::Light)]);
        set_wants(&mut router, "s2", &[want("pub", VideoQuality::Light)]);
        assert!(needed_at(&mut router, back + Duration::from_secs(1)).is_empty());
        assert_eq!(
            needed_at(
                &mut router,
                back + Duration::from_secs(1) + video_needed::UNNEEDED_AFTER
            ),
            vec![(pid("pub"), false, false)]
        );
    }

    #[test]
    fn the_pass_is_throttled_unless_wants_changed() {
        let t0 = Instant::now();
        let mut router = MediaRouter::new();
        router.add_peer(needed_publisher(
            "pub",
            true,
            MediaPeer::new(pid("pub"), t0),
        ));
        assert_eq!(drive_video_needed(&mut router, t0).len(), 1);
        router
            .peer_mut(&pid("pub"))
            .expect("there")
            .video_needed_not_delivered();
        let soon = t0 + VIDEO_NEEDED_INTERVAL.saturating_sub(Duration::from_millis(1));
        assert!(drive_video_needed(&mut router, soon).is_empty());
        // The frame that was not delivered is produced again.
        assert_eq!(
            drive_video_needed(&mut router, t0 + VIDEO_NEEDED_INTERVAL).len(),
            1
        );
    }

    #[test]
    fn a_publisher_without_a_middle_sender_is_never_told_middle() {
        let t0 = Instant::now();
        let mut router = MediaRouter::new();
        router.add_peer(needed_publisher(
            "pub",
            false,
            MediaPeer::new(pid("pub"), t0),
        ));
        router.add_peer(needed_subscriber("sub", "pub", false));
        assert_eq!(needed_at(&mut router, t0), vec![(pid("pub"), true, false)]);
        // Asked for middle of a publisher that has none: main it is.
        set_wants(&mut router, "sub", &[want("pub", VideoQuality::Middle)]);
        let later = t0 + video_needed::UNNEEDED_AFTER * 3;
        assert!(needed_at(&mut router, later).is_empty());
        set_wants(&mut router, "sub", &[want("pub", VideoQuality::Light)]);
        assert!(needed_at(&mut router, later).is_empty());
        assert_eq!(
            needed_at(&mut router, later + video_needed::UNNEEDED_AFTER),
            vec![(pid("pub"), false, false)]
        );
    }

    #[test]
    fn a_subscriber_that_left_no_longer_counts() {
        let t0 = Instant::now();
        let mut router = MediaRouter::new();
        router.add_peer(needed_publisher(
            "pub",
            true,
            MediaPeer::new(pid("pub"), t0),
        ));
        router.add_peer(needed_subscriber("grid", "pub", true));
        router.add_peer(needed_subscriber("speaker", "pub", true));
        set_wants(&mut router, "grid", &[want("pub", VideoQuality::Middle)]);
        set_wants(&mut router, "speaker", &[want("pub", VideoQuality::Main)]);
        assert_eq!(needed_at(&mut router, t0), vec![(pid("pub"), true, true)]);
        // The speaker view keeps main on for as long as it is there.
        let long = t0 + video_needed::UNNEEDED_AFTER * 5;
        assert!(needed_at(&mut router, long).is_empty());
        assert!(router.remove_peer(&pid("speaker")));
        assert!(needed_at(&mut router, long).is_empty(), "off is late");
        assert_eq!(
            needed_at(&mut router, long + video_needed::UNNEEDED_AFTER),
            vec![(pid("pub"), false, true)]
        );
        // And with the last subscriber gone, the middle one too.
        assert!(router.remove_peer(&pid("grid")));
        let alone = long + video_needed::UNNEEDED_AFTER * 2;
        assert!(needed_at(&mut router, alone).is_empty());
        assert_eq!(
            needed_at(&mut router, alone + video_needed::UNNEEDED_AFTER),
            vec![(pid("pub"), false, false)]
        );
        // A newcomer that names main switches it back on at once.
        let mut joiner = peer_in_call("joiner", "call");
        assert!(
            joiner.observe_remote_track_in(pid("pub"), video_track_in(MAIN_MID, TrackRole::Main))
        );
        router.add_peer(joiner);
        let arrival = alone + video_needed::UNNEEDED_AFTER * 2;
        assert!(
            needed_at(&mut router, arrival).is_empty(),
            "it has said nothing"
        );
        set_wants(&mut router, "joiner", &[want("pub", VideoQuality::Main)]);
        assert_eq!(
            needed_at(&mut router, arrival),
            vec![(pid("pub"), true, false)]
        );
    }

    #[test]
    fn with_the_switch_off_or_no_declared_senders_nothing_is_ever_told() {
        let t0 = Instant::now();
        let off = MediaPeer::with_downlink_control(
            pid("pub"),
            t0,
            None,
            false,
            BitrateControl::Client,
            LossProtection {
                video_needed: false,
                ..LossProtection::default()
            },
            false,
        );
        let mut router = MediaRouter::new();
        router.add_peer(needed_publisher("pub", true, off));
        // A publisher of one video track that declared nothing.
        let mut plain = publisher_with_video("plain", "call", &[MAIN_MID]);
        plain.register_local_track_in(Mid::from(MAIN_MID), MediaKind::Video);
        router.add_peer(plain);
        let mut sub = needed_subscriber("sub", "pub", true);
        open_video_track(&mut sub, &pid("plain"), Mid::from(MAIN_MID));
        router.add_peer(sub);
        set_wants(
            &mut router,
            "sub",
            &[
                want("pub", VideoQuality::Light),
                want("plain", VideoQuality::Light),
            ],
        );
        for secs in [0, 1, 11, 60, 600] {
            assert!(needed_at(&mut router, t0 + Duration::from_secs(secs)).is_empty());
        }
        for name in ["pub", "plain"] {
            let peer = find_peer(&router, name);
            assert!(!peer.told_video_needed());
            assert_eq!(peer.video_needed_told(), None);
            assert!(peer.idle_senders().is_empty());
        }
        // And a packet of theirs is noted against nothing.
        let publisher = router.peer_mut(&pid("pub")).expect("there");
        publisher.note_sender_arrival(&video_packet_on(MAIN_MID, 1, false), t0);
        assert!(publisher.needs.senders.is_empty());
    }

    /// Bring `pub`'s main sender to idle: told `false`, then silent.
    /// Returns the moment it was taken for idle.
    fn main_goes_idle(router: &mut MediaRouter, t0: Instant) -> Instant {
        set_wants(router, "sub", &[want("pub", VideoQuality::Middle)]);
        assert_eq!(needed_at(router, t0), vec![(pid("pub"), true, true)]);
        let off = t0 + video_needed::UNNEEDED_AFTER;
        // The sender is still flowing when it is told to stop.
        let publisher = router.peer_mut(&pid("pub")).expect("there");
        for i in 0..20u64 {
            let at = t0
                + video_needed::UNNEEDED_AFTER.saturating_sub(Duration::from_millis(200))
                + Duration::from_millis(10 * i);
            let packet = RelayPacket {
                arrival: at,
                ..video_packet_on(MAIN_MID, 100 + i, i == 0)
            };
            publisher.note_sender_arrival(&packet, at);
            publisher.observe_uplink_burst(&packet);
            publisher.observe_burst_end(&packet);
        }
        assert_eq!(needed_at(router, off), vec![(pid("pub"), false, true)]);
        let publisher = router.peer_mut(&pid("pub")).expect("there");
        assert!(
            !publisher.sender_idle(Mid::from(MAIN_MID)),
            "not yet silent"
        );
        let idle = off + video_needed::IDLE_QUIET;
        assert!(needed_at(router, idle).is_empty());
        assert!(find_peer(router, "pub").sender_idle(Mid::from(MAIN_MID)));
        idle
    }

    fn idle_router(t0: Instant) -> MediaRouter {
        let mut router = MediaRouter::new();
        router.add_peer(needed_publisher(
            "pub",
            true,
            MediaPeer::new(pid("pub"), t0),
        ));
        router.add_peer(needed_subscriber("sub", "pub", true));
        router
    }

    #[test]
    fn a_sender_told_to_stop_that_goes_on_sending_is_never_idle() {
        let t0 = Instant::now();
        let mut router = idle_router(t0);
        set_wants(&mut router, "sub", &[want("pub", VideoQuality::Middle)]);
        let _ = needed_at(&mut router, t0);
        let off = t0 + video_needed::UNNEEDED_AFTER;
        assert_eq!(needed_at(&mut router, off), vec![(pid("pub"), false, true)]);
        // A client that ignores the frame: a packet every 30 ms.
        for i in 0..200u64 {
            let at = off + Duration::from_millis(30 * i);
            let publisher = router.peer_mut(&pid("pub")).expect("there");
            publisher.note_sender_arrival(&video_packet_on(MAIN_MID, 100 + i, false), at);
            assert!(needed_at(&mut router, at).is_empty());
            assert!(!find_peer(&router, "pub").sender_idle(Mid::from(MAIN_MID)));
        }
        // The middle and the light sender were never expected to stop.
        let publisher = router.peer_mut(&pid("pub")).expect("there");
        assert!(!publisher.sender_idle(Mid::from(MIDDLE_MID)));
        assert!(!publisher.sender_idle(Mid::from(LIGHT_MID)));
    }

    #[test]
    fn going_idle_and_coming_back_is_no_burst_and_no_loss() {
        let t0 = Instant::now();
        let mut router = idle_router(t0);
        set_wants(&mut router, "sub", &[want("pub", VideoQuality::Middle)]);
        assert_eq!(needed_at(&mut router, t0), vec![(pid("pub"), true, true)]);
        let off = t0 + video_needed::UNNEEDED_AFTER;
        assert_eq!(needed_at(&mut router, off), vec![(pid("pub"), false, true)]);
        // The sender runs on for a moment, and two packets are lost
        // right before it stops (under the share of an interval that is
        // a burst by itself: that rule is not what is tested here).
        let publisher = router.peer_mut(&pid("pub")).expect("there");
        let mut last = off;
        for seq in (100..140u64).chain([142]) {
            last = off + Duration::from_millis(seq - 100);
            let packet = RelayPacket {
                arrival: last,
                ..video_packet_on(MAIN_MID, seq, false)
            };
            publisher.note_sender_arrival(&packet, last);
            publisher.observe_uplink_burst(&packet);
            publisher.observe_burst_end(&packet);
        }
        assert_eq!(publisher.uplink_bursts.open_holes(), 2);
        assert!(publisher.burst_ends.in_burst((Mid::from(MAIN_MID), None)));
        // Still within the quiet time: not idle, the books stand.
        let nearly = last + video_needed::IDLE_QUIET.saturating_sub(Duration::from_millis(1));
        assert!(needed_at(&mut router, nearly).is_empty());
        let publisher = router.peer_mut(&pid("pub")).expect("there");
        assert!(!publisher.sender_idle(Mid::from(MAIN_MID)));
        assert_eq!(publisher.uplink_bursts.open_holes(), 2);
        // Silent for the quiet time: idle, and its books are closed.
        let idle = last + video_needed::IDLE_QUIET;
        assert!(needed_at(&mut router, idle).is_empty());
        let publisher = router.peer_mut(&pid("pub")).expect("there");
        assert!(publisher.sender_idle(Mid::from(MAIN_MID)));
        assert_eq!(publisher.uplink_bursts.open_holes(), 0);
        assert_eq!(publisher.burst_ends.open_holes(), 0);
        assert!(!publisher.burst_ends.in_burst((Mid::from(MAIN_MID), None)));
        assert!(!publisher.poll_uplink_burst(idle + Duration::from_secs(1)));
        assert!(publisher
            .poll_burst_ends(idle + Duration::from_secs(5))
            .is_empty());

        // A minute later it is switched back on, 300 sequence numbers on.
        let back = idle + Duration::from_secs(60);
        set_wants(&mut router, "sub", &[want("pub", VideoQuality::Main)]);
        assert_eq!(needed_at(&mut router, back), vec![(pid("pub"), true, true)]);
        let publisher = router.peer_mut(&pid("pub")).expect("there");
        assert!(
            publisher.sender_idle(Mid::from(MAIN_MID)),
            "idle until its first packet"
        );
        for i in 0..60u64 {
            let at = back + Duration::from_millis(100 + 10 * i);
            let packet = RelayPacket {
                arrival: at,
                ..video_packet_on(MAIN_MID, 425 + i, i == 0)
            };
            publisher.note_sender_arrival(&packet, at);
            publisher.observe_uplink_burst(&packet);
            publisher.observe_burst_end(&packet);
            assert!(!publisher.sender_idle(Mid::from(MAIN_MID)));
            assert!(!publisher.poll_uplink_burst(at), "no burst at packet {i}");
        }
        assert_eq!(publisher.uplink_bursts.open_holes(), 0);
        assert!(!publisher.burst_ends.in_burst((Mid::from(MAIN_MID), None)));
        assert!(publisher
            .poll_burst_ends(back + Duration::from_secs(10))
            .is_empty());
        assert!(drive_burst_keyframes(&mut router, back + Duration::from_secs(10)).is_empty());
        // The sender is no longer expected to be silent.
        let sender = find_peer(&router, "pub").needs.senders[&Mid::from(MAIN_MID)];
        assert!(!sender.expect_silence && !sender.idle);
        assert!(sender.returned_at.is_some());
    }

    #[test]
    fn a_resuming_track_waits_for_an_idle_sender_without_giving_up() {
        let t0 = Instant::now();
        let mut router = idle_router(t0);
        let idle = main_goes_idle(&mut router, t0);
        let main = Mid::from(MAIN_MID);
        // Key frame requests that went unanswered while it was off.
        router
            .peer_mut(&pid("pub"))
            .expect("there")
            .unanswered_keyframe_requests
            .insert((main, None), KEYFRAME_UNANSWERED_MAX);

        // The subscriber wants main again; the publisher is told at once.
        let again = idle + Duration::from_secs(30);
        set_wants(&mut router, "sub", &[want("pub", VideoQuality::Main)]);
        assert_eq!(
            needed_at(&mut router, again),
            vec![(pid("pub"), true, true)]
        );
        // For ten seconds the client does not switch the sender on. The
        // track neither asks nor uses up its requests nor starts at any
        // packet.
        let mut at = again;
        while at < again + Duration::from_secs(10) {
            assert_eq!(drive_video_wants(&mut router, at), 0);
            at += Duration::from_millis(250);
        }
        let sub = router.peer_mut(&pid("sub")).expect("there");
        assert!(sub.track_paused(&pid("pub"), main, TrackRole::Main));
        match sub.tracks_out[0].forwarding.gate {
            Gate::Resuming {
                requests,
                any_packet,
                ..
            } => assert!(requests == 0 && !any_packet),
            other => panic!("expected a resuming track, got {other:?}"),
        }
        // A delta packet arrives first (an encoder that kept its state):
        // the sender is back, the count of unanswered requests starts
        // again, the publisher is asked for a key frame, and the track
        // does not start at that packet.
        let publisher = router.peer_mut(&pid("pub")).expect("there");
        publisher.note_sender_arrival(&video_packet_on(MAIN_MID, 500, false), at);
        assert!(!publisher.sender_idle(main));
        assert_eq!(
            publisher.unanswered_keyframe_requests.get(&(main, None)),
            Some(&1),
            "the old count is gone and the request at the return counts one"
        );
        assert_eq!(
            publisher.last_keyframe_request.get(&(main, None)),
            Some(&at)
        );
        let sub = router.peer_mut(&pid("sub")).expect("there");
        assert_eq!(
            sub.forward_packet(&pid("pub"), &video_packet_on(MAIN_MID, 500, false)),
            MediaForward::NotWanted
        );
        // From here on the wait is counted as before.
        let due = sub.due_resume_keyframes(at, &[]);
        assert_eq!(due.len(), 1, "asked at once now that the sender is back");
        // The key frame starts the track on the stream's next number.
        assert_eq!(
            sub.forward_packet(&pid("pub"), &video_packet_on(MAIN_MID, 501, true)),
            MediaForward::NoWriter
        );
        assert!(!sub.track_paused(&pid("pub"), main, TrackRole::Main));
    }

    #[test]
    fn a_sender_that_returns_with_a_key_frame_is_not_asked_for_one() {
        let t0 = Instant::now();
        let mut router = idle_router(t0);
        let idle = main_goes_idle(&mut router, t0);
        let main = Mid::from(MAIN_MID);
        let again = idle + Duration::from_secs(5);
        set_wants(&mut router, "sub", &[want("pub", VideoQuality::Main)]);
        let _ = needed_at(&mut router, again);
        let publisher = router.peer_mut(&pid("pub")).expect("there");
        let before = publisher.last_keyframe_request.get(&(main, None)).copied();
        publisher.note_sender_arrival(&video_packet_on(MAIN_MID, 500, true), again);
        assert_eq!(
            publisher.last_keyframe_request.get(&(main, None)).copied(),
            before
        );
        assert!(!publisher.sender_idle(main));
    }

    #[test]
    fn an_idle_sender_costs_its_last_rate_then_a_nominal_one_never_nothing() {
        let t0 = Instant::now();
        let mut router = MediaRouter::new();
        let mut publisher = downlink_publisher("pub", "call");
        for (i, mid) in [MAIN_MID, LIGHT_MID, MIDDLE_MID].iter().enumerate() {
            let ssrc: Ssrc = (0x1000 + u32::try_from(i).expect("fits")).into();
            let mut api = publisher.rtc.direct_api();
            api.declare_media(Mid::from(*mid), MediaKind::Video);
            api.expect_stream_rx(ssrc, None, Mid::from(*mid), None);
        }
        router.add_peer(publisher);
        let mut sub = downlink_peer("sub", "call");
        add_three_tracks(&mut sub, "pub");
        router.add_peer(sub);
        // Under downlink control the middle sender stays needed while
        // main is wanted, so the light picture is what switches both off.
        set_wants(&mut router, "sub", &[want("pub", VideoQuality::Light)]);
        let _ = needed_at(&mut router, t0);
        // While it flows: what arrives, and that is remembered.
        let publisher = router.peer_mut(&pid("pub")).expect("there");
        for mid in [MAIN_MID, MIDDLE_MID] {
            let packet = video_packet_on(mid, 1, false);
            publisher.note_sender_arrival(&packet, t0);
            publisher.record_track_packet(&packet, t0);
        }
        let flowing = publisher.published_rates(t0);
        assert_eq!(
            (flowing.main_bps, flowing.middle_bps, flowing.light_bps),
            (DL_MAIN_BPS, DL_MIDDLE_BPS, DL_LIGHT_BPS)
        );
        let off = t0 + video_needed::UNNEEDED_AFTER;
        assert_eq!(
            needed_at(&mut router, off),
            vec![(pid("pub"), false, false)]
        );
        let idle = off + video_needed::IDLE_QUIET;
        let _ = needed_at(&mut router, idle);
        let publisher = router.peer_mut(&pid("pub")).expect("there");
        assert!(publisher.sender_idle(Mid::from(MAIN_MID)));
        assert!(publisher.sender_idle(Mid::from(MIDDLE_MID)));

        // The meters have run dry: the cost is the remembered rate.
        let dry = idle + Duration::from_secs(20);
        assert_eq!(publisher.track_rate_bps(Mid::from(MAIN_MID), dry), 0);
        let rates = publisher.published_rates(dry);
        assert_eq!(rates.main_bps, DL_MAIN_BPS);
        assert_eq!(rates.middle_bps, DL_MIDDLE_BPS);
        // Past the memory: the nominal rate of each role.
        let stale = t0 + video_needed::RATE_MEMORY + Duration::from_secs(30);
        let rates = publisher.published_rates(stale);
        assert_eq!(rates.main_bps, video_needed::NOMINAL_MAIN_BPS);
        assert_eq!(rates.middle_bps, video_needed::NOMINAL_MIDDLE_BPS);
        assert_eq!(rates.light_bps, 0, "a light sender is measured as before");

        // Switched back on: for the first two seconds the meter still
        // holds the silence, and the cost stays the nominal one.
        let back = stale + Duration::from_secs(1);
        set_wants(&mut router, "sub", &[want("pub", VideoQuality::Main)]);
        assert_eq!(needed_at(&mut router, back), vec![(pid("pub"), true, true)]);
        let publisher = router.peer_mut(&pid("pub")).expect("there");
        let packet = video_packet_on(MAIN_MID, 900, true);
        publisher.note_sender_arrival(&packet, back);
        publisher.record_track_packet(&packet, back);
        let rates = publisher.published_rates(back + Duration::from_millis(500));
        assert_eq!(rates.main_bps, video_needed::NOMINAL_MAIN_BPS);
        // A publisher that is told nothing is measured as before: zero.
        let mut plain = downlink_peer("plain", "call");
        plain.register_local_track_in(Mid::from(MAIN_MID), MediaKind::Video);
        assert_eq!(plain.published_rates(t0).main_bps, 0);
    }

    #[test]
    fn the_estimate_cut_while_a_sender_was_off_is_put_back_when_it_is_told_on() {
        let t0 = Instant::now();
        let server = MediaPeer::with_downlink_control(
            pid("pub"),
            t0,
            None,
            false,
            BitrateControl::Server(EstimatorConfig::default()),
            LossProtection::default(),
            false,
        );
        let mut router = MediaRouter::new();
        router.add_peer(needed_publisher("pub", true, server));
        router.add_peer(needed_subscriber("sub", "pub", true));
        let estimate = |router: &MediaRouter| {
            find_peer(router, "pub")
                .uplink
                .as_ref()
                .expect("server-side")
                .estimator
                .estimate_bps()
        };
        let before = estimate(&router);
        let idle = main_goes_idle(&mut router, t0);
        assert_eq!(estimate(&router), before, "going idle lowers nothing");
        assert_eq!(
            find_peer(&router, "pub").needs.estimate_before_idle,
            Some(before)
        );
        // The path hiccups while little is arriving, and the estimate is
        // cut to a fraction of that little.
        let publisher = router.peer_mut(&pid("pub")).expect("there");
        let uplink = publisher.uplink.as_mut().expect("server-side");
        uplink.estimator.force_estimate(300_000);
        uplink.last_remb = Some((idle, 300_000));
        assert_eq!(estimate(&router), 300_000);

        // Told to switch main back on: the old figure, and a REMB at the
        // next tick whatever was last sent.
        let back = idle + Duration::from_secs(40);
        set_wants(&mut router, "sub", &[want("pub", VideoQuality::Main)]);
        assert_eq!(needed_at(&mut router, back), vec![(pid("pub"), true, true)]);
        assert_eq!(estimate(&router), before);
        let publisher = find_peer(&router, "pub");
        assert_eq!(publisher.uplink.as_ref().expect("on").last_remb, None);
        assert_eq!(publisher.needs.estimate_before_idle, None);
    }

    #[test]
    fn the_publisher_stream_line_says_whether_the_sender_is_needed() {
        let t0 = Instant::now();
        let mut router = idle_router(t0);
        let mut ledger = FanoutLedger::new(Duration::from_secs(5), t0);
        // Nothing told yet: nothing noted.
        ledger.record_origin(&pid("pub"), &video_packet_on(MAIN_MID, 1, false));
        note_video_needed(&router, &mut ledger);
        assert_eq!(
            ledger.per_origin[&(pid("pub"), Mid::from(MAIN_MID))].needed,
            None
        );
        let _ = main_goes_idle(&mut router, t0);
        let mut ledger = FanoutLedger::new(Duration::from_secs(5), t0);
        ledger.record_origin(&pid("pub"), &video_packet_on(MIDDLE_MID, 1, false));
        ledger.record_origin(&pid("pub"), &video_packet_on(LIGHT_MID, 1, false));
        note_video_needed(&router, &mut ledger);
        // The idle main sender has a line though nothing arrived on it.
        let main = ledger.per_origin[&(pid("pub"), Mid::from(MAIN_MID))];
        assert_eq!((main.packets, main.needed), (0, Some(false)));
        assert_eq!(
            ledger.per_origin[&(pid("pub"), Mid::from(MIDDLE_MID))].needed,
            Some(true)
        );
        assert_eq!(
            ledger.per_origin[&(pid("pub"), Mid::from(LIGHT_MID))].needed,
            None,
            "the light sender is always needed and is told nothing"
        );
    }

    // ------------------------------------------------------------
    // The publish budget (ADR-0068, amendment of 2026-10-11)
    // ------------------------------------------------------------

    /// A publisher named `name` under server-side bitrate control with its
    /// three camera senders declared, `protection` as given.
    fn budget_publisher(name: &str, protection: LossProtection, t0: Instant) -> MediaPeer {
        let server = MediaPeer::with_downlink_control(
            pid(name),
            t0,
            None,
            false,
            BitrateControl::Server(EstimatorConfig::default()),
            protection,
            false,
        );
        needed_publisher(name, true, server)
    }

    /// Make `name` look connected and sending video, with the estimate of
    /// its uplink at `estimate_bps`.
    fn publishing_at(router: &mut MediaRouter, name: &str, estimate_bps: u64) {
        let publisher = router.peer_mut(&pid(name)).expect("there");
        publisher.connected = true;
        let uplink = publisher.uplink.as_mut().expect("server-side");
        if uplink.video_ssrcs.is_empty() {
            uplink.video_ssrcs.push(0x1000.into());
        }
        uplink.estimator.force_estimate(estimate_bps);
    }

    /// Run [`drive_publish_budget`] at `at`, whatever the time since it
    /// last ran, and return what each publisher is told.
    fn budget_at(router: &mut MediaRouter, at: Instant) -> Vec<(MediaPeerId, u32, u32)> {
        router.publish_budget_ran = None;
        drive_publish_budget(router, at)
            .into_iter()
            .map(|(id, frame)| match frame {
                ServerFrame::PublishBudget {
                    main_kbps,
                    middle_kbps,
                } => (id, main_kbps, middle_kbps),
                other => panic!("expected publish-budget, got {other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_publisher_is_told_its_budget_once_it_sends_video_and_a_cut_at_once() {
        let t0 = Instant::now();
        let mut router = MediaRouter::new();
        router.add_peer(budget_publisher("pub", LossProtection::default(), t0));
        // Not connected, no video yet: the start value is not a budget.
        assert_eq!(budget_at(&mut router, t0), vec![]);
        publishing_at(&mut router, "pub", 3_000_000);
        // 3000 - 64 - 150, over 1.15: 2422 for video; middle 500.
        assert_eq!(budget_at(&mut router, t0), vec![(pid("pub"), 1922, 500)]);
        assert_eq!(
            find_peer(&router, "pub").publish_budget_told(),
            Some(publish_budget::Budget {
                main_kbps: 1922,
                middle_kbps: 500
            })
        );
        // Every frame the rule makes is one the wire format accepts.
        for (_, frame) in {
            router.peer_mut(&pid("pub")).expect("there").budget = PublishBudget::new();
            router.publish_budget_ran = None;
            drive_publish_budget(&mut router, t0)
        } {
            assert!(lakko_sfu_proto::encode_server_frame(&frame).is_ok());
        }
        // Nothing while nothing changes.
        assert_eq!(budget_at(&mut router, t0 + Duration::from_secs(30)), vec![]);
        // The estimate is cut: told in the same pass, no interval.
        let cut = t0 + Duration::from_secs(30) + Duration::from_millis(1);
        publishing_at(&mut router, "pub", 1_000_000);
        assert_eq!(budget_at(&mut router, cut), vec![(pid("pub"), 200, 483)]);
        // It climbs back: not before the interval.
        publishing_at(&mut router, "pub", 3_000_000);
        let soon = cut + publish_budget::RAISE_INTERVAL.saturating_sub(Duration::from_millis(1));
        assert_eq!(budget_at(&mut router, soon), vec![]);
        assert_eq!(
            budget_at(&mut router, cut + publish_budget::RAISE_INTERVAL),
            vec![(pid("pub"), 1922, 500)]
        );
    }

    #[test]
    fn the_budget_runs_on_its_interval_and_an_undelivered_frame_comes_again() {
        let t0 = Instant::now();
        let mut router = MediaRouter::new();
        router.add_peer(budget_publisher("pub", LossProtection::default(), t0));
        publishing_at(&mut router, "pub", 2_000_000);
        assert_eq!(drive_publish_budget(&mut router, t0).len(), 1);
        // A cut right after: seen at the next run, not sooner.
        publishing_at(&mut router, "pub", 1_000_000);
        let soon = t0 + PUBLISH_BUDGET_INTERVAL.saturating_sub(Duration::from_millis(1));
        assert_eq!(drive_publish_budget(&mut router, soon).len(), 0);
        let due = t0 + PUBLISH_BUDGET_INTERVAL;
        assert_eq!(drive_publish_budget(&mut router, due).len(), 1);
        // The frame could not be queued: the same content again.
        router
            .peer_mut(&pid("pub"))
            .expect("there")
            .publish_budget_not_delivered();
        assert_eq!(budget_at(&mut router, due), vec![(pid("pub"), 200, 483)]);
    }

    #[test]
    fn a_sender_nobody_watches_gives_its_share_to_the_other_in_the_same_pass() {
        let t0 = Instant::now();
        let mut router = MediaRouter::new();
        router.add_peer(budget_publisher("pub", LossProtection::default(), t0));
        router.add_peer(needed_subscriber("sub", "pub", true));
        publishing_at(&mut router, "pub", 2_000_000);
        // Told nothing of its senders yet: both take from the budget.
        assert_eq!(budget_at(&mut router, t0), vec![(pid("pub"), 1053, 500)]);
        // The subscriber shows the main picture; the middle sender is
        // told off after the delay.
        set_wants(&mut router, "sub", &[want("pub", VideoQuality::Main)]);
        assert_eq!(needed_at(&mut router, t0), vec![(pid("pub"), true, true)]);
        let off = t0 + video_needed::UNNEEDED_AFTER;
        router.publish_budget_ran = Some(off);
        assert_eq!(needed_at(&mut router, off), vec![(pid("pub"), true, false)]);
        // The frame above makes the budget run in this pass, interval or
        // not, and the main sender gets the middle sender's share: a rise
        // told at once because the need changed.
        assert_eq!(router.publish_budget_ran, None);
        assert_eq!(
            drive_publish_budget(&mut router, off)
                .into_iter()
                .map(|(_, frame)| frame)
                .collect::<Vec<_>>(),
            vec![ServerFrame::PublishBudget {
                main_kbps: 1553,
                middle_kbps: 500
            }]
        );
    }

    #[test]
    fn no_budget_is_told_with_the_switch_off_or_when_the_client_decides() {
        let t0 = Instant::now();
        // The switch off, the SFU deciding the bitrate.
        let mut router = MediaRouter::new();
        let off = LossProtection {
            publish_budget: false,
            ..LossProtection::default()
        };
        router.add_peer(budget_publisher("pub", off, t0));
        publishing_at(&mut router, "pub", 2_000_000);
        assert!(!find_peer(&router, "pub").told_publish_budget());
        assert_eq!(budget_at(&mut router, t0), vec![]);
        // The switch on, the client deciding its own bitrate: no estimate
        // to make a budget from.
        let mut router = MediaRouter::new();
        let mut client = needed_publisher("pub", true, peer_in_call("pub", "call"));
        client.connected = true;
        assert!(client.uplink.is_none());
        assert!(!client.told_publish_budget());
        router.add_peer(client);
        assert_eq!(budget_at(&mut router, t0), vec![]);
    }
}
