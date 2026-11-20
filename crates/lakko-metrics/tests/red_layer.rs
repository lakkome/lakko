// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! ADR-0096 section 1: drive a router whose paths carry identifiers through
//! the RED layer, and check that the rendered `/metrics` names route
//! templates only, keeps unknown methods in one series, and holds nothing
//! identifier-shaped.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use lakko_metrics::exposition::{assert_clean, samples};
use tower::ServiceExt;

const HEX_ID: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
const B64_ID: &str = "n4bQgYhMfWWaL-qgxVrQFaO_TxsrC4Is0V1sFbDwCgg";
const UUID: &str = "0191f3a4-7b2c-7d10-9a3e-5c4b2f1e0d9a";

fn app() -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route(
            "/ds/group/{group_hex}/messages",
            get(|| async { StatusCode::UNAUTHORIZED }).post(|| async { StatusCode::CREATED }),
        )
        .route(
            "/v1/connect",
            get(|| async { StatusCode::SWITCHING_PROTOCOLS }),
        )
        .route(
            "/limited",
            get(|| async {
                let mut res = StatusCode::TOO_MANY_REQUESTS.into_response();
                res.extensions_mut()
                    .insert(lakko_metrics::ErrorCode("too-many-attempts"));
                res
            }),
        )
        .layer(lakko_metrics::red_layer("test").without_duration_for(&["/v1/connect"]))
}

async fn send(app: &Router, method: Method, uri: &str) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn identifiers_in_paths_never_reach_the_exposition() {
    let metrics = lakko_metrics::install("test", "0.0.0").unwrap();
    let app = app();

    assert_eq!(send(&app, Method::GET, "/health").await, StatusCode::OK);
    for id in [HEX_ID, B64_ID, UUID] {
        let uri = format!("/ds/group/{id}/messages");
        assert_eq!(
            send(&app, Method::GET, &uri).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(send(&app, Method::POST, &uri).await, StatusCode::CREATED);
        // A path no route matches, and a method token a client made up.
        assert_eq!(
            send(&app, Method::GET, &format!("/nothing/{id}")).await,
            StatusCode::NOT_FOUND
        );
        let odd = Method::from_bytes(&id.as_bytes()[..12]).unwrap();
        send(&app, odd, &uri).await;
    }
    send(&app, Method::GET, "/v1/connect").await;
    assert_eq!(
        send(&app, Method::GET, "/limited").await,
        StatusCode::TOO_MANY_REQUESTS
    );

    let text = metrics.render();
    assert_clean(&text, &[], &[]);

    let samples: Vec<_> = samples(&text).into_iter().map(Result::unwrap).collect();
    let has = |name: &str, want: &[(&str, &str)]| {
        samples.iter().any(|s| {
            s.name == name
                && want
                    .iter()
                    .all(|(k, v)| s.labels.iter().any(|(lk, lv)| lk == k && lv == v))
        })
    };
    assert!(has(
        "lakko_http_requests_total",
        &[
            ("route", "/ds/group/{group_hex}/messages"),
            ("method", "POST"),
            ("status", "2xx")
        ]
    ));
    assert!(has(
        "lakko_http_requests_total",
        &[("route", "unmatched"), ("status", "4xx")]
    ));
    assert!(has("lakko_http_requests_total", &[("method", "other")]));
    assert!(has(
        "lakko_http_request_duration_seconds_bucket",
        &[("route", "/health"), ("le", "0.005")]
    ));
    assert!(has(
        "lakko_http_requests_total",
        &[("route", "/v1/connect"), ("status", "1xx")]
    ));
    assert!(!has(
        "lakko_http_request_duration_seconds_count",
        &[("route", "/v1/connect")]
    ));
    assert!(has(
        "lakko_http_requests_total",
        &[
            ("route", "/limited"),
            ("status", "4xx"),
            ("code", "too-many-attempts")
        ]
    ));
    assert!(has(
        "lakko_build_info",
        &[("service", "test"), ("version", "0.0.0")]
    ));

    // Nothing is in flight once every response has finished.
    assert!(
        text.lines()
            .any(|l| l == "lakko_http_requests_in_flight{service=\"test\"} 0"),
        "{text}"
    );
}

#[tokio::test]
async fn heartbeats_render_with_their_job_name() {
    let metrics = lakko_metrics::install("test", "0.0.0").unwrap();
    lakko_metrics::job_succeeded("journal_pruner");
    lakko_metrics::job_failed("journal_pruner");
    let text = metrics.render();
    assert_clean(&text, &[], &[]);
    assert!(text.contains("lakko_job_last_success_timestamp_seconds{task=\"journal_pruner\"}"));
    assert!(text.contains("lakko_job_runs_total{task=\"journal_pruner\",outcome=\"error\"} 1"));
}

#[tokio::test]
async fn pool_gauge_and_registration_render() {
    let metrics = lakko_metrics::install("test", "0.0.0").unwrap();
    lakko_metrics::job_registered("never_runs");
    let gauge = lakko_metrics::spawn_pool_gauge(|| (5, 2));
    // The first sample is taken at once.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    gauge.abort();
    let text = metrics.render();
    assert_clean(&text, &[], &[]);
    assert!(
        text.contains("lakko_db_pool_connections{state=\"in_use\"} 3"),
        "{text}"
    );
    assert!(
        text.contains("lakko_db_pool_connections{state=\"idle\"} 2"),
        "{text}"
    );
    assert!(text.contains("lakko_job_last_success_timestamp_seconds{task=\"never_runs\"}"));
}
