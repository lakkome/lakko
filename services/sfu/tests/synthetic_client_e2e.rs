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
//! [`Peer`] in the [`PeerRegistry`] — and a `MediaData`
//! frame written by A surfaces **verbatim** at B. This
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

#![cfg(test)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lakko_svc_sfu::media::{spawn_media_task, MediaTaskHandle};
use lakko_svc_sfu::peer::{Peer, PeerId, PeerRegistry};
use lakko_svc_sfu::protocol::ServerFrame;
use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::media::{
    Direction, Frequency, MediaKind, MediaTime, Mid, Rid, Simulcast, SimulcastLayer,
};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, Input, Output, Rtc};
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
}

impl SyntheticClient {
    async fn new() -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("UDP bind for synthetic client");
        let local_addr = socket.local_addr().expect("synthetic client local_addr");
        let mut rtc = Rtc::new(Instant::now());
        let candidate =
            Candidate::host(local_addr, Protocol::Udp).expect("host candidate constructs");
        rtc.add_local_candidate(candidate);
        Self {
            rtc,
            socket: Arc::new(socket),
            local_addr,
            connected: false,
        }
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
    /// SFU surfaces each frame's originating rid on `Event::MediaData`, which is
    /// exactly what the simulcast layer gate keys on.
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
    /// `Output::Transmit` out the socket, flips
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
                    let _ = socket.send_to(&t.contents, t.destination).await;
                },
                Ok(Output::Event(event)) => match event {
                    Event::Connected => self.connected = true,
                    Event::MediaData(data) => payloads.push(data.data.clone()),
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

    /// Feed an inbound datagram (if it came from the SFU).
    fn feed_receive(&mut self, bytes: &[u8], source: SocketAddr, sfu_addr: SocketAddr) {
        if source != sfu_addr {
            return;
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
/// by `MediaPeer::accept_offer`, the SFU surfaces per-rid `Event::MediaData`,
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
        .participant_key(a_leaf, lakko_calls::sframe::MediaType::Audio)
        .expect("A derives its sender participant key");
    let b_receiver = CallKey::from_seed(seed)
        .participant_key(a_leaf, lakko_calls::sframe::MediaType::Audio)
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
    const GOLDEN_PLAINTEXT: &[u8] = b"lakko sframe golden vector v1";
    const GOLDEN_WIRE_HEX: &str =
        "31093c4f19a3b815c0a17a8ad09d2df18c6971987eb1c214484e9ebcc4f92b4c04e5c80bc145a9f23e1c60e2faa4";
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
        .participant_key(GOLDEN_LEAF, lakko_calls::sframe::MediaType::Audio)
        .expect("A derives the golden sender participant key");
    let b_receiver = CallKey::from_seed(GOLDEN_SEED)
        .participant_key(GOLDEN_LEAF, lakko_calls::sframe::MediaType::Audio)
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
/// because `propagate_new_tracks_in` + `fanout_media_data` are
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
        .participant_key(a_leaf, lakko_calls::sframe::MediaType::Audio)
        .expect("A derives its sender participant key");
    let b_receiver = CallKey::from_seed(seed)
        .participant_key(a_leaf, lakko_calls::sframe::MediaType::Audio)
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
        assert_eq!(
            u64::from(a_leaf),
            header.kid,
            "KID is A's leaf on every frame"
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
