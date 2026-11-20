// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The call step: SFrame-sealed audio from A reaches B through the SFU.
//!
//! Both accounts take a `call-join` token and a call-admission JWT for a
//! fresh room, open the SFU signaling WebSocket, and drive a `str0m` peer
//! connection on a UDP socket of their own. B subscribes (one `RecvOnly`
//! audio line), A publishes (one `SendOnly` audio line); the SFU then offers
//! B the downstream track and B answers. A sends frames of random bytes
//! standing in for Opus, each sealed with the per-call key every member
//! derives from the MLS group (`lakko_calls::CallKey::derive`, C-1, the
//! ADR-0071 media frame); B opens what arrives with the same derivation.
//! The step succeeds when [`FRAMES_NEEDED`] frames open and match what A
//! sent.
//!
//! The loop follows `services/sfu/tests/synthetic_client_e2e.rs`
//! (`run_cross_fanout`), the reference str0m client the SFU is tested with.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{SinkExt as _, StreamExt as _};
use lakko_calls::{CallKey, CallKeyRing, MediaCodec};
use lakko_sfu_proto::{
    decode_server_frame, encode_client_frame, ClientFrame, ServerFrame, SIGNALING_SUBPROTOCOL,
    TOKEN_SUBPROTOCOL_PREFIX,
};
use rand::RngCore as _;
use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::format::Codec;
use str0m::media::{Direction, Frequency, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, Input, Output, Rtc};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpStream, UdpSocket};
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use crate::error::{Error, Result};

/// The whole step must finish within this.
pub(crate) const CALL_DEADLINE: Duration = Duration::from_secs(20);
/// Frames A sends, one every [`FRAME_INTERVAL`].
pub(crate) const FRAMES_TO_SEND: u64 = 50;
/// Frames that must open at B and match for the step to pass.
pub(crate) const FRAMES_NEEDED: usize = 10;
/// One 20 ms Opus frame.
const FRAME_INTERVAL: Duration = Duration::from_millis(20);
/// Bytes of each stand-in frame.
const FRAME_LEN: usize = 60;
/// The driving loop's tick.
const TICK: Duration = Duration::from_millis(5);

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

type Ws = WebSocketStream<Box<dyn Io>>;

/// What the step measured.
pub(crate) struct CallResult {
    /// From A's first frame to B's first opened frame.
    pub(crate) first_frame_latency: Duration,
    /// Frames that opened at B and matched.
    pub(crate) matched: usize,
}

/// The inputs of one call: the connect URL, the two JWTs, the per-call
/// seed both members derived, and A's leaf (the sender's KID).
pub(crate) struct CallInputs {
    /// `ws(s)://.../v1/connect`.
    pub(crate) connect_url: String,
    /// A's admission token.
    pub(crate) jwt_a: String,
    /// B's admission token.
    pub(crate) jwt_b: String,
    /// The seed A derived from its group copy.
    pub(crate) seed_a: [u8; 32],
    /// The seed B derived from its group copy.
    pub(crate) seed_b: [u8; 32],
    /// A's leaf index in the group.
    pub(crate) leaf_a: u32,
}

/// A fresh 16-byte call id.
#[must_use]
pub(crate) fn fresh_call_id() -> [u8; 16] {
    let mut id = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut id);
    id
}

/// The host and port of a `ws(s)` URL, and whether it is TLS.
fn ws_target(url: &str) -> Result<(String, u16, bool)> {
    let parsed = reqwest::Url::parse(url).map_err(|_| Error::Usage("sfu url".to_owned()))?;
    let tls = match parsed.scheme() {
        "wss" => true,
        "ws" => false,
        _ => return Err(Error::Usage("sfu url scheme".to_owned())),
    };
    let host = parsed
        .host_str()
        .ok_or_else(|| Error::Usage("sfu url host".to_owned()))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    let port = parsed
        .port_or_known_default()
        .unwrap_or(if tls { 443 } else { 80 });
    Ok((host, port, tls))
}

