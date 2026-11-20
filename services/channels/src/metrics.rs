// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The channels service's own counters ([ADR-0096] section 3).
//!
//! Every label value below is a `&'static str` from an enum in this file;
//! none names a channel, a subscriber, a key or an epoch. The request
//! counters by route, status and error code come from the shared
//! `lakko_metrics::red_layer`; what is here is what a route template cannot
//! say: whether a store accepted the write, which step of a rotation it was,
//! and which check of a proof refused it.
//!
//! The number of envelopes a rotation carried is deliberately not counted:
//! it is the size of one channel's subscriber list, which the service only
//! publishes in bands (ADR-0059 section 4).
//!
//! [ADR-0096]: ../../../docs/adr/0096-service-observability.md

use crate::storage::StorageError;

/// Posts a publisher sent past its proof, by `outcome` (`ok`, `rejected`,
/// `error`).
pub(crate) const POSTS_TOTAL: &str = "lakko_channels_posts_total";
/// Rotation requests past the proof, by `step` (`rotate`: the single-shot
/// route; `stage`: one ADR-0030 chunk; `commit`: the atomic epoch bump) and
/// `outcome` (`ok`, `stale_epoch`, `rejected`, `error`).
pub(crate) const ROTATIONS_TOTAL: &str = "lakko_channels_rotations_total";
/// Single seed-envelope deliveries past the proof, by `outcome` (`ok`,
/// `rejected`, `error`).
pub(crate) const ENVELOPE_DELIVERIES_TOTAL: &str = "lakko_channels_envelope_deliveries_total";
/// Proofs refused, by `kind` (`owner`, `copublisher`, `invite`, `write`) and
/// `reason` (`malformed`: the header is missing or does not parse;
/// `unknown_key`: no such owner, co-publisher or invite key for the
/// channel; `invalid`: outside the window or a bad signature; `replayed`:
/// the nonce was seen; `exhausted`: an invite key expired or used up).
pub(crate) const PROOF_REFUSALS_TOTAL: &str = "lakko_channels_proof_refusals_total";

/// The heartbeat job name of the retention prune loop in `main`.
pub const PRUNE_JOB: &str = "channels_retention_prune";

/// How a store write ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Stored.
    Ok,
    /// A rotation step named an epoch that is not current + 1.
    StaleEpoch,
    /// Refused for the request's own content (unknown channel or
    /// subscriber, a bad length, a future epoch ...).
    Rejected,
    /// The store failed.
    Error,
}

impl Outcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::StaleEpoch => "stale_epoch",
            Self::Rejected => "rejected",
            Self::Error => "error",
        }
    }

    /// The outcome of a store call's result.
    pub(crate) const fn of<T>(result: &Result<T, StorageError>) -> Self {
        match result {
            Ok(_) => Self::Ok,
            Err(StorageError::StaleTargetEpoch { .. }) => Self::StaleEpoch,
            Err(StorageError::Database(_)) => Self::Error,
            Err(_) => Self::Rejected,
        }
    }
}

/// Which rotation route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RotationStep {
    /// `POST .../rotate`.
    Rotate,
    /// `POST .../rotate/stage`.
    Stage,
    /// `POST .../rotate/commit`.
    Commit,
}

impl RotationStep {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Rotate => "rotate",
            Self::Stage => "stage",
            Self::Commit => "commit",
        }
    }
}

/// Which proof was checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProofKind {
    /// `x-lakko-channel-owner-proof`.
    Owner,
    /// `x-lakko-channel-copublisher-proof`.
    CoPublisher,
    /// The invite proof of a private channel's subscribe.
    Invite,
    /// A subscriber's write proof (comments, reactions).
    Write,
}

impl ProofKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::CoPublisher => "copublisher",
            Self::Invite => "invite",
            Self::Write => "write",
        }
    }
}

/// Which check refused a proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefusalReason {
    /// The header is missing or does not parse.
    Malformed,
    /// No such key for the channel.
    UnknownKey,
    /// Outside the window, or the signature does not verify.
    Invalid,
    /// The nonce was seen.
    Replayed,
    /// An invite key that expired or has no use left.
    Exhausted,
}

impl RefusalReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::UnknownKey => "unknown_key",
            Self::Invalid => "invalid",
            Self::Replayed => "replayed",
            Self::Exhausted => "exhausted",
        }
    }
}

