// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The discovery service's own counters and gauges ([ADR-0096] section 3).
//!
//! Every label value below is a `&'static str` from an enum or a constant in
//! this file; none names an account, a username, a key, a handle, a link or
//! an address. The request counters by route, status and error code come
//! from the shared `lakko_metrics::red_layer` (the `code` label is set by
//! [`with_code`] on every `{"error": <code>}` / `{"code": <code>}` refusal);
//! what is here is what a route template cannot say: how many OPRF
//! evaluations a request ran, how an identity upload was decided, and how
//! far the key-transparency log lags behind the directory.
//!
//! Anonymous-token spends are counted centrally
//! (`lakko_anon_token_spends_total`) and are not repeated here.
//!
//! [ADR-0096]: ../../../docs/adr/0096-service-observability.md

use axum::response::Response;

/// OPRF evaluation requests answered with evaluations, by `kind` (`single`:
/// `POST /discovery/oprf`; `batch`: `POST /discovery/oprf/batch`). A refused
/// request is not counted here (the RED counter has it with its `code`).
pub(crate) const OPRF_REQUESTS_TOTAL: &str = "lakko_discovery_oprf_requests_total";
/// Blinded elements evaluated, by `kind` (`single`, `batch`). A single
/// request adds one; a batch adds its length (1..=66).
pub(crate) const OPRF_EVALUATIONS_TOTAL: &str = "lakko_discovery_oprf_evaluations_total";
/// `POST /discovery/identity` uploads, by `outcome`: `unchanged` (the same
/// key re-uploaded), `changed` (a first publish or a proven rotation),
/// `reset` (an attested key reset), one value per refusal code of the
/// change rule (`identity_change_needs_attestation`,
/// `key_generation_superseded`, `key_generation_not_newer`,
/// `key_reset_attestation_invalid`, `key_reset_disabled`,
/// `identity_changed_too_recently`, `supersession_proof_invalid`,
/// `refused_other` for a code not in that list), `invalid` (a malformed body
/// or a body naming another account) and `error` (the store failed or stayed
/// contended).
pub(crate) const IDENTITY_UPLOADS_TOTAL: &str = "lakko_discovery_identity_uploads_total";
/// Appends to the key-transparency log, by `kind` (`upload`: the hot path of
/// an identity upload that changed the key; `reconcile`: the backfill sweep)
/// and `outcome` (`ok`: the log answered 2xx; `failed`: anything else, the
/// sweep retries).
pub(crate) const KT_APPENDS_TOTAL: &str = "lakko_discovery_kt_appends_total";
/// Accounts whose current identity key the transparency log has not
/// confirmed yet (the reconciler's queue), measured at each sweep. A gauge
/// over all accounts, never per account.
pub(crate) const KT_APPEND_PENDING: &str = "lakko_discovery_kt_append_pending";
/// Seconds since the oldest unconfirmed identity row was last written,
/// measured at each sweep; 0 when nothing is pending. Approximate: a
/// same-key re-upload rewrites the row's `updated_at` and makes it look
/// younger.
pub(crate) const KT_APPEND_OLDEST_PENDING_AGE_SECONDS: &str =
    "lakko_discovery_kt_append_oldest_pending_age_seconds";

