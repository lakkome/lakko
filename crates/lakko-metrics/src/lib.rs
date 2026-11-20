// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Aggregate service metrics with no identifier in any label ([ADR-0096]).
//!
//! A metrics store is a dataset an operator, a backup or a seized disk can be
//! asked for, like a log file ([ADR-0078] section 11). So every label value a
//! Lakko service records comes from a closed set fixed in the source: the
//! service name, the route **template** (never the URI, whose path segments
//! are journal ids and mailbox addresses), the method from a fixed list, the
//! status class, and named outcomes. This crate is where that is built once:
//!
//! - [`install`] - the process-wide Prometheus recorder, with one bucket set
//!   for every histogram (all are durations in seconds) and the
//!   `lakko_build_info` gauge;
//! - [`red_layer`] - the tower layer every service router mounts: requests
//!   by route template, method and status class, their duration, and the
//!   number in flight;
//! - [`ErrorCode`] - a response extension a handler sets so the RED layer
//!   adds the service's own error code (`anon-token-spent`) as the `code`
//!   label;
//! - [`job_registered`] / [`job_succeeded`] / [`job_failed`] - the
//!   heartbeat of a background job;
//! - [`spawn_pool_gauge`] - the database pool's connections in use and idle;
//! - [`start_from_env`] / [`serve`] - the `/metrics` listener, on its own
//!   localhost port, never on a service's public listener;
//! - [`exposition::check`] - what a service test runs over the rendered
//!   `/metrics`: label names from an allowed set, route values that are
//!   templates, and no identifier-shaped value anywhere
//!   ([`lakko_log_hygiene::scan`]).
//!
//! [ADR-0096]: ../../../docs/adr/0096-service-observability.md
//! [ADR-0078]: ../../../docs/adr/0078-servers-cannot-link-accounts.md

#![forbid(unsafe_code)]

pub mod exposition;
mod job;
mod pool;
mod red;

use std::net::SocketAddr;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use thiserror::Error;
use tracing::{info, warn};

pub use job::{job_failed, job_registered, job_succeeded};
/// The `metrics` facade, so a service records its own counters through this
/// crate (`lakko_metrics::metrics::counter!`) without a dependency of its
/// own on it.
pub use metrics;
pub use pool::spawn_pool_gauge;
pub use red::{red_layer, ErrorCode, RedLayer, RedService};

/// Metric names this crate records. Dashboards and alert rules quote them.
pub mod names {
    /// Requests, labelled `service`, `route`, `method`, `status`, and `code`
    /// when the handler set an [`crate::ErrorCode`].
    pub const HTTP_REQUESTS_TOTAL: &str = "lakko_http_requests_total";
    /// Request duration in seconds, labelled `service`, `route`, `method`.
    pub const HTTP_REQUEST_DURATION_SECONDS: &str = "lakko_http_request_duration_seconds";
    /// Requests whose response has not finished yet, labelled `service`.
    /// Long polls count here for as long as they wait.
    pub const HTTP_REQUESTS_IN_FLIGHT: &str = "lakko_http_requests_in_flight";
    /// One per process: labelled `service` and `version`, value 1.
    pub const BUILD_INFO: &str = "lakko_build_info";
    /// Unix seconds of a job's last successful run, labelled `task` (the job name; not `job`, which Prometheus reserves for the scrape job and would rename to `exported_job`).
    pub const JOB_LAST_SUCCESS_TIMESTAMP_SECONDS: &str = "lakko_job_last_success_timestamp_seconds";
    /// Runs of a job, labelled `task` and `outcome` (`ok`, `error`).
    pub const JOB_RUNS_TOTAL: &str = "lakko_job_runs_total";
    /// Database pool connections, labelled `state` (`in_use`, `idle`).
    pub const DB_POOL_CONNECTIONS: &str = "lakko_db_pool_connections";
}

/// Bucket bounds of every histogram, in seconds. Chosen, not
/// measured: from a fast in-memory answer to the DS and SFU long polls.
pub const DURATION_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
];

/// How often [`serve`] folds histogram samples into their buckets when no
/// scrape has done it (a render does it too). Without this, a recorder whose
/// endpoint nobody scrapes would keep every sample.
const UPKEEP_INTERVAL: Duration = Duration::from_secs(5);

