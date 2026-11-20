// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The moderators' panel: a third listener (ADR-0099 step 5, UX
//! `reporting-and-moderation.md` section 5).
//!
//! Bound to `LAKKO_MODERATION_PANEL_BIND` (default [`DEFAULT_PANEL_BIND`],
//! loopback). nginx puts it behind its own hostname with client
//! certificates required for the whole server; this service does not see
//! the certificate. It serves the built panel app from
//! `LAKKO_MODERATION_PANEL_DIR` at `/` (any path that is not a file gives
//! `index.html`), and the API at `/panel/api/*`. The routes and their JSON
//! are in [`crate::panel_routes`].
//!
//! ## Signing in
//!
//! `POST /panel/api/session/start` hands out a random 32-byte challenge
//! (kept [`CHALLENGE_TTL_MS`], single use, at most [`MAX_CHALLENGES`]
//! outstanding); `POST /panel/api/session/finish` takes the security key's
//! assertion over it and checks it with `lakko_moderation::webauthn` against
//! the loaded moderator list: the moderator must be listed and not revoked
//! now. The answer is a bearer token
//! `p1.<moderator id b64url>.<expires ms>.<tag b64url>`, the tag an
//! HMAC-SHA256 under a key made at startup, valid [`SESSION_TTL_MS`]. A
//! restart signs everyone out. Every other API route needs
//! `Authorization: Bearer <token>`, and refuses it once the moderator is
//! revoked in the loaded list.
//!
//! A sign-in challenge is random, so no assertion over it can be a
//! statement's (whose challenge is a hash of the statement), and a
//! statement's assertion cannot sign anyone in.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::{any, get, post};
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_crypto::hash::hmac_sha256;
use rand::RngCore as _;
use subtle::ConstantTimeEq as _;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::error;
use crate::internal::Forwarders;
use crate::list::LoadedList;
use crate::log::ModerationLog;
use crate::panel_routes as routes;
use crate::store::ReportStore;

/// Default panel bind: loopback.
pub const DEFAULT_PANEL_BIND: &str = "127.0.0.1:8099";
/// How long a session token lives (chosen).
pub const SESSION_TTL_MS: u64 = 8 * 60 * 60 * 1000;
/// How long a sign-in challenge lives.
pub const CHALLENGE_TTL_MS: u64 = 2 * 60 * 1000;
/// Most sign-in challenges outstanding at once.
pub const MAX_CHALLENGES: usize = 1024;
/// A case's lock is released after this long without its holder acting
/// (chosen).
pub const LOCK_IDLE_MS: u64 = 30 * 60 * 1000;
/// Most evidence blob reads the panel runs at once (each holds up to 50
/// MiB in memory); one more answers `503 busy`. Chosen, not measured.
pub const MAX_BLOB_READS: usize = 4;
/// Body limit of the API: the largest body is a decision with its note.
const MAX_PANEL_BODY_BYTES: usize = 64 * 1024;
/// The token version prefix.
const TOKEN_PREFIX: &str = "p1";
/// The domain of the token tag.
const TOKEN_DOMAIN: &[u8] = b"lakko/moderation/panel-session/v1\0";

/// Milliseconds since the unix epoch.
#[must_use]
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// Sign-in challenges and the token key.
pub struct Sessions {
    key: [u8; 32],
    challenges: Mutex<HashMap<[u8; 32], u64>>,
}

impl std::fmt::Debug for Sessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sessions").finish_non_exhaustive()
    }
}

impl Default for Sessions {
    fn default() -> Self {
        Self::new()
    }
}