/// Heartbeat job names (`lakko_job_*{job}`), one per background loop.
pub(crate) mod jobs {
    /// `registry::spawn_retired_pruner` (retired usernames past the hold).
    pub(crate) const RETIRED_USERNAME_PRUNE: &str = "discovery_retired_username_prune";
    /// `invite::spawn_pruner` (expired contact-invite blobs).
    pub(crate) const INVITE_PRUNE: &str = "discovery_invite_prune";
    /// `group_invite::spawn_pruner` (expired group-invite blobs).
    pub(crate) const GROUP_INVITE_PRUNE: &str = "discovery_group_invite_prune";
    /// `keypackage::spawn_tombstone_pruner` (old device tombstones).
    pub(crate) const KEYPACKAGE_TOMBSTONE_PRUNE: &str = "discovery_keypackage_tombstone_prune";
    /// `handles::spawn_pruner` (lapsed leases, held names, old nonces).
    pub(crate) const HANDLE_PRUNE: &str = "discovery_handle_prune";
    /// `account::spawn_unclaimed_pruner` (ADR-0032 unclaimed accounts).
    pub(crate) const UNCLAIMED_ACCOUNT_PRUNE: &str = "discovery_unclaimed_account_prune";
    /// `rate_limit::spawn_pruner` (idle in-memory buckets).
    pub(crate) const RATE_LIMIT_BUCKET_PRUNE: &str = "discovery_rate_limit_bucket_prune";
    /// `transparency_log::spawn_transparency_reconciler` (KT-log backfill).
    pub(crate) const KT_RECONCILE: &str = "discovery_kt_reconcile";
    /// `continuity::spawn_sweeper` (ADR-0101 expired continuity bindings).
    pub(crate) const CONTINUITY_BINDING_SWEEP: &str = "discovery_continuity_binding_sweep";

    /// Every job name, for the test.
    #[cfg(test)]
    pub(crate) const ALL: [&str; 9] = [
        RETIRED_USERNAME_PRUNE,
        INVITE_PRUNE,
        GROUP_INVITE_PRUNE,
        KEYPACKAGE_TOMBSTONE_PRUNE,
        HANDLE_PRUNE,
        UNCLAIMED_ACCOUNT_PRUNE,
        RATE_LIMIT_BUCKET_PRUNE,
        KT_RECONCILE,
        CONTINUITY_BINDING_SWEEP,
    ];
}

/// Record one run of a background job from its result.
pub(crate) fn job_run<T, E>(job: &'static str, result: &Result<T, E>) {
    if result.is_ok() {
        lakko_metrics::job_succeeded(job);
    } else {
        lakko_metrics::job_failed(job);
    }
}

/// Mark `response` with the service's own error `code`, so the RED layer
/// labels it. `code` is always one of the fixed codes in source.
pub(crate) fn with_code(mut response: Response, code: &'static str) -> Response {
    response
        .extensions_mut()
        .insert(lakko_metrics::ErrorCode(code));
    response
}

/// Which OPRF route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OprfKind {
    /// `POST /discovery/oprf`.
    Single,
    /// `POST /discovery/oprf/batch`.
    Batch,
}

impl OprfKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Batch => "batch",
        }
    }
}

/// Count an answered OPRF request that evaluated `elements` blinded
/// elements.
pub(crate) fn oprf_evaluated(kind: OprfKind, elements: usize) {
    lakko_metrics::metrics::counter!(OPRF_REQUESTS_TOTAL, "kind" => kind.as_str()).increment(1);
    lakko_metrics::metrics::counter!(OPRF_EVALUATIONS_TOTAL, "kind" => kind.as_str())
        .increment(u64::try_from(elements).unwrap_or(u64::MAX));
}

/// How an identity upload ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IdentityOutcome {
    /// The same key was re-uploaded.
    Unchanged,
    /// A first publish or a proven rotation.
    Changed,
    /// An attested key reset.
    Reset,
    /// Refused by the change rule with one of its codes.
    Refused(IdentityRefusal),
    /// A malformed body, or a body naming another account.
    Invalid,
    /// The store failed or stayed contended.
    Error,
}

/// The change-rule refusal codes of an identity upload (`crate::key_reset`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IdentityRefusal {
    /// `identity-change-needs-attestation`.
    NeedsAttestation,
    /// `key-generation-superseded`.
    GenerationSuperseded,
    /// `key-generation-not-newer`.
    GenerationNotNewer,
    /// `key-reset-attestation-invalid`.
    AttestationInvalid,
    /// `key-reset-disabled`.
    ResetDisabled,
    /// `identity-changed-too-recently`.
    CoolingDown,
    /// `supersession-proof-invalid`.
    ProofInvalid,
    /// `x25519-binding-invalid`.
    X25519BindingInvalid,
    /// A code not in this list.
    Other,
}