/// The local address that routes toward the SFU host: the host candidate
/// str0m advertises (a socket bound to `0.0.0.0` cannot be one).
async fn local_ip_toward(host: &str, port: u16) -> Result<IpAddr> {
    let remote = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| Error::Network("resolve"))?
        .next()
        .ok_or(Error::Network("resolve"))?;
    let bind: SocketAddr = if remote.is_ipv4() {
        "0.0.0.0:0".parse().map_err(|_| Error::Network("bind"))?
    } else {
        "[::]:0".parse().map_err(|_| Error::Network("bind"))?
    };
    let probe = UdpSocket::bind(bind)
        .await
        .map_err(|_| Error::Network("bind"))?;
    probe
        .connect(remote)
        .await
        .map_err(|_| Error::Network("route"))?;
    Ok(probe
        .local_addr()
        .map_err(|_| Error::Network("route"))?
        .ip())
}

async fn connect_ws(url: &str, jwt: &str) -> Result<Ws> {
    let (host, port, tls) = ws_target(url)?;
    let tcp = tokio::time::timeout(
        Duration::from_secs(10),
        TcpStream::connect((host.as_str(), port)),
    )
    .await
    .map_err(|_| Error::Timeout("ws-connect"))?
    .map_err(|_| Error::Network("ws-connect"))?;
    let _ = tcp.set_nodelay(true);
    let stream: Box<dyn Io> = if tls {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| Error::Network("tls-config"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let name = rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|_| Error::Usage("sfu host".to_owned()))?;
        let conn = tokio_rustls::TlsConnector::from(Arc::new(config))
            .connect(name, tcp)
            .await
            .map_err(|_| Error::Network("tls"))?;
        Box::new(conn)
    } else {
        Box::new(tcp)
    };
    let mut request = url
        .into_client_request()
        .map_err(|_| Error::Usage("sfu url".to_owned()))?;
    let protocols = format!("{SIGNALING_SUBPROTOCOL}, {TOKEN_SUBPROTOCOL_PREFIX}{jwt}");
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_str(&protocols).map_err(|_| Error::Protocol("jwt-header"))?,
    );
    let (ws, _) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::client_async(request, stream),
    )
    .await
    .map_err(|_| Error::Timeout("ws-handshake"))?
    .map_err(|_| Error::Network("ws-handshake"))?;
    Ok(ws)
}

async fn send_frame(ws: &mut Ws, frame: &ClientFrame) -> Result<()> {
    let text = encode_client_frame(frame).map_err(|_| Error::Protocol("encode"))?;
    ws.send(Message::Text(text))
        .await
        .map_err(|_| Error::Network("ws-send"))
}

/// The next signaling frame, skipping control frames.
async fn next_frame(ws: &mut Ws) -> Result<ServerFrame> {
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(t))) => {
                return decode_server_frame(&t).map_err(|_| Error::Protocol("frame"));
            },
            Some(Ok(Message::Close(_))) | None => return Err(Error::Network("ws-closed")),
            Some(Ok(_)) => {},
            Some(Err(_)) => return Err(Error::Network("ws-read")),
        }
    }
}

/// Wait for a frame the predicate accepts; an `error` frame fails.
async fn expect_frame<T>(ws: &mut Ws, mut pick: impl FnMut(ServerFrame) -> Option<T>) -> Result<T> {
    loop {
        let frame = next_frame(ws).await?;
        if let ServerFrame::Error { code, .. } = &frame {
            let _ = code;
            return Err(Error::Protocol("sfu-error"));
        }
        if let Some(t) = pick(frame) {
            return Ok(t);
        }
    }
}

struct Peer {
    rtc: Rtc,
    socket: Arc<UdpSocket>,
    local: SocketAddr,
    connected: bool,
}

