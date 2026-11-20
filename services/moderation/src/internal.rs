// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The internal listener: moderator statements in (ADR-0098 sections 4, 5,
//! step 5).
//!
//! `POST /internal/statements` is not on the public router. It is served on a
//! second listener bound to `LAKKO_MODERATION_INTERNAL_BIND` (default
//! [`DEFAULT_INTERNAL_BIND`], loopback), which nginx does not proxy, and
//! every request must present `x-lakko-internal-secret` equal to
//! `LAKKO_MODERATION_INTERNAL_SECRET` (base64 of at least
//! [`MIN_SECRET_LEN`] bytes, compared decoded and in constant time; the
//! push-gw pattern). Unset: the listener is not started.
//!
//! Body (JSON): `{"signed_statement_b64", "reason": <u8>,
//! "reason_salt_b64"}`. Base64 is standard; base64url is also accepted. No
//! account key: an applying service finds an account by its target.
//!
//! The work is [`submit_statement`], which the moderators' panel calls too:
//!
//! 1. no moderator list configured: 503 `reporting-unavailable`;
//! 2. the statement is checked against the list with
//!    `lakko_moderation::apply` (400 `bad-statement`, 403 `not-authorised`,
//!    400 `reason-mismatch`);
//! 3. it is appended to the log, idempotent by its id: a repeat returns the
//!    same index, a different statement under a used id is 409
//!    `statement-id-conflict`;
//! 4. it is forwarded to the service that applies its action
//!    (`lakko_moderation::apply::ApplyingService`): Suspend and Lift to auth;
//!    names, search and group handles to discovery; channel actions to
//!    channels; a blob delete to storage. Each is configured by
//!    `LAKKO_MODERATION_<SERVICE>_INTERNAL_URL` and
//!    `LAKKO_MODERATION_<SERVICE>_INTERNAL_SECRET` (both or neither). The
//!    answer:
//!    - not configured: `200 {"log_index", "applied": false,
//!      "forwarded": false}`;
//!    - the service's 2xx: `200 {"log_index", "forwarded": true, "applied",
//!      ...}` with the service's own `applied` (and `repeat` /
//!      `superseded` / `already_removed` / `no_pending_appeal` /
//!      `released_kept` when it sent them);
//!    - the service's 4xx: `422 {"error": "apply-refused", "detail",
//!      "log_index"}`;
//!    - unreachable, a timeout, or 5xx: `502 {"error": "apply-failed",
//!      "log_index"}`.
//!
//!    The entry stays in the log in every case, and resending the same
//!    statement forwards it again.
//!
//! The forward carries `lakko_moderation::apply::StatementBody`
//! (`{"signed_statement_b64", "moderator_list_b64", "log_index", "reason",
//! "reason_salt_b64"}`, base64 standard padded) to
//! `POST /internal/moderation/statement` and presents the configured secret
//! verbatim in `x-lakko-internal-secret`. Every applying service refuses a
//! statement issued more than 24 h before it arrives (`stale-statement`),
//! so a resend after a failed forward must happen within 24 h of signing;
//! after that the moderator signs a new statement.
//!
//! ## The list floor
//!
//! At startup the service pushes its moderator list to every configured
//! applying service ([`spawn_list_push`]): `POST /internal/moderation/list`
//! with `lakko_moderation::apply::ListBody`. A service keeps the highest
//! list version it has seen as a floor and refuses a statement checked under
//! an older list (`409 stale-list`), so a newly loaded list retires a
//! revoked moderator at every service at once, before any statement is
//! sent. A push that fails to arrive (unreachable, a timeout, a 5xx) is
//! retried with a back-off ([`LIST_PUSH_FIRST_DELAY`] doubling to
//! [`LIST_PUSH_MAX_DELAY`]) until the service answers; a 4xx (`bad-list`:
//! the service pins another root; `stale-list`: the service has seen a newer
//! list than this one) is logged and not retried, since retrying the same
//! bytes cannot change the answer.
//!
//! ## Logs
//!
//! An HTTP client error is logged as its kind only ([`error_kind`]): a
//! reqwest error's text names the request URL, which for a blob read holds
//! the blob id.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::post;
use axum::Router;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine as _;
use lakko_moderation::apply::{
    check_raw, ApplyError, ApplyingService, ListBody, StatementBody, ALL_ACTIONS,
};
use lakko_moderation::statement::Statement;
use serde::Deserialize;
use serde_json::json;
use subtle::ConstantTimeEq as _;
use tower_http::trace::TraceLayer;

