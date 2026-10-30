// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Anti-censorship in the host (ADR-0085 section 3, ADR-0026,
//! `docs/ux/desktop.md` section 9).
//!
//! With the setting on, everything the page sends to a Lakko server rides an
//! obfuscated tunnel described by a signed bootstrap config: `/api/*` through
//! [`request`], the SFU signaling socket through the routes below. Call media
//! is not tunnelled, as on iOS and Android.
//!
//! **The setting on means never direct.** While the config is being fetched,
//! or when it could not be, a request is refused rather than sent the ordinary
//! way ([`Route::Refused`]). A build with no pinned trust anchor cannot arm:
//! the page says so and traffic stays direct, since no setting was honoured.
//!
//! Under [`PATH`], for the page:
//! - `GET state` -> `{enabled, phase}`;
//! - `POST enabled` `{enabled}` -> the same;
//! - `POST ws/open` `{subprotocols}` -> `{id}`: a WebSocket to the configured
//!   SFU. The page names no address, so this is no relay to anywhere else;
//! - `POST ws/<id>/send` with a text frame as the body;
//! - `GET ws/<id>/recv` -> `{frames, closed}`, waiting up to [`RECV_WAIT`];
//! - `POST ws/<id>/close`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use futures::{SinkExt as _, StreamExt as _};
use http::{header, Method, Request, Response, StatusCode};
use lakko_crypto::signing::VerifyingKey;
use lakko_transport::tungstenite::Message;
use lakko_transport::{
    tunneled_request_via_dialer, tunneled_websocket, BootstrapConfig, BootstrapDialer, DohFetcher,
    Endpoint, TunneledRequest, TunneledResponse,
};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex as AsyncMutex};
use tracing::{info, warn};

use crate::proxy::plain;

/// Where the page reaches this module on the app scheme.
pub const PATH: &str = "/__host/tunnel";

/// Set at build time; empty or absent means this build cannot arm.
const PINNED_ANCHOR: Option<&str> = option_env!("LAKKO_DESKTOP_BOOTSTRAP_TRUST_ANCHOR");
const PINNED_DOMAIN: Option<&str> = option_env!("LAKKO_DESKTOP_BOOTSTRAP_DOH_DOMAIN");
const PINNED_PATH: Option<&str> = option_env!("LAKKO_DESKTOP_BOOTSTRAP_CONFIG_PATH");

const PREFERENCE_FILE: &str = "anti-censorship";
const MAX_CONFIG_BYTES: usize = 64 * 1024;
const DIAL_DEADLINE: Duration = Duration::from_secs(30);
/// How soon a failed fetch is tried again. Chosen, not measured.
const RETRY: Duration = Duration::from_secs(60);
/// How long one `recv` waits for a frame.
const RECV_WAIT: Duration = Duration::from_secs(25);
/// Frames held for a page that is not reading; the socket waits past that.
const FRAME_QUEUE: usize = 256;
const MAX_FRAMES_PER_RECV: usize = 64;
/// A call uses one socket; a page that reloaded left its own behind.
const MAX_SOCKETS: usize = 4;
const MAX_SUBPROTOCOLS_LEN: usize = 8 * 1024;

/// What the settings screen shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The setting is off.
    Disabled,
    /// On, in a build with no pinned trust anchor.
    Unconfigured,
    /// On, the config not yet held.
    Arming,
    /// On, traffic rides the tunnel.
    Armed,
    /// On, the config could not be had.
    Unavailable,
}

impl Phase {
    const fn name(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Unconfigured => "unconfigured",
            Self::Arming => "arming",
            Self::Armed => "armed",
            Self::Unavailable => "unavailable",
        }
    }
}

/// How the next request leaves the host.
pub enum Route {
    /// Over the host's ordinary HTTP client.
    Direct,
    /// Through this dialer.
    Tunnel(Arc<BootstrapDialer>),
    /// Not at all (invariant #4).
    Refused,
}

/// The rendezvous parameters built into the app. The anchor is what acceptance
/// rests on; the domain and path only say where to look.
struct Pin {
    anchor: VerifyingKey,
    doh_domain: String,
    config_path: String,
}

