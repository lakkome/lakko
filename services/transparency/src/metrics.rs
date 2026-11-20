// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The transparency service's own counters and gauges ([ADR-0096]
//! section 3).
//!
//! Every label value below is a `&'static str` from an enum in this file;
//! none names an account key, a public key, a leaf index or a witness URL.
//! The request counters by route and status come from the shared
//! `lakko_metrics::red_layer`. This service's refusals are plain-text bodies,
//! not `{"error": <code>}`, so no `code` label is set; the append outcomes
//! below say why an append was refused.
//!
//! Roots are signed on demand (every `GET /transparency/root` signs the
//! current head with the current time), so "the age of the newest root" is
//! read here as the age of the newest **co-signed** root: the time and size
//! of the last witness bundle, beside the head size. A head that grew while
//! the co-signed size stayed behind it is a witness round that is not
//! happening or not succeeding.
//!
//! Witnesses are counted, never named: the number configured and the number
//! that co-signed the last round, not one series per witness.
//!
//! [ADR-0096]: ../../../docs/adr/0096-service-observability.md

/// `POST /transparency/append` answers, by `outcome`: `appended` (a new
/// leaf), `unchanged` (the key was already the latest, no leaf),
/// `invalid` (a malformed body), `leaf_auth_missing` (no leaf authorisation
/// under the presence policy), `refused` (a leaf authorisation or genesis
/// attestation did not verify), `error` (the store failed).
pub(crate) const APPENDS_TOTAL: &str = "lakko_transparency_appends_total";
/// Entries in the log (the head size). Set at boot and on each new leaf.
pub(crate) const LOG_SIZE: &str = "lakko_transparency_log_size";
/// The log size the newest witness co-signature bundle binds. Set at boot
/// from the stored bundle and on each round that collected one.
pub(crate) const COSIGNED_LOG_SIZE: &str = "lakko_transparency_cosigned_log_size";
/// Unix seconds when the newest witness co-signature bundle was collected.
/// Not set until the first round of this process (the stored bundle carries
/// no time).
pub(crate) const COSIGNED_TIMESTAMP_SECONDS: &str = "lakko_transparency_cosigned_timestamp_seconds";
/// Witnesses configured (`LAKKO_TRANSPARENCY_WITNESS_URLS`). Set at boot.
pub(crate) const WITNESSES_CONFIGURED: &str = "lakko_transparency_witnesses_configured";
/// Witnesses that co-signed the last fan-out round (0 when every one failed).
/// Compare with the clients' threshold K: below it, clients fail closed.
pub(crate) const WITNESSES_ANSWERED: &str = "lakko_transparency_witnesses_answered";
/// Witness co-signature requests, one per witness per fan-out round, by
/// `outcome`: `ok`, `not_consistent` (the witness refused the root as not
/// extending what it co-signed: HTTP 409, a consistency refusal),
/// `refused` (any other non-2xx), `unreachable` (connect, timeout or
/// transport error), `bad_response` (an undecodable answer or a
/// co-signature of the wrong length).
pub(crate) const WITNESS_COSIGNS_TOTAL: &str = "lakko_transparency_witness_cosigns_total";

/// How an append ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppendOutcome {
    /// A new leaf.
    Appended,
    /// The key was already the latest; no leaf.
    Unchanged,
    /// A malformed body.
    Invalid,
    /// No leaf authorisation under the presence policy.
    LeafAuthMissing,
    /// A leaf authorisation or genesis attestation did not verify.
    Refused,
    /// The store failed.
    Error,
}

impl AppendOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Appended => "appended",
            Self::Unchanged => "unchanged",
            Self::Invalid => "invalid",
            Self::LeafAuthMissing => "leaf_auth_missing",
            Self::Refused => "refused",
            Self::Error => "error",
        }
    }
}

/// How one witness co-signature request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CosignOutcome {
    /// A co-signature of the right length.
    Ok,
    /// HTTP 409: the witness refused the root as not extending its log.
    NotConsistent,
    /// Any other non-2xx status.
    Refused,
    /// Connect, timeout or transport error.
    Unreachable,
    /// An undecodable answer, or a co-signature of the wrong length.
    BadResponse,
}

