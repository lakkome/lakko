// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The app scheme's request handler (ADR-0085 section 3).
//!
//! Three kinds of path:
//! - `/api/<service>/...` for a known service: forwarded to the one configured
//!   Lakko origin with the same method, headers and body;
//! - `/__probe...`: the S1 measurement page (`probe.rs`);
//! - anything else: the bundled web build, with `index.html` for a path that
//!   names no file (the web client's routes).
//!
//! Nothing else is reachable: the handler never forwards to a host the page
//! names, so it is not an open relay.

use http::{header, HeaderMap, HeaderName, Method, Request, Response, StatusCode};
use tauri::{AppHandle, Runtime};
use tracing::{debug, warn};

use std::sync::Arc;

use tauri::Manager;

use crate::{app_shell, device_key, probe};

/// The services the web client addresses as `/api/<service>/` (the dev
/// proxy list in `apps/web/vite.config.ts`).
const SERVICES: &[&str] = &[
    "auth",
    "discovery",
    "ds",
    "storage",
    "push",
    "channels",
    "gifs",
];

/// Request headers never forwarded: the connection's own, and the ones that
/// would name the page's origin to the server.
const DROP_REQUEST_HEADERS: &[HeaderName] = &[
    header::HOST,
    header::ORIGIN,
    header::REFERER,
    header::CONNECTION,
    header::CONTENT_LENGTH,
    header::TRANSFER_ENCODING,
];

/// Response headers not copied back: the webview's scheme handler frames
/// the body itself.
const DROP_RESPONSE_HEADERS: &[HeaderName] = &[
    header::CONNECTION,
    header::CONTENT_LENGTH,
    header::TRANSFER_ENCODING,
];

/// Where `/api/*` goes, and the host state the scheme's routes share.
pub struct ProxyConfig {
    /// The Lakko origin the web vhost answers on, e.g. `https://test.lakko.me`.
    pub api_origin: reqwest::Url,
    client: reqwest::Client,
    /// Device-key handles the page holds (`device_key.rs`).
    pub handles: Arc<device_key::Handles>,
}

impl ProxyConfig {
    /// `LAKKO_DESKTOP_API_ORIGIN`, default `https://test.lakko.me`.
    pub fn from_env() -> Result<Self, String> {
        let raw = std::env::var("LAKKO_DESKTOP_API_ORIGIN")
            .unwrap_or_else(|_| "https://test.lakko.me".to_owned());
        let api_origin = reqwest::Url::parse(&raw).map_err(|e| format!("bad origin {raw}: {e}"))?;
        if api_origin.scheme() != "https" && api_origin.host_str() != Some("127.0.0.1") {
            return Err(format!("origin must be https (or 127.0.0.1): {raw}"));
        }
        if api_origin.path() != "/" || api_origin.query().is_some() {
            return Err(format!("origin must have no path or query: {raw}"));
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        Ok(Self {
            api_origin,
            client,
            handles: Arc::new(device_key::Handles::default()),
        })
    }
}

/// Answer one request from the page.
pub async fn handle<R: Runtime>(
    app: &AppHandle<R>,
    config: &ProxyConfig,
    request: Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    let path = request.uri().path().to_owned();
    if let Some(service) = api_service(&path) {
        debug!(service, method = %request.method(), "forward");
        return forward(config, request).await;
    }
    if path.starts_with(probe::PROBE_PATH) {
        return probe::handle(request).await;
    }
    if path.starts_with(app_shell::PATH) {
        return match app.try_state::<app_shell::Shell>() {
            Some(shell) => app_shell::handle(app, &shell, &request),
            None => plain(StatusCode::SERVICE_UNAVAILABLE, "starting"),
        };
    }
    if path.starts_with(device_key::PATH) {
        // Keychain and Secure Enclave calls block, and may wait on a system
        // prompt; keep them off the async workers.
        let handles = Arc::clone(&config.handles);
        return tokio::task::spawn_blocking(move || device_key::handle(&handles, &request))
            .await
            .unwrap_or_else(|_| plain(StatusCode::INTERNAL_SERVER_ERROR, "device key"));
    }
    let response = asset(app, &path);
    debug!(path = %path, status = response.status().as_u16(), "asset");
    response
}

/// The service a path addresses, when it is one of [`SERVICES`].
fn api_service(path: &str) -> Option<&'static str> {
    let rest = path.strip_prefix("/api/")?;
    let name = rest.split('/').next()?;
    SERVICES.iter().copied().find(|s| *s == name)
}

async fn forward(config: &ProxyConfig, request: Request<Vec<u8>>) -> Response<Vec<u8>> {
    let (parts, body) = request.into_parts();
    let mut url = config.api_origin.clone();
    url.set_path(parts.uri.path());
    url.set_query(parts.uri.query());

    let method = match reqwest::Method::from_bytes(parts.method.as_str().as_bytes()) {
        Ok(m) => m,
        Err(_) => return plain(StatusCode::METHOD_NOT_ALLOWED, "method"),
    };
    let mut headers = HeaderMap::new();
    for (name, value) in &parts.headers {
        if !DROP_REQUEST_HEADERS.contains(name) {
            headers.append(name.clone(), value.clone());
        }
    }
    let mut builder = config.client.request(method, url).headers(headers);
    if parts.method != Method::GET && parts.method != Method::HEAD {
        builder = builder.body(body);
    }

    let upstream = match builder.send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "forward failed");
            return plain(StatusCode::BAD_GATEWAY, "upstream unreachable");
        }
    };
    let status = upstream.status();
    let mut response = Response::builder().status(status.as_u16());
    for (name, value) in upstream.headers() {
        if !DROP_RESPONSE_HEADERS.contains(name) {
            response = response.header(name, value);
        }
    }
    let bytes = match upstream.bytes().await {
        Ok(b) => b.to_vec(),
        Err(e) => {
            warn!(error = %e, "upstream body failed");
            return plain(StatusCode::BAD_GATEWAY, "upstream body");
        }
    };
    response
        .body(bytes)
        .unwrap_or_else(|_| plain(StatusCode::INTERNAL_SERVER_ERROR, "response"))
}

fn asset<R: Runtime>(app: &AppHandle<R>, path: &str) -> Response<Vec<u8>> {
    let resolver = app.asset_resolver();
    let found = resolver.get(path.to_owned()).or_else(|| {
        // A path that names no file is one of the web client's routes.
        let last = path.rsplit('/').next().unwrap_or("");
        if last.contains('.') {
            None
        } else {
            resolver.get("/index.html".to_owned())
        }
    });
    match found {
        Some(a) => {
            let mut response = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, a.mime_type.as_str());
            if let Some(csp) = a.csp_header.as_deref() {
                response = response.header("Content-Security-Policy", csp);
            }
            response
                .body(a.bytes)
                .unwrap_or_else(|_| plain(StatusCode::INTERNAL_SERVER_ERROR, "asset"))
        }
        None => plain(StatusCode::NOT_FOUND, "not found"),
    }
}

/// A short plain-text response.
pub fn plain(status: StatusCode, text: &str) -> Response<Vec<u8>> {
    let mut r = Response::new(text.as_bytes().to_vec());
    *r.status_mut() = status;
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        http::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    r
}

#[cfg(test)]
mod tests {
    use super::api_service;

    #[test]
    fn only_known_services_are_forwarded() {
        assert_eq!(api_service("/api/ds/group/ab/messages"), Some("ds"));
        assert_eq!(api_service("/api/auth"), Some("auth"));
        assert_eq!(api_service("/api/evil/x"), None);
        assert_eq!(api_service("/apix/ds/"), None);
        assert_eq!(api_service("/"), None);
    }
}