impl Pin {
    fn parse(anchor: Option<&str>, domain: Option<&str>, path: Option<&str>) -> Option<Self> {
        let present = |v: Option<&str>| {
            v.map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let bytes = B64.decode(present(anchor)?).ok()?;
        Some(Self {
            anchor: VerifyingKey::from_bytes(&bytes).ok()?,
            doh_domain: present(domain)?,
            config_path: present(path)?,
        })
    }
}

/// A config that passed verification.
struct Held {
    dialer: Arc<BootstrapDialer>,
    issued_at: u64,
    expires_at: u64,
}

struct State {
    started: bool,
    enabled: bool,
    phase: Phase,
    held: Option<Held>,
    /// Moves on every change of the setting, so a running fetch knows it is
    /// no longer wanted.
    generation: u64,
}

/// The server every tunnel reaches.
pub struct Origin {
    /// Host name, also the `Host` header.
    pub host: String,
    /// Port of the public origin.
    pub port: u16,
}

/// Anti-censorship state of this process.
pub struct Tunnel {
    pin: Option<Pin>,
    origin: Origin,
    /// The inner `ws://` URL of the SFU's connect route.
    sfu_url: String,
    dir: Mutex<Option<PathBuf>>,
    state: Mutex<State>,
    sockets: Mutex<HashMap<u64, Arc<Socket>>>,
    next_socket: AtomicU64,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The route for one request. Before the setting has been read from disk
/// nothing leaves: the first request must not be the one that goes direct.
fn route_for(state: &State, pinned: bool, now: u64) -> Route {
    if !state.started {
        return Route::Refused;
    }
    if !state.enabled || !pinned {
        return Route::Direct;
    }
    match &state.held {
        Some(held) if held.expires_at > now => Route::Tunnel(Arc::clone(&held.dialer)),
        _ => Route::Refused,
    }
}

/// When to fetch a held config again: halfway through its life, so a failed
/// fetch still leaves time on the one in use.
fn refresh_after(issued_at: u64, expires_at: u64, now: u64) -> Duration {
    let at = issued_at + expires_at.saturating_sub(issued_at) / 2;
    Duration::from_secs(at.saturating_sub(now)).max(RETRY)
}

/// Parse and verify a config. Nothing unverified leaves this function.
fn verified(bytes: &[u8], anchor: &VerifyingKey, now: u64) -> Result<Held, String> {
    let config: BootstrapConfig =
        serde_json::from_slice(bytes).map_err(|e| format!("bootstrap config: {e}"))?;
    let at = UNIX_EPOCH + Duration::from_secs(now);
    let dialer = BootstrapDialer::from_signed_config(&config, anchor, at)
        .map_err(|e| format!("bootstrap verify: {e}"))?;
    Ok(Held {
        dialer: Arc::new(dialer),
        issued_at: config.issued_at,
        expires_at: config.expires_at,
    })
}

impl Tunnel {
    /// `sfu_url` is the SFU's public base URL, `wss://host/path`.
    pub fn new(origin: Origin, sfu_url: &reqwest::Url) -> Result<Self, String> {
        let host = sfu_url.host_str().ok_or("sfu url has no host")?;
        let path = sfu_url.path().trim_end_matches('/');
        Ok(Self {
            pin: Pin::parse(PINNED_ANCHOR, PINNED_DOMAIN, PINNED_PATH),
            origin,
            sfu_url: format!("ws://{host}{path}/v1/connect"),
            dir: Mutex::new(None),
            state: Mutex::new(State {
                started: false,
                enabled: false,
                phase: Phase::Disabled,
                held: None,
                generation: 0,
            }),
            sockets: Mutex::new(HashMap::new()),
            next_socket: AtomicU64::new(1),
        })
    }

