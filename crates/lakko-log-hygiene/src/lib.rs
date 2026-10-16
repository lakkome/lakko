// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Service logs carry no identifiers ([ADR-0078] section 11).
//!
//! No log line at info level or above, in any service, may carry an account
//! id, an identity key, a journal id, a mailbox address, a call, room or
//! channel id, a `push_gid`, a device pseudonym, a push token, a blob id, or
//! any prefix of one. A log file is part of what an operator holds at rest
//! (class (a) of the ADR's threat model), so an identifier in it is the same
//! leak as an identifier in a table.
//!
//! This crate is the one place that rule is written down, in three parts:
//!
//! - [`request_span`] - the span every service's `TraceLayer` builds for a
//!   request: the method and the **route template** (`/ds/group/{group_hex}`),
//!   never the URI, whose path segments are the ids above;
//! - [`lint`] - the source lint (decision 5, half a): a pure scan of Rust
//!   source that names every `tracing` macro at info level or above whose
//!   fields or arguments name a denied identifier. The test
//!   `tests/source_lint.rs` runs it over every service and fails CI on a hit;
//! - [`scan`] and, behind the `capture` feature, `capture` - the runtime
//!   check (decision 5, half b): service integration tests install a capture
//!   subscriber, drive real routes, and fail if the output holds anything
//!   shaped like a 16- or 32-byte identifier (hex, base64, a UUID, or a byte
//!   array), except values on a reviewed allow-list.
//!
//! [ADR-0078]: ../../../docs/adr/0078-servers-cannot-link-accounts.md

#![forbid(unsafe_code)]

pub mod lint;
pub mod scan;

#[cfg(feature = "capture")]
pub mod capture;

use axum::body::Body;
use axum::extract::MatchedPath;
use axum::http::Request;

/// The span a service's `TraceLayer` opens for one request: the method and
/// the matched route template, never the URI.
///
/// Mount it as
/// `TraceLayer::new_for_http().make_span_with(lakko_log_hygiene::request_span)`.
/// A request that matched no route is named `unmatched`. The span is at debug
/// level, like the default one it replaces.
#[must_use]
pub fn request_span(req: &Request<Body>) -> tracing::Span {
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map_or("unmatched", MatchedPath::as_str);
    tracing::debug_span!("request", method = %req.method(), route)
}