use crate::error;
use crate::list::LoadedList;
use crate::log::{statement_key, LogError, ModerationLog, LEAF_STATEMENT};

/// Default internal bind: loopback.
pub const DEFAULT_INTERNAL_BIND: &str = "127.0.0.1:8096";
/// The header the shared secret is presented in.
pub const SECRET_HEADER: &str = "x-lakko-internal-secret";
/// Minimum decoded length of the shared secret.
pub const MIN_SECRET_LEN: usize = 16;
/// Body limit: a signed statement is under 6 KiB as base64.
const MAX_INTERNAL_BODY_BYTES: usize = 32 * 1024;
/// The route a statement is forwarded to at every applying service.
pub const APPLY_STATEMENT_PATH: &str = "/internal/moderation/statement";
/// Timeout of a forward.
const FORWARD_TIMEOUT: Duration = Duration::from_secs(10);
/// The route a moderator list is pushed to at every applying service.
pub const LIST_PUSH_PATH: &str = "/internal/moderation/list";
/// First wait before a failed list push is retried. Chosen, not measured.
pub const LIST_PUSH_FIRST_DELAY: Duration = Duration::from_secs(1);
/// Longest wait between list push retries. Chosen, not measured.
pub const LIST_PUSH_MAX_DELAY: Duration = Duration::from_secs(300);

/// What a reqwest error was, without its text (which names the URL).
#[must_use]
pub fn error_kind(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connect"
    } else if e.is_decode() {
        "decode"
    } else if e.is_body() {
        "body"
    } else if e.is_redirect() {
        "redirect"
    } else if e.is_builder() {
        "builder"
    } else if e.is_request() {
        "request"
    } else if e.is_status() {
        "status"
    } else {
        "other"
    }
}

/// The internal listener's state.
#[derive(Clone)]
pub struct InternalState {
    /// The moderation log.
    pub log: Arc<ModerationLog>,
    /// The moderator list, if configured.
    pub list: Option<Arc<LoadedList>>,
    /// Where actions are applied.
    pub forwarders: Arc<Forwarders>,
    /// The decoded shared secret.
    pub secret: Arc<Vec<u8>>,
}

/// Parse a shared secret: `Ok(None)` when unset or blank, base64 of at least
/// [`MIN_SECRET_LEN`] bytes otherwise.
///
/// # Errors
///
/// A message naming `var` when the value is not base64 or too short.
pub fn parse_secret(var: &str, raw: Option<&str>) -> Result<Option<Arc<Vec<u8>>>, String> {
    let Some(raw) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let bytes = STANDARD
        .decode(raw.as_bytes())
        .map_err(|_| format!("{var} is not valid base64"))?;
    if bytes.len() < MIN_SECRET_LEN {
        return Err(format!(
            "{var} decodes to {} bytes; need >= {MIN_SECRET_LEN}",
            bytes.len()
        ));
    }
    Ok(Some(Arc::new(bytes)))
}

/// The environment variable prefix of a service's forwarding configuration.
const fn env_prefix(service: ApplyingService) -> &'static str {
    match service {
        ApplyingService::Auth => "LAKKO_MODERATION_AUTH_INTERNAL",
        ApplyingService::Discovery => "LAKKO_MODERATION_DISCOVERY_INTERNAL",
        ApplyingService::Channels => "LAKKO_MODERATION_CHANNELS_INTERNAL",
        ApplyingService::Storage => "LAKKO_MODERATION_STORAGE_INTERNAL",
    }
}