    /// Read the setting and act on it. Called before the window exists.
    pub fn start(self: &Arc<Self>, dir: PathBuf) {
        let enabled = std::fs::read(dir.join(PREFERENCE_FILE)).is_ok_and(|b| b == b"on");
        *locked(&self.dir) = Some(dir);
        {
            let mut state = locked(&self.state);
            state.started = true;
            state.enabled = enabled;
        }
        self.apply();
    }

    /// How the next request leaves.
    pub fn route(&self) -> Route {
        route_for(&locked(&self.state), self.pin.is_some(), now_secs())
    }

    fn state_json(&self) -> Value {
        let state = locked(&self.state);
        json!({ "enabled": state.enabled, "phase": state.phase.name() })
    }

    fn set_enabled(self: &Arc<Self>, enabled: bool) -> Result<(), String> {
        let dir = locked(&self.dir).clone().ok_or("not started")?;
        write_preference(&dir, enabled)?;
        locked(&self.state).enabled = enabled;
        self.apply();
        Ok(())
    }

    /// Bring the phase in line with the setting, and fetch when it is on.
    fn apply(self: &Arc<Self>) {
        let pin = self.pin.is_some();
        let (generation, fetch) = {
            let mut state = locked(&self.state);
            state.generation += 1;
            state.held = None;
            state.phase = match (state.enabled, pin) {
                (false, _) => Phase::Disabled,
                (true, false) => Phase::Unconfigured,
                (true, true) => Phase::Arming,
            };
            (state.generation, state.enabled && pin)
        };
        info!(phase = locked(&self.state).phase.name(), "anti-censorship");
        if fetch {
            let tunnel = Arc::clone(self);
            tauri::async_runtime::spawn(async move { tunnel.keep_armed(generation).await });
        }
    }

    fn current(&self, generation: u64) -> bool {
        locked(&self.state).generation == generation
    }

    /// Hold a live config until the setting changes.
    async fn keep_armed(self: Arc<Self>, generation: u64) {
        while self.current(generation) {
            let wait = match self.fetch().await {
                Ok(held) => {
                    let now = now_secs();
                    let wait = refresh_after(held.issued_at, held.expires_at, now);
                    let mut state = locked(&self.state);
                    if state.generation != generation {
                        return;
                    }
                    state.held = Some(held);
                    state.phase = Phase::Armed;
                    drop(state);
                    info!("anti-censorship armed");
                    wait
                },
                Err(e) => {
                    warn!(error = %e, "anti-censorship config fetch failed");
                    let mut state = locked(&self.state);
                    if state.generation != generation {
                        return;
                    }
                    // A held config stays in use until it expires.
                    let now = now_secs();
                    if state.held.as_ref().is_none_or(|h| h.expires_at <= now) {
                        state.held = None;
                        state.phase = Phase::Unavailable;
                    }
                    RETRY
                },
            };
            tokio::time::sleep(wait).await;
        }
    }

    async fn fetch(&self) -> Result<Held, String> {
        let pin = self.pin.as_ref().ok_or("no pin")?;
        let bytes = match local_config() {
            Some(bytes) => bytes?,
            None => {
                let fetcher = DohFetcher::new().map_err(|e| format!("doh: {e}"))?;
                fetcher
                    .fetch_pinned_bytes(&pin.doh_domain, &pin.config_path, MAX_CONFIG_BYTES)
                    .await
                    .map_err(|e| format!("fetch: {e}"))?
            },
        };
        verified(&bytes, &pin.anchor, now_secs())
    }

