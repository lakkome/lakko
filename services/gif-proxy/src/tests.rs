// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! End-to-end router tests against a local stand-in for GIPHY.
//!
//! The stand-in is an axum server on `127.0.0.1:0` serving both the API
//! (`/v1/gifs/...`) and the media files (`/media/...`). The proxy under test
//! reaches it through [`UpstreamSettings`] with [`MediaHostPolicy::Loopback`],
//! a test-only policy. No test touches the real GIPHY API.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use http_body_util::BodyExt as _;
use lakko_auth_token::TokenSecret;
use serde_json::{json, Value};
use tower::ServiceExt as _;

use crate::config::{RateLimitConfig, UpstreamSettings};
use crate::giphy::{GiphyClient, MediaHostPolicy, FULL_MAX_BYTES, PREVIEW_MAX_BYTES};
use crate::handlers::GifService;
use crate::rate_limit::AccountRateLimiter;
use crate::rendition_cache::RenditionCache;
use crate::{build_app, AppState};

const API_KEY: &str = "test-api-key-7f3a";
const GENEROUS: RateLimitConfig = RateLimitConfig {
    per_minute: 10_000,
    burst: 10_000,
};

// ---- The GIPHY stand-in ----------------------------------------------------

#[derive(Clone, Default)]
struct Upstream {
    base: String,
    lookups: Arc<AtomicUsize>,
    list_queries: Arc<Mutex<Vec<HashMap<String, String>>>>,
    media_hits: Arc<AtomicUsize>,
}

fn gif_json(base: &str, id: &str, preview: &str, full: &str) -> Value {
    json!({
        "type": "gif",
        "id": id,
        "images": {
            "fixed_width": {
                "url": format!("{base}/media/{preview}"),
                "width": "200", "height": "150", "size": "1000"
            },
            "original": {
                "url": format!("{base}/media/{full}"),
                "width": "480", "height": "360", "size": "5000"
            }
        }
    })
}

async fn fake_list(
    State(up): State<Upstream>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if params.get("api_key").map(String::as_str) != Some(API_KEY) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let offset: u64 = params
        .get("offset")
        .and_then(|o| o.parse().ok())
        .unwrap_or(0);
    up.list_queries.lock().expect("lock").push(params);
    axum::Json(json!({
        "data": [
            gif_json(&up.base, "okgif", "ok-preview.gif", "ok-full.gif"),
            gif_json(&up.base, "bad-id!", "ok-preview.gif", "ok-full.gif"),
        ],
        "pagination": { "total_count": 100, "count": 2, "offset": offset },
        "meta": { "status": 200 }
    }))
    .into_response()
}

async fn fake_lookup(
    State(up): State<Upstream>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if params.get("api_key").map(String::as_str) != Some(API_KEY) {
        return StatusCode::FORBIDDEN.into_response();
    }
    up.lookups.fetch_add(1, Ordering::SeqCst);
    let b = &up.base;
    let data = match id.as_str() {
        "okgif" => gif_json(b, &id, "ok-preview.gif", "ok-full.gif"),
        "bigcl" => gif_json(b, &id, "big-cl", "ok-full.gif"),
        "exactcl" => gif_json(b, &id, "exact-cl", "ok-full.gif"),
        "bigchunk" => gif_json(b, &id, "big-chunked", "ok-full.gif"),
        "fullbig" => gif_json(b, &id, "ok-preview.gif", "full-big"),
        "htmlct" => gif_json(b, &id, "html", "ok-full.gif"),
        "redirout" => gif_json(b, &id, "redirect-out", "ok-full.gif"),
        "redirin" => gif_json(b, &id, "redirect-in", "ok-full.gif"),
        "slow" => gif_json(b, &id, "slow", "ok-full.gif"),
        "offhost" => json!({
            "id": id,
            "images": {
                "fixed_width": { "url": "https://media.giphy.com/x.gif" },
                "original": { "url": "https://media.giphy.com/y.gif", "width": "1", "height": "1" }
            }
        }),
        "err500" => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        _ => {
            return (
                StatusCode::NOT_FOUND,
                axum::Json(json!({ "data": [], "meta": { "status": 404 } })),
            )
                .into_response()
        },
    };
    axum::Json(json!({ "data": data, "meta": { "status": 200 } })).into_response()
}

