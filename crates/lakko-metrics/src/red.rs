// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The RED layer: requests, errors and duration per route template.
//!
//! Mount it on a service's router next to the request span:
//!
//! ```ignore
//! Router::new()
//!     .route(...)
//!     .layer(lakko_metrics::red_layer("ds"))
//! ```
//!
//! `Router::layer` applies it after routing, so the request carries axum's
//! [`MatchedPath`] (the template, `/ds/group/{group_hex}/messages`); a
//! request that matched no route is `unmatched`. Every label value is from a
//! closed set: the method is one of a fixed list or `other` (a client may
//! send any method token, which must not become a new series), and the
//! status is its class. A handler that answers with one of the service's own
//! error codes (`{"error": "anon-token-spent"}`) may also set an
//! [`ErrorCode`] on the response; the layer then adds it as the `code`
//! label. Codes are `&'static str` constants, so they stay a closed set.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;

use axum::extract::MatchedPath;
use axum::http::{Method, Request, Response, StatusCode};
use tower::{Layer, Service};

use crate::names;

/// The service's own error code of a response, as the `code` label.
///
/// Set it with `response.extensions_mut().insert(ErrorCode("anon-token-spent"))`.
/// The value must be one of the service's fixed codes, never text built from
/// the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorCode(pub &'static str);

/// The layer [`red_layer`] returns.
#[derive(Debug, Clone, Copy)]
pub struct RedLayer {
    service: &'static str,
    no_duration: &'static [&'static str],
}

/// Count every request of `service` by route template, method and status
/// class, time it, and keep the number in flight.
#[must_use]
pub const fn red_layer(service: &'static str) -> RedLayer {
    RedLayer {
        service,
        no_duration: &[],
    }
}

impl RedLayer {
    /// Record no duration for these route templates. For a route whose
    /// duration is its design rather than its cost (a WebSocket upgrade,
    /// whose request ends at 101 while the socket lives on), the histogram
    /// says nothing; its requests are still counted.
    #[must_use]
    pub const fn without_duration_for(self, routes: &'static [&'static str]) -> Self {
        Self {
            service: self.service,
            no_duration: routes,
        }
    }
}

impl<S> Layer<S> for RedLayer {
    type Service = RedService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RedService {
            inner,
            layer: *self,
        }
    }
}

/// The service [`RedLayer`] wraps around a route.
#[derive(Debug, Clone)]
pub struct RedService<S> {
    inner: S,
    layer: RedLayer,
}

impl<S, B, ResBody> Service<Request<B>> for RedService<S>
where
    S: Service<Request<B>, Response = Response<ResBody>>,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let service = self.layer.service;
        let route = req
            .extensions()
            .get::<MatchedPath>()
            .map_or("unmatched", MatchedPath::as_str)
            .to_owned();
        let timed = !self.layer.no_duration.contains(&route.as_str());
        let method = method_label(req.method());
        let in_flight = InFlight::enter(service);
        let start = Instant::now();
        let fut = self.inner.call(req);
        Box::pin(async move {
            let res = fut.await;
            let (status, code) = match &res {
                Ok(r) => (
                    status_label(r.status()),
                    r.extensions().get::<ErrorCode>().map(|c| c.0),
                ),
                Err(_) => ("error", None),
            };
            if timed {
                metrics::histogram!(
                    names::HTTP_REQUEST_DURATION_SECONDS,
                    "service" => service,
                    "route" => route.clone(),
                    "method" => method,
                )
                .record(start.elapsed().as_secs_f64());
            }
            let mut labels = vec![
                metrics::Label::new("service", service),
                metrics::Label::new("route", route),
                metrics::Label::new("method", method),
                metrics::Label::new("status", status),
            ];
            if let Some(code) = code {
                labels.push(metrics::Label::new("code", code));
            }
            metrics::counter!(names::HTTP_REQUESTS_TOTAL, labels).increment(1);
            drop(in_flight);
            res
        })
    }
}

/// Holds one request in the in-flight gauge until it is dropped, so a
/// request whose future is dropped half-way (the client went away) leaves
/// the gauge too.
struct InFlight {
    service: &'static str,
}

impl InFlight {
    fn enter(service: &'static str) -> Self {
        metrics::gauge!(names::HTTP_REQUESTS_IN_FLIGHT, "service" => service).increment(1.0);
        Self { service }
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        metrics::gauge!(names::HTTP_REQUESTS_IN_FLIGHT, "service" => self.service).decrement(1.0);
    }
}

/// The method as a label: one of the standard methods, else `other`.
pub(crate) fn method_label(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::DELETE => "DELETE",
        Method::PATCH => "PATCH",
        Method::HEAD => "HEAD",
        Method::OPTIONS => "OPTIONS",
        _ => "other",
    }
}

/// The status as a label: its class.
pub(crate) const fn status_label(status: StatusCode) -> &'static str {
    match status.as_u16() {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        _ => "5xx",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_methods_share_one_label() {
        assert_eq!(method_label(&Method::GET), "GET");
        assert_eq!(method_label(&Method::OPTIONS), "OPTIONS");
        let odd = Method::from_bytes(b"A7F3C9E2B1D4").unwrap();
        assert_eq!(method_label(&odd), "other");
        assert_eq!(method_label(&Method::CONNECT), "other");
    }

    #[test]
    fn status_is_its_class() {
        assert_eq!(status_label(StatusCode::SWITCHING_PROTOCOLS), "1xx");
        assert_eq!(status_label(StatusCode::NO_CONTENT), "2xx");
        assert_eq!(status_label(StatusCode::NOT_MODIFIED), "3xx");
        assert_eq!(status_label(StatusCode::TOO_MANY_REQUESTS), "4xx");
        assert_eq!(status_label(StatusCode::SERVICE_UNAVAILABLE), "5xx");
    }
}