    /// One HTTP exchange through `dialer`.
    pub async fn request(
        &self,
        dialer: &BootstrapDialer,
        method: String,
        path: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Result<TunneledResponse, String> {
        let req = TunneledRequest {
            method,
            path,
            host: self.origin.host.clone(),
            headers,
            body,
        };
        tunneled_request_via_dialer(dialer, &self.endpoint(), DIAL_DEADLINE, &req)
            .await
            .map(|(_, response)| response)
            .map_err(|e| e.to_string())
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::Hostname {
            host: self.origin.host.clone(),
            port: self.origin.port,
        }
    }

    async fn open_socket(
        &self,
        dialer: &BootstrapDialer,
        subprotocols: &str,
    ) -> Result<u64, String> {
        let (_, conn) = dialer
            .dial(&self.endpoint(), DIAL_DEADLINE)
            .await
            .map_err(|e| e.to_string())?;
        let ws = tunneled_websocket(conn, &self.sfu_url, Some(subprotocols))
            .await
            .map_err(|e| e.to_string())?;
        let socket = Arc::new(Socket::spawn(ws));
        let id = self.next_socket.fetch_add(1, Ordering::Relaxed);
        let mut sockets = locked(&self.sockets);
        if sockets.len() >= MAX_SOCKETS {
            if let Some(oldest) = sockets.keys().min().copied() {
                sockets.remove(&oldest);
            }
        }
        sockets.insert(id, socket);
        Ok(id)
    }

    fn socket(&self, id: u64) -> Option<Arc<Socket>> {
        locked(&self.sockets).get(&id).cloned()
    }

    fn forget_socket(&self, id: u64) {
        locked(&self.sockets).remove(&id);
    }
}

/// A debug build may take the config from a file, still verified against the
/// pinned anchor, so the path can be walked with no published config.
#[cfg(debug_assertions)]
fn local_config() -> Option<Result<Vec<u8>, String>> {
    let path = std::env::var_os("LAKKO_DESKTOP_BOOTSTRAP_FILE")?;
    Some(std::fs::read(path).map_err(|e| format!("bootstrap file: {e}")))
}

#[cfg(not(debug_assertions))]
const fn local_config() -> Option<Result<Vec<u8>, String>> {
    None
}

fn write_preference(dir: &Path, enabled: bool) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let value: &[u8] = if enabled { b"on" } else { b"off" };
    std::fs::write(dir.join(PREFERENCE_FILE), value).map_err(|e| e.to_string())
}

/// A WebSocket held for the page. Dropping it closes the connection.
struct Socket {
    out: mpsc::Sender<String>,
    frames: AsyncMutex<mpsc::Receiver<String>>,
}

impl Socket {
    fn spawn<S>(mut ws: S) -> Self
    where
        S: futures::Stream<Item = Result<Message, lakko_transport::tungstenite::Error>>
            + futures::Sink<Message, Error = lakko_transport::tungstenite::Error>
            + Unpin
            + Send
            + 'static,
    {
        let (out, mut outgoing) = mpsc::channel::<String>(FRAME_QUEUE);
        let (incoming, frames) = mpsc::channel(FRAME_QUEUE);
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::select! {
                    next = outgoing.recv() => {
                        let Some(text) = next else {
                            let _ = ws.close().await;
                            return;
                        };
                        if ws.send(Message::Text(text)).await.is_err() {
                            return;
                        }
                    }
                    next = ws.next() => match next {
                        Some(Ok(Message::Text(text))) => {
                            if incoming.send(text).await.is_err() {
                                return;
                            }
                        }
                        Some(Ok(Message::Close(_)) | Err(_)) | None => return,
                        // Control frames are the codec's; the signaling
                        // protocol has no binary frames.
                        Some(Ok(_)) => {}
                    },
                }
            }
        });
        Self {
            out,
            frames: AsyncMutex::new(frames),
        }
    }
}

fn json_response(v: &Value) -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(v.to_string().into_bytes())
        .unwrap_or_else(|_| plain(StatusCode::INTERNAL_SERVER_ERROR, "response"))
}

/// The answer to a request the setting keeps from leaving.
pub fn refused() -> Response<Vec<u8>> {
    let body = json!({ "error": "tunnel-unavailable" });
    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.to_string().into_bytes())
        .unwrap_or_else(|_| plain(StatusCode::SERVICE_UNAVAILABLE, "tunnel"))
}