impl Sessions {
    /// A fresh token key and no challenges.
    #[must_use]
    pub fn new() -> Self {
        let mut key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut key);
        Self {
            key,
            challenges: Mutex::new(HashMap::new()),
        }
    }

    fn challenges(&self) -> std::sync::MutexGuard<'_, HashMap<[u8; 32], u64>> {
        self.challenges
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A new challenge, or `None` when [`MAX_CHALLENGES`] are outstanding.
    #[must_use]
    pub fn new_challenge(&self, now_ms: u64) -> Option<([u8; 32], u64)> {
        let mut map = self.challenges();
        map.retain(|_, expires| *expires > now_ms);
        if map.len() >= MAX_CHALLENGES {
            return None;
        }
        let mut challenge = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut challenge);
        let expires = now_ms.saturating_add(CHALLENGE_TTL_MS);
        map.insert(challenge, expires);
        Some((challenge, expires))
    }

    /// Spend `challenge`: `true` when it was outstanding and not expired.
    #[must_use]
    pub fn take_challenge(&self, challenge: &[u8; 32], now_ms: u64) -> bool {
        self.challenges()
            .remove(challenge)
            .is_some_and(|expires| expires > now_ms)
    }

    fn tag(&self, moderator_id: &[u8; 16], expires_ms: u64) -> [u8; 32] {
        let mut message = Vec::with_capacity(TOKEN_DOMAIN.len() + 24);
        message.extend_from_slice(TOKEN_DOMAIN);
        message.extend_from_slice(moderator_id);
        message.extend_from_slice(&expires_ms.to_be_bytes());
        hmac_sha256(&self.key, &message)
    }

    /// A token for `moderator_id` expiring at `expires_ms`.
    #[must_use]
    pub fn issue(&self, moderator_id: &[u8; 16], expires_ms: u64) -> String {
        format!(
            "{TOKEN_PREFIX}.{}.{expires_ms}.{}",
            URL_SAFE_NO_PAD.encode(moderator_id),
            URL_SAFE_NO_PAD.encode(self.tag(moderator_id, expires_ms))
        )
    }

    /// The moderator id and expiry of a valid, unexpired token.
    #[must_use]
    pub fn verify(&self, token: &str, now_ms: u64) -> Option<([u8; 16], u64)> {
        let mut parts = token.split('.');
        let (Some(prefix), Some(id), Some(expires), Some(tag), None) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            return None;
        };
        if prefix != TOKEN_PREFIX {
            return None;
        }
        let id = URL_SAFE_NO_PAD
            .decode(id)
            .ok()
            .and_then(|b| <[u8; 16]>::try_from(b.as_slice()).ok())?;
        let expires: u64 = expires.parse().ok()?;
        let tag = URL_SAFE_NO_PAD.decode(tag).ok()?;
        let ok: bool = self.tag(&id, expires).as_slice().ct_eq(&tag).into();
        (ok && expires > now_ms).then_some((id, expires))
    }
}

/// The panel's state.
#[derive(Clone)]
pub struct PanelState {
    /// Reports and cases.
    pub store: Arc<dyn ReportStore>,
    /// The moderator list. The panel listener is started only with one.
    pub list: Arc<LoadedList>,
    /// The moderation log.
    pub log: Arc<ModerationLog>,
    /// The applying services, for statements and the panel's reads.
    pub forwarders: Arc<Forwarders>,
    /// Sessions.
    pub sessions: Arc<Sessions>,
    /// Slots for evidence blob reads, [`MAX_BLOB_READS`] of them
    /// ([`blob_slots`]).
    pub blob_slots: Arc<tokio::sync::Semaphore>,
}

/// The slots of [`PanelState::blob_slots`].
#[must_use]
pub fn blob_slots() -> Arc<tokio::sync::Semaphore> {
    Arc::new(tokio::sync::Semaphore::new(MAX_BLOB_READS))
}

/// The signed-in moderator, put in the request by the session check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moderator {
    /// The moderator id.
    pub id: [u8; 16],
    /// The listed name.
    pub name: String,
    /// When the session ends.
    pub expires_at_ms: u64,
}

