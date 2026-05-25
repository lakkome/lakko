// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Prometheus metrics exporter for the SFU.
//!
//! The `metrics` crate is a recorder-agnostic facade; we wire
//! the Prometheus recorder behind it and let any module
//! sprinkle `metrics::counter!` / `metrics::gauge!` calls
//! against the canonical names listed in [`names`] below.
//!
//! The exporter binds on a separate port (default 9641) so
//! nginx applies mTLS exclusively to the metrics endpoint
//! without affecting the WebSocket signaling path. See
//! [`infra/nginx/sites-available/sfu.conf`].
//!
//! Wire-shape stability is a contract — every alert rule in
//! `infra/prometheus/rules/calls.yml` references one of the
//! names below. Renaming a metric is a wire-format break.

use std::net::SocketAddr;

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use thiserror::Error;
use tracing::info;

/// Canonical metric names. The Prometheus alert rules in
/// `infra/prometheus/rules/calls.yml` quote these — keep them
/// in lock-step on any rename.
///
/// All names follow the `lakko_sfu_*` namespace so a
/// federation scrape can dedupe with other Lakko services
/// without prefix-clash.
pub mod names {
    /// Total number of peers currently registered.
    pub const PEERS_ACTIVE: &str = "lakko_sfu_peers_active";
    /// Cumulative count of accepted WS upgrades.
    pub const PEERS_ADMITTED_TOTAL: &str = "lakko_sfu_peers_admitted_total";
    /// Cumulative count of rejected WS upgrades — JWT
    /// verify failed, kid unknown, expired, etc.
    pub const PEERS_REJECTED_TOTAL: &str = "lakko_sfu_peers_rejected_total";
    /// Cumulative count of distinct calls observed since
    /// boot.
    pub const CALLS_TOTAL: &str = "lakko_sfu_calls_total";
    /// Cumulative count of signaling frames received from
    /// peers. Labelled `kind` once we have a typed signaling
    /// protocol; for now a single bucket.
    pub const SIGNALING_FRAMES_TOTAL: &str = "lakko_sfu_signaling_frames_total";
    /// JWKS cache age, seconds since the last successful
    /// refresh. The `LakkoSfuJwksStale` alert (future) will
    /// fire when this exceeds the configured refresh cadence
    /// by a wide margin.
    pub const JWKS_CACHE_AGE_SECS: &str = "lakko_sfu_jwks_cache_age_seconds";
}

/// Failures while installing the recorder. The exporter
/// install is `std::sync::Once`-y — only the first call
/// succeeds, which is exactly what we want in production.
#[derive(Debug, Error)]
pub enum MetricsError {
    /// The Prometheus recorder failed to install — usually
    /// because someone else already installed one.
    #[error("metrics recorder install failed: {0}")]
    Install(String),
}

/// Installed Prometheus exporter handle. `render` produces
/// the text-format exposition the `/metrics` HTTP handler
/// returns.
#[derive(Clone)]
pub struct MetricsExporter {
    handle: PrometheusHandle,
}

impl MetricsExporter {
    /// Install the global recorder and stand up the
    /// scraping handle. Must be called exactly once per
    /// process at startup.
    ///
    /// # Errors
    ///
    /// Returns [`MetricsError::Install`] if a recorder is
    /// already installed (e.g. when the integration tests
    /// reuse the global state across iterations — those
    /// tests opt out by using `install_test_recorder` or
    /// run inside their own process).
    pub fn install() -> Result<Self, MetricsError> {
        let handle = PrometheusBuilder::new()
            .install_recorder()
            .map_err(|e| MetricsError::Install(e.to_string()))?;
        info!("metrics: prometheus exporter installed");
        // Touch every counter so the first scrape sees a
        // zero value rather than an empty exposition. This
        // matters for the alert-rule pattern
        // `increase(metric[5m]) == 0` — without an initial
        // sample, the rate evaluator yields NaN and the
        // alert stays NoData.
        metrics::describe_counter!(
            names::PEERS_ADMITTED_TOTAL,
            "Cumulative count of accepted WS upgrades"
        );
        metrics::describe_counter!(
            names::PEERS_REJECTED_TOTAL,
            "Cumulative count of rejected WS upgrades"
        );
        metrics::describe_counter!(
            names::CALLS_TOTAL,
            "Cumulative count of distinct calls observed since boot"
        );
        metrics::describe_counter!(
            names::SIGNALING_FRAMES_TOTAL,
            "Cumulative count of signaling frames received from peers"
        );
        metrics::describe_gauge!(
            names::PEERS_ACTIVE,
            "Number of peers currently registered"
        );
        metrics::describe_gauge!(
            names::JWKS_CACHE_AGE_SECS,
            "JWKS cache age in seconds since last successful refresh"
        );
        // Initialise each counter / gauge so it ships a zero
        // sample at the first scrape.
        metrics::counter!(names::PEERS_ADMITTED_TOTAL).absolute(0);
        metrics::counter!(names::PEERS_REJECTED_TOTAL).absolute(0);
        metrics::counter!(names::CALLS_TOTAL).absolute(0);
        metrics::counter!(names::SIGNALING_FRAMES_TOTAL).absolute(0);
        metrics::gauge!(names::PEERS_ACTIVE).set(0.0);
        metrics::gauge!(names::JWKS_CACHE_AGE_SECS).set(0.0);
        Ok(Self { handle })
    }

    /// Render the Prometheus text-format exposition.
    pub fn render(&self) -> String {
        self.handle.render()
    }
}

/// Run a tiny axum HTTP server that serves `GET /metrics`.
/// Exposed on a dedicated socket so nginx can mTLS-gate the
/// metrics endpoint without touching the signaling path.
pub async fn serve(exporter: MetricsExporter, bind: SocketAddr) -> anyhow::Result<()> {
    use axum::{routing::get, Router};

    let router = Router::new().route(
        "/metrics",
        get({
            let exporter = exporter.clone();
            move || async move { exporter.render() }
        }),
    );

    let listener = tokio::net::TcpListener::bind(bind).await?;
    info!(%bind, "metrics: exporter listening");
    axum::serve(listener, router).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique() {
        let all = [
            names::PEERS_ACTIVE,
            names::PEERS_ADMITTED_TOTAL,
            names::PEERS_REJECTED_TOTAL,
            names::CALLS_TOTAL,
            names::SIGNALING_FRAMES_TOTAL,
            names::JWKS_CACHE_AGE_SECS,
        ];
        let mut s: Vec<&str> = all.to_vec();
        s.sort_unstable();
        s.dedup();
        assert_eq!(s.len(), all.len());
    }

    #[test]
    fn names_use_lakko_sfu_prefix() {
        let all = [
            names::PEERS_ACTIVE,
            names::PEERS_ADMITTED_TOTAL,
            names::PEERS_REJECTED_TOTAL,
            names::CALLS_TOTAL,
            names::SIGNALING_FRAMES_TOTAL,
            names::JWKS_CACHE_AGE_SECS,
        ];
        for n in all {
            assert!(
                n.starts_with("lakko_sfu_"),
                "metric {n} must start with lakko_sfu_"
            );
        }
    }
}
