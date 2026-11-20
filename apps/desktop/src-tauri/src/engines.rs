// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The operating system's on-device translation, transcription and language
//! detection, for the page (ADR-0082 note of 2026-11-18, ADR-0085).
//!
//! The webview has none of these (WKWebView offers no `Translator`, no
//! `LanguageDetector` and no local `SpeechRecognition`), so on macOS the host
//! reaches Apple's through `lakko-engines`, a small Swift program shipped next
//! to this binary (`apps/desktop/macos-engines/main.swift`). This crate forbids
//! `unsafe`, which rules out calling Swift through FFI; the helper runs as a
//! child process, started on the first request, and is spoken to over its
//! standard input and output, one JSON object per line.
//!
//! What crosses: the text of a message to translate or detect, and the audio
//! of a voice or video message as 16 kHz mono PCM the page decoded. Both stay
//! in memory, in this process and the helper's; nothing is written to disk and
//! nothing reaches a log (only the operation and its outcome). Windows and
//! Linux have no helper yet: [`Capabilities`] is all false there and the page
//! says translation and transcription are not available on this device.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use http::{Method, Request, Response, StatusCode};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};
use tracing::{debug, warn};

use crate::proxy::plain;

/// Path prefix of these routes.
pub const PATH: &str = "/__host/engines";

/// The helper's file name, next to the host binary.
pub const HELPER: &str = "lakko-engines";

/// The global the page reads, before any of its scripts run, to learn which
/// engines this computer has (`apps/web/src/translation/host-engines.ts`).
pub const GLOBAL: &str = "__LAKKO_SYSTEM_ENGINES__";

/// The largest JSON body the page may send: a message's text with room for
/// its escaping. Chosen.
const MAX_JSON_BODY: usize = 2 * 1024 * 1024;

/// The most PCM bytes one transcription takes: 16 minutes of 16 kHz 16-bit
/// mono, a little over the 15-minute voice-message bound (ADR-0047). The
/// helper checks the same bound.
const MAX_PCM_BYTES: usize = 16 * 60 * 16_000 * 2;

/// How long the startup probe may take before the engines count as absent.
/// Measured: about 0.16 s on macOS 27.2 (2026-11-18). Chosen.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// How long one answer may take. A transcription of a long recording takes
/// the longest; a pack download has no bound of its own. Chosen.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(600);

/// System Settings > General > Language & Region, where macOS downloads a
/// translation language (`Translation Languages…`).
#[cfg(target_os = "macos")]
const PACK_SETTINGS: Option<&str> =
    Some("x-apple.systempreferences:com.apple.Localization-Settings.extension");
#[cfg(not(target_os = "macos"))]
const PACK_SETTINGS: Option<&str> = None;

/// What this computer's system engines can do at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub translation: bool,
    pub transcription: bool,
}

impl Capabilities {
    fn from_probe(v: &Value) -> Self {
        Self {
            translation: v.get("translation").and_then(Value::as_bool) == Some(true),
            transcription: v.get("transcription").and_then(Value::as_bool) == Some(true),
        }
    }

    /// The initialization script that hands them to the page.
    pub fn script(self) -> String {
        format!(
            "Object.defineProperty(globalThis, '{GLOBAL}', {{ value: Object.freeze({{ translation: {}, transcription: {} }}), writable: false, configurable: false }});",
            self.translation, self.transcription
        )
    }
}

/// Where the helper is: next to this binary, inside `Lakko.app`.
pub fn helper_path() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    let path = exe.parent()?.join(HELPER);
    path.is_file().then_some(path)
}

/// Run `lakko-engines --probe` on a thread of its own. Join it with
/// [`ProbeHandle::wait`]; a helper that is missing, does not load (macOS
/// older than 26.4) or does not answer in time counts as no engines.
pub fn start_probe() -> ProbeHandle {
    let (tx, rx) = std::sync::mpsc::channel();
    if let Some(path) = helper_path() {
        std::thread::spawn(move || {
            let caps = std::process::Command::new(&path)
                .arg("--probe")
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
                .map(|v| Capabilities::from_probe(&v))
                .unwrap_or_default();
            let _ = tx.send(caps);
        });
    } else {
        let _ = tx.send(Capabilities::default());
    }
    ProbeHandle { rx }
}