impl Peer {
    async fn new(local_ip: IpAddr) -> Result<Self> {
        let socket = UdpSocket::bind(SocketAddr::new(local_ip, 0))
            .await
            .map_err(|_| Error::Network("udp-bind"))?;
        let local = socket
            .local_addr()
            .map_err(|_| Error::Network("udp-bind"))?;
        let mut rtc = Rtc::builder().build(Instant::now());
        let candidate =
            Candidate::host(local, Protocol::Udp).map_err(|_| Error::Protocol("candidate"))?;
        rtc.add_local_candidate(candidate);
        Ok(Self {
            rtc,
            socket: Arc::new(socket),
            local,
            connected: false,
        })
    }

    fn offer(&mut self, direction: Direction) -> Result<(String, SdpPendingOffer, Mid)> {
        let mut change = self.rtc.sdp_api();
        let mid = change.add_media(MediaKind::Audio, direction, None, None, None);
        let (offer, pending) = change.apply().ok_or(Error::Protocol("offer"))?;
        Ok((offer.to_sdp_string(), pending, mid))
    }

    fn accept_answer(&mut self, pending: SdpPendingOffer, sdp: &str) -> Result<()> {
        let answer = SdpAnswer::from_sdp_string(sdp).map_err(|_| Error::Protocol("answer"))?;
        self.rtc
            .sdp_api()
            .accept_answer(pending, answer)
            .map_err(|_| Error::Protocol("answer"))
    }

    fn answer_offer(&mut self, sdp: &str) -> Result<String> {
        let offer = SdpOffer::from_sdp_string(sdp).map_err(|_| Error::Protocol("reneg"))?;
        let answer = self
            .rtc
            .sdp_api()
            .accept_offer(offer)
            .map_err(|_| Error::Protocol("reneg"))?;
        Ok(answer.to_sdp_string())
    }

    /// Send what str0m wants sent; return the media frames it surfaced.
    async fn drain(&mut self) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        loop {
            match self.rtc.poll_output() {
                Ok(Output::Transmit(t)) => {
                    let _ = self.socket.send_to(&t.contents, t.destination).await;
                },
                Ok(Output::Event(Event::Connected)) => self.connected = true,
                Ok(Output::Event(Event::MediaData(d))) => frames.push(d.data),
                Ok(Output::Event(_)) => {},
                Ok(Output::Timeout(_)) | Err(_) => break,
            }
        }
        frames
    }

    fn receive(&mut self, bytes: &[u8], source: SocketAddr) {
        let Ok(contents) = bytes.try_into() else {
            return;
        };
        let _ = self.rtc.handle_input(Input::Receive(
            Instant::now(),
            Receive {
                proto: Protocol::Udp,
                source,
                destination: self.local,
                contents,
            },
        ));
    }

    fn tick(&mut self) {
        let _ = self.rtc.handle_input(Input::Timeout(Instant::now()));
    }

    /// Write one frame on `mid` under the Opus payload type.
    fn write_opus(&mut self, mid: Mid, frame: Vec<u8>, index: u64) -> bool {
        let Some(writer) = self.rtc.writer(mid) else {
            return false;
        };
        let Some(pt) = writer
            .payload_params()
            .find(|p| p.spec().codec == Codec::Opus)
            .map(str0m::format::PayloadParams::pt)
        else {
            return false;
        };
        let rtp_time = MediaTime::new(index * 960, Frequency::FORTY_EIGHT_KHZ);
        writer.write(pt, Instant::now(), rtp_time, frame).is_ok()
    }
}

/// Run the call. Closes both sockets before returning.
///
/// # Errors
///
/// Any signaling, transport or media failure, or the deadline.
pub(crate) async fn run(inputs: CallInputs) -> Result<CallResult> {
    let deadline = Instant::now() + CALL_DEADLINE;
    let (host, port, _) = ws_target(&inputs.connect_url)?;
    let local_ip = local_ip_toward(&host, port).await?;

    let mut ws_a = connect_ws(&inputs.connect_url, &inputs.jwt_a).await?;
    let mut ws_b = connect_ws(&inputs.connect_url, &inputs.jwt_b).await?;
    let result = tokio::time::timeout_at(
        deadline.into(),
        Box::pin(drive(&inputs, local_ip, &mut ws_a, &mut ws_b)),
    )
    .await
    .unwrap_or(Err(Error::Timeout("call")));
    let _ = ws_a.close(None).await;
    let _ = ws_b.close(None).await;
    result
}