/// The panel router: the API, and the app from `static_dir` when given.
pub fn build_panel_app(state: PanelState, static_dir: Option<PathBuf>) -> Router {
    let open = Router::new()
        .route("/panel/api/session/start", post(routes::session_start))
        .route("/panel/api/session/finish", post(routes::session_finish));
    let signed_in = Router::new()
        .route("/panel/api/me", get(routes::me))
        .route("/panel/api/moderators", get(routes::moderators))
        .route("/panel/api/cases", get(routes::cases))
        .route("/panel/api/cases/{kind}/{target_hex}", get(routes::case))
        .route(
            "/panel/api/cases/{kind}/{target_hex}/lock",
            post(routes::lock),
        )
        .route(
            "/panel/api/cases/{kind}/{target_hex}/unlock",
            post(routes::unlock),
        )
        .route(
            "/panel/api/cases/{kind}/{target_hex}/no-action",
            post(routes::no_action),
        )
        .route(
            "/panel/api/reports/{receipt_hex}/checks",
            post(routes::checks),
        )
        .route("/panel/api/decide", post(routes::decide))
        .route("/panel/api/appeals", get(routes::appeals))
        .route("/panel/api/log", get(routes::log))
        .route("/panel/api/blob/{blob_hex}", get(routes::blob))
        .route("/panel/api/numbers", get(routes::numbers))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_session,
        ));
    let api = open
        .merge(signed_in)
        .route("/panel/api", any(api_not_found))
        .route("/panel/api/{*rest}", any(api_not_found))
        .layer(axum::middleware::from_fn(no_store))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_PANEL_BODY_BYTES));
    let app = match static_dir {
        Some(dir) => {
            let index = dir.join("index.html");
            api.fallback_service(ServeDir::new(dir).fallback(ServeFile::new(index)))
        },
        None => api,
    };
    app.layer(TraceLayer::new_for_http().make_span_with(lakko_log_hygiene::request_span))
        .with_state(state)
}

async fn api_not_found() -> Response {
    error(StatusCode::NOT_FOUND, "not-found")
}

/// Mark every API answer as not to be stored.
async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Check the bearer token and put the [`Moderator`] in the request.
async fn require_session(
    State(state): State<PanelState>,
    mut request: Request,
    next: Next,
) -> Response {
    let now = now_ms();
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    let Some((id, expires_at_ms)) = token.and_then(|t| state.sessions.verify(t, now)) else {
        return error(StatusCode::UNAUTHORIZED, "session-required");
    };
    let Some(entry) = state.list.list.authorised_at(&id, now) else {
        tracing::warn!("panel session refused: moderator no longer listed");
        return error(StatusCode::UNAUTHORIZED, "session-required");
    };
    request.extensions_mut().insert(Moderator {
        id,
        name: entry.name.clone(),
        expires_at_ms,
    });
    next.run(request).await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn tokens_verify_until_they_expire_and_not_under_another_key() {
        let s = Sessions::new();
        let t = s.issue(&[7; 16], 1_000);
        assert_eq!(s.verify(&t, 999), Some(([7; 16], 1_000)));
        assert_eq!(s.verify(&t, 1_000), None);
        assert_eq!(Sessions::new().verify(&t, 0), None);
        let forged = t.replace(
            &URL_SAFE_NO_PAD.encode([7u8; 16]),
            &URL_SAFE_NO_PAD.encode([8u8; 16]),
        );
        assert_eq!(s.verify(&forged, 0), None);
        assert_eq!(s.verify(&format!("{t}.x"), 0), None);
        assert_eq!(s.verify("p2.a.1.b", 0), None);
    }

    #[test]
    fn challenges_are_single_use_and_expire() {
        let s = Sessions::new();
        let (c, expires) = s.new_challenge(10).unwrap();
        assert_eq!(expires, 10 + CHALLENGE_TTL_MS);
        assert!(s.take_challenge(&c, 11));
        assert!(!s.take_challenge(&c, 11));
        let (c, _) = s.new_challenge(10).unwrap();
        assert!(!s.take_challenge(&c, 10 + CHALLENGE_TTL_MS));
        for _ in 0..MAX_CHALLENGES {
            assert!(s.new_challenge(20).is_some());
        }
        assert!(s.new_challenge(20).is_none());
        // Expired ones are dropped before counting.
        assert!(s.new_challenge(20 + CHALLENGE_TTL_MS).is_some());
    }
}
