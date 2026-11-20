// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The gif-proxy's own counters ([ADR-0096] section 3).
//!
//! Every label value is a `&'static str` from an enum in this file. No label
//! carries a GIPHY URL, a query, a GIF id or an account: the rendition cache
//! is keyed per account, and only whether a lookup hit or missed is counted.
//!
//! [ADR-0096]: ../../../docs/adr/0096-service-observability.md

use std::future::Future;
use std::time::Instant;

use crate::giphy::UpstreamError;

/// Calls to GIPHY, by `kind` (`trending`, `search`, `lookup`, `media`) and
/// `outcome` (`ok`, `timeout`, `transport`, `status`, `not_found`,
/// `too_large`, `decode`, `no_rendition`, `disallowed_host`,
/// `disallowed_content_type`).
pub(crate) const UPSTREAM_CALLS_TOTAL: &str = "lakko_gif_proxy_upstream_calls_total";
/// Duration of a call to GIPHY in seconds, by `kind`. For `media` this is the
/// time to the response headers (and to the whole body when GIPHY declares
/// no length); a body streamed afterwards is not timed.
pub(crate) const UPSTREAM_DURATION_SECONDS: &str = "lakko_gif_proxy_upstream_duration_seconds";
/// Rendition-cache lookups of the media route, by `outcome` (`hit`, `miss`).
/// The hit ratio is `hit / (hit + miss)`, computed by the dashboard.
pub(crate) const RENDITION_CACHE_LOOKUPS_TOTAL: &str =
    "lakko_gif_proxy_rendition_cache_lookups_total";

/// Which GIPHY call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpstreamCall {
    /// The trending page.
    Trending,
    /// A search page.
    Search,
    /// Resolving one GIF id to its renditions.
    Lookup,
    /// Fetching one rendition's bytes.
    Media,
}

impl UpstreamCall {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Trending => "trending",
            Self::Search => "search",
            Self::Lookup => "lookup",
            Self::Media => "media",
        }
    }
}

/// The outcome label of an upstream result. A non-2xx status is one value,
/// whatever the status.
const fn outcome_of<T>(result: &Result<T, UpstreamError>) -> &'static str {
    match result {
        Ok(_) => "ok",
        Err(UpstreamError::Timeout) => "timeout",
        Err(UpstreamError::Transport) => "transport",
        Err(UpstreamError::Status(_)) => "status",
        Err(UpstreamError::NotFound) => "not_found",
        Err(UpstreamError::TooLarge) => "too_large",
        Err(UpstreamError::Decode) => "decode",
        Err(UpstreamError::NoRendition) => "no_rendition",
        Err(UpstreamError::DisallowedHost) => "disallowed_host",
        Err(UpstreamError::DisallowedContentType) => "disallowed_content_type",
    }
}

/// Run one GIPHY call, then count it by outcome and record its duration.
pub(crate) async fn upstream<T>(
    kind: UpstreamCall,
    request: impl Future<Output = Result<T, UpstreamError>>,
) -> Result<T, UpstreamError> {
    let started = Instant::now();
    let result = request.await;
    record_upstream(kind, &result, started.elapsed().as_secs_f64());
    result
}

fn record_upstream<T>(kind: UpstreamCall, result: &Result<T, UpstreamError>, seconds: f64) {
    lakko_metrics::metrics::counter!(
        UPSTREAM_CALLS_TOTAL,
        "kind" => kind.as_str(),
        "outcome" => outcome_of(result),
    )
    .increment(1);
    lakko_metrics::metrics::histogram!(UPSTREAM_DURATION_SECONDS, "kind" => kind.as_str())
        .record(seconds);
}

/// Count a rendition-cache lookup.
pub(crate) fn rendition_cache(hit: bool) {
    let outcome = if hit { "hit" } else { "miss" };
    lakko_metrics::metrics::counter!(RENDITION_CACHE_LOOKUPS_TOTAL, "outcome" => outcome)
        .increment(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    const CALLS: [UpstreamCall; 4] = [
        UpstreamCall::Trending,
        UpstreamCall::Search,
        UpstreamCall::Lookup,
        UpstreamCall::Media,
    ];

    fn errors() -> [UpstreamError; 9] {
        [
            UpstreamError::Timeout,
            UpstreamError::Transport,
            UpstreamError::Status(500),
            UpstreamError::NotFound,
            UpstreamError::TooLarge,
            UpstreamError::Decode,
            UpstreamError::NoRendition,
            UpstreamError::DisallowedHost,
            UpstreamError::DisallowedContentType,
        ]
    }

    #[tokio::test]
    async fn every_series_renders_clean() {
        let metrics = lakko_metrics::install("gif-proxy", "0.0.0").expect("install");
        for call in CALLS {
            let ok: Result<(), UpstreamError> = upstream(call, async { Ok(()) }).await;
            assert!(ok.is_ok());
            for error in errors() {
                let failed: Result<(), UpstreamError> =
                    upstream(call, async move { Err(error) }).await;
                assert!(failed.is_err());
            }
        }
        rendition_cache(true);
        rendition_cache(false);

        let text = metrics.render();
        lakko_metrics::exposition::assert_clean(&text, &["kind"], &[]);
        for call in CALLS {
            let kind = call.as_str();
            let mut outcomes = vec!["ok"];
            outcomes.extend(errors().iter().map(|e| outcome_of::<()>(&Err(*e))));
            for outcome in outcomes {
                let line =
                    format!("{UPSTREAM_CALLS_TOTAL}{{kind=\"{kind}\",outcome=\"{outcome}\"}}");
                assert!(text.contains(&line), "{line}\n{text}");
            }
            assert!(
                text.contains(&format!(
                    "{UPSTREAM_DURATION_SECONDS}_count{{kind=\"{kind}\"}}"
                )),
                "{text}"
            );
        }
        for outcome in ["hit", "miss"] {
            let line = format!("{RENDITION_CACHE_LOOKUPS_TOTAL}{{outcome=\"{outcome}\"}}");
            assert!(text.contains(&line), "{text}");
        }
    }
}