fn usize_of(n: u64) -> usize {
    usize::try_from(n).expect("fits")
}

async fn fake_media(State(up): State<Upstream>, Path(name): Path<String>) -> Response {
    up.media_hits.fetch_add(1, Ordering::SeqCst);
    let gif = [(header::CONTENT_TYPE, "image/gif")];
    match name.as_str() {
        "ok-preview.gif" => (gif, "GIF89a-preview").into_response(),
        "ok-full.gif" => (gif, "GIF89a-full").into_response(),
        "big-cl" => (gif, vec![0_u8; usize_of(PREVIEW_MAX_BYTES) + 1]).into_response(),
        "exact-cl" => (gif, vec![0_u8; usize_of(PREVIEW_MAX_BYTES)]).into_response(),
        "full-big" => (gif, vec![0_u8; usize_of(FULL_MAX_BYTES) + 1]).into_response(),
        "big-chunked" => {
            // Four chunks, no Content-Length: the stand-in answers chunked.
            let chunk = Bytes::from(vec![0_u8; usize_of(PREVIEW_MAX_BYTES) / 3]);
            let chunks = (0..4).map(move |_| Ok::<_, std::io::Error>(chunk.clone()));
            (gif, Body::from_stream(futures::stream::iter(chunks))).into_response()
        },
        "html" => ([(header::CONTENT_TYPE, "text/html")], "<script></script>").into_response(),
        // Same server, but spelled `localhost`: a real, reachable file that
        // the loopback policy (127.0.0.1 only) must still refuse to follow.
        "redirect-out" => (
            StatusCode::FOUND,
            [(
                header::LOCATION,
                up.base.replace("127.0.0.1", "localhost") + "/media/ok-preview.gif",
            )],
        )
            .into_response(),
        "redirect-in" => (
            StatusCode::FOUND,
            [(
                header::LOCATION,
                format!("{}/media/ok-preview.gif", up.base),
            )],
        )
            .into_response(),
        "slow" => {
            tokio::time::sleep(Duration::from_secs(5)).await;
            (gif, "late").into_response()
        },
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn spawn_upstream() -> Upstream {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let up = Upstream {
        base: format!("http://{}", listener.local_addr().expect("addr")),
        ..Upstream::default()
    };
    let app = Router::new()
        .route("/v1/gifs/trending", get(fake_list))
        .route("/v1/gifs/search", get(fake_list))
        .route("/v1/gifs/{id}", get(fake_lookup))
        .route("/media/{name}", get(fake_media))
        .with_state(up.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    up
}

// ---- The proxy under test --------------------------------------------------

fn secret() -> TokenSecret {
    TokenSecret::new(vec![0x42_u8; 32]).expect("secret")
}

fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("fits")
}

fn token(account: u8) -> String {
    secret()
        .issue(&[account; 16], 1, &[0u8; 32], 0, now_ms() + 3_600_000)
        .expect("token")
}

fn service(
    api_base: String,
    search: RateLimitConfig,
    media: RateLimitConfig,
    timeout: Duration,
) -> GifService {
    let settings = UpstreamSettings {
        api_base,
        media_hosts: MediaHostPolicy::Loopback,
        timeout,
    };
    GifService {
        client: GiphyClient::new(&settings, API_KEY.to_owned()).expect("client"),
        search_limiter: AccountRateLimiter::new(search, 64),
        media_limiter: AccountRateLimiter::new(media, 64),
        cache: RenditionCache::new(Duration::from_secs(600), 64),
    }
}

async fn proxy_with(
    search: RateLimitConfig,
    media: RateLimitConfig,
    timeout: Duration,
) -> (Router, Upstream) {
    let up = spawn_upstream().await;
    let svc = service(format!("{}/v1/gifs", up.base), search, media, timeout);
    let state = AppState {
        session_token_secret: Arc::new(secret()),
        gif: Some(Arc::new(svc)),
    };
    (build_app(state), up)
}

async fn proxy() -> (Router, Upstream) {
    proxy_with(GENEROUS, GENEROUS, Duration::from_secs(5)).await
}

fn unconfigured_proxy() -> Router {
    build_app(AppState {
        session_token_secret: Arc::new(secret()),
        gif: None,
    })
}

async fn call(app: &Router, uri: &str, auth: Option<&str>) -> (StatusCode, HeaderMap, Bytes) {
    let mut req = Request::builder().uri(uri);
    if let Some(auth) = auth {
        req = req.header(header::AUTHORIZATION, auth);
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::empty()).expect("request"))
        .await
        .expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.map_or_else(
        |_| Bytes::from_static(b"<body aborted>"),
        http_body_util::Collected::to_bytes,
    );
    (status, headers, body)
}

async fn get_as(app: &Router, uri: &str, account: u8) -> (StatusCode, HeaderMap, Bytes) {
    call(app, uri, Some(&format!("Bearer {}", token(account)))).await
}

fn error_code(body: &Bytes) -> String {
    let v: Value = serde_json::from_slice(body).expect("json error body");
    v["error"].as_str().expect("error field").to_owned()
}

const GIF_ROUTES: [&str; 3] = [
    "/gifs/trending",
    "/gifs/search?q=cats",
    "/gifs/media/okgif/preview",
];

// ---- Tests -----------------------------------------------------------------

#[tokio::test]
async fn health_ready_and_version_are_public() {
    let app = unconfigured_proxy();
    assert_eq!(call(&app, "/health", None).await.0, StatusCode::OK);
    assert_eq!(call(&app, "/ready", None).await.0, StatusCode::OK);
    let (status, _, body) = call(&app, "/version", None).await;
    assert_eq!(status, StatusCode::OK);
    let v: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(v["service"], "lakko-svc-gif-proxy");
    assert_eq!(call(&app, "/nope", None).await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn gif_routes_refuse_a_missing_or_bad_session() {
    let (app, up) = proxy().await;
    let other_secret = TokenSecret::new(vec![0x07_u8; 32]).expect("secret");
    let forged = other_secret
        .issue(&[1; 16], 1, &[0u8; 32], 0, now_ms() + 3_600_000)
        .expect("token");
    let expired = secret()
        .issue(&[1; 16], 1, &[0u8; 32], 0, now_ms() - 1000)
        .expect("token");
    let bad_headers = [
        None,
        Some("Bearer garbage".to_owned()),
        Some(format!("Basic {}", token(1))),
        Some(format!("Bearer {forged}")),
        Some(format!("Bearer {expired}")),
    ];
    for route in GIF_ROUTES {
        for auth in &bad_headers {
            let (status, _, _) = call(&app, route, auth.as_deref()).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{route} with {auth:?}");
        }
    }
    assert!(up.list_queries.lock().expect("lock").is_empty());
    assert_eq!(up.lookups.load(Ordering::SeqCst), 0);
    assert_eq!(up.media_hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_unconfigured_provider_answers_503_after_auth() {
    let app = unconfigured_proxy();
    for route in GIF_ROUTES {
        let (status, _, body) = get_as(&app, route, 1).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{route}");
        assert_eq!(error_code(&body), "gif-provider-not-configured");
        assert_eq!(
            call(&app, route, None).await.0,
            StatusCode::UNAUTHORIZED,
            "auth still comes first on {route}"
        );
    }
}

#[tokio::test]
async fn search_returns_proxy_urls_only() {
    let (app, up) = proxy().await;
    let (status, headers, body) =
        get_as(&app, "/gifs/search?q=%20%20funny%20cats%20&offset=24", 1).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");

    let v: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(
        v,
        json!({
            "items": [{
                "id": "okgif",
                "preview_url": "/gifs/media/okgif/preview",
                "full_url": "/gifs/media/okgif/full",
                "width": 480,
                "height": 360
            }],
            "next_offset": 26
        })
    );
    let text = String::from_utf8(body.to_vec()).expect("utf8");
    assert!(!text.contains("127.0.0.1"), "{text}");
    assert!(!text.contains("http"), "{text}");
    assert!(!text.contains(API_KEY), "{text}");

    let queries = up.list_queries.lock().expect("lock").clone();
    assert_eq!(queries.len(), 1);
    let q = &queries[0];
    assert_eq!(q["q"], "funny cats", "query is trimmed");
    assert_eq!(q["offset"], "24");
    assert_eq!(q["limit"], "24");
    assert_eq!(q["rating"], "pg-13");
    assert_eq!(q["api_key"], API_KEY);
}

#[tokio::test]
async fn trending_needs_no_query() {
    let (app, up) = proxy().await;
    let (status, _, body) = get_as(&app, "/gifs/trending", 1).await;
    assert_eq!(status, StatusCode::OK);
    let v: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(v["items"][0]["preview_url"], "/gifs/media/okgif/preview");
    assert_eq!(v["next_offset"], 2);
    let queries = up.list_queries.lock().expect("lock").clone();
    assert_eq!(queries[0]["offset"], "0");
    assert!(!queries[0].contains_key("q"));
}

#[tokio::test]
async fn list_input_is_validated_before_any_upstream_call() {
    let (app, up) = proxy().await;
    let long = "a".repeat(101);
    let cases = [
        ("/gifs/search".to_owned(), "invalid-query"),
        ("/gifs/search?q=".to_owned(), "invalid-query"),
        ("/gifs/search?q=%20%20%20".to_owned(), "invalid-query"),
        (format!("/gifs/search?q={long}"), "invalid-query"),
        (
            "/gifs/search?q=cats&offset=5000".to_owned(),
            "invalid-offset",
        ),
        (
            "/gifs/search?q=cats&offset=abc".to_owned(),
            "invalid-offset",
        ),
        ("/gifs/trending?offset=-1".to_owned(), "invalid-offset"),
        ("/gifs/trending?offset=".to_owned(), "invalid-offset"),
    ];
    for (uri, code) in cases {
        let (status, _, body) = get_as(&app, &uri, 1).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(error_code(&body), code, "{uri}");
    }
    assert!(up.list_queries.lock().expect("lock").is_empty());

    let (status, _, _) = get_as(&app, "/gifs/search?q=cats&offset=4999", 1).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn media_input_is_validated_before_any_upstream_call() {
    let (app, up) = proxy().await;
    let long = "a".repeat(65);
    let cases = [
        ("/gifs/media/abc-123/preview".to_owned(), "invalid-gif-id"),
        (
            "/gifs/media/abc%2F..%2Fx/preview".to_owned(),
            "invalid-gif-id",
        ),
        ("/gifs/media/abc%20def/preview".to_owned(), "invalid-gif-id"),
        (format!("/gifs/media/{long}/preview"), "invalid-gif-id"),
        ("/gifs/media/okgif/thumb".to_owned(), "invalid-variant"),
        ("/gifs/media/okgif/FULL".to_owned(), "invalid-variant"),
    ];
    for (uri, code) in cases {
        let (status, _, body) = get_as(&app, &uri, 1).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(error_code(&body), code, "{uri}");
    }
    assert_eq!(up.lookups.load(Ordering::SeqCst), 0);
    assert_eq!(up.media_hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn media_is_streamed_with_the_contracted_headers() {
    let (app, _up) = proxy().await;

    let (status, headers, body) = get_as(&app, "/gifs/media/okgif/preview", 1).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"GIF89a-preview");
    assert_eq!(headers[header::CONTENT_TYPE], "image/gif");
    assert_eq!(headers[header::CACHE_CONTROL], "private, max-age=3600");
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(headers[header::CONTENT_LENGTH], "14");

    let (status, _, body) = get_as(&app, "/gifs/media/okgif/full", 1).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"GIF89a-full");
}

#[tokio::test]
async fn the_rendition_cache_is_per_account() {
    let (app, up) = proxy().await;
    assert_eq!(
        get_as(&app, "/gifs/search?q=cats", 1).await.0,
        StatusCode::OK
    );

    assert_eq!(
        get_as(&app, "/gifs/media/okgif/preview", 1).await.0,
        StatusCode::OK
    );
    assert_eq!(
        up.lookups.load(Ordering::SeqCst),
        0,
        "the searching account reuses the renditions from its own search"
    );

    assert_eq!(
        get_as(&app, "/gifs/media/okgif/preview", 2).await.0,
        StatusCode::OK
    );
    assert_eq!(
        up.lookups.load(Ordering::SeqCst),
        1,
        "another account resolves the id itself"
    );
}

#[tokio::test]
async fn size_caps_are_enforced() {
    let (app, _up) = proxy().await;

    // Declared over the preview cap.
    let (status, _, body) = get_as(&app, "/gifs/media/bigcl/preview", 1).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(error_code(&body), "gif-upstream-failed");

    // No declared length, reads past the cap.
    let (status, _, body) = get_as(&app, "/gifs/media/bigchunk/preview", 1).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(error_code(&body), "gif-upstream-failed");

    // Declared over the full cap.
    let (status, _, _) = get_as(&app, "/gifs/media/fullbig/full", 1).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);

    // Exactly at the cap passes, byte for byte.
    let (status, _, body) = get_as(&app, "/gifs/media/exactcl/preview", 1).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(u64::try_from(body.len()).expect("fits"), PREVIEW_MAX_BYTES);
}

#[tokio::test]
async fn a_disallowed_content_type_is_refused() {
    let (app, _up) = proxy().await;
    let (status, headers, body) = get_as(&app, "/gifs/media/htmlct/preview", 1).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(error_code(&body), "gif-upstream-failed");
    assert_ne!(headers[header::CONTENT_TYPE], "text/html");
}

#[tokio::test]
async fn redirects_are_followed_only_to_allowed_hosts() {
    let (app, up) = proxy().await;
    let (status, _, _) = get_as(&app, "/gifs/media/redirout/preview", 1).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(
        up.media_hits.load(Ordering::SeqCst),
        1,
        "the redirect itself was served, its target never fetched"
    );

    let (status, _, body) = get_as(&app, "/gifs/media/redirin/preview", 1).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&body[..], b"GIF89a-preview");
}

#[tokio::test]
async fn upstream_failures_map_to_502_and_unknown_ids_to_404() {
    let (app, _up) = proxy().await;

    let (status, _, body) = get_as(&app, "/gifs/media/err500/preview", 1).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(error_code(&body), "gif-upstream-failed");

    let (status, _, body) = get_as(&app, "/gifs/media/nosuchgif/preview", 1).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&body), "gif-not-found");

    // Renditions on a host the policy refuses are never fetched.
    let (status, _, _) = get_as(&app, "/gifs/media/offhost/preview", 1).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn an_unreachable_upstream_is_502() {
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = closed.local_addr().expect("addr");
    drop(closed);
    let svc = service(
        format!("http://{addr}/v1/gifs"),
        GENEROUS,
        GENEROUS,
        Duration::from_secs(5),
    );
    let app = build_app(AppState {
        session_token_secret: Arc::new(secret()),
        gif: Some(Arc::new(svc)),
    });
    let (status, _, body) = get_as(&app, "/gifs/trending", 1).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(error_code(&body), "gif-upstream-failed");
}

#[tokio::test]
async fn a_slow_upstream_times_out_as_502() {
    let (app, _up) = proxy_with(GENEROUS, GENEROUS, Duration::from_millis(300)).await;
    let started = std::time::Instant::now();
    let (status, _, _) = get_as(&app, "/gifs/media/slow/preview", 1).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "bounded by the timeout"
    );
}

#[tokio::test]
async fn the_per_account_rate_limit_answers_429_with_retry_after() {
    let tight = RateLimitConfig {
        per_minute: 1,
        burst: 2,
    };
    let (app, up) = proxy_with(tight, GENEROUS, Duration::from_secs(5)).await;

    assert_eq!(get_as(&app, "/gifs/trending", 1).await.0, StatusCode::OK);
    assert_eq!(get_as(&app, "/gifs/search?q=a", 1).await.0, StatusCode::OK);

    let (status, headers, body) = get_as(&app, "/gifs/trending", 1).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(error_code(&body), "rate-limited");
    let retry: u64 = headers[header::RETRY_AFTER]
        .to_str()
        .expect("ascii")
        .parse()
        .expect("seconds");
    assert!((1..=60).contains(&retry), "{retry}");
    assert_eq!(up.list_queries.lock().expect("lock").len(), 2);

    // Another account, and the media budget, are unaffected.
    assert_eq!(get_as(&app, "/gifs/trending", 2).await.0, StatusCode::OK);
    assert_eq!(
        get_as(&app, "/gifs/media/okgif/preview", 1).await.0,
        StatusCode::OK
    );
}
