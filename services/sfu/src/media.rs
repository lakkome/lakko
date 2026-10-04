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
//!   ([`crate::frame_descriptor`]), with audio level and
//!   video orientation; `mid`, `rid`, the transport-wide
//!   sequence number and absolute send time belong to each
//!   leg and are set by str0m.
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
//! walks every non-origin peer of the same call; simulcast
//! layer selection is `drive_layer_selection` over
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
use str0m::media::{Direction, KeyframeRequestKind, MediaKind, Mid, Rid};
use str0m::net::{Protocol, Receive, Transmit};
use str0m::rtp::{
    Extension, ExtensionMap, ExtensionValues, RtpPacket, SeqNo, Ssrc, VideoOrientation,
};
use str0m::{Candidate, Event, Input, Output, Rtc};
use thiserror::Error;

use crate::fec_receive::{self, FecReceiver, MutableExtensions};
use crate::fec_send::{FecSender, Generated};
use crate::frame_descriptor::{self, FrameDescriptor, PacketDescriptor};
use crate::peer::{PeerId, PeerRegistry};
use crate::protocol::ServerFrame;
use crate::stream_map::StreamMap;
use crate::uplink_estimate::{EstimatorConfig, PacketArrival, SeqLossCounter, UplinkEstimator};
use crate::video_fec;

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
    /// SFU sends exactly one simulcast layer per subscriber,
    /// so packets of the other layers are dropped here by
    /// design — not a fault.
    WrongLayer,
    /// The packet precedes everything the downstream stream
    /// can number: it is older than the packet a simulcast
    /// switch took effect at, or than the sequence-number
    /// cycle the stream started in. Its number downstream
    /// is already taken, so it is not forwarded.
    Stale,
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
        if self.source_ssrc == Some(packet.ssrc) {
            return map.map(seq_no, packet.rtp_time, packet.arrival, clock_hz);
        }
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
    /// Audio level header extension, if the packet carried it.
    pub(crate) audio_level: Option<i8>,
    /// Voice-activity flag of the same extension.
    pub(crate) voice_activity: Option<bool>,
    /// Video orientation header extension, if carried.
    pub(crate) video_orientation: Option<VideoOrientation>,
    /// The generic frame descriptor exactly as this packet
    /// carried it, relayed verbatim.
    pub(crate) descriptor: Option<PacketDescriptor>,
    /// The packet is the first of a frame with no
    /// dependencies, by the descriptor's own flags: the one
    /// place a simulcast switch may take effect.
    pub(crate) starts_independent_frame: bool,
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
    /// fills them in for the egress leg.
    fn egress_extensions(&self) -> ExtensionValues {
        let mut ext_vals = ExtensionValues {
            audio_level: self.audio_level,
            voice_activity: self.voice_activity,
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
    /// publisher mid of the track it protects. Drained every
    /// loop pass ([`MediaPeer::take_fec_sent`]).
    fec_sent: Vec<(MediaPeerId, Mid)>,
    /// Bitrate control for the media THIS peer publishes
    /// (ADR-0068): the estimate of what its path carries and the
    /// REMB bookkeeping. `None` under [`BitrateControl::Client`],
    /// and then nothing of it is ever touched. Boxed so a peer
    /// without it is no larger for the feature.
    uplink: Option<Box<Uplink>>,
}

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
    /// Publisher and publisher mid of the track, for the ledger.
    source: (MediaPeerId, Mid),
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
}

