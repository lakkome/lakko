// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The S1 measurement page (ADR-0085 section 8).
//!
//! The page reports which web APIs this OS's webview offers under the app
//! scheme, then keeps a one-second timer and a slow-response loop running
//! and posts every tick to the host. The host hides the window for a while
//! and shows it again, and logs each report with its own clock, so the log
//! shows whether the page ran while hidden.

use std::time::{Duration, Instant};

use http::{Method, Request, Response, StatusCode};
use tauri::{AppHandle, Manager, Runtime};
use tracing::{info, warn};

use crate::proxy::plain;

/// Path prefix of the probe.
pub const PROBE_PATH: &str = "/__probe";

const PAGE: &str = include_str!("../probe/probe.html");

/// Debug builds: forwards page errors, rejected promises and
/// `console.error` / `console.warn` to `/__probe/report`.
pub const ERROR_REPORTER: &str = r#"
(() => {
  const send = (kind, msg) => {
    try {
      fetch("/__probe/report", { method: "POST",
        body: JSON.stringify({ kind, msg: String(msg).slice(0, 2000), at: location.pathname }) });
    } catch (_) {}
  };
  addEventListener("error", (e) => send("page-error", (e.message || "") + " @ " + (e.filename || "") + ":" + (e.lineno || "")));
  addEventListener("unhandledrejection", (e) => send("rejection", e.reason && (e.reason.stack || e.reason.message) || e.reason));
  for (const level of ["error", "warn"]) {
    const orig = console[level].bind(console);
    console[level] = (...args) => { send("console-" + level, args.map((a) => a && a.stack || a).join(" ")); orig(...args); };
  }
  addEventListener("DOMContentLoaded", () => send("dom-ready", document.title));
})();
"#;
const WORKER: &str = include_str!("../probe/probe-worker.js");

fn host_started() -> Instant {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *START.get_or_init(Instant::now)
}

fn secs(var: &str, default: u64) -> u64 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Hide the window after `LAKKO_DESKTOP_PROBE_VISIBLE_SECS` (default 15) and
/// show it again after `LAKKO_DESKTOP_PROBE_HIDDEN_SECS` (default 180).
pub fn schedule_hide<R: Runtime>(app: AppHandle<R>, label: String) {
    let _ = host_started();
    let visible = secs("LAKKO_DESKTOP_PROBE_VISIBLE_SECS", 15);
    let hidden = secs("LAKKO_DESKTOP_PROBE_HIDDEN_SECS", 180);
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(visible)).await;
        if let Some(w) = app.get_webview_window(&label) {
            match w.hide() {
                Ok(()) => info!(t = elapsed(), "probe: window hidden"),
                Err(e) => warn!(error = %e, "probe: hide failed"),
            }
        }
        tokio::time::sleep(Duration::from_secs(hidden)).await;
        if let Some(w) = app.get_webview_window(&label) {
            match w.show() {
                Ok(()) => info!(t = elapsed(), "probe: window shown"),
                Err(e) => warn!(error = %e, "probe: show failed"),
            }
        }
    });
}

fn elapsed() -> f64 {
    host_started().elapsed().as_secs_f64()
}

/// Answer a request under [`PROBE_PATH`].
pub async fn handle(request: Request<Vec<u8>>) -> Response<Vec<u8>> {
    let path = request.uri().path().to_owned();
    match (request.method(), path.as_str()) {
        (&Method::GET, "/__probe" | "/__probe/") => typed(PAGE, "text/html; charset=utf-8"),
        (&Method::GET, "/__probe/worker.js") => typed(WORKER, "text/javascript"),
        (&Method::GET, "/__probe/slow") => {
            let ms = request
                .uri()
                .query()
                .and_then(|q| q.strip_prefix("ms="))
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(5000)
                .min(30_000);
            tokio::time::sleep(Duration::from_millis(ms)).await;
            plain(StatusCode::OK, "ok")
        }
        (&Method::POST, "/__probe/report") => {
            let body = String::from_utf8_lossy(request.body());
            info!(t = format!("{:.1}", elapsed()), report = %body, "probe");
            let mut r = Response::new(Vec::new());
            *r.status_mut() = StatusCode::NO_CONTENT;
            r
        }
        _ => plain(StatusCode::NOT_FOUND, "not found"),
    }
}

fn typed(body: &str, content_type: &str) -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::OK)
        .header(http::header::CONTENT_TYPE, content_type)
        .body(body.as_bytes().to_vec())
        .unwrap_or_else(|_| plain(StatusCode::INTERNAL_SERVER_ERROR, "probe"))
}
