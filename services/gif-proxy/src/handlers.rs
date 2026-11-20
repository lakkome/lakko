// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The `/gifs` routes.
//!
//! Every route runs behind [`crate::auth::require_session`]. Order inside a
//! handler: provider configured (503) → per-account rate limit (429) → input
//! validation (400) → upstream (502 / 404).

use std::sync::Arc;
use std::time::Instant;

use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Extension, Path, Query, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::AuthenticatedAccount;
use crate::giphy::{is_valid_gif_id, GifItem, GiphyClient, UpstreamError, Variant, MAX_OFFSET};
use crate::metrics::{self, UpstreamCall};
use crate::rate_limit::{AccountRateLimiter, Decision};
use crate::rendition_cache::RenditionCache;
use crate::AppState;

/// Longest accepted search query, in characters, after trimming.
pub(crate) const MAX_QUERY_CHARS: usize = 100;

/// Everything a configured provider needs.
pub(crate) struct GifService {
    pub(crate) client: GiphyClient,
    pub(crate) search_limiter: AccountRateLimiter,
    pub(crate) media_limiter: AccountRateLimiter,
    pub(crate) cache: RenditionCache,
}

/// Every non-2xx answer a `/gifs` handler gives (401 comes from the auth
/// middleware). The body is always `{"error": "<code>"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApiError {
    NotConfigured,
    RateLimited { retry_after_secs: u64 },
    InvalidQuery,
    InvalidOffset,
    InvalidGifId,
    InvalidVariant,
    NotFound,
    UpstreamFailed,
}

impl ApiError {
    const fn status_and_code(self) -> (StatusCode, &'static str) {
        match self {
            Self::NotConfigured => (
                StatusCode::SERVICE_UNAVAILABLE,
                "gif-provider-not-configured",
            ),
            Self::RateLimited { .. } => (StatusCode::TOO_MANY_REQUESTS, "rate-limited"),
            Self::InvalidQuery => (StatusCode::BAD_REQUEST, "invalid-query"),
            Self::InvalidOffset => (StatusCode::BAD_REQUEST, "invalid-offset"),
            Self::InvalidGifId => (StatusCode::BAD_REQUEST, "invalid-gif-id"),
            Self::InvalidVariant => (StatusCode::BAD_REQUEST, "invalid-variant"),
            Self::NotFound => (StatusCode::NOT_FOUND, "gif-not-found"),
            Self::UpstreamFailed => (StatusCode::BAD_GATEWAY, "gif-upstream-failed"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = self.status_and_code();
        let mut response = (
            status,
            [(CACHE_CONTROL, HeaderValue::from_static("no-store"))],
            Json(json!({ "error": code })),
        )
            .into_response();
        // The code as the `code` label of the request counters (ADR-0096).
        response
            .extensions_mut()
            .insert(lakko_metrics::ErrorCode(code));
        if let Self::RateLimited { retry_after_secs } = self {
            response
                .headers_mut()
                .insert(RETRY_AFTER, HeaderValue::from(retry_after_secs));
        }
        response
    }
}

fn from_upstream(error: UpstreamError) -> ApiError {
    // The error text names a failure kind only: no URL, query, id or account.
    tracing::warn!(error = %error, "gif upstream call failed");
    match error {
        UpstreamError::NotFound => ApiError::NotFound,
        _ => ApiError::UpstreamFailed,
    }
}

fn admit(limiter: &AccountRateLimiter, account: AuthenticatedAccount) -> Result<(), ApiError> {
    match limiter.check(account.0) {
        Decision::Allowed => Ok(()),
        Decision::Limited { retry_after_secs } => Err(ApiError::RateLimited { retry_after_secs }),
    }
}

fn service(state: &AppState) -> Result<&Arc<GifService>, ApiError> {
    state.gif.as_ref().ok_or(ApiError::NotConfigured)
}

/// `offset`: absent means 0; otherwise a decimal integer in `0..=MAX_OFFSET`.
pub(crate) fn validate_offset(raw: Option<&str>) -> Result<u32, ApiError> {
    let Some(raw) = raw else {
        return Ok(0);
    };
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ApiError::InvalidOffset);
    }
    raw.parse::<u32>()
        .ok()
        .filter(|n| *n <= MAX_OFFSET)
        .ok_or(ApiError::InvalidOffset)
}