/// The client that forwards a statement to one applying service.
#[derive(Debug)]
pub struct Forwarder {
    service: ApplyingService,
    base: String,
    url: String,
    secret: String,
    http: reqwest::Client,
}

/// What a service answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForwardOutcome {
    /// 2xx, with the service's JSON answer (`Null` when it sent none).
    Applied(serde_json::Value),
    /// 4xx, with the service's error code if it sent one.
    Refused(Option<String>),
    /// Unreachable, a timeout, or 5xx.
    Failed,
}

impl Forwarder {
    /// A forwarder to `base_url` of `service`, presenting `secret` (the
    /// configured value).
    ///
    /// # Errors
    ///
    /// The HTTP client does not build.
    pub fn new(service: ApplyingService, base_url: &str, secret: &str) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .timeout(FORWARD_TIMEOUT)
            .build()
            .map_err(|e| format!("build the {} HTTP client: {e}", service.name()))?;
        let base = base_url.trim().trim_end_matches('/').to_string();
        Ok(Self {
            service,
            url: format!("{base}{APPLY_STATEMENT_PATH}"),
            base,
            secret: secret.trim().to_string(),
            http,
        })
    }

    /// From `LAKKO_MODERATION_<SERVICE>_INTERNAL_URL` and
    /// `LAKKO_MODERATION_<SERVICE>_INTERNAL_SECRET`: both or neither.
    ///
    /// # Errors
    ///
    /// Only one of the two set.
    pub fn from_env(service: ApplyingService) -> Result<Option<Self>, String> {
        let prefix = env_prefix(service);
        let read = |suffix: &str| {
            std::env::var(format!("{prefix}_{suffix}"))
                .ok()
                .filter(|v| !v.trim().is_empty())
        };
        match (read("URL"), read("SECRET")) {
            (None, None) => Ok(None),
            (Some(url), Some(secret)) => Self::new(service, &url, &secret).map(Some),
            _ => Err(format!(
                "set both {prefix}_URL and {prefix}_SECRET, or neither"
            )),
        }
    }

    /// The service this forwarder reaches.
    #[must_use]
    pub const fn service(&self) -> ApplyingService {
        self.service
    }

    /// Call another internal route of this service (the panel's reads):
    /// `GET` without a body, `POST` with `body` as JSON. Returns the status
    /// and the JSON answer (`Null` when there is none), or `None` when the
    /// service could not be reached.
    pub async fn call(
        &self,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Option<(u16, serde_json::Value)> {
        let url = format!("{}{path}", self.base);
        let request = match body {
            Some(b) => self.http.post(&url).json(b),
            None => self.http.get(&url),
        };
        match request.header(SECRET_HEADER, &self.secret).send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let value = resp
                    .json::<serde_json::Value>()
                    .await
                    .unwrap_or(serde_json::Value::Null);
                Some((status, value))
            },
            Err(e) => {
                tracing::warn!(
                    service = self.service.name(),
                    error_kind = error_kind(&e),
                    "an internal read failed"
                );
                None
            },
        }
    }

    /// `GET` another internal route of this service and read its body as
    /// bytes, at most `max_bytes` (the panel's evidence blobs). Returns the
    /// status and the body; `None` when the service could not be reached,
    /// timed out, or sent more than `max_bytes`.
    pub async fn get_bytes(
        &self,
        path: &str,
        max_bytes: usize,
        timeout: Duration,
    ) -> Option<(u16, Vec<u8>)> {
        let url = format!("{}{path}", self.base);
        let service = self.service.name();
        let mut resp = match self
            .http
            .get(&url)
            .header(SECRET_HEADER, &self.secret)
            .timeout(timeout)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    service,
                    error_kind = error_kind(&e),
                    "an internal read failed"
                );
                return None;
            },
        };
        let status = resp.status().as_u16();
        if resp.content_length().is_some_and(|n| n > max_bytes as u64) {
            tracing::warn!(service, "an internal read was over its size cap");
            return None;
        }
        let mut body = Vec::new();
        loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    if body.len() + chunk.len() > max_bytes {
                        tracing::warn!(service, "an internal read was over its size cap");
                        return None;
                    }
                    body.extend_from_slice(&chunk);
                },
                Ok(None) => return Some((status, body)),
                Err(e) => {
                    tracing::warn!(
                        service,
                        error_kind = error_kind(&e),
                        "an internal read failed"
                    );
                    return None;
                },
            }
        }
    }

    /// Push the moderator list once: `POST /internal/moderation/list`.
    pub async fn push_list(&self, body: &ListBody) -> PushOutcome {
        let service = self.service.name();
        let resp = match self
            .http
            .post(format!("{}{LIST_PUSH_PATH}", self.base))
            .header(SECRET_HEADER, &self.secret)
            .json(body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    service,
                    error_kind = error_kind(&e),
                    "pushing the moderator list failed"
                );
                return PushOutcome::Failed;
            },
        };
        let status = resp.status();
        let answer = resp.json::<serde_json::Value>().await.ok();
        if status.is_success() {
            PushOutcome::Accepted(
                answer
                    .as_ref()
                    .and_then(|v| v.get("list_version"))
                    .and_then(serde_json::Value::as_u64),
            )
        } else if status.is_client_error() {
            PushOutcome::Refused(
                answer
                    .as_ref()
                    .and_then(|v| v.get("error"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
            )
        } else {
            tracing::warn!(
                service,
                status = status.as_u16(),
                "a service failed the moderator list"
            );
            PushOutcome::Failed
        }
    }

    async fn apply(&self, body: &StatementBody) -> ForwardOutcome {
        let service = self.service.name();
        let resp = match self
            .http
            .post(&self.url)
            .header(SECRET_HEADER, &self.secret)
            .json(body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    service,
                    error_kind = error_kind(&e),
                    "forwarding a statement failed"
                );
                return ForwardOutcome::Failed;
            },
        };
        let status = resp.status();
        if status.is_success() {
            let answer = resp
                .json::<serde_json::Value>()
                .await
                .unwrap_or(serde_json::Value::Null);
            ForwardOutcome::Applied(answer)
        } else if status.is_client_error() {
            let code = resp
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned));
            tracing::warn!(
                service,
                status = status.as_u16(),
                "a service refused a statement"
            );
            ForwardOutcome::Refused(code)
        } else {
            tracing::warn!(
                service,
                status = status.as_u16(),
                "a service failed a statement"
            );
            ForwardOutcome::Failed
        }
    }
}

