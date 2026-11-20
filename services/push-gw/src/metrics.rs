// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! push-gw's own metrics ([ADR-0096] section 3).
//!
//! Every label value is a `&'static str` from a closed set in this file or
//! [`ProviderKind::as_str`]. Nothing here names a group, a `push_gid`, a
//! subscription, a token, a nonce, a wake key or a provider endpoint: a
//! wake is counted by what was decided about it, a delivery by the provider
//! kind and what the provider answered.
//!
//! [ADR-0096]: ../../../docs/adr/0096-service-observability.md

use std::time::Duration;

use lakko_metrics::metrics;

use crate::provider::ProviderKind;

/// Sealed wakes the internal listener received past its secret gate,
/// labelled `outcome` ([`WakeOutcome`]): `accepted`, or why it was refused
/// before any subscription row was read. `replayed` is the nonce-duplicate
/// count.
pub(crate) const WAKES_TOTAL: &str = "lakko_push_gw_wakes_total";

/// Provider deliveries attempted for accepted wakes, labelled `provider`
/// ([`ProviderKind::as_str`]: `apns`, `apns_voip`, `fcm`, `webpush`,
/// `unifiedpush`) and `outcome` ([`DeliveryResult`]).
pub(crate) const DELIVERIES_TOTAL: &str = "lakko_push_gw_deliveries_total";

/// Time one provider call took, in seconds, labelled `provider`. Recorded
/// only for a provider that contacts a remote service: a log-only stand-in
/// and an unconfigured kind are not timed.
pub(crate) const PROVIDER_SEND_SECONDS: &str = "lakko_push_gw_provider_send_seconds";

/// Label names push-gw's metrics use beyond `lakko-metrics`' base set, and
/// `context` of the shared anonymous-token spend counter.
#[cfg(test)]
pub(crate) const EXTRA_LABELS: &[&str] = &["provider", "context"];

/// What was decided about one forwarded wake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WakeOutcome {
    /// It opened, verified, was fresh, unseen and within its group's budget.
    Accepted,
    /// Not a sealed wake of the right length, encoding or version (400).
    Malformed,
    /// It did not open under push-gw's key, or its signature did not verify
    /// (403).
    NotOpened,
    /// Its timestamp is outside the freshness window (403).
    Stale,
    /// Its nonce was seen within the window (409).
    Replayed,
    /// The nonce set is full of unexpired nonces, so it was refused rather
    /// than an unexpired nonce forgotten (409).
    NonceSetFull,
    /// Its group's wake budget is spent (429).
    Capped,
}

impl WakeOutcome {
    /// Every value, for the tests.
    #[cfg(test)]
    pub(crate) const ALL: [Self; 7] = [
        Self::Accepted,
        Self::Malformed,
        Self::NotOpened,
        Self::Stale,
        Self::Replayed,
        Self::NonceSetFull,
        Self::Capped,
    ];

    /// The `outcome` label value.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Malformed => "malformed",
            Self::NotOpened => "not_opened",
            Self::Stale => "stale",
            Self::Replayed => "replayed",
            Self::NonceSetFull => "nonce_set_full",
            Self::Capped => "capped",
        }
    }
}

/// What one provider delivery came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeliveryResult {
    /// The provider accepted it.
    Ok,
    /// The provider rejected the token; it is removed from its row.
    TokenInvalid,
    /// The provider call failed (network, status, or credentials).
    Transport,
    /// The kind is served by the log-only stand-in: nothing was sent.
    LogOnly,
    /// No provider is configured for the kind: nothing was sent.
    NotConfigured,
}

impl DeliveryResult {
    /// Every value, for the tests.
    #[cfg(test)]
    pub(crate) const ALL: [Self; 5] = [
        Self::Ok,
        Self::TokenInvalid,
        Self::Transport,
        Self::LogOnly,
        Self::NotConfigured,
    ];

    /// The `outcome` label value.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::TokenInvalid => "token_invalid",
            Self::Transport => "transport",
            Self::LogOnly => "log_only",
            Self::NotConfigured => "not_configured",
        }
    }
}

/// Count one forwarded wake.
pub(crate) fn wake(outcome: WakeOutcome) {
    metrics::counter!(WAKES_TOTAL, "outcome" => outcome.as_str()).increment(1);
}

/// Count one provider delivery.
pub(crate) fn delivery(provider: ProviderKind, result: DeliveryResult) {
    metrics::counter!(
        DELIVERIES_TOTAL,
        "provider" => provider.as_str(),
        "outcome" => result.as_str()
    )
    .increment(1);
}

/// Record how long one remote provider call took.
pub(crate) fn provider_send(provider: ProviderKind, took: Duration) {
    metrics::histogram!(PROVIDER_SEND_SECONDS, "provider" => provider.as_str())
        .record(took.as_secs_f64());
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::provider::tests::ALL_KINDS;

    /// The value of the sample `name` whose labels are exactly `labels`
    /// (in any order) in the exposition `text`, or 0 when there is none.
    pub(crate) fn value_of(text: &str, name: &str, labels: &[(&str, &str)]) -> f64 {
        text.lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .find_map(|line| {
                let sample = lakko_metrics::exposition::samples(line)
                    .into_iter()
                    .next()?
                    .ok()?;
                let same = sample.name == name
                    && sample.labels.len() == labels.len()
                    && labels
                        .iter()
                        .all(|(k, v)| sample.labels.iter().any(|(sk, sv)| sk == k && sv == v));
                if !same {
                    return None;
                }
                line.rsplit(' ').next()?.parse::<f64>().ok()
            })
            .unwrap_or(0.0)
    }

    #[tokio::test]
    async fn every_helper_renders_clean_closed_set_labels() {
        let m = lakko_metrics::install("push-gw", "0.0.0").unwrap();
        for outcome in WakeOutcome::ALL {
            wake(outcome);
        }
        for kind in ALL_KINDS {
            for result in DeliveryResult::ALL {
                delivery(kind, result);
            }
            provider_send(kind, Duration::from_millis(12));
        }
        let text = m.render();
        lakko_metrics::exposition::assert_clean(&text, EXTRA_LABELS, &[]);
        for outcome in WakeOutcome::ALL {
            assert!(
                value_of(&text, WAKES_TOTAL, &[("outcome", outcome.as_str())]) >= 1.0,
                "{}",
                outcome.as_str()
            );
        }
        for kind in ALL_KINDS {
            for result in DeliveryResult::ALL {
                assert!(
                    value_of(
                        &text,
                        DELIVERIES_TOTAL,
                        &[("provider", kind.as_str()), ("outcome", result.as_str())]
                    ) >= 1.0,
                    "{} {}",
                    kind.as_str(),
                    result.as_str()
                );
            }
            assert!(
                value_of(
                    &text,
                    &format!("{PROVIDER_SEND_SECONDS}_count"),
                    &[("provider", kind.as_str())]
                ) >= 1.0,
                "{}",
                kind.as_str()
            );
        }
    }
}