/// `q`: required, trimmed, 1..=MAX_QUERY_CHARS characters.
pub(crate) fn validate_query(raw: Option<&str>) -> Result<String, ApiError> {
    let trimmed = raw.map(str::trim).ok_or(ApiError::InvalidQuery)?;
    let chars = trimmed.chars().count();
    if chars == 0 || chars > MAX_QUERY_CHARS {
        return Err(ApiError::InvalidQuery);
    }
    Ok(trimmed.to_owned())
}

/// The path a client uses to fetch a variant — always this proxy, never GIPHY.
pub(crate) fn proxy_media_path(id: &str, variant: Variant) -> String {
    format!("/gifs/media/{id}/{}", variant.as_str())
}

#[derive(Debug, Deserialize)]
pub(crate) struct ListParams {
    q: Option<String>,
    offset: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct GifItemJson {
    pub(crate) id: String,
    pub(crate) preview_url: String,
    pub(crate) full_url: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl From<&GifItem> for GifItemJson {
    fn from(item: &GifItem) -> Self {
        Self {
            id: item.id.clone(),
            preview_url: proxy_media_path(&item.id, Variant::Preview),
            full_url: proxy_media_path(&item.id, Variant::Full),
            width: item.renditions.width,
            height: item.renditions.height,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct ListResponse {
    items: Vec<GifItemJson>,
    next_offset: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
enum ListKind {
    Trending,
    Search,
}

/// `GET /gifs/trending?offset=N`
pub(crate) async fn trending(
    State(state): State<AppState>,
    Extension(account): Extension<AuthenticatedAccount>,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Response, ApiError> {
    list(&state, account, ListKind::Trending, params).await
}

/// `GET /gifs/search?q=...&offset=N`
pub(crate) async fn search(
    State(state): State<AppState>,
    Extension(account): Extension<AuthenticatedAccount>,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Response, ApiError> {
    list(&state, account, ListKind::Search, params).await
}

async fn list(
    state: &AppState,
    account: AuthenticatedAccount,
    kind: ListKind,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Response, ApiError> {
    let svc = service(state)?;
    admit(&svc.search_limiter, account)?;

    let Query(params) = params.map_err(|_| match kind {
        ListKind::Search => ApiError::InvalidQuery,
        ListKind::Trending => ApiError::InvalidOffset,
    })?;
    let offset = validate_offset(params.offset.as_deref())?;
    let page = match kind {
        ListKind::Trending => {
            metrics::upstream(UpstreamCall::Trending, svc.client.trending(offset)).await
        },
        ListKind::Search => {
            let query = validate_query(params.q.as_deref())?;
            metrics::upstream(UpstreamCall::Search, svc.client.search(&query, offset)).await
        },
    }
    .map_err(|e| match from_upstream(e) {
        ApiError::NotFound => ApiError::UpstreamFailed,
        other => other,
    })?;

    let now = Instant::now();
    let items = page
        .items
        .iter()
        .map(|item| {
            svc.cache
                .insert_at(account.0, item.id.clone(), item.renditions.clone(), now);
            GifItemJson::from(item)
        })
        .collect();

    Ok((
        [(CACHE_CONTROL, HeaderValue::from_static("private, no-store"))],
        Json(ListResponse {
            items,
            next_offset: page.next_offset,
        }),
    )
        .into_response())
}

/// `GET /gifs/media/{id}/{variant}`
pub(crate) async fn media(
    State(state): State<AppState>,
    Extension(account): Extension<AuthenticatedAccount>,
    path: Result<Path<(String, String)>, PathRejection>,
) -> Result<Response, ApiError> {
    let svc = service(&state)?;
    admit(&svc.media_limiter, account)?;

    let Path((id, variant)) = path.map_err(|_| ApiError::InvalidGifId)?;
    if !is_valid_gif_id(&id) {
        return Err(ApiError::InvalidGifId);
    }
    let variant = Variant::parse(&variant).ok_or(ApiError::InvalidVariant)?;

    let cached = svc.cache.get_at(account.0, &id, Instant::now());
    metrics::rendition_cache(cached.is_some());
    let renditions = if let Some(hit) = cached {
        hit
    } else {
        let looked_up = metrics::upstream(UpstreamCall::Lookup, svc.client.lookup(&id))
            .await
            .map_err(from_upstream)?;
        svc.cache
            .insert_at(account.0, id, looked_up.clone(), Instant::now());
        looked_up
    };

    let media = metrics::upstream(
        UpstreamCall::Media,
        svc.client
            .fetch_media(renditions.for_variant(variant), variant.max_bytes()),
    )
    .await
    .map_err(from_upstream)?;

    let mut response = Response::new(media.body);
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(media.content_type));
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=3600"),
    );
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    if let Some(len) = media.content_length {
        headers.insert(CONTENT_LENGTH, HeaderValue::from(len));
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets() {
        assert_eq!(validate_offset(None), Ok(0));
        assert_eq!(validate_offset(Some("0")), Ok(0));
        assert_eq!(validate_offset(Some("4999")), Ok(4999));
        for bad in ["", "5000", "-1", "+5", "1.5", "abc", " 5", "99999999999"] {
            assert_eq!(
                validate_offset(Some(bad)),
                Err(ApiError::InvalidOffset),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn queries() {
        assert_eq!(validate_query(Some("  cats  ")), Ok("cats".to_owned()));
        assert_eq!(
            validate_query(Some(&"a".repeat(MAX_QUERY_CHARS))),
            Ok("a".repeat(MAX_QUERY_CHARS))
        );
        // 100 two-byte characters: counted as characters, not bytes.
        let accented = "\u{e9}".repeat(MAX_QUERY_CHARS);
        assert_eq!(validate_query(Some(&accented)), Ok(accented.clone()));
        for bad in [
            None,
            Some(""),
            Some("   "),
            Some(&*"a".repeat(MAX_QUERY_CHARS + 1)),
        ] {
            assert_eq!(validate_query(bad), Err(ApiError::InvalidQuery));
        }
    }

    #[test]
    fn mapped_items_point_only_at_the_proxy() {
        use crate::giphy::{RenditionRef, Renditions};
        let item = GifItem {
            id: "abc123".into(),
            renditions: Renditions {
                preview: RenditionRef {
                    url: "https://media1.giphy.com/media/abc123/200w.webp?cid=x".into(),
                    declared_size: Some(10),
                },
                full: RenditionRef {
                    url: "https://media1.giphy.com/media/abc123/giphy.gif?cid=x".into(),
                    declared_size: Some(10),
                },
                width: 480,
                height: 270,
            },
        };
        let json = GifItemJson::from(&item);
        assert_eq!(
            json,
            GifItemJson {
                id: "abc123".into(),
                preview_url: "/gifs/media/abc123/preview".into(),
                full_url: "/gifs/media/abc123/full".into(),
                width: 480,
                height: 270,
            }
        );
        let text = serde_json::to_string(&json).expect("json");
        assert!(!text.contains("giphy"), "{text}");
    }

    #[test]
    fn error_codes_and_statuses() {
        let cases = [
            (ApiError::NotConfigured, 503, "gif-provider-not-configured"),
            (
                ApiError::RateLimited {
                    retry_after_secs: 3,
                },
                429,
                "rate-limited",
            ),
            (ApiError::InvalidQuery, 400, "invalid-query"),
            (ApiError::InvalidOffset, 400, "invalid-offset"),
            (ApiError::InvalidGifId, 400, "invalid-gif-id"),
            (ApiError::InvalidVariant, 400, "invalid-variant"),
            (ApiError::NotFound, 404, "gif-not-found"),
            (ApiError::UpstreamFailed, 502, "gif-upstream-failed"),
        ];
        for (error, status, code) in cases {
            let (s, c) = error.status_and_code();
            assert_eq!((s.as_u16(), c), (status, code));
        }
    }
}