/// Answer one request under [`PATH`].
pub async fn handle(tunnel: &Arc<Tunnel>, request: Request<Vec<u8>>) -> Response<Vec<u8>> {
    let path = request
        .uri()
        .path()
        .strip_prefix(PATH)
        .unwrap_or("")
        .to_owned();
    let method = request.method().clone();
    let body = request.into_body();
    match (&method, path.as_str()) {
        (&Method::GET, "/state") => json_response(&tunnel.state_json()),
        (&Method::POST, "/enabled") => {
            let parsed: Option<Value> = serde_json::from_slice(&body).ok();
            let Some(enabled) = parsed.and_then(|v| v["enabled"].as_bool()) else {
                return plain(StatusCode::BAD_REQUEST, "enabled");
            };
            match tunnel.set_enabled(enabled) {
                Ok(()) => json_response(&tunnel.state_json()),
                Err(e) => {
                    warn!(error = %e, "anti-censorship setting not saved");
                    plain(StatusCode::INTERNAL_SERVER_ERROR, "setting")
                },
            }
        },
        (&Method::POST, "/ws/open") => open(tunnel, &body).await,
        _ => match socket_route(&path) {
            Some((id, verb)) => on_socket(tunnel, &method, id, verb, body).await,
            None => plain(StatusCode::NOT_FOUND, "not found"),
        },
    }
}

fn socket_route(path: &str) -> Option<(u64, &str)> {
    let (id, verb) = path.strip_prefix("/ws/")?.split_once('/')?;
    Some((id.parse().ok()?, verb))
}

async fn open(tunnel: &Arc<Tunnel>, body: &[u8]) -> Response<Vec<u8>> {
    let parsed: Option<Value> = serde_json::from_slice(body).ok();
    let Some(list) = parsed.as_ref().and_then(|v| v["subprotocols"].as_array()) else {
        return plain(StatusCode::BAD_REQUEST, "subprotocols");
    };
    let words: Option<Vec<&str>> = list.iter().map(Value::as_str).collect();
    // No space after the comma: the socket library splits the offer on `,`
    // without trimming when it checks the answer.
    let offer = words.map(|w| w.join(",")).unwrap_or_default();
    if offer.is_empty() || offer.len() > MAX_SUBPROTOCOLS_LEN {
        return plain(StatusCode::BAD_REQUEST, "subprotocols");
    }
    match tunnel.route() {
        Route::Direct => plain(StatusCode::CONFLICT, "not tunnelled"),
        Route::Refused => refused(),
        Route::Tunnel(dialer) => match tunnel.open_socket(&dialer, &offer).await {
            Ok(id) => json_response(&json!({ "id": id })),
            Err(e) => {
                warn!(error = %e, "tunnelled socket failed");
                refused()
            },
        },
    }
}