impl IdentityRefusal {
    /// The refusal for a `{"error": code}` body code.
    pub(crate) fn of_code(code: &str) -> Self {
        use crate::key_reset as k;
        match code {
            k::CODE_NEEDS_ATTESTATION => Self::NeedsAttestation,
            k::CODE_KEY_GENERATION_SUPERSEDED => Self::GenerationSuperseded,
            k::CODE_GENERATION_NOT_NEWER => Self::GenerationNotNewer,
            k::CODE_ATTESTATION_INVALID => Self::AttestationInvalid,
            k::CODE_KEY_RESET_DISABLED => Self::ResetDisabled,
            k::CODE_COOLING_DOWN => Self::CoolingDown,
            k::CODE_PROOF_INVALID => Self::ProofInvalid,
            crate::identity_bundle::CODE_X25519_BINDING_INVALID => Self::X25519BindingInvalid,
            _ => Self::Other,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::NeedsAttestation => "identity_change_needs_attestation",
            Self::GenerationSuperseded => "key_generation_superseded",
            Self::GenerationNotNewer => "key_generation_not_newer",
            Self::AttestationInvalid => "key_reset_attestation_invalid",
            Self::ResetDisabled => "key_reset_disabled",
            Self::CoolingDown => "identity_changed_too_recently",
            Self::ProofInvalid => "supersession_proof_invalid",
            Self::X25519BindingInvalid => "x25519_binding_invalid",
            Self::Other => "refused_other",
        }
    }
}

impl IdentityOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Unchanged => "unchanged",
            Self::Changed => "changed",
            Self::Reset => "reset",
            Self::Refused(r) => r.as_str(),
            Self::Invalid => "invalid",
            Self::Error => "error",
        }
    }
}

/// Count an identity upload.
pub(crate) fn identity_upload(outcome: IdentityOutcome) {
    lakko_metrics::metrics::counter!(IDENTITY_UPLOADS_TOTAL, "outcome" => outcome.as_str())
        .increment(1);
}

/// Which path appended to the transparency log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KtAppendKind {
    /// The identity upload that changed the key.
    Upload,
    /// The reconciliation sweep.
    Reconcile,
}

impl KtAppendKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Upload => "upload",
            Self::Reconcile => "reconcile",
        }
    }
}

/// Count a transparency-log append; `confirmed` is whether the log answered
/// 2xx.
pub(crate) fn kt_append(kind: KtAppendKind, confirmed: bool) {
    let outcome = if confirmed { "ok" } else { "failed" };
    lakko_metrics::metrics::counter!(
        KT_APPENDS_TOTAL,
        "kind" => kind.as_str(),
        "outcome" => outcome,
    )
    .increment(1);
}

/// Set the reconciler queue gauges: `pending` unconfirmed accounts, the
/// oldest last written `oldest_age_secs` ago (`None` when nothing is
/// pending).
pub(crate) fn kt_pending(pending: i64, oldest_age_secs: Option<i64>) {
    lakko_metrics::metrics::gauge!(KT_APPEND_PENDING).set(gauge_value(pending));
    lakko_metrics::metrics::gauge!(KT_APPEND_OLDEST_PENDING_AGE_SECONDS)
        .set(gauge_value(oldest_age_secs.unwrap_or(0)));
}

