// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! End-to-end integration test that proves the SFU's
//! media-routing task can drive a real `str0m::Rtc`
//! synthetic client through the full ICE + DTLS handshake
//! over UDP loopback (S-3-B-tracks step 3).
//!
//! ## Why this test exists
//!
//! `tests/integration_signaling.rs` covers the WS
//! signaling layer in-process. `tests/e2e_dockerized.rs`
//! exercises the binary inside a container. **Neither**
//! drives real WebRTC media — until this test, the only
//! coverage of the media task's ICE + DTLS path came from
//! str0m's own unit tests.
//!
//! This test closes the gap: it spawns the production
//! `media::spawn_media_task` on UDP loopback, builds a
//! synthetic `str0m::Rtc` client in the same process on
//! its own UDP socket, exchanges SDP through
//! `MediaTaskHandle::accept_offer`, and then pumps the
//! `poll_output` / `handle_input` loop until both sides
//! converge on connected ICE + DTLS state.
//!
//! ## Two synthetic clients (S-3-B-tracks step 3b)
//!
//! [`synthetic_client_completes_handshake_with_media_task`]
//! proves the single-client DTLS bedrock.
//! [`two_clients_cross_fanout_forwards_media_verbatim`]
//! stacks the cross-fanout layer on top: a publisher A
//! and a subscriber B both complete the handshake, the
//! SFU-initiated renegotiation (`drive_renegotiations`)
//! dispatches a `ServerFrame::SdpOffer` to B's outbound
//! mpsc — which the test owns by registering a fake
//! [`Peer`] in the [`PeerRegistry`] — and a frame written
//! by A surfaces **verbatim** at B (the synthetic clients
//! run `str0m` in frame mode and so assemble what the SFU
//! relays packet by packet). This
//! closes the C-39 SFrame-passthrough invariant on the
//! integration-test side: the SFU forwards opaque RTP
//! payload bytes between peers without inspecting or
//! mutating them.
//!
//! [`sframe_sealed_frame_round_trips_through_the_live_sfu`]
//! (G-26, headless slice) goes one layer further: the
//! payload is a REAL `lakko_calls::sframe` sealed frame, and
//! after the SFU forwards it verbatim the subscriber
//! `open_framed`s it back to the original plaintext — proving
//! the SFrame layer round-trips over the live ICE + DTLS +
//! fanout path (the RFC 9605 header / AAD survives intact).
//! The cross-*runtime* leg (a browser-worker-sealed frame
//! opened by a native shim over a deployed SFU) stays
//! deferred — it needs live runtimes, not headless CI.
//!
//! ## What these tests catch
//!
//! - Any regression in `MediaPeer::accept_offer`'s SDP
//!   plumbing that would prevent str0m's SDP-API from
//!   producing a usable answer.
//! - Any regression in `MediaPeer::add_local_host_candidate`
//!   (the SFU's UDP socket address must be baked into
//!   the answer's `a=candidate:` line; without it
//!   client-side ICE never finds a working pair).
//! - Any regression in the per-tick `run_media_loop`
//!   `poll_output → UdpSocket::send_to` path — without
//!   it the SFU side of the DTLS handshake stops mid-
//!   way and the test times out.
//! - The production bind shape, an unspecified
//!   `0.0.0.0` media socket plus an advertise IP
//!   ([`unspecified_bind_with_advertise_ip_completes_handshake`]):
//!   the answer must carry the advertised candidate, and
//!   the receive path must hand str0m that candidate's
//!   address (not the socket's `0.0.0.0` local address)
//!   as each datagram's destination, or str0m discards
//!   every connectivity check.

#![cfg(test)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lakko_svc_sfu::frame_descriptor::{
    generic_frame_descriptor_extension, FrameDescriptor, GENERIC_FRAME_DESCRIPTOR_ID,
    GENERIC_FRAME_DESCRIPTOR_URI,
};
use lakko_svc_sfu::media::{
    spawn_media_task, spawn_media_task_advertising, spawn_media_task_with_bitrate_control,
    spawn_media_task_with_options, BitrateControl, LossProtection, MediaTaskHandle,
    MediaTaskOptions,
};
use lakko_svc_sfu::peer::{Peer, PeerId, PeerRegistry};
use lakko_svc_sfu::protocol::ServerFrame;
use lakko_svc_sfu::uplink_estimate::EstimatorConfig;
use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::media::{
    Direction, Frequency, KeyframeRequestKind, MediaKind, MediaTime, Mid, Rid, Simulcast,
    SimulcastLayer,
};
use str0m::net::{Protocol, Receive};
use str0m::rtp::rtcp::Rtcp;
use str0m::rtp::{Extension, RawPacket};
use str0m::{Candidate, Event, Input, Output, Rtc, RtcConfig};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

/// A peer id in the shape an admission token carries (32 lower-case hex,
/// ADR-0078 section 2), unique within this test process.
fn fresh_peer_id() -> PeerId {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    PeerId::from_string(format!("{:032x}", NEXT.fetch_add(1, Ordering::Relaxed)))
}

/// Maximum wall-clock budget for the full ICE + DTLS
/// handshake. str0m's example chat.rs uses similar
/// numbers; on a loopback socket the real handshake
/// completes in ~200-400 ms but CI runners under load
/// can drag the tail well past a second.
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

/// One synthetic str0m client driven entirely from the
/// test. Owns its own `tokio::net::UdpSocket` on
/// loopback.
struct SyntheticClient {
    rtc: Rtc,
    socket: Arc<UdpSocket>,
    local_addr: SocketAddr,
    /// `true` once the str0m `Event::Connected` event
    /// fires — the canonical "ICE + DTLS handshake
    /// finished" signal in str0m's API.
    connected: bool,
    /// Frame descriptor observed on the most recent
    /// `Event::MediaData`, if the extension was negotiated
    /// and carried a value. Recorded by [`Self::drain`].
    last_frame_descriptor: Option<Vec<u8>>,
    /// Keyframe requests (PLI / FIR) the SFU has sent THIS
    /// client as a publisher, as surfaced by str0m's
    /// `Event::KeyframeRequest`. Counted by [`Self::drain`].
    keyframe_requests_seen: usize,
    /// Media datagrams this client put on the wire, in order,
    /// as their cleartext RTP header fields. Recorded by
    /// [`Self::drain`].
    rtp_out: Vec<RtpSeen>,
    /// Media datagrams that reached this client's socket from
    /// the SFU, in order of arrival — what the network handed
    /// the client, before its own `str0m` reorders, waits or
    /// assembles anything. Recorded by [`Self::feed_receive`].
    rtp_in: Vec<RtpSeen>,
    /// Outgoing media datagrams [`Self::drain`] threw away
    /// instead of sending, under the two rules below.
    rtp_dropped: Vec<RtpSeen>,
    /// Lose the next outgoing media datagram on this SSRC,
    /// once.
    drop_next_on: Option<u32>,
    /// Lose every outgoing media datagram on these SSRCs.
    drop_all_on: Vec<u32>,
    /// RTP packets a client in RTP mode received, as (payload type,
    /// payload), in order. Recorded by [`Self::drain`]; a frame-mode client
    /// never gets one.
    rtp_packets: Vec<(u8, Vec<u8>)>,
    /// The same packets with the header fields a video FEC test needs.
    rtp_got: Vec<RtpGot>,
    /// Lose the outgoing media datagram with this (SSRC, sequence number),
    /// once.
    drop_seq_on: Option<(u32, u16)>,
    /// The cleartext RTP headers (fixed header, CSRC list, header extension
    /// block) of the media datagrams this client produced, lost ones
    /// included, as (SSRC, sequence number, header bytes). SRTP leaves them
    /// readable on the wire.
    wire_headers: Vec<(u32, u16, Vec<u8>)>,
    /// NACKs this client received, one entry per sequence number named:
    /// (when, media SSRC, sequence number). Filled only for a client built
    /// with raw packets on.
    nacks_seen: Vec<(Instant, u32, u16)>,
    /// The RTP packets an RTP-mode video client received, whole and in the
    /// clear (fixed header, header extension block, payload, padding), as
    /// its `str0m` kept them (`DirectApi::keep_received_rtp`). A packet that
    /// arrived as a retransmission is not among them.
    wire_got: Vec<Vec<u8>>,
    /// Lose, on the way in, the media datagram with this sequence number on
    /// the first SSRC media arrives on, and every datagram on any other SSRC
    /// (the retransmission stream): a packet lost on the downlink for good.
    withhold_in: Option<u16>,
    /// The first SSRC media arrived on.
    main_in_ssrc: Option<u32>,
    /// Lose, on the way in, every n-th media datagram on the first SSRC
    /// media arrives on (first transmissions only; the retransmission
    /// stream gets through): packets a NACK then asks for again.
    lose_in_every: Option<u64>,
    /// Media datagrams seen on that SSRC so far, and how many were lost.
    main_in_seen: u64,
    main_in_lost: u64,
    /// Every packet that arrived on another SSRC (the retransmission
    /// stream), as `str0m` reports it with raw packets on: whether its
    /// header had the padding bit set, and its payload with any padding
    /// removed (the two bytes of the original sequence number, then the
    /// original payload).
    rtx_in: Vec<(bool, Vec<u8>)>,
    /// Datagrams lost under `withhold_in`: (SSRC, sequence number).
    withheld_in: Vec<(u32, u16)>,
    /// REMBs this client received: (when, bitrate in bits per second, the
    /// SSRCs named). Filled only for a client built with raw packets on.
    rembs_seen: Vec<(Instant, u64, Vec<u32>)>,
    /// Lose each outgoing media datagram (first transmissions and
    /// retransmissions alike) with a fixed probability while a time window
    /// is open. Used by the uplink-dip measurements only.
    dip: Option<DipLoss>,
    /// A mobile uplink's jitter on this client's outgoing media: from
    /// this moment on, twice a second, the delay climbs by 20 to 60 ms
    /// over a tenth of a second and falls back over the next. Nothing is
    /// lost and the rate is not limited.
    uplink_jitter_from: Option<Instant>,
    /// Outgoing media held by that jitter: when it goes on the wire, the
    /// datagram, where to.
    uplink_held: std::collections::VecDeque<(Instant, Vec<u8>, SocketAddr)>,
    /// The simulcast layer each keyframe request this client received as a
    /// publisher named, in order. One entry per request counted in
    /// `keyframe_requests_seen`.
    keyframe_request_rids: Vec<Option<Rid>>,
    /// The media line (this client's own mid) each of those named, entry
    /// for entry: what tells two plain senders of one publisher apart.
    keyframe_request_mids: Vec<Mid>,
    /// When each of those arrived, entry for entry.
    keyframe_request_at: Vec<Instant>,
    /// When each packet in `rtp_got` was handed over, entry for entry.
    rtp_got_at: Vec<Instant>,
    /// The media line (this client's own mid) each packet in `rtp_got`
    /// arrived on, entry for entry, resolved from the packet's SSRC: what
    /// tells two downstream tracks of one publisher apart.
    rtp_got_track: Vec<Option<Mid>>,
    /// The playout-delay header extension each packet in `rtp_got`
    /// carried, entry for entry, as (minimum, maximum) in milliseconds.
    /// `None` when the packet carried none or this client's session does
    /// not know the extension.
    rtp_got_playout: Vec<Option<(u64, u64)>>,
    /// Lose each incoming media datagram (first transmissions and
    /// retransmissions alike) with a fixed probability while a time window
    /// is open: a dip on this client's downlink.
    dip_in: Option<DipLoss>,
    /// Incoming media datagrams lost under `dip_in`, and how many arrived
    /// in all.
    dip_in_lost: Vec<RtpSeen>,
    /// The one-byte header extension ids seen on incoming media datagrams
    /// (SRTP leaves the header extension block readable).
    in_ext_ids: std::collections::BTreeSet<u8>,
}

/// The ids of the one-byte header extensions (RFC 8285) in the cleartext
/// header of an RTP datagram.
fn one_byte_extension_ids(datagram: &[u8]) -> Vec<u8> {
    let mut ids = Vec::new();
    let Some(&first) = datagram.first() else {
        return ids;
    };
    if first & 0x10 == 0 {
        return ids;
    }
    let start = 12 + 4 * usize::from(first & 0x0f);
    let Some(head) = datagram.get(start..start + 4) else {
        return ids;
    };
    if head[0] != 0xbe || head[1] != 0xde {
        return ids;
    }
    let len = 4 * usize::from(u16::from_be_bytes([head[2], head[3]]));
    let Some(block) = datagram.get(start + 4..start + 4 + len) else {
        return ids;
    };
    let mut i = 0;
    while i < block.len() {
        let b = block[i];
        if b == 0 {
            i += 1;
            continue;
        }
        let id = b >> 4;
        if id == 15 {
            break;
        }
        ids.push(id);
        i += 2 + usize::from(b & 0x0f);
    }
    ids
}

/// A window of random loss on a client's outgoing media datagrams.
struct DipLoss {
    from: Instant,
    until: Instant,
    /// Probability that one datagram is lost, in [0, 1].
    p: f64,
    /// State of a splitmix64 generator, so a run repeats.
    rng: u64,
}

impl DipLoss {
    /// The next uniform value in [0, 1).
    fn next_unit(&mut self) -> f64 {
        self.rng = self.rng.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        #[allow(clippy::cast_precision_loss)]
        let unit = (z >> 11) as f64 / (1u64 << 53) as f64;
        unit
    }

    /// Whether a datagram leaving at `at` is lost.
    fn loses(&mut self, at: Instant) -> bool {
        at >= self.from && at < self.until && self.next_unit() < self.p
    }
}

/// Length of the RTP headers of a datagram: fixed header, CSRC list and
/// header extension block.
fn rtp_headers_len(datagram: &[u8]) -> Option<usize> {
    let first = *datagram.first()?;
    let mut len = 12 + 4 * usize::from(first & 0x0f);
    if first & 0x10 != 0 {
        let words = datagram.get(len + 2..len + 4)?;
        len += 4 + 4 * usize::from(u16::from_be_bytes([words[0], words[1]]));
    }
    (datagram.len() >= len).then_some(len)
}

/// One RTP packet a client in RTP mode received, as its own `str0m` handed
/// it over (a retransmission is already rewritten as the packet it repairs).
#[derive(Debug, Clone, PartialEq, Eq)]
struct RtpGot {
    pt: u8,
    seq: u16,
    time: u32,
    marker: bool,
    /// The `mid` header extension, when the packet carried one.
    mid: Option<Mid>,
    payload: Vec<u8>,
}

/// The cleartext header of one SRTP media datagram, and when
/// the test saw it. SRTP encrypts the payload and leaves the
/// RTP header readable, which is what lets a test observe
/// sequence numbers on the wire without any key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RtpSeen {
    ssrc: u32,
    seq: u16,
    time: u32,
    marker: bool,
    at: Instant,
}

/// Parse a datagram as RTP media. `None` for STUN, DTLS and
/// RTCP, told apart the RFC 5761 way.
fn rtp_seen(datagram: &[u8]) -> Option<RtpSeen> {
    if datagram.len() < 12 || datagram[0] >> 6 != 2 || (192..=223).contains(&datagram[1]) {
        return None;
    }
    Some(RtpSeen {
        ssrc: u32::from_be_bytes([datagram[8], datagram[9], datagram[10], datagram[11]]),
        seq: u16::from_be_bytes([datagram[2], datagram[3]]),
        time: u32::from_be_bytes([datagram[4], datagram[5], datagram[6], datagram[7]]),
        marker: datagram[1] & 0x80 != 0,
        at: Instant::now(),
    })
}

impl SyntheticClient {
    async fn new() -> Self {
        Self::build(Rtc::builder()).await
    }

    /// A client whose session extension map carries the generic frame
    /// descriptor, so it can send the extension as a publisher and parse it
    /// as a subscriber. Registering it is what a real libwebrtc client does
    /// under the `WebRTC-GenericDescriptorAdvertised` field trial.
    async fn new_with_frame_descriptor() -> Self {
        Self::build(Rtc::builder().set_extension(
            GENERIC_FRAME_DESCRIPTOR_ID,
            generic_frame_descriptor_extension(),
        ))
        .await
    }

    /// A client that hands over an audio frame as soon as one complete
    /// frame has arrived behind a missing packet, instead of waiting for
    /// str0m's default of fifteen.
    ///
    /// The synthetic clients run str0m in frame mode, so a subscriber is
    /// itself a receiver that waits behind a gap. A test that asks what the
    /// **SFU** did with the packets after a loss has to keep that wait out
    /// of the way, or it measures its own harness.
    async fn new_releasing_audio_at_once() -> Self {
        Self::build(Rtc::builder().set_reordering_size_audio(1)).await
    }

    async fn build(builder: RtcConfig) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("UDP bind for synthetic client");
        let local_addr = socket.local_addr().expect("synthetic client local_addr");
        let mut rtc = builder.build(Instant::now());
        let candidate =
            Candidate::host(local_addr, Protocol::Udp).expect("host candidate constructs");
        rtc.add_local_candidate(candidate);
        Self {
            rtc,
            socket: Arc::new(socket),
            local_addr,
            connected: false,
            last_frame_descriptor: None,
            keyframe_requests_seen: 0,
            rtp_out: Vec::new(),
            rtp_in: Vec::new(),
            rtp_dropped: Vec::new(),
            drop_next_on: None,
            drop_all_on: Vec::new(),
            rtp_packets: Vec::new(),
            rtp_got: Vec::new(),
            drop_seq_on: None,
            wire_headers: Vec::new(),
            nacks_seen: Vec::new(),
            wire_got: Vec::new(),
            withhold_in: None,
            lose_in_every: None,
            main_in_seen: 0,
            main_in_lost: 0,
            rtx_in: Vec::new(),
            main_in_ssrc: None,
            withheld_in: Vec::new(),
            rembs_seen: Vec::new(),
            dip: None,
            uplink_jitter_from: None,
            uplink_held: std::collections::VecDeque::new(),
            keyframe_request_rids: Vec::new(),
            keyframe_request_mids: Vec::new(),
            keyframe_request_at: Vec::new(),
            rtp_got_at: Vec::new(),
            rtp_got_track: Vec::new(),
            rtp_got_playout: Vec::new(),
            dip_in: None,
            dip_in_lost: Vec::new(),
            in_ext_ids: std::collections::BTreeSet::new(),
        }
    }

    /// As [`Self::new_rtp_mode_video`], additionally surfacing the RTCP it
    /// receives, so a test can read the NACKs the SFU sends it.
    async fn new_rtp_mode_video_publisher(fec: bool) -> Self {
        let mut config = Rtc::builder().set_rtp_mode(true).enable_raw_packets(true);
        config.codec_config().enable_video_fec(fec);
        Self::build(config).await
    }

    /// Keep every received RTP packet whole, for `wire_got`.
    fn keeping_received(mut self) -> Self {
        self.rtc.direct_api().keep_received_rtp(true);
        self
    }

    /// A client in RTP mode that negotiates video FEC (`red/90000` with its
    /// RTX, and `ulpfec/90000`) the way Chrome and libwebrtc do when `fec` is
    /// set, under str0m's default payload types (VP8 96, red 116, ulpfec
    /// 118); without, it knows the media codecs alone.
    async fn new_rtp_mode_video(fec: bool) -> Self {
        let mut config = Rtc::builder().set_rtp_mode(true);
        config.codec_config().enable_video_fec(fec);
        Self::build(config).await.keeping_received()
    }

    /// A video FEC client that numbers its payload types differently from
    /// everyone else: VP8 100 (RTX 101), red 104 (RTX 105), ulpfec 106.
    async fn new_rtp_mode_video_fec_renumbered() -> Self {
        use str0m::format::{Codec, FormatParams};
        let mut config = Rtc::builder().set_rtp_mode(true);
        let codecs = config.codec_config();
        codecs.clear();
        codecs.enable_opus(true);
        for (pt, rtx, codec) in [
            (100, Some(101), Codec::Vp8),
            (104, Some(105), Codec::RedVideo),
            (106, None, Codec::Ulpfec),
        ] {
            codecs.add_config(
                pt.into(),
                rtx.map(Into::into),
                codec,
                Frequency::NINETY_KHZ,
                None,
                FormatParams::default(),
            );
        }
        Self::build(config).await.keeping_received()
    }

    /// Write one video RTP packet on `mid`. `false` until the send stream
    /// exists.
    fn write_video_rtp(
        &mut self,
        mid: Mid,
        pt: u8,
        seq: u64,
        rtp_time: u32,
        marker: bool,
        payload: &[u8],
    ) -> bool {
        let mut api = self.rtc.direct_api();
        let Some(stream) = api.stream_tx_by_mid(mid, None) else {
            return false;
        };
        stream
            .write_rtp(
                pt.into(),
                seq.into(),
                rtp_time,
                Instant::now(),
                marker,
                str0m::rtp::ExtensionValues::default(),
                true,
                payload.to_vec(),
            )
            .is_ok()
    }

    /// A client in RTP mode: it writes and receives whole RTP packets, which
    /// is the only way a str0m endpoint can put a chosen payload type on the
    /// wire. With `red` it negotiates redundant audio (RFC 2198) the way
    /// Chrome and libwebrtc do; without, it knows Opus alone.
    async fn new_rtp_mode(red: bool) -> Self {
        let mut config = Rtc::builder().set_rtp_mode(true);
        if red {
            config.codec_config().enable_red(true);
        }
        Self::build(config).await
    }

    /// Write one audio RTP packet on `mid` with payload type `pt`. `false`
    /// until the send stream exists.
    fn write_audio_rtp(&mut self, mid: Mid, pt: u8, seq: u64, payload: &[u8]) -> bool {
        self.write_audio_rtp_ext(
            mid,
            pt,
            seq,
            payload,
            str0m::rtp::ExtensionValues::default(),
        )
    }

    /// As [`Self::write_audio_rtp`], with header extension values of the
    /// caller's choosing. str0m puts on the wire only those whose extension
    /// this client believes the media line negotiated.
    fn write_audio_rtp_ext(
        &mut self,
        mid: Mid,
        pt: u8,
        seq: u64,
        payload: &[u8],
        ext_vals: str0m::rtp::ExtensionValues,
    ) -> bool {
        let mut api = self.rtc.direct_api();
        let Some(stream) = api.stream_tx_by_mid(mid, None) else {
            return false;
        };
        let rtp_time = u32::try_from(seq * 960).expect("a short test stream");
        stream
            .write_rtp(
                pt.into(),
                seq.into(),
                rtp_time,
                Instant::now(),
                false,
                ext_vals,
                false,
                payload.to_vec(),
            )
            .is_ok()
    }

    /// The main SSRC, and the retransmission SSRC if there is one, this
    /// client sends `mid` on.
    fn send_ssrcs(&mut self, mid: Mid) -> Option<(u32, Option<u32>)> {
        let mut api = self.rtc.direct_api();
        let stream = api.stream_tx_by_mid(mid, None)?;
        Some((*stream.ssrc(), stream.rtx().map(|rtx| *rtx)))
    }

    /// How much later than now an outgoing datagram goes on the wire
    /// under [`Self::uplink_jitter_from`]; `None` when it goes at once
    /// (no jitter set, not media, or nothing held and no delay due).
    fn uplink_jitter_extra(&self, datagram: &[u8]) -> Option<Duration> {
        let from = self.uplink_jitter_from?;
        rtp_seen(datagram)?;
        let since = Instant::now().checked_duration_since(from)?;
        let micros = u64::try_from(since.as_micros()).unwrap_or(u64::MAX);
        let (slot, into) = (micros / 500_000, micros % 500_000);
        let peak = 20_000 + (slot * 17 % 41) * 1_000;
        let extra = if into < 100_000 {
            peak * into / 100_000
        } else if into < 200_000 {
            peak * (200_000 - into) / 100_000
        } else {
            0
        };
        (extra > 0 || !self.uplink_held.is_empty()).then(|| Duration::from_micros(extra))
    }

    /// Whether an outgoing datagram goes on the wire. Media datagrams are
    /// logged either way; everything else (STUN, DTLS, RTCP) always goes.
    fn keep_outgoing(&mut self, datagram: &[u8]) -> bool {
        let Some(rtp) = rtp_seen(datagram) else {
            return true;
        };
        if let Some(len) = rtp_headers_len(datagram) {
            self.wire_headers
                .push((rtp.ssrc, rtp.seq, datagram[..len].to_vec()));
        }
        let lose_once = self.drop_next_on == Some(rtp.ssrc);
        if lose_once {
            self.drop_next_on = None;
        }
        let lose_seq = self.drop_seq_on == Some((rtp.ssrc, rtp.seq));
        if lose_seq {
            self.drop_seq_on = None;
        }
        let lose_dip = self.dip.as_mut().is_some_and(|d| d.loses(rtp.at));
        if lose_once || lose_seq || lose_dip || self.drop_all_on.contains(&rtp.ssrc) {
            self.rtp_dropped.push(rtp);
            return false;
        }
        self.rtp_out.push(rtp);
        true
    }

    /// Send a PLI on one of this (subscribing) client's receive streams, the
    /// way a decoder with no reference frame does. `false` if str0m has no
    /// receive stream for `mid` yet — one exists once the first packet has
    /// arrived — so a caller sends only after media has started flowing.
    fn send_pli(&mut self, mid: Mid) -> bool {
        self.rtc
            .direct_api()
            .stream_rx_by_mid(mid, None)
            .is_some_and(|s| {
                s.request_keyframe(KeyframeRequestKind::Pli);
                true
            })
    }

    /// Build a plain (non-simulcast) SendOnly video publisher offer. Video
    /// because the frame descriptor is a video extension — an audio media
    /// line never carries it.
    fn create_video_publisher_offer(&mut self) -> (String, SdpPendingOffer, Mid) {
        let mut change = self.rtc.sdp_api();
        let mid = change.add_media(
            MediaKind::Video,
            Direction::SendOnly,
            Some("frame-descriptor-publisher".to_owned()),
            None,
            None,
        );
        let (offer, pending) = change.apply().expect("at least one change → Some(offer)");
        (offer.to_sdp_string(), pending, mid)
    }

    /// Build a RecvOnly **video** subscriber offer, so the SFU has a video
    /// media line to renegotiate a downstream track onto.
    fn create_video_subscriber_offer(&mut self) -> (String, SdpPendingOffer) {
        let mut change = self.rtc.sdp_api();
        let _mid = change.add_media(
            MediaKind::Video,
            Direction::RecvOnly,
            Some("frame-descriptor-subscriber".to_owned()),
            None,
            None,
        );
        let (offer, pending) = change.apply().expect("at least one change → Some(offer)");
        (offer.to_sdp_string(), pending)
    }

    /// Write one video frame carrying a frame descriptor, the way a
    /// libwebrtc publisher would.
    fn write_video_frame_with_descriptor(
        &mut self,
        mid: Mid,
        payload: &[u8],
        descriptor: &[u8],
        frame_idx: u64,
    ) -> bool {
        let Some(pt) = self.rtc.writer(mid).and_then(|w| {
            w.payload_params()
                .next()
                .map(str0m::format::PayloadParams::pt)
        }) else {
            return false;
        };
        let Some(writer) = self.rtc.writer(mid) else {
            return false;
        };
        let rtp_time = MediaTime::new(frame_idx * 3000, Frequency::NINETY_KHZ);
        writer
            .user_extension_value(FrameDescriptor(descriptor.to_vec()))
            .write(pt, Instant::now(), rtp_time, payload.to_vec())
            .is_ok()
    }

    /// Build the offer the SFU's `accept_offer` consumes.
    /// `Direction::SendOnly` because the synthetic client
    /// is acting as a publisher in this test — the SFU
    /// receives the audio it would forward. Returns the
    /// allocated publisher `Mid` alongside the SDP +
    /// pending so the caller can write media on it after
    /// the handshake completes.
    fn create_audio_publisher_offer(&mut self) -> (String, SdpPendingOffer, Mid) {
        let mut change = self.rtc.sdp_api();
        let mid = change.add_media(
            MediaKind::Audio,
            Direction::SendOnly,
            Some("synthetic-publisher".to_owned()),
            None,
            None,
        );
        let (offer, pending) = change.apply().expect("at least one change → Some(offer)");
        (offer.to_sdp_string(), pending, mid)
    }

    /// Build a pure-subscriber initial offer —
    /// `Direction::RecvOnly`. Establishes the ICE + DTLS
    /// transport (and creates the SFU-side `MediaPeer`)
    /// without announcing any upstream track, so the SFU
    /// never renegotiates *back* toward this peer. The
    /// SFU's own SFU-initiated renegotiation (S-3-B-tracks
    /// step 2) adds a *separate* `SendOnly` line later,
    /// which this client receives on.
    ///
    /// This mirrors str0m's own `unidirectional-r-create-
    /// media` integration test, which proves a RecvOnly
    /// offerer reaches `Connected` against a sending
    /// answerer.
    fn create_subscriber_offer(&mut self) -> (String, SdpPendingOffer) {
        let mut change = self.rtc.sdp_api();
        let _mid = change.add_media(
            MediaKind::Audio,
            Direction::RecvOnly,
            Some("synthetic-subscriber".to_owned()),
            None,
            None,
        );
        let (offer, pending) = change.apply().expect("at least one change → Some(offer)");
        (offer.to_sdp_string(), pending)
    }

    /// Answer an SFU-initiated `ServerFrame::SdpOffer`
    /// (the renegotiation that adds a downstream track).
    /// Returns the answer SDP the test ships back via
    /// [`MediaTaskHandle::accept_renegotiation_answer`].
    fn answer_renegotiation(&mut self, sdp: &str) -> Result<String, String> {
        let offer =
            SdpOffer::from_sdp_string(sdp).map_err(|e| format!("parse reneg offer: {e}"))?;
        let answer = self
            .rtc
            .sdp_api()
            .accept_offer(offer)
            .map_err(|e| format!("accept reneg offer: {e}"))?;
        Ok(answer.to_sdp_string())
    }

    /// Write one audio frame on `mid`. Picks the first
    /// negotiated payload type str0m reports for the
    /// writer. No-op (returns `false`) if the writer or a
    /// usable PT is not yet available, or the write
    /// fails — the caller retries on later ticks.
    ///
    /// `frame_idx` advances the RTP timestamp by one 20 ms
    /// Opus frame (960 samples @ 48 kHz) per call so the
    /// receiver's depacketiser sees a monotonic stream.
    fn write_audio_frame(&mut self, mid: Mid, payload: &[u8], frame_idx: u64) -> bool {
        let Some(writer) = self.rtc.writer(mid) else {
            return false;
        };
        let Some(pt) = writer
            .payload_params()
            .next()
            .map(str0m::format::PayloadParams::pt)
        else {
            return false;
        };
        let wallclock = Instant::now();
        let rtp_time = MediaTime::new(frame_idx * 960, Frequency::FORTY_EIGHT_KHZ);
        writer
            .write(pt, wallclock, rtp_time, payload.to_vec())
            .is_ok()
    }

    /// Build a SendOnly offer adding **two plain video senders**, each one
    /// encoding with no rid, as a client that sends its camera twice (the
    /// picture itself and a light copy) does; with `audio`, an audio
    /// sender ahead of them. Returns the mids: audio, main video, light
    /// video.
    fn create_two_sender_publisher_offer(
        &mut self,
        audio: bool,
    ) -> (String, SdpPendingOffer, Option<Mid>, Mid, Mid) {
        let mut change = self.rtc.sdp_api();
        let mut add = |kind, stream: &str| {
            change.add_media(
                kind,
                Direction::SendOnly,
                Some(stream.to_owned()),
                None,
                None,
            )
        };
        let audio_mid = audio.then(|| add(MediaKind::Audio, "two-sender-publisher"));
        let main = add(MediaKind::Video, "two-sender-publisher");
        let light = add(MediaKind::Video, "two-sender-publisher-light");
        let (offer, pending) = change.apply().expect("at least one change → Some(offer)");
        (offer.to_sdp_string(), pending, audio_mid, main, light)
    }

    /// Build a SendOnly offer adding **three plain video senders**, each one
    /// encoding with no rid, as a client that sends its camera three times
    /// (the picture itself, a light copy and a middle copy, ADR-0070 step
    /// 5) does. Returns the mids: main, light, middle.
    fn create_three_sender_publisher_offer(&mut self) -> (String, SdpPendingOffer, Mid, Mid, Mid) {
        let mut change = self.rtc.sdp_api();
        let mut add = |stream: &str| {
            change.add_media(
                MediaKind::Video,
                Direction::SendOnly,
                Some(stream.to_owned()),
                None,
                None,
            )
        };
        let main = add("three-sender-publisher");
        let light = add("three-sender-publisher-light");
        let middle = add("three-sender-publisher-middle");
        let (offer, pending) = change.apply().expect("at least one change → Some(offer)");
        (offer.to_sdp_string(), pending, main, light, middle)
    }

    /// Build a SendOnly **video simulcast** publisher offer announcing the given
    /// `rids` (e.g. `["l", "h"]`). str0m sends each layer on its own SSRC; the
    /// SFU resolves each packet's stream to its rid, which is exactly what
    /// the simulcast layer gate keys on.
    fn create_video_simulcast_publisher_offer(
        &mut self,
        rids: &[&str],
    ) -> (String, SdpPendingOffer, Mid) {
        let simulcast = Simulcast {
            send: rids
                .iter()
                .map(|r| SimulcastLayer {
                    rid: Rid::from(*r),
                    attributes: None,
                })
                .collect(),
            recv: Vec::new(),
        };
        let mut change = self.rtc.sdp_api();
        let mid = change.add_media(
            MediaKind::Video,
            Direction::SendOnly,
            Some("simulcast-publisher".to_owned()),
            None,
            Some(simulcast),
        );
        let (offer, pending) = change.apply().expect("at least one change → Some(offer)");
        (offer.to_sdp_string(), pending, mid)
    }

    /// Write one video frame on a specific simulcast layer `rid`. Picks the
    /// first negotiated video payload type. No-op (`false`) if the writer / PT
    /// is not ready or the rid is not negotiated — the caller retries.
    fn write_video_frame_on_rid(
        &mut self,
        mid: Mid,
        rid: &str,
        payload: &[u8],
        frame_idx: u64,
    ) -> bool {
        let Some(pt) = self.rtc.writer(mid).and_then(|w| {
            w.payload_params()
                .next()
                .map(str0m::format::PayloadParams::pt)
        }) else {
            return false;
        };
        let Some(writer) = self.rtc.writer(mid) else {
            return false;
        };
        // 90 kHz video clock; advance ~one 30 fps frame (3000 ticks) per call.
        let rtp_time = MediaTime::new(frame_idx * 3000, Frequency::NINETY_KHZ);
        writer
            .rid(Rid::from(rid))
            .write(pt, Instant::now(), rtp_time, payload.to_vec())
            .is_ok()
    }

    /// Drain every pending `poll_output`. Sends any
    /// `Output::Transmit` out the socket (unless a loss rule
    /// set on this client says to lose it), flips
    /// `connected` on `Event::Connected`, and returns the
    /// payload bytes of every `Event::MediaData` observed
    /// this pass.
    #[allow(clippy::too_many_lines)]
    async fn drain(&mut self) -> Vec<Vec<u8>> {
        // Clone the Arc so we can `send_to` (shared borrow
        // of the socket) while `poll_output` holds a mut
        // borrow of `self.rtc`.
        let socket = self.socket.clone();
        let mut payloads = Vec::new();
        while self
            .uplink_held
            .front()
            .is_some_and(|(at, _, _)| *at <= Instant::now())
        {
            if let Some((_, datagram, to)) = self.uplink_held.pop_front() {
                let _ = socket.send_to(&datagram, to).await;
            }
        }
        loop {
            match self.rtc.poll_output() {
                Ok(Output::Transmit(t)) => {
                    if self.keep_outgoing(&t.contents) {
                        match self.uplink_jitter_extra(&t.contents) {
                            Some(extra) => {
                                // In order: never before the one held
                                // ahead of it.
                                let at = (Instant::now() + extra).max(
                                    self.uplink_held
                                        .back()
                                        .map_or_else(Instant::now, |(at, _, _)| *at),
                                );
                                self.uplink_held.push_back((
                                    at,
                                    t.contents.to_vec(),
                                    t.destination,
                                ));
                            },
                            None => {
                                let _ = socket.send_to(&t.contents, t.destination).await;
                            },
                        }
                    }
                },
                Ok(Output::Event(event)) => match event {
                    Event::Connected => self.connected = true,
                    Event::MediaData(data) => {
                        if let Some(d) = data.ext_vals.user_values.get::<FrameDescriptor>() {
                            self.last_frame_descriptor = Some(d.0.clone());
                        }
                        payloads.push(data.data.clone());
                    },
                    Event::KeyframeRequest(req) => {
                        self.keyframe_requests_seen += 1;
                        self.keyframe_request_rids.push(req.rid);
                        self.keyframe_request_mids.push(req.mid);
                        self.keyframe_request_at.push(Instant::now());
                    },
                    Event::RawPacket(raw) => {
                        if let RawPacket::RtcpRx(Rtcp::Remb(remb)) = &*raw {
                            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                            let bps = remb.bitrate.max(0.0) as u64;
                            self.rembs_seen
                                .push((Instant::now(), bps, remb.ssrcs.clone()));
                        }
                        if let RawPacket::RtpRx(header, data) = &*raw {
                            if self.main_in_ssrc.is_some_and(|main| main != *header.ssrc) {
                                self.rtx_in.push((header.has_padding, data.clone()));
                            }
                        }
                        if let RawPacket::RtcpRx(Rtcp::Nack(nack)) = *raw {
                            let now = Instant::now();
                            for entry in &nack.reports {
                                self.nacks_seen.push((now, *nack.ssrc, entry.pid));
                                for bit in 0..16u16 {
                                    if entry.blp & (1 << bit) != 0 {
                                        self.nacks_seen.push((
                                            now,
                                            *nack.ssrc,
                                            entry.pid.wrapping_add(bit + 1),
                                        ));
                                    }
                                }
                            }
                        }
                    },
                    Event::RtpPacket(packet) => {
                        if let Some(whole) = self.rtc.direct_api().take_received_rtp() {
                            self.wire_got.push(whole);
                        }
                        self.rtp_got_at.push(Instant::now());
                        let track = self
                            .rtc
                            .direct_api()
                            .stream_rx(&packet.header.ssrc)
                            .map(|stream| stream.mid());
                        self.rtp_got_track.push(track);
                        let ext = &packet.header.ext_vals;
                        self.rtp_got_playout
                            .push(
                                ext.play_delay_min
                                    .zip(ext.play_delay_max)
                                    .map(|(min, max)| {
                                        let ms = |t: str0m::media::MediaTime| {
                                            u64::try_from(Duration::from(t).as_millis())
                                                .expect("a short delay")
                                        };
                                        (ms(min), ms(max))
                                    }),
                            );
                        self.rtp_got.push(RtpGot {
                            pt: *packet.header.payload_type,
                            seq: packet.header.sequence_number,
                            time: packet.header.timestamp,
                            marker: packet.header.marker,
                            mid: packet.header.ext_vals.mid,
                            payload: packet.payload.clone(),
                        });
                        self.rtp_packets
                            .push((*packet.header.payload_type, packet.payload));
                    },
                    _ => {},
                },
                // A pending timeout means there is nothing more
                // to drain this pass; an error means the Rtc is
                // wedged. Either way, stop draining.
                Ok(Output::Timeout(_)) | Err(_) => break,
            }
        }
        payloads
    }

    /// As [`Self::drain`], but nothing is put on the wire: every pending
    /// `Output::Transmit` is discarded. Used right after a write to lose
    /// exactly that frame's packet on the way to the SFU.
    ///
    /// Returns how many of the discarded datagrams were media packets (RTP,
    /// as opposed to RTCP or STUN, told apart the RFC 5761 way). str0m
    /// queues a written frame and puts it on the wire on a later timeout
    /// tick, so one call right after the write may well discard none.
    fn discard_pending_transmits(&mut self) -> usize {
        let mut discarded = 0;
        loop {
            match self.rtc.poll_output() {
                Ok(Output::Transmit(t)) => {
                    let b: &[u8] = &t.contents;
                    if b.len() > 1 && b[0] >> 6 == 2 && !(192..=223).contains(&b[1]) {
                        discarded += 1;
                    }
                },
                Ok(Output::Event(Event::Connected)) => self.connected = true,
                Ok(Output::Event(_)) => {},
                Ok(Output::Timeout(_)) | Err(_) => break,
            }
        }
        discarded
    }

    /// Feed an inbound datagram (if it came from the SFU).
    fn feed_receive(&mut self, bytes: &[u8], source: SocketAddr, sfu_addr: SocketAddr) {
        self.feed_receive_at(bytes, source, sfu_addr, Instant::now());
    }

    /// As [`Self::feed_receive`], with the moment the datagram arrived
    /// given: what a link model between the socket and the client says,
    /// which is what the client's transport-wide feedback then reports.
    fn feed_receive_at(
        &mut self,
        bytes: &[u8],
        source: SocketAddr,
        sfu_addr: SocketAddr,
        at: Instant,
    ) {
        if source != sfu_addr {
            return;
        }
        if let Some(mut rtp) = rtp_seen(bytes) {
            rtp.at = at;
            if let Some(withheld) = self.withhold_in {
                let main = *self.main_in_ssrc.get_or_insert(rtp.ssrc);
                if rtp.ssrc != main || rtp.seq == withheld {
                    self.withheld_in.push((rtp.ssrc, rtp.seq));
                    return;
                }
            }
            if let Some(every) = self.lose_in_every {
                let main = *self.main_in_ssrc.get_or_insert(rtp.ssrc);
                if rtp.ssrc == main {
                    self.main_in_seen += 1;
                    if self.main_in_seen % every == 0 {
                        self.main_in_lost += 1;
                        return;
                    }
                }
            }
            if self.dip_in.as_mut().is_some_and(|d| d.loses(rtp.at)) {
                self.dip_in_lost.push(rtp);
                return;
            }
            self.in_ext_ids.extend(one_byte_extension_ids(bytes));
            self.rtp_in.push(rtp);
        }
        let Ok(contents) = bytes.try_into() else {
            return;
        };
        let input = Input::Receive(
            at,
            Receive {
                proto: Protocol::Udp,
                source,
                destination: self.local_addr,
                contents,
            },
        );
        let _ = self.rtc.handle_input(input);
    }

    /// Drive the str0m timeout branch.
    fn feed_timeout(&mut self) {
        let _ = self.rtc.handle_input(Input::Timeout(Instant::now()));
    }

    fn accept_answer(&mut self, pending: SdpPendingOffer, sdp: &str) -> Result<(), String> {
        let answer = SdpAnswer::from_sdp_string(sdp).map_err(|e| format!("parse answer: {e}"))?;
        self.rtc
            .sdp_api()
            .accept_answer(pending, answer)
            .map_err(|e| format!("accept_answer: {e}"))
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    fn is_alive(&self) -> bool {
        self.rtc.is_alive()
    }
}

/// Drive a single synthetic client + the SFU's media
/// task until the client reports
/// `IceConnectionState::Connected`. Returns `true` on
/// success, `false` on timeout / disconnect.
///
/// The function is deliberately one big loop rather than
/// abstracted helpers — keeping the control flow visible
/// in one place makes it easier to debug a stuck
/// handshake from a CI log.
async fn drive_until_connected(client: &mut SyntheticClient, sfu_addr: SocketAddr) -> bool {
    let mut buf = vec![0u8; 2048];
    let deadline = Instant::now() + HANDSHAKE_DEADLINE;

    loop {
        if !client.is_alive() {
            return false;
        }
        if client.is_connected() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }

        // Drain every available Output before reading
        // the socket — str0m's event loop expects you to
        // pump until Timeout, otherwise the queue
        // builds up.
        let mut next_timeout = deadline;
        loop {
            match client.rtc.poll_output() {
                Ok(Output::Transmit(t)) => {
                    let _ = client.socket.send_to(&t.contents, t.destination).await;
                },
                Ok(Output::Timeout(at)) => {
                    if at < next_timeout {
                        next_timeout = at;
                    }
                    break;
                },
                Ok(Output::Event(event)) => {
                    // `Event::Connected` is the
                    // canonical "ICE + DTLS finished"
                    // signal — record it so the
                    // outer-loop predicate flips.
                    if matches!(event, Event::Connected) {
                        client.connected = true;
                    }
                },
                Err(_) => {
                    return false;
                },
            }
        }

        // Wait for either an inbound UDP packet or the
        // soonest pending str0m timeout, whichever
        // comes first.
        let sleep_for = next_timeout
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(100));

        let recv = tokio::time::timeout(sleep_for, client.socket.recv_from(&mut buf)).await;
        let now = Instant::now();
        match recv {
            Ok(Ok((n, source))) => {
                if source != sfu_addr {
                    // Loopback traffic shouldn't carry
                    // anything else, but defend.
                    continue;
                }
                let Ok(contents) = (&buf[..n]).try_into() else {
                    continue;
                };
                let input = Input::Receive(
                    now,
                    Receive {
                        proto: Protocol::Udp,
                        source,
                        destination: client.local_addr,
                        contents,
                    },
                );
                if let Err(_e) = client.rtc.handle_input(input) {
                    return false;
                }
            },
            Ok(Err(_)) => return false,
            Err(_) => {
                // sleep timed out — fire the str0m
                // timeout branch.
                if let Err(_e) = client.rtc.handle_input(Input::Timeout(now)) {
                    return false;
                }
            },
        }
    }
}

/// **S-3-B-tracks step 3** — full ICE + DTLS handshake
/// between a synthetic str0m client and the production
/// media task over UDP loopback.
///
/// The test:
///
/// 1. Spawns `media::spawn_media_task` on
///    `127.0.0.1:0` with a fresh [`PeerRegistry`].
/// 2. Builds a [`SyntheticClient`] on its own UDP
///    socket + a host candidate pointing at that socket.
/// 3. Hands the client's SDP offer to the SFU via
///    `MediaTaskHandle::accept_offer`, applies the
///    answer back on the synthetic Rtc.
/// 4. Pumps UDP loopback until the synthetic Rtc
///    reports `IceConnectionState::Connected`.
///
/// On success the test asserts both `is_alive` and the
/// connected state. On timeout it fails with the
/// observed final state so a stuck CI run produces an
/// actionable error.
#[tokio::test]
async fn synthetic_client_completes_handshake_with_media_task() {
    // 1. Spawn the production media task on loopback.
    let peers = PeerRegistry::new();
    let (media, sfu_addr) = spawn_media_task("127.0.0.1:0".parse().unwrap(), peers)
        .await
        .expect("UDP bind on loopback:0 succeeds");

    // 2. Synthetic client on its own UDP socket.
    let mut client = SyntheticClient::new().await;
    let client_id = fresh_peer_id();

    // 3. Offer / answer exchange.
    let (offer_sdp, pending, _mid) = client.create_audio_publisher_offer();
    let answer_sdp = media
        .accept_offer(
            client_id.clone(),
            "handshake-test-call".to_owned(),
            offer_sdp,
        )
        .await
        .expect("SFU accepts a real str0m-produced SDP offer");
    client
        .accept_answer(pending, &answer_sdp)
        .expect("client applies the SFU's SDP answer");

    // 4. Pump until connected.
    let connected = drive_until_connected(&mut client, sfu_addr).await;
    assert!(
        connected,
        "synthetic client failed to reach ICE Connected within {HANDSHAKE_DEADLINE:?}; \
         final state: alive={} ice={:?}",
        client.is_alive(),
        client.is_connected(),
    );
    assert!(client.is_alive(), "Rtc must stay alive after handshake");
}

/// The production bind shape: the media socket binds
/// every interface (`0.0.0.0:0`) and advertises
/// `127.0.0.1` as its ICE host candidate. The synthetic
/// client must complete ICE + DTLS against it.
///
/// Two independent defects are pinned here, and each makes
/// this test fail on its own:
///
/// 1. Building the host candidate from the socket's local
///    address: str0m refuses `0.0.0.0` as a host candidate,
///    so the SDP answer carries no `a=candidate:` line and
///    the client has nothing to dial.
/// 2. Handing str0m the socket's local address
///    (`0.0.0.0:port`) as each inbound datagram's
///    destination: str0m's ICE agent drops a STUN binding
///    request whose destination matches no local candidate,
///    so the client's checks are discarded and ICE never
///    nominates a pair even though the answer advertised
///    the right address.
#[tokio::test]
async fn unspecified_bind_with_advertise_ip_completes_handshake() {
    let peers = PeerRegistry::new();
    let (media, sfu_addr) = spawn_media_task_advertising(
        "0.0.0.0:0".parse().unwrap(),
        Some("127.0.0.1".parse().unwrap()),
        peers,
        None,
    )
    .await
    .expect("unspecified bind with an advertise IP starts");
    assert!(
        sfu_addr.ip().is_loopback(),
        "the returned address is the advertised one, got {sfu_addr}"
    );

    let mut client = SyntheticClient::new().await;
    let client_id = fresh_peer_id();

    let (offer_sdp, pending, _mid) = client.create_audio_publisher_offer();
    let answer_sdp = media
        .accept_offer(
            client_id.clone(),
            "unspecified-bind-test-call".to_owned(),
            offer_sdp,
        )
        .await
        .expect("SFU accepts a real str0m-produced SDP offer");
    let expected_candidate = format!("127.0.0.1 {} typ host", sfu_addr.port());
    assert!(
        answer_sdp
            .lines()
            .any(|l| l.starts_with("a=candidate:") && l.contains(&expected_candidate)),
        "SDP answer must advertise the host candidate {sfu_addr}; answer was:\n{answer_sdp}"
    );
    client
        .accept_answer(pending, &answer_sdp)
        .expect("client applies the SFU's SDP answer");

    let connected = drive_until_connected(&mut client, sfu_addr).await;
    assert!(
        connected,
        "synthetic client failed to reach ICE Connected against a 0.0.0.0-bound SFU \
         advertising {sfu_addr} within {HANDSHAKE_DEADLINE:?}; alive={}",
        client.is_alive(),
    );
}

/// An unspecified bind WITHOUT an advertise IP refuses to
/// start — the media plane never comes up in a shape whose
/// answers would carry no SFU candidate.
#[tokio::test]
async fn unspecified_bind_without_advertise_ip_refuses_to_start() {
    let result = spawn_media_task("0.0.0.0:0".parse().unwrap(), PeerRegistry::new()).await;
    assert!(
        result.is_err(),
        "an unspecified media bind with nothing to advertise must not start"
    );
}

/// Wall-clock budget for the full two-client cross-fanout
/// flow: two handshakes + one SFU-initiated renegotiation
/// round-trip + media forwarding. Generous so a loaded CI
/// runner does not flake on the tail.
const CROSS_FANOUT_DEADLINE: Duration = Duration::from_secs(20);

/// Final state of a [`run_cross_fanout`] drive. Each field
/// is surfaced in the test's failure message so a stuck CI
/// run says *where* it stalled (handshake vs renegotiation
/// vs forwarding). The four independent booleans are a
/// progress checklist, not a state enum — a struct of bools
/// is the clearest shape for that.
#[allow(clippy::struct_excessive_bools)]
struct FanoutOutcome {
    a_connected: bool,
    b_connected: bool,
    reneg_answered: bool,
    received_expected: bool,
}

/// Drive publisher A + subscriber B + the SFU media task
/// concurrently until B receives `expected_payload` or the
/// deadline elapses.
///
/// Per loop iteration:
/// 1. Drain both clients' `poll_output` (sends queued
///    transmits, records `Connected`, collects B's
///    `MediaData` payloads).
/// 2. Short-circuit to success if any B payload matches.
/// 3. Pull any `ServerFrame::SdpOffer` the SFU dispatched
///    to B's outbound channel; once B's transport is up,
///    answer it and route the answer back through
///    [`MediaTaskHandle::accept_renegotiation_answer`].
/// 4. Once both peers are connected and the downstream
///    track is negotiated, A writes one audio frame
///    (repeated each tick so a pre-`Open` drop is followed
///    by a landing frame).
/// 5. `select!` on either client's socket or a 5 ms tick,
///    then feed the right state machine.
#[allow(clippy::too_many_arguments)]
async fn run_cross_fanout(
    a: &mut SyntheticClient,
    b: &mut SyntheticClient,
    sfu_addr: SocketAddr,
    handle: &MediaTaskHandle,
    b_id: PeerId,
    b_outbound_rx: &mut mpsc::Receiver<ServerFrame>,
    a_publisher_mid: Mid,
    expected_payload: &[u8],
) -> FanoutOutcome {
    let deadline = Instant::now() + CROSS_FANOUT_DEADLINE;
    let mut reneg_answered = false;
    let mut pending_reneg: Option<String> = None;
    let mut frame_idx: u64 = 0;
    let mut received_expected = false;

    while Instant::now() < deadline {
        // 1 + 2. Pump both sides; B's received payloads.
        let _ = a.drain().await;
        let b_payloads = b.drain().await;
        if b_payloads.iter().any(|p| p.as_slice() == expected_payload) {
            received_expected = true;
            break;
        }

        // 3. Capture the SFU-initiated renegotiation offer.
        while let Ok(frame) = b_outbound_rx.try_recv() {
            if let ServerFrame::SdpOffer { sdp } = frame {
                pending_reneg = Some(sdp);
            }
        }
        if !reneg_answered && b.is_connected() {
            if let Some(sdp) = pending_reneg.take() {
                if let Ok(answer) = b.answer_renegotiation(&sdp) {
                    if handle
                        .accept_renegotiation_answer(b_id.clone(), answer)
                        .await
                        .is_ok()
                    {
                        reneg_answered = true;
                    }
                }
            }
        }

        // 4. Publish from A once the downstream track exists.
        if a.is_connected() && b.is_connected() && reneg_answered {
            a.write_audio_frame(a_publisher_mid, expected_payload, frame_idx);
            frame_idx += 1;
        }

        // 5. Wait for socket activity or a short tick.
        let a_sock = a.socket.clone();
        let b_sock = b.socket.clone();
        let mut abuf = [0u8; 2048];
        let mut bbuf = [0u8; 2048];
        tokio::select! {
            r = a_sock.recv_from(&mut abuf) => {
                if let Ok((n, src)) = r {
                    a.feed_receive(&abuf[..n], src, sfu_addr);
                }
            }
            r = b_sock.recv_from(&mut bbuf) => {
                if let Ok((n, src)) = r {
                    b.feed_receive(&bbuf[..n], src, sfu_addr);
                }
            }
            () = tokio::time::sleep(Duration::from_millis(5)) => {
                a.feed_timeout();
                b.feed_timeout();
            }
        }
    }

    FanoutOutcome {
        a_connected: a.is_connected(),
        b_connected: b.is_connected(),
        reneg_answered,
        received_expected,
    }
}

/// **S-3-B-tracks step 3b** — two synthetic clients +
/// the production media task: a publisher A's audio frame
/// is forwarded verbatim to a subscriber B through the
/// SFU's RTP fanout, exercising the full SFU-initiated
/// renegotiation wire end-to-end.
///
/// The test:
///
/// 1. Spawns `media::spawn_media_task` on loopback with a
///    [`PeerRegistry`] in which subscriber B is registered
///    with a **test-owned** outbound channel — the same
///    channel `drive_renegotiations` ships a
///    `ServerFrame::SdpOffer` down.
/// 2. Establishes B (RecvOnly subscriber) and A (SendOnly
///    publisher) through `MediaTaskHandle::accept_offer`.
/// 3. Drives both to ICE + DTLS `Connected`; intercepts the
///    SFU-initiated renegotiation offer for B; answers it
///    via `accept_renegotiation_answer`.
/// 4. Writes a distinctive 80-byte payload from A and
///    asserts the **exact same bytes** surface as an
///    `Event::MediaData` at B.
///
/// A pass means the SFU received A's RTP, resolved B's
/// downstream track via the `tracks_out` map, and forwarded
/// the opaque payload to B without mutating it — the
/// integration-side proof of audit-pack invariant **C-39**
/// (the SFU forwards SFrame-encrypted bytes verbatim and
/// never inspects them).
#[tokio::test]
async fn two_clients_cross_fanout_forwards_media_verbatim() {
    // 1. Media task + registry. B is registered with a
    //    test-owned outbound channel so we can intercept
    //    the SFU-initiated SdpOffer. A does not need a
    //    registry entry — it never receives a downstream
    //    track in this test (B is a pure subscriber), so
    //    the SFU never renegotiates toward A.
    let peers = PeerRegistry::new();
    let b_id = fresh_peer_id();
    let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
    let registered = peers
        .register(Peer {
            id: b_id.clone(),
            room: "cross-fanout-test".to_owned(),
            outbound: Some(b_tx),
        })
        .await;
    assert!(registered, "fresh PeerId must register cleanly");

    let (media, sfu_addr) = spawn_media_task("127.0.0.1:0".parse().unwrap(), peers)
        .await
        .expect("UDP bind on loopback:0 succeeds");

    // 2. Two synthetic clients on their own UDP sockets.
    let mut a = SyntheticClient::new().await;
    let mut b = SyntheticClient::new().await;
    let a_id = fresh_peer_id();

    // B subscribes first (RecvOnly) — establishes the
    // SFU-side MediaPeer + transport without announcing an
    // upstream track.
    let (b_offer, b_pending) = b.create_subscriber_offer();
    let b_answer = media
        .accept_offer(b_id.clone(), "fanout-test-call".to_owned(), b_offer)
        .await
        .expect("SFU accepts B's RecvOnly subscriber offer");
    b.accept_answer(b_pending, &b_answer)
        .expect("B applies the SFU's answer");

    // A joins as a publisher (SendOnly audio).
    let (a_offer, a_pending, publisher_mid) = a.create_audio_publisher_offer();
    let a_answer = media
        .accept_offer(a_id.clone(), "fanout-test-call".to_owned(), a_offer)
        .await
        .expect("SFU accepts A's SendOnly publisher offer");
    a.accept_answer(a_pending, &a_answer)
        .expect("A applies the SFU's answer");

    // 3 + 4. Distinctive payload that will not appear by
    // accident in DTLS / STUN / SRTP framing. Iterating
    // over `u8` directly keeps the byte pattern lossless
    // (no truncating cast).
    let expected_payload: Vec<u8> = (0u8..80)
        .map(|i| i.wrapping_mul(7).wrapping_add(3))
        .collect();

    let outcome = run_cross_fanout(
        &mut a,
        &mut b,
        sfu_addr,
        &media,
        b_id.clone(),
        &mut b_rx,
        publisher_mid,
        &expected_payload,
    )
    .await;

    assert!(
        outcome.received_expected,
        "subscriber B never received A's media frame verbatim within \
         {CROSS_FANOUT_DEADLINE:?}; final state: a_connected={} b_connected={} \
         reneg_answered={} a_alive={} b_alive={}",
        outcome.a_connected,
        outcome.b_connected,
        outcome.reneg_answered,
        a.is_alive(),
        b.is_alive(),
    );
    // The handshake + renegotiation must all have completed
    // for the frame to arrive; assert them explicitly so a
    // future regression that *happens* to deliver a frame
    // through a different path still surfaces a broken
    // intermediate step.
    assert!(outcome.a_connected, "publisher A must reach Connected");
    assert!(outcome.b_connected, "subscriber B must reach Connected");
    assert!(
        outcome.reneg_answered,
        "the SFU-initiated renegotiation toward B must have been answered"
    );
    assert!(a.is_alive() && b.is_alive(), "both Rtc must stay alive");
}

/// **G3-3 simulcast, live leg** — a real `str0m` publisher sending a
/// **two-layer video simulcast** track (rids `l` + `h`) negotiates with the
/// production media task and the SFU forwards a layer to a subscriber over the
/// live ICE + DTLS + renegotiation path.
///
/// This closes the "needs a simulcast-capable test client" gap: the publisher
/// announces `a=simulcast:send` with two rids via `add_media(.., Some(Simulcast
/// { send: [l, h], .. }))` and writes frames on BOTH layers (`Writer::rid`).
/// The SFU receives the multi-rid track, and its per-subscriber layer gate
/// (`layer_gate_allows`, unit-proven to latch one rid and drop the rest) forwards
/// a single layer to B. The assertion here is the LIVE-path property the unit
/// tests cannot cover: that str0m's simulcast-SEND SDP negotiation is accepted
/// by `MediaPeer::accept_offer`, the SFU tells the layers' packets apart by rid,
/// and the fanout delivers a forwarded video frame to the subscriber. The
/// one-layer-only gating itself is asserted by
/// `media::tests::layer_gate_latches_the_first_layer_and_drops_the_others` +
/// `forward_drops_a_non_selected_simulcast_layer_as_wrong_layer`.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn simulcast_publisher_negotiates_and_forwards_a_layer_over_the_live_sfu() {
    let peers = PeerRegistry::new();
    let b_id = fresh_peer_id();
    let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
    assert!(
        peers
            .register(Peer {
                id: b_id.clone(),
                room: "simulcast-test".to_owned(),
                outbound: Some(b_tx),
            })
            .await,
        "fresh PeerId registers"
    );

    let (media, sfu_addr) = spawn_media_task("127.0.0.1:0".parse().unwrap(), peers)
        .await
        .expect("UDP bind on loopback:0 succeeds");

    let mut a = SyntheticClient::new().await;
    let mut b = SyntheticClient::new().await;
    let a_id = fresh_peer_id();

    // B subscribes first (RecvOnly) — establishes transport; the SFU adds the
    // downstream video track by renegotiation.
    let (b_offer, b_pending) = b.create_subscriber_offer();
    let b_answer = media
        .accept_offer(b_id.clone(), "simulcast-test-call".to_owned(), b_offer)
        .await
        .expect("SFU accepts B's RecvOnly subscriber offer");
    b.accept_answer(b_pending, &b_answer)
        .expect("B applies the SFU's answer");

    // A publishes a two-layer video simulcast track.
    let (a_offer, a_pending, pub_mid) = a.create_video_simulcast_publisher_offer(&["l", "h"]);
    let a_answer = media
        .accept_offer(a_id.clone(), "simulcast-test-call".to_owned(), a_offer)
        .await
        .expect("SFU accepts A's simulcast-send video offer");
    a.accept_answer(a_pending, &a_answer)
        .expect("A applies the SFU's answer");

    // Distinguishable per-layer markers (first byte) so, IF the payload survives
    // video (de)packetisation, we can additionally check no two layers mix.
    let low: Vec<u8> = std::iter::once(0xAAu8).chain(0u8..40).collect();
    let high: Vec<u8> = std::iter::once(0xBBu8).chain(0u8..40).collect();

    let deadline = Instant::now() + Duration::from_secs(12);
    let mut reneg_answered = false;
    let mut pending_reneg: Option<String> = None;
    let mut frame_idx: u64 = 0;
    let mut b_frames = 0usize;
    let mut markers = std::collections::BTreeSet::new();

    while Instant::now() < deadline && b_frames == 0 {
        let _ = a.drain().await;
        for p in b.drain().await {
            b_frames += 1;
            if let Some(&first) = p.first() {
                if first == 0xAA || first == 0xBB {
                    markers.insert(first);
                }
            }
        }

        while let Ok(ServerFrame::SdpOffer { sdp }) = b_rx.try_recv() {
            pending_reneg = Some(sdp);
        }
        if !reneg_answered && b.is_connected() {
            if let Some(sdp) = pending_reneg.take() {
                if let Ok(answer) = b.answer_renegotiation(&sdp) {
                    reneg_answered = media
                        .accept_renegotiation_answer(b_id.clone(), answer)
                        .await
                        .is_ok();
                }
            }
        }

        // Once the downstream track exists, publish on BOTH layers each tick.
        if a.is_connected() && b.is_connected() && reneg_answered {
            a.write_video_frame_on_rid(pub_mid, "l", &low, frame_idx);
            a.write_video_frame_on_rid(pub_mid, "h", &high, frame_idx);
            frame_idx += 1;
        }

        let a_sock = a.socket.clone();
        let b_sock = b.socket.clone();
        let mut abuf = [0u8; 2048];
        let mut bbuf = [0u8; 2048];
        tokio::select! {
            r = a_sock.recv_from(&mut abuf) => {
                if let Ok((n, src)) = r { a.feed_receive(&abuf[..n], src, sfu_addr); }
            }
            r = b_sock.recv_from(&mut bbuf) => {
                if let Ok((n, src)) = r { b.feed_receive(&bbuf[..n], src, sfu_addr); }
            }
            () = tokio::time::sleep(Duration::from_millis(5)) => {
                a.feed_timeout();
                b.feed_timeout();
            }
        }
    }

    assert!(
        a.is_connected(),
        "simulcast publisher A must reach Connected"
    );
    assert!(b.is_connected(), "subscriber B must reach Connected");
    assert!(
        reneg_answered,
        "the SFU-initiated renegotiation toward B must have been answered"
    );
    assert!(
        b_frames > 0,
        "the SFU must forward at least one simulcast video frame to B \
         (a_connected={} b_connected={} reneg={})",
        a.is_connected(),
        b.is_connected(),
        reneg_answered
    );
    // Best-effort layer-gate cross-check: if the markers survived packetisation,
    // B must have seen frames from AT MOST ONE layer (the gate forwards one rid,
    // drops the rest). An empty marker set (payload mangled by the video
    // depacketiser) does not fail — the one-layer gate is unit-proven.
    assert!(
        markers.len() <= 1,
        "the SFU forwarded frames from more than one simulcast layer: {markers:?}"
    );
}

/// G-26 (the last open SFrame interop gate, headless slice) — a frame
/// SEALED with the real `lakko_calls::sframe` core (ADR-0013: the RFC 9605
/// SFrame wire header is the AEAD AAD) survives the **live** SFU media path
/// and OPENS back to the original plaintext at the subscriber.
///
/// This is the cross-fanout test with the opaque payload replaced by a real
/// sealed SFrame frame. It proves, end-to-end over a real `str0m` ICE +
/// DTLS handshake + the production `spawn_media_task` fanout, that:
///
///   1. the SFU forwards the SEALED ciphertext **verbatim** (it never
///      inspects or mutates `payload_b64` — the opacity invariant, C-39);
///   2. the SFrame layer round-trips: `seal_framed` → SFU forward →
///      `open_framed` recovers the exact plaintext — which can only happen
///      if the RFC 9605 header (the AAD) and the ciphertext arrive intact.
///
/// A + B derive the SAME `CallKey` from a shared seed — the integration-side
/// stand-in for the MLS exporter handing both peers identical bytes (C-1).
/// The remaining (deferred) part of G-26 is the *cross-runtime* leg: a frame
/// sealed in a real browser worker opened by a real native shim over a
/// deployed SFU — that needs live browser + native runtimes, not headless CI.
#[tokio::test]
async fn sframe_sealed_frame_round_trips_through_the_live_sfu() {
    use lakko_calls::sframe::CALL_KEY_LEN;
    use lakko_calls::CallKey;

    // 1. Media task + registry (B is the subscriber with an intercept
    //    channel; A publishes). Identical to the verbatim cross-fanout.
    let peers = PeerRegistry::new();
    let b_id = fresh_peer_id();
    let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
    let registered = peers
        .register(Peer {
            id: b_id.clone(),
            room: "sframe-fanout-test".to_owned(),
            outbound: Some(b_tx),
        })
        .await;
    assert!(registered, "fresh PeerId must register cleanly");

    let (media, sfu_addr) = spawn_media_task("127.0.0.1:0".parse().unwrap(), peers)
        .await
        .expect("UDP bind on loopback:0 succeeds");

    let mut a = SyntheticClient::new().await;
    let mut b = SyntheticClient::new().await;
    let a_id = fresh_peer_id();

    let (b_offer, b_pending) = b.create_subscriber_offer();
    let b_answer = media
        .accept_offer(b_id.clone(), "fanout-test-call".to_owned(), b_offer)
        .await
        .expect("SFU accepts B's RecvOnly subscriber offer");
    b.accept_answer(b_pending, &b_answer)
        .expect("B applies the SFU's answer");

    let (a_offer, a_pending, publisher_mid) = a.create_audio_publisher_offer();
    let a_answer = media
        .accept_offer(a_id.clone(), "fanout-test-call".to_owned(), a_offer)
        .await
        .expect("SFU accepts A's SendOnly publisher offer");
    a.accept_answer(a_pending, &a_answer)
        .expect("A applies the SFU's answer");

    // 2. Derive the SAME per-call key on both ends (C-1 stand-in) and seal
    //    a plaintext frame as A's sender. `a_leaf` is A's MLS leaf index;
    //    B opens A's frames with the participant key for that leaf.
    let seed = [0x5a_u8; CALL_KEY_LEN];
    let a_leaf: u32 = 7;
    let mut a_sender = CallKey::from_seed(seed)
        .participant_key_for(a_leaf, 0)
        .expect("A derives its sender participant key");
    let b_receiver = CallKey::from_seed(seed)
        .participant_key_for(a_leaf, 0)
        .expect("B derives the receiver participant key for A's leaf");

    let plaintext: &[u8] = b"lakko encrypted media plane -- G-26 round-trip";
    let sealed = a_sender.seal_framed(plaintext).expect("A seals the frame");
    // Sanity: the wire is genuinely encrypted (not the plaintext) and is a
    // plausible RTP-payload size for the loopback path.
    assert_ne!(
        sealed.as_slice(),
        plaintext,
        "sealed wire must be ciphertext"
    );
    assert!(
        sealed.len() <= 1024,
        "sealed frame fits a single RTP payload"
    );

    // 3. The SFU must forward the SEALED bytes verbatim to B.
    let outcome = run_cross_fanout(
        &mut a,
        &mut b,
        sfu_addr,
        &media,
        b_id.clone(),
        &mut b_rx,
        publisher_mid,
        &sealed,
    )
    .await;

    assert!(
        outcome.received_expected,
        "subscriber B never received A's SEALED frame verbatim within \
         {CROSS_FANOUT_DEADLINE:?}; final state: a_connected={} b_connected={} \
         reneg_answered={} a_alive={} b_alive={}",
        outcome.a_connected,
        outcome.b_connected,
        outcome.reneg_answered,
        a.is_alive(),
        b.is_alive(),
    );
    assert!(outcome.a_connected, "publisher A must reach Connected");
    assert!(outcome.b_connected, "subscriber B must reach Connected");
    assert!(
        outcome.reneg_answered,
        "the SFU-initiated renegotiation toward B must have been answered"
    );
    assert!(a.is_alive() && b.is_alive(), "both Rtc must stay alive");

    // 4. The forwarded bytes (just asserted byte-equal to `sealed`) OPEN
    //    back to the original plaintext — so the RFC 9605 header (AAD) and
    //    the ciphertext survived the SFU forward intact. This is the
    //    SFrame-over-live-SFU round-trip that G-26 is about.
    let opened = b_receiver
        .open_framed(&sealed)
        .expect("B opens the SFrame frame the SFU forwarded");
    assert_eq!(
        opened.as_slice(),
        plaintext,
        "B must recover A's exact plaintext after the SFU relayed the sealed frame"
    );
}

/// G-26 — the CANONICAL cross-stack golden vector survives the live SFU.
///
/// `sframe_sealed_frame_round_trips_through_the_live_sfu` (above) proves an
/// ARBITRARY sealed frame round-trips the SFU. This sibling pins the SAME
/// canonical SFrame wire the cross-runtime golden-vector tests assert on EVERY
/// stack — the Rust core (`lakko_calls::sframe` `golden_vector_*`), web
/// (`apps/web/src/calls/sframe-golden.test.ts`), iOS
/// (`apps/ios/LakkoTests/SFrameGoldenTests.swift`), Android
/// (`apps/android/.../calls/SframeGoldenTest.kt`): sealing
/// `(seed=[0x42;32], leaf=3, "lakko sframe golden vector v1")` on a fresh
/// `ParticipantKey` yields a byte-pinned wire.
///
/// By asserting the seal produces EXACTLY that wire AND that the live SFU
/// forwards it verbatim + the subscriber opens it back to the golden plaintext,
/// this composes the two previously-separate facts — "every stack produces +
/// consumes these exact bytes" and "these exact bytes survive the real `str0m`
/// forward" — into ONE observed fact: the bytes a real web / iOS / Android
/// client emits are the bytes proven to cross the SFU intact. The only residual
/// is the live real-browser (+) real-native (+) deployed-SFU smoke (needs
/// runtimes, not headless CI), redundant for correctness given this composition.
///
/// The golden constants are RE-DECLARED here, not imported — they live in
/// `lakko-calls`'s `#[cfg(test)]` module. This is the same hand-mirrored-fixture
/// posture the per-platform golden tests use; an intentional cross-check, so a
/// core wire-format change fails BOTH this test and the `lakko-calls` golden
/// test (and the three client golden tests).
#[tokio::test]
async fn golden_vector_round_trips_through_the_live_sfu() {
    use lakko_calls::sframe::CALL_KEY_LEN;
    use lakko_calls::CallKey;

    // The cross-stack canonical SFrame golden vector — MUST stay byte-identical
    // to `crates/lakko-calls/src/sframe.rs` GOLDEN_* + the web / iOS / Android
    // golden tests.
    const GOLDEN_SEED: [u8; CALL_KEY_LEN] = [0x42; CALL_KEY_LEN];
    const GOLDEN_LEAF: u32 = 3;
    // ADR-0035 — the KID is per-`(leaf, stream)`: `(leaf << 8) | stream_index`.
    // The golden pins stream 0, so leaf 3 encodes as the 2-byte KID `0x0300`.
    const GOLDEN_STREAM: u32 = 0;
    const GOLDEN_PLAINTEXT: &[u8] = b"lakko sframe golden vector v1";
    const GOLDEN_WIRE_HEX: &str =
        "910300fe2c2af65799594a4c693ff4c9e0d49b0565756ddd7299b0945fc8adb1c9c12fced57c6937f372e6b94bdae40a";
    let golden_wire: Vec<u8> = (0..GOLDEN_WIRE_HEX.len() / 2)
        .map(|i| u8::from_str_radix(&GOLDEN_WIRE_HEX[i * 2..i * 2 + 2], 16).expect("golden hex"))
        .collect();

    // 1. Media task + A-publisher / B-subscriber over the live SFU (same setup
    //    as the arbitrary-payload sibling above).
    let peers = PeerRegistry::new();
    let b_id = fresh_peer_id();
    let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
    let registered = peers
        .register(Peer {
            id: b_id.clone(),
            room: "golden-fanout-call".to_owned(),
            outbound: Some(b_tx),
        })
        .await;
    assert!(registered, "fresh PeerId must register cleanly");

    let (media, sfu_addr) = spawn_media_task("127.0.0.1:0".parse().unwrap(), peers)
        .await
        .expect("UDP bind on loopback:0 succeeds");

    let mut a = SyntheticClient::new().await;
    let mut b = SyntheticClient::new().await;
    let a_id = fresh_peer_id();

    let (b_offer, b_pending) = b.create_subscriber_offer();
    let b_answer = media
        .accept_offer(b_id.clone(), "golden-fanout-call".to_owned(), b_offer)
        .await
        .expect("SFU accepts B's RecvOnly subscriber offer");
    b.accept_answer(b_pending, &b_answer)
        .expect("B applies the SFU's answer");

    let (a_offer, a_pending, publisher_mid) = a.create_audio_publisher_offer();
    let a_answer = media
        .accept_offer(a_id.clone(), "golden-fanout-call".to_owned(), a_offer)
        .await
        .expect("SFU accepts A's SendOnly publisher offer");
    a.accept_answer(a_pending, &a_answer)
        .expect("A applies the SFU's answer");

    // 2. A publishes the GOLDEN VECTOR itself: the canonical cross-stack bytes
    //    web / iOS / Android each open. Since ADR-0069 a fresh sender key starts
    //    at a random counter, so no stack seals these bytes any more; they are
    //    forwarded as they stand and B opens them.
    let b_receiver = CallKey::from_seed(GOLDEN_SEED)
        .participant_key_for(GOLDEN_LEAF, GOLDEN_STREAM)
        .expect("B derives the golden receiver participant key");
    let sealed = golden_wire.clone();

    // 3. The SFU forwards the golden bytes VERBATIM to B (`received_expected`
    //    asserts B's forwarded payload is byte-equal to `sealed`).
    let outcome = run_cross_fanout(
        &mut a,
        &mut b,
        sfu_addr,
        &media,
        b_id.clone(),
        &mut b_rx,
        publisher_mid,
        &sealed,
    )
    .await;
    assert!(
        outcome.received_expected,
        "subscriber B never received the golden frame verbatim within \
         {CROSS_FANOUT_DEADLINE:?}; a_connected={} b_connected={} reneg_answered={}",
        outcome.a_connected, outcome.b_connected, outcome.reneg_answered,
    );
    assert!(
        outcome.a_connected && outcome.b_connected,
        "both peers must reach Connected"
    );
    assert!(
        outcome.reneg_answered,
        "the SFU-initiated renegotiation toward B must have been answered"
    );

    // 4. The forwarded golden bytes open back to the golden plaintext — the
    //    canonical cross-stack wire survives the real SFU forward intact.
    let opened = b_receiver
        .open_framed(&sealed)
        .expect("B opens the golden SFrame frame the SFU forwarded");
    assert_eq!(
        opened.as_slice(),
        GOLDEN_PLAINTEXT,
        "B must recover the golden plaintext after the SFU relayed the golden wire"
    );
}

/// Cross-call isolation over the LIVE SFU (a hard SFU
/// invariant): a publisher A in call ALPHA and a subscriber B
/// in call BETA both complete their handshakes against the one
/// process-wide media task, but the SFU NEVER renegotiates a
/// downstream track toward B and B NEVER receives A's media —
/// because `propagate_new_tracks_in` and the packet fanout are
/// call-scoped.
///
/// The negative is observed structurally + fast: once A is
/// connected its SendOnly track is live, so the SFU would have
/// opened a downstream track + dispatched a `ServerFrame::SdpOffer`
/// to B within a renegotiation cycle IF B shared A's call (the
/// same-call path is proven by
/// `two_clients_cross_fanout_forwards_media_verbatim`). Here B is
/// in a DIFFERENT call, so across a generous settle window B's
/// outbound channel receives NO offer and B drains NO media.
#[tokio::test]
async fn fanout_never_crosses_calls_over_the_live_sfu() {
    let peers = PeerRegistry::new();
    let b_id = fresh_peer_id();
    let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
    let registered = peers
        .register(Peer {
            id: b_id.clone(),
            room: "call-beta".to_owned(),
            outbound: Some(b_tx),
        })
        .await;
    assert!(registered, "fresh PeerId must register cleanly");

    let (media, sfu_addr) = spawn_media_task("127.0.0.1:0".parse().unwrap(), peers)
        .await
        .expect("UDP bind on loopback:0 succeeds");

    let mut a = SyntheticClient::new().await;
    let mut b = SyntheticClient::new().await;
    let a_id = fresh_peer_id();

    // B subscribes in call BETA; A publishes in call ALPHA.
    let (b_offer, b_pending) = b.create_subscriber_offer();
    let b_answer = media
        .accept_offer(b_id.clone(), "call-beta".to_owned(), b_offer)
        .await
        .expect("SFU accepts B's RecvOnly subscriber offer");
    b.accept_answer(b_pending, &b_answer)
        .expect("B applies the SFU's answer");

    let (a_offer, a_pending, publisher_mid) = a.create_audio_publisher_offer();
    let a_answer = media
        .accept_offer(a_id.clone(), "call-alpha".to_owned(), a_offer)
        .await
        .expect("SFU accepts A's SendOnly publisher offer");
    a.accept_answer(a_pending, &a_answer)
        .expect("A applies the SFU's answer");

    // Bounded settle window — long enough for the handshakes +
    // several SFU renegotiation cycles, short enough to keep the
    // test fast (the same-call reneg lands well under a second).
    let settle_deadline = Instant::now() + Duration::from_secs(6);
    let mut b_got_offer = false;
    let mut b_got_media = false;
    let mut frame_idx: u64 = 0;

    while Instant::now() < settle_deadline {
        let _ = a.drain().await;
        if !b.drain().await.is_empty() {
            b_got_media = true;
        }
        while let Ok(frame) = b_rx.try_recv() {
            if matches!(frame, ServerFrame::SdpOffer { .. }) {
                b_got_offer = true;
            }
        }
        // Keep publishing once A is up so the SFU has a live
        // track to (not) propagate across the call boundary.
        if a.is_connected() {
            a.write_audio_frame(publisher_mid, b"cross-call-must-not-arrive", frame_idx);
            frame_idx += 1;
        }

        let a_sock = a.socket.clone();
        let b_sock = b.socket.clone();
        let mut abuf = [0u8; 2048];
        let mut bbuf = [0u8; 2048];
        tokio::select! {
            r = a_sock.recv_from(&mut abuf) => {
                if let Ok((n, src)) = r { a.feed_receive(&abuf[..n], src, sfu_addr); }
            }
            r = b_sock.recv_from(&mut bbuf) => {
                if let Ok((n, src)) = r { b.feed_receive(&bbuf[..n], src, sfu_addr); }
            }
            () = tokio::time::sleep(Duration::from_millis(5)) => {
                a.feed_timeout();
                b.feed_timeout();
            }
        }
    }

    // A must have connected — otherwise its track never went live
    // and the negative would be vacuous (B got nothing because
    // nothing was ever published, not because of isolation).
    assert!(
        a.is_connected(),
        "publisher A must connect so its track is live (else the test is vacuous)"
    );
    assert!(
        b.is_connected(),
        "subscriber B must connect — isolation, not a broken transport"
    );
    assert!(
        !b_got_offer,
        "cross-call isolation breach: the SFU renegotiated a downstream track toward a peer in a DIFFERENT call"
    );
    assert!(
        !b_got_media,
        "cross-call isolation breach: a peer in call BETA received media from a publisher in call ALPHA"
    );
}

/// Final state of a [`run_multi_frame_fanout`] drive. `received_order` is the
/// order in which B first observed each frame index, so a failure message can
/// show which frames landed and which stalled.
#[allow(clippy::struct_excessive_bools)]
struct MultiFanoutOutcome {
    a_connected: bool,
    b_connected: bool,
    reneg_answered: bool,
    /// Index of each `frames[i]` in the order B first received it.
    received_order: Vec<usize>,
}

/// Drive publisher A + subscriber B + the SFU media task until subscriber B has
/// received EVERY distinct frame in `frames` (or the deadline elapses). The
/// generalisation of [`run_cross_fanout`] from one payload to N: each tick, A
/// writes the next not-yet-received frame (round-robin over the outstanding
/// set, so a single loopback drop of one frame does not stall the others), and
/// B's drained `MediaData` payloads are matched back to their frame index.
#[allow(clippy::too_many_arguments)]
async fn run_multi_frame_fanout(
    a: &mut SyntheticClient,
    b: &mut SyntheticClient,
    sfu_addr: SocketAddr,
    handle: &MediaTaskHandle,
    b_id: PeerId,
    b_outbound_rx: &mut mpsc::Receiver<ServerFrame>,
    a_publisher_mid: Mid,
    frames: &[Vec<u8>],
) -> MultiFanoutOutcome {
    let deadline = Instant::now() + CROSS_FANOUT_DEADLINE;
    let mut reneg_answered = false;
    let mut pending_reneg: Option<String> = None;
    let mut frame_idx: u64 = 0;
    // Round-robin cursor over `frames` (a `usize` so no u64->usize cast).
    let mut cursor: usize = 0;
    let mut received = vec![false; frames.len()];
    let mut received_order: Vec<usize> = Vec::new();

    while Instant::now() < deadline {
        // 1 + 2. Pump both sides; classify B's received payloads by frame index.
        let _ = a.drain().await;
        for p in b.drain().await {
            if let Some(idx) = frames.iter().position(|f| f.as_slice() == p.as_slice()) {
                if !received[idx] {
                    received[idx] = true;
                    received_order.push(idx);
                }
            }
        }
        if received.iter().all(|&r| r) {
            break;
        }

        // 3. Answer the SFU-initiated renegotiation for B (same as cross_fanout).
        while let Ok(frame) = b_outbound_rx.try_recv() {
            if let ServerFrame::SdpOffer { sdp } = frame {
                pending_reneg = Some(sdp);
            }
        }
        if !reneg_answered && b.is_connected() {
            if let Some(sdp) = pending_reneg.take() {
                if let Ok(answer) = b.answer_renegotiation(&sdp) {
                    if handle
                        .accept_renegotiation_answer(b_id.clone(), answer)
                        .await
                        .is_ok()
                    {
                        reneg_answered = true;
                    }
                }
            }
        }

        // 4. Publish the next OUTSTANDING frame, round-robin from a rotating
        //    cursor so one persistently-dropped frame cannot starve the rest.
        if a.is_connected() && b.is_connected() && reneg_answered {
            if let Some(off) = (0..frames.len()).find(|k| !received[(cursor + k) % frames.len()]) {
                let idx = (cursor + off) % frames.len();
                a.write_audio_frame(a_publisher_mid, &frames[idx], frame_idx);
            }
            cursor = (cursor + 1) % frames.len();
            frame_idx += 1;
        }

        // 5. Wait for socket activity or a short tick.
        let a_sock = a.socket.clone();
        let b_sock = b.socket.clone();
        let mut abuf = [0u8; 2048];
        let mut bbuf = [0u8; 2048];
        tokio::select! {
            r = a_sock.recv_from(&mut abuf) => {
                if let Ok((n, src)) = r {
                    a.feed_receive(&abuf[..n], src, sfu_addr);
                }
            }
            r = b_sock.recv_from(&mut bbuf) => {
                if let Ok((n, src)) = r {
                    b.feed_receive(&bbuf[..n], src, sfu_addr);
                }
            }
            () = tokio::time::sleep(Duration::from_millis(5)) => {
                a.feed_timeout();
                b.feed_timeout();
            }
        }
    }

    MultiFanoutOutcome {
        a_connected: a.is_connected(),
        b_connected: b.is_connected(),
        reneg_answered,
        received_order,
    }
}

/// G-26 / G3-4 — a SUSTAINED SFrame stream (not just one frame) round-trips the
/// live SFU between two synthetic peers.
///
/// [`sframe_sealed_frame_round_trips_through_the_live_sfu`] proves a SINGLE
/// sealed frame (counter 0) survives the SFU media path. This closes the "≥ K
/// SFrame-decrypted frames" leg: publisher A seals `N` DISTINCT frames with the
/// real `lakko_calls::sframe` core — the participant key auto-advances the
/// per-frame counter (pre-incremented, so the wire carries counters `1..N`,
/// audit-pack C-5, monotone per sender per epoch) — every frame is forwarded
/// through the
/// production `str0m` media task over a real ICE + DTLS + cross-fanout
/// renegotiation, and subscriber B `open_framed`s EACH forwarded frame back to
/// its exact plaintext. A pass proves the SFrame layer (ciphertext + the
/// header-as-AAD, ADR-0013) survives sustained forwarding, not just a one-shot,
/// while the SFU stays SFrame-opaque (`lakko-calls` is a dev-only dep of
/// `services/sfu`; the production binary never links it — C-39).
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn multi_frame_sframe_stream_round_trips_through_the_live_sfu() {
    use lakko_calls::sframe::CALL_KEY_LEN;
    use lakko_calls::{CallKey, SframeHeader};

    // Number of distinct frames in the stream. Small enough to complete well
    // within CROSS_FANOUT_DEADLINE on a loaded CI runner, large enough that the
    // counter progression + sustained forwarding are genuinely exercised.
    const STREAM_LEN: usize = 5;

    // 1. Media task + registry (B is the subscriber with an intercept channel).
    let peers = PeerRegistry::new();
    let b_id = fresh_peer_id();
    let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
    let registered = peers
        .register(Peer {
            id: b_id.clone(),
            room: "sframe-stream-test".to_owned(),
            outbound: Some(b_tx),
        })
        .await;
    assert!(registered, "fresh PeerId must register cleanly");

    let (media, sfu_addr) = spawn_media_task("127.0.0.1:0".parse().unwrap(), peers)
        .await
        .expect("UDP bind on loopback:0 succeeds");

    let mut a = SyntheticClient::new().await;
    let mut b = SyntheticClient::new().await;
    let a_id = fresh_peer_id();

    let (b_offer, b_pending) = b.create_subscriber_offer();
    let b_answer = media
        .accept_offer(b_id.clone(), "stream-test-call".to_owned(), b_offer)
        .await
        .expect("SFU accepts B's RecvOnly subscriber offer");
    b.accept_answer(b_pending, &b_answer)
        .expect("B applies the SFU's answer");

    let (a_offer, a_pending, publisher_mid) = a.create_audio_publisher_offer();
    let a_answer = media
        .accept_offer(a_id.clone(), "stream-test-call".to_owned(), a_offer)
        .await
        .expect("SFU accepts A's SendOnly publisher offer");
    a.accept_answer(a_pending, &a_answer)
        .expect("A applies the SFU's answer");

    // 2. Derive the SAME per-call key on both ends and seal N DISTINCT frames.
    //    `seal_framed` advances the counter internally, so the sealed wires
    //    carry counters 0..N-1.
    let seed = [0x5a_u8; CALL_KEY_LEN];
    let a_leaf: u32 = 7;
    let mut a_sender = CallKey::from_seed(seed)
        .participant_key_for(a_leaf, 0)
        .expect("A derives its sender participant key");
    let b_receiver = CallKey::from_seed(seed)
        .participant_key_for(a_leaf, 0)
        .expect("B derives the receiver participant key for A's leaf");

    let mut plaintexts: Vec<Vec<u8>> = Vec::with_capacity(STREAM_LEN);
    let mut sealed_frames: Vec<Vec<u8>> = Vec::with_capacity(STREAM_LEN);
    for i in 0..STREAM_LEN {
        let pt = format!("lakko media frame #{i} -- sustained sframe stream").into_bytes();
        let sealed = a_sender.seal_framed(&pt).expect("A seals frame");
        assert_ne!(sealed.as_slice(), pt.as_slice(), "each wire is ciphertext");
        plaintexts.push(pt);
        sealed_frames.push(sealed);
    }

    // Pre-SFU: the sealed wires carry strictly-increasing counters 1..N — the
    // participant key PRE-increments the counter inside `seal`, so the first
    // frame is counter 1 (audit-pack C-5, monotone per sender per epoch) —
    // asserted on the seal side, independent of the SFU.
    // ADR-0069: the first counter is random, the rest follow it one by one.
    let first_ctr = SframeHeader::decode(&sealed_frames[0])
        .expect("sealed wire has a header")
        .0
        .ctr;
    for (i, wire) in sealed_frames.iter().enumerate() {
        let expected_ctr = first_ctr + i as u64;
        let (header, _consumed) = SframeHeader::decode(wire).expect("sealed wire has a header");
        // ADR-0035 — the KID is per-`(leaf, stream)`, `(leaf << 8) | stream_index`,
        // not the bare leaf. Asserted through the core's own composer and
        // accessors rather than against a hand-computed number, so a change to
        // the packing fails here for the reason it happened instead of looking
        // like an arithmetic slip in this test.
        assert_eq!(
            SframeHeader::kid_for(a_leaf, 0).expect("A's KID composes"),
            header.kid,
            "KID is A's (leaf, stream) on every frame"
        );
        assert_eq!(
            a_leaf,
            SframeHeader::leaf_of(header.kid),
            "the sender leaf is recoverable from the KID"
        );
        assert_eq!(
            0,
            SframeHeader::stream_of(header.kid),
            "this stream is stream 0"
        );
        assert_eq!(
            expected_ctr, header.ctr,
            "frame {i} must carry counter {expected_ctr} (C-5 monotone)"
        );
    }

    // 3. Forward the whole stream through the live SFU.
    let outcome = run_multi_frame_fanout(
        &mut a,
        &mut b,
        sfu_addr,
        &media,
        b_id.clone(),
        &mut b_rx,
        publisher_mid,
        &sealed_frames,
    )
    .await;

    assert_eq!(
        outcome.received_order.len(),
        STREAM_LEN,
        "subscriber B must receive all {STREAM_LEN} distinct sealed frames within \
         {CROSS_FANOUT_DEADLINE:?}; got {} (order {:?}); a_connected={} b_connected={} \
         reneg_answered={} a_alive={} b_alive={}",
        outcome.received_order.len(),
        outcome.received_order,
        outcome.a_connected,
        outcome.b_connected,
        outcome.reneg_answered,
        a.is_alive(),
        b.is_alive(),
    );
    assert!(
        outcome.a_connected && outcome.b_connected,
        "both peers must connect"
    );
    assert!(a.is_alive() && b.is_alive(), "both Rtc must stay alive");

    // 4. Every forwarded frame OPENS back to its exact plaintext — so the
    //    SFrame ciphertext + the header-as-AAD survived the SFU forward for the
    //    WHOLE stream, not just one frame.
    for i in 0..STREAM_LEN {
        let opened = b_receiver
            .open_framed(&sealed_frames[i])
            .unwrap_or_else(|_| panic!("B opens forwarded frame {i}"));
        assert_eq!(
            opened, plaintexts[i],
            "B must recover frame {i}'s exact plaintext after the SFU relayed it"
        );
    }
}

/// **Frame-descriptor relay, live leg** — a publisher's
/// `generic-frame-descriptor-00` header-extension value survives the SFU and
/// arrives at a subscriber.
///
/// ## Why this matters more than it looks
///
/// Lakko seals the whole encoded frame with SFrame, which overwrites the
/// bytes a libwebrtc receiver reads the key-frame flag and picture size from
/// (`video_rtp_depacketizer_vp8.cc:180` at `branch-heads/6422`). The frame
/// descriptor is the only remaining channel for those facts, so if the SFU
/// drops it — which it did, silently, in three separate places before this
/// change — group video cannot decode however correct the clients are.
///
/// The test drives the whole path that has to hold for the extension to
/// arrive: the SFU must echo the URI in its answer to the publisher's offer
/// (else the publisher never sends it), parse the value off the wire (else
/// it is discarded on arrival), offer the URI in its own renegotiation
/// toward the subscriber (else the subscriber refuses it), and write the
/// value back onto the relayed frame.
///
/// The payload is deliberately larger than one RTP packet, so the frame
/// crosses the SFU as several packets, each with its own descriptor value:
/// the full one on the first, a one-byte continuation on the rest. The SFU
/// relays every packet's value as it arrived (ADR-0066). What the subscriber
/// assembles is asserted to carry a **first-packet** value — the
/// frame-level fields, with the begin-of-frame bit set — which is what
/// catches a regression where a bare continuation byte reaches the
/// subscriber in place of the real descriptor. The per-packet forms
/// themselves are pinned by the unit tests in
/// `lakko_svc_sfu::frame_descriptor`, which can enumerate positions a live
/// path cannot.
///
/// Opacity is unaffected: the SFU relays these bytes as they arrived and
/// never touches the SFrame-sealed payload (C-39).
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn frame_descriptor_survives_the_relay_to_a_subscriber() {
    let peers = PeerRegistry::new();
    let b_id = fresh_peer_id();
    let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
    assert!(
        peers
            .register(Peer {
                id: b_id.clone(),
                room: "frame-descriptor-test".to_owned(),
                outbound: Some(b_tx),
            })
            .await,
        "fresh PeerId must register cleanly"
    );

    let (media, sfu_addr) = spawn_media_task("127.0.0.1:0".parse().unwrap(), peers)
        .await
        .expect("UDP bind on loopback:0 succeeds");

    // Both synthetic clients advertise the extension, as a libwebrtc client
    // does under `WebRTC-GenericDescriptorAdvertised`.
    let mut a = SyntheticClient::new_with_frame_descriptor().await;
    let mut b = SyntheticClient::new_with_frame_descriptor().await;
    let a_id = fresh_peer_id();

    let (b_offer, b_pending) = b.create_video_subscriber_offer();
    let b_answer = media
        .accept_offer(b_id.clone(), "frame-descriptor-call".to_owned(), b_offer)
        .await
        .expect("SFU accepts B's RecvOnly video subscriber offer");
    assert!(
        b_answer.contains(GENERIC_FRAME_DESCRIPTOR_URI),
        "the SFU must echo the frame-descriptor extension in its answer, \
         or the peer never activates it; answer was:\n{b_answer}"
    );
    b.accept_answer(b_pending, &b_answer)
        .expect("B applies the SFU's answer");

    let (a_offer, a_pending, publisher_mid) = a.create_video_publisher_offer();
    let a_answer = media
        .accept_offer(a_id.clone(), "frame-descriptor-call".to_owned(), a_offer)
        .await
        .expect("SFU accepts A's SendOnly video publisher offer");
    assert!(
        a_answer.contains(GENERIC_FRAME_DESCRIPTOR_URI),
        "the SFU must echo the frame-descriptor extension to the publisher too"
    );
    a.accept_answer(a_pending, &a_answer)
        .expect("A applies the SFU's answer");

    // A key-frame-shaped first-packet descriptor: B|E|F|L flags, spatial
    // bitmask, frame id, then a 1280x720 resolution. Opaque to the SFU; this
    // test only needs it to be a value libwebrtc's own writer could produce.
    let descriptor: Vec<u8> = vec![0xf0, 0x01, 0x2a, 0x00, 0x05, 0x00, 0x02, 0xd0];
    // Larger than one RTP packet, so the frame is several packets on both
    // legs and the per-packet descriptor path runs for real.
    let payload: Vec<u8> = (0..3000_u32)
        .map(|i| {
            u8::try_from(i % 251)
                .unwrap_or(0)
                .wrapping_mul(7)
                .wrapping_add(3)
        })
        .collect();

    let deadline = Instant::now() + CROSS_FANOUT_DEADLINE;
    let mut reneg_answered = false;
    let mut pending_reneg: Option<String> = None;
    let mut frame_idx: u64 = 0;
    let mut received = false;

    while Instant::now() < deadline {
        let _ = a.drain().await;
        let b_payloads = b.drain().await;
        if b_payloads
            .iter()
            .any(|p| p.as_slice() == payload.as_slice())
        {
            received = true;
            break;
        }

        while let Ok(frame) = b_rx.try_recv() {
            if let ServerFrame::SdpOffer { sdp } = frame {
                pending_reneg = Some(sdp);
            }
        }
        if !reneg_answered && b.is_connected() {
            if let Some(sdp) = pending_reneg.take() {
                assert!(
                    sdp.contains(GENERIC_FRAME_DESCRIPTOR_URI),
                    "the SFU's own offer toward the subscriber must carry the \
                     frame-descriptor extension, or the downstream track can \
                     never receive one; offer was:\n{sdp}"
                );
                if let Ok(answer) = b.answer_renegotiation(&sdp) {
                    if media
                        .accept_renegotiation_answer(b_id.clone(), answer)
                        .await
                        .is_ok()
                    {
                        reneg_answered = true;
                    }
                }
            }
        }

        if a.is_connected() && b.is_connected() && reneg_answered {
            a.write_video_frame_with_descriptor(publisher_mid, &payload, &descriptor, frame_idx);
            frame_idx += 1;
        }

        let a_sock = a.socket.clone();
        let b_sock = b.socket.clone();
        let mut abuf = [0u8; 2048];
        let mut bbuf = [0u8; 2048];
        tokio::select! {
            r = a_sock.recv_from(&mut abuf) => {
                if let Ok((n, src)) = r {
                    a.feed_receive(&abuf[..n], src, sfu_addr);
                }
            }
            r = b_sock.recv_from(&mut bbuf) => {
                if let Ok((n, src)) = r {
                    b.feed_receive(&bbuf[..n], src, sfu_addr);
                }
            }
            () = tokio::time::sleep(Duration::from_millis(5)) => {
                a.feed_timeout();
                b.feed_timeout();
            }
        }
    }

    assert!(
        received,
        "subscriber B never received A's video frame within \
         {CROSS_FANOUT_DEADLINE:?}; a_connected={} b_connected={} \
         reneg_answered={}",
        a.is_connected(),
        b.is_connected(),
        reneg_answered,
    );

    let arrived = b
        .last_frame_descriptor
        .clone()
        .expect("the relayed frame must carry a frame descriptor");

    // Every frame-level field survives byte for byte. This is the payload of
    // the change: the frame type (encoded as "has no dependency diffs") and
    // the 1280x720 resolution are what the receiver has left to recognise a
    // key frame by, once SFrame has overwritten the bitstream header.
    assert_eq!(
        arrived[1..],
        descriptor[1..],
        "the frame-level fields must cross the relay unaltered"
    );
    assert!(
        arrived.len() >= 4,
        "a first-packet value shorter than four bytes is rejected outright by \
         libwebrtc's parser"
    );
    assert_ne!(
        arrived[0] & 0x80,
        0,
        "what reaches the subscriber must be a first-packet value (B set), \
         not a bare continuation byte"
    );
    assert_eq!(
        arrived[0] & 0x0f,
        descriptor[0] & 0x0f,
        "byte 0's low nibble (the dependency flag and temporal layer) is \
         frame-level and must not be touched"
    );
    // The payload is larger than one RTP packet, so this frame is several
    // packets on both legs and its FIRST packet does not end the frame. `E`
    // clear here is the publisher's packetizer writing the first packet's own
    // value and the SFU relaying it untouched: the fixture had `E` set, and a
    // relay that put that frame-level value on every packet would have told
    // the receiver each one was a whole frame.
    assert_eq!(
        arrived[0] & 0x40,
        0,
        "the first packet of a multi-packet frame must not claim to end it"
    );
}

/// **ADR-0083, screen share** — a publisher's video line declared in its
/// offer's `screen_mids` reaches a subscriber over the live SFU as a screen
/// track: the SFU's renegotiation offer names it under the MSID stream id
/// `<publisher>~screen` (the `~` restored after str0m drops it), and the
/// publisher's frame arrives at the subscriber byte for byte. The
/// subscriber has said, in `video-wants`, that it shows only the light
/// picture of this publisher: a screen is forwarded all the same.
///
/// Opacity is unaffected: the SFU relays the payload as it arrived (C-39).
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn a_declared_screen_line_reaches_a_subscriber_as_a_screen_track_verbatim() {
    use lakko_svc_sfu::protocol::{VideoQuality, VideoWant};
    let peers = PeerRegistry::new();
    let b_id = fresh_peer_id();
    let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
    assert!(
        peers
            .register(Peer {
                id: b_id.clone(),
                room: "screen-share-test".to_owned(),
                outbound: Some(b_tx),
            })
            .await,
        "fresh PeerId must register cleanly"
    );

    let (media, sfu_addr) = spawn_media_task("127.0.0.1:0".parse().unwrap(), peers)
        .await
        .expect("UDP bind on loopback:0 succeeds");

    let mut a = SyntheticClient::new_with_frame_descriptor().await;
    let mut b = SyntheticClient::new_with_frame_descriptor().await;
    let a_id = fresh_peer_id();

    let (b_offer, b_pending) = b.create_video_subscriber_offer();
    let b_answer = media
        .accept_offer(b_id.clone(), "screen-share-call".to_owned(), b_offer)
        .await
        .expect("SFU accepts B's RecvOnly video subscriber offer");
    b.accept_answer(b_pending, &b_answer)
        .expect("B applies the SFU's answer");

    // A publishes one video line and declares it as its screen.
    let (a_offer, a_pending, screen_mid) = a.create_video_publisher_offer();
    let a_answer = media
        .accept_offer_with_screen(
            a_id.clone(),
            "screen-share-call".to_owned(),
            a_offer,
            Vec::new(),
            Vec::new(),
            vec![screen_mid.to_string()],
        )
        .await
        .expect("SFU accepts A's screen-share offer");
    a.accept_answer(a_pending, &a_answer)
        .expect("A applies the SFU's answer");

    // B shows only the light picture of A: a camera main track would be
    // paused, a screen is not.
    media
        .set_video_wants(
            b_id.clone(),
            vec![VideoWant {
                peer: a_id.to_string(),
                quality: VideoQuality::Light,
            }],
        )
        .expect("the wants are queued");

    let descriptor: Vec<u8> = vec![0xf0, 0x01, 0x2a, 0x00, 0x05, 0x00, 0x02, 0xd0];
    let payload: Vec<u8> = (0..3000_u32)
        .map(|i| {
            u8::try_from(i % 241)
                .unwrap_or(0)
                .wrapping_mul(11)
                .wrapping_add(5)
        })
        .collect();

    let deadline = Instant::now() + CROSS_FANOUT_DEADLINE;
    let mut reneg_answered = false;
    let mut pending_reneg: Option<String> = None;
    let mut offers: Vec<String> = Vec::new();
    let mut frame_idx: u64 = 0;
    let mut received = false;

    while Instant::now() < deadline {
        let _ = a.drain().await;
        let b_payloads = b.drain().await;
        if b_payloads
            .iter()
            .any(|p| p.as_slice() == payload.as_slice())
        {
            received = true;
            break;
        }

        while let Ok(frame) = b_rx.try_recv() {
            if let ServerFrame::SdpOffer { sdp } = frame {
                offers.push(sdp.clone());
                pending_reneg = Some(sdp);
            }
        }
        if !reneg_answered && b.is_connected() {
            if let Some(sdp) = pending_reneg.take() {
                if let Ok(answer) = b.answer_renegotiation(&sdp) {
                    if media
                        .accept_renegotiation_answer(b_id.clone(), answer)
                        .await
                        .is_ok()
                    {
                        reneg_answered = true;
                    }
                }
            }
        }

        if a.is_connected() && b.is_connected() && reneg_answered {
            a.write_video_frame_with_descriptor(screen_mid, &payload, &descriptor, frame_idx);
            frame_idx += 1;
        }

        let a_sock = a.socket.clone();
        let b_sock = b.socket.clone();
        let mut abuf = [0u8; 2048];
        let mut bbuf = [0u8; 2048];
        tokio::select! {
            r = a_sock.recv_from(&mut abuf) => {
                if let Ok((n, src)) = r {
                    a.feed_receive(&abuf[..n], src, sfu_addr);
                }
            }
            r = b_sock.recv_from(&mut bbuf) => {
                if let Ok((n, src)) = r {
                    b.feed_receive(&bbuf[..n], src, sfu_addr);
                }
            }
            () = tokio::time::sleep(Duration::from_millis(5)) => {
                a.feed_timeout();
                b.feed_timeout();
            }
        }
    }

    assert!(
        received,
        "subscriber B never received A's screen frame within \
         {CROSS_FANOUT_DEADLINE:?}; a_connected={} b_connected={} \
         reneg_answered={}",
        a.is_connected(),
        b.is_connected(),
        reneg_answered,
    );
    let msid_of_screen = format!("a=msid:{a_id}~screen ");
    assert!(
        offers.iter().any(|sdp| sdp.contains(&msid_of_screen)),
        "the SFU's offer to B must name A's screen under `<peer>~screen`; \
         offers were:\n{offers:?}"
    );
    let msid_of_main = format!("a=msid:{a_id} ");
    assert!(
        !offers.iter().any(|sdp| sdp.contains(&msid_of_main)),
        "a declared screen line is never opened as a main track"
    );
}

/// **The case the live failure was, and the harness did not cover** — a
/// subscriber that joined a call **audio-only**, and later has a video track
/// renegotiated onto it by the SFU.
///
/// ## Why this shape specifically
///
/// The extension map is **per peer**, and `ExtensionMap::remap` only pins an
/// id for an extension that peer itself offered. A peer that joined
/// audio-only never offered a video m-line, so nothing pins the frame
/// descriptor's id — and the SFU's later offer of a video track to that peer
/// carries whatever the SFU registered. Every other test here has the
/// subscriber offering video up front, which pins the id and hides the
/// question entirely.
///
/// This asserts the id the SFU offers is **12**, libwebrtc's own
/// (`webrtc_video_engine.cc:812-852` at `branch-heads/6422`), so that the
/// pinned and the unpinned case agree. The previous value, 5, is free inside
/// str0m's `ExtensionMap::standard()` but is playout-delay in libwebrtc's
/// table — a real divergence, and invisible to a harness whose subscriber is
/// also str0m.
///
/// The test then drives media through anyway, because an id assertion alone
/// would not catch a renegotiation that is well-formed and still never
/// delivers.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn audio_only_subscriber_is_offered_video_on_libwebrtcs_own_extmap_id() {
    let peers = PeerRegistry::new();
    let b_id = fresh_peer_id();
    let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
    assert!(
        peers
            .register(Peer {
                id: b_id.clone(),
                room: "audio-only-join".to_owned(),
                outbound: Some(b_tx),
            })
            .await,
        "fresh PeerId must register cleanly"
    );

    let (media, sfu_addr) = spawn_media_task("127.0.0.1:0".parse().unwrap(), peers)
        .await
        .expect("UDP bind on loopback:0 succeeds");

    let mut a = SyntheticClient::new_with_frame_descriptor().await;
    let mut b = SyntheticClient::new_with_frame_descriptor().await;
    let a_id = fresh_peer_id();

    // B joins AUDIO-ONLY — no video m-line, so nothing pins the frame
    // descriptor's id for this peer.
    let (b_offer, b_pending) = b.create_subscriber_offer();
    assert!(
        !b_offer.contains(GENERIC_FRAME_DESCRIPTOR_URI),
        "the point of this test is that B never offers the extension itself"
    );
    let b_answer = media
        .accept_offer(b_id.clone(), "audio-only-call".to_owned(), b_offer)
        .await
        .expect("SFU accepts B's audio-only subscriber offer");
    b.accept_answer(b_pending, &b_answer)
        .expect("B applies the SFU's answer");

    let (a_offer, a_pending, publisher_mid) = a.create_video_publisher_offer();
    let a_answer = media
        .accept_offer(a_id.clone(), "audio-only-call".to_owned(), a_offer)
        .await
        .expect("SFU accepts A's SendOnly video publisher offer");
    a.accept_answer(a_pending, &a_answer)
        .expect("A applies the SFU's answer");

    let descriptor: Vec<u8> = vec![0xf0, 0x01, 0x2a, 0x00, 0x05, 0x00, 0x02, 0xd0];
    let payload: Vec<u8> = (0..3000_u32)
        .map(|i| {
            u8::try_from(i % 251)
                .unwrap_or(0)
                .wrapping_mul(7)
                .wrapping_add(3)
        })
        .collect();

    let deadline = Instant::now() + CROSS_FANOUT_DEADLINE;
    let mut reneg_answered = false;
    let mut pending_reneg: Option<String> = None;
    let mut offered_id: Option<String> = None;
    let mut frame_idx: u64 = 0;
    let mut received = false;

    while Instant::now() < deadline {
        let _ = a.drain().await;
        let b_payloads = b.drain().await;
        if b_payloads
            .iter()
            .any(|p| p.as_slice() == payload.as_slice())
        {
            received = true;
            break;
        }

        while let Ok(frame) = b_rx.try_recv() {
            if let ServerFrame::SdpOffer { sdp } = frame {
                pending_reneg = Some(sdp);
            }
        }
        if !reneg_answered && b.is_connected() {
            if let Some(sdp) = pending_reneg.take() {
                offered_id = sdp
                    .lines()
                    .find(|l| {
                        l.starts_with("a=extmap:") && l.ends_with(GENERIC_FRAME_DESCRIPTOR_URI)
                    })
                    .and_then(|l| l.strip_prefix("a=extmap:"))
                    .and_then(|rest| rest.split_whitespace().next())
                    .map(std::borrow::ToOwned::to_owned);
                if let Ok(answer) = b.answer_renegotiation(&sdp) {
                    if media
                        .accept_renegotiation_answer(b_id.clone(), answer)
                        .await
                        .is_ok()
                    {
                        reneg_answered = true;
                    }
                }
            }
        }

        if a.is_connected() && b.is_connected() && reneg_answered {
            a.write_video_frame_with_descriptor(publisher_mid, &payload, &descriptor, frame_idx);
            frame_idx += 1;
        }

        let a_sock = a.socket.clone();
        let b_sock = b.socket.clone();
        let mut abuf = [0u8; 2048];
        let mut bbuf = [0u8; 2048];
        tokio::select! {
            r = a_sock.recv_from(&mut abuf) => {
                if let Ok((n, src)) = r {
                    a.feed_receive(&abuf[..n], src, sfu_addr);
                }
            }
            r = b_sock.recv_from(&mut bbuf) => {
                if let Ok((n, src)) = r {
                    b.feed_receive(&bbuf[..n], src, sfu_addr);
                }
            }
            () = tokio::time::sleep(Duration::from_millis(5)) => {
                a.feed_timeout();
                b.feed_timeout();
            }
        }
    }

    assert_eq!(
        offered_id.as_deref(),
        Some("12"),
        "the SFU must offer the frame descriptor on libwebrtc's own id when \
         nothing has pinned it for this peer; 5 is playout-delay there"
    );
    assert!(
        received,
        "the audio-only subscriber never received the renegotiated video \
         within {CROSS_FANOUT_DEADLINE:?}; a_connected={} b_connected={} \
         reneg_answered={}",
        a.is_connected(),
        b.is_connected(),
        reneg_answered,
    );
    let arrived = b
        .last_frame_descriptor
        .clone()
        .expect("the renegotiated video must carry a frame descriptor too");
    assert_eq!(
        arrived[1..],
        descriptor[1..],
        "the frame-level fields must survive on this path as well"
    );
}

/// **The late joiner** — A publishes first, B joins the call afterwards, and
/// B must be offered the track A was already sending.
///
/// ## Why every other test here misses this
///
/// `propagate_new_tracks_in` is driven by `Event::MediaAdded` — a track being
/// *announced* — and opens it onto every peer present at that instant. It is
/// one-directional by construction. Nothing opened the tracks a call already
/// had onto a peer that arrived later, so a late joiner received only what
/// was published after it got there.
///
/// Every other test in this file has the subscriber established first and the
/// publisher arriving last, which is exactly the order in which the missing
/// direction never matters. A live three-person call with the publisher
/// joining twelve minutes before the subscriber produced not one downstream
/// track aimed at the subscriber, for the whole call.
///
/// This asserts the repaired direction end to end: A's frame reaches B
/// verbatim even though A announced it before B existed.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn a_peer_joining_a_call_in_progress_receives_the_tracks_already_in_it() {
    let peers = PeerRegistry::new();
    let b_id = fresh_peer_id();
    let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
    assert!(
        peers
            .register(Peer {
                id: b_id.clone(),
                room: "late-joiner".to_owned(),
                outbound: Some(b_tx),
            })
            .await,
        "fresh PeerId must register cleanly"
    );

    let (media, sfu_addr) = spawn_media_task("127.0.0.1:0".parse().unwrap(), peers)
        .await
        .expect("UDP bind on loopback:0 succeeds");

    let mut a = SyntheticClient::new_with_frame_descriptor().await;
    let a_id = fresh_peer_id();

    // A joins FIRST and publishes.
    let (a_offer, a_pending, publisher_mid) = a.create_video_publisher_offer();
    let a_answer = media
        .accept_offer(a_id.clone(), "late-joiner-call".to_owned(), a_offer)
        .await
        .expect("SFU accepts A's publisher offer");
    a.accept_answer(a_pending, &a_answer)
        .expect("A applies the SFU's answer");

    // Drive A to Connected and let it announce its track, so the
    // announcement has definitively happened BEFORE B exists. Without this
    // the test could pass by accident through the forward direction.
    let deadline = Instant::now() + HANDSHAKE_DEADLINE;
    while Instant::now() < deadline && !a.is_connected() {
        let _ = a.drain().await;
        let a_sock = a.socket.clone();
        let mut abuf = [0u8; 2048];
        tokio::select! {
            r = a_sock.recv_from(&mut abuf) => {
                if let Ok((n, src)) = r {
                    a.feed_receive(&abuf[..n], src, sfu_addr);
                }
            }
            () = tokio::time::sleep(Duration::from_millis(5)) => a.feed_timeout(),
        }
    }
    assert!(a.is_connected(), "publisher A must connect before B joins");

    // NOW B joins, audio-only, the way a peer answering a call in progress
    // does.
    let mut b = SyntheticClient::new_with_frame_descriptor().await;
    let (b_offer, b_pending) = b.create_subscriber_offer();
    let b_answer = media
        .accept_offer(b_id.clone(), "late-joiner-call".to_owned(), b_offer)
        .await
        .expect("SFU accepts B's offer");
    b.accept_answer(b_pending, &b_answer)
        .expect("B applies the SFU's answer");

    let descriptor: Vec<u8> = vec![0xf0, 0x01, 0x2a, 0x00, 0x05, 0x00, 0x02, 0xd0];
    let payload: Vec<u8> = (0..3000_u32)
        .map(|i| {
            u8::try_from(i % 251)
                .unwrap_or(0)
                .wrapping_mul(7)
                .wrapping_add(3)
        })
        .collect();

    let deadline = Instant::now() + CROSS_FANOUT_DEADLINE;
    let mut reneg_answered = false;
    let mut offered = false;
    let mut pending_reneg: Option<String> = None;
    let mut frame_idx: u64 = 0;
    let mut received = false;

    while Instant::now() < deadline {
        let _ = a.drain().await;
        let b_payloads = b.drain().await;
        if b_payloads
            .iter()
            .any(|p| p.as_slice() == payload.as_slice())
        {
            received = true;
            break;
        }

        while let Ok(frame) = b_rx.try_recv() {
            if let ServerFrame::SdpOffer { sdp } = frame {
                offered = true;
                pending_reneg = Some(sdp);
            }
        }
        if !reneg_answered && b.is_connected() {
            if let Some(sdp) = pending_reneg.take() {
                if let Ok(answer) = b.answer_renegotiation(&sdp) {
                    if media
                        .accept_renegotiation_answer(b_id.clone(), answer)
                        .await
                        .is_ok()
                    {
                        reneg_answered = true;
                    }
                }
            }
        }

        if a.is_connected() && b.is_connected() && reneg_answered {
            a.write_video_frame_with_descriptor(publisher_mid, &payload, &descriptor, frame_idx);
            frame_idx += 1;
        }

        let a_sock = a.socket.clone();
        let b_sock = b.socket.clone();
        let mut abuf = [0u8; 2048];
        let mut bbuf = [0u8; 2048];
        tokio::select! {
            r = a_sock.recv_from(&mut abuf) => {
                if let Ok((n, src)) = r {
                    a.feed_receive(&abuf[..n], src, sfu_addr);
                }
            }
            r = b_sock.recv_from(&mut bbuf) => {
                if let Ok((n, src)) = r {
                    b.feed_receive(&bbuf[..n], src, sfu_addr);
                }
            }
            () = tokio::time::sleep(Duration::from_millis(5)) => {
                a.feed_timeout();
                b.feed_timeout();
            }
        }
    }

    // Asserted separately from delivery: "never offered" and "offered but
    // never delivered" are different bugs, and the live failure was the
    // former. A bare delivery assertion would not have said which.
    assert!(
        offered,
        "the SFU never offered the late joiner the track the call already had"
    );
    assert!(
        received,
        "the late joiner never received the already-published video within \
         {CROSS_FANOUT_DEADLINE:?}; a_connected={} b_connected={} \
         reneg_answered={}",
        a.is_connected(),
        b.is_connected(),
        reneg_answered,
    );
}

/// The mid of the video m-section in an SFU renegotiation offer — the
/// subscriber-side handle a PLI is sent on.
fn video_mid_in(sdp: &str) -> Option<Mid> {
    let mut in_video = false;
    for line in sdp.lines() {
        if let Some(rest) = line.strip_prefix("m=") {
            in_video = rest.starts_with("video");
        } else if in_video {
            if let Some(mid) = line.strip_prefix("a=mid:") {
                return Some(Mid::from(mid.trim()));
            }
        }
    }
    None
}

/// Pump both clients for `for_how_long`, feeding sockets and timeouts,
/// while `publish` writes a frame from A on each pass if set.
async fn pump(
    a: &mut SyntheticClient,
    b: &mut SyntheticClient,
    sfu_addr: SocketAddr,
    for_how_long: Duration,
    publish: Option<(Mid, &[u8], &[u8], &mut u64)>,
) {
    let until = Instant::now() + for_how_long;
    let mut publish = publish;
    while Instant::now() < until {
        let _ = a.drain().await;
        let _ = b.drain().await;
        if let Some((mid, payload, descriptor, idx)) = publish.as_mut() {
            a.write_video_frame_with_descriptor(*mid, payload, descriptor, **idx);
            **idx += 1;
        }
        let a_sock = a.socket.clone();
        let b_sock = b.socket.clone();
        let mut abuf = [0u8; 2048];
        let mut bbuf = [0u8; 2048];
        tokio::select! {
            r = a_sock.recv_from(&mut abuf) => {
                if let Ok((n, src)) = r {
                    a.feed_receive(&abuf[..n], src, sfu_addr);
                }
            }
            r = b_sock.recv_from(&mut bbuf) => {
                if let Ok((n, src)) = r {
                    b.feed_receive(&bbuf[..n], src, sfu_addr);
                }
            }
            () = tokio::time::sleep(Duration::from_millis(5)) => {
                a.feed_timeout();
                b.feed_timeout();
            }
        }
    }
}

/// **A late joiner's first picture is requested, not awaited.**
///
/// The previous test proves a late joiner is *offered* the track. This one
/// proves it can *decode* it in bounded time. A subscriber that joins after
/// the publisher's initial keyframe holds no reference for the deltas it
/// starts receiving; unless someone asks the publisher for a new keyframe,
/// it waits for the next natural one — which in a live call was five
/// minutes, during which the receiver sent 1475 PLIs that nothing relayed.
///
/// Two things are asserted, against the publisher's own view (str0m
/// surfaces an incoming PLI as `Event::KeyframeRequest`):
///
/// 1. After the joiner answers the downstream offer, the publisher receives
///    a keyframe request **without the joiner having sent any PLI** — the
///    SFU asked on the joiner's behalf, at the moment the joiner could use
///    it.
/// 2. A burst of the joiner's own PLIs reaches the publisher as **one**
///    request within [`KEYFRAME_REQUEST_MIN_INTERVAL`], not one per PLI.
///
/// The publisher streams from before the joiner exists, which is both the
/// realistic shape and the one in which a publisher-side receive stream
/// exists for the SFU to send a PLI on at answer time.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn a_late_joiners_first_keyframe_is_requested_and_pli_bursts_collapse() {
    use lakko_svc_sfu::media::KEYFRAME_REQUEST_MIN_INTERVAL;

    let peers = PeerRegistry::new();
    let b_id = fresh_peer_id();
    let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
    assert!(
        peers
            .register(Peer {
                id: b_id.clone(),
                room: "keyframe-relay".to_owned(),
                outbound: Some(b_tx),
            })
            .await
    );
    let (media, sfu_addr) = spawn_media_task("127.0.0.1:0".parse().unwrap(), peers)
        .await
        .expect("UDP bind on loopback:0 succeeds");

    let descriptor: Vec<u8> = vec![0xf0, 0x01, 0x2a, 0x00, 0x05, 0x00, 0x02, 0xd0];
    let payload: Vec<u8> = (0..3000_u32)
        .map(|i| {
            u8::try_from(i % 251)
                .unwrap_or(0)
                .wrapping_mul(7)
                .wrapping_add(3)
        })
        .collect();
    let mut frame_idx: u64 = 0;

    // A joins first, connects, and starts streaming into an empty call.
    let mut a = SyntheticClient::new_with_frame_descriptor().await;
    let a_id = fresh_peer_id();
    let (a_offer, a_pending, publisher_mid) = a.create_video_publisher_offer();
    let a_answer = media
        .accept_offer(a_id.clone(), "keyframe-call".to_owned(), a_offer)
        .await
        .expect("SFU accepts A's publisher offer");
    a.accept_answer(a_pending, &a_answer)
        .expect("A applies answer");
    let mut placeholder = SyntheticClient::new().await; // never connected; pump wants two
    let deadline = Instant::now() + HANDSHAKE_DEADLINE;
    while Instant::now() < deadline && !a.is_connected() {
        pump(
            &mut a,
            &mut placeholder,
            sfu_addr,
            Duration::from_millis(20),
            None,
        )
        .await;
    }
    assert!(a.is_connected(), "publisher A must connect before B joins");
    // Stream for a while so the SFU has a live receive stream for A's video
    // — the thing a PLI is sent on.
    pump(
        &mut a,
        &mut placeholder,
        sfu_addr,
        Duration::from_millis(300),
        Some((publisher_mid, &payload, &descriptor, &mut frame_idx)),
    )
    .await;
    let requests_before_join = a.keyframe_requests_seen;

    // B joins the call in progress.
    let mut b = SyntheticClient::new_with_frame_descriptor().await;
    let (b_offer, b_pending) = b.create_subscriber_offer();
    let b_answer = media
        .accept_offer(b_id.clone(), "keyframe-call".to_owned(), b_offer)
        .await
        .expect("SFU accepts B's offer");
    b.accept_answer(b_pending, &b_answer)
        .expect("B applies answer");

    // Drive to: B connected, renegotiation answered, B receiving.
    let deadline = Instant::now() + CROSS_FANOUT_DEADLINE;
    let mut reneg_answered = false;
    let mut b_video_mid: Option<Mid> = None;
    let mut pending_reneg: Option<String> = None;
    let mut b_received = false;
    while Instant::now() < deadline && !(reneg_answered && b_received) {
        let _ = a.drain().await;
        if !b.drain().await.is_empty() {
            b_received = true;
        }
        while let Ok(frame) = b_rx.try_recv() {
            if let ServerFrame::SdpOffer { sdp } = frame {
                pending_reneg = Some(sdp);
            }
        }
        if !reneg_answered && b.is_connected() {
            if let Some(sdp) = pending_reneg.take() {
                b_video_mid = video_mid_in(&sdp);
                if let Ok(answer) = b.answer_renegotiation(&sdp) {
                    if media
                        .accept_renegotiation_answer(b_id.clone(), answer)
                        .await
                        .is_ok()
                    {
                        reneg_answered = true;
                    }
                }
            }
        }
        a.write_video_frame_with_descriptor(publisher_mid, &payload, &descriptor, frame_idx);
        frame_idx += 1;
        let a_sock = a.socket.clone();
        let b_sock = b.socket.clone();
        let mut abuf = [0u8; 2048];
        let mut bbuf = [0u8; 2048];
        tokio::select! {
            r = a_sock.recv_from(&mut abuf) => {
                if let Ok((n, src)) = r { a.feed_receive(&abuf[..n], src, sfu_addr); }
            }
            r = b_sock.recv_from(&mut bbuf) => {
                if let Ok((n, src)) = r { b.feed_receive(&bbuf[..n], src, sfu_addr); }
            }
            () = tokio::time::sleep(Duration::from_millis(5)) => {
                a.feed_timeout();
                b.feed_timeout();
            }
        }
    }
    assert!(reneg_answered, "B must answer the downstream offer");
    assert!(b_received, "B must be receiving A's video");
    let b_video_mid = b_video_mid.expect("the renegotiation offer carries a video mid");

    // (1) The SFU asked A for a keyframe on B's behalf. B has sent no PLI
    //     of its own, so any request A sees came from the downstream-opened
    //     path. Allow a few more ticks for the PLI to cross the loopback.
    pump(
        &mut a,
        &mut b,
        sfu_addr,
        Duration::from_millis(200),
        Some((publisher_mid, &payload, &descriptor, &mut frame_idx)),
    )
    .await;
    assert!(
        a.keyframe_requests_seen > requests_before_join,
        "the publisher must be asked for a keyframe when a late joiner's track \
         opens, without the joiner having to ask; before={requests_before_join} \
         after={}",
        a.keyframe_requests_seen
    );

    // Let the rate-limit window expire, then establish a quiet baseline so
    // the burst assertion below measures the burst and nothing else.
    pump(
        &mut a,
        &mut b,
        sfu_addr,
        KEYFRAME_REQUEST_MIN_INTERVAL + Duration::from_millis(200),
        Some((publisher_mid, &payload, &descriptor, &mut frame_idx)),
    )
    .await;
    let quiet_start = a.keyframe_requests_seen;
    pump(
        &mut a,
        &mut b,
        sfu_addr,
        Duration::from_millis(200),
        Some((publisher_mid, &payload, &descriptor, &mut frame_idx)),
    )
    .await;
    assert_eq!(
        a.keyframe_requests_seen, quiet_start,
        "no keyframe requests should reach the publisher while nobody asks"
    );

    // (2) B fires a burst of PLIs, the way a decoder with no reference does.
    //     They must collapse to exactly one request at A inside the window.
    let baseline = a.keyframe_requests_seen;
    let mut sent = 0;
    for _ in 0..10 {
        if b.send_pli(b_video_mid) {
            sent += 1;
        }
        pump(
            &mut a,
            &mut b,
            sfu_addr,
            Duration::from_millis(10),
            Some((publisher_mid, &payload, &descriptor, &mut frame_idx)),
        )
        .await;
    }
    assert_eq!(sent, 10, "B must have a receive stream to send PLIs on");
    // Total elapsed since the burst began stays well under the window.
    pump(
        &mut a,
        &mut b,
        sfu_addr,
        Duration::from_millis(150),
        Some((publisher_mid, &payload, &descriptor, &mut frame_idx)),
    )
    .await;
    assert_eq!(
        a.keyframe_requests_seen - baseline,
        1,
        "ten subscriber PLIs inside one window must reach the publisher as \
         exactly one keyframe request"
    );

    // (3) The nine PLIs that followed the first are not relayed and not
    //     owed: the subscriber has just been asked for, and one that goes
    //     on asking is relayed again only after
    //     `PLI_RELAY_FIRST_INTERVAL` (then less and less often). Before
    //     this rule each burst cost a second request when the window
    //     ended, and a receiver that never stopped asking cost its
    //     publisher two key frames a second for a whole call.
    pump(
        &mut a,
        &mut b,
        sfu_addr,
        KEYFRAME_REQUEST_MIN_INTERVAL + Duration::from_millis(200),
        Some((publisher_mid, &payload, &descriptor, &mut frame_idx)),
    )
    .await;
    assert_eq!(
        a.keyframe_requests_seen - baseline,
        1,
        "the repeats of one burst do not become a second request by themselves"
    );
    // A subscriber that still has no picture asks again; a second later
    // that is relayed, once, however often it asks.
    pump(
        &mut a,
        &mut b,
        sfu_addr,
        Duration::from_millis(400),
        Some((publisher_mid, &payload, &descriptor, &mut frame_idx)),
    )
    .await;
    for _ in 0..5 {
        assert!(b.send_pli(b_video_mid));
        pump(
            &mut a,
            &mut b,
            sfu_addr,
            Duration::from_millis(10),
            Some((publisher_mid, &payload, &descriptor, &mut frame_idx)),
        )
        .await;
    }
    pump(
        &mut a,
        &mut b,
        sfu_addr,
        KEYFRAME_REQUEST_MIN_INTERVAL + Duration::from_millis(200),
        Some((publisher_mid, &payload, &descriptor, &mut frame_idx)),
    )
    .await;
    assert_eq!(
        a.keyframe_requests_seen - baseline,
        2,
        "a subscriber that asks again after the wait is relayed, once"
    );
}

/// One pass of socket activity for a publisher and a subscriber: whichever
/// datagram arrives first is fed to its client, or both get a timeout tick.
async fn tick_pair(a: &mut SyntheticClient, b: &mut SyntheticClient, sfu_addr: SocketAddr) {
    let a_sock = a.socket.clone();
    let b_sock = b.socket.clone();
    let mut abuf = [0u8; 2048];
    let mut bbuf = [0u8; 2048];
    tokio::select! {
        r = a_sock.recv_from(&mut abuf) => {
            if let Ok((n, src)) = r {
                a.feed_receive(&abuf[..n], src, sfu_addr);
            }
        }
        r = b_sock.recv_from(&mut bbuf) => {
            if let Ok((n, src)) = r {
                b.feed_receive(&bbuf[..n], src, sfu_addr);
            }
        }
        () = tokio::time::sleep(Duration::from_millis(5)) => {
            a.feed_timeout();
            b.feed_timeout();
        }
    }
}

/// A lost audio packet costs the listener that one packet and nothing more.
///
/// The SFU forwards RTP packets as they arrive (ADR-0066): a packet lost on
/// the way to it is a gap in what it forwards, and the packets after the gap
/// go out at once. Before that it assembled frames, and behind a missing
/// packet str0m held the frames that followed until the packet arrived or a
/// set number of frames had queued. Audio is never retransmitted, so with
/// str0m's default of fifteen frames every lost 20 ms packet stopped a
/// publisher's audio for 300 ms and then released fifteen frames at once; a
/// listener's jitter buffer grows to absorb that and the call runs with a
/// constant lag (found on a live call over a Wi-Fi link losing 1-3% of
/// packets, 2026-10-02).
///
/// Here one media packet is dropped on the way to the SFU and only a few
/// frames follow it (fewer than fifteen). The subscriber gets every one of
/// them; an SFU that waited for fifteen frames behind the gap would deliver
/// none, since nothing else is ever sent.
///
/// The subscriber is built with [`SyntheticClient::new_releasing_audio_at_once`]:
/// it is itself a str0m frame-mode receiver, it now sees the gap the SFU
/// passes on, and with the default setting it would do the fifteen-frame
/// wait itself and hide what the SFU did.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn a_lost_audio_packet_does_not_hold_back_the_frames_after_it() {
    const FOLLOWING: u64 = 3;
    let payload = |i: u64| format!("audio frame {i:04} of the lost-packet test").into_bytes();

    let peers = PeerRegistry::new();
    let b_id = fresh_peer_id();
    let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
    assert!(
        peers
            .register(Peer {
                id: b_id.clone(),
                room: "audio-gap-test".to_owned(),
                outbound: Some(b_tx),
            })
            .await,
        "fresh PeerId must register cleanly"
    );
    let (media, sfu_addr) = spawn_media_task("127.0.0.1:0".parse().unwrap(), peers)
        .await
        .expect("UDP bind on loopback:0 succeeds");

    let mut a = SyntheticClient::new().await;
    let mut b = SyntheticClient::new_releasing_audio_at_once().await;
    let a_id = fresh_peer_id();

    let (b_offer, b_pending) = b.create_subscriber_offer();
    let b_answer = media
        .accept_offer(b_id.clone(), "audio-gap-call".to_owned(), b_offer)
        .await
        .expect("SFU accepts B's subscriber offer");
    b.accept_answer(b_pending, &b_answer)
        .expect("B applies the SFU's answer");
    let (a_offer, a_pending, mid) = a.create_audio_publisher_offer();
    let a_answer = media
        .accept_offer(a_id, "audio-gap-call".to_owned(), a_offer)
        .await
        .expect("SFU accepts A's publisher offer");
    a.accept_answer(a_pending, &a_answer)
        .expect("A applies the SFU's answer");

    let deadline = Instant::now() + CROSS_FANOUT_DEADLINE;
    let mut reneg_answered = false;
    let mut pending_reneg: Option<String> = None;
    let mut received: Vec<Vec<u8>> = Vec::new();
    let mut next_idx: u64 = 0;

    // 1. Warm up: one new frame per tick until the subscriber has heard one,
    //    so the stream is flowing end to end before anything is lost.
    while Instant::now() < deadline && received.is_empty() {
        let _ = a.drain().await;
        received.extend(b.drain().await);
        while let Ok(frame) = b_rx.try_recv() {
            if let ServerFrame::SdpOffer { sdp } = frame {
                pending_reneg = Some(sdp);
            }
        }
        if !reneg_answered && b.is_connected() {
            if let Some(sdp) = pending_reneg.take() {
                if let Ok(answer) = b.answer_renegotiation(&sdp) {
                    reneg_answered = media
                        .accept_renegotiation_answer(b_id.clone(), answer)
                        .await
                        .is_ok();
                }
            }
        }
        if a.is_connected()
            && b.is_connected()
            && reneg_answered
            && a.write_audio_frame(mid, &payload(next_idx), next_idx)
        {
            next_idx += 1;
        }
        tick_pair(&mut a, &mut b, sfu_addr).await;
    }
    assert!(
        !received.is_empty(),
        "the stream must reach the subscriber before the loss is staged"
    );

    // 2. Lose exactly one media packet. str0m queues a written frame and
    //    sends it on a later tick, so the packet thrown away is the next one
    //    to leave: this frame's, or the last warm-up frame's if that one was
    //    still queued. Which frame was lost is read back below from what the
    //    subscriber never got.
    let _ = a.drain().await;
    assert!(a.write_audio_frame(mid, &payload(next_idx), next_idx));
    next_idx += 1;
    let mut discarded = 0;
    for _ in 0..200 {
        discarded += a.discard_pending_transmits();
        if discarded > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
        a.feed_timeout();
    }
    assert_eq!(discarded, 1, "exactly one media packet must be thrown away");

    // 3. Send only FOLLOWING more frames, then nothing.
    for _ in 0..FOLLOWING {
        assert!(a.write_audio_frame(mid, &payload(next_idx), next_idx));
        next_idx += 1;
        let _ = a.drain().await;
        received.extend(b.drain().await);
        tick_pair(&mut a, &mut b, sfu_addr).await;
    }
    let written = next_idx;

    // 4. Give the SFU two seconds to forward them. No further frame is
    //    written, so an SFU that held packets behind the gap until more
    //    frames arrived would never release them.
    let settle = Instant::now() + Duration::from_secs(2);
    let final_frame = payload(written - 1);
    while Instant::now() < settle && !received.contains(&final_frame) {
        let _ = a.drain().await;
        received.extend(b.drain().await);
        tick_pair(&mut a, &mut b, sfu_addr).await;
    }

    let missing: Vec<u64> = (0..written)
        .filter(|i| !received.contains(&payload(*i)))
        .collect();
    // Frames written before the subscriber's track opened are legitimately
    // absent; they all precede the first frame it heard.
    let first_heard = (0..written)
        .find(|i| received.contains(&payload(*i)))
        .expect("the warm-up proved at least one frame arrived");
    let lost: Vec<u64> = missing.into_iter().filter(|i| *i > first_heard).collect();
    assert_eq!(
        lost.len(),
        1,
        "one packet was dropped, so exactly one frame may be missing after the first \
         one heard; missing {lost:?} of {written} written means the SFU held back the \
         frames behind the gap"
    );
    // The proof only holds while fewer frames followed the loss than str0m's
    // frame-mode hold of fifteen: an SFU waiting that long would have
    // released none of them, and more than one frame would be missing above.
    let after = written - 1 - lost[0];
    assert!(
        (1..15).contains(&after),
        "between 1 and 14 frames must follow the lost one, {after} did"
    );
}

/// The `m=audio` line of the first audio section of an SDP.
fn audio_m_line(sdp: &str) -> Option<&str> {
    sdp.lines().find(|l| l.starts_with("m=audio"))
}

/// Redundant audio (RED, RFC 2198, ADR-0067 step 3) crosses the SFU as it
/// was sent, to the subscribers that can take it, and plain Opus still
/// reaches everyone.
///
/// A client builds the RED container before SFrame seals the frame, so what
/// the SFU sees under the `red` payload type is ciphertext it can neither
/// unwrap nor renumber. Three clients in RTP mode (the only way a str0m
/// endpoint can put a chosen payload type on the wire):
///
/// - A publishes audio and negotiated `red`. It sends packets alternately
///   under the red payload type and under plain Opus on one stream, which is
///   what libwebrtc does when a packet's redundancy does not fit.
/// - B subscribes and negotiated `red`: it gets every packet, each under the
///   payload type it was sent with, payload bytes verbatim.
/// - C subscribes and knows Opus alone: it gets the Opus packets and none of
///   the RED ones.
///
/// The SDP side is asserted too: the SFU's answer to A and its offer to B
/// list `red` before `opus` (libwebrtc sends RED only then), and its offer
/// to C is the same offer, which C narrows to Opus.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn red_audio_reaches_only_the_subscribers_that_negotiated_it() {
    const RED_PT: u8 = 63;
    const OPUS_PT: u8 = 111;
    const PACKETS: u64 = 12;
    let payload = |i: u64| format!("sealed audio packet {i:04} of the red test").into_bytes();
    let pt_of = |i: u64| if i % 2 == 0 { RED_PT } else { OPUS_PT };

    let peers = PeerRegistry::new();
    let mut register = Vec::new();
    for _subscriber in ["subscriber-b", "subscriber-c"] {
        let id = fresh_peer_id();
        let (tx, rx) = mpsc::channel::<ServerFrame>(64);
        assert!(
            peers
                .register(Peer {
                    id: id.clone(),
                    room: "red-test".to_owned(),
                    outbound: Some(tx),
                })
                .await,
            "fresh PeerId must register cleanly"
        );
        register.push((id, rx));
    }
    let (c_id, mut c_rx) = register.pop().expect("two registered");
    let (b_id, mut b_rx) = register.pop().expect("two registered");
    let (media, sfu_addr) = spawn_media_task("127.0.0.1:0".parse().unwrap(), peers)
        .await
        .expect("UDP bind on loopback:0 succeeds");

    let mut a = SyntheticClient::new_rtp_mode(true).await;
    let mut b = SyntheticClient::new_rtp_mode(true).await;
    let mut c = SyntheticClient::new_rtp_mode(false).await;
    let a_id = fresh_peer_id();

    for (client, id) in [(&mut b, &b_id), (&mut c, &c_id)] {
        let (offer, pending) = client.create_subscriber_offer();
        let answer = media
            .accept_offer(id.clone(), "red-call".to_owned(), offer)
            .await
            .expect("SFU accepts a subscriber offer");
        client
            .accept_answer(pending, &answer)
            .expect("subscriber applies the SFU's answer");
    }
    let (a_offer, a_pending, mid) = a.create_audio_publisher_offer();
    assert_eq!(
        audio_m_line(&a_offer),
        Some("m=audio 9 UDP/TLS/RTP/SAVPF 63 111"),
        "the publisher offers red and opus"
    );
    let a_answer = media
        .accept_offer(a_id, "red-call".to_owned(), a_offer)
        .await
        .expect("SFU accepts A's publisher offer");
    assert_eq!(
        audio_m_line(&a_answer),
        Some("m=audio 9 UDP/TLS/RTP/SAVPF 63 111"),
        "the SFU's answer lists red before opus:\n{a_answer}"
    );
    assert!(
        a_answer.contains("a=rtpmap:63 red/48000/2") && a_answer.contains("a=fmtp:63 111/111"),
        "the SFU's answer carries the red payload type and its fmtp:\n{a_answer}"
    );
    a.accept_answer(a_pending, &a_answer)
        .expect("A applies the SFU's answer");

    let deadline = Instant::now() + CROSS_FANOUT_DEADLINE;
    let mut b_reneg = false;
    let mut c_reneg = false;
    let mut b_offer_seen: Option<String> = None;
    let mut c_answer_seen: Option<String> = None;
    let mut written: u64 = 0;
    let mut settle: Option<Instant> = None;
    let mut buf = [0u8; 2048];

    loop {
        let now = Instant::now();
        assert!(now < deadline, "the red test did not finish in time");
        if settle.is_some_and(|at| now >= at) {
            break;
        }
        for client in [&mut a, &mut b, &mut c] {
            let _ = client.drain().await;
        }
        if !b_reneg && b.is_connected() {
            while let Ok(frame) = b_rx.try_recv() {
                if let ServerFrame::SdpOffer { sdp } = frame {
                    let answer = b.answer_renegotiation(&sdp).expect("B answers");
                    b_offer_seen = Some(sdp);
                    b_reneg = media
                        .accept_renegotiation_answer(b_id.clone(), answer)
                        .await
                        .is_ok();
                }
            }
        }
        if !c_reneg && c.is_connected() {
            while let Ok(frame) = c_rx.try_recv() {
                if let ServerFrame::SdpOffer { sdp } = frame {
                    let answer = c.answer_renegotiation(&sdp).expect("C answers");
                    c_answer_seen = Some(answer.clone());
                    c_reneg = media
                        .accept_renegotiation_answer(c_id.clone(), answer)
                        .await
                        .is_ok();
                }
            }
        }
        if a.is_connected()
            && b.is_connected()
            && c.is_connected()
            && b_reneg
            && c_reneg
            && written < PACKETS
            && a.write_audio_rtp(mid, pt_of(written), written, &payload(written))
        {
            written += 1;
            if written == PACKETS {
                // Everything is written; give the last packets time to cross.
                settle = Some(Instant::now() + Duration::from_secs(1));
            }
        }
        for client in [&mut a, &mut b, &mut c] {
            while let Ok((n, src)) = client.socket.try_recv_from(&mut buf) {
                client.feed_receive(&buf[..n], src, sfu_addr);
            }
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
        for client in [&mut a, &mut b, &mut c] {
            client.feed_timeout();
        }
    }

    // What the SFU offered B, and what C made of the same offer.
    let b_offer = b_offer_seen.expect("B was offered the track");
    let offered: Vec<&str> = b_offer
        .lines()
        .filter(|l| l.starts_with("m=audio"))
        .collect();
    assert_eq!(
        offered.last().copied(),
        Some("m=audio 9 UDP/TLS/RTP/SAVPF 63 111"),
        "the SFU's own offer lists red before opus:\n{b_offer}"
    );
    let c_answer = c_answer_seen.expect("C answered the track");
    assert!(
        !c_answer.contains(" red/"),
        "a client without red answers without it:\n{c_answer}"
    );

    let sent: Vec<(u8, Vec<u8>)> = (0..PACKETS).map(|i| (pt_of(i), payload(i))).collect();
    assert_eq!(
        b.rtp_packets, sent,
        "the red subscriber gets every packet, each under the payload type it was \
         sent with, payload verbatim"
    );
    let opus_only: Vec<(u8, Vec<u8>)> = sent
        .iter()
        .filter(|(pt, _)| *pt == OPUS_PT)
        .cloned()
        .collect();
    assert_eq!(
        c.rtp_packets, opus_only,
        "the subscriber without red gets the Opus packets and no RED packet"
    );
    // On the wire, not only in what C's own stack accepted: C would discard
    // a payload type it does not know, so count the media datagrams the SFU
    // actually sent it.
    assert_eq!(
        c.rtp_in.len(),
        opus_only.len(),
        "the SFU sends a subscriber without red no RED packet at all"
    );
    assert_eq!(b.rtp_in.len(), sent.len());
}

/// The RFC 6464 audio-level header extension.
const AUDIO_LEVEL_URI: &str = "urn:ietf:params:rtp-hdrext:ssrc-audio-level";

/// What one run of [`run_audio_level`] saw.
struct AudioLevelRun {
    /// The publisher's offer, the SFU's answer to it, and the SFU's own
    /// offer of the track to the subscriber.
    publisher_offer: String,
    answer_to_publisher: String,
    offer_to_subscriber: String,
    /// One-byte header extension ids on the audio packets the publisher
    /// put on the wire, and on those the subscriber was sent.
    publisher_wire_ids: std::collections::BTreeSet<u8>,
    subscriber_wire_ids: std::collections::BTreeSet<u8>,
    /// Payloads the subscriber received, and those sent.
    received: Vec<Vec<u8>>,
    sent: Vec<Vec<u8>>,
}

/// An audio publisher A and a subscriber B through the live media task. A
/// writes every packet with an audio level and the voice-activity flag set.
///
/// With `sends_anyway`, A applies the SFU's answer with the audio-level
/// line put back into it, so A's own stack believes the extension was
/// negotiated and writes it on the wire: a client that sends the value
/// although the SFU never accepted it.
#[allow(clippy::too_many_lines)]
async fn run_audio_level(sends_anyway: bool) -> AudioLevelRun {
    const OPUS_PT: u8 = 111;
    const PACKETS: u64 = 10;
    let payload = |i: u64| format!("sealed audio packet {i:04} of the level test").into_bytes();

    let peers = PeerRegistry::new();
    let b_id = fresh_peer_id();
    let (tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
    assert!(
        peers
            .register(Peer {
                id: b_id.clone(),
                room: "level-test".to_owned(),
                outbound: Some(tx),
            })
            .await
    );
    let (media, sfu_addr) = spawn_media_task("127.0.0.1:0".parse().unwrap(), peers)
        .await
        .expect("UDP bind on loopback:0 succeeds");

    // Both clients keep str0m's default extension map, which, like a
    // browser's, has the audio level in it.
    let mut a = SyntheticClient::new_rtp_mode(false).await;
    let mut b = SyntheticClient::new_rtp_mode(false).await;
    let a_id = fresh_peer_id();

    let (b_offer, b_pending) = b.create_subscriber_offer();
    let b_answer = media
        .accept_offer(b_id.clone(), "level-call".to_owned(), b_offer)
        .await
        .expect("SFU accepts the subscriber offer");
    b.accept_answer(b_pending, &b_answer).expect("B applies");

    let (a_offer, a_pending, mid) = a.create_audio_publisher_offer();
    let a_answer = media
        .accept_offer(a_id, "level-call".to_owned(), a_offer.clone())
        .await
        .expect("SFU accepts the publisher offer");
    let applied = if sends_anyway {
        let id_line = a_offer
            .lines()
            .find(|l| l.ends_with(AUDIO_LEVEL_URI))
            .expect("the publisher offered the audio level");
        let mid_line = a_answer
            .lines()
            .find(|l| l.starts_with("a=mid:"))
            .expect("the answer has a media line");
        a_answer.replacen(mid_line, &format!("{mid_line}\r\n{id_line}"), 1)
    } else {
        a_answer.clone()
    };
    a.accept_answer(a_pending, &applied).expect("A applies");

    let deadline = Instant::now() + CROSS_FANOUT_DEADLINE;
    let mut b_reneg = false;
    let mut offer_to_subscriber: Option<String> = None;
    let mut written: u64 = 0;
    let mut settle: Option<Instant> = None;
    let mut buf = [0u8; 2048];
    loop {
        let now = Instant::now();
        assert!(
            now < deadline,
            "the audio-level test did not finish in time"
        );
        if settle.is_some_and(|at| now >= at) {
            break;
        }
        for client in [&mut a, &mut b] {
            let _ = client.drain().await;
        }
        if !b_reneg && b.is_connected() {
            while let Ok(frame) = b_rx.try_recv() {
                if let ServerFrame::SdpOffer { sdp } = frame {
                    let answer = b.answer_renegotiation(&sdp).expect("B answers");
                    offer_to_subscriber = Some(sdp);
                    b_reneg = media
                        .accept_renegotiation_answer(b_id.clone(), answer)
                        .await
                        .is_ok();
                }
            }
        }
        if a.is_connected() && b.is_connected() && b_reneg && written < PACKETS {
            let ext = str0m::rtp::ExtensionValues {
                audio_level: Some(-30),
                voice_activity: Some(true),
                ..str0m::rtp::ExtensionValues::default()
            };
            if a.write_audio_rtp_ext(mid, OPUS_PT, written, &payload(written), ext) {
                written += 1;
                if written == PACKETS {
                    settle = Some(Instant::now() + Duration::from_secs(1));
                }
            }
        }
        for client in [&mut a, &mut b] {
            while let Ok((n, src)) = client.socket.try_recv_from(&mut buf) {
                client.feed_receive(&buf[..n], src, sfu_addr);
            }
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
        for client in [&mut a, &mut b] {
            client.feed_timeout();
        }
    }
    drop(media);

    AudioLevelRun {
        publisher_offer: a_offer,
        answer_to_publisher: a_answer,
        offer_to_subscriber: offer_to_subscriber.expect("B was offered the track"),
        publisher_wire_ids: a
            .wire_headers
            .iter()
            .flat_map(|(_, _, header)| one_byte_extension_ids(header))
            .collect(),
        subscriber_wire_ids: b.in_ext_ids.clone(),
        received: b.rtp_packets.iter().map(|(_, p)| p.clone()).collect(),
        sent: (0..PACKETS).map(payload).collect(),
    }
}

/// The id an SDP gives the audio-level extension, if it names it at all.
fn audio_level_id(sdp: &str) -> Option<u8> {
    sdp.lines()
        .find(|l| l.ends_with(AUDIO_LEVEL_URI))
        .and_then(|l| l.strip_prefix("a=extmap:"))
        .and_then(|l| l.split_whitespace().next())
        .and_then(|id| id.parse().ok())
}

/// The SFU negotiates no audio-level extension (RFC 6464) with anyone
/// (`docs/ux/group-call-grid.md` section 10): it is outside the end-to-end
/// encryption and would tell the server who is speaking and how loudly.
///
/// A publisher that offers it, as every libwebrtc client does, is answered
/// without it and so does not send it; the SFU's own offer to a subscriber
/// does not name it; audio itself crosses as before.
#[tokio::test]
async fn the_audio_level_is_negotiated_with_neither_publisher_nor_subscriber() {
    let run = Box::pin(run_audio_level(false)).await;
    let id = audio_level_id(&run.publisher_offer).expect("the publisher offers the audio level");
    assert!(
        !run.answer_to_publisher.contains("audio-level"),
        "the answer does not accept it:\n{}",
        run.answer_to_publisher
    );
    assert!(
        !run.offer_to_subscriber.contains("audio-level"),
        "the SFU's own offer does not name it:\n{}",
        run.offer_to_subscriber
    );
    for sdp in [&run.answer_to_publisher, &run.offer_to_subscriber] {
        assert!(
            sdp.contains("opus/48000/2") && sdp.contains("urn:ietf:params:rtp-hdrext:sdes:mid")
        );
    }
    assert!(
        !run.publisher_wire_ids.contains(&id),
        "a publisher answered without the extension does not send it: {:?}",
        run.publisher_wire_ids
    );
    assert!(!run.publisher_wire_ids.is_empty(), "it still sends others");
    assert!(!run.subscriber_wire_ids.contains(&id));
    assert_eq!(run.received, run.sent, "audio crosses verbatim");
}

/// A client that sends the audio level although the SFU did not accept it
/// gains nothing: the value reaches no other peer. The subscriber's packets
/// carry no extension under the publisher's id for it, and none the SFU's
/// offer to the subscriber did not name.
#[tokio::test]
async fn an_audio_level_sent_anyway_reaches_no_subscriber() {
    let run = Box::pin(run_audio_level(true)).await;
    let id = audio_level_id(&run.publisher_offer).expect("the publisher offers the audio level");
    assert!(!run.answer_to_publisher.contains("audio-level"));
    assert!(
        run.publisher_wire_ids.contains(&id),
        "the misbehaving publisher does put the extension on the wire: {:?}",
        run.publisher_wire_ids
    );
    let offered: std::collections::BTreeSet<u8> = run
        .offer_to_subscriber
        .lines()
        .filter_map(|l| l.strip_prefix("a=extmap:"))
        .filter_map(|l| l.split_whitespace().next())
        .filter_map(|id| id.parse().ok())
        .collect();
    assert!(
        !offered.contains(&id),
        "id {id} is free on the subscriber's leg"
    );
    assert!(
        !run.subscriber_wire_ids.contains(&id),
        "nothing under the audio-level id reaches the subscriber: {:?}",
        run.subscriber_wire_ids
    );
    assert!(
        run.subscriber_wire_ids.is_subset(&offered),
        "the subscriber is sent only extensions the SFU offered it: {:?} of {offered:?}",
        run.subscriber_wire_ids
    );
    assert_eq!(
        run.received, run.sent,
        "the audio itself still crosses verbatim"
    );
}

/// A video publisher A and a video subscriber B, connected through the live
/// media task with A's video already reaching B. The starting point of the
/// two lost-video-packet tests below.
struct VideoPair {
    /// Kept alive for the life of the test: the media task ends when the
    /// last handle to it is dropped.
    _media: MediaTaskHandle,
    sfu_addr: SocketAddr,
    a: SyntheticClient,
    b: SyntheticClient,
    /// A's publishing mid.
    mid: Mid,
    /// Index of the next frame A writes.
    next_idx: u64,
}

/// Frame descriptor A sends with every frame: a first-packet value marking a
/// key frame. Opaque to these tests beyond being well formed.
const LOSS_TEST_DESCRIPTOR: [u8; 8] = [0xf0, 0x01, 0x2a, 0x00, 0x05, 0x00, 0x02, 0xd0];

/// A 3000-byte payload — three RTP packets — whose bytes depend on `idx`, so
/// an assembled frame can be told from every other one.
fn loss_test_frame(idx: u64) -> Vec<u8> {
    let seed = u8::try_from(idx % 200).unwrap_or(0);
    (0..3000_u32)
        .map(|i| {
            u8::try_from(i % 251)
                .unwrap_or(0)
                .wrapping_mul(7)
                .wrapping_add(seed)
        })
        .collect()
}

impl VideoPair {
    /// Connect both clients, answer the SFU's downstream offer, and stream
    /// until B has assembled one of A's frames.
    async fn streaming(call: &str) -> Self {
        let peers = PeerRegistry::new();
        let b_id = fresh_peer_id();
        let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
        assert!(
            peers
                .register(Peer {
                    id: b_id.clone(),
                    room: call.to_owned(),
                    outbound: Some(b_tx),
                })
                .await,
            "fresh PeerId must register cleanly"
        );
        let (media, sfu_addr) = spawn_media_task("127.0.0.1:0".parse().unwrap(), peers)
            .await
            .expect("UDP bind on loopback:0 succeeds");

        let mut a = SyntheticClient::new_with_frame_descriptor().await;
        let mut b = SyntheticClient::new_with_frame_descriptor().await;
        let (b_offer, b_pending) = b.create_video_subscriber_offer();
        let b_answer = media
            .accept_offer(b_id.clone(), call.to_owned(), b_offer)
            .await
            .expect("SFU accepts B's subscriber offer");
        b.accept_answer(b_pending, &b_answer)
            .expect("B applies the SFU's answer");
        let (a_offer, a_pending, mid) = a.create_video_publisher_offer();
        let a_answer = media
            .accept_offer(fresh_peer_id(), call.to_owned(), a_offer)
            .await
            .expect("SFU accepts A's publisher offer");
        a.accept_answer(a_pending, &a_answer)
            .expect("A applies the SFU's answer");

        let deadline = Instant::now() + CROSS_FANOUT_DEADLINE;
        let mut reneg_answered = false;
        let mut pending_reneg: Option<String> = None;
        let mut next_idx: u64 = 0;
        let mut assembled = false;
        while Instant::now() < deadline && !assembled {
            let _ = a.drain().await;
            assembled = !b.drain().await.is_empty();
            while let Ok(frame) = b_rx.try_recv() {
                if let ServerFrame::SdpOffer { sdp } = frame {
                    pending_reneg = Some(sdp);
                }
            }
            if !reneg_answered && b.is_connected() {
                if let Some(sdp) = pending_reneg.take() {
                    if let Ok(answer) = b.answer_renegotiation(&sdp) {
                        reneg_answered = media
                            .accept_renegotiation_answer(b_id.clone(), answer)
                            .await
                            .is_ok();
                    }
                }
            }
            if a.is_connected()
                && b.is_connected()
                && reneg_answered
                && a.write_video_frame_with_descriptor(
                    mid,
                    &loss_test_frame(next_idx),
                    &LOSS_TEST_DESCRIPTOR,
                    next_idx,
                )
            {
                next_idx += 1;
            }
            tick_pair(&mut a, &mut b, sfu_addr).await;
        }
        assert!(
            assembled,
            "A's video must reach B before a loss is staged; a_connected={} b_connected={} \
             reneg_answered={reneg_answered}",
            a.is_connected(),
            b.is_connected(),
        );
        Self {
            _media: media,
            sfu_addr,
            a,
            b,
            mid,
            next_idx,
        }
    }

    /// One pass: both clients send what is ready, take what has arrived, and
    /// run their timers. Returns the frames B assembled.
    async fn tick(&mut self) -> Vec<Vec<u8>> {
        let _ = self.a.drain().await;
        let assembled = self.b.drain().await;
        tick_pair(&mut self.a, &mut self.b, self.sfu_addr).await;
        assembled
    }

    /// Write A's next frame. Returns its index.
    fn write_next_frame(&mut self) -> u64 {
        let idx = self.next_idx;
        assert!(
            self.a.write_video_frame_with_descriptor(
                self.mid,
                &loss_test_frame(idx),
                &LOSS_TEST_DESCRIPTOR,
                idx,
            ),
            "A writes frame {idx}"
        );
        self.next_idx += 1;
        idx
    }

    /// Write nothing more and let everything in flight land: tick until
    /// neither A's send log nor B's arrival log has grown for 100 ms (two
    /// seconds at most). Returns the frames B assembled meanwhile.
    ///
    /// The tests below compare the two logs packet for packet, which is only
    /// meaningful once the last packet A sent has had time to reach B.
    async fn quiesce(&mut self) -> Vec<Vec<u8>> {
        let give_up = Instant::now() + Duration::from_secs(2);
        let mut assembled = Vec::new();
        let mut seen = (self.a.rtp_out.len(), self.b.rtp_in.len());
        let mut quiet_since = Instant::now();
        while Instant::now() < give_up {
            self.a.feed_timeout();
            assembled.extend(self.tick().await);
            let now_seen = (self.a.rtp_out.len(), self.b.rtp_in.len());
            if now_seen != seen {
                seen = now_seen;
                quiet_since = Instant::now();
            } else if quiet_since.elapsed() >= Duration::from_millis(100) {
                break;
            }
        }
        assembled
    }

    /// Write one frame and lose its first packet on the way to the SFU.
    /// Returns the lost packet's header and the index into A's send log
    /// where the packets after it begin.
    async fn write_a_frame_and_lose_its_first_packet(
        &mut self,
        main_ssrc: u32,
    ) -> (RtpSeen, usize) {
        // Everything written so far has left A and crossed, so the frame
        // written next is the next thing to leave.
        let _ = self.quiesce().await;
        let lost_before = self.a.rtp_dropped.len();
        let sent_mark = self.a.rtp_out.len();
        self.a.drop_next_on = Some(main_ssrc);
        self.write_next_frame();
        for _ in 0..200 {
            self.a.feed_timeout();
            let _ = self.tick().await;
            if self.a.drop_next_on.is_none() {
                break;
            }
        }
        let lost: Vec<RtpSeen> = self.a.rtp_dropped[lost_before..]
            .iter()
            .copied()
            .filter(|p| p.ssrc == main_ssrc)
            .collect();
        assert_eq!(lost.len(), 1, "exactly one media packet must be lost");
        (lost[0], sent_mark)
    }

    /// The media packets A put on the wire on `main_ssrc` from `mark` on.
    fn sent_since(&self, mark: usize, main_ssrc: u32) -> Vec<RtpSeen> {
        self.a.rtp_out[mark..]
            .iter()
            .copied()
            .filter(|p| p.ssrc == main_ssrc)
            .collect()
    }

    /// What reached B's socket on its downstream media stream, in order of
    /// arrival. The stream is the SSRC of the first media datagram B got.
    fn arrived_at_subscriber(&self) -> Vec<RtpSeen> {
        let Some(first) = self.b.rtp_in.first() else {
            return Vec::new();
        };
        self.b
            .rtp_in
            .iter()
            .copied()
            .filter(|p| p.ssrc == first.ssrc)
            .collect()
    }
}

/// The fixed distance between the publisher's wire values and the
/// subscriber's (sequence number, RTP timestamp), read off the newest packet
/// on each side: `last_sent` is the last packet A sent, and its counterpart
/// is the packet furthest ahead in what B received.
///
/// Panics unless that packet reached B after `last_sent` left A. Every frame
/// in these tests has the same shape, so an older packet would line up with
/// `last_sent` just as well and every later comparison would be between the
/// wrong packets; the arrival time is what tells the two apart.
fn stream_offsets(last_sent: RtpSeen, arrived: &[RtpSeen]) -> (u16, u32) {
    let first = arrived.first().expect("the subscriber received media");
    let head = arrived
        .iter()
        .max_by_key(|p| p.seq.wrapping_sub(first.seq))
        .expect("the subscriber received media");
    assert!(
        head.at >= last_sent.at,
        "the newest packet B holds arrived before A sent its last one: nothing A sent \
         last has been forwarded, so the SFU is holding media back"
    );
    (
        head.seq.wrapping_sub(last_sent.seq),
        head.time.wrapping_sub(last_sent.time),
    )
}

/// The packet in `arrived` that is `sent` as B received it: the number
/// `sent` maps to under `seq_offset`, on a datagram that reached B after
/// `sent` left A.
fn counterpart(sent: RtpSeen, seq_offset: u16, arrived: &[RtpSeen]) -> Option<RtpSeen> {
    let want = sent.seq.wrapping_add(seq_offset);
    arrived
        .iter()
        .copied()
        .find(|p| p.seq == want && p.at >= sent.at)
}

/// A lost video packet reaches the subscriber as a gap, and nothing waits
/// behind it (ADR-0066 points 1 and 2).
///
/// One packet of a three-packet frame is lost on the way to the SFU, and the
/// publisher's retransmissions are lost too, so the SFU never gets the
/// packet. Two more frames follow and then nothing. Observed at the
/// subscriber's **socket** — the datagrams as they arrive, before the
/// subscriber's own stack waits for or assembles anything:
///
/// 1. every packet the publisher sent after the lost one arrives, each
///    within a second of leaving the publisher;
/// 2. each arrives with the publisher's sequence number and timestamp plus
///    one fixed offset, and with its marker bit;
/// 3. the lost packet's number is unused: the subscriber sees the loss as a
///    gap, where a relay that re-packetized frames would have hidden it.
///
/// An SFU that assembled frames would forward none of this: the frame with
/// the missing packet never completes, the retransmission never comes, and
/// two frames are fewer than it would wait for behind the gap.
///
/// What this does not show: what a decoder does with the gap, loss between
/// the SFU and the subscriber, or anything about a libwebrtc client — both
/// ends here are str0m.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn a_lost_video_packet_is_a_gap_at_the_subscriber_and_holds_nothing_back() {
    const FOLLOWING: usize = 2;
    let mut pair = Box::pin(VideoPair::streaming("video-gap-call")).await;
    let (main_ssrc, rtx_ssrc) = pair
        .a
        .send_ssrcs(pair.mid)
        .expect("A has a send stream for its video");
    let rtx_ssrc = rtx_ssrc.expect("video is negotiated with a retransmission stream");
    // From here on no retransmission from A reaches the SFU.
    pair.a.drop_all_on.push(rtx_ssrc);

    let (lost, sent_mark) = pair
        .write_a_frame_and_lose_its_first_packet(main_ssrc)
        .await;
    for _ in 0..FOLLOWING {
        pair.write_next_frame();
        let _ = pair.tick().await;
    }

    // Nothing more is written. A relay waiting for more frames behind the
    // gap never delivers what was sent after it.
    let _ = pair.quiesce().await;

    let sent_after = pair.sent_since(sent_mark, main_ssrc);
    let arrived = pair.arrived_at_subscriber();
    // The damaged frame's two remaining packets and three per following
    // frame.
    assert_eq!(
        sent_after.len(),
        2 + 3 * FOLLOWING,
        "A must have sent the rest of the damaged frame and {FOLLOWING} whole frames"
    );
    let last_sent = *sent_after.last().expect("A sent packets after the loss");
    let (seq_offset, time_offset) = stream_offsets(last_sent, &arrived);

    // 1 + 2. Every packet after the lost one arrived, numbered and timed by
    // the one fixed offset, marker kept, and without waiting.
    let mut slowest = Duration::ZERO;
    for sent in &sent_after {
        let got = counterpart(*sent, seq_offset, &arrived).unwrap_or_else(|| {
            panic!(
                "the packet A sent as {} never reached B: the SFU held it behind the gap; \
                 B received {:?}",
                sent.seq,
                arrived.iter().map(|p| p.seq).collect::<Vec<_>>()
            )
        });
        assert_eq!(
            got.time,
            sent.time.wrapping_add(time_offset),
            "the RTP timestamp is the publisher's plus a fixed offset"
        );
        assert_eq!(got.marker, sent.marker, "the marker bit is kept");
        slowest = slowest.max(got.at.saturating_duration_since(sent.at));
    }
    assert!(
        slowest < Duration::from_secs(1),
        "a packet took {slowest:?} from the publisher's socket to the subscriber's"
    );

    // 3. The loss is a gap: the number the lost packet maps to never arrived,
    // while its neighbours on both sides did.
    let gap = lost.seq.wrapping_add(seq_offset);
    assert!(
        !arrived.iter().any(|p| p.seq == gap),
        "the lost packet's number must stay unused at the subscriber"
    );
    assert!(
        arrived.iter().any(|p| p.seq == gap.wrapping_sub(1)),
        "the packet before the lost one was delivered"
    );
    assert!(
        arrived.iter().any(|p| p.seq == gap.wrapping_add(1)),
        "the packet after the lost one was delivered"
    );
}

/// A retransmission fills the subscriber's gap under the number it maps to
/// (ADR-0066 point 3).
///
/// One video packet is lost on the way to the SFU; this time the publisher's
/// retransmission stream is left alone. The SFU asks the publisher for the
/// packet again, the retransmission arrives, and the SFU forwards it. At the
/// subscriber's socket:
///
/// - the packet arrives under exactly the number its loss left unused;
/// - it arrives **after** packets with higher numbers, which therefore did
///   not wait for it;
///
/// and the subscriber's own stack then assembles the damaged frame byte for
/// byte, so what was retransmitted and relayed is the packet that was lost.
///
/// Both ends are str0m. Whether libwebrtc repairs its picture from the same
/// packets is not shown here.
#[tokio::test]
async fn a_retransmitted_video_packet_fills_the_subscribers_gap() {
    let mut pair = Box::pin(VideoPair::streaming("video-repair-call")).await;
    let (main_ssrc, rtx_ssrc) = pair
        .a
        .send_ssrcs(pair.mid)
        .expect("A has a send stream for its video");
    assert!(
        rtx_ssrc.is_some(),
        "video is negotiated with a retransmission stream"
    );

    let damaged_idx = pair.next_idx;
    let (lost, sent_mark) = pair
        .write_a_frame_and_lose_its_first_packet(main_ssrc)
        .await;
    let damaged_frame = loss_test_frame(damaged_idx);

    // Keep the stream going for up to three seconds, until B has assembled
    // the damaged frame — which it can only do once the lost packet arrives.
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut repaired = false;
    while Instant::now() < deadline && !repaired {
        pair.write_next_frame();
        repaired = pair.tick().await.contains(&damaged_frame);
    }
    repaired |= pair.quiesce().await.contains(&damaged_frame);

    let sent_after = pair.sent_since(sent_mark, main_ssrc);
    let arrived = pair.arrived_at_subscriber();
    let last_sent = *sent_after.last().expect("A sent packets after the loss");
    let (seq_offset, time_offset) = stream_offsets(last_sent, &arrived);

    let slot = lost.seq.wrapping_add(seq_offset);
    let position = arrived
        .iter()
        .position(|p| p.seq == slot && p.at >= lost.at)
        .unwrap_or_else(|| {
            panic!(
                "the retransmitted packet never reached B under number {slot}; B received {:?}",
                arrived.iter().map(|p| p.seq).collect::<Vec<_>>()
            )
        });
    assert_eq!(
        arrived[position].time,
        lost.time.wrapping_add(time_offset),
        "the repaired packet carries the timestamp of the frame it belongs to"
    );
    let next_sent = *sent_after
        .first()
        .expect("A sent the rest of the damaged frame");
    assert_eq!(
        next_sent.seq,
        lost.seq.wrapping_add(1),
        "the first packet A sent after the loss is the one right after the lost one"
    );
    let next = counterpart(next_sent, seq_offset, &arrived)
        .expect("the packet after the lost one reached B");
    let next_position = arrived
        .iter()
        .position(|p| *p == next)
        .expect("it is in B's log");
    assert!(
        next_position < position,
        "the packet after the gap arrived first: it was forwarded without waiting for the repair"
    );
    assert!(
        repaired,
        "B must assemble the damaged frame once its lost packet is retransmitted and relayed"
    );
}

/// A packet lost in a burst that also swallows the first retransmissions is
/// still repaired once the burst ends (ADR-0067, the vendored `str0m` NACK
/// schedule).
///
/// One video packet is lost on the way to the SFU and, for the next 600 ms,
/// every retransmission the publisher sends is lost too, the way a second of
/// Wi-Fi loss behaves. Upstream `str0m` asked five times in about 165 ms and
/// then gave the packet up, so it never reached the subscriber. The fork
/// keeps asking for about 1.4 s; a request after the burst gets through, and
/// the subscriber assembles the damaged frame.
#[tokio::test]
async fn a_packet_lost_in_a_burst_is_repaired_after_it() {
    let mut pair = Box::pin(VideoPair::streaming("video-burst-repair-call")).await;
    let (main_ssrc, rtx_ssrc) = pair
        .a
        .send_ssrcs(pair.mid)
        .expect("A has a send stream for its video");
    let rtx_ssrc = rtx_ssrc.expect("video is negotiated with a retransmission stream");

    let damaged_idx = pair.next_idx;
    pair.a.drop_all_on.push(rtx_ssrc);
    let _ = pair
        .write_a_frame_and_lose_its_first_packet(main_ssrc)
        .await;
    let damaged_frame = loss_test_frame(damaged_idx);

    let burst_ends = Instant::now() + Duration::from_millis(600);
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut last_write = Instant::now();
    let mut repaired = false;
    while Instant::now() < deadline && !repaired {
        if Instant::now() >= burst_ends {
            pair.a.drop_all_on.retain(|s| *s != rtx_ssrc);
        }
        if last_write.elapsed() >= Duration::from_millis(33) {
            pair.write_next_frame();
            last_write = Instant::now();
        }
        repaired = pair.tick().await.contains(&damaged_frame);
    }
    assert!(
        repaired,
        "the packet lost in the burst must be repaired by a retransmission \
         requested after the burst"
    );
}

// ============================================================
// Video FEC: RED + ULPFEC (ADR-0067 step 4)
// ============================================================

/// The `m=video` sections of an SDP, as one string per section.
fn video_sections(sdp: &str) -> Vec<String> {
    let mut sections: Vec<String> = Vec::new();
    let mut in_video = false;
    for line in sdp.lines() {
        if line.starts_with("m=") {
            in_video = line.starts_with("m=video");
            if in_video {
                sections.push(String::new());
            }
        }
        if in_video {
            if let Some(section) = sections.last_mut() {
                section.push_str(line);
                section.push('\n');
            }
        }
    }
    sections
}

/// Zero the data of the header extensions `ids` in an RTP header whose
/// extension block is in the RFC 8285 one-byte form, which is what a `str0m`
/// client writes. Written here from the RFC, not shared with the SFU.
fn zero_one_byte_extensions(header: &mut [u8], ids: &[u8]) {
    if header[0] & 0x10 == 0 {
        return;
    }
    let start = 12 + 4 * usize::from(header[0] & 0x0f);
    assert_eq!(
        &header[start..start + 2],
        &[0xbe, 0xde],
        "the test client writes the one-byte extension form"
    );
    let mut at = start + 4;
    while at < header.len() {
        if header[at] == 0 {
            at += 1;
            continue;
        }
        let id = header[at] >> 4;
        let len = usize::from(header[at] & 0x0f) + 1;
        if ids.contains(&id) {
            header[at + 1..at + 1 + len].fill(0);
        }
        at += 1 + len;
    }
}

/// A media packet as a libwebrtc FEC encoder sees it: the RTP headers the
/// client put on the wire (CSRC list and header extension block included),
/// with the media payload type in place of `red`, the extensions the pacer
/// fills in zeroed, and the media payload without the RED header byte,
/// followed by the packet's padding.
///
/// The padding is not readable on the wire (SRTP encrypts it with the
/// payload), so it is rebuilt from what the `str0m` sender does: it pads
/// the RTP payload, RED header byte included, with zeroes to a multiple of
/// 16 bytes and writes the padding length into the last byte
/// (`streams/send.rs`, `RtpHeader::pad_packet`). FEC covers padding, and a
/// receiver removes the RED byte and keeps the padding as sent.
fn fec_view(wire_header: &[u8], media_pt: u8, zero_ids: &[u8], body: &[u8]) -> Vec<u8> {
    let mut packet = wire_header.to_vec();
    packet[1] = (packet[1] & 0x80) | media_pt;
    zero_one_byte_extensions(&mut packet, zero_ids);
    packet.extend_from_slice(body);
    let pad = (16 - (1 + body.len()) % 16) % 16;
    assert_eq!(
        wire_header[0] & 0x20 != 0,
        pad != 0,
        "the padding bit on the wire matches the sender's padding rule"
    );
    if pad != 0 {
        packet.extend(std::iter::repeat(0).take(pad - 1));
        packet.push(u8::try_from(pad).expect("under 16"));
    }
    packet
}

/// Build one RFC 5109 FEC packet over whole media packets, the way
/// libwebrtc's `ForwardErrorCorrection` does: the FEC header (P, X, CC, M
/// and PT recovery in the first two bytes, `SN base`, TS recovery, length
/// recovery), one level with a 16-bit mask, and a payload that is the XOR of
/// everything after each packet's 12-byte fixed header. `SN base` is the
/// first packet's sequence number.
fn ulpfec_encode(packets: &[Vec<u8>]) -> Vec<u8> {
    const HEADER_LEN: usize = 14;
    let sn_base = u16::from_be_bytes([packets[0][2], packets[0][3]]);
    let longest = packets.iter().map(|p| p.len() - 12).max().unwrap_or(0);
    let mut fec = vec![0u8; HEADER_LEN + longest];
    let mut length: u16 = 0;
    let mut mask: u16 = 0;
    for p in packets {
        let distance = u16::from_be_bytes([p[2], p[3]]).wrapping_sub(sn_base);
        assert!(distance < 16, "a 16-bit mask covers 16 sequence numbers");
        mask |= 0x8000 >> distance;
        fec[0] ^= p[0];
        fec[1] ^= p[1];
        for i in 4..8 {
            fec[i] ^= p[i];
        }
        length ^= u16::try_from(p.len() - 12).expect("short test packet");
        for (x, b) in fec[HEADER_LEN..].iter_mut().zip(&p[12..]) {
            *x ^= b;
        }
    }
    fec[0] &= 0x3f; // E = 0, L = 0
    fec[2..4].copy_from_slice(&sn_base.to_be_bytes());
    fec[8..10].copy_from_slice(&length.to_be_bytes());
    fec[10..12].copy_from_slice(&u16::try_from(longest).expect("short").to_be_bytes());
    fec[12..14].copy_from_slice(&mask.to_be_bytes());
    fec
}

/// One packet the publisher of the uplink-loss tests sends.
enum Planned {
    Media {
        seq: u64,
        time: u32,
        marker: bool,
        body: Vec<u8>,
    },
    /// An FEC packet protecting the media packets with these sequence
    /// numbers. Its bytes are computed when it is sent, from what the
    /// client put on the wire for them.
    Fec {
        seq: u64,
        time: u32,
        protects: Vec<u64>,
    },
}

/// Four frames of three media packets, sequence numbers running across the
/// 16-bit wrap. With `fec`, each frame is followed by one FEC packet
/// protecting it, in the same sequence-number space, as libwebrtc sends it.
fn uplink_plan(fec: bool) -> Vec<Planned> {
    let mut plan = Vec::new();
    let mut seq: u64 = 65_530;
    for frame in 0..4u32 {
        let time = 90_000 + frame * 3_000;
        let mut protects = Vec::new();
        for part in 0..3u32 {
            let body = format!(
                "sealed video frame {frame} packet {part} {}",
                "x".repeat((frame * 3 + part) as usize * 5)
            )
            .into_bytes();
            plan.push(Planned::Media {
                seq,
                time,
                marker: part == 2,
                body,
            });
            protects.push(seq);
            seq += 1;
        }
        if fec {
            plan.push(Planned::Fec {
                seq,
                time,
                protects,
            });
            seq += 1;
        }
    }
    plan
}

/// Register `n` subscribers of one call with the signaling registry.
async fn register_subscribers(
    peers: &PeerRegistry,
    call: &str,
    n: usize,
) -> Vec<(PeerId, mpsc::Receiver<ServerFrame>)> {
    let mut out = Vec::new();
    for _ in 0..n {
        let id = fresh_peer_id();
        let (tx, rx) = mpsc::channel::<ServerFrame>(64);
        assert!(
            peers
                .register(Peer {
                    id: id.clone(),
                    room: call.to_owned(),
                    outbound: Some(tx),
                })
                .await,
            "fresh PeerId must register cleanly"
        );
        out.push((id, rx));
    }
    out
}

/// What [`run_uplink_loss`] observed.
struct UplinkLoss {
    /// The media packets the publisher wrote: (sequence number, timestamp,
    /// marker, media payload).
    media: Vec<(u16, u32, bool, Vec<u8>)>,
    /// How many FEC packets the publisher wrote.
    fec_written: usize,
    /// Sequence number of the media packet that never reached the SFU.
    lost_seq: u16,
    /// When it would have left the publisher.
    lost_at: Instant,
    /// When the FEC packet protecting it left the publisher, if one did.
    fec_sent_at: Option<Instant>,
    /// Sequence numbers of the publisher's main-stream datagrams that were
    /// thrown away instead of sent.
    main_dropped: Vec<u16>,
    /// How many retransmission datagrams of the publisher were thrown away.
    rtx_dropped: usize,
    /// When the publisher received a NACK naming `lost_seq`.
    nacks_for_lost: Vec<Instant>,
    /// What subscribers B, C and D received, as their own `str0m` handed it
    /// over, and how many media datagrams reached each one's socket.
    got: [Vec<RtpGot>; 3],
    datagrams: [usize; 3],
    /// The SFU's answer to the publisher and its offers to B, C and D.
    publisher_answer: String,
    offers: [String; 3],
    /// C's answer to the SFU's offer.
    c_answer: String,
    /// The publisher's mid, and the mids B received packets with.
    publisher_mid: Mid,
    b_mids: std::collections::BTreeSet<Mid>,
    /// The FEC packets the publisher wrote: (sequence number, the media
    /// sequence numbers each protects).
    fec_plan: Vec<(u16, Vec<u16>)>,
    /// What B, C and D received as whole RTP packets in the clear (first
    /// transmissions only), and the ids, on each one's media line, of the
    /// absolute-send-time and transport-wide sequence number extensions.
    wire: [Vec<Vec<u8>>; 3],
    zero_ids: [Vec<u8>; 3],
    /// The media packet that was withheld from B on the downlink, with every
    /// retransmission, when the run asked for that; and what was thrown away
    /// under that rule, as (SSRC, sequence number).
    withheld_seq: Option<u16>,
    withheld_from_b: Vec<(u32, u16)>,
}

/// The ids of the two header extensions a sender fills in after FEC was
/// computed, on the media line this client receives video on.
fn leg_zero_ids(client: &SyntheticClient) -> Vec<u8> {
    let Some(mid) = client.rtp_got.iter().find_map(|g| g.mid) else {
        return Vec::new();
    };
    let extmap = client.rtc.media(mid).expect("media").remote_extmap();
    [
        Extension::AbsoluteSendTime,
        Extension::TransportSequenceNumber,
    ]
    .into_iter()
    .filter_map(|e| extmap.id_of(e))
    .collect()
}

const UPLINK_VP8_PT: u8 = 96;
const UPLINK_RED_PT: u8 = 116;
const UPLINK_ULPFEC_PT: u8 = 118;

/// How long after an event a NACK that was already on its way may still
/// arrive.
const NACK_IN_FLIGHT: Duration = Duration::from_millis(150);

/// A publisher loses one media packet on its way to the SFU, and every
/// retransmission of it too.
///
/// Four clients in RTP mode. A publishes video and offers `red` and
/// `ulpfec`; B subscribes with the pair under the same payload type numbers;
/// C subscribes without the pair; D subscribes with the pair under different
/// numbers (VP8 100, red 104, ulpfec 106).
///
/// With `video_fec` the SFU runs with `LAKKO_SFU_VIDEO_FEC` on: A sends, as
/// libwebrtc does once the pair is negotiated, every packet under the `red`
/// payload type, media as `[media pt] ++ body` and FEC as
/// `[ulpfec pt] ++ FEC packet`, on one SSRC and one sequence-number space.
/// Each FEC packet is computed over the three media packets of its frame as
/// A's own `str0m` put them on the wire: the RTP headers are read off the
/// datagrams (SRTP leaves them in the clear), header extension block
/// included, with the media payload type and with the absolute send time and
/// transport-wide sequence number zeroed.
///
/// Without, the SFU negotiates no `red`; A sends the same media under its
/// VP8 payload type and no FEC.
///
/// With `withhold_downlink`, B additionally never receives the middle packet
/// of the third frame, nor any retransmission: a packet lost on the SFU's
/// leg to B for good.
///
/// After the last packet the clients keep running for two seconds, longer
/// than the SFU keeps asking for a missing packet (about 1.4 s).
#[allow(clippy::too_many_lines)]
async fn run_uplink_loss(video_fec: bool, withhold_downlink: bool) -> UplinkLoss {
    let plan = uplink_plan(video_fec);
    let media: Vec<(u16, u32, bool, Vec<u8>)> = plan
        .iter()
        .filter_map(|p| match p {
            Planned::Media {
                seq,
                time,
                marker,
                body,
            } => Some(((seq & 0xffff) as u16, *time, *marker, body.clone())),
            Planned::Fec { .. } => None,
        })
        .collect();
    // The middle packet of the second frame.
    let lost_seq = media[4].0;
    // The middle packet of the third frame.
    let withheld_seq = withhold_downlink.then(|| media[7].0);
    let fec_plan: Vec<(u16, Vec<u16>)> = plan
        .iter()
        .filter_map(|p| match p {
            Planned::Fec { seq, protects, .. } => Some((
                (seq & 0xffff) as u16,
                protects.iter().map(|s| (s & 0xffff) as u16).collect(),
            )),
            Planned::Media { .. } => None,
        })
        .collect();

    let peers = PeerRegistry::new();
    let mut registered = register_subscribers(&peers, "uplink-loss", 3).await;
    let (d_id, mut d_rx) = registered.pop().expect("three registered");
    let (c_id, mut c_rx) = registered.pop().expect("three registered");
    let (b_id, mut b_rx) = registered.pop().expect("three registered");
    let (media_task, sfu_addr) = spawn_media_task_with_options(
        "127.0.0.1:0".parse().unwrap(),
        None,
        peers,
        MediaTaskOptions {
            diagnostics: None,
            video_fec,
            uplink_fallback: false,
            protection: LossProtection::default(),
            downlink_control: false,
        },
    )
    .await
    .expect("UDP bind on loopback:0 succeeds");

    let mut a = SyntheticClient::new_rtp_mode_video_publisher(true).await;
    let mut b = SyntheticClient::new_rtp_mode_video(true).await;
    let mut c = SyntheticClient::new_rtp_mode_video(false).await;
    let mut d = SyntheticClient::new_rtp_mode_video_fec_renumbered().await;
    b.withhold_in = withheld_seq;

    for (client, id) in [(&mut b, &b_id), (&mut c, &c_id), (&mut d, &d_id)] {
        let (offer, pending) = client.create_video_subscriber_offer();
        let answer = media_task
            .accept_offer(id.clone(), "uplink-loss-call".to_owned(), offer)
            .await
            .expect("SFU accepts a subscriber offer");
        client
            .accept_answer(pending, &answer)
            .expect("subscriber applies the SFU's answer");
    }
    let (a_offer, a_pending, mid) = a.create_video_publisher_offer();
    let publisher_answer = media_task
        .accept_offer(fresh_peer_id(), "uplink-loss-call".to_owned(), a_offer)
        .await
        .expect("SFU accepts A's publisher offer");
    a.accept_answer(a_pending, &publisher_answer)
        .expect("A applies the SFU's answer");

    let deadline = Instant::now() + CROSS_FANOUT_DEADLINE;
    let mut reneg = [false; 3];
    let mut offers: [Option<String>; 3] = [None, None, None];
    let mut answers: [Option<String>; 3] = [None, None, None];
    let mut written = 0usize;
    let mut fec_written = 0usize;
    let mut settle: Option<Instant> = None;
    let mut ssrcs: Option<(u32, u32)> = None;
    let mut fec_seq_for_lost: Option<u16> = None;
    let mut buf = [0u8; 2048];
    let expected = if video_fec {
        media.len()
    } else {
        media.len() - 1
    };

    loop {
        let now = Instant::now();
        assert!(
            now < deadline,
            "the uplink-loss run did not finish in time: b={} c={} d={} written={written}",
            b.rtp_got.len(),
            c.rtp_got.len(),
            d.rtp_got.len()
        );
        if settle.is_some_and(|at| now >= at) {
            break;
        }
        for client in [&mut a, &mut b, &mut c, &mut d] {
            let _ = client.drain().await;
        }
        for (i, (client, id, rx)) in [
            (&mut b, &b_id, &mut b_rx),
            (&mut c, &c_id, &mut c_rx),
            (&mut d, &d_id, &mut d_rx),
        ]
        .into_iter()
        .enumerate()
        {
            if reneg[i] || !client.is_connected() {
                continue;
            }
            while let Ok(frame) = rx.try_recv() {
                if let ServerFrame::SdpOffer { sdp } = frame {
                    let answer = client.answer_renegotiation(&sdp).expect("answers");
                    offers[i] = Some(sdp);
                    answers[i] = Some(answer.clone());
                    reneg[i] = media_task
                        .accept_renegotiation_answer(id.clone(), answer)
                        .await
                        .is_ok();
                }
            }
        }
        let all_up = a.is_connected()
            && b.is_connected()
            && c.is_connected()
            && d.is_connected()
            && reneg.iter().all(|r| *r);
        if all_up && ssrcs.is_none() {
            let (main, rtx) = a.send_ssrcs(mid).expect("A has a send stream");
            let rtx = rtx.expect("video is negotiated with a retransmission stream");
            // One media packet never reaches the SFU, and neither does any
            // retransmission.
            a.drop_seq_on = Some((main, lost_seq));
            a.drop_all_on.push(rtx);
            ssrcs = Some((main, rtx));
        }
        if let (true, Some((main, _))) = (all_up && written < plan.len(), ssrcs) {
            match &plan[written] {
                Planned::Media {
                    seq,
                    time,
                    marker,
                    body,
                } => {
                    let (pt, payload) = if video_fec {
                        let mut payload = vec![UPLINK_VP8_PT];
                        payload.extend_from_slice(body);
                        (UPLINK_RED_PT, payload)
                    } else {
                        (UPLINK_VP8_PT, body.clone())
                    };
                    if a.write_video_rtp(mid, pt, *seq, *time, *marker, &payload) {
                        written += 1;
                    }
                },
                Planned::Fec {
                    seq,
                    time,
                    protects,
                } => {
                    // The extensions a libwebrtc pacer writes after FEC was
                    // computed, under the ids negotiated on this media line.
                    let extmap = a.rtc.media(mid).expect("media").remote_extmap();
                    let zero_ids: Vec<u8> = [
                        Extension::AbsoluteSendTime,
                        Extension::TransportSequenceNumber,
                    ]
                    .into_iter()
                    .filter_map(|e| extmap.id_of(e))
                    .collect();
                    // Wait until A has put the whole frame on the wire: the
                    // FEC covers the headers as they were sent.
                    let views: Option<Vec<Vec<u8>>> = protects
                        .iter()
                        .map(|s| {
                            let seq16 = (s & 0xffff) as u16;
                            let header = a
                                .wire_headers
                                .iter()
                                .find(|(ssrc, seq, _)| *ssrc == main && *seq == seq16)
                                .map(|(_, _, header)| header)?;
                            let body = media
                                .iter()
                                .find(|m| m.0 == seq16)
                                .map(|m| &m.3)
                                .expect("planned");
                            Some(fec_view(header, UPLINK_VP8_PT, &zero_ids, body))
                        })
                        .collect();
                    if let Some(views) = views {
                        let mut payload = vec![UPLINK_ULPFEC_PT];
                        payload.extend_from_slice(&ulpfec_encode(&views));
                        if a.write_video_rtp(mid, UPLINK_RED_PT, *seq, *time, false, &payload) {
                            written += 1;
                            fec_written += 1;
                            if protects.iter().any(|s| (s & 0xffff) as u16 == lost_seq) {
                                fec_seq_for_lost = Some((seq & 0xffff) as u16);
                            }
                        }
                    }
                },
            }
        }
        // Media packets received, FEC packets (the `red` payload type with
        // the `ulpfec` payload type in the RED header byte) left out.
        let distinct = |got: &[RtpGot], red: u8, ulpfec: u8| {
            got.iter()
                .filter(|g| !(g.pt == red && g.payload.first() == Some(&ulpfec)))
                .map(|g| g.seq)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
        };
        if settle.is_none()
            && written == plan.len()
            && distinct(&b.rtp_got, UPLINK_RED_PT, UPLINK_ULPFEC_PT)
                >= expected - usize::from(withhold_downlink)
            && distinct(&c.rtp_got, UPLINK_RED_PT, UPLINK_ULPFEC_PT) >= expected
            && distinct(&d.rtp_got, 104, 106) >= expected
        {
            settle = Some(Instant::now() + Duration::from_secs(2));
        }
        for client in [&mut a, &mut b, &mut c, &mut d] {
            while let Ok((n, src)) = client.socket.try_recv_from(&mut buf) {
                client.feed_receive(&buf[..n], src, sfu_addr);
                // str0m in RTP mode holds one received packet until the next
                // poll; two datagrams fed back to back would lose the first.
                let _ = client.drain().await;
            }
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
        for client in [&mut a, &mut b, &mut c, &mut d] {
            client.feed_timeout();
        }
    }

    let (main, rtx) = ssrcs.expect("the stream was sent");
    let lost_at = a
        .rtp_dropped
        .iter()
        .find(|p| p.ssrc == main && p.seq == lost_seq)
        .map(|p| p.at)
        .expect("the packet was withheld");
    let fec_sent_at = fec_seq_for_lost.map(|seq| {
        a.rtp_out
            .iter()
            .find(|p| p.ssrc == main && p.seq == seq)
            .map(|p| p.at)
            .expect("the FEC packet left A")
    });
    UplinkLoss {
        media,
        fec_written,
        lost_seq,
        lost_at,
        fec_sent_at,
        main_dropped: a
            .rtp_dropped
            .iter()
            .filter(|p| p.ssrc == main)
            .map(|p| p.seq)
            .collect(),
        rtx_dropped: a.rtp_dropped.iter().filter(|p| p.ssrc == rtx).count(),
        nacks_for_lost: a
            .nacks_seen
            .iter()
            .filter(|(_, ssrc, seq)| *ssrc == main && *seq == lost_seq)
            .map(|(at, _, _)| *at)
            .collect(),
        datagrams: [b.rtp_in.len(), c.rtp_in.len(), d.rtp_in.len()],
        b_mids: b.rtp_got.iter().filter_map(|g| g.mid).collect(),
        zero_ids: [leg_zero_ids(&b), leg_zero_ids(&c), leg_zero_ids(&d)],
        fec_plan,
        withheld_seq,
        withheld_from_b: b.withheld_in,
        wire: [b.wire_got, c.wire_got, d.wire_got],
        got: [b.rtp_got, c.rtp_got, d.rtp_got],
        publisher_answer,
        offers: offers.map(|o| o.expect("was offered the track")),
        c_answer: answers[1].clone().expect("C answered"),
        publisher_mid: mid,
    }
}

/// The SFU repairs a publisher's lost video packet from the publisher's own
/// FEC (ADR-0067 step 4, part 1), with `LAKKO_SFU_VIDEO_FEC` on.
///
/// The run is [`run_uplink_loss`]: one media packet is withheld from the SFU
/// and so is every retransmission of it. Asserted:
///
/// - the SFU's answer to A and its offers to B and D carry the pair, each in
///   the client's own numbers; C answers without it;
/// - **every** media packet reaches all three subscribers, the withheld one
///   included, under the publisher's sequence number, with its timestamp and
///   marker, and the payload after the RED header byte verbatim: RED-wrapped
///   for B and D (outer payload type and header byte in each one's own
///   numbers), unwrapped for C;
/// - C, which did not negotiate the pair, is sent no FEC packet; B and D are
///   sent the SFU's own (see the downlink tests below);
/// - the SFU sends A no NACK naming the withheld packet later than 150 ms
///   after the FEC packet protecting it left A (150 ms being the allowance
///   for a NACK already in flight), over the two seconds that follow.
///
/// What this does not show: a libwebrtc sender. The FEC here is computed by
/// this file's encoder over a `str0m` client's packets; that it matches what
/// Chrome computes is taken from reading libwebrtc M125, not from a run.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn a_video_packet_lost_on_the_uplink_is_rebuilt_from_the_publishers_fec() {
    let run = Box::pin(run_uplink_loss(true, false)).await;

    // ── SDP ─────────────────────────────────────────────────────────────
    let answered = video_sections(&run.publisher_answer);
    assert_eq!(answered.len(), 1);
    println!("SFU answer to the FEC publisher, m=video:\n{}", answered[0]);
    for line in [
        "a=rtpmap:116 red/90000",
        "a=rtpmap:117 rtx/90000",
        "a=fmtp:117 apt=116",
        "a=rtpmap:118 ulpfec/90000",
    ] {
        assert!(
            answered[0].lines().any(|l| l == line),
            "the SFU's answer to a publisher offering the pair has {line}:\n{}",
            answered[0]
        );
    }
    let offered = |i: usize| -> String {
        video_sections(&run.offers[i])
            .pop()
            .expect("a video section")
    };
    for line in [
        "a=rtpmap:116 red/90000",
        "a=fmtp:117 apt=116",
        "a=rtpmap:118 ulpfec/90000",
    ] {
        assert!(offered(0).lines().any(|l| l == line), "B offer: {line}");
        // C is offered the pair too (its session with the SFU knows no
        // client numbers for it) and answers without.
        assert!(offered(1).lines().any(|l| l == line), "C offer: {line}");
    }
    for line in [
        "a=rtpmap:104 red/90000",
        "a=fmtp:105 apt=104",
        "a=rtpmap:106 ulpfec/90000",
        "a=rtpmap:100 VP8/90000",
    ] {
        assert!(offered(2).lines().any(|l| l == line), "D offer: {line}");
    }
    assert!(
        !run.c_answer.contains(" red/") && !run.c_answer.contains("ulpfec"),
        "a client without the pair answers without it:\n{}",
        run.c_answer
    );

    // ── The loss was real ───────────────────────────────────────────────
    assert_eq!(
        run.main_dropped,
        vec![run.lost_seq],
        "exactly the one media packet was withheld from the SFU"
    );
    assert_eq!(run.fec_written, 4, "one FEC packet per frame");
    let fec_sent_at = run.fec_sent_at.expect("an FEC packet protects the loss");

    // ── Every media packet reached every subscriber ─────────────────────
    let by_seq = |got: &[RtpGot]| -> std::collections::BTreeMap<u16, RtpGot> {
        got.iter().map(|g| (g.seq, g.clone())).collect()
    };
    // (name, outer payload type, RED header byte or None when unwrapped)
    let shapes = [
        ("B", UPLINK_RED_PT, Some(UPLINK_VP8_PT)),
        ("C", UPLINK_VP8_PT, None),
        ("D", 104, Some(100)),
    ];
    for (i, (name, pt, red_header)) in shapes.into_iter().enumerate() {
        let got = by_seq(&run.got[i]);
        // A subscriber with the pair also gets the SFU's own FEC packets, in
        // the slots of the publisher's (checked by the downlink tests below);
        // one without gets the media packets and nothing else.
        let slots = if red_header.is_some() {
            run.fec_plan.len()
        } else {
            0
        };
        assert_eq!(
            got.len(),
            run.media.len() + slots,
            "{name} gets every media packet, and an FEC packet per slot only with the pair: {:?}",
            got.keys().collect::<Vec<_>>()
        );
        for (seq, time, marker, body) in &run.media {
            let what = if *seq == run.lost_seq {
                "the withheld packet"
            } else {
                "a media packet"
            };
            let g = got
                .get(seq)
                .unwrap_or_else(|| panic!("{name} did not get {what} {seq}"));
            assert_eq!(g.pt, pt, "{name}: outer payload type of {what} {seq}");
            match red_header {
                Some(header) => {
                    assert_eq!(g.payload[0], header, "{name}: RED header byte of {seq}");
                    assert_eq!(&g.payload[1..], &body[..], "{name}: {what} {seq} verbatim");
                },
                None => assert_eq!(&g.payload, body, "{name}: {what} {seq} verbatim"),
            }
            assert_eq!((g.time, g.marker), (*time, *marker), "{name}: {seq}");
        }
        println!(
            "{name}: {} packets received, {} media datagrams on its socket",
            run.got[i].len(),
            run.datagrams[i]
        );
    }

    // ── The publisher is no longer asked for the packet ─────────────────
    let before = run
        .nacks_for_lost
        .iter()
        .filter(|at| **at <= fec_sent_at + NACK_IN_FLIGHT)
        .count();
    let after: Vec<Duration> = run
        .nacks_for_lost
        .iter()
        .filter(|at| **at > fec_sent_at + NACK_IN_FLIGHT)
        .map(|at| at.duration_since(fec_sent_at))
        .collect();
    println!(
        "NACKs naming the withheld packet: {before} up to 150 ms after its FEC packet left A, \
         {} later; retransmissions A sent and the test dropped: {}; FEC left A {:?} after the \
         packet was withheld",
        after.len(),
        run.rtx_dropped,
        fec_sent_at.duration_since(run.lost_at)
    );
    assert!(
        after.is_empty(),
        "the SFU kept asking for a packet it had rebuilt, at {after:?} after the FEC packet"
    );

    // ── Why the FEC itself is not forwarded ─────────────────────────────
    // A's packets left with A's mid in the header extension block; B's
    // arrived with the mid of B's own media line. FEC covers that block.
    println!(
        "A sent on mid {}; B received with mid(s) {:?}",
        run.publisher_mid, run.b_mids
    );
    assert!(
        !run.b_mids.is_empty() && !run.b_mids.contains(&run.publisher_mid),
        "the header extension block is rewritten per leg"
    );
}

/// The same loss with `LAKKO_SFU_VIDEO_FEC` off behaves as before the
/// feature existed: no `red` or `ulpfec` is negotiated, the withheld packet
/// is a gap at every subscriber, and the SFU goes on asking the publisher
/// for it.
#[tokio::test]
async fn the_same_uplink_loss_without_the_switch_stays_a_gap() {
    let run = Box::pin(run_uplink_loss(false, false)).await;

    for (what, sdp) in [
        ("answer to the publisher", &run.publisher_answer),
        ("offer to B", &run.offers[0]),
        ("offer to C", &run.offers[1]),
        ("offer to D", &run.offers[2]),
    ] {
        for section in video_sections(sdp) {
            assert!(
                !section.contains("red/") && !section.contains("ulpfec"),
                "{what} must carry no red or ulpfec for video:\n{section}"
            );
        }
    }
    assert_eq!(run.main_dropped, vec![run.lost_seq]);
    assert_eq!(run.fec_written, 0);

    for (i, (name, pt)) in [("B", UPLINK_VP8_PT), ("C", UPLINK_VP8_PT), ("D", 100)]
        .into_iter()
        .enumerate()
    {
        let got: std::collections::BTreeMap<u16, &RtpGot> =
            run.got[i].iter().map(|g| (g.seq, g)).collect();
        assert_eq!(got.len(), run.media.len() - 1, "{name}: all but one");
        assert!(
            !got.contains_key(&run.lost_seq),
            "{name}: the withheld packet's number stays unused"
        );
        for (seq, time, marker, body) in &run.media {
            if *seq == run.lost_seq {
                continue;
            }
            let g = got[seq];
            assert_eq!(g.pt, pt, "{name}: payload type of {seq}");
            assert_eq!(&g.payload, body, "{name}: {seq} verbatim");
            assert_eq!((g.time, g.marker), (*time, *marker));
        }
    }

    let late = run
        .nacks_for_lost
        .iter()
        .filter(|at| **at > run.lost_at + NACK_IN_FLIGHT)
        .count();
    println!(
        "NACKs naming the withheld packet: {} in all, {late} later than 150 ms after the loss; \
         retransmissions A sent and the test dropped: {}",
        run.nacks_for_lost.len(),
        run.rtx_dropped
    );
    assert!(
        late >= 2,
        "without FEC the SFU keeps asking the publisher for the packet; saw {late} NACKs \
         later than 150 ms after the loss"
    );
}

/// With `LAKKO_SFU_VIDEO_FEC` off (the default), video FEC is never
/// negotiated: a publisher and a subscriber that both offer `red` and
/// `ulpfec` get an answer and an offer without either.
#[tokio::test]
async fn video_fec_is_not_negotiated_when_the_switch_is_off() {
    let peers = PeerRegistry::new();
    let mut registered = register_subscribers(&peers, "fec-off-test", 1).await;
    let (b_id, mut b_rx) = registered.pop().expect("one registered");
    let (media, sfu_addr) = spawn_media_task("127.0.0.1:0".parse().unwrap(), peers)
        .await
        .expect("UDP bind on loopback:0 succeeds");

    let mut a = SyntheticClient::new_rtp_mode_video(true).await;
    let mut b = SyntheticClient::new_rtp_mode_video(true).await;
    let a_id = fresh_peer_id();

    let (b_offer, b_pending) = b.create_video_subscriber_offer();
    assert!(
        b_offer.contains("red/90000") && b_offer.contains("ulpfec/90000"),
        "the subscriber does offer the pair"
    );
    let b_answer = media
        .accept_offer(b_id.clone(), "fec-off-call".to_owned(), b_offer)
        .await
        .expect("SFU accepts the subscriber offer");
    b.accept_answer(b_pending, &b_answer)
        .expect("B applies the SFU's answer");
    let (a_offer, a_pending, _mid) = a.create_video_publisher_offer();
    assert!(a_offer.contains("red/90000") && a_offer.contains("ulpfec/90000"));
    let a_answer = media
        .accept_offer(a_id, "fec-off-call".to_owned(), a_offer)
        .await
        .expect("SFU accepts A's publisher offer");
    a.accept_answer(a_pending, &a_answer)
        .expect("A applies the SFU's answer");

    let deadline = Instant::now() + CROSS_FANOUT_DEADLINE;
    let mut buf = [0u8; 2048];
    let sfu_offer = loop {
        assert!(Instant::now() < deadline, "B was never offered A's track");
        for client in [&mut a, &mut b] {
            let _ = client.drain().await;
        }
        if b.is_connected() {
            if let Ok(ServerFrame::SdpOffer { sdp }) = b_rx.try_recv() {
                break sdp;
            }
        }
        for client in [&mut a, &mut b] {
            while let Ok((n, src)) = client.socket.try_recv_from(&mut buf) {
                client.feed_receive(&buf[..n], src, sfu_addr);
            }
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
        for client in [&mut a, &mut b] {
            client.feed_timeout();
        }
    };

    for (what, sdp) in [
        ("answer to the subscriber", &b_answer),
        ("answer to the publisher", &a_answer),
        ("offer to the subscriber", &sfu_offer),
    ] {
        let sections = video_sections(sdp);
        assert!(!sections.is_empty(), "{what} has a video section");
        for section in sections {
            assert!(
                !section.contains("red/") && !section.contains("ulpfec"),
                "{what} must carry no red or ulpfec for video:\n{section}"
            );
            assert!(section.contains("VP8/90000"), "{what} still has VP8");
        }
    }
}

// ============================================================
// Video FEC on the subscriber's leg (ADR-0067 step 4, part 2)
// ============================================================

/// What one subscriber received on its video stream, split the way a
/// receiver splits it before FEC decoding. Written from RFC 2198, RFC 5109
/// and the libwebrtc receiver's behaviour as recorded in
/// `services/sfu/src/fec_receive.rs`; shares no code with the SFU.
struct LegPackets {
    /// The stream's SSRC.
    ssrc: u32,
    /// Media packets by sequence number, each in the form an FEC decoder is
    /// fed: the RTP headers as received with the media payload type in place
    /// of `red` and the two pacer-written extensions zeroed, then everything
    /// after the RED header byte, padding included.
    media: std::collections::BTreeMap<u16, Vec<u8>>,
    /// FEC packets by sequence number: what follows the RED header byte,
    /// without the RTP padding.
    fec: std::collections::BTreeMap<u16, Vec<u8>>,
}

/// Split `wire` (whole received RTP packets, first transmissions only) of a
/// subscriber that negotiated `red`, `ulpfec` and `media_pt`.
fn leg_packets(
    name: &str,
    wire: &[Vec<u8>],
    (red, ulpfec, media_pt): (u8, u8, u8),
    zero_ids: &[u8],
) -> LegPackets {
    let mut leg = LegPackets {
        ssrc: 0,
        media: std::collections::BTreeMap::new(),
        fec: std::collections::BTreeMap::new(),
    };
    for w in wire {
        let headers = rtp_headers_len(w).expect("an RTP packet");
        assert_eq!(w[1] & 0x7f, red, "{name}: every packet is under `red`");
        let seq = u16::from_be_bytes([w[2], w[3]]);
        leg.ssrc = u32::from_be_bytes([w[8], w[9], w[10], w[11]]);
        let inner = w[headers];
        if inner == ulpfec {
            let end = if w[0] & 0x20 == 0 {
                w.len()
            } else {
                w.len() - usize::from(*w.last().expect("not empty"))
            };
            leg.fec.insert(seq, w[headers + 1..end].to_vec());
        } else {
            assert_eq!(inner, media_pt, "{name}: RED header byte of {seq}");
            let mut view = w[..headers].to_vec();
            view[1] = (view[1] & 0x80) | media_pt;
            zero_one_byte_extensions(&mut view, zero_ids);
            view.extend_from_slice(&w[headers + 1..]);
            leg.media.insert(seq, view);
        }
    }
    leg
}

/// The sequence numbers an FEC packet's mask names (RFC 5109 section 7.4).
fn ulpfec_protected(fec: &[u8]) -> Vec<u16> {
    let mask_len = if fec[0] & 0x40 == 0 { 2 } else { 6 };
    let sn_base = u16::from_be_bytes([fec[2], fec[3]]);
    let mut out = Vec::new();
    for (i, byte) in fec[12..12 + mask_len].iter().enumerate() {
        for bit in 0..8u16 {
            if byte & (0x80 >> bit) != 0 {
                out.push(sn_base.wrapping_add(u16::try_from(i * 8).expect("small") + bit));
            }
        }
    }
    out
}

/// Rebuild the one protected packet that is missing (RFC 5109 section 9.2):
/// `others` are the protected packets that did arrive, `seq` and `ssrc` are
/// what the rebuilt packet is given.
fn ulpfec_recover(fec: &[u8], others: &[&Vec<u8>], seq: u16, ssrc: u32) -> Vec<u8> {
    let header_len = if fec[0] & 0x40 == 0 { 14 } else { 18 };
    let protection = usize::from(u16::from_be_bytes([fec[10], fec[11]]));
    assert!(fec[0] & 0x80 == 0, "E is zero");
    assert_eq!(fec.len(), header_len + protection, "one protection level");
    let mut head = [fec[0], fec[1]];
    let mut time = [fec[4], fec[5], fec[6], fec[7]];
    let mut length = u16::from_be_bytes([fec[8], fec[9]]);
    let mut body = fec[header_len..].to_vec();
    for p in others {
        head[0] ^= p[0];
        head[1] ^= p[1];
        for i in 0..4 {
            time[i] ^= p[4 + i];
        }
        length ^= u16::try_from(p.len() - 12).expect("short");
        assert!(
            p.len() - 12 <= body.len(),
            "the protection length covers it"
        );
        for (x, b) in body.iter_mut().zip(&p[12..]) {
            *x ^= b;
        }
    }
    let mut out = vec![(head[0] | 0x80) & 0xbf, head[1]];
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&time);
    out.extend_from_slice(&ssrc.to_be_bytes());
    out.extend_from_slice(&body[..usize::from(length)]);
    out
}

/// The RTP payload of a whole packet: after the headers, without padding.
fn payload_of(packet: &[u8]) -> &[u8] {
    let headers = rtp_headers_len(packet).expect("an RTP packet");
    let end = if packet[0] & 0x20 == 0 {
        packet.len()
    } else {
        packet.len() - usize::from(*packet.last().expect("not empty"))
    };
    &packet[headers..end]
}

/// The SFU sends each subscriber that negotiated `red` and `ulpfec` an FEC
/// packet of its own in the slot of each of the publisher's (ADR-0067 step 4,
/// part 2), with `LAKKO_SFU_VIDEO_FEC` on.
///
/// The run is [`run_uplink_loss`] without a downlink loss: four frames of
/// three packets, each followed by a publisher FEC packet; one packet of the
/// second frame never reaches the SFU and is rebuilt there. The decoder is
/// this file's own, fed the packets as each subscriber's `str0m` received
/// them, whole and in the clear. Asserted, for B (same payload type numbers
/// as the publisher) and D (VP8 100, red 104, ulpfec 106 against the
/// publisher's 96, 116, 118):
///
/// - at each publisher FEC packet's sequence number the subscriber received
///   a packet under its own `red` payload type whose RED header byte is its
///   own `ulpfec` payload type;
/// - its mask names exactly the frame's three media packets;
/// - each of the three, taken out one at a time, is rebuilt from the FEC
///   packet and the other two byte for byte as the subscriber received it
///   (the two pacer-written extensions zeroed on both sides), so with the
///   subscriber's media payload type, this leg's header extension block and
///   the padding as sent, and with the publisher's payload;
/// - the same holds for the frame whose middle packet was lost on the uplink
///   and rebuilt by the SFU.
///
/// And for C, which did not negotiate the pair: no FEC packet, and media
/// under its media payload type without a RED header byte.
///
/// What this does not show: a libwebrtc receiver. That its decoder agrees
/// with this file's is taken from reading, not from a run.
#[tokio::test]
async fn each_pair_subscriber_gets_fec_computed_over_its_own_leg() {
    let run = Box::pin(run_uplink_loss(true, false)).await;
    assert_eq!(run.fec_plan.len(), 4, "one publisher FEC packet per frame");
    assert!(
        run.fec_plan.iter().any(|(_, p)| p.contains(&run.lost_seq)),
        "one of the frames is the one repaired on the uplink"
    );

    let numbers = [
        (
            "B",
            0usize,
            (UPLINK_RED_PT, UPLINK_ULPFEC_PT, UPLINK_VP8_PT),
        ),
        ("D", 2, (104, 106, 100)),
    ];
    for (name, i, pts) in numbers {
        assert_eq!(run.zero_ids[i].len(), 2, "{name}: both extensions are on");
        let leg = leg_packets(name, &run.wire[i], pts, &run.zero_ids[i]);
        assert_eq!(leg.media.len(), run.media.len(), "{name}: all media");
        assert_eq!(
            leg.fec.keys().copied().collect::<Vec<_>>().len(),
            run.fec_plan.len(),
            "{name}: one FEC packet per publisher FEC packet"
        );
        let mut rebuilt = 0;
        for (fec_seq, protects) in &run.fec_plan {
            let fec = leg
                .fec
                .get(fec_seq)
                .unwrap_or_else(|| panic!("{name}: no FEC packet at {fec_seq}"));
            assert_eq!(
                &ulpfec_protected(fec),
                protects,
                "{name}: the FEC packet at {fec_seq} names its frame's packets"
            );
            for lost in protects {
                let others: Vec<&Vec<u8>> = protects
                    .iter()
                    .filter(|s| *s != lost)
                    .map(|s| &leg.media[s])
                    .collect();
                let packet = ulpfec_recover(fec, &others, *lost, leg.ssrc);
                assert_eq!(
                    packet, leg.media[lost],
                    "{name}: {lost} rebuilt from the FEC packet at {fec_seq} is the packet received"
                );
                assert_eq!(packet[1] & 0x7f, pts.2, "{name}: media payload type");
                let (_, time, marker, body) = run
                    .media
                    .iter()
                    .find(|m| m.0 == *lost)
                    .expect("a planned packet");
                assert_eq!(payload_of(&packet), &body[..], "{name}: payload of {lost}");
                assert_eq!(
                    (
                        u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]),
                        packet[1] & 0x80 != 0
                    ),
                    (*time, *marker),
                    "{name}: timestamp and marker of {lost}"
                );
                rebuilt += 1;
            }
        }
        println!(
            "{name}: {} FEC packets, {rebuilt} single-packet recoveries byte for byte, \
             header extension block {} bytes",
            leg.fec.len(),
            rtp_headers_len(&run.wire[i][0]).expect("rtp") - 12
        );
    }

    // C did not negotiate the pair.
    assert_eq!(run.wire[1].len(), run.media.len(), "C: media only");
    for w in &run.wire[1] {
        assert_eq!(
            w[1] & 0x7f,
            UPLINK_VP8_PT,
            "C: under its media payload type"
        );
        let seq = u16::from_be_bytes([w[2], w[3]]);
        let body = &run.media.iter().find(|m| m.0 == seq).expect("planned").3;
        assert_eq!(payload_of(w), &body[..], "C: {seq} has no RED header byte");
    }
}

/// A media packet lost between the SFU and a subscriber, and never
/// retransmitted, is rebuilt at the subscriber from the SFU's FEC packet.
///
/// The run is [`run_uplink_loss`] with the middle packet of the third frame
/// withheld from B together with every packet on B's retransmission stream.
/// The test's decoder takes the FEC packet B received in that frame's slot
/// and the frame's other two packets as B received them; the packet it
/// rebuilds carries the withheld packet's sequence number, timestamp and
/// marker, B's media payload type and the publisher's payload.
#[tokio::test]
async fn a_packet_lost_on_the_downlink_is_rebuilt_from_the_sfus_fec() {
    let run = Box::pin(run_uplink_loss(true, true)).await;
    let withheld = run.withheld_seq.expect("the run withheld a packet");

    assert!(
        run.withheld_from_b.iter().any(|(_, seq)| *seq == withheld),
        "the packet was sent to B and thrown away: {:?}",
        run.withheld_from_b
    );
    assert!(
        run.got[0].iter().all(|g| g.seq != withheld),
        "B never received the withheld packet, by retransmission either"
    );

    let pts = (UPLINK_RED_PT, UPLINK_ULPFEC_PT, UPLINK_VP8_PT);
    let leg = leg_packets("B", &run.wire[0], pts, &run.zero_ids[0]);
    assert!(!leg.media.contains_key(&withheld));
    let (fec_seq, protects) = run
        .fec_plan
        .iter()
        .find(|(_, p)| p.contains(&withheld))
        .expect("a publisher FEC packet protects the frame");
    let fec = leg
        .fec
        .get(fec_seq)
        .unwrap_or_else(|| panic!("B received no FEC packet at {fec_seq}"));
    assert_eq!(&ulpfec_protected(fec), protects);
    let others: Vec<&Vec<u8>> = protects
        .iter()
        .filter(|s| **s != withheld)
        .map(|s| &leg.media[s])
        .collect();
    let packet = ulpfec_recover(fec, &others, withheld, leg.ssrc);

    let (_, time, marker, body) = run
        .media
        .iter()
        .find(|m| m.0 == withheld)
        .expect("a planned packet");
    assert_eq!(packet[0] >> 6, 2, "RTP version 2");
    assert_eq!(packet[1] & 0x7f, UPLINK_VP8_PT, "B's media payload type");
    assert_eq!(packet[1] & 0x80 != 0, *marker, "marker");
    assert_eq!(
        u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]),
        *time,
        "timestamp"
    );
    assert_eq!(
        payload_of(&packet),
        &body[..],
        "the rebuilt packet carries the publisher's payload"
    );
    // The rebuilt header extension block has the leg's shape: the same
    // length as the block of a packet B did receive in that frame.
    assert_eq!(
        rtp_headers_len(&packet),
        rtp_headers_len(others[0]),
        "the header extension block is this leg's"
    );
    println!(
        "B: packet {withheld} withheld with {} datagrams in all (retransmissions included), \
         rebuilt from the FEC packet at {fec_seq}: {} bytes",
        run.withheld_from_b.len(),
        packet.len()
    );
}

// ============================================================
// Server-side bitrate control (ADR-0068)
// ============================================================

/// What a publisher and a subscriber saw of one call under a given
/// `LAKKO_SFU_BITRATE_CONTROL` mode.
struct BitrateRun {
    /// The SFU's answer to the subscriber's offer.
    subscriber_answer: String,
    /// The SFU's answer to the publisher's offer.
    publisher_answer: String,
    /// The SFU's own offer to the subscriber, for the publisher's track.
    sfu_offer: String,
    /// When the publisher's first media datagram went on the wire.
    first_media_at: Instant,
    /// When the publisher stopped being driven.
    ended_at: Instant,
    /// The main SSRC the publisher sent its video on.
    video_ssrc: u32,
    /// Every REMB the publisher received.
    rembs: Vec<(Instant, u64, Vec<u32>)>,
}

/// How long the publisher of [`run_bitrate_control`] sends for.
const BITRATE_RUN_SENDING: Duration = Duration::from_millis(3500);

/// One publisher sending video and one subscriber, on an SFU started with
/// `bitrate`. The publisher sends one small packet every 10 ms for
/// [`BITRATE_RUN_SENDING`], far below any start value, so the SFU's
/// estimate has no reason to move from where it starts.
async fn run_bitrate_control(bitrate: BitrateControl) -> BitrateRun {
    let peers = PeerRegistry::new();
    let mut registered = register_subscribers(&peers, "bitrate-control", 1).await;
    let (b_id, mut b_rx) = registered.pop().expect("one registered");
    let (media, sfu_addr) = spawn_media_task_with_bitrate_control(
        "127.0.0.1:0".parse().unwrap(),
        None,
        peers,
        MediaTaskOptions::default(),
        bitrate,
    )
    .await
    .expect("UDP bind on loopback:0 succeeds");

    let mut a = SyntheticClient::new_rtp_mode_video_publisher(false).await;
    let mut b = SyntheticClient::new_rtp_mode_video(false).await;

    let (b_offer, b_pending) = b.create_video_subscriber_offer();
    let subscriber_answer = media
        .accept_offer(b_id.clone(), "bitrate-call".to_owned(), b_offer)
        .await
        .expect("SFU accepts the subscriber offer");
    b.accept_answer(b_pending, &subscriber_answer)
        .expect("B applies the SFU's answer");
    let (a_offer, a_pending, mid) = a.create_video_publisher_offer();
    assert!(
        a_offer.contains("transport-cc") && a_offer.contains("transport-wide-cc"),
        "the publisher does offer transport-wide congestion control"
    );
    let publisher_answer = media
        .accept_offer(fresh_peer_id(), "bitrate-call".to_owned(), a_offer)
        .await
        .expect("SFU accepts A's publisher offer");
    a.accept_answer(a_pending, &publisher_answer)
        .expect("A applies the SFU's answer");

    let deadline = Instant::now() + CROSS_FANOUT_DEADLINE;
    let mut buf = [0u8; 2048];
    let mut sfu_offer: Option<String> = None;
    let mut first_media_at: Option<Instant> = None;
    let mut next_write = Instant::now();
    let mut seq: u64 = 1;
    loop {
        let now = Instant::now();
        assert!(now < deadline, "the bitrate-control run never finished");
        if first_media_at.is_some_and(|at| now.duration_since(at) >= BITRATE_RUN_SENDING)
            && sfu_offer.is_some()
        {
            break;
        }
        if a.is_connected() && now >= next_write {
            let rtp_time = u32::try_from(seq * 900).expect("a short test stream");
            if a.write_video_rtp(mid, UPLINK_VP8_PT, seq, rtp_time, true, &[0x5a; 200]) {
                seq += 1;
                next_write = now + Duration::from_millis(10);
            }
        }
        for client in [&mut a, &mut b] {
            let _ = client.drain().await;
        }
        if first_media_at.is_none() {
            first_media_at = a.rtp_out.first().map(|rtp| rtp.at);
        }
        if sfu_offer.is_none() && b.is_connected() {
            if let Ok(ServerFrame::SdpOffer { sdp }) = b_rx.try_recv() {
                sfu_offer = Some(sdp);
            }
        }
        for client in [&mut a, &mut b] {
            while let Ok((n, src)) = client.socket.try_recv_from(&mut buf) {
                client.feed_receive(&buf[..n], src, sfu_addr);
            }
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
        for client in [&mut a, &mut b] {
            client.feed_timeout();
        }
    }
    let _ = a.drain().await;
    let (video_ssrc, _rtx) = a.send_ssrcs(mid).expect("A has a video send stream");
    BitrateRun {
        subscriber_answer,
        publisher_answer,
        sfu_offer: sfu_offer.expect("the loop ends only with the offer"),
        first_media_at: first_media_at.expect("the loop ends only after media"),
        ended_at: Instant::now(),
        video_ssrc,
        rembs: a.rembs_seen.clone(),
    }
}

/// A publisher on a mobile uplink: 1.2 Mbit/s of video for thirty
/// seconds through a path whose delay moves by 20 to 60 ms twice a second
/// and that loses nothing and limits nothing. Returns every REMB the
/// publisher received with its time since the first media packet.
async fn run_jittery_uplink() -> Vec<(Duration, u64)> {
    const SENDING: Duration = Duration::from_secs(30);
    let estimator = EstimatorConfig {
        start_bps: 1_500_000,
        max_bps: 5_000_000,
    };
    // `LAKKO_SFU_E2E_LOG=1` prints the SFU's own lines, the estimate's
    // interval line among them (how often the detector was doubted).
    if std::env::var_os("LAKKO_SFU_E2E_LOG").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_writer(std::io::stderr)
            .try_init();
    }
    let peers = PeerRegistry::new();
    let (media, sfu_addr) = spawn_media_task_with_bitrate_control(
        "127.0.0.1:0".parse().unwrap(),
        None,
        peers,
        MediaTaskOptions {
            diagnostics: Some(Duration::from_secs(5)),
            ..MediaTaskOptions::default()
        },
        BitrateControl::Server(estimator),
    )
    .await
    .expect("UDP bind on loopback:0 succeeds");

    let mut a = SyntheticClient::new_rtp_mode_video_publisher(false).await;
    let (a_offer, a_pending, mid) = a.create_video_publisher_offer();
    let answer = media
        .accept_offer(fresh_peer_id(), "jittery-uplink".to_owned(), a_offer)
        .await
        .expect("SFU accepts the publisher offer");
    a.accept_answer(a_pending, &answer)
        .expect("the publisher applies the SFU's answer");

    let deadline = Instant::now() + SENDING + Duration::from_secs(30);
    let mut buf = [0u8; 2048];
    let mut first_media_at: Option<Instant> = None;
    let mut next_write = Instant::now();
    let mut seq: u64 = 1;
    loop {
        let now = Instant::now();
        assert!(now < deadline, "the jittery-uplink run never finished");
        if first_media_at.is_some_and(|at| now.duration_since(at) >= SENDING) {
            break;
        }
        if a.is_connected() && now >= next_write {
            // Three packets of 1000 bytes every 20 ms: 1.2 Mbit/s.
            let rtp_time = u32::try_from(seq * 600).expect("a short test stream");
            let mut wrote = true;
            for i in 0..3u64 {
                wrote &=
                    a.write_video_rtp(mid, UPLINK_VP8_PT, seq + i, rtp_time, i == 2, &[0x5a; 1000]);
            }
            if wrote {
                seq += 3;
                next_write += Duration::from_millis(20);
                if next_write + Duration::from_millis(200) < now {
                    next_write = now;
                }
            }
        }
        let _ = a.drain().await;
        if first_media_at.is_none() {
            first_media_at = a.rtp_out.first().map(|rtp| rtp.at);
            if let Some(at) = first_media_at {
                a.uplink_jitter_from = Some(at + Duration::from_secs(2));
            }
        }
        a.rtp_out.clear();
        a.wire_headers.clear();
        while let Ok((n, src)) = a.socket.try_recv_from(&mut buf) {
            a.feed_receive(&buf[..n], src, sfu_addr);
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
        a.feed_timeout();
    }
    let first = first_media_at.expect("the loop ends only after media");
    a.rembs_seen
        .iter()
        .map(|(at, bps, _)| (at.saturating_duration_since(first), *bps))
        .collect()
}

/// A publisher whose uplink has a mobile network's jitter and no limit is
/// not cut: every REMB it is sent, from first to last, is at least the
/// rate it is sending. On a live call (2026-10-07) such a publisher was
/// walked from 2.7 Mbit/s to 53 kbit/s by cuts for "over-use" with no
/// loss, each anchored to the rate the cut before it had imposed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::print_stderr)]
async fn a_jittery_uplink_with_no_limit_is_not_cut() {
    /// What the publisher sends. No REMB may be below it.
    const SENT_BPS: u64 = 1_200_000;
    let rembs = tokio::time::timeout(Duration::from_secs(90), Box::pin(run_jittery_uplink()))
        .await
        .expect("the run ends by itself");
    let lowest = rembs.iter().map(|(_, bps)| *bps).min().expect("REMBs came");
    let highest = rembs.iter().map(|(_, bps)| *bps).max().expect("REMBs came");
    eprintln!(
        "jittery uplink: {} REMBs, lowest {lowest} highest {highest} bit/s",
        rembs.len()
    );
    assert!(rembs.len() >= 25, "about one a second: {}", rembs.len());
    assert!(
        lowest >= SENT_BPS,
        "a REMB below what the path was carrying: {:?}",
        rembs
            .iter()
            .filter(|(_, bps)| *bps < SENT_BPS)
            .collect::<Vec<_>>()
    );
}

/// The payload of the `n`-th packet of [`run_lossy_subscriber`]: its
/// number in the first four bytes, a length that runs through 5 to 68,
/// and a last byte that runs through the values a padding count could be
/// mistaken in (0, 1, 4, 255).
fn lossy_payload(n: u32) -> Vec<u8> {
    let len = 5 + (n as usize % 64);
    let mut payload = vec![0x5a; len];
    payload[..4].copy_from_slice(&n.to_be_bytes());
    payload[len - 1] = [0x00, 0x01, 0x04, 0xff][n as usize % 4];
    payload
}

/// One publisher, one subscriber that loses every seventh media packet on
/// the way in and asks for it again. Returns what arrived on the
/// subscriber's retransmission stream (padding bit, payload) and how many
/// packets it lost.
async fn run_lossy_subscriber() -> (Vec<(bool, Vec<u8>)>, u64) {
    const PACKETS: u32 = 400;
    let peers = PeerRegistry::new();
    let mut registered = register_subscribers(&peers, "lossy-subscriber", 1).await;
    let (b_id, mut b_rx) = registered.pop().expect("one registered");
    let (media, sfu_addr) = spawn_media_task_with_options(
        "127.0.0.1:0".parse().unwrap(),
        None,
        peers,
        MediaTaskOptions::default(),
    )
    .await
    .expect("UDP bind on loopback:0 succeeds");

    let mut a = SyntheticClient::new_rtp_mode_video_publisher(false).await;
    let mut b =
        SyntheticClient::build(Rtc::builder().set_rtp_mode(true).enable_raw_packets(true)).await;
    b.lose_in_every = Some(7);

    let (b_offer, b_pending) = b.create_video_subscriber_offer();
    let answer = media
        .accept_offer(b_id.clone(), "lossy-call".to_owned(), b_offer)
        .await
        .expect("SFU accepts the subscriber offer");
    b.accept_answer(b_pending, &answer)
        .expect("B applies the SFU's answer");
    let (a_offer, a_pending, mid) = a.create_video_publisher_offer();
    let answer = media
        .accept_offer(fresh_peer_id(), "lossy-call".to_owned(), a_offer)
        .await
        .expect("SFU accepts the publisher offer");
    a.accept_answer(a_pending, &answer)
        .expect("A applies the SFU's answer");

    let deadline = Instant::now() + Duration::from_secs(40);
    let mut buf = [0u8; 2048];
    let mut next_write = Instant::now();
    let mut n: u32 = 0;
    let mut subscribed = false;
    let mut done_at: Option<Instant> = None;
    loop {
        let now = Instant::now();
        assert!(now < deadline, "the lossy-subscriber run never finished");
        // A second after the last packet: time for the last NACKs.
        if done_at.is_some_and(|at| now >= at + Duration::from_secs(1)) {
            break;
        }
        if subscribed && a.is_connected() && b.is_connected() && n < PACKETS && now >= next_write {
            let seq = u64::from(n) + 1;
            let rtp_time = n * 900;
            if a.write_video_rtp(mid, UPLINK_VP8_PT, seq, rtp_time, true, &lossy_payload(n)) {
                n += 1;
                next_write = now + Duration::from_millis(5);
                if n == PACKETS {
                    done_at = Some(now);
                }
            }
        }
        for client in [&mut a, &mut b] {
            let _ = client.drain().await;
        }
        a.rtp_out.clear();
        a.wire_headers.clear();
        b.rtp_in.clear();
        if let Ok(ServerFrame::SdpOffer { sdp }) = b_rx.try_recv() {
            let answer = b.answer_renegotiation(&sdp).expect("B answers");
            media
                .accept_renegotiation_answer(b_id.clone(), answer)
                .await
                .expect("the SFU takes B's answer");
            subscribed = true;
        }
        for client in [&mut a, &mut b] {
            while let Ok((len, src)) = client.socket.try_recv_from(&mut buf) {
                client.feed_receive(&buf[..len], src, sfu_addr);
                let _ = client.drain().await;
            }
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
        for client in [&mut a, &mut b] {
            client.feed_timeout();
        }
    }
    (b.rtx_in.clone(), b.main_in_lost)
}

/// A subscriber that loses packets and asks for them again gets
/// retransmissions it can use: no retransmission carries the RTP padding
/// bit, and each one's payload behind its two bytes of original sequence
/// number is, byte for byte, a payload the publisher sent.
///
/// A libwebrtc receiver restores the original packet behind a copy of
/// the retransmission's header. With the padding bit set there and the
/// padding itself gone, it then took as many bytes off the tail as the
/// last byte says: on a live call (2026-10-07) fifteen of sixteen
/// retransmitted frames reached a Chrome viewer one byte short. The
/// byte-exact model of that restore is the vendored crate's
/// `a_retransmission_restores_to_the_payload_that_was_sent`; this is the
/// same property through the SFU, for retransmissions a NACK asked for.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::print_stderr)]
async fn a_retransmission_to_a_subscriber_carries_the_original_payload_and_no_padding() {
    let (rtx, lost) =
        tokio::time::timeout(Duration::from_secs(60), Box::pin(run_lossy_subscriber()))
            .await
            .expect("the run ends by itself");
    let padded = rtx.iter().filter(|(padding, _)| *padding).count();
    let mut restored = 0usize;
    for (_, payload) in &rtx {
        // Padding-only packets of the retransmission stream carry nothing.
        if payload.len() < 2 + 5 {
            continue;
        }
        let original = &payload[2..];
        let n = u32::from_be_bytes([original[0], original[1], original[2], original[3]]);
        assert_eq!(
            original,
            &lossy_payload(n)[..],
            "retransmission of packet {n} is not the payload that was sent"
        );
        restored += 1;
    }
    eprintln!(
        "lossy subscriber: lost {lost} first transmissions, received {} packets on the \
         retransmission stream, {restored} of them carrying a payload, {padded} with the \
         padding bit",
        rtx.len()
    );
    assert!(lost >= 40, "the subscriber lost packets: {lost}");
    assert!(
        restored as u64 >= lost * 9 / 10,
        "nearly every lost packet came again: {restored} of {lost}"
    );
    assert_eq!(padded, 0, "no retransmission carries the padding bit");
}

/// `LAKKO_SFU_BITRATE_CONTROL=server` (ADR-0068): no SDP the SFU writes
/// names transport-wide congestion control, on any media line, as a header
/// extension or as RTCP feedback, while `goog-remb` and `abs-send-time`
/// stay; and a publisher is sent a REMB carrying the SFU's estimate within
/// a second of starting to send, and again at least once a second.
#[tokio::test]
async fn server_bitrate_control_negotiates_no_transport_cc_and_sends_remb() {
    let estimator = EstimatorConfig {
        start_bps: 1_500_000,
        max_bps: 5_000_000,
    };
    let run = Box::pin(run_bitrate_control(BitrateControl::Server(estimator))).await;

    for (what, sdp) in [
        ("answer to the subscriber", &run.subscriber_answer),
        ("answer to the publisher", &run.publisher_answer),
        ("offer to the subscriber", &run.sfu_offer),
    ] {
        assert!(
            !sdp.contains("transport-cc") && !sdp.contains("transport-wide-cc"),
            "{what} must not name transport-wide congestion control:\n{sdp}"
        );
        let sections = video_sections(sdp);
        assert!(!sections.is_empty(), "{what} has a video section");
        for section in sections {
            assert!(
                section.contains("goog-remb"),
                "{what} keeps goog-remb on video:\n{section}"
            );
            assert!(
                section.contains("abs-send-time"),
                "{what} keeps abs-send-time on video:\n{section}"
            );
            assert!(
                section.contains("nack") && section.contains("VP8/90000"),
                "{what} keeps the rest of the video line:\n{section}"
            );
        }
    }

    assert!(!run.rembs.is_empty(), "the publisher received REMB");
    let first = run.rembs[0].0;
    assert!(
        first.saturating_duration_since(run.first_media_at) <= Duration::from_secs(1),
        "the first REMB came {:?} after the first media packet",
        first.saturating_duration_since(run.first_media_at)
    );
    let mut previous = first;
    for (at, _, _) in &run.rembs[1..] {
        assert!(
            at.saturating_duration_since(previous) <= Duration::from_secs(1),
            "two REMBs {:?} apart",
            at.saturating_duration_since(previous)
        );
        previous = *at;
    }
    assert!(
        run.ended_at.saturating_duration_since(previous) <= Duration::from_secs(1),
        "no REMB in the last {:?} of the run",
        run.ended_at.saturating_duration_since(previous)
    );
    assert!(
        run.rembs.len() >= 4,
        "about one every half second over 3.5 s, got {}",
        run.rembs.len()
    );
    for (_, bps, ssrcs) in &run.rembs {
        assert_eq!(
            ssrcs,
            &vec![run.video_ssrc],
            "a REMB names the publisher's video stream"
        );
        assert_eq!(
            *bps, estimator.start_bps,
            "the estimate stays at its start value for a stream this small"
        );
    }
}

/// `LAKKO_SFU_BITRATE_CONTROL=client` (the default) is the behaviour
/// before ADR-0068: transport-wide congestion control is negotiated on
/// every SDP the SFU writes, and the SFU sends a publisher no REMB.
#[tokio::test]
async fn client_bitrate_control_keeps_transport_cc_and_sends_no_remb() {
    let run = Box::pin(run_bitrate_control(BitrateControl::Client)).await;
    for (what, sdp) in [
        ("answer to the subscriber", &run.subscriber_answer),
        ("answer to the publisher", &run.publisher_answer),
        ("offer to the subscriber", &run.sfu_offer),
    ] {
        assert!(
            sdp.contains("transport-cc") && sdp.contains("transport-wide-cc"),
            "{what} names transport-wide congestion control as before:\n{sdp}"
        );
        assert!(sdp.contains("goog-remb") && sdp.contains("abs-send-time"));
    }
    assert!(
        run.rembs.is_empty(),
        "the SFU sends no REMB in client mode: {:?}",
        run.rembs
    );
}

// ============================================================
// Uplink dip measurement (a publisher's uplink loses most of its
// datagrams for one second). Ignored by default: these print numbers
// and assert nothing about them.
// ============================================================

/// One measured case.
#[derive(Clone, Copy)]
struct DipCase {
    /// Probability that a datagram from A is lost inside the window.
    p: f64,
    /// RTP packets per video frame.
    packets_per_frame: u64,
    /// Seed of the loss generator.
    seed: u64,
    /// Whether A keeps `str0m`'s default cap on retransmitted bytes (15% of
    /// what it sent over the last second; queued retransmissions are thrown
    /// away above it). A browser has no such rule, so every case is run with
    /// and without it.
    rtx_cap: bool,
}

/// Frames a second A writes in every case.
const DIP_FPS: u64 = 25;
/// How long A streams.
const DIP_STREAM: Duration = Duration::from_secs(6);
/// When the loss window opens, from the first frame, and how long it lasts.
const DIP_WINDOW_OPENS: Duration = Duration::from_millis(2500);
const DIP_WINDOW: Duration = Duration::from_secs(1);
/// How long the clients keep running after the last frame: longer than the
/// SFU keeps asking for a missing packet (about 1.4 s).
const DIP_SETTLE: Duration = Duration::from_millis(2500);
/// Media payload bytes per packet.
const DIP_PAYLOAD: usize = 1100;
/// First sequence number A writes.
const DIP_FIRST_SEQ: u64 = 5000;

/// What one run observed, in milliseconds unless said otherwise.
struct DipRun {
    case: DipCase,
    /// Media packets first sent inside the window, and how many of those
    /// were lost at that first transmission.
    in_window: usize,
    lost_first: usize,
    /// Of the packets sent inside the window, how many never reached B.
    never: usize,
    /// First send to arrival at B, packets sent inside the window that
    /// arrived: min, median, p95, max.
    delay: [f64; 4],
    /// The same for the packets lost at their first transmission only.
    repaired_delay: [f64; 4],
    /// First send to arrival at B before the window: median and max.
    baseline: [f64; 2],
    /// Longest gap between the moments consecutive frames were complete
    /// (each with every earlier frame), over the frames that did complete,
    /// and the frame it ended at.
    stall: f64,
    stall_frame: usize,
    /// First frame that never completed at B, if any.
    first_incomplete: Option<usize>,
    /// When every frame sent before the window closed was complete at B,
    /// relative to the window's end. `None` if one never was.
    caught_up: Option<f64>,
    /// When the last packet that was lost at its first transmission reached
    /// B, relative to the window's end.
    last_repair: Option<f64>,
    /// Sequence numbers named in NACKs A received on its media stream, inside
    /// and after the window; distinct numbers named; and when the last NACK
    /// arrived relative to the window's end.
    nacks_in: usize,
    nacks_after: usize,
    nacked_distinct: usize,
    last_nack: Option<f64>,
    /// Retransmissions A put on the wire inside the window (reached the SFU /
    /// lost) and after it.
    rtx_in_ok: usize,
    rtx_in_lost: usize,
    rtx_after: usize,
    /// First send of a lost packet to the first NACK naming it reaching A:
    /// median and max.
    detect: [f64; 2],
    /// The last NACK naming a packet before it arrived at B, to that
    /// arrival: median and max.
    turnaround: [f64; 2],
    /// Keyframe requests the SFU sent A over the whole run.
    plis: usize,
    /// Media datagrams B received twice on its main stream, and datagrams it
    /// received on any other stream (the SFU's retransmissions to B).
    b_duplicates: usize,
    b_rtx: usize,
    /// Packets that reached B with a lower number than one that arrived
    /// before them, among packets that were NOT lost at first transmission.
    unlost_reordered: usize,
    /// The packet sent inside the window that took longest to reach B: when
    /// it was first sent relative to the window's opening, when each NACK
    /// naming it reached A relative to that first send, and how long it took.
    slowest: Option<(f64, Vec<f64>, f64)>,
    /// Median time between the k-th and the (k+1)-th NACK naming one packet,
    /// for k = 1, 2, ..., over every packet named more than once.
    nack_spacing: Vec<f64>,
}

/// `[min, median, p95, max]` of a set of millisecond values; zeros when
/// empty.
fn dip_quartet(values: &mut [f64]) -> [f64; 4] {
    if values.is_empty() {
        return [0.0; 4];
    }
    values.sort_by(f64::total_cmp);
    let at = |q: usize| values[((values.len() * q).div_ceil(100)).saturating_sub(1)];
    [values[0], at(50), at(95), values[values.len() - 1]]
}

fn dip_ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// The generic frame descriptor value of packet `i` of `n` in frame `frame`:
/// a first-packet value (a key frame for frame 0, one dependency on the
/// previous frame otherwise) or the one-byte continuation.
fn dip_descriptor(frame: u64, i: u64, n: u64) -> Vec<u8> {
    let end = if i + 1 == n { 0x40 } else { 0x00 };
    if i > 0 {
        return vec![0x30 | end];
    }
    let id = (frame & 0xffff) as u16;
    let [lo, hi] = id.to_le_bytes();
    if frame == 0 {
        // B, F, L set, no dependencies: width and height follow.
        vec![0xb0 | end, 0x01, lo, hi, 0x05, 0x00, 0x02, 0xd0]
    } else {
        // D set: one frame-diff of 1, no extended byte.
        vec![0xb8 | end, 0x01, lo, hi, 0x04]
    }
}

/// Publisher A streams paced video to subscriber B through the production
/// media task; for one second each datagram A sends is lost with probability
/// `case.p`. Returns what was observed at A's and B's sockets.
#[allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss
)]
async fn run_uplink_dip(case: DipCase) -> DipRun {
    use lakko_svc_sfu::frame_descriptor::PacketDescriptor;
    use std::collections::BTreeMap;

    let call = "uplink-dip-call";
    let peers = PeerRegistry::new();
    let mut registered = register_subscribers(&peers, call, 1).await;
    let (b_id, mut b_rx) = registered.pop().expect("one registered");
    let (media_task, sfu_addr) = spawn_media_task("127.0.0.1:0".parse().unwrap(), peers)
        .await
        .expect("UDP bind on loopback:0 succeeds");

    let mut a = SyntheticClient::build(
        Rtc::builder()
            .set_rtp_mode(true)
            .enable_raw_packets(true)
            .set_extension(
                GENERIC_FRAME_DESCRIPTOR_ID,
                generic_frame_descriptor_extension(),
            ),
    )
    .await;
    let mut b = SyntheticClient::build(Rtc::builder().set_rtp_mode(true).set_extension(
        GENERIC_FRAME_DESCRIPTOR_ID,
        generic_frame_descriptor_extension(),
    ))
    .await;

    let (b_offer, b_pending) = b.create_video_subscriber_offer();
    let b_answer = media_task
        .accept_offer(b_id.clone(), call.to_owned(), b_offer)
        .await
        .expect("SFU accepts B's subscriber offer");
    b.accept_answer(b_pending, &b_answer)
        .expect("B applies the SFU's answer");
    let (a_offer, a_pending, mid) = a.create_video_publisher_offer();
    let a_answer = media_task
        .accept_offer(fresh_peer_id(), call.to_owned(), a_offer)
        .await
        .expect("SFU accepts A's publisher offer");
    a.accept_answer(a_pending, &a_answer)
        .expect("A applies the SFU's answer");

    let n = case.packets_per_frame;
    let frame_every = Duration::from_micros(1_000_000 / DIP_FPS);
    let total_frames = DIP_STREAM.as_micros() as u64 / frame_every.as_micros() as u64;
    let payload = vec![0x5a_u8; DIP_PAYLOAD];

    let give_up = Instant::now() + Duration::from_secs(40);
    let mut reneg = false;
    let mut t0: Option<Instant> = None;
    let mut window: Option<(Instant, Instant)> = None;
    let mut ssrcs: Option<(u32, u32)> = None;
    let mut frames_written: u64 = 0;
    let mut finish_at: Option<Instant> = None;
    let mut abuf = [0u8; 2048];
    let mut bbuf = [0u8; 2048];

    loop {
        let now = Instant::now();
        assert!(
            now < give_up,
            "the dip run did not finish: frames_written={frames_written} a_connected={} \
             b_connected={} reneg={reneg}",
            a.is_connected(),
            b.is_connected()
        );
        if finish_at.is_some_and(|at| now >= at) {
            break;
        }
        let _ = a.drain().await;
        let _ = b.drain().await;
        if !reneg && b.is_connected() {
            while let Ok(frame) = b_rx.try_recv() {
                if let ServerFrame::SdpOffer { sdp } = frame {
                    let answer = b.answer_renegotiation(&sdp).expect("B answers");
                    reneg = media_task
                        .accept_renegotiation_answer(b_id.clone(), answer)
                        .await
                        .is_ok();
                }
            }
        }
        if t0.is_none() && a.is_connected() && b.is_connected() && reneg {
            if let Some((main, Some(rtx))) = a.send_ssrcs(mid) {
                if !case.rtx_cap {
                    let mut api = a.rtc.direct_api();
                    let stream = api.stream_tx_by_mid(mid, None).expect("send stream");
                    stream.set_rtx_cache(4000, Duration::from_secs(3), None);
                }
                let start = Instant::now() + Duration::from_millis(50);
                let from = start + DIP_WINDOW_OPENS;
                let until = from + DIP_WINDOW;
                a.dip = Some(DipLoss {
                    from,
                    until,
                    p: case.p,
                    rng: case.seed,
                });
                t0 = Some(start);
                window = Some((from, until));
                ssrcs = Some((main, rtx));
            }
        }
        if let Some(start) = t0 {
            let due = start + frame_every * (frames_written as u32);
            if frames_written < total_frames && Instant::now() >= due {
                let rtp_time = 90_000 + (frames_written * (90_000 / DIP_FPS)) as u32;
                for i in 0..n {
                    let mut ext = str0m::rtp::ExtensionValues::default();
                    ext.user_values
                        .set(PacketDescriptor(dip_descriptor(frames_written, i, n)));
                    let seq = DIP_FIRST_SEQ + frames_written * n + i;
                    let mut api = a.rtc.direct_api();
                    let stream = api.stream_tx_by_mid(mid, None).expect("send stream");
                    stream
                        .write_rtp(
                            UPLINK_VP8_PT.into(),
                            seq.into(),
                            rtp_time,
                            Instant::now(),
                            i + 1 == n,
                            ext,
                            true,
                            payload.clone(),
                        )
                        .expect("A writes a packet");
                }
                frames_written += 1;
                if frames_written == total_frames {
                    finish_at = Some(Instant::now() + DIP_SETTLE);
                }
                // Put the frame on the wire now rather than after the wait
                // below.
                a.feed_timeout();
                let _ = a.drain().await;
            }
        }
        // str0m in RTP mode holds one received packet until the next poll,
        // so each datagram is followed by a drain.
        while let Ok((len, src)) = a.socket.try_recv_from(&mut abuf) {
            a.feed_receive(&abuf[..len], src, sfu_addr);
            let _ = a.drain().await;
        }
        while let Ok((len, src)) = b.socket.try_recv_from(&mut bbuf) {
            b.feed_receive(&bbuf[..len], src, sfu_addr);
            let _ = b.drain().await;
        }
        let a_sock = a.socket.clone();
        let b_sock = b.socket.clone();
        tokio::select! {
            r = a_sock.recv_from(&mut abuf) => {
                if let Ok((len, src)) = r {
                    a.feed_receive(&abuf[..len], src, sfu_addr);
                }
            }
            r = b_sock.recv_from(&mut bbuf) => {
                if let Ok((len, src)) = r {
                    b.feed_receive(&bbuf[..len], src, sfu_addr);
                }
            }
            () = tokio::time::sleep(Duration::from_millis(1)) => {}
        }
        let _ = a.drain().await;
        let _ = b.drain().await;
        a.feed_timeout();
        b.feed_timeout();
    }

    let (main, rtx) = ssrcs.expect("the stream was sent");
    let (from, until) = window.expect("the window was set");

    // seq -> (first send, lost at that first transmission).
    let mut sent: BTreeMap<u16, (Instant, bool)> = BTreeMap::new();
    for (log, lost) in [(&a.rtp_out, false), (&a.rtp_dropped, true)] {
        for p in log.iter().filter(|p| p.ssrc == main) {
            let slot = sent.entry(p.seq).or_insert((p.at, lost));
            if p.at < slot.0 {
                *slot = (p.at, lost);
            }
        }
    }
    assert_eq!(
        sent.len() as u64,
        total_frames * n,
        "A put every written packet on the wire exactly once on its media stream"
    );

    let b_main = b.rtp_in.first().expect("B received media").ssrc;
    let first_seq = (DIP_FIRST_SEQ & 0xffff) as u16;
    let seq_offset = b
        .rtp_in
        .iter()
        .find(|p| p.ssrc == b_main)
        .expect("B received media")
        .seq
        .wrapping_sub(first_seq);
    // A's seq -> first arrival at B's socket.
    let mut arrived: BTreeMap<u16, Instant> = BTreeMap::new();
    let mut b_duplicates = 0usize;
    let mut unlost_reordered = 0usize;
    let mut highest: Option<u16> = None;
    for p in b.rtp_in.iter().filter(|p| p.ssrc == b_main) {
        let seq = p.seq.wrapping_sub(seq_offset);
        if arrived.contains_key(&seq) {
            b_duplicates += 1;
            continue;
        }
        arrived.insert(seq, p.at);
        let lost_first = sent.get(&seq).is_some_and(|s| s.1);
        if highest.is_some_and(|h| seq < h) && !lost_first {
            unlost_reordered += 1;
        }
        highest = Some(highest.map_or(seq, |h| h.max(seq)));
    }
    let b_rtx = b.rtp_in.iter().filter(|p| p.ssrc != b_main).count();

    // NACKs A received, per sequence number.
    let mut nacks: BTreeMap<u16, Vec<Instant>> = BTreeMap::new();
    for (at, ssrc, seq) in &a.nacks_seen {
        if *ssrc == main {
            nacks.entry(*seq).or_default().push(*at);
        }
    }
    let nack_times: Vec<Instant> = nacks.values().flatten().copied().collect();
    let nacks_in = nack_times
        .iter()
        .filter(|t| **t >= from && **t < until)
        .count();
    let nacks_after = nack_times.iter().filter(|t| **t >= until).count();
    let last_nack = nack_times
        .iter()
        .max()
        .map(|t| dip_ms(t.saturating_duration_since(until)));

    let in_win = |at: Instant| at >= from && at < until;
    let rtx_in_ok = a
        .rtp_out
        .iter()
        .filter(|p| p.ssrc == rtx && in_win(p.at))
        .count();
    let rtx_in_lost = a
        .rtp_dropped
        .iter()
        .filter(|p| p.ssrc == rtx && in_win(p.at))
        .count();
    let rtx_after = a
        .rtp_out
        .iter()
        .filter(|p| p.ssrc == rtx && p.at >= until)
        .count();

    let mut baseline = Vec::new();
    let mut delay = Vec::new();
    let mut repaired_delay = Vec::new();
    let mut detect = Vec::new();
    let mut turnaround = Vec::new();
    let mut in_window = 0usize;
    let mut lost_first = 0usize;
    let mut never = 0usize;
    let mut last_repair: Option<Instant> = None;
    let mut slowest: Option<(f64, Vec<f64>, f64)> = None;
    for (seq, (at, lost)) in &sent {
        let got = arrived.get(seq).copied();
        if *at < from {
            assert!(!*lost, "nothing is lost before the window");
            let got = got.expect("every packet sent before the window reached B");
            baseline.push(dip_ms(got.saturating_duration_since(*at)));
            continue;
        }
        if !in_win(*at) {
            assert!(!*lost, "nothing is lost after the window");
            continue;
        }
        in_window += 1;
        lost_first += usize::from(*lost);
        if *lost {
            if let Some(first) = nacks.get(seq).and_then(|v| v.first()) {
                detect.push(dip_ms(first.saturating_duration_since(*at)));
            }
        }
        let Some(got) = got else {
            never += 1;
            continue;
        };
        let took = dip_ms(got.saturating_duration_since(*at));
        delay.push(took);
        if slowest.as_ref().is_none_or(|s| took > s.2) {
            let named = nacks.get(seq).map_or_else(Vec::new, |v| {
                v.iter()
                    .map(|t| dip_ms(t.saturating_duration_since(*at)))
                    .collect()
            });
            slowest = Some((dip_ms(at.saturating_duration_since(from)), named, took));
        }
        if *lost {
            repaired_delay.push(took);
            last_repair = Some(last_repair.map_or(got, |l| l.max(got)));
            if let Some(last) = nacks
                .get(seq)
                .and_then(|v| v.iter().filter(|t| **t <= got).max())
            {
                turnaround.push(dip_ms(got.saturating_duration_since(*last)));
            }
        }
    }

    let mut spacing: Vec<Vec<f64>> = Vec::new();
    for times in nacks.values() {
        for (k, pair) in times.windows(2).enumerate() {
            if spacing.len() <= k {
                spacing.push(Vec::new());
            }
            spacing[k].push(dip_ms(pair[1].saturating_duration_since(pair[0])));
        }
    }
    let nack_spacing: Vec<f64> = spacing
        .iter_mut()
        .map(|values| dip_quartet(values)[1])
        .collect();

    // When each frame, with every frame before it, was complete at B.
    let mut complete_at: Vec<Instant> = Vec::new();
    let mut first_incomplete = None;
    let mut running: Option<Instant> = None;
    for frame in 0..total_frames {
        let mut frame_done = running;
        let mut whole = true;
        for i in 0..n {
            let seq = ((DIP_FIRST_SEQ + frame * n + i) & 0xffff) as u16;
            match arrived.get(&seq) {
                Some(at) => frame_done = Some(frame_done.map_or(*at, |d| d.max(*at))),
                None => whole = false,
            }
        }
        if !whole {
            first_incomplete = Some(frame as usize);
            break;
        }
        running = frame_done;
        complete_at.push(frame_done.expect("a whole frame has an arrival time"));
    }
    let (mut stall, mut stall_frame) = (0.0_f64, 0usize);
    for (i, pair) in complete_at.windows(2).enumerate() {
        let gap = dip_ms(pair[1].saturating_duration_since(pair[0]));
        if gap > stall {
            stall = gap;
            stall_frame = i + 1;
        }
    }
    // The last frame whose first packet left before the window closed.
    let last_window_frame = (0..total_frames)
        .rev()
        .find(|f| {
            let seq = ((DIP_FIRST_SEQ + f * n) & 0xffff) as u16;
            sent.get(&seq).is_some_and(|s| s.0 < until)
        })
        .expect("frames were sent before the window closed") as usize;
    let signed_ms = |at: Instant| {
        if at >= until {
            dip_ms(at.duration_since(until))
        } else {
            -dip_ms(until.duration_since(at))
        }
    };
    let caught_up = complete_at.get(last_window_frame).map(|at| signed_ms(*at));

    DipRun {
        case,
        in_window,
        lost_first,
        never,
        delay: dip_quartet(&mut delay),
        repaired_delay: dip_quartet(&mut repaired_delay),
        baseline: {
            let q = dip_quartet(&mut baseline);
            [q[1], q[3]]
        },
        stall,
        stall_frame,
        first_incomplete,
        caught_up,
        last_repair: last_repair.map(signed_ms),
        nacks_in,
        nacks_after,
        nacked_distinct: nacks.len(),
        last_nack,
        rtx_in_ok,
        rtx_in_lost,
        rtx_after,
        detect: {
            let q = dip_quartet(&mut detect);
            [q[1], q[3]]
        },
        turnaround: {
            let q = dip_quartet(&mut turnaround);
            [q[1], q[3]]
        },
        plis: a.keyframe_requests_seen,
        b_duplicates,
        b_rtx,
        unlost_reordered,
        slowest,
        nack_spacing,
    }
}

/// One line per run, as a table.
#[allow(clippy::print_stderr)]
fn print_dip_table(runs: &[DipRun]) {
    let opt = |v: Option<f64>| v.map_or_else(|| "never".to_owned(), |v| format!("{v:.0}"));
    eprintln!(
        "\nuplink dip: {DIP_FPS} frames/s, {DIP_PAYLOAD}-byte packets, loss window {} ms, \
         times in ms",
        DIP_WINDOW.as_millis()
    );
    eprintln!(
        "{:>5} {:>5} {:>4} | {:>4} {:>4} {:>5} | {:>24} | {:>24} | {:>8} | {:>7} {:>6} {:>8} \
         {:>8} | {:>11} {:>5} {:>8} | {:>11} {:>5} | {:>9} {:>9} | {:>4} {:>4} {:>5} {:>4}",
        "p",
        "pkt/s",
        "cap",
        "sent",
        "lost",
        "never",
        "delay min/med/p95/max",
        "repaired min/med/p95/max",
        "base m/x",
        "stall",
        "@frame",
        "caughtup",
        "lastrep",
        "nack in/aft",
        "seqs",
        "lastnack",
        "rtx ok/lost",
        "after",
        "detect",
        "turnarnd",
        "pli",
        "dupB",
        "rtxB",
        "ooo"
    );
    for r in runs {
        let stall = r.first_incomplete.map_or_else(
            || format!("{:.0}", r.stall),
            |f| format!("{:.0}+f{f}", r.stall),
        );
        eprintln!(
            "{:>5.2} {:>5} {:>4} | {:>4} {:>4} {:>5} | {:>24} | {:>24} | {:>8} | {:>7} {:>6} \
             {:>8} {:>8} | {:>11} {:>5} {:>8} | {:>11} {:>5} | {:>9} {:>9} | {:>4} {:>4} {:>5} \
             {:>4}",
            r.case.p,
            r.case.packets_per_frame * DIP_FPS,
            if r.case.rtx_cap { "0.15" } else { "none" },
            r.in_window,
            r.lost_first,
            r.never,
            format!(
                "{:.1}/{:.0}/{:.0}/{:.0}",
                r.delay[0], r.delay[1], r.delay[2], r.delay[3]
            ),
            format!(
                "{:.0}/{:.0}/{:.0}/{:.0}",
                r.repaired_delay[0], r.repaired_delay[1], r.repaired_delay[2], r.repaired_delay[3]
            ),
            format!("{:.1}/{:.1}", r.baseline[0], r.baseline[1]),
            stall,
            r.stall_frame,
            opt(r.caught_up),
            opt(r.last_repair),
            format!("{}/{}", r.nacks_in, r.nacks_after),
            r.nacked_distinct,
            opt(r.last_nack),
            format!("{}/{}", r.rtx_in_ok, r.rtx_in_lost),
            r.rtx_after,
            format!("{:.0}/{:.0}", r.detect[0], r.detect[1]),
            format!("{:.1}/{:.0}", r.turnaround[0], r.turnaround[1]),
            r.plis,
            r.b_duplicates,
            r.b_rtx,
            r.unlost_reordered,
        );
    }
    eprintln!("\nslowest packet of each run and the spacing of repeated NACKs:");
    for r in runs {
        let list = |v: &[f64]| {
            v.iter()
                .map(|x| format!("{x:.0}"))
                .collect::<Vec<_>>()
                .join(",")
        };
        let slowest = r
            .slowest
            .as_ref()
            .map_or_else(String::new, |(at, named, took)| {
                format!(
                    "sent {at:.0} ms into the window, NACKed at +[{}], at B after {took:.0}",
                    list(named)
                )
            });
        eprintln!(
            "{:>5.2} {:>5} {:>4} | {slowest} | median NACK spacing [{}]",
            r.case.p,
            r.case.packets_per_frame * DIP_FPS,
            if r.case.rtx_cap { "0.15" } else { "none" },
            list(&r.nack_spacing),
        );
    }
}

/// The cases of the uplink-dip measurement: loss probability and packets
/// per frame (at 25 frames a second: 10 -> 250 packets/s, 4 -> 100, 2 -> 50,
/// 1 -> 25).
const DIP_CASES: [(f64, u64); 7] = [
    (0.70, 10),
    (0.70, 4),
    (0.70, 2),
    (0.70, 1),
    (0.20, 10),
    (0.20, 1),
    (1.0, 10),
];

/// Measure how long a subscriber waits when a publisher's uplink loses most
/// of its datagrams for one second. Prints a table; asserts nothing about
/// the numbers.
///
/// `LAKKO_DIP_SEED` (a number, default 1) changes the loss pattern.
/// `LAKKO_DIP_CAP` picks the publisher: `none` (no cap on retransmitted
/// bytes), `default` (`str0m`'s 15%), or both when unset.
#[tokio::test]
#[ignore = "a measurement, about two and a half minutes: run by name with --nocapture"]
async fn measure_uplink_dip() {
    let seed: u64 = std::env::var("LAKKO_DIP_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let mut runs = Vec::new();
    let caps: &[bool] = match std::env::var("LAKKO_DIP_CAP").as_deref() {
        Ok("none") => &[false],
        Ok("default") => &[true],
        _ => &[true, false],
    };
    for &rtx_cap in caps {
        for (i, (p, packets_per_frame)) in DIP_CASES.into_iter().enumerate() {
            let case = DipCase {
                p,
                packets_per_frame,
                seed: seed.wrapping_mul(1000) + i as u64,
                rtx_cap,
            };
            runs.push(Box::pin(run_uplink_dip(case)).await);
        }
    }
    print_dip_table(&runs);
}

// ============================================================
// Uplink fallback: a simulcast publisher (`q` light, `f` full) loses
// most of its datagrams for one second; the subscriber is moved to
// the light layer for the dip and back after it.
// ============================================================

/// How long the publisher streams in a fallback run: the dip, the time the
/// SFU waits before it moves the subscriber back, and room after that.
const FB_STREAM: Duration = Duration::from_secs(11);
/// How long the clients keep running after the last frame.
const FB_SETTLE: Duration = Duration::from_millis(1500);
/// Packets per delta frame on each layer at 25 frames a second: 2 on the
/// light layer (50 packets a second) and 10 on the full one (250).
const FB_Q_PACKETS: u64 = 2;
const FB_F_PACKETS: u64 = 10;
/// A key frame is this many times a delta frame, in packets.
const FB_KEY_FACTOR: u64 = 3;
/// Media payload bytes per packet on each layer: 2 x 375 bytes at 25
/// frames a second is 150 kbit/s; 10 x 1100 is 2.2 Mbit/s.
const FB_Q_PAYLOAD: usize = 375;
const FB_F_PAYLOAD: usize = 1100;
/// First sequence number on each layer.
const FB_Q_FIRST_SEQ: u64 = 20_000;
const FB_F_FIRST_SEQ: u64 = 5000;

/// One frame the publisher wrote on one layer.
#[derive(Clone, Copy)]
struct FbFrame {
    key: bool,
    /// Packets in the frame.
    n: u64,
    /// When it was written.
    at: Instant,
}

/// One simulcast layer of the publisher in a fallback run.
struct FbLayer {
    /// The simulcast layer it is sent on; `None` for a plain sender.
    rid: Option<Rid>,
    /// The publisher's media line it is sent on, once offered.
    mid: Option<Mid>,
    /// First payload byte of its packets.
    tag: u8,
    /// Packets per delta frame.
    packets: u64,
    /// Payload bytes per packet.
    payload: usize,
    next_seq: u64,
    frames: Vec<FbFrame>,
    /// The next frame is to be a key frame.
    want_key: bool,
}

/// What one fallback run observed on the subscriber's **main** track.
/// Times in milliseconds.
struct FallbackRun {
    /// The layer of each stretch of packets the subscriber received, in
    /// order of arrival, with when the stretch began relative to the
    /// opening of the loss window.
    stretches: Vec<(char, f64)>,
    /// Longest gap between the moments consecutive frames could be shown
    /// at the subscriber (see [`run_simulcast`]), and when it
    /// ended relative to the opening of the loss window.
    stall: f64,
    stall_ended: f64,
    /// Time the picture stood still in the two seconds from the opening of
    /// the window: over every gap longer than two frame intervals, the gap
    /// less one frame interval.
    frozen: f64,
    /// The longest any frame shown from the opening of the window on took
    /// from being written to being shown.
    late: f64,
    /// Frames that could be shown, of the frames written on any layer.
    shown: usize,
    /// When the publisher received the first keyframe request for the
    /// light layer from the opening of the loss window on, relative to
    /// that opening. Requests before it are the tracks' start.
    first_q_request: Option<f64>,
    /// Keyframe requests the publisher received from the opening of the
    /// loss window on, per layer.
    plis_q: usize,
    plis_f: usize,
    /// Media datagrams the publisher lost inside the window on its main
    /// streams and on its retransmission streams, of how many it sent.
    lost: usize,
    sent_in_window: usize,
}

/// The payload of packet `i` of frame `frame` on `layer`: enough to tell
/// at the subscriber which packet of which frame arrived.
fn fb_payload(layer: u8, frame: u64, i: u64, len: usize) -> Vec<u8> {
    let mut payload = vec![0x5a_u8; len];
    payload[0] = layer;
    payload[1..5].copy_from_slice(&u32::try_from(frame).expect("short stream").to_be_bytes());
    payload[5..7].copy_from_slice(&u16::try_from(i).expect("small frame").to_be_bytes());
    payload
}

/// The generic frame descriptor of packet `i` of `n` of a frame: a key
/// frame (no dependencies) or a frame depending on the one before it.
fn fb_descriptor(frame: u64, key: bool, i: u64, n: u64) -> Vec<u8> {
    let end = if i + 1 == n { 0x40 } else { 0x00 };
    if i > 0 {
        return vec![0x30 | end];
    }
    let [lo, hi] = ((frame & 0xffff) as u16).to_le_bytes();
    if key {
        vec![0xb0 | end, 0x01, lo, hi, 0x05, 0x00, 0x02, 0xd0]
    } else {
        vec![0xb8 | end, 0x01, lo, hi, 0x04]
    }
}

/// How the publisher of a two-track run sends its two pictures.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Senders {
    /// One media line, simulcast: `q` (light) and `f`.
    Simulcast,
    /// Two plain senders on two media lines, the second declared in the
    /// offer as the light copy. What the Lakko clients do.
    TwoDeclared,
    /// Two plain senders and an audio sender; the offer declares the audio
    /// line and a mid that does not exist as light, and neither video line.
    TwoMisdeclared,
    /// The publisher joins with audio alone and turns its camera on once
    /// connected: a later offer adds the two senders, the second declared
    /// light.
    TwoDeclaredMidCall,
}

/// How a two-track run ([`run_simulcast`]) is set up.
#[derive(Clone, Copy)]
struct SimulcastCase {
    /// How the publisher sends.
    senders: Senders,
    /// Run the SFU with the uplink fallback switched on. Off is the
    /// default of the service.
    fallback: bool,
    /// Loss on the publisher's datagrams for [`DIP_WINDOW`], opening
    /// [`DIP_WINDOW_OPENS`] after the first frame: the probability that one
    /// is lost, and the seed of the generator. `None` loses nothing.
    dip: Option<(f64, u64)>,
    /// The subscriber joins this long after the publisher's first frame,
    /// instead of before it.
    late_join: Option<Duration>,
    /// How long the publisher streams, and how long the clients keep
    /// running after its last frame.
    stream: Duration,
    settle: Duration,
    /// What the SFU does to carry video through a burst of loss.
    protection: LossProtection,
    /// The dip is on the subscriber's downlink (every media datagram it
    /// is sent) instead of the publisher's uplink.
    downlink_dip: bool,
    /// The subscriber's session knows the playout-delay header extension,
    /// as a libwebrtc receiver's does.
    playout_ext: bool,
    /// How often the subscriber asks again for a missing packet, on every
    /// stream; `None` is the vendored `str0m`'s default (60 ms).
    subscriber_nack: Option<Duration>,
    /// The publisher's cap on retransmitted bytes (`set_rtx_cache`): above
    /// this share of what it sends, queued retransmissions are thrown
    /// away. `None`: it answers every request, however many. `Some(0.15)`
    /// is upstream `str0m`'s default, used here as a stand-in for a sender
    /// that does not repair a whole burst (see
    /// `measure_burst_end_key_frame`).
    publisher_rtx_cap: Option<f32>,
}

/// What a subscriber received on one of its downstream tracks, as a
/// decoder in outline would see it (see [`run_simulcast`]).
struct TrackSeen {
    /// Media packets received on the track.
    packets: usize,
    /// The stretches of one layer in order of arrival: layer, when the
    /// stretch began, and the frame it began with.
    stretches: Vec<(u8, Instant, u64)>,
    /// When each packet first arrived: (layer, frame, packet) -> arrival.
    arrived: std::collections::BTreeMap<(u8, u64, u64), Instant>,
    /// Each frame that could be shown: (when, when it was written).
    shown: Vec<(Instant, Instant)>,
    /// The last frame shown is the last frame the publisher wrote on that
    /// layer: the chain was alive at the end of the run.
    alive: bool,
}

/// Work out what `got`, the packets of one downstream track in order of
/// arrival, let a decoder show.
///
/// A key frame can be shown once all its packets have arrived; a delta
/// frame once all its packets have arrived and the frame before it on the
/// same layer could be shown. That is the frame chain of
/// `measure_uplink_dip` (a frame is complete when it and every frame before
/// it are), with the one addition a stream that carries more than one key
/// frame needs: a key frame begins a new chain.
///
/// Asserted on the way: every payload is byte for byte what the publisher
/// wrote for that packet, and, with `key_frame_starts`, every stretch of a
/// layer begins with the first packet of a key frame. That holds for a
/// simulcast source, whose tracks the SFU starts and switches at a whole
/// key frame; a plain sender is forwarded from whichever packet arrives
/// first, like any single-encoding track, and its first frames are simply
/// not shown until its key frame arrives.
fn fb_track_seen(
    got: &[(&RtpGot, Instant)],
    layers: &[FbLayer; 2],
    key_frame_starts: bool,
) -> TrackSeen {
    use std::collections::BTreeMap;
    let of = |layer: u8| &layers[usize::from(layer == b'f')];
    let mut arrived: BTreeMap<(u8, u64, u64), Instant> = BTreeMap::new();
    let mut stretches: Vec<(u8, Instant, u64)> = Vec::new();
    for (got, at) in got {
        let pl = &got.payload;
        assert!(pl.len() >= 7, "a media packet of this test");
        let layer = pl[0];
        assert!(layer == b'q' || layer == b'f', "a layer of this test");
        let frame = u64::from(u32::from_be_bytes([pl[1], pl[2], pl[3], pl[4]]));
        let i = u64::from(u16::from_be_bytes([pl[5], pl[6]]));
        assert_eq!(
            *pl,
            fb_payload(layer, frame, i, of(layer).payload),
            "the payload arrives byte for byte as the publisher wrote it"
        );
        if stretches.last().map(|s| s.0) != Some(layer) {
            let info = of(layer).frames[usize::try_from(frame).expect("fits")];
            assert!(
                !key_frame_starts || (info.key && i == 0),
                "a stretch of layer {} begins with the first packet of a key frame \
                 (frame {frame}, packet {i}, key {})",
                char::from(layer),
                info.key
            );
            stretches.push((layer, *at, frame));
        }
        arrived.entry((layer, frame, i)).or_insert(*at);
    }

    // When each frame could be shown, stretch by stretch.
    let mut shown: Vec<(Instant, Instant)> = Vec::new();
    let mut last_shown: Option<(u8, u64)> = None;
    for (s, (layer, _, first_frame)) in stretches.iter().enumerate() {
        let frames = &of(*layer).frames;
        // A stretch ends where the next stretch of the same layer begins.
        let end = stretches[s + 1..]
            .iter()
            .find(|next| next.0 == *layer)
            .map_or(frames.len() as u64, |next| next.2);
        let mut chain: Option<Instant> = None;
        for frame in *first_frame..end {
            let info = frames[usize::try_from(frame).expect("fits")];
            let mut done: Option<Instant> = None;
            let mut whole = true;
            for i in 0..info.n {
                match arrived.get(&(*layer, frame, i)) {
                    Some(at) => done = Some(done.map_or(*at, |d: Instant| d.max(*at))),
                    None => whole = false,
                }
            }
            let Some(done) = done.filter(|_| whole) else {
                // Nothing after it can be shown until a key frame.
                chain = None;
                continue;
            };
            let at = if info.key {
                // A key frame that was complete before later frames of the
                // chain before it: those came too late to be shown.
                while shown.last().is_some_and(|last| last.0 > done) {
                    shown.pop();
                }
                done
            } else {
                let Some(before) = chain else {
                    continue;
                };
                before.max(done)
            };
            chain = Some(at);
            shown.push((at, info.at));
            last_shown = Some((*layer, frame));
        }
    }
    let alive = last_shown.is_some_and(|(layer, frame)| frame + 1 == of(layer).frames.len() as u64);
    TrackSeen {
        packets: got.len(),
        stretches,
        arrived,
        shown,
        alive,
    }
}

/// The longest interval in which nothing in `shown` (moments a frame could
/// be shown, ascending) happened, in milliseconds, and when it ended. When
/// the chain was not `alive` at the end of the run the time from the last
/// moment to `end` counts as well.
fn fb_longest_gap(shown: &[Instant], alive: bool, end: Instant) -> (f64, Option<Instant>) {
    let mut longest = (0.0_f64, None);
    let tail = (!alive).then_some(end);
    let mut before: Option<Instant> = None;
    for at in shown.iter().copied().chain(tail) {
        if let Some(b) = before {
            let gap = dip_ms(at.saturating_duration_since(b));
            if gap > longest.0 {
                longest = (gap, Some(at));
            }
        }
        before = Some(at);
    }
    longest
}

/// One packet a subscriber was handed: its track, when, and the playout
/// delay it carried as (minimum, maximum) in milliseconds.
type PlayoutSeen = (Option<Mid>, Instant, Option<(u64, u64)>);

/// Everything one simulcast run observed.
struct SimulcastRun {
    /// The publisher's SFU-side peer id: the MSID stream id of the main
    /// track, and with `~light` appended that of the light companion.
    publisher: PeerId,
    /// Every offer the SFU sent the subscriber, in order.
    offers: Vec<String>,
    /// The subscriber's own mid of each downstream track, from the offers.
    main_mid: Option<Mid>,
    light_mid: Option<Mid>,
    main: TrackSeen,
    light: TrackSeen,
    /// Every other video track of the publisher the SFU offered the
    /// subscriber: its mid, its MSID stream id and what arrived on it.
    others: Vec<(Mid, String, TrackSeen)>,
    /// The MSID stream ids of the audio media lines the SFU offered.
    audio_streams: Vec<String>,
    /// The publisher's own mids of its light (`q`) and main (`f`) sender;
    /// one mid twice for a simulcast publisher.
    pub_light_mid: Mid,
    pub_main_mid: Mid,
    /// Packets that arrived on none of those tracks.
    stray: usize,
    /// When the publisher wrote its first frame, when the subscriber
    /// joined, and when the run stopped.
    started: Instant,
    joined: Instant,
    ended: Instant,
    /// The loss window.
    window: (Instant, Instant),
    /// Keyframe requests the publisher received: the publisher's media
    /// line and the layer named, and when.
    keyframe_requests: Vec<(Mid, Option<Rid>, Instant)>,
    /// Media datagrams the publisher lost inside the window, of how many
    /// it sent there.
    lost: usize,
    sent_in_window: usize,
    frame_every: Duration,
    /// Datagrams that reached the subscriber on the light companion's
    /// main stream and on its retransmission stream.
    light_in: (usize, usize),
    /// The same for the main track.
    main_in: (usize, usize),
    /// Every packet the subscriber was handed: its track, when, and the
    /// playout delay it carried as (minimum, maximum) in milliseconds.
    playout: Vec<PlayoutSeen>,
    /// The one-byte header extension ids seen on the media datagrams that
    /// reached the subscriber.
    in_ext_ids: Vec<u8>,
    /// The NACKs the publisher received: when, media SSRC, sequence
    /// number.
    nacks: Vec<(Instant, u32, u16)>,
    /// The SSRC the publisher sends its light picture on.
    pub_light_ssrc: u32,
    /// When the publisher wrote each frame of its light (`q`) and of its
    /// main (`f`) picture.
    written_q: Vec<Instant>,
    written_f: Vec<Instant>,
    /// The key frames the publisher wrote on its light and on its main
    /// picture: (frame number, packets).
    keys_q: Vec<(u64, u64)>,
    keys_f: Vec<(u64, u64)>,
    /// When each retransmission the publisher sent reached the wire.
    rtx_out: Vec<Instant>,
}

/// The MSID stream ids of the audio media lines of an SDP the SFU sent.
fn audio_msids(sdp: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_audio = false;
    for line in sdp.lines() {
        if line.starts_with("m=") {
            in_audio = line.starts_with("m=audio");
        }
        if in_audio {
            if let Some(stream) = line
                .strip_prefix("a=msid:")
                .and_then(|rest| rest.split_whitespace().next())
            {
                out.push(stream.to_owned());
            }
        }
    }
    out
}

/// The video media lines of an SDP the SFU sent, as (mid, MSID stream id).
fn video_msids(sdp: &str) -> Vec<(Mid, String)> {
    video_sections(sdp)
        .iter()
        .filter_map(|section| {
            let mid = section.lines().find_map(|l| l.strip_prefix("a=mid:"))?;
            let stream = section
                .lines()
                .find_map(|l| l.strip_prefix("a=msid:"))?
                .split_whitespace()
                .next()?;
            Some((Mid::from(mid.trim()), stream.to_owned()))
        })
        .collect()
}

/// Publisher A streams two pictures (`q` at 50 packets a second, `f` at
/// 250) to subscriber B through the production media task: as two
/// simulcast layers of one media line, or as two plain senders on two media
/// lines ([`Senders`]). With a
/// dip, for one second each media datagram A sends, on either layer and on
/// the retransmission streams, is lost with probability `p`. A answers a
/// keyframe request for a layer with a key frame on that layer, as an
/// encoder does.
///
/// B is offered two downstream tracks for A's camera: the main track and
/// the light companion. What arrived on each is worked out separately
/// ([`fb_track_seen`]): B is a decoder in outline, one per track.
#[allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss
)]
async fn run_simulcast(case: SimulcastCase) -> SimulcastRun {
    use lakko_svc_sfu::frame_descriptor::PacketDescriptor;
    use lakko_svc_sfu::media::LIGHT_STREAM_SUFFIX;

    let call = "simulcast-two-track-call";
    let peers = PeerRegistry::new();
    let mut registered = register_subscribers(&peers, call, 1).await;
    let (b_id, mut b_rx) = registered.pop().expect("one registered");
    let (media_task, sfu_addr) = spawn_media_task_with_options(
        "127.0.0.1:0".parse().unwrap(),
        None,
        peers,
        MediaTaskOptions {
            diagnostics: None,
            video_fec: false,
            uplink_fallback: case.fallback,
            protection: case.protection,
            downlink_control: false,
        },
    )
    .await
    .expect("UDP bind on loopback:0 succeeds");

    let mut a = SyntheticClient::build(
        Rtc::builder()
            .set_rtp_mode(true)
            .enable_raw_packets(true)
            .set_extension(
                GENERIC_FRAME_DESCRIPTOR_ID,
                generic_frame_descriptor_extension(),
            ),
    )
    .await;
    let mut b_config = Rtc::builder().set_rtp_mode(true).set_extension(
        GENERIC_FRAME_DESCRIPTOR_ID,
        generic_frame_descriptor_extension(),
    );
    if case.playout_ext {
        b_config = b_config.set_extension(
            lakko_svc_sfu::media::PLAYOUT_DELAY_EXTENSION_ID,
            str0m::rtp::Extension::PlayoutDelay,
        );
    }
    let mut b = SyntheticClient::build(b_config).await;
    // Incoming streams of B put on the case's NACK schedule so far.
    let mut b_scheduled: Vec<u32> = Vec::new();
    let mut b_in_read = 0usize;

    let mut joined: Option<Instant> = None;
    if case.late_join.is_none() {
        let (b_offer, b_pending) = b.create_video_subscriber_offer();
        let b_answer = media_task
            .accept_offer(b_id.clone(), call.to_owned(), b_offer)
            .await
            .expect("SFU accepts B's subscriber offer");
        b.accept_answer(b_pending, &b_answer)
            .expect("B applies the SFU's answer");
        joined = Some(Instant::now());
    }
    let a_id = fresh_peer_id();
    // The publisher's mids of its light and main picture, once offered.
    let mut pub_mids: Option<(Mid, Mid)> = None;
    {
        let (a_offer, a_pending, light_mids) = match case.senders {
            Senders::Simulcast => {
                // The light layer is listed first, as the web client lists it.
                let (offer, pending, mid) = a.create_video_simulcast_publisher_offer(&["q", "f"]);
                pub_mids = Some((mid, mid));
                (offer, pending, Vec::new())
            },
            Senders::TwoDeclared => {
                let (offer, pending, _, main, light) = a.create_two_sender_publisher_offer(false);
                pub_mids = Some((light, main));
                (offer, pending, vec![light.to_string()])
            },
            Senders::TwoMisdeclared => {
                let (offer, pending, audio, main, light) =
                    a.create_two_sender_publisher_offer(true);
                pub_mids = Some((light, main));
                let audio = audio.expect("an audio sender");
                (offer, pending, vec![audio.to_string(), "zz".to_owned()])
            },
            Senders::TwoDeclaredMidCall => {
                let (offer, pending, _) = a.create_audio_publisher_offer();
                (offer, pending, Vec::new())
            },
        };
        let a_answer = media_task
            .accept_offer_with_light(a_id.clone(), call.to_owned(), a_offer, light_mids)
            .await
            .expect("SFU accepts A's offer");
        a.accept_answer(a_pending, &a_answer)
            .expect("A applies the SFU's answer");
    }

    let simulcast = case.senders == Senders::Simulcast;
    let rid_q = simulcast.then(|| Rid::from("q"));
    let rid_f = simulcast.then(|| Rid::from("f"));
    let frame_every = Duration::from_micros(1_000_000 / DIP_FPS);
    let total_frames = case.stream.as_micros() as u64 / frame_every.as_micros() as u64;

    let mut layers = [
        FbLayer {
            rid: rid_q,
            mid: pub_mids.map(|m| m.0),
            tag: b'q',
            packets: FB_Q_PACKETS,
            payload: FB_Q_PAYLOAD,
            next_seq: FB_Q_FIRST_SEQ,
            frames: Vec::new(),
            want_key: true,
        },
        FbLayer {
            rid: rid_f,
            mid: pub_mids.map(|m| m.1),
            tag: b'f',
            packets: FB_F_PACKETS,
            payload: FB_F_PAYLOAD,
            next_seq: FB_F_FIRST_SEQ,
            frames: Vec::new(),
            want_key: true,
        },
    ];

    let give_up = Instant::now() + case.stream + case.settle + Duration::from_secs(45);
    let mut offers: Vec<String> = Vec::new();
    let mut t0: Option<Instant> = None;
    let mut window: Option<(Instant, Instant)> = None;
    let mut main_ssrcs: Vec<u32> = Vec::new();
    let mut ticks: u64 = 0;
    let mut plis_handled = 0usize;
    let mut finish_at: Option<Instant> = None;
    let mut abuf = [0u8; 2048];
    let mut bbuf = [0u8; 2048];

    let ended = loop {
        let now = Instant::now();
        assert!(
            now < give_up,
            "the simulcast run did not finish: ticks={ticks} a_connected={} b_connected={} \
             offers={}",
            a.is_connected(),
            b.is_connected(),
            offers.len()
        );
        if finish_at.is_some_and(|at| now >= at) {
            break now;
        }
        let _ = a.drain().await;
        let _ = b.drain().await;
        // A keyframe request for a layer makes the next frame on it a key
        // frame.
        while plis_handled < a.keyframe_request_rids.len() {
            let rid = a.keyframe_request_rids[plis_handled];
            let on = a.keyframe_request_mids[plis_handled];
            plis_handled += 1;
            for layer in &mut layers {
                if layer.mid == Some(on) && (rid.is_none() || rid == layer.rid) {
                    layer.want_key = true;
                }
            }
        }
        // The camera turned on mid-call: once connected, a second offer
        // adds the two senders and declares the light one.
        if pub_mids.is_none() && a.is_connected() {
            let (offer, pending, _, main, light) = a.create_two_sender_publisher_offer(false);
            let answer = media_task
                .accept_offer_with_light(
                    a_id.clone(),
                    call.to_owned(),
                    offer,
                    vec![light.to_string()],
                )
                .await
                .expect("SFU accepts A's camera offer");
            a.accept_answer(pending, &answer)
                .expect("A applies the SFU's answer");
            pub_mids = Some((light, main));
            layers[0].mid = Some(light);
            layers[1].mid = Some(main);
        }
        // A late subscriber joins once the publisher has streamed a while.
        if joined.is_none() {
            let due = t0
                .zip(case.late_join)
                .is_some_and(|(start, after)| now >= start + after);
            if due {
                let (b_offer, b_pending) = b.create_video_subscriber_offer();
                let b_answer = media_task
                    .accept_offer(b_id.clone(), call.to_owned(), b_offer)
                    .await
                    .expect("SFU accepts B's subscriber offer");
                b.accept_answer(b_pending, &b_answer)
                    .expect("B applies the SFU's answer");
                joined = Some(Instant::now());
            }
        }
        if b.is_connected() {
            while let Ok(frame) = b_rx.try_recv() {
                if let ServerFrame::SdpOffer { sdp } = frame {
                    let answer = b.answer_renegotiation(&sdp).expect("B answers");
                    media_task
                        .accept_renegotiation_answer(b_id.clone(), answer)
                        .await
                        .expect("the SFU takes B's answer");
                    offers.push(sdp);
                }
            }
        }
        // B has answered an offer that carries video: its tracks are open.
        let b_has_video = offers.iter().any(|sdp| {
            video_msids(sdp)
                .iter()
                .any(|(_, stream)| stream.starts_with(&a_id.to_string()))
        });
        let b_ready = case.late_join.is_some() || (b.is_connected() && b_has_video);
        if t0.is_none() && a.is_connected() && b_ready && pub_mids.is_some() {
            let mut api = a.rtc.direct_api();
            let mut ready = true;
            for layer in &layers {
                let stream = layer
                    .mid
                    .and_then(|mid| api.stream_tx_by_mid(mid, layer.rid));
                match stream {
                    Some(stream) if stream.rtx().is_some() => {
                        // No cap on retransmitted bytes, as a browser has
                        // none.
                        stream.set_rtx_cache(4000, Duration::from_secs(3), case.publisher_rtx_cap);
                        main_ssrcs.push(*stream.ssrc());
                    },
                    _ => ready = false,
                }
            }
            if ready {
                let start = Instant::now() + Duration::from_millis(50);
                let from = start + DIP_WINDOW_OPENS;
                let until = from + DIP_WINDOW;
                let (p, seed) = case.dip.unwrap_or((0.0, 0));
                let dip = DipLoss {
                    from,
                    until,
                    p,
                    rng: seed,
                };
                if case.downlink_dip {
                    b.dip_in = Some(dip);
                } else {
                    a.dip = Some(dip);
                }
                t0 = Some(start);
                window = Some((from, until));
            } else {
                main_ssrcs.clear();
            }
        }
        if let Some(start) = t0 {
            let due = start + frame_every * (ticks as u32);
            if ticks < total_frames && Instant::now() >= due {
                // The full layer is written first, and the light one starts
                // a frame later.
                for idx in [1usize, 0] {
                    if idx == 0 && ticks == 0 {
                        continue;
                    }
                    let layer = &mut layers[idx];
                    let frame = layer.frames.len() as u64;
                    let key = std::mem::take(&mut layer.want_key);
                    let n = if key {
                        layer.packets * FB_KEY_FACTOR
                    } else {
                        layer.packets
                    };
                    let rtp_time = 90_000 + (ticks * (90_000 / DIP_FPS)) as u32;
                    let written = Instant::now();
                    for i in 0..n {
                        let mut ext = str0m::rtp::ExtensionValues::default();
                        ext.user_values
                            .set(PacketDescriptor(fb_descriptor(frame, key, i, n)));
                        let seq = layer.next_seq;
                        layer.next_seq += 1;
                        let mut api = a.rtc.direct_api();
                        let stream = api
                            .stream_tx_by_mid(layer.mid.expect("offered"), layer.rid)
                            .expect("send stream");
                        stream
                            .write_rtp(
                                UPLINK_VP8_PT.into(),
                                seq.into(),
                                rtp_time,
                                written,
                                i + 1 == n,
                                ext,
                                true,
                                fb_payload(layer.tag, frame, i, layer.payload),
                            )
                            .expect("A writes a packet");
                    }
                    layer.frames.push(FbFrame {
                        key,
                        n,
                        at: written,
                    });
                }
                ticks += 1;
                if ticks == total_frames {
                    finish_at = Some(Instant::now() + case.settle);
                }
                a.feed_timeout();
                let _ = a.drain().await;
            }
        }
        while let Ok((len, src)) = a.socket.try_recv_from(&mut abuf) {
            a.feed_receive(&abuf[..len], src, sfu_addr);
            let _ = a.drain().await;
        }
        while let Ok((len, src)) = b.socket.try_recv_from(&mut bbuf) {
            b.feed_receive(&bbuf[..len], src, sfu_addr);
            let _ = b.drain().await;
        }
        let a_sock = a.socket.clone();
        let b_sock = b.socket.clone();
        tokio::select! {
            r = a_sock.recv_from(&mut abuf) => {
                if let Ok((len, src)) = r {
                    a.feed_receive(&abuf[..len], src, sfu_addr);
                }
            }
            r = b_sock.recv_from(&mut bbuf) => {
                if let Ok((len, src)) = r {
                    b.feed_receive(&bbuf[..len], src, sfu_addr);
                }
            }
            () = tokio::time::sleep(Duration::from_millis(1)) => {}
        }
        let _ = a.drain().await;
        let _ = b.drain().await;
        if let Some(retry) = case.subscriber_nack {
            // Every stream B receives is asked on the case's schedule, from
            // its first packet on.
            let fresh: Vec<u32> = b.rtp_in[b_in_read..].iter().map(|p| p.ssrc).collect();
            b_in_read = b.rtp_in.len();
            for ssrc in fresh {
                if b_scheduled.contains(&ssrc) {
                    continue;
                }
                if let Some(stream) = b.rtc.direct_api().stream_rx(&ssrc.into()) {
                    stream.set_nack_retry_interval(Some(retry));
                    b_scheduled.push(ssrc);
                }
            }
        }
        a.feed_timeout();
        b.feed_timeout();
    };

    let (from, until) = window.expect("the window was set");

    // Which of B's media lines is which track: by the MSID stream id of
    // the contract.
    let light_stream = format!("{a_id}{LIGHT_STREAM_SUFFIX}");
    let mut main_mid = None;
    let mut light_mid = None;
    let mut video_tracks: Vec<(Mid, String)> = Vec::new();
    for (mid, stream) in offers.iter().flat_map(|sdp| video_msids(sdp)) {
        if stream == a_id.to_string() {
            main_mid = Some(mid);
        } else if stream == light_stream {
            light_mid = Some(mid);
        }
        // The publisher's tracks only: B's own media line is in the SFU's
        // offers as well.
        if stream.starts_with(&a_id.to_string()) && !video_tracks.iter().any(|(m, _)| *m == mid) {
            video_tracks.push((mid, stream));
        }
    }
    let audio_streams: Vec<String> = offers.iter().flat_map(|sdp| audio_msids(sdp)).collect();

    assert_eq!(b.rtp_got.len(), b.rtp_got_at.len());
    assert_eq!(b.rtp_got.len(), b.rtp_got_track.len());
    let on = |track: Option<Mid>| -> Vec<(&RtpGot, Instant)> {
        b.rtp_got
            .iter()
            .zip(&b.rtp_got_at)
            .zip(&b.rtp_got_track)
            .filter(|(_, t)| track.is_some() && **t == track)
            .map(|((got, at), _)| (got, *at))
            .collect()
    };
    let main = fb_track_seen(&on(main_mid), &layers, simulcast);
    let light = fb_track_seen(&on(light_mid), &layers, simulcast);
    let others: Vec<(Mid, String, TrackSeen)> = video_tracks
        .into_iter()
        .filter(|(mid, _)| Some(*mid) != main_mid && Some(*mid) != light_mid)
        .map(|(mid, stream)| {
            let seen = fb_track_seen(&on(Some(mid)), &layers, simulcast);
            (mid, stream, seen)
        })
        .collect();
    let stray = b.rtp_got.len()
        - main.packets
        - light.packets
        - others
            .iter()
            .map(|(_, _, seen)| seen.packets)
            .sum::<usize>();
    let (pub_light_mid, pub_main_mid) = pub_mids.expect("the publisher offered its video");

    let in_win = |at: Instant| at >= from && at < until;
    let (lost, sent_in_window) = if case.downlink_dip {
        let lost = b.dip_in_lost.len();
        (
            lost,
            lost + b.rtp_in.iter().filter(|p| in_win(p.at)).count(),
        )
    } else {
        let lost = a.rtp_dropped.iter().filter(|p| in_win(p.at)).count();
        (
            lost,
            lost + a.rtp_out.iter().filter(|p| in_win(p.at)).count(),
        )
    };
    assert!(
        a.rtp_dropped.iter().all(|p| in_win(p.at)) && b.dip_in_lost.iter().all(|p| in_win(p.at)),
        "nothing is lost outside the window"
    );
    assert_eq!(main_ssrcs.len(), 2);
    // Datagrams that reached B on the light companion's main stream and on
    // its retransmission stream.
    let mut datagrams_in = |track: Option<Mid>| -> (usize, usize) {
        track
            .and_then(|mid| {
                let mut api = b.rtc.direct_api();
                let stream = api.stream_rx_by_mid(mid, None)?;
                Some((*stream.ssrc(), stream.rtx().map(|rtx| *rtx)))
            })
            .map_or((0, 0), |(ssrc, rtx)| {
                let count = |of: u32| b.rtp_in.iter().filter(|p| p.ssrc == of).count();
                (count(ssrc), rtx.map_or(0, count))
            })
    };
    let light_in = datagrams_in(light_mid);
    let main_in = datagrams_in(main_mid);
    let playout: Vec<PlayoutSeen> = b
        .rtp_got_track
        .iter()
        .zip(&b.rtp_got_at)
        .zip(&b.rtp_got_playout)
        .map(|((track, at), delay)| (*track, *at, *delay))
        .collect();

    SimulcastRun {
        publisher: a_id,
        offers,
        main_mid,
        light_mid,
        main,
        light,
        others,
        audio_streams,
        pub_light_mid,
        pub_main_mid,
        stray,
        started: t0.expect("the publisher streamed"),
        joined: joined.expect("the subscriber joined"),
        ended,
        window: (from, until),
        keyframe_requests: a
            .keyframe_request_mids
            .iter()
            .copied()
            .zip(a.keyframe_request_rids.iter().copied())
            .zip(a.keyframe_request_at.iter().copied())
            .map(|((mid, rid), at)| (mid, rid, at))
            .collect(),
        lost,
        sent_in_window,
        frame_every,
        light_in,
        main_in,
        playout,
        in_ext_ids: b.in_ext_ids.iter().copied().collect(),
        nacks: a.nacks_seen.clone(),
        pub_light_ssrc: main_ssrcs[0],
        written_q: layers[0].frames.iter().map(|f| f.at).collect(),
        written_f: layers[1].frames.iter().map(|f| f.at).collect(),
        keys_q: fb_key_frames(&layers[0]),
        keys_f: fb_key_frames(&layers[1]),
        rtx_out: a
            .rtp_out
            .iter()
            .filter(|p| !main_ssrcs.contains(&p.ssrc))
            .map(|p| p.at)
            .collect(),
    }
}

/// The key frames of one layer: (frame number, packets).
fn fb_key_frames(layer: &FbLayer) -> Vec<(u64, u64)> {
    layer
        .frames
        .iter()
        .enumerate()
        .filter(|(_, frame)| frame.key)
        .map(|(i, frame)| (i as u64, frame.n))
        .collect()
}

/// One uplink-fallback run: [`run_simulcast`] with a dip, reduced to what
/// the subscriber's **main** track went through. With `fallback` off the
/// SFU runs as it does by default and the main track stays on `f`.
///
/// **What is measured.** A layer switch starts at a key frame (the SFU
/// guarantees it), and frames of the layer left behind that were still
/// incomplete are never shown. The stall is the longest gap between two
/// consecutive moments a frame could be shown.
#[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
async fn run_uplink_fallback_dip(fallback: bool, p: f64, seed: u64) -> FallbackRun {
    let run = Box::pin(run_simulcast(SimulcastCase {
        senders: Senders::Simulcast,
        fallback,
        dip: Some((p, seed)),
        late_join: None,
        stream: FB_STREAM,
        settle: FB_SETTLE,
        protection: LossProtection::default(),
        downlink_dip: false,
        playout_ext: false,
        subscriber_nack: None,
        publisher_rtx_cap: None,
    }))
    .await;
    let (from, _) = run.window;
    let signed_ms = |at: Instant| {
        if at >= from {
            dip_ms(at.duration_since(from))
        } else {
            -dip_ms(from.duration_since(at))
        }
    };
    let main = &run.main;
    let shown = &main.shown;
    if std::env::var_os("LAKKO_FB_DETAIL").is_some() {
        // Per stretch: when each packet of its first (key) frame arrived.
        for (layer, at, frame) in &main.stretches {
            let packets: Vec<String> = main
                .arrived
                .range((*layer, *frame, 0)..(*layer, *frame + 1, 0))
                .map(|(_, t)| format!("{:.0}", signed_ms(*t)))
                .collect();
            #[allow(clippy::print_stderr)]
            {
                eprintln!(
                    "  stretch {} from {:.0} ms: key frame {frame}, packets arrived at [{}]",
                    char::from(*layer),
                    signed_ms(*at),
                    packets.join(",")
                );
            }
        }
        let plis: Vec<String> = run
            .keyframe_requests
            .iter()
            .map(|(_, rid, at)| format!("{rid:?}@{:.0}", signed_ms(*at)))
            .collect();
        #[allow(clippy::print_stderr)]
        {
            eprintln!("  keyframe requests at the publisher [{}]", plis.join(","));
        }
        let around: Vec<String> = shown
            .iter()
            .map(|t| signed_ms(t.0))
            .filter(|t| (-100.0..1800.0).contains(t))
            .map(|t| format!("{t:.0}"))
            .collect();
        #[allow(clippy::print_stderr)]
        {
            eprintln!("  frames shown at [{}]", around.join(","));
        }
    }
    let (mut stall, mut stall_ended) = (0.0_f64, 0.0_f64);
    let mut frozen = 0.0_f64;
    let two_frames = 2.0 * dip_ms(run.frame_every);
    for pair in shown.windows(2) {
        let gap = dip_ms(pair[1].0.saturating_duration_since(pair[0].0));
        if gap > stall {
            stall = gap;
            stall_ended = signed_ms(pair[1].0);
        }
        if pair[1].0 >= from && pair[1].0 < from + Duration::from_secs(2) && gap > two_frames {
            frozen += gap - dip_ms(run.frame_every);
        }
    }
    let late = shown
        .iter()
        .filter(|(at, _)| *at >= from)
        .map(|(at, written)| dip_ms(at.saturating_duration_since(*written)))
        .fold(0.0_f64, f64::max);
    // The requests that start the two tracks come before the window.
    let in_or_after = |rid: Rid| {
        run.keyframe_requests
            .iter()
            .filter(move |(_, r, at)| *r == Some(rid) && *at >= from)
    };
    let (rid_q, rid_f) = (Rid::from("q"), Rid::from("f"));

    FallbackRun {
        stretches: main
            .stretches
            .iter()
            .map(|(layer, at, _)| (char::from(*layer), signed_ms(*at)))
            .collect(),
        stall,
        stall_ended,
        frozen,
        late,
        shown: shown.len(),
        first_q_request: in_or_after(rid_q).next().map(|(_, _, at)| signed_ms(*at)),
        plis_q: in_or_after(rid_q).count(),
        plis_f: in_or_after(rid_f).count(),
        lost: run.lost,
        sent_in_window: run.sent_in_window,
    }
}

#[allow(clippy::print_stderr)]
fn print_fallback_run(what: &str, run: &FallbackRun) {
    let stretches = run
        .stretches
        .iter()
        .map(|(layer, at)| format!("{layer}@{at:.0}"))
        .collect::<Vec<_>>()
        .join(" ");
    eprintln!(
        "uplink fallback dip [{what}]: stall {:.0} ms (ended {:.0} ms after the window opened), \
         frozen {:.0} ms of the two seconds from the opening, latest frame {:.0} ms late, \
         {} frames shown, layers at the subscriber: {stretches}, keyframe requests q/f {}/{}, \
         lost {} of {} datagrams in the window",
        run.stall,
        run.stall_ended,
        run.frozen,
        run.late,
        run.shown,
        run.plis_q,
        run.plis_f,
        run.lost,
        run.sent_in_window
    );
}

/// The uplink fallback over the live SFU: a simulcast publisher's uplink
/// loses 70% of its datagrams for one second. With the fallback switched on
/// (it is off by default) the subscriber's main track is selected to `q`
/// during the dip, switched to it at a key frame, and switched back to `f`
/// after the dip; the same loss is then run with the fallback off, and what
/// the subscriber could show on its main track is printed for both.
///
/// The two runs share a seed, not a loss pattern: the generator is drawn
/// once per datagram, and the two runs do not send the same datagrams.
///
/// **What is asserted and what is not.** That the subscriber is moved and
/// moved back, when, and that the picture never stands still for longer
/// than the dip and the wait before it. Not that the fallback shortens the
/// stall: over the runs made while this was written it did not, reliably
/// (see `services/sfu/README.md`). The numbers are printed so the next
/// change to the policy can be judged against them.
///
/// `LAKKO_DIP_SEED` (a number, default 1) changes the loss pattern;
/// `LAKKO_FB_DETAIL` prints, per run, when each packet of the key frames
/// arrived and when frames could be shown.
#[tokio::test]
async fn the_uplink_fallback_carries_a_subscriber_through_a_dip() {
    let seed: u64 = std::env::var("LAKKO_DIP_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let with = Box::pin(run_uplink_fallback_dip(true, 0.70, seed)).await;
    print_fallback_run("fallback on", &with);
    let without = Box::pin(run_uplink_fallback_dip(false, 0.70, seed)).await;
    print_fallback_run("fallback off", &without);

    // Without the fallback the subscriber's main track never leaves the
    // full layer, and once its two tracks have started the publisher is
    // asked for nothing.
    let layers_off: Vec<char> = without.stretches.iter().map(|s| s.0).collect();
    assert_eq!(layers_off, vec!['f'], "{:?}", without.stretches);
    assert_eq!((without.plis_q, without.plis_f), (0, 0));

    // With it: on `f`, to `q`, back to `f`.
    let layers_on: Vec<char> = with.stretches.iter().map(|s| s.0).collect();
    assert_eq!(layers_on, vec!['f', 'q', 'f'], "{:?}", with.stretches);
    let window_ms = dip_ms(DIP_WINDOW);
    let hole_age_ms = dip_ms(lakko_svc_sfu::uplink_fallback::HOLE_AGE);
    let restore_ms = dip_ms(lakko_svc_sfu::uplink_fallback::RESTORE_AFTER);
    // The light layer is selected, and its key frame asked for, inside the
    // dip: no sooner than a hole can be old, and well before the dip ends.
    let asked = with
        .first_q_request
        .expect("a key frame on q was asked for");
    assert!(
        asked >= hole_age_ms && asked < window_ms,
        "the light layer is selected during the dip: asked at {asked:.0} ms"
    );
    // The switch follows the request, and the switch back comes no sooner
    // than the uplink has been clean for the restore wait.
    let to_q = with.stretches[1].1;
    let to_f = with.stretches[2].1;
    assert!(to_q > asked, "{to_q:.0} ms after {asked:.0} ms");
    assert!(
        to_f >= asked + restore_ms,
        "back on the full layer at {to_f:.0} ms, asked at {asked:.0} ms"
    );
    assert!(with.plis_q >= 1 && with.plis_f >= 1);
    // The picture does not stand still for much longer than the dip.
    assert!(
        with.stall < 1.5 * window_ms,
        "stall with the fallback: {:.0} ms",
        with.stall
    );
}

// ============================================================
// Two-track delivery: a subscriber of a simulcast publisher gets the
// main track and the light companion, both all the time.
// ============================================================

/// What the two-track tests assert of any run: the offer names the two
/// stream ids of the contract, the heavy layer and nothing else arrives on
/// the main track, the light layer and nothing else on the companion, and
/// each begins with a key frame ([`fb_track_seen`] has already checked
/// that, and that every payload is verbatim).
fn assert_two_tracks(run: &SimulcastRun) {
    use lakko_svc_sfu::media::LIGHT_STREAM_SUFFIX;
    assert_eq!(LIGHT_STREAM_SUFFIX, "~light");
    let offer = run.offers.last().expect("the SFU offered B its tracks");
    let streams: Vec<String> = video_msids(offer).into_iter().map(|(_, s)| s).collect();
    let main_stream = run.publisher.to_string();
    let light_stream = format!("{main_stream}~light");
    assert!(
        streams.contains(&main_stream) && streams.contains(&light_stream),
        "the offer carries both stream ids of the contract: {streams:?}"
    );
    assert_eq!(
        streams
            .iter()
            .filter(|s| s.starts_with(&main_stream))
            .count(),
        2,
        "two video tracks for the one published: {streams:?}"
    );
    let (main_mid, light_mid) = (
        run.main_mid.expect("a main track"),
        run.light_mid.expect("a light companion"),
    );
    assert_ne!(main_mid, light_mid);
    let layers_of = |seen: &TrackSeen| -> Vec<char> {
        seen.stretches.iter().map(|s| char::from(s.0)).collect()
    };
    assert_eq!(layers_of(&run.main), vec!['f'], "the main track carries f");
    assert_eq!(layers_of(&run.light), vec!['q'], "the companion carries q");
    assert!(run.main.packets > 0 && run.light.packets > 0);
    assert_eq!(run.stray, 0, "nothing arrives on any other media line");
    assert!(
        run.main.alive && run.light.alive,
        "both tracks carry the stream to its end (main {}, light {})",
        run.main.alive,
        run.light.alive
    );
}

/// A simulcast publisher (`q`, `f`) and a subscriber over the live SFU: the
/// subscriber is offered two video tracks whose MSID stream ids are the
/// publisher's peer id and that id followed by `~light`; packets of `f`
/// arrive on the first, packets of `q` on the second, byte for byte.
#[tokio::test]
async fn a_simulcast_publisher_reaches_a_subscriber_as_two_tracks() {
    let run = Box::pin(run_simulcast(SimulcastCase {
        senders: Senders::Simulcast,
        fallback: false,
        dip: None,
        late_join: None,
        stream: Duration::from_secs(3),
        settle: Duration::from_millis(700),
        protection: LossProtection::default(),
        downlink_dip: false,
        playout_ext: false,
        subscriber_nack: None,
        publisher_rtx_cap: None,
    }))
    .await;
    assert_two_tracks(&run);
    // One offer opened both tracks.
    assert_eq!(run.offers.len(), 1, "one renegotiation for the two tracks");
    #[allow(clippy::print_stderr)]
    {
        let msid: Vec<&str> = run.offers[0]
            .lines()
            .filter(|l| l.starts_with("a=msid:") || l.starts_with("a=mid:"))
            .collect();
        eprintln!("the SFU's offer to the subscriber, mid and msid lines: {msid:?}");
    }
    // Every frame the publisher wrote on each layer could be shown on its
    // track, from the track's first key frame on.
    let frames = |seen: &TrackSeen| seen.shown.len();
    assert!(
        frames(&run.main) >= 70 && frames(&run.light) >= 70,
        "about 75 frames a layer in three seconds: main {}, light {}",
        frames(&run.main),
        frames(&run.light)
    );
}

/// A subscriber that joins a simulcast publisher already streaming gets
/// both tracks, each starting at a key frame of its own layer that the SFU
/// asked the publisher for.
#[tokio::test]
async fn a_late_joiner_gets_both_tracks_each_from_a_key_frame() {
    let join_after = Duration::from_millis(1500);
    let run = Box::pin(run_simulcast(SimulcastCase {
        senders: Senders::Simulcast,
        fallback: false,
        dip: None,
        late_join: Some(join_after),
        stream: Duration::from_millis(4500),
        settle: Duration::from_millis(700),
        protection: LossProtection::default(),
        downlink_dip: false,
        playout_ext: false,
        subscriber_nack: None,
        publisher_rtx_cap: None,
    }))
    .await;
    assert_two_tracks(&run);
    assert!(run.joined >= run.started + join_after);
    // The publisher was asked for a key frame on each layer after the
    // subscriber joined, and each track begins with one written after it.
    for (rid, seen) in [("q", &run.light), ("f", &run.main)] {
        let asked = run
            .keyframe_requests
            .iter()
            .any(|(_, r, at)| *r == Some(Rid::from(rid)) && *at >= run.joined);
        assert!(asked, "a key frame on {rid} was asked for after the join");
        let (_, first_at, _) = seen.stretches[0];
        let (first_shown, written) = seen.shown[0];
        assert!(
            first_at >= run.joined && written >= run.joined,
            "the track on {rid} begins with a key frame written after the join"
        );
        assert!(first_shown >= first_at);
    }
}

// ============================================================
// Two-track delivery from two senders: the publisher sends its camera
// on two ordinary media lines and declares the light one in its offer.
// ============================================================

/// The `a=mid` and `a=msid` lines of every offer the SFU sent the
/// subscriber, for the test log.
#[allow(clippy::print_stderr)]
fn print_offer_msids(what: &str, run: &SimulcastRun) {
    for (i, offer) in run.offers.iter().enumerate() {
        let lines: Vec<&str> = offer
            .lines()
            .filter(|l| l.starts_with("a=msid:") || l.starts_with("a=mid:") || l.starts_with("m="))
            .collect();
        eprintln!("{what}: the SFU's offer {i} to the subscriber: {lines:?}");
    }
}

/// A publisher with two plain video senders, the second declared light in
/// its offer, and a subscriber: the subscriber is offered two video tracks
/// whose MSID stream ids are the publisher's peer id and that id followed
/// by `~light`; the packets of the main sender arrive on the first and the
/// packets of the light sender on the second, byte for byte, and nothing
/// arrives anywhere else.
#[tokio::test]
async fn two_senders_with_the_light_one_declared_reach_a_subscriber_as_two_tracks() {
    let run = Box::pin(run_simulcast(SimulcastCase {
        senders: Senders::TwoDeclared,
        fallback: false,
        dip: None,
        late_join: None,
        stream: Duration::from_secs(3),
        settle: Duration::from_millis(700),
        protection: LossProtection::default(),
        downlink_dip: false,
        playout_ext: false,
        subscriber_nack: None,
        publisher_rtx_cap: None,
    }))
    .await;
    print_offer_msids("two senders", &run);
    assert_two_tracks(&run);
    assert!(run.others.is_empty(), "no third video track");
    assert_ne!(run.pub_main_mid, run.pub_light_mid);
    // The two senders were offered together and are opened together.
    assert_eq!(run.offers.len(), 1, "one renegotiation for the two tracks");
    let frames = |seen: &TrackSeen| seen.shown.len();
    assert!(
        frames(&run.main) >= 70 && frames(&run.light) >= 70,
        "about 75 frames a sender in three seconds: main {}, light {}",
        frames(&run.main),
        frames(&run.light)
    );
    // No layer is involved: every keyframe request names a media line and
    // no rid.
    assert!(run
        .keyframe_requests
        .iter()
        .all(|(_, rid, _)| rid.is_none()));
}

/// A subscriber that joins a two-sender publisher already streaming gets
/// both tracks, and the SFU asks the publisher for a key frame on each of
/// its two media lines once the subscriber can receive; each track can be
/// shown from a key frame written after the join.
///
/// A plain sender is forwarded from the first packet that arrives after
/// the track opens, as any single-encoding track is: what precedes its key
/// frame reaches the subscriber and cannot be shown.
#[tokio::test]
async fn a_late_joiner_of_two_senders_gets_both_tracks_and_a_key_frame_on_each() {
    let join_after = Duration::from_millis(1500);
    let run = Box::pin(run_simulcast(SimulcastCase {
        senders: Senders::TwoDeclared,
        fallback: false,
        dip: None,
        late_join: Some(join_after),
        stream: Duration::from_millis(4500),
        settle: Duration::from_millis(700),
        protection: LossProtection::default(),
        downlink_dip: false,
        playout_ext: false,
        subscriber_nack: None,
        publisher_rtx_cap: None,
    }))
    .await;
    print_offer_msids("two senders, late joiner", &run);
    assert_two_tracks(&run);
    assert!(run.others.is_empty());
    assert!(run.joined >= run.started + join_after);
    for (what, pub_mid, seen) in [
        ("light", run.pub_light_mid, &run.light),
        ("main", run.pub_main_mid, &run.main),
    ] {
        let asked = run
            .keyframe_requests
            .iter()
            .any(|(mid, rid, at)| *mid == pub_mid && rid.is_none() && *at >= run.joined);
        assert!(
            asked,
            "a key frame on the {what} sender was asked for after the join"
        );
        let (first_shown, written) = seen.shown[0];
        assert!(
            written >= run.joined && first_shown >= run.joined,
            "the {what} track is shown from a key frame written after the join"
        );
    }
}

/// A declaration that names an audio media line, and one that names a mid
/// the offer does not have, are ignored: the audio track is an ordinary
/// audio track, and the publisher's two video senders, neither of them
/// declared, are two ordinary main tracks, as two video senders of one
/// peer are without any declaration: both under the publisher's peer id
/// as MSID stream id, told apart by track id alone, and no `~light`
/// anywhere.
#[tokio::test]
async fn a_declaration_naming_an_audio_or_unknown_mid_is_ignored() {
    let run = Box::pin(run_simulcast(SimulcastCase {
        senders: Senders::TwoMisdeclared,
        fallback: false,
        dip: None,
        late_join: None,
        stream: Duration::from_secs(2),
        settle: Duration::from_millis(700),
        protection: LossProtection::default(),
        downlink_dip: false,
        playout_ext: false,
        subscriber_nack: None,
        publisher_rtx_cap: None,
    }))
    .await;
    print_offer_msids("misdeclared", &run);
    let publisher = run.publisher.to_string();
    assert!(
        run.offers.iter().all(|sdp| !sdp.contains("~light")),
        "no light companion was offered"
    );
    assert_eq!(run.light_mid, None);
    assert!(run.light.packets == 0);
    // Two video tracks, both under the publisher's own stream id.
    assert!(run.main_mid.is_some());
    assert_eq!(run.others.len(), 1, "a second main video track");
    let (other_mid, other_stream, other) = &run.others[0];
    assert_eq!(*other_stream, publisher);
    assert_ne!(Some(*other_mid), run.main_mid);
    // Each carries one of the two senders, whole and verbatim.
    let layers_of = |seen: &TrackSeen| -> Vec<char> {
        seen.stretches.iter().map(|s| char::from(s.0)).collect()
    };
    let mut carried = [layers_of(&run.main), layers_of(other)];
    carried.sort();
    assert_eq!(carried, [vec!['f'], vec!['q']]);
    assert!(run.main.alive && other.alive);
    assert_eq!(run.stray, 0);
    // The audio line that was named is an ordinary audio track.
    assert_eq!(run.audio_streams, vec![publisher]);
}

/// The camera turned on mid-call: a publisher that joined with audio alone
/// adds its two video senders in a later offer, the second declared light.
/// The subscriber gets the two tracks exactly as when they came with the
/// first offer.
#[tokio::test]
async fn two_senders_added_in_a_later_offer_reach_a_subscriber_as_two_tracks() {
    let run = Box::pin(run_simulcast(SimulcastCase {
        senders: Senders::TwoDeclaredMidCall,
        fallback: false,
        dip: None,
        late_join: None,
        stream: Duration::from_secs(3),
        settle: Duration::from_millis(700),
        protection: LossProtection::default(),
        downlink_dip: false,
        playout_ext: false,
        subscriber_nack: None,
        publisher_rtx_cap: None,
    }))
    .await;
    print_offer_msids("camera on mid-call", &run);
    assert_two_tracks(&run);
    assert!(run.others.is_empty());
    // The audio track was offered first, the two video tracks after it.
    assert!(run.offers.len() >= 2, "{} offers", run.offers.len());
    let publisher = run.publisher.to_string();
    assert!(
        video_msids(&run.offers[0])
            .iter()
            .all(|(_, stream)| !stream.starts_with(&publisher)),
        "the first offer carries none of the publisher's video"
    );
    assert_eq!(run.audio_streams.first(), Some(&publisher));
    let frames = |seen: &TrackSeen| seen.shown.len();
    assert!(
        frames(&run.main) >= 70 && frames(&run.light) >= 70,
        "main {}, light {}",
        frames(&run.main),
        frames(&run.light)
    );
}

/// The usual time from a frame being written to it being decodable on a
/// track, in milliseconds: the median over the frames written between one
/// second into the stream and the opening of the loss window. `0.0` when
/// none was shown in that time.
fn dip_baseline(seen: &TrackSeen, started: Instant, from: Instant) -> f64 {
    let mut usual: Vec<f64> = seen
        .shown
        .iter()
        .filter(|(_, written)| *written >= started + Duration::from_secs(1) && *written < from)
        .map(|(at, written)| dip_ms(at.saturating_duration_since(*written)))
        .collect();
    if usual.is_empty() {
        return 0.0;
    }
    dip_quartet(&mut usual)[1]
}

/// The worst frame lateness on `tracks`, in milliseconds, and whether every
/// moment was reached before the run ended.
///
/// `moments` are the instants the publisher wrote a frame at. A viewer has
/// the picture of a moment once any frame written at or after it is
/// decodable on one of `tracks` (a frame that never completes is passed
/// over by the next key frame). On time means the track's usual transit
/// after the moment ([`dip_baseline`]); the lateness of a moment is how
/// much later than that it was reached, never below zero. A moment that
/// was not reached by the end of the run counts up to the end.
fn dip_lateness(
    moments: &[Instant],
    tracks: &[(&TrackSeen, f64)],
    from: Instant,
    ended: Instant,
) -> (f64, bool) {
    let mut worst = 0.0_f64;
    let mut all_reached = true;
    for moment in moments.iter().filter(|m| **m >= from) {
        let mut best: Option<f64> = None;
        for (seen, baseline) in tracks {
            for (at, written) in &seen.shown {
                if written < moment {
                    continue;
                }
                let late = dip_ms(at.saturating_duration_since(*moment)) - baseline;
                best = Some(best.map_or(late, |b: f64| b.min(late)));
            }
        }
        let late = best.unwrap_or_else(|| {
            all_reached = false;
            dip_ms(ended.saturating_duration_since(*moment))
        });
        worst = worst.max(late.max(0.0));
    }
    (worst, all_reached)
}

/// The playout delay a viewer is assumed to hold in the third figure of
/// `measure_two_track_dip`, in milliseconds: what the SFU asks for after a
/// burst ([`lakko_svc_sfu::playout_delay::BURST_DELAY_MS`]).
const DIP_VIEWER_DELAY_MS: f64 = 300.0;

/// Measure what a subscriber that shows whichever of its two tracks is
/// alive would see when most datagrams are lost for one second, on the
/// publisher's uplink or on the subscriber's own downlink. Prints a table;
/// asserts nothing about the numbers.
///
/// The dip is [`DipLoss`] for [`DIP_WINDOW`]: on every media datagram the
/// publisher sends (both pictures and their retransmission streams), or,
/// with `LAKKO_DIP_DOWNLINK` set, on every media datagram that reaches the
/// subscriber. The SFU runs without the uplink fallback. Per seed and per
/// track (main: `f`, 250 packets a second; light: the companion, `q`, 50
/// packets a second; either: a client that shows whichever is alive):
///
/// - **gap**: the longest interval in which no decodable frame completes;
/// - **late**: the worst frame lateness ([`dip_lateness`]): how much later
///   than its usual transit the picture of a moment became decodable;
/// - **stop**: what a viewer playing [`DIP_VIEWER_DELAY_MS`] late would
///   see stand still: the lateness beyond that delay.
///
/// A decodable frame is [`fb_track_seen`]'s: complete, and every frame
/// before it on its layer back to a key frame complete. A `+` marks a track
/// whose chain was broken at the end of the run (gap) or a moment never
/// reached (late); the time to the end of the run is then counted.
///
/// The publisher sends the two pictures as two plain senders, the light one
/// declared in its offer, which is what the Lakko clients do; with
/// `LAKKO_DIP_SIMULCAST` set it sends them as two simulcast layers of one
/// media line instead.
///
/// - `LAKKO_DIP_SEED` (default 1): the first seed; `LAKKO_DIP_RUNS`
///   (default 10): how many consecutive seeds; `LAKKO_DIP_P` (default
///   0.70): the loss probability. The burst measured on a live call
///   (2026-10-09) is `LAKKO_DIP_P=0.80`: about eight datagrams in ten
///   lost for one second ([`measure_burst_end_key_frame`] uses it).
/// - `LAKKO_DIP_BURST_KEYFRAME=off`: the SFU does not ask for a key frame
///   at the end of an unrepaired burst (the behaviour before 2026-10-09;
///   the tables in the README were taken that way).
/// - `LAKKO_DIP_PROTECTION=off`: the SFU without ADR-0070 step 3 (the light
///   track asked again every 60 ms like every stream, each light packet
///   sent once) and step 4. Anything else, or unset: everything on.
/// - `LAKKO_DIP_SUB_NACK_MS`: how often the subscriber asks again for a
///   missing packet (default: the vendored `str0m`'s 60 ms).
#[tokio::test]
#[ignore = "a measurement, about three minutes for 20 seeds: run by name with --nocapture"]
#[allow(
    clippy::print_stderr,
    clippy::cast_precision_loss,
    clippy::too_many_lines
)]
async fn measure_two_track_dip() {
    let env = |name: &str| std::env::var(name).ok();
    let first: u64 = env("LAKKO_DIP_SEED")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let runs: u64 = env("LAKKO_DIP_RUNS")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    let p: f64 = env("LAKKO_DIP_P")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.70);
    let senders = if env("LAKKO_DIP_SIMULCAST").is_some() {
        Senders::Simulcast
    } else {
        Senders::TwoDeclared
    };
    let protected = env("LAKKO_DIP_PROTECTION").as_deref() != Some("off");
    let burst_keyframe = env("LAKKO_DIP_BURST_KEYFRAME").as_deref() != Some("off");
    let protection = if protected {
        LossProtection {
            burst_keyframe,
            ..LossProtection::default()
        }
    } else {
        LossProtection::off()
    };
    let downlink_dip = env("LAKKO_DIP_DOWNLINK").is_some();
    let subscriber_nack = env("LAKKO_DIP_SUB_NACK_MS")
        .and_then(|s| s.parse().ok())
        .map(Duration::from_millis);
    eprintln!(
        "\ntwo-track dip on the {} ({}), protection {}, subscriber asks again every {} ms: \
         p {p:.2} for {} ms, f {} packets/s on the main track, q {} packets/s on the light \
         companion, times in ms",
        if downlink_dip {
            "subscriber's downlink"
        } else {
            "publisher's uplink"
        },
        if senders == Senders::Simulcast {
            "one simulcast sender"
        } else {
            "two plain senders, the light one declared"
        },
        if protected { "on" } else { "off" },
        subscriber_nack.map_or(60, |d| d.as_millis()),
        DIP_WINDOW.as_millis(),
        FB_F_PACKETS * DIP_FPS,
        FB_Q_PACKETS * DIP_FPS
    );
    eprintln!(
        "{:>4} | {:>9} | {:>17} | {:>17} | {:>17} | {:>9}",
        "seed",
        "lost/sent",
        "main gap/late/stop",
        "light gap/late/stop",
        "either gap/late/stop",
        "q rtx/sent"
    );
    // Per seed: gap, late and stop for main, light and either.
    let mut table: Vec<[f64; 9]> = Vec::new();
    for seed in first..first + runs {
        let run = Box::pin(run_simulcast(SimulcastCase {
            senders,
            fallback: false,
            dip: Some((p, seed)),
            late_join: None,
            stream: DIP_STREAM,
            settle: DIP_SETTLE,
            protection,
            downlink_dip,
            playout_ext: true,
            subscriber_nack,
            publisher_rtx_cap: None,
        }))
        .await;
        let (from, _) = run.window;
        let times = |seen: &TrackSeen| -> Vec<Instant> { seen.shown.iter().map(|s| s.0).collect() };
        let main_gap = fb_longest_gap(&times(&run.main), run.main.alive, run.ended);
        let light_gap = fb_longest_gap(&times(&run.light), run.light.alive, run.ended);
        let mut both = times(&run.main);
        both.extend(times(&run.light));
        both.sort_unstable();
        let either_alive = run.main.alive || run.light.alive;
        let either_gap = fb_longest_gap(&both, either_alive, run.ended);

        let base_main = dip_baseline(&run.main, run.started, from);
        let base_light = dip_baseline(&run.light, run.started, from);
        let main_late = dip_lateness(&run.written_f, &[(&run.main, base_main)], from, run.ended);
        let light_late = dip_lateness(&run.written_q, &[(&run.light, base_light)], from, run.ended);
        // The main picture's frame is written first in a tick, so its
        // moments are the ones a frame of either track can answer.
        let either_late = dip_lateness(
            &run.written_f,
            &[(&run.main, base_main), (&run.light, base_light)],
            from,
            run.ended,
        );
        let stop = |late: f64| (late - DIP_VIEWER_DELAY_MS).max(0.0);
        let cell = |gap: f64, alive: bool, late: (f64, bool)| {
            format!(
                "{gap:.0}{}/{:.0}{}/{:.0}",
                if alive { "" } else { "+" },
                late.0,
                if late.1 { "" } else { "+" },
                stop(late.0)
            )
        };
        eprintln!(
            "{:>4} | {:>9} | {:>17} | {:>17} | {:>17} | {:>9}",
            seed,
            format!("{}/{}", run.lost, run.sent_in_window),
            cell(main_gap.0, run.main.alive, main_late),
            cell(light_gap.0, run.light.alive, light_late),
            cell(either_gap.0, either_alive, either_late),
            format!("{}/{}", run.light_in.1, run.light_in.0),
        );
        table.push([
            main_gap.0,
            main_late.0,
            stop(main_late.0),
            light_gap.0,
            light_late.0,
            stop(light_late.0),
            either_gap.0,
            either_late.0,
            stop(either_late.0),
        ]);
    }
    eprintln!("min / median / p95 / max over {} seeds:", table.len());
    let names = [
        "main   gap",
        "main   late",
        "main   stop",
        "light  gap",
        "light  late",
        "light  stop",
        "either gap",
        "either late",
        "either stop",
    ];
    for (column, name) in names.iter().enumerate() {
        let mut values: Vec<f64> = table.iter().map(|row| row[column]).collect();
        let zero = values.iter().filter(|v| **v <= 0.0).count();
        let q = dip_quartet(&mut values);
        let zeros = if name.ends_with("stop") {
            format!("   (zero in {zero} of {})", table.len())
        } else {
            String::new()
        };
        eprintln!(
            "  {name:<12} {:>5.0} / {:>5.0} / {:>5.0} / {:>5.0}{zeros}",
            q[0], q[1], q[2], q[3]
        );
    }
}

// ============================================================
// A key frame at the end of an unrepaired burst (ADR-0070, note of
// 2026-10-09).
// ============================================================

/// The loss measured on a live call on 2026-10-09 (a home Wi-Fi uplink,
/// every 30 s): about eight datagrams in ten lost for one second
/// ([`DIP_WINDOW`]), media and retransmissions alike.
const MEASURED_BURST_P: f64 = 0.80;

/// Upstream `str0m`'s cap on retransmitted bytes, which the fork removed
/// for the SFU's own sending. Given back to the test's publisher it stands
/// in for a sender that does not repair a whole burst.
const CAPPED_PUBLISHER: f32 = 0.15;

/// A cap so low that the test's publisher sends little a second time: a
/// burst on its main stream (some two hundred packets lost) stays
/// unrepaired. `str0m` works the share out at intervals, so the forty or so
/// packets its light stream loses are sometimes all sent again before the
/// cap bites.
const UNREPAIRING_PUBLISHER: f32 = 0.001;

/// One two-sender run through the measured burst on the publisher's
/// uplink, with every protection of the SFU on and the key frame at the
/// end of a burst switched as said.
async fn run_burst_end(
    burst_keyframe: bool,
    publisher_rtx_cap: Option<f32>,
    p: f64,
    seed: u64,
    stream: Duration,
    settle: Duration,
) -> SimulcastRun {
    Box::pin(run_simulcast(SimulcastCase {
        senders: Senders::TwoDeclared,
        fallback: false,
        dip: Some((p, seed)),
        late_join: None,
        stream,
        settle,
        protection: LossProtection {
            burst_keyframe,
            ..LossProtection::default()
        },
        downlink_dip: false,
        playout_ext: true,
        subscriber_nack: None,
        publisher_rtx_cap,
    }))
    .await
}

/// What one track of a [`run_burst_end`] run came to. Milliseconds; the
/// last four relative to the end of the loss window.
struct BurstEndTrack {
    /// Longest interval with no decodable frame, and whether the chain was
    /// alive at the end of the run (when not, the time to the end counts).
    gap: f64,
    alive: bool,
    /// What a viewer playing [`DIP_VIEWER_DELAY_MS`] late sees stand still.
    stop: f64,
    /// When the first key frame request since the window opened reached
    /// the publisher on this track.
    asked: Option<f64>,
    /// When the first packet of the first key frame written since the
    /// window opened reached the subscriber, and when the whole of it had.
    key_first: Option<f64>,
    key_whole: Option<f64>,
    /// Key frame requests that reached the publisher on this track since
    /// the window opened.
    requests: usize,
    /// When the publisher wrote that key frame.
    key_written: Option<f64>,
    /// Of its packets, how many reached the subscriber within 20 ms of the
    /// first one: the rest came by retransmission.
    key_at_once: Option<(usize, u64)>,
}

/// Reduce one track of `run` to [`BurstEndTrack`]. `tag` is the picture's
/// payload tag (`q` light, `f` main), `pub_mid` the publisher's media line.
#[allow(clippy::cast_precision_loss)]
fn burst_end_track(
    run: &SimulcastRun,
    seen: &TrackSeen,
    tag: u8,
    pub_mid: Mid,
    written: &[Instant],
    keys: &[(u64, u64)],
) -> BurstEndTrack {
    let (from, until) = run.window;
    let rel = |at: Instant| {
        if at >= until {
            dip_ms(at.duration_since(until))
        } else {
            -dip_ms(until.duration_since(at))
        }
    };
    let times: Vec<Instant> = seen.shown.iter().map(|s| s.0).collect();
    let gap = fb_longest_gap(&times, seen.alive, run.ended).0;
    let baseline = dip_baseline(seen, run.started, from);
    let late = dip_lateness(written, &[(seen, baseline)], from, run.ended).0;
    let requests: Vec<Instant> = run
        .keyframe_requests
        .iter()
        .filter(|(mid, _, at)| *mid == pub_mid && *at >= from)
        .map(|(_, _, at)| *at)
        .collect();
    let key = keys.iter().copied().find(|(frame, _)| {
        usize::try_from(*frame)
            .ok()
            .and_then(|i| written.get(i))
            .is_some_and(|at| *at >= from)
    });
    let key_first = key
        .and_then(|(frame, _)| seen.arrived.get(&(tag, frame, 0)))
        .map(|at| rel(*at));
    let key_whole = key.and_then(|(frame, n)| {
        (0..n)
            .map(|i| seen.arrived.get(&(tag, frame, i)).copied())
            .collect::<Option<Vec<Instant>>>()
            .and_then(|all| all.into_iter().max())
            .map(rel)
    });
    let key_written = key
        .and_then(|(frame, _)| written.get(usize::try_from(frame).ok()?))
        .map(|at| rel(*at));
    let key_at_once = key.and_then(|(frame, n)| {
        let first = (0..n)
            .filter_map(|i| seen.arrived.get(&(tag, frame, i)))
            .min()?;
        let soon = (0..n)
            .filter_map(|i| seen.arrived.get(&(tag, frame, i)))
            .filter(|at| at.saturating_duration_since(*first) <= Duration::from_millis(20))
            .count();
        Some((soon, n))
    });
    BurstEndTrack {
        gap,
        alive: seen.alive,
        stop: (late - DIP_VIEWER_DELAY_MS).max(0.0),
        asked: requests.first().map(|at| rel(*at)),
        key_first,
        key_whole,
        requests: requests.len(),
        key_written,
        key_at_once,
    }
}

/// The SFU asks the publisher for a key frame on a video stream on which a
/// burst has ended unrepaired, without any subscriber asking, and the
/// subscriber has a decodable picture on both tracks again; switched off,
/// nobody asks (the test's subscriber never does).
///
/// The publisher sends little a second time ([`UNREPAIRING_PUBLISHER`]), so
/// the burst on its main stream is not repaired; its light stream is asked
/// for only in the runs in which it was not repaired either. With a
/// publisher that answers every request the burst is repaired and nothing
/// is asked, which `a_repaired_uplink_burst_costs_no_key_frame` pins.
#[tokio::test]
async fn an_unrepaired_uplink_burst_ends_with_a_key_frame_asked_by_the_sfu() {
    let stream = Duration::from_secs(5);
    let settle = Duration::from_millis(1500);
    let on = run_burst_end(
        true,
        Some(UNREPAIRING_PUBLISHER),
        MEASURED_BURST_P,
        1,
        stream,
        settle,
    )
    .await;
    assert_two_tracks(&on);
    let light = burst_end_track(
        &on,
        &on.light,
        b'q',
        on.pub_light_mid,
        &on.written_q,
        &on.keys_q,
    );
    let main = burst_end_track(
        &on,
        &on.main,
        b'f',
        on.pub_main_mid,
        &on.written_f,
        &on.keys_f,
    );
    assert!(
        main.asked.is_some(),
        "the main stream was asked for a key frame"
    );
    for (name, track) in [("light", &light), ("main", &main)] {
        if let Some(asked) = track.asked {
            assert!(
                (0.0..=1000.0).contains(&asked),
                "{name}: asked {asked:.0} ms after the burst ended"
            );
            let whole = track
                .key_whole
                .unwrap_or_else(|| panic!("the {name} key frame reached the subscriber whole"));
            assert!(
                whole > asked,
                "{name}: whole at {whole:.0}, asked at {asked:.0}"
            );
        }
        assert!(
            track.requests <= 2,
            "{name}: one request per burst (and at most one repeat), saw {}",
            track.requests
        );
        assert!(
            track.alive,
            "{name}: decodable at the end of the run (longest gap {:.0} ms)",
            track.gap
        );
    }

    let off = run_burst_end(
        false,
        Some(UNREPAIRING_PUBLISHER),
        MEASURED_BURST_P,
        1,
        stream,
        settle,
    )
    .await;
    let (from, _) = off.window;
    assert!(
        off.keyframe_requests.iter().all(|(_, _, at)| *at < from),
        "switched off, nobody asks after the burst: {:?}",
        off.keyframe_requests
            .iter()
            .map(|(mid, _, at)| (*mid, dip_ms(at.saturating_duration_since(from))))
            .collect::<Vec<_>>()
    );
}

/// A burst the publisher repairs costs no key frame: with a publisher that
/// answers every retransmission request, the SFU asks for none after the
/// loss window, and both tracks are decodable at the end.
#[tokio::test]
async fn a_repaired_uplink_burst_costs_no_key_frame() {
    let run = run_burst_end(
        true,
        None,
        0.50,
        1,
        Duration::from_secs(5),
        Duration::from_millis(1500),
    )
    .await;
    assert_two_tracks(&run);
    let (from, _) = run.window;
    let after: Vec<_> = run
        .keyframe_requests
        .iter()
        .filter(|(_, _, at)| *at >= from)
        .map(|(mid, _, at)| (*mid, dip_ms(at.duration_since(from))))
        .collect();
    assert!(after.is_empty(), "asked for a key frame: {after:?}");
    assert!(run.lost > 100, "the burst lost packets: {}", run.lost);
    assert!(run.main.alive && run.light.alive);
}

/// Measure what asking for a key frame at the end of an unrepaired burst
/// buys, on the burst measured on a live call ([`MEASURED_BURST_P`] for
/// [`DIP_WINDOW`] on the publisher's uplink). Prints a table; asserts
/// nothing about the numbers.
///
/// Every seed is run twice: **before** (`LossProtection::burst_keyframe`
/// off: the SFU of 2026-10-08) and **after** (on). Per track (main: `f`,
/// 250 packets a second; light: `q`, 50 packets a second):
///
/// - **gap**: the longest interval in which no decodable frame completes
///   ([`fb_track_seen`]); a `+` marks a chain still broken when the run
///   ended, [`DIP_SETTLE`] after the last frame, and the time to the end is
///   then counted;
/// - **stop**: what a viewer playing [`DIP_VIEWER_DELAY_MS`] late sees
///   stand still;
/// - **asked**: when the first key frame request reached the publisher,
///   from the end of the burst;
/// - **first** / **whole**: when the first packet of that key frame, and
///   all of it, had reached the subscriber, from the end of the burst.
///
/// The test's subscriber never asks for a key frame itself. On the live
/// call a browser did, about three seconds after the burst had ended.
///
/// - `LAKKO_DIP_SEED` (default 1), `LAKKO_DIP_RUNS` (default 20),
///   `LAKKO_DIP_P` (default 0.80).
/// - `LAKKO_DIP_PUBLISHER`: `capped` (default): the publisher throws
///   queued retransmissions away above 15% of what it sends
///   ([`CAPPED_PUBLISHER`]), so a burst stays unrepaired; `all`: it answers
///   every request, as in `measure_two_track_dip`. Neither is a browser:
///   what a libwebrtc sender retransmits after a burst was not measured.
/// - `LAKKO_DIP_DETAIL`: one more line per track and run, about the key
///   frame itself.
#[tokio::test]
#[ignore = "a measurement, about seven minutes for 20 seeds: run by name with --nocapture"]
#[allow(
    clippy::print_stderr,
    clippy::cast_precision_loss,
    clippy::too_many_lines
)]
async fn measure_burst_end_key_frame() {
    let env = |name: &str| std::env::var(name).ok();
    let first: u64 = env("LAKKO_DIP_SEED")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let runs: u64 = env("LAKKO_DIP_RUNS")
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let p: f64 = env("LAKKO_DIP_P")
        .and_then(|s| s.parse().ok())
        .unwrap_or(MEASURED_BURST_P);
    let capped = env("LAKKO_DIP_PUBLISHER").as_deref() != Some("all");
    let cap = capped.then_some(CAPPED_PUBLISHER);
    eprintln!(
        "\nkey frame at the end of an unrepaired burst: p {p:.2} for {} ms on the publisher's \
         uplink, publisher {}, f {} packets/s on the main track, q {} packets/s on the light \
         companion, times in ms; asked / first / whole from the end of the burst",
        DIP_WINDOW.as_millis(),
        if capped {
            "caps its retransmissions at 15%"
        } else {
            "answers every request"
        },
        FB_F_PACKETS * DIP_FPS,
        FB_Q_PACKETS * DIP_FPS
    );
    eprintln!(
        "{:>4} {:>6} | {:>9} | {:>30} | {:>30} | {:>11}",
        "seed",
        "",
        "lost/sent",
        "main gap/stop asked/first/whole",
        "light gap/stop asked/first/whole",
        "nacks/rtx"
    );
    let opt = |v: Option<f64>| v.map_or_else(|| "-".to_owned(), |v| format!("{v:.0}"));
    let cell = |t: &BurstEndTrack| {
        format!(
            "{:.0}{}/{:.0} {}/{}/{}",
            t.gap,
            if t.alive { "" } else { "+" },
            t.stop,
            opt(t.asked),
            opt(t.key_first),
            opt(t.key_whole)
        )
    };
    // [before, after] x [main, light].
    let mut rows: [Vec<BurstEndTrack>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    for seed in first..first + runs {
        for (i, on) in [false, true].into_iter().enumerate() {
            let run = run_burst_end(on, cap, p, seed, DIP_STREAM, DIP_SETTLE).await;
            let (_, until) = run.window;
            let main = burst_end_track(
                &run,
                &run.main,
                b'f',
                run.pub_main_mid,
                &run.written_f,
                &run.keys_f,
            );
            let light = burst_end_track(
                &run,
                &run.light,
                b'q',
                run.pub_light_mid,
                &run.written_q,
                &run.keys_q,
            );
            // Sequence numbers the publisher was asked for, and
            // retransmissions it put on the wire, after the burst.
            let nacks = run.nacks.iter().filter(|(at, _, _)| *at >= until).count();
            let rtx = run.rtx_out.iter().filter(|at| **at >= until).count();
            eprintln!(
                "{:>4} {:>6} | {:>9} | {:>30} | {:>30} | {:>11}",
                seed,
                if on { "after" } else { "before" },
                format!("{}/{}", run.lost, run.sent_in_window),
                cell(&main),
                cell(&light),
                format!("{nacks}/{rtx}")
            );
            if env("LAKKO_DIP_DETAIL").is_some() {
                for (name, t) in [("main", &main), ("light", &light)] {
                    eprintln!(
                        "       {name}: key frame written {} ms after the burst, {:?} of its \
                         packets at the subscriber within 20 ms of the first, {} request(s)",
                        opt(t.key_written),
                        t.key_at_once,
                        t.requests
                    );
                }
            }
            rows[2 * i].push(main);
            rows[2 * i + 1].push(light);
        }
    }
    eprintln!("min / median / p95 / max over {runs} seeds:");
    let names = [
        "before main ",
        "before light",
        "after  main ",
        "after  light",
    ];
    for (name, tracks) in names.iter().zip(&rows) {
        let line = |what: &str, mut values: Vec<f64>, of: usize| {
            if values.is_empty() {
                eprintln!("  {name} {what:<6} -   (0 of {of})");
                return;
            }
            let n = values.len();
            let q = dip_quartet(&mut values);
            eprintln!(
                "  {name} {what:<6} {:>5.0} / {:>5.0} / {:>5.0} / {:>5.0}   ({n} of {of})",
                q[0], q[1], q[2], q[3]
            );
        };
        let of = tracks.len();
        line("gap", tracks.iter().map(|t| t.gap).collect(), of);
        line("stop", tracks.iter().map(|t| t.stop).collect(), of);
        line("asked", tracks.iter().filter_map(|t| t.asked).collect(), of);
        line(
            "first",
            tracks.iter().filter_map(|t| t.key_first).collect(),
            of,
        );
        line(
            "whole",
            tracks.iter().filter_map(|t| t.key_whole).collect(),
            of,
        );
        eprintln!(
            "  {name} decodable at the end of the run in {} of {of}; key frame requests since \
             the burst began: {}",
            tracks.iter().filter(|t| t.alive).count(),
            tracks.iter().map(|t| t.requests).sum::<usize>()
        );
    }
}

// ============================================================
// ADR-0070 steps 3 and 4: the light track is protected harder, and the
// playout delay follows the path.
// ============================================================

/// A two-sender run for the tests below: the light picture declared, the
/// uplink fallback off.
fn protected_case(
    protection: LossProtection,
    dip: Option<(f64, u64)>,
    downlink_dip: bool,
    playout_ext: bool,
    stream: Duration,
) -> SimulcastCase {
    SimulcastCase {
        senders: Senders::TwoDeclared,
        fallback: false,
        dip,
        late_join: None,
        stream,
        settle: Duration::from_millis(1500),
        protection,
        downlink_dip,
        playout_ext,
        subscriber_nack: None,
        publisher_rtx_cap: None,
    }
}

/// The gaps between consecutive requests for one sequence number of the
/// stream `ssrc`, over every sequence number asked for more than once, in
/// milliseconds, sorted.
fn nack_repeat_gaps(nacks: &[(Instant, u32, u16)], ssrc: u32) -> Vec<f64> {
    let mut by_seq: std::collections::BTreeMap<u16, Vec<Instant>> =
        std::collections::BTreeMap::new();
    for (at, of, seq) in nacks {
        if *of == ssrc {
            by_seq.entry(*seq).or_default().push(*at);
        }
    }
    let mut gaps: Vec<f64> = by_seq
        .values()
        .flat_map(|times| {
            times
                .windows(2)
                .map(|pair| dip_ms(pair[1].saturating_duration_since(pair[0])))
                .collect::<Vec<_>>()
        })
        .collect();
    gaps.sort_by(f64::total_cmp);
    gaps
}

/// ADR-0070 step 3, subscriber leg: every packet of the light companion
/// reaches the subscriber twice, the second time on the track's
/// retransmission stream, and the subscriber is handed each packet once.
/// The main track is sent once. Switched off, nothing is sent twice.
#[tokio::test]
#[allow(clippy::cast_precision_loss)]
async fn a_light_companion_packet_is_sent_twice_and_the_main_track_once() {
    let stream = Duration::from_secs(2);
    let on = Box::pin(run_simulcast(protected_case(
        LossProtection::default(),
        None,
        false,
        false,
        stream,
    )))
    .await;
    assert_two_tracks(&on);
    let (first, second) = on.light_in;
    assert!(first > 80, "the light track streamed: {first} datagrams");
    assert!(
        second as f64 >= 0.9 * first as f64 && second <= first,
        "a second copy of nearly every light packet on the retransmission stream, and never \
         more than one: {second} of {first}"
    );
    // The subscriber's own stack threw every second copy away: it was
    // handed each packet once.
    assert_eq!(
        on.light.packets,
        on.light.arrived.len(),
        "no packet was handed over twice"
    );
    // The main track is sent once: nothing lost, so nothing on its
    // retransmission stream.
    assert!(on.main_in.0 > 400, "the main track streamed");
    assert_eq!(on.main_in.1, 0, "no second copy of a main track packet");
    assert_eq!(on.main.packets, on.main.arrived.len());

    let off = Box::pin(run_simulcast(protected_case(
        LossProtection {
            light_duplicate: false,
            ..LossProtection::default()
        },
        None,
        false,
        false,
        stream,
    )))
    .await;
    assert_two_tracks(&off);
    assert!(off.light_in.0 > 80);
    assert_eq!(
        off.light_in.1, 0,
        "switched off, nothing is sent a second time"
    );
}

/// ADR-0070 step 3, publisher leg: through a dip on the publisher's uplink
/// a missing packet of its declared light track is asked for again about
/// every 20 ms, and a missing packet of its main track about every 60 ms
/// as before. With the light schedule left alone, both are asked every
/// 60 ms.
#[tokio::test]
async fn a_missing_light_packet_is_asked_for_again_three_times_as_often() {
    let median = |gaps: &[f64]| gaps[gaps.len() / 2];
    let fast = Box::pin(run_simulcast(protected_case(
        LossProtection::default(),
        Some((0.7, 1)),
        false,
        false,
        DIP_STREAM,
    )))
    .await;
    let light = nack_repeat_gaps(&fast.nacks, fast.pub_light_ssrc);
    let main: Vec<f64> = {
        let mut ssrcs: Vec<u32> = fast.nacks.iter().map(|n| n.1).collect();
        ssrcs.sort_unstable();
        ssrcs.dedup();
        let other = ssrcs
            .into_iter()
            .find(|ssrc| *ssrc != fast.pub_light_ssrc)
            .expect("the main stream lost packets too");
        nack_repeat_gaps(&fast.nacks, other)
    };
    assert!(light.len() > 20 && main.len() > 20, "both were asked again");
    eprintln!(
        "repeat requests: light median {:.1} ms over {}, main median {:.1} ms over {}",
        median(&light),
        light.len(),
        median(&main),
        main.len()
    );
    assert!(
        (15.0..40.0).contains(&median(&light)),
        "the light track is asked again about every 20 ms: median {:.1} ms",
        median(&light)
    );
    assert!(
        median(&main) >= 50.0,
        "the main track keeps its 60 ms: median {:.1} ms",
        median(&main)
    );

    let slow = Box::pin(run_simulcast(protected_case(
        LossProtection {
            light_nack_retry: None,
            ..LossProtection::default()
        },
        Some((0.7, 1)),
        false,
        false,
        DIP_STREAM,
    )))
    .await;
    let light = nack_repeat_gaps(&slow.nacks, slow.pub_light_ssrc);
    assert!(light.len() > 20);
    assert!(
        median(&light) >= 50.0,
        "without the faster schedule the light track is asked every 60 ms: median {:.1} ms",
        median(&light)
    );
}

/// The minimum playout delay each video packet handed to the subscriber
/// carried, split at `at`: the values seen before it and from it on.
fn playout_minimums(run: &SimulcastRun, track: Option<Mid>, at: Instant) -> (Vec<u64>, Vec<u64>) {
    let mut before = Vec::new();
    let mut after = Vec::new();
    for (on, when, delay) in &run.playout {
        if *on != track {
            continue;
        }
        let (min, max) = delay.expect("every video packet carries the extension");
        assert_eq!(
            max,
            u64::from(lakko_svc_sfu::playout_delay::MAX_PLAYOUT_DELAY_MS),
            "the maximum is the fixed generous value"
        );
        if *when < at {
            before.push(min);
        } else {
            after.push(min);
        }
    }
    (before, after)
}

/// ADR-0070 step 4: a subscriber that negotiated the playout-delay header
/// extension gets it on every video packet of the publisher, main track
/// and light companion alike, with a minimum of 0 until a burst on the
/// publisher's uplink and of 300 ms after it.
#[tokio::test]
async fn video_carries_a_playout_delay_of_300_ms_after_a_publisher_dip_and_0_before() {
    let run = Box::pin(run_simulcast(protected_case(
        LossProtection::default(),
        Some((0.7, 1)),
        false,
        true,
        DIP_STREAM,
    )))
    .await;
    let (from, until) = run.window;
    assert!(
        run.in_ext_ids
            .contains(&lakko_svc_sfu::media::PLAYOUT_DELAY_EXTENSION_ID),
        "the extension is on the wire: ids {:?}",
        run.in_ext_ids
    );
    for (name, track) in [("main", run.main_mid), ("light", run.light_mid)] {
        assert!(track.is_some());
        let (before, _) = playout_minimums(&run, track, from);
        assert!(before.len() > 50, "{name}: packets before the dip");
        assert!(
            before.iter().all(|min| *min == 0),
            "{name}: no delay is asked for before a burst was seen"
        );
        let (_, after) = playout_minimums(&run, track, until);
        assert!(after.len() > 50, "{name}: packets after the dip");
        assert!(
            after.iter().all(|min| *min == 300),
            "{name}: 300 ms on every packet once the burst was seen: {:?}",
            after.iter().filter(|min| **min != 300).count()
        );
    }
}

/// ADR-0070 step 4, the other burst source: loss on the subscriber's own
/// downlink (it asks for more than a tenth of what it is sent again, and
/// its receiver reports say so) raises the delay of that subscriber's
/// video too.
#[tokio::test]
async fn a_dip_on_the_subscribers_downlink_raises_its_playout_delay() {
    let run = Box::pin(run_simulcast(protected_case(
        LossProtection::default(),
        Some((0.7, 1)),
        true,
        true,
        DIP_STREAM,
    )))
    .await;
    let (from, until) = run.window;
    assert!(run.lost > 100, "the downlink lost datagrams: {}", run.lost);
    for (name, track) in [("main", run.main_mid), ("light", run.light_mid)] {
        let (before, _) = playout_minimums(&run, track, from);
        assert!(
            before.len() > 50 && before.iter().all(|min| *min == 0),
            "{name}"
        );
        // Packets sent before the delay was raised are still being
        // retransmitted for a while after the dip, with the value they
        // were first sent with.
        let (_, after) = playout_minimums(&run, track, until + Duration::from_secs(2));
        assert!(after.len() > 20, "{name}: packets well after the dip");
        assert!(
            after.iter().all(|min| *min == 300),
            "{name}: 300 ms once the burst was seen"
        );
    }
}

/// ADR-0070 step 4: a subscriber whose session does not know the
/// playout-delay extension leaves it out of its answer and gets none on
/// the wire, dip or no dip; and with the feature switched off, a subscriber
/// that does know it is not offered it.
#[tokio::test]
async fn the_playout_delay_is_stamped_only_when_negotiated_and_switched_on() {
    let id = lakko_svc_sfu::media::PLAYOUT_DELAY_EXTENSION_ID;
    let extmap = "http://www.webrtc.org/experiments/rtp-hdrext/playout-delay";
    // The subscriber does not know the extension.
    let unaware = Box::pin(run_simulcast(protected_case(
        LossProtection::default(),
        Some((0.7, 1)),
        false,
        false,
        DIP_STREAM,
    )))
    .await;
    assert!(
        unaware.offers.iter().any(|sdp| sdp.contains(extmap)),
        "the SFU offers it"
    );
    assert!(
        !unaware.in_ext_ids.contains(&id),
        "but stamps nothing on a track that did not negotiate it: ids {:?}",
        unaware.in_ext_ids
    );
    assert!(unaware.playout.iter().all(|(_, _, delay)| delay.is_none()));
    assert!(unaware.main.packets > 500 && unaware.light.packets > 100);

    // The feature is off.
    let off = Box::pin(run_simulcast(protected_case(
        LossProtection {
            playout_delay: false,
            ..LossProtection::default()
        },
        Some((0.7, 1)),
        false,
        true,
        DIP_STREAM,
    )))
    .await;
    assert!(
        off.offers.iter().all(|sdp| !sdp.contains(extmap)),
        "switched off, the extension is not offered"
    );
    assert!(!off.in_ext_ids.contains(&id));
    assert!(off.playout.iter().all(|(_, _, delay)| delay.is_none()));
    assert!(off.main.packets > 500 && off.light.packets > 100);
}

// ============================================================
// ADR-0070 step 5: three senders, and the subscriber says which
// picture it shows
// ============================================================

/// One packet the subscriber of [`three_senders_and_the_subscribers_wants`]
/// was handed on one of its tracks.
#[derive(Clone, Copy)]
struct WantsGot {
    at: Instant,
    seq: u16,
    /// Frame and packet index, from the payload.
    frame: u64,
    i: u64,
}

/// A publisher with three declared senders (main, middle, light) and a
/// subscriber, through the production media task.
///
/// - The subscriber is offered three video tracks whose MSID stream ids are
///   the publisher's peer id, that id + `~mid` and that id + `~light`.
/// - Having said nothing, it receives the main and the light track; the
///   middle track carries no packet.
/// - After `video-wants: middle` the middle and the light track carry
///   packets and the main track stops; the middle track begins at the first
///   packet of a key frame the publisher was asked for.
/// - After `video-wants: main` the main track resumes at the first packet
///   of a key frame written after the request, the middle track stops, and
///   the main track's sequence numbers run on without a gap across the
///   pause: the subscriber asked for no retransmission on it.
/// - Every payload arrives byte for byte, on the track of its sender.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::print_stderr
)]
async fn three_senders_and_the_subscribers_wants() {
    use lakko_svc_sfu::frame_descriptor::PacketDescriptor;
    use lakko_svc_sfu::media::{LIGHT_STREAM_SUFFIX, MIDDLE_STREAM_SUFFIX};
    use lakko_svc_sfu::protocol::{VideoQuality, VideoWant};

    // When the wants change, in frames from the start, and when the
    // publisher stops.
    const TO_MIDDLE: u64 = 35;
    const TO_MAIN: u64 = 80;
    const FRAMES: u64 = 125;
    // A packet the SFU had already written when a set arrived may still
    // be on its way.
    const IN_FLIGHT: Duration = Duration::from_millis(300);

    let call = "three-sender-wants-call";
    let peers = PeerRegistry::new();
    let mut registered = register_subscribers(&peers, call, 1).await;
    let (b_id, mut b_rx) = registered.pop().expect("one registered");
    let (media_task, sfu_addr) = spawn_media_task_with_options(
        "127.0.0.1:0".parse().unwrap(),
        None,
        peers,
        MediaTaskOptions {
            diagnostics: None,
            video_fec: false,
            uplink_fallback: false,
            protection: LossProtection::default(),
            downlink_control: false,
        },
    )
    .await
    .expect("UDP bind on loopback:0 succeeds");

    let mut a = SyntheticClient::build(
        Rtc::builder()
            .set_rtp_mode(true)
            .enable_raw_packets(true)
            .set_extension(
                GENERIC_FRAME_DESCRIPTOR_ID,
                generic_frame_descriptor_extension(),
            ),
    )
    .await;
    let mut b = SyntheticClient::build(Rtc::builder().set_rtp_mode(true).set_extension(
        GENERIC_FRAME_DESCRIPTOR_ID,
        generic_frame_descriptor_extension(),
    ))
    .await;

    let (b_offer, b_pending) = b.create_video_subscriber_offer();
    let b_answer = media_task
        .accept_offer(b_id.clone(), call.to_owned(), b_offer)
        .await
        .expect("SFU accepts B's subscriber offer");
    b.accept_answer(b_pending, &b_answer)
        .expect("B applies the SFU's answer");

    let a_id = fresh_peer_id();
    let (a_offer, a_pending, pub_main, pub_light, pub_middle) =
        a.create_three_sender_publisher_offer();
    let a_answer = media_task
        .accept_offer_with_copies(
            a_id.clone(),
            call.to_owned(),
            a_offer,
            vec![pub_light.to_string()],
            vec![pub_middle.to_string()],
        )
        .await
        .expect("SFU accepts A's offer");
    a.accept_answer(a_pending, &a_answer)
        .expect("A applies the SFU's answer");

    // Main `f`, middle `m`, light `q`.
    let layer = |mid: Mid, tag: u8, packets: u64, payload: usize, first_seq: u64| FbLayer {
        rid: None,
        mid: Some(mid),
        tag,
        packets,
        payload,
        next_seq: first_seq,
        frames: Vec::new(),
        want_key: true,
    };
    let mut layers = [
        layer(pub_main, b'f', FB_F_PACKETS, FB_F_PAYLOAD, FB_F_FIRST_SEQ),
        layer(pub_middle, b'm', 5, 800, 40_000),
        layer(pub_light, b'q', FB_Q_PACKETS, FB_Q_PAYLOAD, FB_Q_FIRST_SEQ),
    ];

    let frame_every = Duration::from_micros(1_000_000 / DIP_FPS);
    let give_up = Instant::now() + Duration::from_secs(60);
    let mut offers: Vec<String> = Vec::new();
    let mut t0: Option<Instant> = None;
    let mut ticks: u64 = 0;
    let mut plis_handled = 0usize;
    let mut finish_at: Option<Instant> = None;
    let mut to_middle_at: Option<Instant> = None;
    let mut to_main_at: Option<Instant> = None;
    let mut abuf = [0u8; 2048];
    let mut bbuf = [0u8; 2048];

    loop {
        let now = Instant::now();
        assert!(
            now < give_up,
            "the run did not finish: ticks={ticks} a_connected={} b_connected={} offers={}",
            a.is_connected(),
            b.is_connected(),
            offers.len()
        );
        if finish_at.is_some_and(|at| now >= at) {
            break;
        }
        let _ = a.drain().await;
        let _ = b.drain().await;
        // A keyframe request on a media line makes the next frame of that
        // sender a key frame, as an encoder does.
        while plis_handled < a.keyframe_request_mids.len() {
            let on = a.keyframe_request_mids[plis_handled];
            plis_handled += 1;
            for layer in &mut layers {
                if layer.mid == Some(on) {
                    layer.want_key = true;
                }
            }
        }
        if b.is_connected() {
            while let Ok(frame) = b_rx.try_recv() {
                if let ServerFrame::SdpOffer { sdp } = frame {
                    let answer = b.answer_renegotiation(&sdp).expect("B answers");
                    media_task
                        .accept_renegotiation_answer(b_id.clone(), answer)
                        .await
                        .expect("the SFU takes B's answer");
                    offers.push(sdp);
                }
            }
        }
        let b_tracks = offers
            .iter()
            .flat_map(|sdp| video_msids(sdp))
            .filter(|(_, stream)| stream.starts_with(&a_id.to_string()))
            .count();
        if t0.is_none() && a.is_connected() && b.is_connected() && b_tracks >= 3 {
            let mut api = a.rtc.direct_api();
            let ready = layers.iter().all(|layer| {
                layer
                    .mid
                    .and_then(|mid| api.stream_tx_by_mid(mid, None))
                    .is_some_and(|stream| stream.rtx().is_some())
            });
            if ready {
                t0 = Some(Instant::now() + Duration::from_millis(50));
            }
        }
        if let Some(start) = t0 {
            let due = start + frame_every * (ticks as u32);
            if ticks < FRAMES && Instant::now() >= due {
                if ticks == TO_MIDDLE {
                    media_task
                        .set_video_wants(
                            b_id.clone(),
                            vec![VideoWant {
                                peer: a_id.to_string(),
                                quality: VideoQuality::Middle,
                            }],
                        )
                        .expect("the media task takes the wants");
                    to_middle_at = Some(Instant::now());
                }
                if ticks == TO_MAIN {
                    media_task
                        .set_video_wants(
                            b_id.clone(),
                            vec![VideoWant {
                                peer: a_id.to_string(),
                                quality: VideoQuality::Main,
                            }],
                        )
                        .expect("the media task takes the wants");
                    to_main_at = Some(Instant::now());
                }
                for layer in &mut layers {
                    let frame = layer.frames.len() as u64;
                    let key = std::mem::take(&mut layer.want_key);
                    let n = if key {
                        layer.packets * FB_KEY_FACTOR
                    } else {
                        layer.packets
                    };
                    let rtp_time = 90_000 + (ticks * (90_000 / DIP_FPS)) as u32;
                    let written = Instant::now();
                    for i in 0..n {
                        let mut ext = str0m::rtp::ExtensionValues::default();
                        ext.user_values
                            .set(PacketDescriptor(fb_descriptor(frame, key, i, n)));
                        let seq = layer.next_seq;
                        layer.next_seq += 1;
                        let mut api = a.rtc.direct_api();
                        let stream = api
                            .stream_tx_by_mid(layer.mid.expect("offered"), None)
                            .expect("send stream");
                        stream
                            .write_rtp(
                                UPLINK_VP8_PT.into(),
                                seq.into(),
                                rtp_time,
                                written,
                                i + 1 == n,
                                ext,
                                true,
                                fb_payload(layer.tag, frame, i, layer.payload),
                            )
                            .expect("A writes a packet");
                    }
                    layer.frames.push(FbFrame {
                        key,
                        n,
                        at: written,
                    });
                }
                ticks += 1;
                if ticks == FRAMES {
                    finish_at = Some(Instant::now() + Duration::from_millis(700));
                }
                a.feed_timeout();
                let _ = a.drain().await;
            }
        }
        while let Ok((len, src)) = a.socket.try_recv_from(&mut abuf) {
            a.feed_receive(&abuf[..len], src, sfu_addr);
            let _ = a.drain().await;
        }
        while let Ok((len, src)) = b.socket.try_recv_from(&mut bbuf) {
            b.feed_receive(&bbuf[..len], src, sfu_addr);
            let _ = b.drain().await;
        }
        let a_sock = a.socket.clone();
        let b_sock = b.socket.clone();
        tokio::select! {
            r = a_sock.recv_from(&mut abuf) => {
                if let Ok((len, src)) = r {
                    a.feed_receive(&abuf[..len], src, sfu_addr);
                }
            }
            r = b_sock.recv_from(&mut bbuf) => {
                if let Ok((len, src)) = r {
                    b.feed_receive(&bbuf[..len], src, sfu_addr);
                }
            }
            () = tokio::time::sleep(Duration::from_millis(1)) => {}
        }
        let _ = a.drain().await;
        let _ = b.drain().await;
        a.feed_timeout();
        b.feed_timeout();
    }
    let to_middle_at = to_middle_at.expect("the middle picture was asked for");
    let to_main_at = to_main_at.expect("the main picture was asked for again");

    // Three MSID stream ids, as the contract spells them.
    let main_stream = a_id.to_string();
    let middle_stream = format!("{a_id}{MIDDLE_STREAM_SUFFIX}");
    let light_stream = format!("{a_id}{LIGHT_STREAM_SUFFIX}");
    for (i, offer) in offers.iter().enumerate() {
        let lines: Vec<&str> = offer
            .lines()
            .filter(|l| l.starts_with("a=msid:") || l.starts_with("a=mid:") || l.starts_with("m="))
            .collect();
        eprintln!("three senders: the SFU's offer {i} to the subscriber: {lines:?}");
    }
    let mut tracks: Vec<(Mid, String)> = Vec::new();
    for (mid, stream) in offers.iter().flat_map(|sdp| video_msids(sdp)) {
        if stream.starts_with(&main_stream) && !tracks.iter().any(|(m, _)| *m == mid) {
            tracks.push((mid, stream));
        }
    }
    let mut streams: Vec<&str> = tracks.iter().map(|(_, s)| s.as_str()).collect();
    streams.sort_unstable();
    let mut expected = vec![
        main_stream.as_str(),
        middle_stream.as_str(),
        light_stream.as_str(),
    ];
    expected.sort_unstable();
    assert_eq!(streams, expected, "three tracks, three stream ids");
    let mid_of = |stream: &str| {
        tracks
            .iter()
            .find(|(_, s)| s == stream)
            .map(|(mid, _)| *mid)
            .expect("the track was offered")
    };
    let (main_mid, middle_mid, light_mid) = (
        mid_of(&main_stream),
        mid_of(&middle_stream),
        mid_of(&light_stream),
    );

    // What arrived on each track, every payload byte for byte and on the
    // track of its sender.
    let on = |track: Mid, layer: &FbLayer| -> Vec<WantsGot> {
        b.rtp_got
            .iter()
            .zip(&b.rtp_got_at)
            .zip(&b.rtp_got_track)
            .filter(|(_, t)| **t == Some(track))
            .map(|((got, at), _)| {
                let pl = &got.payload;
                assert!(pl.len() >= 7, "a media packet of this test");
                assert_eq!(
                    pl[0],
                    layer.tag,
                    "a packet of sender {} on the track of sender {}",
                    char::from(pl[0]),
                    char::from(layer.tag)
                );
                let frame = u64::from(u32::from_be_bytes([pl[1], pl[2], pl[3], pl[4]]));
                let i = u64::from(u16::from_be_bytes([pl[5], pl[6]]));
                assert_eq!(
                    *pl,
                    fb_payload(layer.tag, frame, i, layer.payload),
                    "the payload arrives byte for byte as the publisher wrote it"
                );
                WantsGot {
                    at: *at,
                    seq: got.seq,
                    frame,
                    i,
                }
            })
            .collect()
    };
    let main = on(main_mid, &layers[0]);
    let middle = on(middle_mid, &layers[1]);
    let light = on(light_mid, &layers[2]);
    assert_eq!(
        main.len() + middle.len() + light.len(),
        b.rtp_got.len(),
        "nothing arrived anywhere else"
    );
    let count = |got: &[WantsGot], from: Instant, until: Instant| {
        got.iter().filter(|p| p.at >= from && p.at < until).count()
    };
    let start = t0.expect("the publisher streamed");
    let end = Instant::now();
    eprintln!(
        "three senders: packets main/middle/light: no wants {}/{}/{}, middle {}/{}/{}, \
         main again {}/{}/{}",
        count(&main, start, to_middle_at),
        count(&middle, start, to_middle_at),
        count(&light, start, to_middle_at),
        count(&main, to_middle_at, to_main_at),
        count(&middle, to_middle_at, to_main_at),
        count(&light, to_middle_at, to_main_at),
        count(&main, to_main_at, end),
        count(&middle, to_main_at, end),
        count(&light, to_main_at, end),
    );

    // No wants: main and light carry packets, the middle track none.
    assert!(count(&main, start, to_middle_at) > 200, "main flows");
    assert!(count(&light, start, to_middle_at) > 40, "light flows");
    assert_eq!(
        count(&middle, start, to_middle_at),
        0,
        "the middle track is not sent to a subscriber that did not ask"
    );

    // Middle: middle and light carry packets, the main track stops.
    assert!(
        count(&middle, to_middle_at, to_main_at) > 100,
        "middle flows"
    );
    assert!(
        count(&light, to_middle_at, to_main_at) > 40,
        "light flows on"
    );
    assert_eq!(
        count(&main, to_middle_at + IN_FLIGHT, to_main_at),
        0,
        "the main track stops"
    );
    let first_middle = middle.first().expect("the middle track carried packets");
    let frame_of =
        |layer: &FbLayer, frame: u64| layer.frames[usize::try_from(frame).expect("fits")];
    assert!(
        frame_of(&layers[1], first_middle.frame).key && first_middle.i == 0,
        "the middle track begins at the first packet of a key frame \
         (frame {}, packet {})",
        first_middle.frame,
        first_middle.i
    );
    assert!(
        frame_of(&layers[1], first_middle.frame).at >= to_middle_at,
        "a key frame the publisher wrote after it was asked"
    );

    // Main again: the main track resumes from a key frame, the middle
    // track stops, the light track flows on.
    assert!(count(&main, to_main_at, end) > 200, "main flows again");
    assert!(count(&light, to_main_at, end) > 40, "light flows on");
    assert_eq!(
        count(&middle, to_main_at + IN_FLIGHT, end),
        0,
        "the middle track stops"
    );
    let resumed = main
        .iter()
        .find(|p| p.at >= to_main_at)
        .expect("the main track resumed");
    let resumed_at = main
        .iter()
        .position(|p| p.seq == resumed.seq)
        .expect("in the list");
    // The first packet after the pause: nothing arrived on the track for
    // a while before it.
    let before = main[resumed_at - 1];
    assert!(
        resumed.at.saturating_duration_since(before.at) > Duration::from_secs(1),
        "the pause is visible on the main track"
    );
    assert!(
        frame_of(&layers[0], resumed.frame).key && resumed.i == 0,
        "the main track resumes at the first packet of a key frame \
         (frame {}, packet {})",
        resumed.frame,
        resumed.i
    );
    assert!(
        frame_of(&layers[0], resumed.frame).at >= to_main_at,
        "a key frame the publisher wrote after it was asked"
    );
    assert!(
        resumed.frame > before.frame + 20,
        "about 45 frames of the main sender went by during the pause"
    );
    // No sequence gap across the pause, or anywhere: the numbers the
    // subscriber was handed on the main track are consecutive, and it was
    // sent no retransmission on it (it asked for none).
    for pair in main.windows(2) {
        assert_eq!(
            pair[1].seq,
            pair[0].seq.wrapping_add(1),
            "the main track's sequence numbers are consecutive (frames {} and {})",
            pair[0].frame,
            pair[1].frame
        );
    }
    for pair in middle.windows(2) {
        assert_eq!(pair[1].seq, pair[0].seq.wrapping_add(1));
    }
    let main_rtx = {
        let mut api = b.rtc.direct_api();
        let stream = api.stream_rx_by_mid(main_mid, None).expect("the stream");
        stream.rtx().map(|rtx| *rtx)
    };
    let main_rtx = main_rtx.expect("the main track has a retransmission stream");
    assert_eq!(
        b.rtp_in.iter().filter(|p| p.ssrc == main_rtx).count(),
        0,
        "no retransmission was sent on the main track"
    );

    // The publisher was asked for a key frame on the track that was to
    // start, at the moment it was wanted.
    let asked = |mid: Mid, from: Instant, until: Instant| {
        a.keyframe_request_mids
            .iter()
            .zip(&a.keyframe_request_at)
            .filter(|(m, at)| **m == mid && **at >= from && **at < until)
            .count()
    };
    assert!(
        asked(pub_middle, to_middle_at, to_main_at) >= 1,
        "a key frame of the middle sender was asked for when it was wanted"
    );
    assert!(
        asked(pub_main, to_main_at, end) >= 1,
        "a key frame of the main sender was asked for when it was wanted again"
    );
    assert_eq!(
        asked(pub_middle, start, to_middle_at),
        0,
        "nobody asks for a key frame of a track nobody is sent"
    );
}

// ============================================================
// Senders nobody watches: the SFU tells a publisher which of its
// camera senders somebody is being sent (`video-needed`)
// ============================================================

/// A publisher with three declared senders and two subscribers that both
/// show the middle picture, through the production media task under
/// server-side bitrate control.
///
/// - The publisher is first told `main: true, middle: true`, and, once
///   nobody has wanted its main picture for
///   `lakko_svc_sfu::video_needed::UNNEEDED_AFTER` (nobody ever names
///   it: a subscriber that has said nothing counts for nothing),
///   `main: false,
///   middle: true`. It then stops writing its main sender, as a client
///   does.
/// - While the sender is off the SFU asks for nothing on it: no
///   retransmission and no key frame.
/// - One subscriber then asks for the main picture. The publisher is told
///   `main: true` within [`NEEDED_TOLD_WITHIN`] and starts writing again,
///   **with a delta frame first** (an encoder that kept its state; the
///   harder case for the SFU). The SFU asks it for a key frame, and the
///   subscriber's main track starts at the first packet of that key
///   frame, within [`NEEDED_BACK_WITHIN`] of the request, on consecutive
///   sequence numbers (it has no gap to ask a retransmission for, and is
///   sent none).
/// - The first REMB the publisher is sent after that is not lower than
///   the last one before its sender was switched off.
/// - While the main sender is off a third subscriber joins, says `light`
///   the moment it is offered the tracks and `middle` 230 ms later: the
///   publisher is told nothing because of it for the seconds until
///   somebody names main, and the joiner is never sent a main packet.
/// - The other subscriber's main track stays silent throughout, and every
///   payload arrives byte for byte on the track of its sender.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
#[allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::cast_possible_truncation,
    clippy::print_stderr
)]
async fn a_sender_nobody_watches_is_switched_off_and_comes_back_at_a_key_frame() {
    use lakko_svc_sfu::frame_descriptor::PacketDescriptor;
    use lakko_svc_sfu::media::{LIGHT_STREAM_SUFFIX, MIDDLE_STREAM_SUFFIX};
    use lakko_svc_sfu::protocol::{VideoQuality, VideoWant};
    use lakko_svc_sfu::video_needed::UNNEEDED_AFTER;

    /// How soon after a subscriber asks for a picture whose sender is off
    /// the publisher has been told to switch it on. Chosen for this test:
    /// the SFU decides within 20 ms and one loop pass; the rest is the
    /// test's own loop.
    const NEEDED_TOLD_WITHIN: Duration = Duration::from_millis(500);
    /// How soon after that request the subscriber has the first packet
    /// of a key frame on its main track: told, a delta frame, the SFU's
    /// key frame request, the key frame.
    const NEEDED_BACK_WITHIN: Duration = Duration::from_millis(1500);
    // Both subscribers ask for the middle picture this many frames in.
    const TO_MIDDLE: u64 = 25;
    // How long the main sender stays off before one of them asks for
    // main, and how long the run goes on afterwards, in frames.
    const OFF_FRAMES: u64 = 150;
    // A third subscriber joins this many frames after the main sender
    // went off, says `light` the moment it is offered the tracks and
    // `middle` this long after, as the web client does. A joiner counted
    // as wanting main at any point would show as an early `main: true`.
    const JOIN_FRAMES: u64 = 15;
    const JOINER_SAYS_MIDDLE_AFTER: Duration = Duration::from_millis(230);
    const BACK_FRAMES: u64 = 75;
    const IN_FLIGHT: Duration = Duration::from_millis(300);

    let call = "video-needed-call";
    let peers = PeerRegistry::new();
    let mut registered = register_subscribers(&peers, call, 4).await;
    let (d_id, mut d_rx) = registered.pop().expect("registered");
    let (a_id, mut a_rx) = registered.pop().expect("registered");
    let (c_id, mut c_rx) = registered.pop().expect("registered");
    let (b_id, mut b_rx) = registered.pop().expect("registered");
    let (media_task, sfu_addr) = spawn_media_task_with_bitrate_control(
        "127.0.0.1:0".parse().unwrap(),
        None,
        peers,
        MediaTaskOptions {
            diagnostics: None,
            video_fec: false,
            uplink_fallback: false,
            protection: LossProtection::default(),
            downlink_control: false,
        },
        BitrateControl::Server(EstimatorConfig::default()),
    )
    .await
    .expect("UDP bind on loopback:0 succeeds");

    let builder = || {
        Rtc::builder().set_rtp_mode(true).set_extension(
            GENERIC_FRAME_DESCRIPTOR_ID,
            generic_frame_descriptor_extension(),
        )
    };
    let mut a = SyntheticClient::build(builder().enable_raw_packets(true)).await;
    let mut b = SyntheticClient::build(builder()).await;
    let mut c = SyntheticClient::build(builder()).await;
    // The one that joins later.
    let mut d = SyntheticClient::build(builder()).await;
    let mut d_joined_at: Option<Instant> = None;
    let mut d_offers: Vec<String> = Vec::new();
    let mut d_said_light: Option<Instant> = None;
    let mut d_said_middle: Option<Instant> = None;

    for (client, id) in [(&mut b, &b_id), (&mut c, &c_id)] {
        let (offer, pending) = client.create_video_subscriber_offer();
        let answer = media_task
            .accept_offer(id.clone(), call.to_owned(), offer)
            .await
            .expect("SFU accepts a subscriber offer");
        client
            .accept_answer(pending, &answer)
            .expect("the subscriber applies the SFU's answer");
    }
    let (a_offer, a_pending, pub_main, pub_light, pub_middle) =
        a.create_three_sender_publisher_offer();
    let a_answer = media_task
        .accept_offer_with_copies(
            a_id.clone(),
            call.to_owned(),
            a_offer,
            vec![pub_light.to_string()],
            vec![pub_middle.to_string()],
        )
        .await
        .expect("SFU accepts A's offer");
    a.accept_answer(a_pending, &a_answer)
        .expect("A applies the SFU's answer");

    let layer = |mid: Mid, tag: u8, packets: u64, payload: usize, first_seq: u64| FbLayer {
        rid: None,
        mid: Some(mid),
        tag,
        packets,
        payload,
        next_seq: first_seq,
        frames: Vec::new(),
        want_key: true,
    };
    let mut layers = [
        layer(pub_main, b'f', FB_F_PACKETS, FB_F_PAYLOAD, FB_F_FIRST_SEQ),
        layer(pub_middle, b'm', 5, 800, 40_000),
        layer(pub_light, b'q', FB_Q_PACKETS, FB_Q_PAYLOAD, FB_Q_FIRST_SEQ),
    ];
    // What the publisher was told, in order: (when, main, middle).
    let mut told: Vec<(Instant, bool, bool)> = Vec::new();
    // The `publish-budget` frames the publisher got (ADR-0068, amendment
    // of 2026-10-11): when, and the two ceilings.
    let mut budgets: Vec<(Instant, u32, u32)> = Vec::new();
    // Whether the "client" is sending its main and its middle sender.
    let mut sending = [true, true];

    let frame_every = Duration::from_micros(1_000_000 / DIP_FPS);
    let give_up = Instant::now() + Duration::from_secs(60);
    let mut b_offers: Vec<String> = Vec::new();
    let mut c_offers: Vec<String> = Vec::new();
    let mut t0: Option<Instant> = None;
    let mut ticks: u64 = 0;
    let mut plis_handled = 0usize;
    let mut to_middle_at: Option<Instant> = None;
    // The tick the main sender was switched off at, and the one it was
    // switched back on at.
    let mut off_tick: Option<u64> = None;
    let mut to_main_at: Option<Instant> = None;
    let mut back_tick: Option<u64> = None;
    let mut finish_at: Option<Instant> = None;
    let mut bufs = [[0u8; 2048]; 3];
    let mut dbuf = [0u8; 2048];

    loop {
        let now = Instant::now();
        assert!(
            now < give_up,
            "the run did not finish: ticks={ticks} told={told:?} off_tick={off_tick:?} \
             back_tick={back_tick:?}"
        );
        if finish_at.is_some_and(|at| now >= at) {
            break;
        }
        for client in [&mut a, &mut b, &mut c] {
            let _ = client.drain().await;
        }
        // A keyframe request on a media line makes the next frame of that
        // sender a key frame, as an encoder does.
        while plis_handled < a.keyframe_request_mids.len() {
            let on = a.keyframe_request_mids[plis_handled];
            plis_handled += 1;
            for layer in &mut layers {
                if layer.mid == Some(on) {
                    layer.want_key = true;
                }
            }
        }
        // The publisher's own signaling: what the web client will do
        // with the frame. A sender that is switched back on goes on from
        // its next delta frame.
        while let Ok(frame) = a_rx.try_recv() {
            if let ServerFrame::PublishBudget {
                main_kbps,
                middle_kbps,
                ..
            } = frame
            {
                budgets.push((Instant::now(), main_kbps, middle_kbps));
                continue;
            }
            if let ServerFrame::VideoNeeded { main, middle } = frame {
                told.push((Instant::now(), main, middle));
                if sending[0] && !main {
                    off_tick = Some(ticks);
                }
                if !sending[0] && main {
                    back_tick = Some(ticks);
                    layers[0].want_key = false;
                }
                sending = [main, middle];
            }
        }
        for (client, id, rx, offers) in [
            (&mut b, &b_id, &mut b_rx, &mut b_offers),
            (&mut c, &c_id, &mut c_rx, &mut c_offers),
        ] {
            if !client.is_connected() {
                continue;
            }
            while let Ok(frame) = rx.try_recv() {
                if let ServerFrame::SdpOffer { sdp } = frame {
                    let answer = client.answer_renegotiation(&sdp).expect("answers");
                    media_task
                        .accept_renegotiation_answer(id.clone(), answer)
                        .await
                        .expect("the SFU takes the answer");
                    offers.push(sdp);
                }
            }
        }
        // The late joiner, once it has joined.
        if d_joined_at.is_some() {
            let _ = d.drain().await;
            if d.is_connected() {
                while let Ok(frame) = d_rx.try_recv() {
                    if let ServerFrame::SdpOffer { sdp } = frame {
                        let answer = d.answer_renegotiation(&sdp).expect("answers");
                        media_task
                            .accept_renegotiation_answer(d_id.clone(), answer)
                            .await
                            .expect("the SFU takes the answer");
                        d_offers.push(sdp);
                    }
                }
            }
            let say = |quality: VideoQuality| {
                media_task
                    .set_video_wants(
                        d_id.clone(),
                        vec![VideoWant {
                            peer: a_id.to_string(),
                            quality,
                        }],
                    )
                    .expect("the media task takes the wants");
            };
            if d_said_light.is_none() && !d_offers.is_empty() {
                say(VideoQuality::Light);
                d_said_light = Some(Instant::now());
            }
            if d_said_middle.is_none()
                && d_said_light.is_some_and(|at| at.elapsed() >= JOINER_SAYS_MIDDLE_AFTER)
            {
                say(VideoQuality::Middle);
                d_said_middle = Some(Instant::now());
            }
            while let Ok((len, src)) = d.socket.try_recv_from(&mut dbuf) {
                d.feed_receive(&dbuf[..len], src, sfu_addr);
                let _ = d.drain().await;
            }
            d.feed_timeout();
        }
        let tracks_of = |offers: &[String]| {
            let mut mids: Vec<Mid> = Vec::new();
            for (mid, stream) in offers.iter().flat_map(|sdp| video_msids(sdp)) {
                if stream.starts_with(&a_id.to_string()) && !mids.contains(&mid) {
                    mids.push(mid);
                }
            }
            mids.len()
        };
        if t0.is_none()
            && a.is_connected()
            && b.is_connected()
            && c.is_connected()
            && tracks_of(&b_offers) >= 3
            && tracks_of(&c_offers) >= 3
        {
            let mut api = a.rtc.direct_api();
            let ready = layers.iter().all(|layer| {
                layer
                    .mid
                    .and_then(|mid| api.stream_tx_by_mid(mid, None))
                    .is_some_and(|stream| stream.rtx().is_some())
            });
            if ready {
                t0 = Some(Instant::now() + Duration::from_millis(50));
            }
        }
        if let Some(start) = t0 {
            let due = start + frame_every * (ticks as u32);
            if finish_at.is_none() && Instant::now() >= due {
                if ticks == TO_MIDDLE {
                    for id in [&b_id, &c_id] {
                        media_task
                            .set_video_wants(
                                id.clone(),
                                vec![VideoWant {
                                    peer: a_id.to_string(),
                                    quality: VideoQuality::Middle,
                                }],
                            )
                            .expect("the media task takes the wants");
                    }
                    to_middle_at = Some(Instant::now());
                }
                if d_joined_at.is_none() && off_tick.is_some_and(|off| ticks == off + JOIN_FRAMES) {
                    let (offer, pending) = d.create_video_subscriber_offer();
                    let answer = media_task
                        .accept_offer(d_id.clone(), call.to_owned(), offer)
                        .await
                        .expect("SFU accepts the joiner's offer");
                    d.accept_answer(pending, &answer)
                        .expect("the joiner applies the SFU's answer");
                    d_joined_at = Some(Instant::now());
                }
                if to_main_at.is_none() && off_tick.is_some_and(|off| ticks == off + OFF_FRAMES) {
                    media_task
                        .set_video_wants(
                            b_id.clone(),
                            vec![VideoWant {
                                peer: a_id.to_string(),
                                quality: VideoQuality::Main,
                            }],
                        )
                        .expect("the media task takes the wants");
                    to_main_at = Some(Instant::now());
                }
                for (which, layer) in layers.iter_mut().enumerate() {
                    // The light sender is always sent.
                    if which < 2 && !sending[which] {
                        continue;
                    }
                    let frame = layer.frames.len() as u64;
                    let key = std::mem::take(&mut layer.want_key);
                    let n = if key {
                        layer.packets * FB_KEY_FACTOR
                    } else {
                        layer.packets
                    };
                    let rtp_time = 90_000 + (ticks * (90_000 / DIP_FPS)) as u32;
                    let written = Instant::now();
                    for i in 0..n {
                        let mut ext = str0m::rtp::ExtensionValues::default();
                        ext.user_values
                            .set(PacketDescriptor(fb_descriptor(frame, key, i, n)));
                        let seq = layer.next_seq;
                        layer.next_seq += 1;
                        let mut api = a.rtc.direct_api();
                        let stream = api
                            .stream_tx_by_mid(layer.mid.expect("offered"), None)
                            .expect("send stream");
                        stream
                            .write_rtp(
                                UPLINK_VP8_PT.into(),
                                seq.into(),
                                rtp_time,
                                written,
                                i + 1 == n,
                                ext,
                                true,
                                fb_payload(layer.tag, frame, i, layer.payload),
                            )
                            .expect("A writes a packet");
                    }
                    layer.frames.push(FbFrame {
                        key,
                        n,
                        at: written,
                    });
                }
                ticks += 1;
                if back_tick.is_some_and(|back| ticks >= back + BACK_FRAMES) {
                    finish_at = Some(Instant::now() + Duration::from_millis(700));
                }
                a.feed_timeout();
                let _ = a.drain().await;
            }
        }
        for (client, buf) in [&mut a, &mut b, &mut c].into_iter().zip(bufs.iter_mut()) {
            while let Ok((len, src)) = client.socket.try_recv_from(buf) {
                client.feed_receive(&buf[..len], src, sfu_addr);
                let _ = client.drain().await;
            }
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
        for client in [&mut a, &mut b, &mut c] {
            let _ = client.drain().await;
            client.feed_timeout();
        }
    }
    let start = t0.expect("the publisher streamed");
    let end = Instant::now();
    let to_middle_at = to_middle_at.expect("the middle picture was asked for");
    let to_main_at = to_main_at.expect("the main picture was asked for");
    let ms = |at: Instant| at.saturating_duration_since(start).as_millis();
    eprintln!(
        "video-needed: the publisher was told (ms from the first frame, main, middle): {:?}; \
         both wanted middle at {} ms, one wanted main at {} ms",
        told.iter()
            .map(|(at, m, d)| (ms(*at), *m, *d))
            .collect::<Vec<_>>(),
        ms(to_middle_at),
        ms(to_main_at),
    );

    // The publisher was also told how much its senders may send (the SFU
    // decides its bitrate here): at least once, over the same socket, and
    // every budget within the rule's floors and plan maxima.
    eprintln!(
        "publish-budget: the publisher was told (ms from the first frame, main kbit/s, middle \
         kbit/s): {:?}",
        budgets
            .iter()
            .map(|(at, m, d)| (ms(*at), *m, *d))
            .collect::<Vec<_>>(),
    );
    assert!(
        !budgets.is_empty(),
        "a publisher under server-side bitrate control got no publish-budget frame"
    );
    for (_, main_kbps, middle_kbps) in &budgets {
        use lakko_svc_sfu::publish_budget::{
            MAIN_FLOOR_BPS, MAIN_PLAN_BPS, MIDDLE_FLOOR_BPS, MIDDLE_PLAN_BPS,
        };
        let main = u64::from(*main_kbps) * 1000;
        let middle = u64::from(*middle_kbps) * 1000;
        assert!((MAIN_FLOOR_BPS..=MAIN_PLAN_BPS).contains(&main), "{main}");
        assert!(
            (MIDDLE_FLOOR_BPS..=MIDDLE_PLAN_BPS).contains(&middle),
            "{middle}"
        );
    }

    // What the publisher was told, and when.
    assert_eq!(
        told.iter().map(|(_, m, d)| (*m, *d)).collect::<Vec<_>>(),
        vec![(true, true), (false, true), (true, true)],
        "everything on first, main off after the delay, main on again"
    );
    let (off_at, on_at) = (told[1].0, told[2].0);
    // Nobody ever named main: a subscriber that has said nothing counts
    // for nothing heavy. So main has been unwanted since the publisher was
    // first counted, which is a moment before this test read the first
    // frame off its channel (hence the slack below the delay).
    let waited = off_at.saturating_duration_since(told[0].0);
    assert!(
        waited + Duration::from_millis(500) >= UNNEEDED_AFTER
            && waited < UNNEEDED_AFTER + Duration::from_secs(2),
        "off is late, by the stated delay: {waited:?}"
    );

    // The late joiner: it joined while main was off, said light and then
    // middle, and the publisher was told nothing because of it (the only
    // `main: true` after the first is the one a subscriber asked for,
    // checked just below: it came after that request).
    let d_joined_at = d_joined_at.expect("the third subscriber joined");
    let d_said_light = d_said_light.expect("it was offered the tracks and said light");
    let d_said_middle = d_said_middle.expect("and then middle");
    assert!(d_joined_at > off_at && d_said_middle < to_main_at);
    let silent_for = to_main_at.saturating_duration_since(d_said_light);
    assert!(
        silent_for > Duration::from_secs(3),
        "main stayed off for seconds after it joined: {silent_for:?}"
    );
    eprintln!(
        "video-needed: a third subscriber joined {} ms after main went off, said light {} ms \
         and middle {} ms after joining; main stayed off for {silent_for:?} more",
        d_joined_at.saturating_duration_since(off_at).as_millis(),
        d_said_light
            .saturating_duration_since(d_joined_at)
            .as_millis(),
        d_said_middle
            .saturating_duration_since(d_joined_at)
            .as_millis(),
    );
    let told_after = on_at.saturating_duration_since(to_main_at);
    assert!(
        told_after <= NEEDED_TOLD_WITHIN,
        "on is at once: told {told_after:?} after the request"
    );
    assert!(on_at >= to_main_at);

    // The publisher's own side: what it wrote on its main sender.
    let main_frames = &layers[0].frames;
    let last_before = main_frames
        .iter()
        .rposition(|f| f.at < off_at)
        .expect("main frames before it was switched off");
    let first_after = last_before + 1;
    let silent = main_frames[first_after]
        .at
        .saturating_duration_since(main_frames[last_before].at);
    assert!(
        main_frames[first_after].at >= on_at && silent > Duration::from_secs(2),
        "the publisher wrote nothing on its main sender while it was off: {silent:?}"
    );
    assert!(
        !main_frames[first_after].key,
        "it came back with a delta frame, the harder case"
    );
    let key_after = main_frames[first_after..]
        .iter()
        .position(|f| f.key)
        .map(|i| first_after + i)
        .expect("a key frame after it was switched back on");

    // Nothing was asked of the sender while it was off.
    let (main_ssrc, _) = a.send_ssrcs(pub_main).expect("the main send stream");
    let quiet_from = off_at + Duration::from_millis(500);
    let nacked = a
        .nacks_seen
        .iter()
        .filter(|(at, ssrc, _)| *ssrc == main_ssrc && *at >= quiet_from)
        .count();
    assert_eq!(
        nacked, 0,
        "no retransmission is asked of a sender that is off, or of its return"
    );
    let asked = |mid: Mid, from: Instant, until: Instant| {
        a.keyframe_request_mids
            .iter()
            .zip(&a.keyframe_request_at)
            .filter(|(m, at)| **m == mid && **at >= from && **at < until)
            .count()
    };
    assert_eq!(
        asked(pub_main, quiet_from, on_at),
        0,
        "no key frame is asked of a sender that is off"
    );
    assert!(
        asked(pub_main, on_at, end) >= 1,
        "the SFU asked for a key frame when the sender came back with a delta frame"
    );
    assert!(
        asked(pub_main, on_at, end) <= 3,
        "and not again and again: {}",
        asked(pub_main, on_at, end)
    );

    // The subscribers' tracks.
    let main_stream = a_id.to_string();
    let middle_stream = format!("{a_id}{MIDDLE_STREAM_SUFFIX}");
    let light_stream = format!("{a_id}{LIGHT_STREAM_SUFFIX}");
    let mids_of = |offers: &[String]| -> (Mid, Mid, Mid) {
        let find = |stream: &str| {
            offers
                .iter()
                .flat_map(|sdp| video_msids(sdp))
                .find(|(_, s)| s == stream)
                .map(|(mid, _)| mid)
                .expect("the track was offered")
        };
        (
            find(&main_stream),
            find(&middle_stream),
            find(&light_stream),
        )
    };
    let on = |client: &SyntheticClient, track: Mid, layer: &FbLayer| -> Vec<WantsGot> {
        client
            .rtp_got
            .iter()
            .zip(&client.rtp_got_at)
            .zip(&client.rtp_got_track)
            .filter(|(_, t)| **t == Some(track))
            .map(|((got, at), _)| {
                let pl = &got.payload;
                assert!(pl.len() >= 7, "a media packet of this test");
                assert_eq!(pl[0], layer.tag, "a packet on the track of another sender");
                let frame = u64::from(u32::from_be_bytes([pl[1], pl[2], pl[3], pl[4]]));
                let i = u64::from(u16::from_be_bytes([pl[5], pl[6]]));
                assert_eq!(
                    *pl,
                    fb_payload(layer.tag, frame, i, layer.payload),
                    "the payload arrives byte for byte as the publisher wrote it"
                );
                WantsGot {
                    at: *at,
                    seq: got.seq,
                    frame,
                    i,
                }
            })
            .collect()
    };
    let count = |got: &[WantsGot], from: Instant, until: Instant| {
        got.iter().filter(|p| p.at >= from && p.at < until).count()
    };
    let (b_main_mid, b_middle_mid, b_light_mid) = mids_of(&b_offers);
    let (c_main_mid, c_middle_mid, c_light_mid) = mids_of(&c_offers);
    let b_main = on(&b, b_main_mid, &layers[0]);
    let b_middle = on(&b, b_middle_mid, &layers[1]);
    let b_light = on(&b, b_light_mid, &layers[2]);
    let c_main = on(&c, c_main_mid, &layers[0]);
    let c_middle = on(&c, c_middle_mid, &layers[1]);
    let c_light = on(&c, c_light_mid, &layers[2]);
    assert_eq!(
        b_main.len() + b_middle.len() + b_light.len(),
        b.rtp_got.len(),
        "nothing arrived anywhere else"
    );
    assert_eq!(
        c_main.len() + c_middle.len() + c_light.len(),
        c.rtp_got.len()
    );

    // While the main sender was off both showed the middle picture, and
    // the light one flowed.
    for (who, middle, light) in [("B", &b_middle, &b_light), ("C", &c_middle, &c_light)] {
        assert!(
            count(middle, off_at, to_main_at) > 100,
            "{who}'s middle picture flows while the main sender is off"
        );
        assert!(count(light, off_at, to_main_at) > 40, "{who}'s light too");
    }
    // The late joiner is sent the middle and the light picture, and never
    // a packet of the main one.
    let (d_main_mid, d_middle_mid, d_light_mid) = mids_of(&d_offers);
    let d_main = on(&d, d_main_mid, &layers[0]);
    let d_middle = on(&d, d_middle_mid, &layers[1]);
    let d_light = on(&d, d_light_mid, &layers[2]);
    assert_eq!(
        d_main.len() + d_middle.len() + d_light.len(),
        d.rtp_got.len()
    );
    assert_eq!(d_main.len(), 0, "the joiner is never sent the main picture");
    assert!(d_middle.len() > 100, "its middle picture flows");
    assert!(d_light.len() > 40, "and its light one");
    let first = d_middle.first().expect("checked");
    assert!(
        layers[1].frames[usize::try_from(first.frame).expect("fits")].key && first.i == 0,
        "its middle track begins at the first packet of a key frame"
    );

    // The one that never asked for main is sent none after it went to
    // the middle picture.
    assert_eq!(count(&c_main, to_middle_at + IN_FLIGHT, end), 0);
    assert!(count(&c_middle, on_at, end) > 100, "C's middle flows on");

    // The one that asked: its main track starts at the first packet of
    // the key frame the SFU asked for, within the stated time.
    let resumed = b_main
        .iter()
        .find(|p| p.at >= to_main_at)
        .expect("B's main track resumed");
    assert_eq!(
        (resumed.frame, resumed.i),
        (key_after as u64, 0),
        "B's main track starts at the first packet of the key frame"
    );
    let back_after = resumed.at.saturating_duration_since(to_main_at);
    eprintln!(
        "video-needed: told {told_after:?} after the request, first packet of the key frame \
         at the subscriber {back_after:?} after it; {} delta frames went by first",
        key_after - first_after
    );
    assert!(
        back_after <= NEEDED_BACK_WITHIN,
        "back within the stated time: {back_after:?}"
    );
    assert!(count(&b_main, on_at, end) > 200, "and flows from there");
    assert_eq!(
        count(&b_middle, to_main_at + IN_FLIGHT, end),
        0,
        "B's middle track stops"
    );
    // No sequence gap anywhere on it, and no retransmission was sent.
    for pair in b_main.windows(2) {
        assert_eq!(
            pair[1].seq,
            pair[0].seq.wrapping_add(1),
            "B's main track runs on consecutive sequence numbers (frames {} and {})",
            pair[0].frame,
            pair[1].frame
        );
    }
    let b_main_rtx = {
        let mut api = b.rtc.direct_api();
        let stream = api.stream_rx_by_mid(b_main_mid, None).expect("the stream");
        stream.rtx().map(|rtx| *rtx)
    };
    let b_main_rtx = b_main_rtx.expect("the main track has a retransmission stream");
    assert_eq!(
        b.rtp_in.iter().filter(|p| p.ssrc == b_main_rtx).count(),
        0,
        "B asked for no retransmission on its main track and was sent none"
    );

    // The publisher's bitrate: what it is allowed after its sender is
    // back is not less than before the sender was switched off.
    let before = a
        .rembs_seen
        .iter()
        .rev()
        .find(|(at, _, _)| *at <= off_at)
        .map(|(_, bps, _)| *bps)
        .expect("a REMB before the sender was switched off");
    // Counted from the subscriber's request, not from the moment this test
    // read `main: true` off its channel: the REMB the SFU sends with that
    // frame can reach the publisher's socket first.
    let (after_at, after, _) = a
        .rembs_seen
        .iter()
        .find(|(at, _, _)| *at >= to_main_at)
        .cloned()
        .expect("a REMB after the sender was switched back on");
    let lowest_idle = a
        .rembs_seen
        .iter()
        .filter(|(at, _, _)| *at >= off_at && *at < on_at)
        .map(|(_, bps, _)| *bps)
        .min()
        .expect("REMBs while it was off");
    eprintln!(
        "video-needed: REMBs around the request for main, ms relative to it: {:?}",
        a.rembs_seen
            .iter()
            .map(|(at, _, _)| if *at >= to_main_at {
                i64::try_from(at.duration_since(to_main_at).as_millis()).unwrap_or(i64::MAX)
            } else {
                -i64::try_from(to_main_at.duration_since(*at).as_millis()).unwrap_or(i64::MAX)
            })
            .filter(|ms| ms.abs() < 1500)
            .collect::<Vec<_>>()
    );
    eprintln!(
        "video-needed: REMB before the sender went off {before}, lowest while off {lowest_idle}, \
         first after main was asked for {after} ({:?} after)",
        after_at.saturating_duration_since(to_main_at)
    );
    assert!(
        after >= before,
        "the REMB after resuming ({after}) is not lower than before ({before})"
    );
    assert!(
        after_at.saturating_duration_since(to_main_at) <= Duration::from_millis(250),
        "and it comes at once"
    );
    assert!(
        lowest_idle >= before,
        "an idle sender lowered the estimate: {lowest_idle} < {before}"
    );
}

// ============================================================
// ADR-0070 step 6: the SFU estimates a subscriber's downlink and
// gives up the larger pictures first
// ============================================================

/// A link of a given rate with a drop-tail queue, between a client's
/// socket and the client: a token bucket one packet deep. A datagram that
/// arrives while the queue is full is lost; every other datagram leaves
/// when the ones before it have, at the link's rate. With no rate the
/// link passes everything at once, in order.
struct RateLimitedLink {
    /// Added to every datagram's way after the bottleneck.
    delay: Duration,
    /// And up to this much more, drawn per datagram; the order of the
    /// datagrams is kept.
    jitter: Duration,
    /// State of the generator the jitter is drawn from, so a run repeats.
    rng: u64,
    /// When the datagram last handed over comes out of the delay.
    last_out: Instant,
    /// Bits per second; `None` passes everything.
    rate_bps: Option<u64>,
    /// Most bytes waiting.
    queue_limit: usize,
    queue: std::collections::VecDeque<(Instant, Vec<u8>, SocketAddr)>,
    queued_bytes: usize,
    /// When the datagram last taken leaves.
    next_free: Instant,
    /// Media datagrams lost at the full queue: when, and their SSRC.
    dropped: Vec<(Instant, u32)>,
    /// Every media datagram that reached the link: when, and its SSRC.
    entered: Vec<(Instant, u32)>,
    /// Media datagrams that left the link: when they left, SSRC, bytes.
    passed: Vec<(Instant, u32, usize)>,
    /// A mobile-like path in place of the fixed rate, and when its clock
    /// started ([`MobileLink`]).
    mobile: Option<(Instant, MobileLink)>,
}

/// A downlink that behaves like the mobile link of the live call of
/// 2026-10-09 (two browsers on one laptop sharing one mobile link), as
/// far as the SFU's log of that call shows it. **Every figure is chosen**
/// to resemble that log, none is measured on a mobile network:
///
/// - the path carries `capacity_bps` when nobody else is on it;
/// - its delay moves by itself: twice a second it climbs by
///   `wave` over a tenth of a second and falls back over the next (the
///   delay detector spoke of over-use 282 times in six and a half
///   minutes on that call with no queue standing);
/// - at each of `dips` the path carries `dip_bps` for `dip_for` and then
///   all of it again (both directions of the mobile link showed a queue
///   of 400 to 600 ms in the same second, five times in two minutes, and
///   what was acknowledged then was a few hundred kbit/s);
/// - from `shared_from` until `shared_until` a competing flow comes and
///   goes: for `shared_on` the path carries `shared_bps` for this
///   subscriber, then for `shared_off` all of it again (the other browser
///   on the same link, whose own pictures came and went).
#[derive(Clone, Copy)]
struct MobileLink {
    capacity_bps: u64,
    wave: Duration,
    dips: [Duration; 3],
    dip_for: Duration,
    dip_bps: u64,
    shared_from: Duration,
    shared_until: Duration,
    shared_on: Duration,
    shared_off: Duration,
    shared_bps: u64,
}

impl MobileLink {
    /// The rate the path carries for this subscriber at `at`.
    fn rate_at(&self, t0: Instant, at: Instant) -> u64 {
        let t = at.saturating_duration_since(t0);
        if self
            .dips
            .iter()
            .any(|from| t >= *from && t < *from + self.dip_for)
        {
            return self.dip_bps;
        }
        if t >= self.shared_from && t < self.shared_until {
            let cycle = (self.shared_on + self.shared_off).as_millis().max(1);
            let into = t.saturating_sub(self.shared_from).as_millis() % cycle;
            if into < self.shared_on.as_millis() {
                return self.shared_bps;
            }
        }
        self.capacity_bps
    }

    /// The path's own delay at `at`, beyond its smallest.
    fn wave_at(&self, t0: Instant, at: Instant) -> Duration {
        let ms = at.saturating_duration_since(t0).as_millis() % 500;
        let part = match ms {
            0..=99 => ms,
            100..=199 => 200 - ms,
            _ => 0,
        };
        self.wave * u32::try_from(part).unwrap_or(0) / 100
    }
}

impl RateLimitedLink {
    fn new(rate_bps: Option<u64>, queue_limit: usize) -> Self {
        Self {
            delay: Duration::ZERO,
            jitter: Duration::ZERO,
            rng: 0x5eed,
            last_out: Instant::now(),
            rate_bps,
            queue_limit,
            queue: std::collections::VecDeque::new(),
            queued_bytes: 0,
            next_free: Instant::now(),
            dropped: Vec::new(),
            entered: Vec::new(),
            passed: Vec::new(),
            mobile: None,
        }
    }

    /// A datagram reaches the link at `at`.
    fn arrive(&mut self, at: Instant, bytes: &[u8], source: SocketAddr) {
        if let Some(rtp) = rtp_seen(bytes) {
            self.entered.push((at, rtp.ssrc));
        }
        let starts = at.max(self.next_free);
        // A mobile-like path: the rate is the one of the moment the
        // datagram is served.
        let rate_bps = match self.mobile {
            Some((t0, mobile)) if at >= t0 => Some(mobile.rate_at(t0, starts)),
            _ => self.rate_bps,
        };
        let leaves = match rate_bps {
            None => starts,
            Some(rate) => {
                // What waits at the bottleneck is what it has not yet
                // sent: the time until it is free, at its rate. (The
                // delay after it holds datagrams too, and those are not
                // in its queue.)
                #[allow(
                    clippy::cast_precision_loss,
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss
                )]
                let waiting = (self.next_free.saturating_duration_since(at).as_secs_f64()
                    * rate as f64
                    / 8.0) as usize;
                if waiting + bytes.len() > self.queue_limit {
                    if let Some(rtp) = rtp_seen(bytes) {
                        self.dropped.push((at, rtp.ssrc));
                    }
                    return;
                }
                #[allow(clippy::cast_precision_loss)]
                let secs = (bytes.len() * 8) as f64 / rate as f64;
                starts + Duration::from_secs_f64(secs)
            },
        };
        self.next_free = leaves;
        self.queued_bytes += bytes.len();
        let mut out = leaves + self.delay;
        if let Some((t0, mobile)) = self.mobile {
            out += mobile.wave_at(t0, leaves);
        }
        if !self.jitter.is_zero() {
            self.rng = self.rng.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.rng;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^= z >> 31;
            let micros = u64::try_from(self.jitter.as_micros()).unwrap_or(1).max(1);
            out += Duration::from_micros(z % micros);
        }
        let out = out.max(self.last_out);
        self.last_out = out;
        self.queue.push_back((out, bytes.to_vec(), source));
    }

    /// The next datagram that has left the link by `now`, with when.
    fn leave(&mut self, now: Instant) -> Option<(Instant, Vec<u8>, SocketAddr)> {
        if self.queue.front().is_some_and(|(at, _, _)| *at <= now) {
            let (at, bytes, source) = self.queue.pop_front()?;
            self.queued_bytes -= bytes.len();
            if let Some(rtp) = rtp_seen(&bytes) {
                self.passed.push((at, rtp.ssrc, bytes.len()));
            }
            return Some((at, bytes, source));
        }
        None
    }
}

/// Which of a publisher's three tracks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DlRole {
    Main,
    Middle,
    Light,
}

/// One packet the subscriber of a downlink run was handed.
#[derive(Clone, Copy)]
struct DlGot {
    at: Instant,
    publisher: usize,
    role: DlRole,
    seq: u16,
}

/// One line of the timeline a downlink run prints: the SFU's own view of
/// the subscriber's downlink at a moment.
#[derive(Clone, Copy)]
struct DlSample {
    at: Instant,
    snap: lakko_svc_sfu::media::DownlinkSnapshot,
}

/// What a downlink run is.
#[derive(Clone, Copy)]
struct DownlinkCase {
    /// The subscriber's downlink rate, bits per second, from the start
    /// until `lift_after`; `None` is an unlimited link throughout.
    limit_bps: Option<u64>,
    /// Bytes the limited link queues.
    queue_limit: usize,
    /// How long after the publishers start the limit is lifted.
    lift_after: Duration,
    /// The run ends this long after the publishers start, or as soon as
    /// the subscriber has been given every main track again for
    /// `DL_STABLE` after the lift.
    run_for: Duration,
    /// `LAKKO_SFU_DOWNLINK_CONTROL`.
    downlink_control: bool,
    /// One-way delay and jitter added on the subscriber's downlink.
    delay: Duration,
    jitter: Duration,
    /// A dip on the subscriber's downlink: from this long after the
    /// publishers start, for a second, only 30% of the media datagrams
    /// arrive (ADR-0070's dip).
    dip_after: Option<Duration>,
    /// The first publisher's frames are withheld for [`DL_CLUMP`] from
    /// this long after the publishers start, and then written together:
    /// an uplink that stalled and delivered in a clump.
    clump_at: Option<Duration>,
    /// Every publisher's frames are withheld for [`DL_CLUMP`] from this
    /// long after the publishers start (what a stall of this test's own
    /// loop does, and three uplinks stalling at once).
    clump_all_at: Option<Duration>,
    /// And every publisher's once more this long after the limit is
    /// lifted.
    clump_after_lift: Option<Duration>,
    /// The subscriber asks for a key frame (PLI) on every video track it
    /// receives, five times a second for the whole run: a receiver that
    /// cannot decode what it is sent. And it wants main of the first
    /// publisher, middle of the second and light of the third, so that a
    /// track of every role is being sent.
    subscriber_keeps_asking: bool,
    /// And stops asking this long after the publishers start (it has a
    /// picture). `None`: it asks to the end.
    asking_stops_after: Option<Duration>,
    /// The publishers' packets carry no frame descriptor (Firefox
    /// negotiates none): the SFU cannot see a key frame.
    no_descriptor: bool,
    /// The subscriber's downlink is a [`MobileLink`] (in place of
    /// `limit_bps`, which is then not looked at), the subscriber asks for
    /// the **middle** picture of all three, as a tile of a four-person
    /// call does, each publisher's middle sender moves between 400 and
    /// 700 kbit/s as an encoder's does, and each publisher sends a key
    /// frame on its middle and light senders every thirty seconds (what
    /// the SFU's own request at the end of an uplink burst brings).
    mobile: Option<&'static MobileLink>,
}

/// What a downlink run observed.
struct DownlinkRun {
    /// When the publishers started.
    t0: Instant,
    /// When the limit was lifted, if it was.
    lifted_at: Option<Instant>,
    ended_at: Instant,
    /// The publishers' peer ids, in the order they joined.
    publishers: Vec<String>,
    /// Every `video-given` frame the subscriber received: when, and the
    /// set as (publisher index, quality).
    given: Vec<(Instant, Vec<(usize, lakko_svc_sfu::protocol::VideoQuality)>)>,
    /// The SFU's view, twice a second.
    samples: Vec<DlSample>,
    /// Everything the subscriber was handed.
    got: Vec<DlGot>,
    /// Media datagrams the limited link lost and passed, by track:
    /// (when, publisher, role, is the retransmission stream).
    link_dropped: Vec<(Instant, usize, DlRole, bool)>,
    /// Bytes that left the link on retransmission streams, and of those
    /// the bytes of padding-only packets ([`DL_PADDING_BELOW`]).
    /// Every media datagram the SFU sent toward the subscriber, as it
    /// reached the link: when, the kind of track, and whether it was on
    /// a retransmission stream (a resent packet, a second copy of a
    /// light packet, or probe padding).
    link_entered: Vec<(Instant, DlRole, bool)>,
    rtx_bytes: usize,
    padding_bytes: usize,
    media_bytes: usize,
    /// Media datagrams lost in the dip.
    dip_lost: usize,
    /// The longest the test's own loop went without a pass.
    worst_stall: Duration,
    /// For every frame the publishers wrote: when it was due, and how
    /// late the test's loop wrote it.
    tick_late: Vec<(Instant, Duration)>,
    /// When each withholding of the first publisher's frames began.
    clumps: Vec<Instant>,
    /// Key frame requests each publisher received on its main, middle
    /// and light sender.
    keyframe_requests: Vec<[usize; 3]>,
    /// When each arrived: (publisher, 0 main / 1 middle / 2 light, time).
    keyframe_request_times: Vec<(usize, usize, Instant)>,
    /// The SFU's answer to each publisher's offer, and to the
    /// subscriber's own; the SFU's offers to the subscriber.
    publisher_answers: Vec<String>,
    subscriber_answer: String,
    sfu_offers: Vec<String>,
}

/// Packets per delta frame and payload bytes per packet of a main sender
/// in a downlink run: 14 x 1100 bytes at 25 frames a second is 3.1 Mbit/s.
const DL_MAIN_PACKETS: u64 = 14;
const DL_MAIN_PAYLOAD: usize = FB_F_PAYLOAD;

/// A datagram on a retransmission stream shorter than this is padding and
/// nothing else: 255 bytes of padding, RTP header, header extensions and
/// the SRTP tag come to less; the smallest media payload of a downlink
/// run, 375 bytes, to more.
const DL_PADDING_BELOW: usize = 340;

/// How long after the lift every main track must have been given again
/// without a break before a run ends early.
const DL_STABLE: Duration = Duration::from_secs(3);

/// A run in which the test's own loop stood still for longer than this is
/// not a measurement of the SFU: the publishers, the link model and the
/// subscriber all live in that loop, and a stall of it is a burst and a
/// blackout the scenario does not have. It happens when the machine is
/// loaded, as when this file's tests all run at once.
///
/// Chosen from what was seen: on an idle machine the loop's longest stall
/// was 28 to 179 ms over some thirty runs (the long ones in the first second of a
/// narrow link, while everything is being sent and asked for again);
/// with all of this file's tests running at once it stood still for
/// whole seconds.
const DL_WORST_STALL: Duration = Duration::from_millis(250);
/// How long a publisher's frames are withheld in a clump case.
const DL_CLUMP: Duration = Duration::from_millis(200);
/// How often a stalled run is repeated before the test gives up.
const DL_ATTEMPTS: u32 = 3;

/// One downlink run at a time in this process: each is three publishers,
/// a link model and a subscriber in one loop beside the SFU's media
/// task, and two of them at once make each other's publishers late.
static DL_ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Ends the test process when a downlink test has not ended by itself:
/// a thread on the wall clock, so that it also fires when the test's own
/// runtime is stuck in a wait, or when the machine slept in the middle
/// of a run (the monotonic clock every deadline inside the run uses does
/// not move while it sleeps; one run was found "running" 2 h 13 min
/// after it started).
struct DlWatchdog(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl DlWatchdog {
    #[allow(clippy::print_stderr)]
    fn start(what: &str, limit: Duration) -> Self {
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = std::sync::Arc::clone(&done);
        let what = what.to_owned();
        let started = std::time::SystemTime::now();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(1));
            if seen.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            if started.elapsed().is_ok_and(|elapsed| elapsed > limit) {
                eprintln!(
                    "{what}: the test did not end within {limit:?} by the wall clock; ending \
                     the process"
                );
                std::process::abort();
            }
        });
        Self(done)
    }
}

impl Drop for DlWatchdog {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// [`run_downlink`], repeated while the test's own loop stalled
/// ([`DL_WORST_STALL`]). Every attempt's timeline is printed. Panics
/// when no attempt ran without a stall: nothing was measured.
#[allow(clippy::print_stderr)]
async fn run_downlink_measured(what: &str, case: DownlinkCase) -> DownlinkRun {
    // One attempt ends by itself `run_for` after its publishers start,
    // and gives up a minute after that at the latest; anything longer is
    // a wait that will not end.
    let attempt_limit = case.run_for + Duration::from_secs(90);
    let _watchdog = DlWatchdog::start(what, attempt_limit * DL_ATTEMPTS);
    for attempt in 1..=DL_ATTEMPTS {
        let run = {
            let _one = DL_ONE_AT_A_TIME.lock().await;
            let ended = tokio::time::timeout(attempt_limit, Box::pin(run_downlink(case))).await;
            assert!(
                ended.is_ok(),
                "{what}: attempt {attempt} did not end within {attempt_limit:?}: the run was \
                 waiting on the SFU's media task or on a client and never got back to its own \
                 loop"
            );
            ended.expect("just checked")
        };
        if run.worst_stall <= DL_WORST_STALL {
            eprintln!(
                "{what}: attempt {attempt} is the measurement ({} discarded before it)",
                attempt - 1
            );
            return run;
        }
        print_downlink_timeline(what, &run);
        eprintln!(
            "{what}: attempt {attempt} is not a measurement: the test's own loop stood still \
             for {} ms",
            run.worst_stall.as_millis()
        );
    }
    panic!("{what}: the machine was too loaded to measure in {DL_ATTEMPTS} attempts");
}

/// Three publishers, each with three declared senders (main 3.1 Mbit/s,
/// middle 500 kbit/s, light 150 kbit/s, at 25 frames a second), and one
/// subscriber that asks for the main picture of all three, through the
/// production media task under server-side bitrate control. The
/// subscriber's downlink is a [`RateLimitedLink`]. A publisher answers a
/// keyframe request with a key frame on that sender, as an encoder does;
/// it does not follow the SFU's REMB.
#[allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss
)]
async fn run_downlink(case: DownlinkCase) -> DownlinkRun {
    use lakko_svc_sfu::frame_descriptor::PacketDescriptor;
    use lakko_svc_sfu::media::{LIGHT_STREAM_SUFFIX, MIDDLE_STREAM_SUFFIX};
    use lakko_svc_sfu::protocol::{VideoQuality, VideoWant};

    const PUBLISHERS: usize = 3;

    struct Publisher {
        client: SyntheticClient,
        id: PeerId,
        layers: [FbLayer; 3],
        plis_handled: usize,
        /// The next frame this publisher has not written yet.
        next_tick: u64,
    }

    // `LAKKO_SFU_E2E_LOG=1` prints the SFU's own log lines of the run.
    if std::env::var_os("LAKKO_SFU_E2E_LOG").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_timer(tracing_subscriber::fmt::time::uptime())
            .with_writer(std::io::stderr)
            .try_init();
    }
    let call = "downlink-control-call";
    let peers = PeerRegistry::new();
    let mut registered = register_subscribers(&peers, call, 1).await;
    let (b_id, mut b_rx) = registered.pop().expect("one registered");
    let (media_task, sfu_addr) = spawn_media_task_with_bitrate_control(
        "127.0.0.1:0".parse().unwrap(),
        None,
        peers,
        MediaTaskOptions {
            diagnostics: None,
            video_fec: false,
            uplink_fallback: false,
            protection: LossProtection::default(),
            downlink_control: case.downlink_control,
        },
        BitrateControl::Server(EstimatorConfig::default()),
    )
    .await
    .expect("UDP bind on loopback:0 succeeds");

    let rtp_client = || {
        Rtc::builder().set_rtp_mode(true).set_extension(
            GENERIC_FRAME_DESCRIPTOR_ID,
            generic_frame_descriptor_extension(),
        )
    };
    let mut b = SyntheticClient::build(rtp_client()).await;
    let (b_offer, b_pending) = b.create_video_subscriber_offer();
    let subscriber_answer = media_task
        .accept_offer(b_id.clone(), call.to_owned(), b_offer)
        .await
        .expect("SFU accepts B's subscriber offer");
    b.accept_answer(b_pending, &subscriber_answer)
        .expect("B applies the SFU's answer");

    let mut publishers: Vec<Publisher> = Vec::new();
    let mut publisher_answers = Vec::new();
    for _ in 0..PUBLISHERS {
        let mut client = SyntheticClient::build(rtp_client()).await;
        let id = fresh_peer_id();
        let (offer, pending, main, light, middle) = client.create_three_sender_publisher_offer();
        let answer = media_task
            .accept_offer_with_copies(
                id.clone(),
                call.to_owned(),
                offer,
                vec![light.to_string()],
                vec![middle.to_string()],
            )
            .await
            .expect("SFU accepts a publisher's offer");
        client
            .accept_answer(pending, &answer)
            .expect("the publisher applies the SFU's answer");
        publisher_answers.push(answer);
        let layer = |mid: Mid, tag: u8, packets: u64, payload: usize, first_seq: u64| FbLayer {
            rid: None,
            mid: Some(mid),
            tag,
            packets,
            payload,
            next_seq: first_seq,
            frames: Vec::new(),
            want_key: true,
        };
        publishers.push(Publisher {
            client,
            id,
            layers: [
                layer(main, b'f', DL_MAIN_PACKETS, DL_MAIN_PAYLOAD, FB_F_FIRST_SEQ),
                layer(middle, b'm', 5, 500, 40_000),
                layer(light, b'q', FB_Q_PACKETS, FB_Q_PAYLOAD, FB_Q_FIRST_SEQ),
            ],
            plis_handled: 0,
            next_tick: 0,
        });
    }
    let names: Vec<String> = publishers.iter().map(|p| p.id.to_string()).collect();
    let index_of = |peer: &str| names.iter().position(|n| n == peer);

    let mut link = RateLimitedLink::new(case.limit_bps, case.queue_limit);
    link.delay = case.delay;
    link.jitter = case.jitter;
    let mut dip_lost = 0usize;
    // B's datagrams are taken off its socket by a task of their own and
    // stamped there: when a datagram reached the client is what its
    // transport-wide feedback reports, and it must not depend on how busy
    // this loop is.
    let (arrived_tx, mut arrived) = mpsc::unbounded_channel::<(Instant, Vec<u8>, SocketAddr)>();
    let b_socket = b.socket.clone();
    let receiver = tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        while let Ok((len, src)) = b_socket.recv_from(&mut buf).await {
            if arrived_tx
                .send((Instant::now(), buf[..len].to_vec(), src))
                .is_err()
            {
                break;
            }
        }
    });
    let frame_every = Duration::from_micros(1_000_000 / DIP_FPS);
    let give_up = Instant::now() + case.run_for + Duration::from_secs(60);
    let mut offers: Vec<String> = Vec::new();
    let mut tracks: Vec<(Mid, usize, DlRole)> = Vec::new();
    let mut t0: Option<Instant> = None;
    let mut ticks: u64 = 0;
    let mut lifted_at: Option<Instant> = None;
    let mut given: Vec<(Instant, Vec<(usize, VideoQuality)>)> = Vec::new();
    let mut samples: Vec<DlSample> = Vec::new();
    let mut next_sample = Instant::now();
    let mut got: Vec<DlGot> = Vec::new();
    let mut buf = [0u8; 2048];
    let mut last_pass: Option<Instant> = None;
    let mut worst_stall = Duration::ZERO;
    let mut tick_late: Vec<(Instant, Duration)> = Vec::new();
    let mut clumps: Vec<Instant> = Vec::new();
    let mut next_pli = Instant::now();

    loop {
        let now = Instant::now();
        // How long this loop (the publishers, the link and the subscriber
        // all live in it) went without a pass, once the publishers run.
        if t0.is_some_and(|start| now >= start) {
            if let Some(last) = last_pass {
                worst_stall = worst_stall.max(now.saturating_duration_since(last));
            }
            last_pass = Some(now);
        }
        assert!(
            now < give_up,
            "the run did not finish: ticks={ticks} b_connected={} offers={}",
            b.is_connected(),
            offers.len()
        );
        if let Some(start) = t0 {
            if lifted_at.is_none() && case.limit_bps.is_some() && now >= start + case.lift_after {
                link.rate_bps = None;
                lifted_at = Some(now);
            }
            if now >= start + case.run_for {
                break;
            }
            // Every main track given again and nothing else since: done.
            if let (Some(lifted), Some((at, set))) = (lifted_at, given.last()) {
                let all_main = set.len() == PUBLISHERS
                    && set
                        .iter()
                        .all(|(_, quality)| *quality == VideoQuality::Main);
                if all_main && now >= (*at).max(lifted) + DL_STABLE {
                    break;
                }
            }
        }

        for publisher in &mut publishers {
            let _ = publisher.client.drain().await;
            let client = &publisher.client;
            while publisher.plis_handled < client.keyframe_request_mids.len() {
                let on = client.keyframe_request_mids[publisher.plis_handled];
                publisher.plis_handled += 1;
                for layer in &mut publisher.layers {
                    if layer.mid == Some(on) {
                        layer.want_key = true;
                    }
                }
            }
        }
        let _ = b.drain().await;
        // What B was handed since the last pass, kept small.
        for ((packet, at), track) in b
            .rtp_got
            .drain(..)
            .zip(b.rtp_got_at.drain(..))
            .zip(b.rtp_got_track.drain(..))
        {
            let Some((_, publisher, role)) = tracks.iter().find(|(mid, _, _)| Some(*mid) == track)
            else {
                continue;
            };
            let tag = match role {
                DlRole::Main => b'f',
                DlRole::Middle => b'm',
                DlRole::Light => b'q',
            };
            assert_eq!(
                packet.payload.first(),
                Some(&tag),
                "a packet arrives on the track of its sender"
            );
            got.push(DlGot {
                at,
                publisher: *publisher,
                role: *role,
                seq: packet.seq,
            });
        }
        b.rtp_got_playout.clear();
        b.rtp_packets.clear();
        b.rtp_in.clear();
        dip_lost += b.dip_in_lost.len();
        b.dip_in_lost.clear();

        while let Ok(frame) = b_rx.try_recv() {
            match frame {
                ServerFrame::SdpOffer { sdp } => {
                    let answer = b.answer_renegotiation(&sdp).expect("B answers");
                    media_task
                        .accept_renegotiation_answer(b_id.clone(), answer)
                        .await
                        .expect("the SFU takes B's answer");
                    for (mid, stream) in video_msids(&sdp) {
                        if tracks.iter().any(|(m, _, _)| *m == mid) {
                            continue;
                        }
                        let (peer, role) =
                            if let Some(peer) = stream.strip_suffix(MIDDLE_STREAM_SUFFIX) {
                                (peer, DlRole::Middle)
                            } else if let Some(peer) = stream.strip_suffix(LIGHT_STREAM_SUFFIX) {
                                (peer, DlRole::Light)
                            } else {
                                (stream.as_str(), DlRole::Main)
                            };
                        if let Some(publisher) = index_of(peer) {
                            tracks.push((mid, publisher, role));
                        }
                    }
                    offers.push(sdp);
                },
                ServerFrame::VideoGiven { given: set } => {
                    let set = set
                        .iter()
                        .map(|entry| {
                            (
                                index_of(&entry.peer).expect("a publisher of this call"),
                                entry.quality,
                            )
                        })
                        .collect();
                    given.push((Instant::now(), set));
                },
                _ => {},
            }
        }

        if t0.is_none() && b.is_connected() && tracks.len() == PUBLISHERS * 3 {
            let ready = publishers.iter_mut().all(|publisher| {
                let connected = publisher.client.is_connected();
                let mut api = publisher.client.rtc.direct_api();
                connected
                    && publisher.layers.iter().all(|layer| {
                        layer
                            .mid
                            .and_then(|mid| api.stream_tx_by_mid(mid, None))
                            .is_some_and(|stream| stream.rtx().is_some())
                    })
            });
            if ready {
                // The subscriber shows the main picture of all three, the
                // first publisher being the most important.
                media_task
                    .set_video_wants(
                        b_id.clone(),
                        names
                            .iter()
                            .enumerate()
                            .map(|(index, peer)| VideoWant {
                                peer: peer.clone(),
                                quality: match (case.subscriber_keeps_asking, index) {
                                    _ if case.mobile.is_some() => VideoQuality::Middle,
                                    (true, 1) => VideoQuality::Middle,
                                    (true, 2) => VideoQuality::Light,
                                    _ => VideoQuality::Main,
                                },
                            })
                            .collect(),
                    )
                    .expect("the media task takes the wants");
                let start = Instant::now() + Duration::from_millis(50);
                t0 = Some(start);
                link.mobile = case.mobile.map(|mobile| (start, *mobile));
                if let Some(after) = case.dip_after {
                    b.dip_in = Some(DipLoss {
                        from: start + after,
                        until: start + after + Duration::from_secs(1),
                        p: 0.7,
                        rng: 0xd1b,
                    });
                }
            }
        }
        if let Some(start) = t0 {
            let due = start + frame_every * (ticks as u32);
            if Instant::now() >= due {
                tick_late.push((due, Instant::now().saturating_duration_since(due)));
                // The first publisher's uplink stalls: its frames are
                // withheld and then written together.
                let withheld = [
                    case.clump_at.map(|after| (start + after, false)),
                    case.clump_all_at.map(|after| (start + after, true)),
                    lifted_at
                        .zip(case.clump_after_lift)
                        .map(|(at, after)| (at + after, true)),
                ]
                .into_iter()
                .flatten()
                .find(|(from, _)| due >= *from && due < *from + DL_CLUMP);
                if let Some((from, _)) = withheld {
                    if !clumps.contains(&from) {
                        clumps.push(from);
                    }
                }
                for (index, publisher) in publishers.iter_mut().enumerate() {
                    if withheld.is_some_and(|(_, all)| all || index == 0) {
                        continue;
                    }
                    for tick in publisher.next_tick..=ticks {
                        // A mobile case: a key frame on the middle and the
                        // light sender every thirty seconds, each publisher
                        // ten seconds after the one before it.
                        let periodic_key = case.mobile.is_some()
                            && tick > 0
                            && (tick + 10 * DIP_FPS * index as u64) % (30 * DIP_FPS) == 0;
                        for layer in &mut publisher.layers {
                            let frame = layer.frames.len() as u64;
                            if periodic_key && layer.tag != b'f' {
                                layer.want_key = true;
                            }
                            let key = std::mem::take(&mut layer.want_key);
                            // And its middle sender's rate moves: four to
                            // seven packets a frame, a step every eight
                            // seconds, each publisher one step behind the
                            // one before it (the three together 1.4 to
                            // 1.9 Mbit/s).
                            // Not while the competing flow comes and
                            // goes: then every middle sender is at its
                            // 500 kbit/s, so that what fits and what
                            // does not is the path's doing alone.
                            let moving = case.mobile.is_some_and(|mobile| {
                                let at = frame_every * (tick as u32);
                                at < mobile.shared_from || at >= mobile.shared_until
                            });
                            let packets = if moving && layer.tag == b'm' {
                                const ROUND: [u64; 6] = [5, 6, 7, 6, 5, 4];
                                ROUND[((tick / (8 * DIP_FPS)) as usize + index) % ROUND.len()]
                            } else {
                                layer.packets
                            };
                            // A key frame of a mobile case is six frames'
                            // worth: 15 kB on the middle sender.
                            let key_factor = if case.mobile.is_some() {
                                2 * FB_KEY_FACTOR
                            } else {
                                FB_KEY_FACTOR
                            };
                            let n = if key { packets * key_factor } else { packets };
                            let rtp_time = 90_000 + (tick * (90_000 / DIP_FPS)) as u32;
                            let written = Instant::now();
                            for i in 0..n {
                                let mut ext = str0m::rtp::ExtensionValues::default();
                                if !case.no_descriptor {
                                    ext.user_values
                                        .set(PacketDescriptor(fb_descriptor(frame, key, i, n)));
                                }
                                let seq = layer.next_seq;
                                layer.next_seq += 1;
                                let mut api = publisher.client.rtc.direct_api();
                                let stream = api
                                    .stream_tx_by_mid(layer.mid.expect("offered"), None)
                                    .expect("send stream");
                                stream
                                    .write_rtp(
                                        UPLINK_VP8_PT.into(),
                                        seq.into(),
                                        rtp_time,
                                        written,
                                        i + 1 == n,
                                        ext,
                                        true,
                                        fb_payload(layer.tag, frame, i, layer.payload),
                                    )
                                    .expect("a publisher writes a packet");
                            }
                            // Only the count is used; the list would grow for
                            // the whole run otherwise.
                            if layer.frames.len() < 4 {
                                layer.frames.push(FbFrame {
                                    key,
                                    n,
                                    at: written,
                                });
                            }
                        }
                    }
                    publisher.next_tick = ticks + 1;
                    publisher.client.feed_timeout();
                    let _ = publisher.client.drain().await;
                    publisher.client.rtp_out.clear();
                    publisher.client.wire_headers.clear();
                }
                ticks += 1;
            }
            let still_asking = case
                .asking_stops_after
                .map_or(true, |after| Instant::now() < start + after);
            if case.subscriber_keeps_asking && still_asking && Instant::now() >= next_pli {
                next_pli = Instant::now() + Duration::from_millis(200);
                for (mid, _, _) in &tracks {
                    let _ = b.send_pli(*mid);
                }
            }
            if Instant::now() >= next_sample {
                next_sample = Instant::now() + Duration::from_millis(500);
                if let Some(snap) = media_task
                    .downlink_snapshot(b_id.clone())
                    .await
                    .expect("the media task answers")
                {
                    samples.push(DlSample {
                        at: Instant::now(),
                        snap,
                    });
                }
            }
        }

        // A client in RTP mode holds one received packet at a time: it is
        // drained after every datagram.
        for publisher in &mut publishers {
            while let Ok((len, src)) = publisher.client.socket.try_recv_from(&mut buf) {
                publisher.client.feed_receive(&buf[..len], src, sfu_addr);
                let _ = publisher.client.drain().await;
            }
            publisher.client.feed_timeout();
            let _ = publisher.client.drain().await;
        }
        while let Ok((at, bytes, src)) = arrived.try_recv() {
            link.arrive(at, &bytes, src);
        }
        while let Some((at, bytes, src)) = link.leave(Instant::now()) {
            b.feed_receive_at(&bytes, src, sfu_addr, at);
            let _ = b.drain().await;
        }
        b.feed_timeout();
        let _ = b.drain().await;

        // Wake for B's socket, for the next datagram leaving the link, or
        // within a millisecond for everything else.
        let wait = link
            .queue
            .front()
            .map_or(Duration::from_millis(1), |(at, _, _)| {
                at.saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(1))
            });
        tokio::select! {
            r = arrived.recv() => {
                if let Some((at, bytes, src)) = r {
                    link.arrive(at, &bytes, src);
                }
            }
            () = tokio::time::sleep(wait) => {}
        }
    }
    receiver.abort();

    // Which SSRC is which track, main stream and retransmission stream.
    let mut ssrcs: Vec<(u32, usize, DlRole, bool)> = Vec::new();
    for (mid, publisher, role) in &tracks {
        let mut api = b.rtc.direct_api();
        if let Some(stream) = api.stream_rx_by_mid(*mid, None) {
            ssrcs.push((*stream.ssrc(), *publisher, *role, false));
            if let Some(rtx) = stream.rtx() {
                ssrcs.push((*rtx, *publisher, *role, true));
            }
        }
    }
    let track_of = |ssrc: u32| ssrcs.iter().find(|(s, _, _, _)| *s == ssrc).copied();
    let link_dropped = link
        .dropped
        .iter()
        .filter_map(|(at, ssrc)| track_of(*ssrc).map(|(_, p, role, rtx)| (*at, p, role, rtx)))
        .collect();
    let link_entered = link
        .entered
        .iter()
        .filter_map(|(at, ssrc)| track_of(*ssrc).map(|(_, _, role, rtx)| (*at, role, rtx)))
        .collect();
    let mut rtx_bytes = 0;
    let mut padding_bytes = 0;
    let mut media_bytes = 0;
    for (_, ssrc, len) in &link.passed {
        match track_of(*ssrc) {
            Some((_, _, _, true)) => {
                rtx_bytes += len;
                // A padding-only packet carries at most 255 bytes; every
                // media payload of this run is 375 bytes or more, so a
                // datagram this small on a retransmission stream is one.
                if *len < DL_PADDING_BELOW {
                    padding_bytes += len;
                }
            },
            Some(_) => media_bytes += len,
            None => {},
        }
    }

    let keyframe_requests = publishers
        .iter()
        .map(|publisher| {
            let on = |layer: &FbLayer| {
                publisher
                    .client
                    .keyframe_request_mids
                    .iter()
                    .filter(|mid| Some(**mid) == layer.mid)
                    .count()
            };
            [
                on(&publisher.layers[0]),
                on(&publisher.layers[1]),
                on(&publisher.layers[2]),
            ]
        })
        .collect();

    let mut keyframe_request_times = Vec::new();
    for (index, publisher) in publishers.iter().enumerate() {
        let client = &publisher.client;
        for (mid, at) in client
            .keyframe_request_mids
            .iter()
            .zip(&client.keyframe_request_at)
        {
            if let Some(track) = publisher.layers.iter().position(|l| l.mid == Some(*mid)) {
                keyframe_request_times.push((index, track, *at));
            }
        }
    }

    DownlinkRun {
        t0: t0.expect("the publishers streamed"),
        lifted_at,
        ended_at: Instant::now(),
        publishers: names,
        given,
        samples,
        got,
        link_dropped,
        link_entered,
        rtx_bytes,
        padding_bytes,
        media_bytes,
        dip_lost,
        worst_stall,
        tick_late,
        clumps,
        keyframe_requests,
        keyframe_request_times,
        publisher_answers,
        subscriber_answer,
        sfu_offers: offers,
    }
}

/// The given set as letters, most important publisher first: `M` main,
/// `m` middle, `l` light.
fn dl_letters(set: &[(usize, lakko_svc_sfu::protocol::VideoQuality)]) -> String {
    use lakko_svc_sfu::protocol::VideoQuality;
    set.iter()
        .map(|(_, quality)| match quality {
            VideoQuality::Main => 'M',
            VideoQuality::Middle => 'm',
            VideoQuality::Light => 'l',
        })
        .collect()
}

/// Print a downlink run second by second: the SFU's view of the
/// subscriber's downlink, what the subscriber was told it is given, what
/// it was handed on each kind of track and what its link lost.
#[allow(
    clippy::print_stderr,
    clippy::cast_precision_loss,
    clippy::too_many_lines
)]
fn print_downlink_timeline(what: &str, run: &DownlinkRun) {
    let secs = |at: Instant| at.saturating_duration_since(run.t0).as_secs_f64();
    eprintln!(
        "{what}: lifted at {:?} s, ended at {:.1} s",
        run.lifted_at.map(|at| (secs(at) * 10.0).round() / 10.0),
        secs(run.ended_at)
    );
    eprintln!(
        "{what}: every video-given frame (s, set; M main, m middle, l light, first is most \
         important): {:?}",
        run.given
            .iter()
            .map(|(at, set)| format!("{:.2} {}", secs(*at), dl_letters(set)))
            .collect::<Vec<_>>()
    );
    eprintln!(
        "{what}:   s | budget carried sending demand probing kbit/s | given M/m/l | probes \
         started/carried, padding kB, cuts, standing queue ms, detector, the limit kbit/s, the \
         level from which sets are held while one that proved too large is waited out, the \
         wait the next such set costs s, times the limit followed the set's cost | handed main/middle/light \
         pkts | link lost \
         main/middle/light/light-rtx/other-rtx | SFU sent main/middle/light first \
         transmissions and retransmission-stream packets | the test's publishers: frames \
         written, the latest by ms"
    );
    let whole = run.ended_at.saturating_duration_since(run.t0).as_secs();
    for second in 0..=whole {
        let from = run.t0 + Duration::from_secs(second);
        let until = from + Duration::from_secs(1);
        let within = |at: Instant| at >= from && at < until;
        let sample = run.samples.iter().rfind(|sample| sample.at < until);
        let kbps =
            |bps: Option<u64>| bps.map_or_else(|| "-".to_owned(), |b| (b / 1000).to_string());
        let handed = |role: DlRole| {
            run.got
                .iter()
                .filter(|p| p.role == role && within(p.at))
                .count()
        };
        let lost = |role: DlRole, rtx: bool| {
            run.link_dropped
                .iter()
                .filter(|(at, _, r, x)| within(*at) && *r == role && *x == rtx)
                .count()
        };
        let other_rtx = lost(DlRole::Main, true) + lost(DlRole::Middle, true);
        let entered = |role: Option<DlRole>, rtx: bool| {
            run.link_entered
                .iter()
                .filter(|(at, r, x)| {
                    within(*at) && *x == rtx && role.map_or(true, |role| *r == role)
                })
                .count()
        };
        eprintln!(
            "{what}: {second:>3} | {:>6} {:>7} {:>7} {:>6} {:>7} | {} | {} | {:>4}/{:>4}/{:>4} | \
             {}/{}/{}/{}/{} | {}/{}/{} {} | {} {}",
            sample.map_or_else(|| "-".to_owned(), |s| kbps(s.snap.budget_bps)),
            sample.map_or_else(|| "-".to_owned(), |s| kbps(s.snap.carried_bps)),
            sample.map_or_else(
                || "-".to_owned(),
                |s| (s.snap.sending_bps / 1000).to_string()
            ),
            sample.map_or_else(|| "-".to_owned(), |s| kbps(s.snap.demand_bps)),
            sample.map_or_else(|| "-".to_owned(), |s| kbps(s.snap.probing_bps)),
            sample.map_or_else(
                || "-".to_owned(),
                |s| format!("{}/{}/{}", s.snap.main, s.snap.middle, s.snap.light)
            ),
            sample.map_or_else(
                || "-".to_owned(),
                |s| format!(
                    "{}/{} {:>5} {} q{} {} lim{} held{} wait{} f{}",
                    s.snap.probes_started,
                    s.snap.probes_carried,
                    s.snap.padding_bytes / 1000,
                    s.snap.cuts,
                    s.snap
                        .queue_ms
                        .map_or_else(|| "-".to_owned(), |q| q.to_string()),
                    s.snap.detector,
                    kbps(s.snap.limit_bps),
                    s.snap
                        .held_from_level
                        .map_or_else(|| "-".to_owned(), |l| l.to_string()),
                    s.snap.too_large_wait_secs,
                    s.snap.followed
                )
            ),
            handed(DlRole::Main),
            handed(DlRole::Middle),
            handed(DlRole::Light),
            lost(DlRole::Main, false),
            lost(DlRole::Middle, false),
            lost(DlRole::Light, false),
            lost(DlRole::Light, true),
            other_rtx,
            entered(Some(DlRole::Main), false),
            entered(Some(DlRole::Middle), false),
            entered(Some(DlRole::Light), false),
            entered(None, true),
            // A frame is counted in the second it was written in.
            run.tick_late
                .iter()
                .filter(|(due, late)| within(*due + *late))
                .count(),
            run.tick_late
                .iter()
                .filter(|(due, _)| within(*due))
                .map(|(_, late)| late.as_millis())
                .max()
                .unwrap_or(0),
        );
    }
    let total = (run.media_bytes + run.rtx_bytes).max(1);
    eprintln!(
        "{what}: bytes that left the link: media {} retransmission streams {} ({:.1}% of all), \
         of which padding-only packets {} ({:.1}% of all)",
        run.media_bytes,
        run.rtx_bytes,
        run.rtx_bytes as f64 * 100.0 / total as f64,
        run.padding_bytes,
        run.padding_bytes as f64 * 100.0 / total as f64,
    );
    eprintln!("{what}: media datagrams lost in the dip: {}", run.dip_lost);
    eprintln!(
        "{what}: the test's own loop never went longer than {} ms without a pass",
        run.worst_stall.as_millis()
    );
}

/// Sequence numbers a light track never handed the subscriber between
/// `from` and `until`, over the three light tracks: holes between the
/// packets it was handed in that time, after retransmission.
fn dl_light_holes(run: &DownlinkRun, from: Instant, until: Instant) -> usize {
    let mut holes = 0usize;
    for publisher in 0..run.publishers.len() {
        let mut seqs: Vec<u16> = run
            .got
            .iter()
            .filter(|p| {
                p.publisher == publisher && p.role == DlRole::Light && p.at >= from && p.at < until
            })
            .map(|p| p.seq)
            .collect();
        let Some(&first) = seqs.first() else {
            continue;
        };
        // Offsets from the first, so a wrap of the 16-bit number is not
        // a hole.
        let mut offsets: Vec<u16> = seqs.drain(..).map(|s| s.wrapping_sub(first)).collect();
        offsets.sort_unstable();
        offsets.dedup();
        let span = usize::from(*offsets.last().unwrap_or(&0)) + 1;
        holes += span - offsets.len();
    }
    holes
}

/// The 7.5 Mbit/s link of the narrow-downlink tests, with a 150 ms
/// drop-tail queue, lifted 40 s after the publishers start.
const DL_NARROW_BPS: u64 = 7_500_000;
/// The SFU has settled on one main and two middles this long after the
/// publishers start, at the latest. (Observed on 2026-10-05: see the
/// README's "Downlink control".)
const DL_SETTLED_WITHIN: Duration = Duration::from_secs(10);
/// All three are back on main this long after the limit is lifted, at
/// the latest: the longest wait between two probes
/// (`downlink_probe::PROBE_MAX_RETRY`, 10 s) and two probes of a second
/// and a half each.
const DL_BACK_WITHIN: Duration = Duration::from_secs(15);
/// A link that carries two middle pictures and a light one (2.2 Mbit/s
/// on the wire) and not three middles (2.5).
const DL_VERY_NARROW_BPS: u64 = 2_350_000;
/// A publisher's clump may take a picture from the subscriber for at most
/// this long. Chosen: the bound the dip case has.
const DL_CLUMP_BACK_WITHIN: Duration = Duration::from_secs(8);
/// After this long the limited link must drop nothing more.
const DL_NO_DROPS_AFTER: Duration = Duration::from_secs(2);

fn dl_narrow_case(delay: Duration, jitter: Duration) -> DownlinkCase {
    DownlinkCase {
        limit_bps: Some(DL_NARROW_BPS),
        queue_limit: (DL_NARROW_BPS / 8 * 150 / 1000) as usize,
        lift_after: Duration::from_secs(40),
        run_for: Duration::from_secs(40) + DL_BACK_WITHIN + Duration::from_secs(5),
        downlink_control: true,
        delay,
        jitter,
        dip_after: None,
        clump_at: None,
        clump_all_at: None,
        clump_after_lift: None,
        subscriber_keeps_asking: false,
        asking_stops_after: None,
        no_descriptor: false,
        mobile: None,
    }
}

/// On a downlink that carries one main and two middle pictures, the SFU
/// settles on exactly that, and the link drops nothing after its first
/// two seconds; when the limit is lifted, all three publishers are back
/// on their main picture. A publisher's own bitrate control is untouched.
/// The acceptance test of ADR-0070 step 6.
///
/// The link: 7.5 Mbit/s with a 150 ms queue. What the three publishers
/// cost, as the SFU measures it: light 3 x 2 x 0.155 (each light packet is
/// sent twice) = 0.93 Mbit/s first; a main track 3.11 x 1.2 = 3.74; a
/// middle track 0.51 x 1.2 = 0.61. One main and two middles need
/// 5.89 Mbit/s and put 5.1 on the wire; two mains and a middle need 9.0
/// and would put 7.7 on the wire, more than the link carries.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
#[ignore = "ADR-0070 step 6 acceptance: about a minute; run by name with --ignored"]
#[allow(clippy::print_stderr)]
async fn a_narrow_downlink_gives_up_the_larger_pictures_and_gets_them_back() {
    let what = "narrow downlink";
    let run = run_downlink_measured(what, dl_narrow_case(Duration::ZERO, Duration::ZERO)).await;
    check_narrow_downlink(what, &run);
}

/// The same on a path with 50 ms of one-way delay and up to 10 ms of
/// jitter after the bottleneck: the estimator works on the growth of
/// delay, and neither a fixed delay nor jitter of that size may read as
/// a queue.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
#[ignore = "ADR-0070 step 6 acceptance: about a minute; run by name with --ignored"]
#[allow(clippy::print_stderr)]
async fn a_narrow_downlink_with_delay_and_jitter_settles_the_same() {
    let what = "narrow downlink, 50 ms delay, 10 ms jitter";
    let run = run_downlink_measured(
        what,
        dl_narrow_case(Duration::from_millis(50), Duration::from_millis(10)),
    )
    .await;
    check_narrow_downlink(what, &run);
}

/// ADR-0070's dip on a downlink that is otherwise unlimited: for one
/// second only 30% of the media datagrams arrive. A burst of loss with no
/// growth of delay is not a narrow path: the subscriber must not be left
/// with less than it asked for.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
#[allow(clippy::print_stderr)]
async fn a_dip_on_an_unlimited_downlink_does_not_leave_the_subscriber_downgraded() {
    use lakko_svc_sfu::protocol::VideoQuality::Main;

    /// The longest the subscriber may be given less than it asked for,
    /// in all, because of the dip.
    const DOWNGRADED_AT_MOST: Duration = Duration::from_secs(8);
    let what = "dip on an unlimited downlink";
    let case = DownlinkCase {
        limit_bps: None,
        queue_limit: 0,
        lift_after: Duration::ZERO,
        run_for: Duration::from_secs(25),
        downlink_control: true,
        delay: Duration::ZERO,
        jitter: Duration::ZERO,
        dip_after: Some(Duration::from_secs(10)),
        clump_at: None,
        clump_all_at: None,
        clump_after_lift: None,
        subscriber_keeps_asking: false,
        asking_stops_after: None,
        no_descriptor: false,
        mobile: None,
    };
    let run = run_downlink_measured(what, case).await;
    print_downlink_timeline(what, &run);
    assert!(
        run.dip_lost > 500,
        "the dip lost {} datagrams",
        run.dip_lost
    );

    // The time spent on less than three main pictures: from each frame
    // that says so to the next frame.
    let mut downgraded = Duration::ZERO;
    for (i, (at, set)) in run.given.iter().enumerate() {
        if set.len() == 3 && set.iter().any(|(_, quality)| *quality != Main) {
            let until = run.given.get(i + 1).map_or(run.ended_at, |(next, _)| *next);
            downgraded += until.saturating_duration_since(*at);
        }
    }
    eprintln!(
        "{what}: given less than three main pictures for {:.2} s in all",
        downgraded.as_secs_f64()
    );
    assert!(
        downgraded <= DOWNGRADED_AT_MOST,
        "downgraded for {downgraded:?}"
    );
    assert_eq!(
        run.given.last().map(|(_, set)| set),
        Some(&vec![(0, Main), (1, Main), (2, Main)]),
        "and it ends on three main pictures"
    );
}

/// [`dl_narrow_case`] with the first publisher's uplink stalling for
/// [`DL_CLUMP`] and delivering its frames together, once in the steady
/// state and once just after the limit is lifted.
fn dl_clump_case(delay: Duration, jitter: Duration) -> DownlinkCase {
    DownlinkCase {
        clump_at: Some(Duration::from_secs(20)),
        clump_all_at: Some(Duration::from_secs(30)),
        clump_after_lift: Some(Duration::from_millis(300)),
        ..dl_narrow_case(delay, jitter)
    }
}

/// A publisher whose frames arrive late in a clump does not cost a
/// subscriber its pictures. The publisher of the main picture stalls for
/// 200 ms and then sends its five frames at once, twenty seconds in: the
/// SFU forwards as packets arrive, so the clump reaches the subscriber's
/// narrow link as a burst and stands in its queue for a while. That is
/// the publisher's path, not the subscriber's: one main and two middles
/// stay, or are back within [`DL_CLUMP_BACK_WITHIN`]. The same again
/// 300 ms after the limit is lifted: all three are on main within the
/// time the plain case has.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
#[ignore = "ADR-0070 step 6 acceptance: a 60 s run of a feature that is off by default; run by name"]
#[allow(clippy::print_stderr)]
async fn a_publishers_clump_does_not_cost_a_narrow_downlink_its_pictures() {
    let what = "narrow downlink, publisher clump";
    let run = run_downlink_measured(what, dl_clump_case(Duration::ZERO, Duration::ZERO)).await;
    assert_eq!(run.clumps.len(), 3, "every clump happened");
    check_narrow_downlink(what, &run);
}

/// The same on the path with 50 ms of delay and up to 10 ms of jitter.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
#[ignore = "ADR-0070 step 6 acceptance: a 60 s run of a feature that is off by default; run by name"]
#[allow(clippy::print_stderr)]
async fn a_publishers_clump_on_a_downlink_with_delay_and_jitter_costs_no_pictures() {
    let what = "narrow downlink with delay and jitter, publisher clump";
    let run = run_downlink_measured(
        what,
        dl_clump_case(Duration::from_millis(50), Duration::from_millis(10)),
    )
    .await;
    assert_eq!(run.clumps.len(), 3, "every clump happened");
    check_narrow_downlink(what, &run);
}

/// A downlink that carries two middle pictures and a light one, lifted
/// after twenty seconds: from little the SFU must find its way back to
/// everything. No state may be one from which nothing probes, and a
/// probe from a low rate must be able to pass (in a run of the second
/// round a subscriber stood on two middles and a light for 28 s, 20 of
/// them after the limit was gone, every probe ending short).
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
#[ignore = "ADR-0070 step 6 acceptance: a 40 s run of a feature that is off by default; run by name"]
#[allow(clippy::print_stderr)]
async fn a_very_narrow_downlink_gets_everything_back_when_it_is_lifted() {
    use lakko_svc_sfu::protocol::VideoQuality::Main;

    let what = "very narrow downlink";
    let run = run_downlink_measured(
        what,
        DownlinkCase {
            limit_bps: Some(DL_VERY_NARROW_BPS),
            queue_limit: (DL_VERY_NARROW_BPS / 8 * 150 / 1000) as usize,
            lift_after: Duration::from_secs(20),
            run_for: Duration::from_secs(20) + DL_BACK_WITHIN + Duration::from_secs(5),
            ..dl_narrow_case(Duration::ZERO, Duration::ZERO)
        },
    )
    .await;
    print_downlink_timeline(what, &run);
    let lifted = run.lifted_at.expect("the limit was lifted");
    let (_, before) = run
        .given
        .iter()
        .rfind(|(at, _)| *at < lifted)
        .expect("frames before the lift");
    assert!(
        before.iter().all(|(_, quality)| *quality != Main),
        "the link carries no main picture: {}",
        dl_letters(before)
    );
    let late_drops = run
        .link_dropped
        .iter()
        .filter(|(at, _, _, _)| *at >= run.t0 + Duration::from_secs(8))
        .count();
    let all_main = vec![(0, Main), (1, Main), (2, Main)];
    let (back_at, _) = run
        .given
        .iter()
        .find(|(at, set)| *at >= lifted && set == &all_main)
        .expect("all three are given main again after the lift");
    let back_after = back_at.saturating_duration_since(lifted);
    eprintln!(
        "{what}: stood on {} before the lift; the link dropped {late_drops} datagrams after its \
         first 8 s; all three back on main {:.1} s after the limit was lifted",
        dl_letters(before),
        back_after.as_secs_f64()
    );
    assert!(
        back_after <= DL_BACK_WITHIN,
        "back {back_after:?} after the lift"
    );
}

/// The mobile-like downlink of the oscillation test ([`MobileLink`];
/// every figure chosen). Three phases:
///
/// - **0 to 90 s, a path that carries the set**: 4 Mbit/s (three middle
///   pictures need about 2.9 with headroom and put about 2.6 on the
///   wire), delay waves of 40 ms twice a second, and a dip to a tenth
///   of that for 1.2 s at 20, 50 and 78 s.
/// - **90 to 198 s, a path that does not**: a competing flow leaves this
///   subscriber 2.2 Mbit/s for ten seconds and then all 4 for eight, six
///   times over, and the publishers' rates stand still (0.5 Mbit/s a
///   middle sender). Two middles put 1.9 Mbit/s on the wire and three
///   2.4: three fit only while the competing flow is away.
/// - **198 s to the end, the path as at first**, no dip.
const DL_MOBILE: MobileLink = MobileLink {
    capacity_bps: 4_000_000,
    wave: Duration::from_millis(40),
    dips: [
        Duration::from_secs(20),
        Duration::from_secs(50),
        Duration::from_secs(78),
    ],
    dip_for: Duration::from_millis(1200),
    dip_bps: 400_000,
    shared_from: Duration::from_secs(90),
    shared_until: Duration::from_secs(198),
    shared_on: Duration::from_secs(10),
    shared_off: Duration::from_secs(8),
    shared_bps: 2_200_000,
};
/// How long the mobile-like run lasts.
const DL_MOBILE_RUN: Duration = Duration::from_secs(275);
/// The most a publisher's given quality may change per minute while the
/// path carries the set (the first phase), the dips included: a dip takes
/// the three middle pictures away once and they come back once, which is
/// eighteen changes in 87 s (12.4 a minute) before anything else. Seen:
/// 12.4 to 13.8.
const DL_MOBILE_CHANGES_A: f64 = 16.0;
/// And while a competing flow comes and goes (the second phase): a set
/// that proved too large is tried again after a longer wait each time.
/// Seen: 3.3 and 4.4.
const DL_MOBILE_CHANGES_B: f64 = 7.0;
/// Times anything may be taken away in the first phase: the three
/// dips, and one more.
const DL_MOBILE_TAKEN_A: usize = 4;
/// Times anything may be taken away in the second phase (six rounds of
/// the competing flow). Seen: 2 and 3.
const DL_MOBILE_TAKEN_B: usize = 4;
/// Three middles are given again this long after the path is its own
/// again, at the latest: the longest wait a set that proved too large
/// costs (`downlink_probe::TOO_LARGE_MAX_WAIT`, 60 s), a failed probe's
/// 8 s and the probes themselves.
const DL_MOBILE_BACK_WITHIN: Duration = Duration::from_secs(75);

/// What a mobile-like run came to, per phase.
struct DlMobilePhase {
    /// Changes of a publisher's given quality, up and down.
    ups: usize,
    downs: usize,
    /// Times something was taken away (downward changes less than a
    /// second apart are one).
    taken: usize,
    /// Seconds with all three on middle.
    full: f64,
    secs: f64,
}

impl DlMobilePhase {
    #[allow(clippy::cast_precision_loss)]
    fn per_minute(&self) -> f64 {
        (self.ups + self.downs) as f64 * 60.0 / self.secs.max(1.0)
    }
}

/// The changes of what the subscriber was given between `from` and
/// `until` after the publishers started.
fn dl_mobile_phase(run: &DownlinkRun, from: Duration, until: Duration) -> DlMobilePhase {
    use lakko_svc_sfu::protocol::VideoQuality;
    let rank = |quality: VideoQuality| match quality {
        VideoQuality::Light => 0,
        VideoQuality::Middle => 1,
        VideoQuality::Main => 2,
    };
    let (from, until) = (run.t0 + from, (run.t0 + until).min(run.ended_at));
    let mut phase = DlMobilePhase {
        ups: 0,
        downs: 0,
        taken: 0,
        full: 0.0,
        secs: until.saturating_duration_since(from).as_secs_f64(),
    };
    let mut last_down: Option<Instant> = None;
    for (i, (at, set)) in run.given.iter().enumerate() {
        let next = run.given.get(i + 1).map_or(run.ended_at, |(next, _)| *next);
        if set.len() == 3 && set.iter().all(|(_, q)| *q == VideoQuality::Middle) {
            let (a, b) = ((*at).max(from), next.min(until));
            phase.full += b.saturating_duration_since(a).as_secs_f64();
        }
        if *at < from || *at >= until || i == 0 {
            continue;
        }
        let (_, before) = &run.given[i - 1];
        let mut down = false;
        for (publisher, quality) in set {
            let Some((_, was)) = before.iter().find(|(p, _)| p == publisher) else {
                continue;
            };
            match rank(*quality).cmp(&rank(*was)) {
                std::cmp::Ordering::Greater => phase.ups += 1,
                std::cmp::Ordering::Less => {
                    phase.downs += 1;
                    down = true;
                },
                std::cmp::Ordering::Equal => {},
            }
        }
        if down {
            if last_down.map_or(true, |last| {
                at.saturating_duration_since(last) >= Duration::from_secs(1)
            }) {
                phase.taken += 1;
            }
            last_down = Some(*at);
        }
    }
    phase
}

/// On a downlink like the mobile link of the live call of 2026-10-09 the
/// SFU holds a picture quality instead of giving it and taking it back
/// every few seconds. The path: [`DL_MOBILE`]; the subscriber asks for
/// the middle picture of three publishers whose rates move.
///
/// - While the path carries three middles they stay, the three dips
///   apart: the delay waves, the publishers' moving rates and their key
///   frames take nothing away.
/// - While a competing flow comes and goes, the set that fits only when
///   it is away is tried at longer and longer intervals, not at every
///   chance.
/// - A dip or the competing flow's return is answered at once: the
///   light pictures lose no packet for it.
/// - When the path is its own again, three middles are back within
///   [`DL_MOBILE_BACK_WITHIN`], and from then on nothing is taken away:
///   the publishers' rates go on moving, and a set that comes to cost
///   more on a path that carries it is kept. (The build before the
///   fourth round took a picture away there, 2.4 and 2.7 s into that
///   phase in two runs of two, with no cut of the estimate, and gave it
///   back 2 s later.)
///
/// What the bounds do not show: the same build before the fourth round
/// stays within those of the first two phases on this model (it sat on
/// one middle picture for 84 s of the second phase). See the README,
/// "The fourth round".
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
#[ignore = "ADR-0070 step 6 acceptance: a run of nearly five minutes of a feature that is off by default; run by name"]
#[allow(clippy::print_stderr, clippy::cast_precision_loss)]
async fn a_mobile_like_downlink_holds_a_picture_instead_of_cycling() {
    use lakko_svc_sfu::protocol::VideoQuality::Middle;

    let what = "mobile-like downlink";
    let case = DownlinkCase {
        limit_bps: Some(DL_MOBILE.capacity_bps),
        // A mobile network's queue is deep: two seconds.
        queue_limit: usize::try_from(DL_MOBILE.capacity_bps / 8 * 2).expect("small"),
        lift_after: Duration::from_secs(3600),
        run_for: DL_MOBILE_RUN,
        mobile: Some(&DL_MOBILE),
        ..dl_narrow_case(Duration::from_millis(30), Duration::from_millis(5))
    };
    let run = run_downlink_measured(what, case).await;
    print_downlink_timeline(what, &run);

    let secs = |d: Duration| d.as_secs_f64();
    let (a_until, b_until) = (DL_MOBILE.shared_from, DL_MOBILE.shared_until);
    // The first three seconds are the start (nothing is known of the
    // path and everything asked for is sent).
    let a = dl_mobile_phase(&run, Duration::from_secs(3), a_until);
    let b = dl_mobile_phase(&run, a_until, b_until);
    let c = dl_mobile_phase(&run, b_until, DL_MOBILE_RUN);
    let all_middle = vec![(0, Middle), (1, Middle), (2, Middle)];
    let own_again = run.t0 + b_until;
    let back = run
        .given
        .iter()
        .enumerate()
        .find(|(i, (at, set))| {
            let until = run.given.get(i + 1).map_or(run.ended_at, |(next, _)| *next);
            set == &all_middle && until > own_again && *at <= run.ended_at
        })
        .map(|(_, (at, _))| at.saturating_duration_since(own_again));
    let light_lost = run
        .link_dropped
        .iter()
        .filter(|(at, _, role, _)| *role == DlRole::Light && *at >= run.t0 + Duration::from_secs(3))
        .count();
    let light_holes = dl_light_holes(&run, run.t0 + Duration::from_secs(3), run.ended_at);
    for (name, phase) in [
        ("the path carries the set", &a),
        ("a competing flow comes and goes", &b),
        ("the path is its own again", &c),
    ] {
        eprintln!(
            "{what}: {name}: {:.0} s, {} given and {} taken away ({:.1} changes a minute), \
             something taken away {} times, all three on middle for {:.0} s",
            phase.secs,
            phase.ups,
            phase.downs,
            phase.per_minute(),
            phase.taken,
            phase.full
        );
    }
    eprintln!(
        "{what}: the link lost {light_lost} light datagrams after the first 3 s and the \
         subscriber is missing {light_holes} light packets; three middles were back {:?} s after \
         the path was its own again; padding {} kB",
        back.map(|d| (secs(d) * 10.0).round() / 10.0),
        run.padding_bytes / 1000
    );

    assert!(
        a.per_minute() <= DL_MOBILE_CHANGES_A && a.taken <= DL_MOBILE_TAKEN_A,
        "while the path carries the set: {:.1} changes a minute, taken away {} times",
        a.per_minute(),
        a.taken
    );
    assert!(
        a.full >= a.secs * 0.6,
        "while the path carries the set all three are on middle most of the time: {:.0} of {:.0} s",
        a.full,
        a.secs
    );
    assert!(
        b.per_minute() <= DL_MOBILE_CHANGES_B && b.taken <= DL_MOBILE_TAKEN_B,
        "while a competing flow comes and goes: {:.1} changes a minute, taken away {} times",
        b.per_minute(),
        b.taken
    );
    assert_eq!(
        c.taken, 0,
        "nothing is taken away once the path is its own again ({} changes down)",
        c.downs
    );
    assert_eq!(light_lost, 0, "the link loses no light datagram");
    assert_eq!(light_holes, 0, "and the subscriber misses no light packet");
    let back = back.expect("three middles are given again when the path is its own");
    assert!(
        back <= DL_MOBILE_BACK_WITHIN,
        "three middles back {back:?} after the path was its own again"
    );
}

/// The most key frame requests one publisher track may receive in the
/// twenty seconds of [`run_keyframe_requests`]. A subscriber that asks
/// five times a second is relayed after 0, 1, 3, 7 and 15 s (five), and
/// the track's opening and starting ask a few times more; a track whose
/// key frames the SFU cannot see is asked five times and then once in
/// ten seconds. Without either bound a track was asked twice a second:
/// forty times.
const KF_MOST_REQUESTS: usize = 10;

/// Three publishers of three senders each and a subscriber that asks for
/// a key frame on every track it receives five times a second for twenty
/// seconds, on an unlimited link without downlink control. Returns the
/// requests each publisher received per sender.
async fn run_keyframe_requests(what: &str, no_descriptor: bool) -> Vec<[usize; 3]> {
    Box::pin(run_keyframe_requests_until(what, no_descriptor, None))
        .await
        .keyframe_requests
}

/// As [`run_keyframe_requests`], with the subscriber asking only for the
/// first `asks_for` of the run when that is given.
async fn run_keyframe_requests_until(
    what: &str,
    no_descriptor: bool,
    asks_for: Option<Duration>,
) -> DownlinkRun {
    let _one = DL_ONE_AT_A_TIME.lock().await;
    let _watchdog = DlWatchdog::start(what, Duration::from_secs(150));
    Box::pin(run_downlink(DownlinkCase {
        limit_bps: None,
        queue_limit: usize::MAX,
        lift_after: Duration::from_secs(600),
        run_for: Duration::from_secs(20),
        downlink_control: false,
        subscriber_keeps_asking: true,
        asking_stops_after: asks_for,
        no_descriptor,
        ..dl_narrow_case(Duration::ZERO, Duration::ZERO)
    }))
    .await
}

/// A subscriber of publishers without a frame descriptor that needs a key
/// frame is asked for promptly, again after a second and after two more
/// while it keeps needing one, and no more once it stops asking. The SFU
/// cannot see a key frame arrive on such a stream; the subscriber's own
/// asking is the only sign. On a live call (2026-10-07) a viewer of a
/// Firefox publisher's main track had packets it could not decode for
/// more than half a minute while its requests were let through once in
/// ten seconds.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
#[allow(clippy::print_stderr)]
async fn a_subscriber_of_a_publisher_without_a_descriptor_is_asked_for_while_it_asks() {
    /// The subscriber asks for this long, then has its picture.
    const ASKS_FOR: Duration = Duration::from_secs(9);
    let what = "key frame requests while a subscriber asks";
    let run = run_keyframe_requests_until(what, true, Some(ASKS_FOR)).await;
    // The tracks the subscriber is being sent: main and light of the
    // first publisher, middle and light of the second, light of the third.
    for (publisher, track) in [(0, 0), (0, 2), (1, 1), (1, 2), (2, 2)] {
        let times: Vec<Duration> = run
            .keyframe_request_times
            .iter()
            .filter(|(p, t, _)| (*p, *t) == (publisher, track))
            .map(|(_, _, at)| at.saturating_duration_since(run.t0))
            .collect();
        eprintln!("{what}: publisher {publisher} track {track}: {times:?}");
        let within = |until: Duration| times.iter().filter(|at| **at <= until).count();
        assert!(
            within(Duration::from_millis(1500)) >= 1,
            "asked promptly: {times:?}"
        );
        assert!(
            within(Duration::from_secs(5)) >= 3,
            "asked again after one second and after two more: {times:?}"
        );
        assert!(
            within(ASKS_FOR + Duration::from_secs(1)) >= 4,
            "and again four seconds after that: {times:?}"
        );
        let last = times.iter().max().copied().unwrap_or_default();
        assert!(
            last <= ASKS_FOR + Duration::from_millis(1500),
            "nothing once the subscriber has stopped asking: {times:?}"
        );
        assert!(times.len() <= KF_MOST_REQUESTS, "{times:?}");
    }
}

/// The SFU does not ask a publisher for a key frame for ever because a
/// subscriber does. On the first live call a receiver that could not
/// decode what it was sent asked on every light and middle track without
/// end, the SFU relayed two requests a second per track for the whole
/// call (270 per track in four and a half minutes), and the browsers
/// obliged with a key frame a second on a 150 kbit/s sender.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
#[allow(clippy::print_stderr)]
async fn a_subscriber_that_keeps_asking_does_not_cost_a_key_frame_a_second() {
    let what = "key frame requests";
    let requests = run_keyframe_requests(what, false).await;
    eprintln!("{what}: per publisher, on main / middle / light: {requests:?}");
    assert_eq!(requests.len(), 3);
    for (publisher, per_track) in requests.iter().enumerate() {
        for (track, count) in per_track.iter().enumerate() {
            assert!(
                *count <= KF_MOST_REQUESTS,
                "publisher {publisher} track {track} was asked {count} times in 20 s"
            );
        }
    }
    // The tracks that are being sent were asked at all: main of the
    // first, middle of the second, light of every one.
    assert!(requests[0][0] > 0 && requests[1][1] > 0);
    assert!(requests.iter().all(|per_track| per_track[2] > 0));
}

/// The same with publishers whose packets carry no frame descriptor: the
/// SFU never sees a key frame arrive, so no request is ever answered as
/// far as it can tell, and it stops asking after a bounded number.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
#[allow(clippy::print_stderr)]
async fn a_publisher_without_a_frame_descriptor_is_asked_a_bounded_number_of_times() {
    let what = "key frame requests, no frame descriptor";
    let requests = run_keyframe_requests(what, true).await;
    eprintln!("{what}: per publisher, on main / middle / light: {requests:?}");
    assert_eq!(requests.len(), 3);
    for (publisher, per_track) in requests.iter().enumerate() {
        for (track, count) in per_track.iter().enumerate() {
            assert!(
                *count <= KF_MOST_REQUESTS,
                "publisher {publisher} track {track} was asked {count} times in 20 s"
            );
        }
    }
    assert!(requests.iter().all(|per_track| per_track[2] > 0));
}

/// What the narrow-downlink tests assert of a run of [`dl_narrow_case`].
#[allow(clippy::print_stderr, clippy::too_many_lines)]
fn check_narrow_downlink(what: &str, run: &DownlinkRun) {
    use lakko_svc_sfu::protocol::VideoQuality::{Main, Middle};

    print_downlink_timeline(what, run);

    let lifted = run.lifted_at.expect("the limit was lifted");
    let one_main_two_middles = vec![(0, Main), (1, Middle), (2, Middle)];
    let all_main = vec![(0, Main), (1, Main), (2, Main)];

    // The subscriber is told as its tracks open and after its
    // `video-wants`, before anything is known of the path: everything it
    // asked for.
    let (_, first) = run
        .given
        .iter()
        .find(|(_, set)| set.len() == 3)
        .expect("a video-given frame naming all three arrived");
    assert_eq!(first, &all_main, "an unknown budget gives what was asked");

    // Settled: the last frame before the lift (before the first clump,
    // when the run has one) says one main and two middles, and it came
    // within the stated time.
    let steady_until = run
        .clumps
        .iter()
        .copied()
        .filter(|at| *at < lifted)
        .min()
        .unwrap_or(lifted);
    let (settled_at, settled) = run
        .given
        .iter()
        .rfind(|(at, _)| *at < steady_until)
        .expect("frames before the lift");
    assert_eq!(
        settled, &one_main_two_middles,
        "the SFU settles on one main and two middles; the most important publisher keeps main"
    );
    // A clump of one publisher's frames does not cost the subscriber its
    // pictures: whatever was taken is back within the stated time, and
    // one main and two middles is what stands when the limit is lifted.
    if steady_until < lifted {
        let (last_at, last) = run
            .given
            .iter()
            .rfind(|(at, _)| *at < lifted)
            .expect("frames before the lift");
        assert_eq!(
            last, &one_main_two_middles,
            "after a publisher's clump the subscriber is on one main and two middles again"
        );
        let changes = run
            .given
            .iter()
            .filter(|(at, _)| *at >= steady_until && *at < lifted)
            .count();
        // Counted from the last clump that began before the change.
        let clump_before = run
            .clumps
            .iter()
            .copied()
            .filter(|at| at <= last_at)
            .max()
            .unwrap_or(steady_until);
        let back = last_at.saturating_duration_since(clump_before);
        eprintln!(
            "{what}: the clumps before the lift: {changes} video-given frames after the first, \
             the last {:.2} s after the clump before it began",
            if changes == 0 {
                0.0
            } else {
                back.as_secs_f64()
            }
        );
        assert!(
            changes == 0 || back <= DL_CLUMP_BACK_WITHIN,
            "the steady set was back {back:?} after the clump"
        );
    }
    let settled_after = settled_at.saturating_duration_since(run.t0);
    eprintln!(
        "{what}: settled on one main and two middles {:.1} s after the start",
        settled_after.as_secs_f64()
    );
    assert!(
        settled_after <= DL_SETTLED_WITHIN,
        "settled {settled_after:?} after the start"
    );

    // What was handed over agrees with what was said: from a second after
    // the settling until the lift, publisher 0's main track and the other
    // two's middle tracks carry packets, and their other heavy tracks
    // none.
    let quiet_from = *settled_at + Duration::from_secs(1);
    let steady_until = steady_until.max(quiet_from);
    let handed = |publisher: usize, role: DlRole, from: Instant, until: Instant| {
        run.got
            .iter()
            .filter(|p| p.publisher == publisher && p.role == role && p.at >= from && p.at < until)
            .count()
    };
    assert!(handed(0, DlRole::Main, quiet_from, steady_until) > 0);
    assert_eq!(handed(0, DlRole::Middle, quiet_from, steady_until), 0);
    for publisher in [1, 2] {
        assert!(handed(publisher, DlRole::Middle, quiet_from, steady_until) > 0);
        assert_eq!(handed(publisher, DlRole::Main, quiet_from, steady_until), 0);
    }

    // No sustained loss on the light tracks: once settled, the link
    // loses none of their packets and the subscriber is handed every one.
    let light_lost_settled = run
        .link_dropped
        .iter()
        .filter(|(at, _, role, _)| *role == DlRole::Light && *at >= quiet_from && *at < lifted)
        .count();
    let light_lost_before = run
        .link_dropped
        .iter()
        .filter(|(at, _, role, rtx)| *role == DlRole::Light && !*rtx && *at < quiet_from)
        .count();
    let holes_settled = dl_light_holes(run, quiet_from, lifted);
    let holes_before = dl_light_holes(run, run.t0, quiet_from);
    let light_handed: usize = (0..3)
        .map(|p| handed(p, DlRole::Light, quiet_from, lifted))
        .sum();
    eprintln!(
        "{what}: light tracks: before settling the link lost {light_lost_before} first \
         transmissions and {holes_before} were never handed over; once settled the link lost \
         {light_lost_settled} (retransmissions included) and {holes_settled} of {light_handed} \
         were never handed over"
    );
    assert_eq!(
        light_lost_settled, 0,
        "once settled the link drops no light packet"
    );
    assert_eq!(
        holes_settled, 0,
        "once settled every light packet is handed over"
    );

    // The lesson: with the limit lifted the SFU finds out, though it is
    // sending little, and gives all three their main picture back.
    let (back_at, _) = run
        .given
        .iter()
        .find(|(at, set)| *at >= lifted && set == &all_main)
        .expect("all three are given main again after the lift");
    let back_after = back_at.saturating_duration_since(lifted);
    eprintln!(
        "{what}: all three back on main {:.1} s after the limit was lifted",
        back_after.as_secs_f64()
    );
    assert!(
        back_after <= DL_BACK_WITHIN,
        "back {back_after:?} after the lift"
    );
    assert_eq!(
        run.given.last().map(|(_, set)| set),
        Some(&all_main),
        "and it stays there"
    );
    for publisher in 0..3 {
        assert!(
            handed(publisher, DlRole::Main, *back_at, run.ended_at) > 0,
            "publisher {publisher}'s main track carries packets again"
        );
    }

    // A publisher's own bitrate control is as before step 6: the answer
    // to its offer negotiates no transport-wide congestion control on
    // the lines it publishes on, and keeps REMB and abs-send-time.
    for answer in run.publisher_answers.iter().chain([&run.subscriber_answer]) {
        assert!(
            !answer.contains("transport-cc") && !answer.contains("transport-wide-cc"),
            "the answer to a peer's own offer names no transport-wide congestion control:\n\
             {answer}"
        );
    }
    for answer in &run.publisher_answers {
        for section in video_sections(answer) {
            assert!(section.contains("goog-remb") && section.contains("abs-send-time"));
        }
    }
    // The lines the SFU sends on carry both.
    assert!(!run.sfu_offers.is_empty());
    let last_offer = run.sfu_offers.last().expect("an offer");
    let sections = video_sections(last_offer);
    // (The subscriber's own receive-only line is one the SFU could send
    // on and never does: it opens a line of its own for every track.)
    let sends_on =
        |s: &&String| s.contains("a=sendonly") && !s.contains("frame-descriptor-subscriber");
    for section in sections.iter().filter(sends_on) {
        assert!(
            section.contains("transport-wide-cc") && section.contains("transport-cc"),
            "a line the SFU sends on negotiates transport-wide congestion control:\n{section}"
        );
    }
    assert_eq!(sections.iter().filter(sends_on).count(), 9);

    // The link drops nothing after its first two seconds, to the end.
    let late_drops = run
        .link_dropped
        .iter()
        .filter(|(at, _, _, _)| at.saturating_duration_since(run.t0) > DL_NO_DROPS_AFTER)
        .count();
    let early_drops = run.link_dropped.len() - late_drops;
    eprintln!(
        "{what}: the link dropped {early_drops} datagrams in its first two seconds and \
         {late_drops} after"
    );
    assert_eq!(
        late_drops, 0,
        "no drops on the link after the first two seconds"
    );
}

/// On a downlink with no limit nobody is ever given less than was asked
/// for.
///
/// An acceptance test of ADR-0070 step 6: the estimate starts at its
/// ceiling, nothing on a loopback link is a standing queue, and a probe
/// is never needed because nobody is given less than was asked for.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
#[allow(clippy::print_stderr)]
async fn a_subscriber_on_an_unlimited_downlink_is_never_downgraded() {
    use lakko_svc_sfu::protocol::VideoQuality::Main;

    let what = "unlimited downlink";
    let case = DownlinkCase {
        limit_bps: None,
        queue_limit: 0,
        lift_after: Duration::ZERO,
        run_for: Duration::from_secs(25),
        downlink_control: true,
        delay: Duration::ZERO,
        jitter: Duration::ZERO,
        dip_after: None,
        clump_at: None,
        clump_all_at: None,
        clump_after_lift: None,
        subscriber_keeps_asking: false,
        asking_stops_after: None,
        no_descriptor: false,
        mobile: None,
    };
    let run = run_downlink_measured(what, case).await;
    print_downlink_timeline(what, &run);

    assert!(!run.given.is_empty(), "the subscriber was told");
    // (The first frames list the publishers as the subscriber's tracks of
    // them open, so they can name fewer than three.)
    for (at, set) in &run.given {
        assert!(
            set.iter().all(|(_, quality)| *quality == Main),
            "{:?} after the start: {set:?}",
            at.saturating_duration_since(run.t0)
        );
    }
    assert_eq!(
        run.given.last().map(|(_, set)| set),
        Some(&vec![(0, Main), (1, Main), (2, Main)])
    );
    for sample in &run.samples {
        assert_eq!(
            (sample.snap.middle, sample.snap.light),
            (0, 0),
            "nobody is given less than main"
        );
    }
    // Every main track flowed in every second of the run.
    let whole = run.ended_at.saturating_duration_since(run.t0).as_secs();
    for second in 1..whole {
        let from = run.t0 + Duration::from_secs(second);
        for publisher in 0..3 {
            let handed = run
                .got
                .iter()
                .filter(|p| {
                    p.publisher == publisher
                        && p.role == DlRole::Main
                        && p.at >= from
                        && p.at < from + Duration::from_secs(1)
                })
                .count();
            assert!(
                handed > 100,
                "publisher {publisher}'s main track carried {handed} packets in second {second}"
            );
        }
    }
    assert!(run.link_dropped.is_empty());
}

/// `LAKKO_SFU_DOWNLINK_CONTROL` off: nothing of step 6. On the narrow link
/// of the test above the subscriber is told nothing, everything goes on
/// being sent into a link that does not carry it, and no SDP the SFU
/// writes names transport-wide congestion control.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
#[allow(clippy::print_stderr)]
async fn downlink_control_switched_off_changes_nothing() {
    const LIMIT_BPS: u64 = 7_500_000;
    let _one = DL_ONE_AT_A_TIME.lock().await;
    let _watchdog = DlWatchdog::start("downlink control off", Duration::from_secs(120));
    let run = Box::pin(run_downlink(DownlinkCase {
        limit_bps: Some(LIMIT_BPS),
        queue_limit: (LIMIT_BPS / 8 * 150 / 1000) as usize,
        lift_after: Duration::from_secs(600),
        run_for: Duration::from_secs(12),
        downlink_control: false,
        delay: Duration::ZERO,
        jitter: Duration::ZERO,
        dip_after: None,
        clump_at: None,
        clump_all_at: None,
        clump_after_lift: None,
        subscriber_keeps_asking: false,
        asking_stops_after: None,
        no_descriptor: false,
        mobile: None,
    }))
    .await;
    print_downlink_timeline("downlink control off", &run);

    assert!(run.given.is_empty(), "no video-given frame");
    assert!(run.samples.is_empty(), "no downlink state at all");
    for sdp in run
        .publisher_answers
        .iter()
        .chain([&run.subscriber_answer])
        .chain(&run.sfu_offers)
    {
        assert!(
            !sdp.contains("transport-cc") && !sdp.contains("transport-wide-cc"),
            "no SDP names transport-wide congestion control:\n{sdp}"
        );
    }
    // Everything goes on being sent, into a link that does not carry it:
    // long after the first seconds the link is still losing packets of
    // the main tracks and of the light tracks alike. (Not "in the last
    // seconds": under this flood the test's own loop falls behind, by
    // seconds at a time, and with it its publishers.)
    let later = run.t0 + Duration::from_secs(4);
    for role in [DlRole::Main, DlRole::Light] {
        assert!(
            run.link_dropped
                .iter()
                .any(|(at, _, r, _)| *r == role && *at >= later),
            "the link is still losing {role:?} packets"
        );
    }
}