#[allow(clippy::too_many_lines)]
async fn drive(
    inputs: &CallInputs,
    local_ip: IpAddr,
    ws_a: &mut Ws,
    ws_b: &mut Ws,
) -> Result<CallResult> {
    expect_frame(ws_a, |f| {
        matches!(f, ServerFrame::Welcome { .. }).then_some(())
    })
    .await?;
    expect_frame(ws_b, |f| {
        matches!(f, ServerFrame::Welcome { .. }).then_some(())
    })
    .await?;

    // B first: a subscriber with one RecvOnly line.
    let mut b = Peer::new(local_ip).await?;
    let (sdp, pending, _) = b.offer(Direction::RecvOnly)?;
    send_frame(ws_b, &offer_frame(sdp)).await?;
    let answer = expect_frame(ws_b, |f| match f {
        ServerFrame::SdpAnswer { sdp } => Some(sdp),
        _ => None,
    })
    .await?;
    b.accept_answer(pending, &answer)?;

    // Then A: a publisher with one SendOnly line.
    let mut a = Peer::new(local_ip).await?;
    let (sdp, pending, mid_a) = a.offer(Direction::SendOnly)?;
    send_frame(ws_a, &offer_frame(sdp)).await?;
    let answer = expect_frame(ws_a, |f| match f {
        ServerFrame::SdpAnswer { sdp } => Some(sdp),
        _ => None,
    })
    .await?;
    a.accept_answer(pending, &answer)?;

    let mut sender = CallKey::from_seed(inputs.seed_a)
        .participant_key_for(inputs.leaf_a, 0)
        .map_err(|_| Error::Crypto("sframe-key"))?;
    let ring = CallKeyRing::from_seed(inputs.seed_b);

    let mut pending_offer_b: Option<String> = None;
    let mut reneg_answered = false;
    let mut sent: HashSet<Vec<u8>> = HashSet::new();
    let mut sent_count: u64 = 0;
    let mut next_send = Instant::now();
    let mut first_sent: Option<Instant> = None;
    let mut first_opened: Option<Instant> = None;
    let mut matched = 0usize;
    let mut abuf = vec![0u8; 2048];
    let mut bbuf = vec![0u8; 2048];

    loop {
        a.drain().await;
        for frame in b.drain().await {
            if let Ok(plain) = ring.try_open_media_frame_with_overlap(&frame) {
                if sent.contains(&plain) {
                    matched += 1;
                    first_opened.get_or_insert_with(Instant::now);
                }
            }
        }
        if matched >= FRAMES_NEEDED {
            let (Some(s), Some(o)) = (first_sent, first_opened) else {
                return Err(Error::Protocol("timing"));
            };
            return Ok(CallResult {
                first_frame_latency: o.saturating_duration_since(s),
                matched,
            });
        }

        // The SFU's offer of A's track to B; answered once B is connected.
        if !reneg_answered && b.connected {
            if let Some(sdp) = pending_offer_b.take() {
                let answer = b.answer_offer(&sdp)?;
                send_frame(ws_b, &ClientFrame::SdpAnswer { sdp: answer }).await?;
                reneg_answered = true;
            }
        }

        if a.connected && b.connected && reneg_answered && sent_count < FRAMES_TO_SEND {
            let now = Instant::now();
            if now >= next_send {
                let mut plain = vec![0u8; FRAME_LEN];
                rand::thread_rng().fill_bytes(&mut plain);
                let sealed = sender
                    .seal_media_frame_for_codec(&plain, MediaCodec::Opus)
                    .map_err(|_| Error::Crypto("sframe-seal"))?;
                if a.write_opus(mid_a, sealed, sent_count) {
                    sent.insert(plain);
                    sent_count += 1;
                    first_sent.get_or_insert(now);
                    next_send = now + FRAME_INTERVAL;
                }
            }
        }

        let a_sock = a.socket.clone();
        let b_sock = b.socket.clone();
        tokio::select! {
            r = a_sock.recv_from(&mut abuf) => {
                if let Ok((n, src)) = r {
                    a.receive(&abuf[..n], src);
                }
            }
            r = b_sock.recv_from(&mut bbuf) => {
                if let Ok((n, src)) = r {
                    b.receive(&bbuf[..n], src);
                }
            }
            f = next_frame(ws_b) => {
                match f? {
                    ServerFrame::SdpOffer { sdp } => pending_offer_b = Some(sdp),
                    ServerFrame::Error { .. } => return Err(Error::Protocol("sfu-error")),
                    _ => {},
                }
            }
            f = next_frame(ws_a) => {
                match f? {
                    ServerFrame::SdpOffer { sdp } => {
                        let answer = a.answer_offer(&sdp)?;
                        send_frame(ws_a, &ClientFrame::SdpAnswer { sdp: answer }).await?;
                    }
                    ServerFrame::Error { .. } => return Err(Error::Protocol("sfu-error")),
                    _ => {},
                }
            }
            () = tokio::time::sleep(TICK) => {
                a.tick();
                b.tick();
            }
        }
    }
}