/// Failures installing the recorder or starting the listener.
#[derive(Debug, Error)]
pub enum MetricsError {
    /// The Prometheus recorder could not be built or installed (usually
    /// another recorder is already installed in this process).
    #[error("metrics recorder install failed: {0}")]
    Install(String),
    /// The listener address in the environment does not parse.
    #[error("{var} is not a socket address: {value}")]
    BadBind {
        /// The environment variable.
        var: &'static str,
        /// What it holds.
        value: String,
    },
    /// The listener could not bind or serve.
    #[error("metrics listener: {0}")]
    Io(#[from] std::io::Error),
}

/// The installed recorder. [`Metrics::render`] produces the text the
/// `/metrics` endpoint returns.
#[derive(Clone)]
pub struct Metrics {
    handle: PrometheusHandle,
}

impl Metrics {
    /// The Prometheus text exposition of everything recorded so far.
    #[must_use]
    pub fn render(&self) -> String {
        self.handle.render()
    }
}

static INSTALLED: Mutex<Option<Metrics>> = Mutex::new(None);

/// Install the process-wide recorder and record `lakko_build_info` for
/// `service` at `version`.
///
/// A second call in the same process returns the recorder the first one
/// installed (a test binary drives several tests through one process), and
/// records `lakko_build_info` for its own `service` as well.
pub fn install(service: &'static str, version: &'static str) -> Result<Metrics, MetricsError> {
    let mut slot = INSTALLED.lock().unwrap_or_else(PoisonError::into_inner);
    let metrics = if let Some(existing) = slot.as_ref() {
        existing.clone()
    } else {
        let handle = PrometheusBuilder::new()
            // Every histogram a Lakko service records is a duration in
            // seconds, so one bucket set serves all of them; without it the
            // exporter would render them as summaries, which cannot be
            // aggregated across instances.
            .set_buckets(DURATION_BUCKETS)
            .map_err(|e| MetricsError::Install(e.to_string()))?
            .install_recorder()
            .map_err(|e| MetricsError::Install(e.to_string()))?;
        describe();
        let metrics = Metrics { handle };
        *slot = Some(metrics.clone());
        metrics
    };
    metrics::gauge!(names::BUILD_INFO, "service" => service, "version" => version).set(1.0);
    Ok(metrics)
}

fn describe() {
    metrics::describe_counter!(
        names::HTTP_REQUESTS_TOTAL,
        "Requests by route template, method and status class"
    );
    metrics::describe_histogram!(
        names::HTTP_REQUEST_DURATION_SECONDS,
        metrics::Unit::Seconds,
        "Request duration by route template and method"
    );
    metrics::describe_gauge!(
        names::HTTP_REQUESTS_IN_FLIGHT,
        "Requests whose response has not finished, long polls included"
    );
    metrics::describe_gauge!(names::BUILD_INFO, "Service and version of this process");
    metrics::describe_gauge!(
        names::JOB_LAST_SUCCESS_TIMESTAMP_SECONDS,
        metrics::Unit::Seconds,
        "Unix time of a background job's last successful run"
    );
    metrics::describe_counter!(names::JOB_RUNS_TOTAL, "Background job runs by outcome");
    metrics::describe_gauge!(
        names::DB_POOL_CONNECTIONS,
        "Database pool connections in use and idle"
    );
}

/// Start the `/metrics` listener at the address in the environment variable
/// `var`, after installing the recorder.
///
/// Unset or empty: no recorder is installed, nothing listens, every metric
/// call in the process is a no-op, and `Ok(None)` is returned. Set to an
/// address: the listener is bound before this returns (so a port in use is
/// a startup error) and then served on a spawned task. An address that is
/// not loopback is accepted with a warning: the endpoint has no
/// authentication of its own, nginx with a client certificate is meant to
/// stand in front of it.
pub async fn start_from_env(
    service: &'static str,
    version: &'static str,
    var: &'static str,
) -> Result<Option<Metrics>, MetricsError> {
    let raw = std::env::var(var).unwrap_or_default();
    let raw = raw.trim();
    if raw.is_empty() {
        info!(var, "metrics listener off");
        return Ok(None);
    }
    let bind: SocketAddr = raw.parse().map_err(|_| MetricsError::BadBind {
        var,
        value: raw.to_owned(),
    })?;
    let metrics = install(service, version)?;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let served = metrics.clone();
    tokio::spawn(async move {
        if let Err(e) = serve_on(served, listener).await {
            tracing::error!(error = %e, "metrics listener stopped");
        }
    });
    Ok(Some(metrics))
}

/// Bind `bind` and serve `GET /metrics` until the process ends.
pub async fn serve(metrics: Metrics, bind: SocketAddr) -> Result<(), MetricsError> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    serve_on(metrics, listener).await
}

async fn serve_on(metrics: Metrics, listener: tokio::net::TcpListener) -> Result<(), MetricsError> {
    let bind = listener.local_addr()?;
    if !bind.ip().is_loopback() {
        warn!(%bind, "metrics listener is not on loopback; put nginx with a client certificate in front of it");
    }
    let upkeep = metrics.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(UPKEEP_INTERVAL);
        loop {
            tick.tick().await;
            upkeep.handle.run_upkeep();
        }
    });
    let router = axum::Router::new().route(
        "/metrics",
        axum::routing::get(move || {
            let metrics = metrics.clone();
            async move { metrics.render() }
        }),
    );
    info!(%bind, "metrics listener on");
    axum::serve(listener, router).await?;
    Ok(())
}
