// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Request log: method, route template, status, latency. Nothing else.
//!
//! `tower_http::trace::TraceLayer` is deliberately not used: its default span
//! records the full URI, which here carries the search query (`?q=`) and the
//! GIF id. The route is taken from axum's [`MatchedPath`], which is the
//! template (`/gifs/media/{id}/{variant}`), never the concrete path.

use std::time::Instant;

use axum::extract::{MatchedPath, Request};
use axum::middleware::Next;
use axum::response::Response;

/// The label logged for a request: its route template, or `unmatched`.
pub(crate) fn route_label(request: &Request) -> String {
    request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_owned(), |m| m.as_str().to_owned())
}

pub(crate) async fn log_request(request: Request, next: Next) -> Response {
    let route = route_label(&request);
    let method = request.method().clone();
    let started = Instant::now();
    let response = next.run(request).await;
    tracing::info!(
        method = %method,
        route = %route,
        status = response.status().as_u16(),
        latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "request"
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use axum::body::Body;
    use axum::middleware;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt as _;

    #[tokio::test]
    async fn the_label_is_the_template_never_the_path_or_query() {
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let recorder = seen.clone();
        let app = Router::new()
            .route("/gifs/media/{id}/{variant}", get(|| async { "ok" }))
            .route("/gifs/search", get(|| async { "ok" }))
            .layer(middleware::from_fn(move |req: Request, next: Next| {
                let recorder = recorder.clone();
                async move {
                    recorder.lock().expect("lock").push(route_label(&req));
                    next.run(req).await
                }
            }));

        for uri in [
            "/gifs/media/secretid123/preview",
            "/gifs/search?q=very+private+words&offset=24",
            "/nowhere?q=also+private",
        ] {
            app.clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .body(Body::empty())
                        .expect("req"),
                )
                .await
                .expect("response");
        }

        let seen = seen.lock().expect("lock").clone();
        assert_eq!(
            seen,
            vec![
                "/gifs/media/{id}/{variant}".to_owned(),
                "/gifs/search".to_owned(),
                "unmatched".to_owned(),
            ]
        );
    }
}
