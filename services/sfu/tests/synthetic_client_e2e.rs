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
//! ## Why only one synthetic client (for now)
//!
//! The full two-client cross-fanout test —
//! "client A writes MediaData, the SFU forwards it
//! verbatim to client B" — needs the renegotiation wire
//! to drive client B's `Rtc::sdp_api().accept_offer(..)`
//! against the SFU-initiated `ServerFrame::SdpOffer`.
//! That requires intercepting the offer the
//! `drive_renegotiations` pass dispatches to client B's
//! outbound mpsc, which in turn means installing fake
//! `Peer` entries in the `PeerRegistry` whose outbound
//! channels the test owns. The plumbing is mechanically
//! straightforward but adds another ~150 lines on top
//! of this file; landing the single-client DTLS path
//! first lets us prove the bedrock works before stacking
//! the cross-fanout layer on top. Two-client
//! cross-fanout is tracked as the follow-up sub-sprint
//! `S-3-B-tracks step 3b`.
//!
//! ## What this test catches
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

use lakko_svc_sfu::media::spawn_media_task;
use lakko_svc_sfu::peer::PeerRegistry;
use str0m::change::{SdpAnswer, SdpPendingOffer};
use str0m::media::{Direction, MediaKind};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, Input, Output, Rtc};
use tokio::net::UdpSocket;

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
    /// receives the audio it would forward.
    fn create_audio_publisher_offer(&mut self) -> (String, SdpPendingOffer) {
        let mut change = self.rtc.sdp_api();
        let _mid = change.add_media(
            MediaKind::Audio,
            Direction::SendOnly,
            Some("synthetic-publisher".to_owned()),
            None,
            None,
        );
        let (offer, pending) = change.apply().expect("at least one change → Some(offer)");
        (offer.to_sdp_string(), pending)
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
    let (offer_sdp, pending) = client.create_audio_publisher_offer();
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