/// What a service answered to a list push.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushOutcome {
    /// 2xx, with the version the service answered, if it sent one.
    Accepted(Option<u64>),
    /// 4xx, with the service's error code if it sent one. Not retried.
    Refused(Option<String>),
    /// Unreachable, a timeout, or 5xx. Retried.
    Failed,
}

/// Push `list` to `forwarder` until it answers: a [`PushOutcome::Failed`]
/// is retried after `first_delay`, doubling up to `max_delay`; an accepted
/// or refused push ends it. Returns the final outcome.
pub async fn push_list_until_answered(
    forwarder: &Forwarder,
    list: &LoadedList,
    first_delay: Duration,
    max_delay: Duration,
) -> PushOutcome {
    let body = ListBody::new(&list.bytes);
    let service = forwarder.service.name();
    let mut delay = first_delay;
    loop {
        match forwarder.push_list(&body).await {
            PushOutcome::Failed => {
                tokio::time::sleep(delay).await;
                delay = delay.saturating_mul(2).min(max_delay);
            },
            PushOutcome::Accepted(version) => {
                tracing::info!(
                    service,
                    list_version = list.list.list_version,
                    floor = version,
                    "moderator list accepted"
                );
                return PushOutcome::Accepted(version);
            },
            PushOutcome::Refused(code) => {
                tracing::error!(
                    service,
                    list_version = list.list.list_version,
                    code = code.as_deref().unwrap_or("none"),
                    "a service refused the moderator list; statements under it will be refused \
                     there"
                );
                return PushOutcome::Refused(code);
            },
        }
    }
}