/// The startup probe in flight.
pub struct ProbeHandle {
    rx: std::sync::mpsc::Receiver<Capabilities>,
}

impl ProbeHandle {
    pub fn wait(self) -> Capabilities {
        self.rx.recv_timeout(PROBE_TIMEOUT).unwrap_or_default()
    }
}

type Pending = Arc<std::sync::Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

struct Running {
    stdin: ChildStdin,
    pending: Pending,
    _child: Child,
}

/// The helper, started on the first request and kept while it lives.
pub struct SystemEngines {
    /// Set once, from the startup probe ([`start_probe`]).
    caps: std::sync::OnceLock<Capabilities>,
    helper: Option<PathBuf>,
    running: Mutex<Option<Running>>,
    next_id: AtomicU64,
}

impl SystemEngines {
    pub fn new() -> Self {
        Self {
            caps: std::sync::OnceLock::new(),
            helper: helper_path(),
            running: Mutex::new(None),
            next_id: AtomicU64::new(1),
        }
    }

    /// Engines over a helper at `path`, for tests.
    #[cfg(test)]
    fn with_helper(path: PathBuf, caps: Capabilities) -> Self {
        let engines = Self {
            caps: std::sync::OnceLock::new(),
            helper: Some(path),
            running: Mutex::new(None),
            next_id: AtomicU64::new(1),
        };
        engines.set_capabilities(caps);
        engines
    }

    /// Record what the startup probe found; later calls change nothing.
    pub fn set_capabilities(&self, caps: Capabilities) {
        let _ = self.caps.set(caps);
    }

    /// What the probe found, or nothing before it answered.
    pub fn capabilities(&self) -> Capabilities {
        self.caps.get().copied().unwrap_or_default()
    }

    /// Send one request and wait for its answer.
    async fn ask(&self, mut request: Value) -> Result<Value, &'static str> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        request["id"] = json!(id);
        let mut line = serde_json::to_vec(&request).map_err(|_| "encode")?;
        line.push(b'\n');
        let (tx, rx) = oneshot::channel();
        {
            let mut guard = self.running.lock().await;
            if guard.is_none() {
                *guard = Some(self.spawn()?);
            }
            let Some(running) = guard.as_mut() else {
                return Err("helper");
            };
            if let Ok(mut p) = running.pending.lock() {
                p.insert(id, tx);
            }
            if running.stdin.write_all(&line).await.is_err() || running.stdin.flush().await.is_err()
            {
                // The helper is gone; the next request starts a new one.
                *guard = None;
                return Err("helper");
            }
        }
        match tokio::time::timeout(ANSWER_TIMEOUT, rx).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(_)) => Err("helper"),
            Err(_) => Err("timeout"),
        }
    }

    fn spawn(&self) -> Result<Running, &'static str> {
        let path = self.helper.as_ref().ok_or("no helper")?;
        let mut child = Command::new(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| "spawn")?;
        let stdin = child.stdin.take().ok_or("stdin")?;
        let stdout = child.stdout.take().ok_or("stdout")?;
        let pending: Pending = Arc::default();
        let readers = Arc::clone(&pending);
        tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(mut v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let Some(id) = v.get("id").and_then(Value::as_u64) else {
                    continue;
                };
                if let Some(obj) = v.as_object_mut() {
                    obj.remove("id");
                }
                let waiter = readers.lock().ok().and_then(|mut p| p.remove(&id));
                if let Some(tx) = waiter {
                    let _ = tx.send(v);
                }
            }
            // Gone: every request still waiting gets an error.
            if let Ok(mut p) = readers.lock() {
                p.clear();
            }
            debug!("engine helper exited");
        });
        Ok(Running {
            stdin,
            pending,
            _child: child,
        })
    }
}