impl KeyframeReason {
    /// Stable, low-cardinality label for logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DownstreamOpened => "downstream-opened",
            Self::SubscriberRequest => "subscriber-pli",
            Self::Owed => "owed",
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

/// The session extension map of a peer under [`BitrateControl::Server`]:
/// str0m's standard map without the transport-wide sequence number. An
/// extension that is not in the session map is neither offered nor echoed
/// in an answer, so no media line of this peer negotiates it;
/// `abs-send-time` stays.
fn extension_map_without_transport_cc() -> ExtensionMap {
    let mut map = ExtensionMap::empty();
    for (id, ext) in ExtensionMap::standard().iter() {
        if !matches!(ext, Extension::TransportSequenceNumber) {
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
        // is that `Event::EgressBitrateEstimate` never fires, so the
        // simulcast layer policy (`drive_layer_selection`) has no estimate to
        // act on and stays on the latched layer; nothing publishes simulcast
        // today. TWCC feedback toward publishers is a receive-side setting
        // and is unaffected.
        //
        // Redundant audio (RFC 2198, ADR-0067 step 3) is switched on: a
        // `red/48000/2` payload type listed before Opus, in the answer to a
        // publisher that offers it and in the SFU's own offers. A client
        // builds the RED container before SFrame seals the frame, so the SFU
        // forwards a RED payload as it forwards any other, unread, and only
        // to a subscriber whose track negotiated `red` (see
        // [`Self::forward_packet`]).
        let mut config = Rtc::builder().set_rtp_mode(true).enable_bwe(None);
        if matches!(bitrate, BitrateControl::Server(_)) {
            // ADR-0068: no transport-wide congestion control with this
            // peer. Without the extension in the session map str0m neither
            // offers it nor echoes it.
            config = config.set_extension_map(extension_map_without_transport_cc());
        }
        let mut config = config
            .set_extension(
                frame_descriptor::GENERIC_FRAME_DESCRIPTOR_ID,
                frame_descriptor::generic_frame_descriptor_extension(),
            )
            .set_stats_interval(stats_interval);
        config.codec_config().enable_red(true);
        // Video FEC (ADR-0067 step 4) is opt-in per deployment: with it, a
        // `red/90000` (with its RTX) and a `ulpfec/90000` payload type are
        // answered to a publisher that offers both and offered to every
        // subscriber. Unlike audio RED, the video RED header and the FEC
        // header are outside the SFrame-sealed frame, so the SFU can and
        // must map them per subscriber (see [`Self::forward_packet`]).
        config.codec_config().enable_video_fec(video_fec);
        if matches!(bitrate, BitrateControl::Server(_)) {
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
            owed_keyframe_requests: std::collections::HashSet::new(),
            video_fec,
            fec_receivers: HashMap::new(),
            fec_senders: HashMap::new(),
            fec_sent: Vec::new(),
            uplink: match bitrate {
                BitrateControl::Client => None,
                BitrateControl::Server(estimator) => Some(Box::new(Uplink {
                    estimator: UplinkEstimator::new(estimator, now),
                    loss: SeqLossCounter::new(),
                    send_time_base: now,
                    video_ssrcs: Vec::new(),
                    last_loss_feed: now,
                    last_remb: None,
                })),
            },
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
            forwarding: Forwarding::default(),
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
            audio_level: ext_vals.audio_level,
            voice_activity: ext_vals.voice_activity,
            video_orientation: ext_vals.video_orientation,
            descriptor,
            starts_independent_frame,
            payload: packet.payload,
        })
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
    ///    the selected layer. Anything else is `WrongLayer`.
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
    /// publisher's frame descriptor, audio level and video
    /// orientation, `nackable` for video (so str0m keeps it for a
    /// subscriber's retransmission request) and not for audio,
    /// and its arrival time as the wall clock str0m builds sender
    /// reports from (a late packet gets the matching earlier
    /// moment instead, see [`crate::stream_map`]).
    pub(crate) fn forward_packet(
        &mut self,
        source_peer: &MediaPeerId,
        packet: &RelayPacket,
    ) -> MediaForward {
        // Mutable: the layer gate latches the selected layer and the map
        // advances in place.
        let entry = self.tracks_out.iter_mut().find(|entry| {
            entry.source_peer == *source_peer
                && entry.source_mid == packet.mid
                && matches!(entry.state, TrackOutState::Open(_))
        });
        let Some(entry) = entry else {
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
            // Unreachable — the `matches!` filter above kept only Open entries.
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
                source: (entry.source_peer.clone(), entry.source_mid),
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
        match stream.write_rtp(
            pt,
            SeqNo::from(position.seq_no),
            position.rtp_time,
            position.wallclock,
            packet.marker,
            packet.egress_extensions(),
            nackable,
            payload,
        ) {
            Ok(()) => MediaForward::Sent,
            Err(e) => MediaForward::WriteFailed(e.to_string()),
        }
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
    pub(crate) fn offer_publisher_fec(&mut self, source_peer: &MediaPeerId, packet: &RelayPacket) {
        if !self.video_fec || !self.connected || !packet.is_fec() {
            return;
        }
        let entry = self.tracks_out.iter_mut().find(|entry| {
            entry.source_peer == *source_peer
                && entry.source_mid == packet.mid
                && matches!(entry.state, TrackOutState::Open(_))
        });
        let Some(entry) = entry else {
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
    /// as the publisher track each protects.
    pub(crate) fn take_fec_sent(&mut self) -> Vec<(MediaPeerId, Mid)> {
        std::mem::take(&mut self.fec_sent)
    }

    /// Choose which simulcast layer this peer receives for the downstream track
    /// mirroring `(source_peer, source_mid)`. A bandwidth / quality policy calls
    /// this to override the auto-latched first layer — e.g. downgrade to a lower
    /// layer when the subscriber's egress estimate drops. `None` re-arms the
    /// auto-latch (the next observed layer is chosen). Returns `true` if a
    /// matching downstream track exists.
    ///
    /// The selection is not the switch. A track that is already forwarding a
    /// layer keeps forwarding it until a packet begins an independent frame
    /// on the selected one (`layer_gate_allows`), so the subscriber never
    /// receives a delta frame it has no reference for.
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
        let key = (mid, rid);
        if !keyframe_request_allowed(self.last_keyframe_request.get(&key).copied(), now) {
            // Inside the window: owed, not dropped.
            self.owed_keyframe_requests.insert(key);
            return false;
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
        }
        sent
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
            if self.request_keyframe_limited(mid, rid, now) {
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
///   would otherwise be read from.
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
            self.peers.push(MediaPeer::with_bitrate_control(
                id,
                now,
                self.stats_interval,
                self.video_fec,
                self.bitrate_control,
            ));
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
    /// every packet was fanned out to it, and every joiner was
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
    let mut router = MediaRouter::new();
    router.set_stats_interval(diagnostics);
    router.set_video_fec(options.video_fec);
    router.set_bitrate_control(bitrate);
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
            if l.is_due(now) {
                log_uplink_estimates(&mut router, now);
            }
            l.flush_if_due(now);
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
        let layer_switches = drive_layer_selection(&mut router);
        record_layer_selection_metrics(&layer_switches);
        // A packet written to a peer, or a keyframe request queued on one,
        // leaves on that peer's next clock tick and poll. Go straight round
        // rather than waiting for the next datagram: nothing is held at the
        // SFU. The second pass receives no input, so it captures no packet
        // and falls through to the wait below.
        if forwarded.sent > 0 || relayed > 0 || fec_sent > 0 {
            continue;
        }
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
                            peer.observe_uplink(seen, &packet);
                            let rebuilt = peer.fec_receive(&packet, received, now);
                            for packet in std::iter::once(packet).chain(rebuilt) {
                                // Meter this layer's bitrate (simulcast only)
                                // before the packet moves into the fanout
                                // queue — the per-receiver policy reads
                                // these rates.
                                peer.record_layer_packet(&packet, now);
                                pending.push(PendingFanout {
                                    origin: origin.clone(),
                                    packet,
                                });
                            }
                        }
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
                        // holds no media, so the only thing that can answer
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
/// never offered its own packet.
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
    /// Video FEC packets the SFU computed for and wrote to the
    /// destination ([`crate::fec_send`]). Not a forward attempt
    /// and not part of [`Self::attempted`]: filed by the media
    /// loop per destination, in the ledger only.
    pub(crate) fec_sent: u64,
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
    }

    /// File one forward result into its bucket.
    pub(crate) fn tally(&mut self, forwarded: &MediaForward) {
        match forwarded {
            MediaForward::Sent => self.sent += 1,
            MediaForward::NoWriter => self.no_writer += 1,
            MediaForward::NoCodecMatch => self.no_codec_match += 1,
            MediaForward::WrongLayer => self.wrong_layer += 1,
            MediaForward::Stale => self.stale += 1,
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
        forwarded: &MediaForward,
    ) {
        self.per_target
            .entry((target.clone(), origin.clone(), origin_mid))
            .or_default()
            .tally(forwarded);
    }

    /// File one FEC packet the SFU wrote to `target` for the
    /// track `(origin, origin_mid)`.
    pub(crate) fn record_fec_sent(
        &mut self,
        target: &MediaPeerId,
        origin: &MediaPeerId,
        origin_mid: Mid,
    ) {
        self.per_target
            .entry((target.clone(), origin.clone(), origin_mid))
            .or_default()
            .fec_sent += 1;
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
                stale = o.stale,
                fec_sent = o.fec_sent,
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
    for event in pending {
        if let Some(l) = ledger.as_deref_mut() {
            l.record_origin(&event.origin, &event.packet);
        }
        // Cross-call isolation: media is forwarded ONLY to
        // peers in the SAME call as the origin. Look up the
        // origin's call once; if the origin is gone (pruned)
        // or unbound (empty call), forward nothing. A peer in
        // a different call NEVER receives this packet.
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
            let forwarded = peer.forward_packet(&event.origin, &event.packet);
            if let Some(l) = ledger.as_deref_mut() {
                l.record(peer.id(), &event.origin, event.packet.mid, &forwarded);
            }
            if let MediaForward::WriteFailed(detail) = &forwarded {
                tracing::warn!(
                    target_peer = %peer.id(),
                    error = %detail,
                    "media: forward write failed"
                );
            }
            outcome.tally(&forwarded);
        }
    }
    record_fanout_metrics(&outcome);
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
            for (origin, origin_mid) in &sent {
                l.record_fec_sent(peer.id(), origin, *origin_mid);
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
/// immediately, recovery upgrades with headroom hysteresis. A no-op for a peer
/// without an estimate yet, or a track with fewer than two measured layers
/// (audio / non-simulcast video). The two-pass shape (snapshot every
/// publisher's per-layer bitrates, then mutate each subscriber, then request a
/// keyframe from each publisher whose layer changed) avoids a cross-peer borrow
/// conflict. On every selection the SFU asks the publisher for a keyframe (PLI)
/// for the newly-selected layer; the subscriber's track goes on forwarding its
/// current layer and switches at the packet that begins that keyframe
/// (`layer_gate_allows`), so the switch starts on a decodable frame. Returns a
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
    fn server_bitrate_control_drops_only_the_transport_wide_sequence_number() {
        let map = extension_map_without_transport_cc();
        let standard = ExtensionMap::standard();
        assert!(map.id_of(Extension::TransportSequenceNumber).is_none());
        assert!(standard.id_of(Extension::TransportSequenceNumber).is_some());
        assert_eq!(map.iter().count() + 1, standard.iter().count());
        for ext in [
            Extension::AbsoluteSendTime,
            Extension::AudioLevel,
            Extension::RtpMid,
            Extension::RtpStreamId,
            Extension::RepairedRtpStreamId,
            Extension::VideoOrientation,
        ] {
            assert_eq!(map.id_of(ext.clone()), standard.id_of(ext));
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
            audio_level: None,
            voice_activity: None,
            video_orientation: None,
            descriptor: None,
            starts_independent_frame: false,
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
        l.record(&pid("sub1"), &pid("pub"), packet.mid, &MediaForward::Sent);
        l.record(
            &pid("sub1"),
            &pid("pub"),
            packet.mid,
            &MediaForward::NoWriter,
        );
        l.record(&pid("sub2"), &pid("pub"), packet.mid, &MediaForward::Sent);
        let sub1 = &l.per_target[&(pid("sub1"), pid("pub"), packet.mid)];
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
                fec_sent: 0,
            }
        );
        assert_eq!(o.attempted(), 6);
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
        for mut subscriber in [fec_peer("sub"), MediaPeer::new(pid("sub"), Instant::now())] {
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
        l.record(&pid("sub"), &pid("pub"), mid, &MediaForward::Sent);
        l.record_fec_sent(&pid("sub"), &pid("pub"), mid);
        l.record_fec_sent(&pid("sub"), &pid("pub"), mid);
        let o = l.per_target[&(pid("sub"), pid("pub"), mid)];
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
            audio_level: Some(-30),
            voice_activity: Some(true),
            video_orientation: Some(VideoOrientation::Deg90),
            descriptor: Some(descriptor.clone()),
            ..synthetic_video_packet()
        };
        let ev = packet.egress_extensions();
        assert_eq!(ev.audio_level, Some(-30));
        assert_eq!(ev.voice_activity, Some(true));
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
        assert!(bare.audio_level.is_none() && bare.video_orientation.is_none());
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