/// Push `list` to every configured applying service in the background, one
/// task each, with the retry of [`push_list_until_answered`] and the delays
/// [`LIST_PUSH_FIRST_DELAY`] / [`LIST_PUSH_MAX_DELAY`].
pub fn spawn_list_push(
    forwarders: &Arc<Forwarders>,
    list: &Arc<LoadedList>,
) -> Vec<tokio::task::JoinHandle<PushOutcome>> {
    [
        ApplyingService::Auth,
        ApplyingService::Discovery,
        ApplyingService::Channels,
        ApplyingService::Storage,
    ]
    .into_iter()
    .filter(|s| forwarders.get(*s).is_some())
    .map(|service| {
        let forwarders = Arc::clone(forwarders);
        let list = Arc::clone(list);
        tokio::spawn(async move {
            match forwarders.get(service) {
                Some(f) => {
                    push_list_until_answered(f, &list, LIST_PUSH_FIRST_DELAY, LIST_PUSH_MAX_DELAY)
                        .await
                },
                None => PushOutcome::Refused(None),
            }
        })
    })
    .collect()
}

/// One forwarder per applying service, each optional.
#[derive(Debug, Default)]
pub struct Forwarders {
    /// Suspend, Lift.
    pub auth: Option<Forwarder>,
    /// Names, search, group handles.
    pub discovery: Option<Forwarder>,
    /// Channel actions.
    pub channels: Option<Forwarder>,
    /// Blob deletes.
    pub storage: Option<Forwarder>,
}

impl Forwarders {
    /// None configured.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// All four from the environment.
    ///
    /// # Errors
    ///
    /// A service with only one of its two variables set.
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            auth: Forwarder::from_env(ApplyingService::Auth)?,
            discovery: Forwarder::from_env(ApplyingService::Discovery)?,
            channels: Forwarder::from_env(ApplyingService::Channels)?,
            storage: Forwarder::from_env(ApplyingService::Storage)?,
        })
    }

    /// Add `forwarder` in its service's slot, replacing what was there.
    #[must_use]
    pub fn with(mut self, forwarder: Forwarder) -> Self {
        let slot = match forwarder.service {
            ApplyingService::Auth => &mut self.auth,
            ApplyingService::Discovery => &mut self.discovery,
            ApplyingService::Channels => &mut self.channels,
            ApplyingService::Storage => &mut self.storage,
        };
        *slot = Some(forwarder);
        self
    }

    /// The forwarder of `service`, if configured.
    #[must_use]
    pub const fn get(&self, service: ApplyingService) -> Option<&Forwarder> {
        match service {
            ApplyingService::Auth => self.auth.as_ref(),
            ApplyingService::Discovery => self.discovery.as_ref(),
            ApplyingService::Channels => self.channels.as_ref(),
            ApplyingService::Storage => self.storage.as_ref(),
        }
    }

    /// The services that are not configured.
    #[must_use]
    pub fn missing(&self) -> Vec<&'static str> {
        [
            ApplyingService::Auth,
            ApplyingService::Discovery,
            ApplyingService::Channels,
            ApplyingService::Storage,
        ]
        .into_iter()
        .filter(|s| self.get(*s).is_none())
        .map(ApplyingService::name)
        .collect()
    }
}

/// The internal router.
pub fn build_internal_app(state: InternalState) -> Router {
    Router::new()
        .route("/internal/statements", post(statements))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_internal_secret,
        ))
        .layer(axum::extract::DefaultBodyLimit::max(
            MAX_INTERNAL_BODY_BYTES,
        ))
        .layer(TraceLayer::new_for_http().make_span_with(lakko_log_hygiene::request_span))
        .with_state(state)
}

