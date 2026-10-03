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
use lakko_svc_sfu::media::{spawn_media_task, spawn_media_task_advertising, MediaTaskHandle};
use lakko_svc_sfu::peer::{Peer, PeerId, PeerRegistry};
use lakko_svc_sfu::protocol::ServerFrame;
use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::media::{
    Direction, Frequency, KeyframeRequestKind, MediaKind, MediaTime, Mid, Rid, Simulcast,
    SimulcastLayer,
};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, Input, Output, Rtc, RtcConfig};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

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
        }
    }

    /// The main SSRC, and the retransmission SSRC if there is one, this
    /// client sends `mid` on.
    fn send_ssrcs(&mut self, mid: Mid) -> Option<(u32, Option<u32>)> {
        let mut api = self.rtc.direct_api();
        let stream = api.stream_tx_by_mid(mid, None)?;
        Some((*stream.ssrc(), stream.rtx().map(|rtx| *rtx)))
    }

    /// Whether an outgoing datagram goes on the wire. Media datagrams are
    /// logged either way; everything else (STUN, DTLS, RTCP) always goes.
    fn keep_outgoing(&mut self, datagram: &[u8]) -> bool {
        let Some(rtp) = rtp_seen(datagram) else {
            return true;
        };
        let lose_once = self.drop_next_on == Some(rtp.ssrc);
        if lose_once {
            self.drop_next_on = None;
        }
        if lose_once || self.drop_all_on.contains(&rtp.ssrc) {
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
    async fn drain(&mut self) -> Vec<Vec<u8>> {
        // Clone the Arc so we can `send_to` (shared borrow
        // of the socket) while `poll_output` holds a mut
        // borrow of `self.rtc`.
        let socket = self.socket.clone();
        let mut payloads = Vec::new();
        loop {
            match self.rtc.poll_output() {
                Ok(Output::Transmit(t)) => {
                    if self.keep_outgoing(&t.contents) {
                        let _ = socket.send_to(&t.contents, t.destination).await;
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
                    Event::KeyframeRequest(_) => self.keyframe_requests_seen += 1,
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
        if source != sfu_addr {
            return;
        }
        if let Some(rtp) = rtp_seen(bytes) {
            self.rtp_in.push(rtp);
        }
        let Ok(contents) = bytes.try_into() else {
            return;
        };
        let input = Input::Receive(
            Instant::now(),
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
    let client_id = lakko_svc_sfu::peer::PeerId::fresh();

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
    let client_id = lakko_svc_sfu::peer::PeerId::fresh();

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
    let b_id = PeerId::fresh();
    let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
    let registered = peers
        .register(Peer {
            id: b_id.clone(),
            call_id: "cross-fanout-test".to_owned(),
            account_id: "subscriber-b".to_owned(),
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
    let a_id = PeerId::fresh();

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
    let b_id = PeerId::fresh();
    let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
    assert!(
        peers
            .register(Peer {
                id: b_id.clone(),
                call_id: "simulcast-test".to_owned(),
                account_id: "subscriber-b".to_owned(),
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
    let a_id = PeerId::fresh();

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
    let b_id = PeerId::fresh();
    let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
    let registered = peers
        .register(Peer {
            id: b_id.clone(),
            call_id: "sframe-fanout-test".to_owned(),
            account_id: "subscriber-b".to_owned(),
            outbound: Some(b_tx),
        })
        .await;
    assert!(registered, "fresh PeerId must register cleanly");

    let (media, sfu_addr) = spawn_media_task("127.0.0.1:0".parse().unwrap(), peers)
        .await
        .expect("UDP bind on loopback:0 succeeds");

    let mut a = SyntheticClient::new().await;
    let mut b = SyntheticClient::new().await;
    let a_id = PeerId::fresh();

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
    let b_id = PeerId::fresh();
    let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
    let registered = peers
        .register(Peer {
            id: b_id.clone(),
            call_id: "golden-fanout-call".to_owned(),
            account_id: "subscriber-b".to_owned(),
            outbound: Some(b_tx),
        })
        .await;
    assert!(registered, "fresh PeerId must register cleanly");

    let (media, sfu_addr) = spawn_media_task("127.0.0.1:0".parse().unwrap(), peers)
        .await
        .expect("UDP bind on loopback:0 succeeds");

    let mut a = SyntheticClient::new().await;
    let mut b = SyntheticClient::new().await;
    let a_id = PeerId::fresh();

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

    // 2. Seal the GOLDEN VECTOR as A's sender and assert the wire is EXACTLY the
    //    canonical cross-stack bytes — the same wire web / iOS / Android each
    //    produce + consume. This is what links the live-SFU forward to the
    //    cross-runtime golden equality.
    let mut a_sender = CallKey::from_seed(GOLDEN_SEED)
        .participant_key_for(GOLDEN_LEAF, GOLDEN_STREAM)
        .expect("A derives the golden sender participant key");
    let b_receiver = CallKey::from_seed(GOLDEN_SEED)
        .participant_key_for(GOLDEN_LEAF, GOLDEN_STREAM)
        .expect("B derives the golden receiver participant key");
    let sealed = a_sender
        .seal_framed(GOLDEN_PLAINTEXT)
        .expect("A seals the golden frame");
    assert_eq!(
        sealed, golden_wire,
        "the sealed wire must be the canonical cross-stack golden vector — a \
         mismatch is a wire-format regression (also caught by the lakko-calls \
         golden test + the web / iOS / Android golden tests)"
    );

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
    let b_id = PeerId::fresh();
    let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
    let registered = peers
        .register(Peer {
            id: b_id.clone(),
            call_id: "call-beta".to_owned(),
            account_id: "subscriber-b".to_owned(),
            outbound: Some(b_tx),
        })
        .await;
    assert!(registered, "fresh PeerId must register cleanly");

    let (media, sfu_addr) = spawn_media_task("127.0.0.1:0".parse().unwrap(), peers)
        .await
        .expect("UDP bind on loopback:0 succeeds");

    let mut a = SyntheticClient::new().await;
    let mut b = SyntheticClient::new().await;
    let a_id = PeerId::fresh();

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
    let b_id = PeerId::fresh();
    let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
    let registered = peers
        .register(Peer {
            id: b_id.clone(),
            call_id: "sframe-stream-test".to_owned(),
            account_id: "subscriber-b".to_owned(),
            outbound: Some(b_tx),
        })
        .await;
    assert!(registered, "fresh PeerId must register cleanly");

    let (media, sfu_addr) = spawn_media_task("127.0.0.1:0".parse().unwrap(), peers)
        .await
        .expect("UDP bind on loopback:0 succeeds");

    let mut a = SyntheticClient::new().await;
    let mut b = SyntheticClient::new().await;
    let a_id = PeerId::fresh();

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
    for (i, wire) in sealed_frames.iter().enumerate() {
        let expected_ctr = (i + 1) as u64;
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
            "frame {i} must carry counter {expected_ctr} (C-5 monotone, 1-based)"
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
    let b_id = PeerId::fresh();
    let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
    assert!(
        peers
            .register(Peer {
                id: b_id.clone(),
                call_id: "frame-descriptor-test".to_owned(),
                account_id: "subscriber-b".to_owned(),
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
    let a_id = PeerId::fresh();

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
    let b_id = PeerId::fresh();
    let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
    assert!(
        peers
            .register(Peer {
                id: b_id.clone(),
                call_id: "audio-only-join".to_owned(),
                account_id: "subscriber-b".to_owned(),
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
    let a_id = PeerId::fresh();

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
    let b_id = PeerId::fresh();
    let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
    assert!(
        peers
            .register(Peer {
                id: b_id.clone(),
                call_id: "late-joiner".to_owned(),
                account_id: "subscriber-b".to_owned(),
                outbound: Some(b_tx),
            })
            .await,
        "fresh PeerId must register cleanly"
    );

    let (media, sfu_addr) = spawn_media_task("127.0.0.1:0".parse().unwrap(), peers)
        .await
        .expect("UDP bind on loopback:0 succeeds");

    let mut a = SyntheticClient::new_with_frame_descriptor().await;
    let a_id = PeerId::fresh();

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
    let b_id = PeerId::fresh();
    let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
    assert!(
        peers
            .register(Peer {
                id: b_id.clone(),
                call_id: "keyframe-relay".to_owned(),
                account_id: "subscriber-b".to_owned(),
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
    let a_id = PeerId::fresh();
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
    let b_id = PeerId::fresh();
    let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
    assert!(
        peers
            .register(Peer {
                id: b_id.clone(),
                call_id: "audio-gap-test".to_owned(),
                account_id: "subscriber-b".to_owned(),
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
    let a_id = PeerId::fresh();

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
        let b_id = PeerId::fresh();
        let (b_tx, mut b_rx) = mpsc::channel::<ServerFrame>(64);
        assert!(
            peers
                .register(Peer {
                    id: b_id.clone(),
                    call_id: call.to_owned(),
                    account_id: "subscriber-b".to_owned(),
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
            .accept_offer(PeerId::fresh(), call.to_owned(), a_offer)
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