/// A non-negative count as a gauge value, saturating at `u32::MAX`.
fn gauge_value(n: i64) -> f64 {
    f64::from(u32::try_from(n.max(0)).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFUSALS: [IdentityRefusal; 8] = [
        IdentityRefusal::NeedsAttestation,
        IdentityRefusal::GenerationSuperseded,
        IdentityRefusal::GenerationNotNewer,
        IdentityRefusal::AttestationInvalid,
        IdentityRefusal::ResetDisabled,
        IdentityRefusal::CoolingDown,
        IdentityRefusal::ProofInvalid,
        IdentityRefusal::Other,
    ];

    #[test]
    fn every_change_rule_code_has_its_own_outcome() {
        use crate::key_reset as k;
        for (code, refusal) in [
            (k::CODE_NEEDS_ATTESTATION, IdentityRefusal::NeedsAttestation),
            (
                k::CODE_KEY_GENERATION_SUPERSEDED,
                IdentityRefusal::GenerationSuperseded,
            ),
            (
                k::CODE_GENERATION_NOT_NEWER,
                IdentityRefusal::GenerationNotNewer,
            ),
            (
                k::CODE_ATTESTATION_INVALID,
                IdentityRefusal::AttestationInvalid,
            ),
            (k::CODE_KEY_RESET_DISABLED, IdentityRefusal::ResetDisabled),
            (k::CODE_COOLING_DOWN, IdentityRefusal::CoolingDown),
            (k::CODE_PROOF_INVALID, IdentityRefusal::ProofInvalid),
        ] {
            assert_eq!(IdentityRefusal::of_code(code), refusal);
            assert_eq!(refusal.as_str(), code.replace('-', "_"));
        }
        assert_eq!(
            IdentityRefusal::of_code(k::CODE_DEVICE_MISMATCH),
            IdentityRefusal::Other
        );
    }

    #[tokio::test]
    async fn every_series_renders_clean() {
        let metrics = lakko_metrics::install("discovery", "0.0.0").unwrap();
        for kind in [OprfKind::Single, OprfKind::Batch] {
            oprf_evaluated(kind, 3);
        }
        let mut outcomes = vec![
            IdentityOutcome::Unchanged,
            IdentityOutcome::Changed,
            IdentityOutcome::Reset,
            IdentityOutcome::Invalid,
            IdentityOutcome::Error,
        ];
        outcomes.extend(REFUSALS.map(IdentityOutcome::Refused));
        for outcome in &outcomes {
            identity_upload(*outcome);
        }
        for kind in [KtAppendKind::Upload, KtAppendKind::Reconcile] {
            kt_append(kind, true);
            kt_append(kind, false);
        }
        kt_pending(4, Some(90));
        for job in jobs::ALL {
            lakko_metrics::job_registered(job);
            job_run(job, &Ok::<(), ()>(()));
            job_run(job, &Err::<(), ()>(()));
        }

        let text = metrics.render();
        lakko_metrics::exposition::assert_clean(&text, &["kind", "context"], &[]);
        for kind in ["single", "batch"] {
            assert!(
                text.contains(&format!(r#"{OPRF_REQUESTS_TOTAL}{{kind="{kind}"}}"#)),
                "{text}"
            );
            assert!(
                text.contains(&format!(r#"{OPRF_EVALUATIONS_TOTAL}{{kind="{kind}"}}"#)),
                "{text}"
            );
        }
        for outcome in &outcomes {
            let line = format!(
                r#"{IDENTITY_UPLOADS_TOTAL}{{outcome="{}"}}"#,
                outcome.as_str()
            );
            assert!(text.contains(&line), "missing {line}:\n{text}");
        }
        for kind in ["upload", "reconcile"] {
            for outcome in ["ok", "failed"] {
                let line = format!(r#"{KT_APPENDS_TOTAL}{{kind="{kind}",outcome="{outcome}"}}"#);
                assert!(text.contains(&line), "missing {line}:\n{text}");
            }
        }
        assert!(text.contains(&format!("{KT_APPEND_PENDING} 4")), "{text}");
        assert!(
            text.contains(&format!("{KT_APPEND_OLDEST_PENDING_AGE_SECONDS} 90")),
            "{text}"
        );
        for job in jobs::ALL {
            for outcome in ["ok", "error"] {
                let line = format!(r#"lakko_job_runs_total{{task="{job}",outcome="{outcome}"}}"#);
                assert!(text.contains(&line), "missing {line}:\n{text}");
            }
        }
    }

    #[test]
    fn with_code_sets_the_extension() {
        use axum::response::IntoResponse as _;
        let res = with_code(
            axum::http::StatusCode::CONFLICT.into_response(),
            "name-taken",
        );
        assert_eq!(
            res.extensions().get::<lakko_metrics::ErrorCode>(),
            Some(&lakko_metrics::ErrorCode("name-taken"))
        );
    }
}