/// The JSON fields the page may send for each operation; anything else is
/// dropped before the helper sees it.
fn fields(op: &str) -> Option<&'static [&'static str]> {
    Some(match op {
        "detect" => &["text"],
        "translation-state" => &["source", "target"],
        "translate" => &["source", "target", "text"],
        "speech-state" | "speech-install" => &["language"],
        "installed" => &["target"],
        _ => return None,
    })
}

/// Answer one request from the page.
pub async fn handle(engines: &SystemEngines, request: Request<Vec<u8>>) -> Response<Vec<u8>> {
    let path = request.uri().path().to_owned();
    let Some(op) = path.strip_prefix(PATH).and_then(|p| p.strip_prefix('/')) else {
        return plain(StatusCode::NOT_FOUND, "not found");
    };
    match (request.method(), op) {
        (&Method::GET, "capabilities") => {
            let c = engines.capabilities();
            return json_response(
                &json!({ "translation": c.translation, "transcription": c.transcription }),
            );
        },
        (&Method::POST, "open-pack-settings") => {
            let Some(link) = PACK_SETTINGS else {
                return plain(StatusCode::NOT_FOUND, "no settings");
            };
            if let Err(e) = std::process::Command::new("open").arg(link).spawn() {
                warn!(error = %e, "open language settings");
                return plain(StatusCode::INTERNAL_SERVER_ERROR, "open");
            }
            return Response::builder()
                .status(StatusCode::NO_CONTENT)
                .body(Vec::new())
                .unwrap_or_else(|_| plain(StatusCode::INTERNAL_SERVER_ERROR, "response"));
        },
        _ => {},
    }
    if request.method() != Method::POST {
        return plain(StatusCode::METHOD_NOT_ALLOWED, "method");
    }
    let caps = engines.capabilities();
    let present = match op {
        "detect" | "translation-state" | "translate" => caps.translation,
        "speech-state" | "speech-install" | "transcribe" => caps.transcription,
        // The packs macOS holds (UX section 6), for either engine.
        "installed" => caps.translation || caps.transcription,
        _ => return plain(StatusCode::NOT_FOUND, "not found"),
    };
    if !present {
        return plain(StatusCode::SERVICE_UNAVAILABLE, "no engine");
    }
    let message = if op == "transcribe" {
        let language = request
            .uri()
            .query()
            .and_then(|q| url::form_urlencoded::parse(q.as_bytes()).find(|(k, _)| k == "language"))
            .map(|(_, v)| v.into_owned());
        let Some(language) = language else {
            return plain(StatusCode::BAD_REQUEST, "language");
        };
        let body = request.body();
        if body.is_empty() || body.len() > MAX_PCM_BYTES || !body.len().is_multiple_of(2) {
            return plain(StatusCode::PAYLOAD_TOO_LARGE, "audio");
        }
        json!({
            "op": "transcribe",
            "language": language,
            "pcm": base64::engine::general_purpose::STANDARD.encode(body),
        })
    } else {
        if request.body().len() > MAX_JSON_BODY {
            return plain(StatusCode::PAYLOAD_TOO_LARGE, "body");
        }
        let Ok(Value::Object(sent)) = serde_json::from_slice::<Value>(request.body()) else {
            return plain(StatusCode::BAD_REQUEST, "json");
        };
        let Some(allowed) = fields(op) else {
            return plain(StatusCode::NOT_FOUND, "not found");
        };
        let mut out = serde_json::Map::new();
        out.insert("op".into(), json!(op));
        for f in allowed {
            if let Some(v) = sent.get(*f) {
                out.insert((*f).to_owned(), v.clone());
            }
        }
        Value::Object(out)
    };
    match engines.ask(message).await {
        Ok(answer) => {
            // The operation and whether it worked; never the text, the audio,
            // the transcript or a language (ADR-0082 section 8).
            let ok = answer.get("ok").and_then(serde_json::Value::as_bool) == Some(true);
            debug!(op, ok, "engine");
            json_response(&answer)
        },
        Err(why) => {
            warn!(op, why, "engine helper");
            plain(StatusCode::SERVICE_UNAVAILABLE, "engine helper")
        },
    }
}