/// Refuse a request without the secret: 401 for a missing, malformed or
/// unequal header.
async fn require_internal_secret(
    State(state): State<InternalState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let presented = request
        .headers()
        .get(SECRET_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| STANDARD.decode(v.trim().as_bytes()).ok());
    let Some(presented) = presented else {
        tracing::warn!("internal request refused: missing or malformed secret");
        return Err(StatusCode::UNAUTHORIZED);
    };
    let matches: bool = state.secret.as_slice().ct_eq(presented.as_slice()).into();
    if !matches {
        tracing::warn!("internal request refused: secret mismatch");
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(request).await)
}

#[derive(Deserialize)]
struct StatementReq {
    signed_statement_b64: String,
    reason: u8,
    reason_salt_b64: String,
}

/// Decode base64, standard or url-safe, padded or not.
pub(crate) fn b64(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    STANDARD
        .decode(s)
        .or_else(|_| STANDARD_NO_PAD.decode(s))
        .or_else(|_| URL_SAFE.decode(s))
        .or_else(|_| URL_SAFE_NO_PAD.decode(s))
        .ok()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// What [`submit_statement`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submitted {
    /// The statement, decoded and verified.
    pub statement: Statement,
    /// Its index in the log.
    pub log_index: u64,
    /// `false` when the identical statement was already logged.
    pub new: bool,
    /// The service that applies it.
    pub service: ApplyingService,
    /// What that service answered; `None` when it is not configured.
    pub forwarded: Option<ForwardOutcome>,
}

impl Submitted {
    /// The response of `POST /internal/statements`.
    #[must_use]
    pub fn response(&self) -> Response {
        let (status, body) = self.response_parts();
        (status, Json(body)).into_response()
    }

    /// The status and body of [`Self::response`], for a caller that adds
    /// fields (the panel).
    #[must_use]
    pub fn response_parts(&self) -> (StatusCode, serde_json::Value) {
        let log_index = self.log_index;
        match &self.forwarded {
            None => (
                StatusCode::OK,
                json!({
                    "log_index": log_index,
                    "applied": false,
                    "forwarded": false,
                }),
            ),
            Some(ForwardOutcome::Applied(answer)) => {
                let mut body = json!({
                    "log_index": log_index,
                    "forwarded": true,
                    "applied": answer.get("applied").and_then(serde_json::Value::as_bool).unwrap_or(true),
                });
                for key in [
                    "repeat",
                    "superseded",
                    "already_removed",
                    "no_pending_appeal",
                    "released_kept",
                ] {
                    if let Some(v) = answer.get(key).and_then(serde_json::Value::as_bool) {
                        body[key] = json!(v);
                    }
                }
                (StatusCode::OK, body)
            },
            Some(ForwardOutcome::Refused(detail)) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({ "error": "apply-refused", "detail": detail, "log_index": log_index }),
            ),
            Some(ForwardOutcome::Failed) => (
                StatusCode::BAD_GATEWAY,
                json!({ "error": "apply-failed", "log_index": log_index }),
            ),
        }
    }
}

/// Why [`submit_statement`] refused before forwarding.
#[derive(Debug)]
pub enum SubmitError {
    /// The statement failed a check; nothing was logged.
    Check(ApplyError),
    /// A different statement holds its id in the log.
    Conflict,
    /// The log failed.
    Log(LogError),
}

impl IntoResponse for SubmitError {
    fn into_response(self) -> Response {
        match self {
            Self::Check(e) => error(
                StatusCode::from_u16(e.status()).unwrap_or(StatusCode::BAD_REQUEST),
                e.code(),
            ),
            Self::Conflict => error(StatusCode::CONFLICT, "statement-id-conflict"),
            Self::Log(e) => {
                tracing::error!(error = %e, "appending a statement to the moderation log failed");
                error(StatusCode::INTERNAL_SERVER_ERROR, "internal")
            },
        }
    }
}

