// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The witness service's own counters and gauges ([ADR-0096] section 3).
//!
//! Every label value below is a `&'static str` from an enum in this file;
//! none names a root, a key or an operator. The request counters by route
//! and status come from the shared `lakko_metrics::red_layer`. This
//! service's refusals are plain-text bodies, not `{"error": <code>}`, so no
//! `code` label is set; the co-sign outcomes below say why one was refused.
//!
//! [ADR-0096]: ../../../docs/adr/0096-service-observability.md

use crate::store::CosignError;

/// `POST /witness/cosign` answers, by `outcome`: `ok` (co-signed),
/// `not_consistent` (the root does not extend the last co-signed one: a
/// fork, the consistency refusal), `rollback` (a size below the
/// high-water), `bad_operator_signature`, `invalid` (a malformed body),
/// `error` (the store failed).
pub(crate) const COSIGNS_TOTAL: &str = "lakko_witness_cosigns_total";
/// The log size this witness last co-signed (its high-water). Set at boot
/// and on each co-signature that grew it.
pub(crate) const LOG_SIZE: &str = "lakko_witness_log_size";
/// Unix seconds of this witness's last co-signature. Not set until the
/// first co-signature of this process (the stored high-water carries no
/// time).
pub(crate) const COSIGNED_TIMESTAMP_SECONDS: &str = "lakko_witness_cosigned_timestamp_seconds";

/// How a co-sign request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CosignOutcome {
    /// Co-signed.
    Ok,
    /// The root does not append-only-extend the last co-signed one.
    NotConsistent,
    /// A size below the high-water.
    Rollback,
    /// The root is not signed by the pinned operator key.
    BadOperatorSignature,
    /// A malformed body.
    Invalid,
    /// The store failed.
    Error,
}

impl CosignOutcome {
    /// The outcome of a refusal decided by the store.
    pub(crate) const fn of(error: &CosignError) -> Self {
        match error {
            CosignError::BadOperatorSignature => Self::BadOperatorSignature,
            CosignError::NotConsistent => Self::NotConsistent,
            CosignError::Rollback { .. } => Self::Rollback,
            CosignError::Db(_) => Self::Error,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::NotConsistent => "not_consistent",
            Self::Rollback => "rollback",
            Self::BadOperatorSignature => "bad_operator_signature",
            Self::Invalid => "invalid",
            Self::Error => "error",
        }
    }
}

/// Count a co-sign answer.
pub(crate) fn cosign(outcome: CosignOutcome) {
    lakko_metrics::metrics::counter!(COSIGNS_TOTAL, "outcome" => outcome.as_str()).increment(1);
    if outcome == CosignOutcome::Ok {
        lakko_metrics::metrics::gauge!(COSIGNED_TIMESTAMP_SECONDS).set(unix_now());
    }
}

/// Set the high-water size.
pub(crate) fn log_size(size: u64) {
    lakko_metrics::metrics::gauge!(LOG_SIZE)
        .set(f64::from(u32::try_from(size).unwrap_or(u32::MAX)));
}

fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTCOMES: [CosignOutcome; 6] = [
        CosignOutcome::Ok,
        CosignOutcome::NotConsistent,
        CosignOutcome::Rollback,
        CosignOutcome::BadOperatorSignature,
        CosignOutcome::Invalid,
        CosignOutcome::Error,
    ];

    #[test]
    fn every_store_refusal_has_its_outcome() {
        assert_eq!(
            CosignOutcome::of(&CosignError::NotConsistent),
            CosignOutcome::NotConsistent
        );
        assert_eq!(
            CosignOutcome::of(&CosignError::BadOperatorSignature),
            CosignOutcome::BadOperatorSignature
        );
        assert_eq!(
            CosignOutcome::of(&CosignError::Rollback {
                presented: 1,
                seen: 2
            }),
            CosignOutcome::Rollback
        );
        assert_eq!(
            CosignOutcome::of(&CosignError::Db("x".into())),
            CosignOutcome::Error
        );
    }

    #[tokio::test]
    async fn every_series_renders_clean() {
        let metrics = lakko_metrics::install("witness", "0.0.0").unwrap();
        for outcome in OUTCOMES {
            cosign(outcome);
        }
        log_size(9);

        let text = metrics.render();
        lakko_metrics::exposition::assert_clean(&text, &[], &[]);
        for outcome in OUTCOMES {
            let line = format!(r#"{COSIGNS_TOTAL}{{outcome="{}"}}"#, outcome.as_str());
            assert!(text.contains(&line), "missing {line}:\n{text}");
        }
        assert!(text.contains(&format!("{LOG_SIZE} 9")), "{text}");
        assert!(
            text.contains(&format!("{COSIGNED_TIMESTAMP_SECONDS} ")),
            "{text}"
        );
    }
}