fn json_response(v: &Value) -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::OK)
        .header(http::header::CONTENT_TYPE, "application/json")
        .header(http::header::CACHE_CONTROL, "no-store")
        .body(serde_json::to_vec(v).unwrap_or_default())
        .unwrap_or_else(|_| plain(StatusCode::INTERNAL_SERVER_ERROR, "response"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_read_only_true_booleans() {
        assert_eq!(
            Capabilities::from_probe(&json!({ "translation": true, "transcription": true })),
            Capabilities {
                translation: true,
                transcription: true
            }
        );
        assert_eq!(
            Capabilities::from_probe(&json!({ "translation": "yes", "transcription": 1 })),
            Capabilities::default()
        );
        assert_eq!(
            Capabilities::from_probe(&json!(null)),
            Capabilities::default()
        );
    }

    #[test]
    fn the_script_freezes_a_read_only_global() {
        let s = Capabilities {
            translation: true,
            transcription: false,
        }
        .script();
        assert!(s.contains(GLOBAL));
        assert!(s.contains("translation: true, transcription: false"));
        assert!(s.contains("writable: false"));
    }

    #[test]
    fn only_known_fields_reach_the_helper() {
        assert_eq!(fields("translate"), Some(&["source", "target", "text"][..]));
        assert_eq!(fields("detect"), Some(&["text"][..]));
        assert_eq!(fields("installed"), Some(&["target"][..]));
        assert_eq!(fields("transcribe"), None);
        assert_eq!(fields("probe"), None);
        assert_eq!(fields("anything"), None);
    }

    /// The real helper, when `just dev-desktop` has built it next to the test
    /// binary's directory (`target/debug/lakko-engines`); skipped otherwise.
    #[tokio::test]
    async fn the_real_helper_answers_over_its_pipe() {
        let Some(path) = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent()?.parent().map(|d| d.join(HELPER)))
            .filter(|p| p.is_file())
        else {
            eprintln!("skipped: no {HELPER} built next to the test binary");
            return;
        };
        let engines = SystemEngines::with_helper(
            path,
            Capabilities {
                translation: true,
                transcription: true,
            },
        );
        let detected = engines
            .ask(json!({ "op": "detect", "text": "Hola, como estas? Espero que todo vaya bien." }))
            .await;
        let detected = detected.expect("detect");
        assert_eq!(detected["ok"], json!(true));
        assert_eq!(detected["hypotheses"][0]["tag"], json!("es"));
        let state = engines
            .ask(json!({ "op": "translation-state", "source": "es", "target": "es" }))
            .await
            .expect("state");
        assert_eq!(state["state"], json!("unavailable"));
        let bad = engines
            .ask(json!({ "op": "transcribe", "language": "en", "pcm": "AAE=AA" }))
            .await
            .expect("answer");
        assert_eq!(bad["ok"], json!(false));
        let unknown = engines
            .ask(json!({ "op": "probe-everything" }))
            .await
            .expect("answer");
        assert_eq!(unknown["error"], json!("bad-request"));
    }

    /// The host answers pack questions the same way whatever the network
    /// route: there is no anti-censorship block (ADR-0082 amendment of
    /// 2026-11-19), so the old `packs-blocked` route is gone and `handle`
    /// takes no tunnel.
    #[tokio::test]
    async fn no_pack_block_route() {
        let engines =
            SystemEngines::with_helper(PathBuf::from("/nonexistent"), Capabilities::default());
        let request = Request::builder()
            .method(Method::GET)
            .uri(format!("{PATH}/packs-blocked"))
            .body(Vec::new())
            .expect("request");
        let response = handle(&engines, request).await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[test]
    fn the_audio_bound_matches_the_helper() {
        // `maxSamples` in main.swift: 16 minutes at 16 kHz, two bytes each.
        assert_eq!(MAX_PCM_BYTES, 30_720_000);
    }
}