async fn on_socket(
    tunnel: &Arc<Tunnel>,
    method: &Method,
    id: u64,
    verb: &str,
    body: Vec<u8>,
) -> Response<Vec<u8>> {
    match (method, verb) {
        (&Method::POST, "close") => {
            tunnel.forget_socket(id);
            plain(StatusCode::NO_CONTENT, "")
        },
        (&Method::POST, "send") => {
            let Some(socket) = tunnel.socket(id) else {
                return plain(StatusCode::GONE, "socket");
            };
            let Ok(text) = String::from_utf8(body) else {
                return plain(StatusCode::BAD_REQUEST, "text");
            };
            match socket.out.send(text).await {
                Ok(()) => plain(StatusCode::NO_CONTENT, ""),
                Err(_) => plain(StatusCode::GONE, "socket"),
            }
        },
        (&Method::GET, "recv") => {
            let Some(socket) = tunnel.socket(id) else {
                return json_response(&json!({ "frames": [], "closed": true }));
            };
            let mut frames = socket.frames.lock().await;
            let mut out = Vec::new();
            match tokio::time::timeout(RECV_WAIT, frames.recv()).await {
                Err(_) => {},
                Ok(None) => {
                    drop(frames);
                    tunnel.forget_socket(id);
                    return json_response(&json!({ "frames": [], "closed": true }));
                },
                Ok(Some(first)) => {
                    out.push(first);
                    while out.len() < MAX_FRAMES_PER_RECV {
                        match frames.try_recv() {
                            Ok(next) => out.push(next),
                            Err(_) => break,
                        }
                    }
                },
            }
            json_response(&json!({ "frames": out, "closed": false }))
        },
        _ => plain(StatusCode::NOT_FOUND, "not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::signing::SigningKey;
    use lakko_svc_obfs_proxy::relay::relay;
    use lakko_transport::{
        shadowsocks_accept_inbound, ShadowsocksConfig, ShadowsocksReplayWindow, TransportSpec,
    };
    use std::net::SocketAddr;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;

    const PSK: [u8; 32] = [0x55; 32];

    fn state(started: bool, enabled: bool, held: Option<Held>) -> State {
        State {
            started,
            enabled,
            phase: Phase::Arming,
            held,
            generation: 0,
        }
    }

    fn config(remote: &str, issued_at: u64, expires_at: u64) -> BootstrapConfig {
        BootstrapConfig {
            version: 1,
            issued_at,
            expires_at,
            transports: vec![TransportSpec::Shadowsocks2022(ShadowsocksConfig {
                remote: remote.to_owned(),
                method: "2022-blake3-chacha20-poly1305".to_owned(),
                psk_b64: B64.encode(PSK),
            })],
            signature_hex: String::new(),
        }
    }

    fn signed(signer: &SigningKey, remote: &str) -> Vec<u8> {
        let now = now_secs();
        let mut cfg = config(remote, now, now + 3600);
        cfg.sign(signer).unwrap();
        serde_json::to_vec(&cfg).unwrap()
    }

    fn held(signer: &SigningKey, remote: &str) -> Held {
        verified(&signed(signer, remote), &signer.verifying_key(), now_secs()).unwrap()
    }

    #[test]
    fn a_pin_needs_all_three_values_and_a_real_key() {
        let key = B64.encode(SigningKey::generate().verifying_key().to_bytes());
        assert!(Pin::parse(Some(&key), Some("d.example"), Some("/b.json")).is_some());
        assert!(Pin::parse(None, Some("d.example"), Some("/b.json")).is_none());
        assert!(Pin::parse(Some(&key), Some("  "), Some("/b.json")).is_none());
        assert!(Pin::parse(Some(&key), Some("d.example"), None).is_none());
        assert!(Pin::parse(Some("AAAA"), Some("d.example"), Some("/b.json")).is_none());
    }

    #[test]
    fn the_setting_on_never_goes_direct() {
        let signer = SigningKey::generate();
        let now = now_secs();
        assert!(matches!(
            route_for(&state(true, false, None), true, now),
            Route::Direct
        ));
        // No pin: the setting cannot be honoured and the page says so.
        assert!(matches!(
            route_for(&state(true, true, None), false, now),
            Route::Direct
        ));
        // Fetching, or failed.
        assert!(matches!(
            route_for(&state(true, true, None), true, now),
            Route::Refused
        ));
        let live = held(&signer, "127.0.0.1:9");
        let expires = live.expires_at;
        let armed = state(true, true, Some(live));
        assert!(matches!(route_for(&armed, true, now), Route::Tunnel(_)));
        assert!(matches!(route_for(&armed, true, expires), Route::Refused));
    }

    #[test]
    fn nothing_leaves_before_the_setting_was_read() {
        assert!(matches!(
            route_for(&state(false, false, None), true, 0),
            Route::Refused
        ));
    }

    #[test]
    fn a_config_that_does_not_verify_is_refused() {
        let signer = SigningKey::generate();
        let bytes = signed(&signer, "127.0.0.1:9");
        let other = SigningKey::generate().verifying_key();
        assert!(verified(&bytes, &other, now_secs()).is_err());
        assert!(verified(b"{}", &signer.verifying_key(), now_secs()).is_err());
        let late = now_secs() + 7200;
        assert!(verified(&bytes, &signer.verifying_key(), late).is_err());
    }

    #[test]
    fn a_held_config_is_fetched_again_halfway_through_its_life() {
        assert_eq!(refresh_after(1000, 5000, 1000), Duration::from_secs(2000));
        assert_eq!(refresh_after(1000, 5000, 2990), RETRY);
        assert_eq!(refresh_after(1000, 5000, 9000), RETRY);
    }

    #[test]
    fn the_preference_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("lakko-tunnel-{}", std::process::id()));
        write_preference(&dir, true).unwrap();
        assert_eq!(std::fs::read(dir.join(PREFERENCE_FILE)).unwrap(), b"on");
        write_preference(&dir, false).unwrap();
        assert_eq!(std::fs::read(dir.join(PREFERENCE_FILE)).unwrap(), b"off");
        let _ = std::fs::remove_dir_all(dir);
    }

    fn tunnel() -> Arc<Tunnel> {
        let sfu = reqwest::Url::parse("wss://sfu.example/sfu/").unwrap();
        let origin = Origin {
            host: "api.example".to_owned(),
            port: 443,
        };
        Arc::new(Tunnel::new(origin, &sfu).unwrap())
    }

    /// An obfs listener relaying one tunnel to `origin`, as `obfs-proxy` does.
    async fn spawn_proxy(origin: SocketAddr) -> SocketAddr {
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bound = tcp.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = tcp.accept().await.unwrap();
            let mut window = ShadowsocksReplayWindow::new();
            let conn = shadowsocks_accept_inbound(stream, &PSK, &mut window)
                .await
                .unwrap();
            let _ = relay(conn, origin).await;
        });
        bound
    }

    #[tokio::test]
    async fn an_api_request_rides_the_tunnel_to_the_origin() {
        let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin_addr = origin.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut s, _) = origin.accept().await.unwrap();
            let mut seen = Vec::new();
            let mut buf = [0u8; 2048];
            while !seen.windows(5).any(|w| w == b"hello") {
                let n = s.read(&mut buf).await.unwrap();
                seen.extend_from_slice(&buf[..n]);
            }
            s.write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n",
            )
            .await
            .unwrap();
            String::from_utf8(seen).unwrap()
        });
        let proxy = spawn_proxy(origin_addr).await;
        let signer = SigningKey::generate();
        let live = held(&signer, &proxy.to_string());

        let response = tunnel()
            .request(
                &live.dialer,
                "POST".to_owned(),
                "/api/ds/x?wait=25".to_owned(),
                vec![("Authorization".to_owned(), "Bearer t".to_owned())],
                b"hello".to_vec(),
            )
            .await
            .unwrap();

        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"ok");
        let seen = server.await.unwrap();
        assert!(
            seen.starts_with("POST /api/ds/x?wait=25 HTTP/1.1\r\n"),
            "{seen}"
        );
        assert!(seen.contains("Host: api.example\r\n"), "{seen}");
        assert!(seen.contains("Authorization: Bearer t\r\n"), "{seen}");
    }

    #[tokio::test]
    async fn a_dial_that_fails_is_an_error_and_never_a_direct_request() {
        let signer = SigningKey::generate();
        let live = held(&signer, "127.0.0.1:1");
        let result = tunnel()
            .request(
                &live.dialer,
                "GET".to_owned(),
                "/".to_owned(),
                vec![],
                vec![],
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn the_signaling_socket_rides_the_tunnel_and_names_no_address() {
        let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin_addr = origin.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = origin.accept().await.unwrap();
            let callback = |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                            mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                assert_eq!(request.uri().path(), "/sfu/v1/connect");
                let offer = request.headers()["sec-websocket-protocol"].to_str().unwrap();
                assert_eq!(offer, "lakko-sfu-v1,lakko-sfu-token.jwt");
                response
                    .headers_mut()
                    .insert("sec-websocket-protocol", "lakko-sfu-v1".parse().unwrap());
                Ok(response)
            };
            let mut ws = tokio_tungstenite::accept_hdr_async(stream, callback)
                .await
                .unwrap();
            while let Some(Ok(message)) = ws.next().await {
                if message.is_text() {
                    ws.send(message).await.unwrap();
                }
            }
        });
        let proxy = spawn_proxy(origin_addr).await;
        let signer = SigningKey::generate();
        let live = held(&signer, &proxy.to_string());
        let tunnel = tunnel();

        let id = tunnel
            .open_socket(&live.dialer, "lakko-sfu-v1,lakko-sfu-token.jwt")
            .await
            .unwrap();
        let send = Request::builder()
            .method(Method::POST)
            .uri(format!("{PATH}/ws/{id}/send"))
            .body(b"{\"kind\":\"ping\"}".to_vec())
            .unwrap();
        assert_eq!(handle(&tunnel, send).await.status(), StatusCode::NO_CONTENT);
        let recv = Request::builder()
            .method(Method::GET)
            .uri(format!("{PATH}/ws/{id}/recv"))
            .body(Vec::new())
            .unwrap();
        let answer: Value = serde_json::from_slice(handle(&tunnel, recv).await.body()).unwrap();
        assert_eq!(answer["frames"][0], "{\"kind\":\"ping\"}");
        assert_eq!(answer["closed"], false);

        let close = Request::builder()
            .method(Method::POST)
            .uri(format!("{PATH}/ws/{id}/close"))
            .body(Vec::new())
            .unwrap();
        handle(&tunnel, close).await;
        let after = Request::builder()
            .method(Method::GET)
            .uri(format!("{PATH}/ws/{id}/recv"))
            .body(Vec::new())
            .unwrap();
        let answer: Value = serde_json::from_slice(handle(&tunnel, after).await.body()).unwrap();
        assert_eq!(answer["closed"], true);
    }

    /// Against the deployed stack, one transport at a time: the config comes the
    /// way it does in the app (DoH, the pinned anchor), a request reaches auth
    /// and a socket reaches the SFU. `LAKKO_LIVE_ANCHOR` is the trust anchor of
    /// `just deploy-test-obfs`.
    #[tokio::test]
    #[ignore = "needs the deployed test stack"]
    async fn live_every_transport_reaches_the_test_stack() {
        let anchor = std::env::var("LAKKO_LIVE_ANCHOR").unwrap();
        let domain = "test.lakko.me";
        let pin = Pin::parse(Some(&anchor), Some(domain), Some("/bootstrap.json")).unwrap();
        let bytes = DohFetcher::new()
            .unwrap()
            .fetch_pinned_bytes(&pin.doh_domain, &pin.config_path, MAX_CONFIG_BYTES)
            .await
            .unwrap();
        verified(&bytes, &pin.anchor, now_secs()).unwrap();
        let config: BootstrapConfig = serde_json::from_slice(&bytes).unwrap();
        let sfu = reqwest::Url::parse("wss://test.lakko.me/sfu").unwrap();
        let origin = Origin {
            host: domain.to_owned(),
            port: 443,
        };
        let tunnel = Tunnel::new(origin, &sfu).unwrap();

        for spec in &config.transports {
            let mut one = config.clone();
            one.transports = vec![spec.clone()];
            let dialer = BootstrapDialer::from_verified_config(&one).unwrap();
            let kinds = dialer.candidate_kinds();
            let response = tunnel
                .request(
                    &dialer,
                    "GET".to_owned(),
                    "/api/auth/health".to_owned(),
                    vec![],
                    vec![],
                )
                .await
                .unwrap();
            assert_eq!(response.status, 200, "{kinds:?}");

            // A made-up admission token: the SFU itself has to answer, so the
            // refusal shows the socket got through nginx to it.
            let (_, conn) = dialer
                .dial(&tunnel.endpoint(), DIAL_DEADLINE)
                .await
                .unwrap();
            let outcome = tunneled_websocket(
                conn,
                &tunnel.sfu_url,
                Some("lakko-sfu-v1,lakko-sfu-token.bad"),
            )
            .await;
            println!(
                "{kinds:?}: health 200, sfu socket: {:?}",
                outcome.map(|_| "opened")
            );
        }
    }

    #[test]
    fn a_socket_route_is_read_strictly() {
        assert_eq!(socket_route("/ws/7/send"), Some((7, "send")));
        assert_eq!(socket_route("/ws/x/send"), None);
        assert_eq!(socket_route("/ws/7"), None);
        assert_eq!(socket_route("/state"), None);
    }
}