fn offer_frame(sdp: String) -> ClientFrame {
    ClientFrame::SdpOffer {
        sdp,
        light_mids: Vec::new(),
        middle_mids: Vec::new(),
        screen_mids: Vec::new(),
    }
}

/// The per-call seed a member derives from its group copy (C-1).
///
/// # Errors
///
/// [`Error::Mls`] when the exporter fails.
pub(crate) fn call_seed(
    group: &lakko_mls::Group,
    device: &lakko_mls::Identity,
    call_id: &[u8; 16],
) -> Result<[u8; 32]> {
    let key = CallKey::derive(group, device, call_id).map_err(|_| Error::Mls("call-key"))?;
    Ok(*key.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `wss` path against a live SFU: the TLS handshake must succeed and
    /// the SFU must then refuse the bogus token at the WebSocket upgrade.
    /// Network-dependent, so ignored by default:
    ///   LAKKO_CANARY_WSS_PROBE=wss://test.lakko.me/sfu/v1/connect \
    ///     cargo test -p lakko-canary-tool -- --ignored wss_tls_handshake
    #[tokio::test]
    #[ignore = "needs network and a live SFU"]
    async fn wss_tls_handshake_reaches_the_sfu() {
        let url = std::env::var("LAKKO_CANARY_WSS_PROBE")
            .unwrap_or_else(|_| "wss://test.lakko.me/sfu/v1/connect".to_owned());
        let outcome = connect_ws(&url, "not-a-jwt").await;
        assert!(
            matches!(outcome, Err(Error::Network("ws-handshake"))),
            "expected TLS to pass and the upgrade to be refused"
        );
    }

    #[test]
    fn ws_targets() {
        assert_eq!(
            ws_target("wss://test.lakko.me/sfu/v1/connect").unwrap(),
            ("test.lakko.me".to_owned(), 443, true)
        );
        assert_eq!(
            ws_target("ws://127.0.0.1:18184/v1/connect").unwrap(),
            ("127.0.0.1".to_owned(), 18184, false)
        );
        assert!(ws_target("https://x/v1/connect").is_err());
    }

    #[test]
    fn a_sealed_frame_opens_with_the_receivers_ring_and_nothing_else() {
        let seed = [7u8; 32];
        let mut pk = CallKey::from_seed(seed).participant_key_for(3, 0).unwrap();
        let sealed = pk
            .seal_media_frame_for_codec(&[1u8; FRAME_LEN], MediaCodec::Opus)
            .unwrap();
        assert_eq!(
            CallKeyRing::from_seed(seed)
                .try_open_media_frame_with_overlap(&sealed)
                .unwrap(),
            vec![1u8; FRAME_LEN]
        );
        assert!(CallKeyRing::from_seed([8u8; 32])
            .try_open_media_frame_with_overlap(&sealed)
            .is_err());
    }
}