/// Count a post write.
pub(crate) fn post(outcome: Outcome) {
    lakko_metrics::metrics::counter!(POSTS_TOTAL, "outcome" => outcome.as_str()).increment(1);
}

/// Count a rotation step.
pub(crate) fn rotation(step: RotationStep, outcome: Outcome) {
    lakko_metrics::metrics::counter!(
        ROTATIONS_TOTAL,
        "step" => step.as_str(),
        "outcome" => outcome.as_str(),
    )
    .increment(1);
}

/// Count a single envelope delivery.
pub(crate) fn envelope_delivery(outcome: Outcome) {
    lakko_metrics::metrics::counter!(ENVELOPE_DELIVERIES_TOTAL, "outcome" => outcome.as_str())
        .increment(1);
}

/// Count a refused proof.
pub(crate) fn proof_refused(kind: ProofKind, reason: RefusalReason) {
    lakko_metrics::metrics::counter!(
        PROOF_REFUSALS_TOTAL,
        "kind" => kind.as_str(),
        "reason" => reason.as_str(),
    )
    .increment(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTCOMES: [Outcome; 4] = [
        Outcome::Ok,
        Outcome::StaleEpoch,
        Outcome::Rejected,
        Outcome::Error,
    ];
    const STEPS: [RotationStep; 3] = [
        RotationStep::Rotate,
        RotationStep::Stage,
        RotationStep::Commit,
    ];
    const KINDS: [ProofKind; 4] = [
        ProofKind::Owner,
        ProofKind::CoPublisher,
        ProofKind::Invite,
        ProofKind::Write,
    ];
    const REASONS: [RefusalReason; 5] = [
        RefusalReason::Malformed,
        RefusalReason::UnknownKey,
        RefusalReason::Invalid,
        RefusalReason::Replayed,
        RefusalReason::Exhausted,
    ];

    #[test]
    fn outcome_of_a_store_result() {
        assert_eq!(Outcome::of(&Ok::<(), StorageError>(())), Outcome::Ok);
        assert_eq!(
            Outcome::of(&Err::<(), _>(StorageError::StaleTargetEpoch {
                expected: 2,
                got: 5
            })),
            Outcome::StaleEpoch
        );
        assert_eq!(
            Outcome::of(&Err::<(), _>(StorageError::Database("x".into()))),
            Outcome::Error
        );
        assert_eq!(
            Outcome::of(&Err::<(), _>(StorageError::UnknownChannel)),
            Outcome::Rejected
        );
    }

    #[tokio::test]
    async fn every_series_renders_clean() {
        let metrics = lakko_metrics::install("channels", "0.0.0").unwrap();
        for outcome in OUTCOMES {
            post(outcome);
            envelope_delivery(outcome);
            for step in STEPS {
                rotation(step, outcome);
            }
        }
        for kind in KINDS {
            for reason in REASONS {
                proof_refused(kind, reason);
            }
        }
        lakko_metrics::job_registered(PRUNE_JOB);
        lakko_metrics::job_succeeded(PRUNE_JOB);
        lakko_metrics::job_failed(PRUNE_JOB);
        let text = metrics.render();
        lakko_metrics::exposition::assert_clean(&text, &["step", "kind", "reason"], &[]);
        for outcome in OUTCOMES {
            let o = outcome.as_str();
            assert!(
                text.contains(&format!("{POSTS_TOTAL}{{outcome=\"{o}\"}}")),
                "{text}"
            );
            assert!(
                text.contains(&format!("{ENVELOPE_DELIVERIES_TOTAL}{{outcome=\"{o}\"}}")),
                "{text}"
            );
            for step in STEPS {
                let s = step.as_str();
                assert!(
                    text.contains(&format!(
                        "{ROTATIONS_TOTAL}{{step=\"{s}\",outcome=\"{o}\"}}"
                    )),
                    "{text}"
                );
            }
        }
        for kind in KINDS {
            for reason in REASONS {
                let line = format!(
                    "{PROOF_REFUSALS_TOTAL}{{kind=\"{}\",reason=\"{}\"}}",
                    kind.as_str(),
                    reason.as_str()
                );
                assert!(text.contains(&line), "{text}");
            }
        }
        assert!(text.contains(&format!(
            "lakko_job_runs_total{{task=\"{PRUNE_JOB}\",outcome=\"ok\"}}"
        )));
    }
}