impl CosignOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::NotConsistent => "not_consistent",
            Self::Refused => "refused",
            Self::Unreachable => "unreachable",
            Self::BadResponse => "bad_response",
        }
    }
}

/// Count an append answer.
pub(crate) fn append(outcome: AppendOutcome) {
    lakko_metrics::metrics::counter!(APPENDS_TOTAL, "outcome" => outcome.as_str()).increment(1);
}

/// Set the head size.
pub(crate) fn log_size(size: u64) {
    lakko_metrics::metrics::gauge!(LOG_SIZE).set(gauge_value(size));
}

/// Record a collected co-signature bundle binding `log_size`; `fresh` when
/// it was collected just now (not restored from the database).
pub(crate) fn cosigned(log_size: u64, fresh: bool) {
    lakko_metrics::metrics::gauge!(COSIGNED_LOG_SIZE).set(gauge_value(log_size));
    if fresh {
        lakko_metrics::metrics::gauge!(COSIGNED_TIMESTAMP_SECONDS).set(unix_now());
    }
}

/// Set the number of configured witnesses.
pub(crate) fn witnesses_configured(n: usize) {
    lakko_metrics::metrics::gauge!(WITNESSES_CONFIGURED).set(gauge_value_usize(n));
}

/// Record a fan-out round in which `answered` witnesses co-signed.
pub(crate) fn witnesses_answered(answered: usize) {
    lakko_metrics::metrics::gauge!(WITNESSES_ANSWERED).set(gauge_value_usize(answered));
}

/// Count one witness co-signature request.
pub(crate) fn witness_cosign(outcome: CosignOutcome) {
    lakko_metrics::metrics::counter!(WITNESS_COSIGNS_TOTAL, "outcome" => outcome.as_str())
        .increment(1);
}

/// A count as a gauge value, saturating at `u32::MAX`.
fn gauge_value(n: u64) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

fn gauge_value_usize(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    const APPENDS: [AppendOutcome; 6] = [
        AppendOutcome::Appended,
        AppendOutcome::Unchanged,
        AppendOutcome::Invalid,
        AppendOutcome::LeafAuthMissing,
        AppendOutcome::Refused,
        AppendOutcome::Error,
    ];
    const COSIGNS: [CosignOutcome; 5] = [
        CosignOutcome::Ok,
        CosignOutcome::NotConsistent,
        CosignOutcome::Refused,
        CosignOutcome::Unreachable,
        CosignOutcome::BadResponse,
    ];

    #[tokio::test]
    async fn every_series_renders_clean() {
        let metrics = lakko_metrics::install("transparency", "0.0.0").unwrap();
        for outcome in APPENDS {
            append(outcome);
        }
        for outcome in COSIGNS {
            witness_cosign(outcome);
        }
        log_size(7);
        cosigned(5, true);
        witnesses_configured(3);
        witnesses_answered(2);

        let text = metrics.render();
        lakko_metrics::exposition::assert_clean(&text, &[], &[]);
        for outcome in APPENDS {
            let line = format!(r#"{APPENDS_TOTAL}{{outcome="{}"}}"#, outcome.as_str());
            assert!(text.contains(&line), "missing {line}:\n{text}");
        }
        for outcome in COSIGNS {
            let line = format!(
                r#"{WITNESS_COSIGNS_TOTAL}{{outcome="{}"}}"#,
                outcome.as_str()
            );
            assert!(text.contains(&line), "missing {line}:\n{text}");
        }
        for line in [
            format!("{LOG_SIZE} 7"),
            format!("{COSIGNED_LOG_SIZE} 5"),
            format!("{WITNESSES_CONFIGURED} 3"),
            format!("{WITNESSES_ANSWERED} 2"),
            format!("{COSIGNED_TIMESTAMP_SECONDS} "),
        ] {
            assert!(text.contains(&line), "missing {line}:\n{text}");
        }
    }
}
