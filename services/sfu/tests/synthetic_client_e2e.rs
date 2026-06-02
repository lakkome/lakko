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
use str0m::media::{Direction, Frequency, MediaKind, MediaTime, Mid};
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
        .accept_offer(client_id.clone(), offer_sdp)
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
        .accept_offer(b_id.clone(), b_offer)
        .await
        .expect("SFU accepts B's RecvOnly subscriber offer");
    b.accept_answer(b_pending, &b_answer)
        .expect("B applies the SFU's answer");

    // A joins as a publisher (SendOnly audio).
    let (a_offer, a_pending, publisher_mid) = a.create_audio_publisher_offer();
    let a_answer = media
        .accept_offer(a_id.clone(), a_offer)
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