/// Check a signed statement against the list, append it to the log and
/// forward it to the service that applies its action. The one statement
/// path: the internal route and the moderators' panel both call it.
///
/// `reason` and `reason_salt` are as received; an unknown reason byte or a
/// salt that is not 16 bytes is refused as `reason-mismatch`.
///
/// # Errors
///
/// [`SubmitError`]; nothing is logged on a check failure.
pub async fn submit_statement(
    log: &ModerationLog,
    list: &LoadedList,
    forwarders: &Forwarders,
    signed_statement: &[u8],
    reason: u8,
    reason_salt: &[u8],
    now_ms: u64,
) -> Result<Submitted, SubmitError> {
    let checked = check_raw(
        &list.list,
        signed_statement,
        reason,
        reason_salt,
        ALL_ACTIONS,
        i64::try_from(now_ms).unwrap_or(i64::MAX),
    )
    .map_err(SubmitError::Check)?;
    let appended = match log
        .append(
            LEAF_STATEMENT,
            statement_key(&checked.statement.statement_id),
            signed_statement,
            now_ms,
        )
        .await
    {
        Ok(a) => a,
        Err(LogError::DedupConflict) => return Err(SubmitError::Conflict),
        Err(e) => return Err(SubmitError::Log(e)),
    };
    let service = ApplyingService::of(checked.statement.action);
    tracing::info!(
        log_index = appended.index,
        new = appended.new,
        action = checked.statement.action as u8,
        service = service.name(),
        "moderation statement logged"
    );
    let forwarded = match forwarders.get(service) {
        None => {
            tracing::warn!(
                service = service.name(),
                "a statement was logged but its service is not configured"
            );
            None
        },
        Some(forwarder) => {
            let body = StatementBody::new(
                signed_statement,
                &list.bytes,
                appended.index,
                checked.reason,
                &checked.reason_salt,
            );
            Some(forwarder.apply(&body).await)
        },
    };
    Ok(Submitted {
        statement: checked.statement,
        log_index: appended.index,
        new: appended.new,
        service,
        forwarded,
    })
}

/// `POST /internal/statements`.
async fn statements(State(state): State<InternalState>, body: Bytes) -> Response {
    let Some(loaded) = state.list.as_ref() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "reporting-unavailable");
    };
    let Ok(req) = serde_json::from_slice::<StatementReq>(&body) else {
        return error(StatusCode::BAD_REQUEST, "bad-request");
    };
    let signed = b64(&req.signed_statement_b64).unwrap_or_default();
    let salt = b64(&req.reason_salt_b64).unwrap_or_default();
    match submit_statement(
        &state.log,
        loaded,
        &state.forwarders,
        &signed,
        req.reason,
        &salt,
        now_ms(),
    )
    .await
    {
        Ok(submitted) => submitted.response(),
        Err(e) => e.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::{env_prefix, parse_secret, Forwarder, Forwarders};
    use lakko_moderation::apply::{ApplyingService, ALL_ACTIONS};

    #[test]
    fn secrets_parse_or_refuse() {
        assert!(parse_secret("V", None).unwrap().is_none());
        assert!(parse_secret("V", Some("  ")).unwrap().is_none());
        assert!(parse_secret("V", Some("not base64!")).is_err());
        assert!(parse_secret("V", Some("c2hvcnQ=")).is_err());
        let s = parse_secret("V", Some("MDEyMzQ1Njc4OWFiY2RlZg=="))
            .unwrap()
            .unwrap();
        assert_eq!(s.as_slice(), b"0123456789abcdef");
    }

    #[test]
    fn every_action_routes_to_its_slot() {
        let mut all = Forwarders::none();
        for service in [
            ApplyingService::Auth,
            ApplyingService::Discovery,
            ApplyingService::Channels,
            ApplyingService::Storage,
        ] {
            assert!(env_prefix(service).starts_with("LAKKO_MODERATION_"));
            all = all.with(Forwarder::new(service, "http://127.0.0.1:1", "c2VjcmV0").unwrap());
        }
        assert!(all.missing().is_empty());
        for action in ALL_ACTIONS {
            let service = ApplyingService::of(*action);
            assert_eq!(all.get(service).unwrap().service(), service);
        }
        assert_eq!(
            Forwarders::none().missing(),
            vec!["auth", "discovery", "channels", "storage"]
        );
    }
}
