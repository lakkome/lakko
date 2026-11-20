// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The auth service's own counters ([ADR-0096] section 3, the auth row).
//!
//! Every label value below is a `&'static str` from a closed set fixed in
//! this file or in an enum's `as_str` (the limiter's [`Scope`] and
//! [`Category`], the anonymous-token context's `name()`). Nothing here is
//! labelled by an account, a device, a login handle or any request data
//! ([ADR-0078] section 11). Handlers call the helpers with one line; the
//! names are what dashboards and alert rules quote.
//!
//! The RED layer already counts every request by route template, method,
//! status class and, where the handler set one, the stable body code
//! ([`lakko_metrics::ErrorCode`]); the counters here say what the service
//! decided in the few flows an operator watches by name.
//!
//! [ADR-0096]: ../../../docs/adr/0096-service-observability.md
//! [ADR-0078]: ../../../docs/adr/0078-servers-cannot-link-accounts.md

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use lakko_anon_token::TokenContext;
use lakko_metrics::metrics::counter;

use crate::limits::{Category, LimitError, Scope};
use crate::webauthn::ApiError;

/// Signup steps answered, labelled `step` (`start`: `/signup/start`,
/// `finish`: `/signup/finish`) and `outcome` ([`outcome_of`]).
pub(crate) const SIGNUPS_TOTAL: &str = "lakko_auth_signups_total";

/// Login finishes answered, labelled `kind` ([`LoginKind`]) and `outcome`
/// ([`outcome_of`]). A `start` step is not counted: it proves nothing.
pub(crate) const LOGINS_TOTAL: &str = "lakko_auth_logins_total";

/// Refusals by a per-name counter, labelled `scope` (the limiter's
/// [`Scope::as_str`], plus [`SCOPE_HANDLE_CHANGE_INTERVAL`] and
/// [`SCOPE_BACKUP_HEAD_MINT`]) and `outcome` (`limited`: the window is
/// spent; `unavailable`: the counter store failed and the request was
/// refused). Counted where the limiter decides, so a refusal a route answers
/// silently (a recovery letter not sent) is counted too.
pub(crate) const LIMIT_REFUSALS_TOTAL: &str = "lakko_auth_limit_refusals_total";

/// `429`s by a per-address bucket, labelled `category` (the limiter's
/// [`Category::as_str`]).
pub(crate) const ADDRESS_LIMIT_REFUSALS_TOTAL: &str = "lakko_auth_address_limit_refusals_total";

/// Anonymous tokens issued (one per evaluated blinded element), labelled
/// `context` (the token context's name).
pub(crate) const ANON_TOKENS_ISSUED_TOTAL: &str = "lakko_auth_anon_tokens_issued_total";

/// Issuance requests refused because the account's quota for the period is
/// spent, labelled `context`.
pub(crate) const ANON_TOKEN_QUOTA_EXHAUSTED_TOTAL: &str =
    "lakko_auth_anon_token_quota_exhausted_total";

/// `POST /account/keys/reset/finish` answers, labelled `outcome`
/// ([`outcome_of`]).
pub(crate) const KEY_RESETS_TOTAL: &str = "lakko_auth_key_resets_total";

/// Responses refused with `403 {"error":"fresh-auth-required"}`, on any
/// route. No labels: the RED layer's `code` label has the route.
pub(crate) const FRESH_AUTH_REFUSALS_TOTAL: &str = "lakko_auth_fresh_auth_refusals_total";

/// `POST /auth/session/refresh` answers, labelled `outcome`: `ok`,
/// `invalid` (`refresh-invalid`), `superseded`
/// (`key-generation-superseded`), `revoked` (`device-revoked`), `stale`
/// (`statement-stale`), `error` (anything else).
pub(crate) const SESSION_REFRESHES_TOTAL: &str = "lakko_auth_session_refreshes_total";

/// `scope` of the minimum interval between two handle-changing binds of one
/// account (`PUT /auth/login-handle`, `handle-bind-limited`), which is not a
/// per-name counter.
pub(crate) const SCOPE_HANDLE_CHANGE_INTERVAL: &str = "handle_change_interval";

/// `scope` of the per-account backup-head mint rate limit (ADR-0037 F5b).
pub(crate) const SCOPE_BACKUP_HEAD_MINT: &str = "backup_head_mint";

/// Background-job names (`lakko_job_*{job}`), one per pruner of this service.
/// The anonymous-token and proof-of-work pruners name themselves in their
/// shared crates.
pub(crate) mod jobs {
    /// `opaque_login_states` and `signup_states`.
    pub(crate) const LOGIN_STATE_PRUNE: &str = "login_state_prune";
    /// Name counters, address buckets, device-link wrong codes and
    /// suspensions.
    pub(crate) const LIMITS_PRUNE: &str = "limits_prune";
    /// `webauthn_states`, action grants, device grants.
    pub(crate) const WEBAUTHN_STATE_PRUNE: &str = "webauthn_state_prune";
    /// `device_session_floors`.
    pub(crate) const SESSION_FLOOR_PRUNE: &str = "session_floor_prune";
    /// Recall anchors, states and outcomes.
    pub(crate) const RECALL_PRUNE: &str = "recall_prune";
    /// Recovery-email challenges and tickets.
    pub(crate) const RECOVERY_EMAIL_PRUNE: &str = "recovery_email_prune";
    /// `account_notices` past their retention.
    pub(crate) const ACCOUNT_NOTICE_PRUNE: &str = "account_notice_prune";
    /// Accounts nobody named within the unclaimed lifetime.
    pub(crate) const UNCLAIMED_ACCOUNT_PRUNE: &str = "unclaimed_account_prune";
}

/// The `kind` of a login finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoginKind {
    /// `POST /login/finish`.
    Password,
    /// `POST /webauthn/auth/finish`.
    Passkey,
    /// `POST /recovery/login/finish` (the recovery phrase).
    Recovery,
    /// `POST /totp/recover`.
    Totp,
    /// `POST /device/authorize/redeem`.
    DeviceGrant,
}

impl LoginKind {
    /// Every kind, for tests.
    #[cfg(test)]
    pub(crate) const ALL: [Self; 5] = [
        Self::Password,
        Self::Passkey,
        Self::Recovery,
        Self::Totp,
        Self::DeviceGrant,
    ];

    /// The label value.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Passkey => "passkey",
            Self::Recovery => "recovery",
            Self::Totp => "totp",
            Self::DeviceGrant => "device_grant",
        }
    }
}

/// The `step` of a signup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignupStep {
    /// `POST /signup/start`.
    Start,
    /// `POST /signup/finish`.
    Finish,
}

impl SignupStep {
    /// The label value.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Finish => "finish",
        }
    }
}

/// The `outcome` of an answer, from the status the handler answered with:
/// `ok` (2xx), `limited` (429), `refused` (any other 4xx), `error`
/// (anything else, a 5xx included).
pub(crate) fn outcome_of(status: StatusCode) -> &'static str {
    if status.is_success() {
        "ok"
    } else if status == StatusCode::TOO_MANY_REQUESTS {
        "limited"
    } else if status.is_client_error() {
        "refused"
    } else {
        "error"
    }
}

/// Render a signup step's answer and count it.
pub(crate) fn signup<T: IntoResponse, E: IntoResponse>(
    step: SignupStep,
    answer: Result<T, E>,
) -> Response {
    let response = answer.into_response();
    counter!(SIGNUPS_TOTAL, "step" => step.as_str(), "outcome" => outcome_of(response.status()))
        .increment(1);
    response
}

/// Render a login finish's answer and count it.
pub(crate) fn login<T: IntoResponse, E: IntoResponse>(
    kind: LoginKind,
    answer: Result<T, E>,
) -> Response {
    let response = answer.into_response();
    counter!(LOGINS_TOTAL, "kind" => kind.as_str(), "outcome" => outcome_of(response.status()))
        .increment(1);
    response
}

/// Render a key-reset finish's answer and count it.
pub(crate) fn key_reset<T: IntoResponse, E: IntoResponse>(answer: Result<T, E>) -> Response {
    let response = answer.into_response();
    counter!(KEY_RESETS_TOTAL, "outcome" => outcome_of(response.status())).increment(1);
    response
}

/// The `outcome` of a session refresh, from what the handler decided.
pub(crate) const fn refresh_outcome<T>(answer: &Result<T, ApiError>) -> &'static str {
    match answer {
        Ok(_) => "ok",
        Err(ApiError::RefreshInvalid) => "invalid",
        Err(ApiError::KeyGenerationSuperseded) => "superseded",
        Err(ApiError::DeviceRevoked) => "revoked",
        Err(ApiError::StatementStale { .. }) => "stale",
        Err(_) => "error",
    }
}

/// Count a session refresh and render its answer.
pub(crate) fn session_refresh<T: IntoResponse>(answer: Result<T, ApiError>) -> Response {
    counter!(SESSION_REFRESHES_TOTAL, "outcome" => refresh_outcome(&answer)).increment(1);
    answer.into_response()
}

fn limit_refused(scope: &'static str, outcome: &'static str) {
    counter!(LIMIT_REFUSALS_TOTAL, "scope" => scope, "outcome" => outcome).increment(1);
}

/// Count a per-name limiter refusal of a [`Scope`].
pub(crate) fn scope_refused(scope: Scope, refusal: &LimitError) {
    let outcome = match refusal {
        LimitError::Limited { .. } => "limited",
        LimitError::Unavailable => "unavailable",
    };
    limit_refused(scope.as_str(), outcome);
}

/// Count a handle-changing bind refused for coming too soon after the last.
pub(crate) fn handle_change_too_soon() {
    limit_refused(SCOPE_HANDLE_CHANGE_INTERVAL, "limited");
}

/// Count a backup-head mint refused by the per-account mint rate limit.
pub(crate) fn backup_head_mint_limited() {
    limit_refused(SCOPE_BACKUP_HEAD_MINT, "limited");
}

/// Count a per-address `429` of `category`.
pub(crate) fn address_refused(category: Category) {
    counter!(ADDRESS_LIMIT_REFUSALS_TOTAL, "category" => category.as_str()).increment(1);
}

/// Count `count` anonymous tokens issued for `context`.
pub(crate) fn anon_tokens_issued(context: TokenContext, count: u32) {
    counter!(ANON_TOKENS_ISSUED_TOTAL, "context" => context.name()).increment(u64::from(count));
}

/// Count an issuance refused for a spent quota of `context`.
pub(crate) fn anon_token_quota_exhausted(context: TokenContext) {
    counter!(ANON_TOKEN_QUOTA_EXHAUSTED_TOTAL, "context" => context.name()).increment(1);
}

/// Count a `fresh-auth-required` refusal.
pub(crate) fn fresh_auth_refused() {
    counter!(FRESH_AUTH_REFUSALS_TOTAL).increment(1);
}

/// Record one pass of a background job as its heartbeat: `ok` when
/// every step succeeded, else `error`.
pub(crate) fn job_ran(job: &'static str, all_ok: bool) {
    if all_ok {
        lakko_metrics::job_succeeded(job);
    } else {
        lakko_metrics::job_failed(job);
    }
}

/// What route tests use to read the counters a driven request moved.
#[cfg(test)]
pub(crate) mod test_support {
    /// The process recorder, installed on first use.
    pub(crate) fn installed() -> lakko_metrics::Metrics {
        lakko_metrics::install("auth", "0.0.0").expect("install the metrics recorder")
    }

    /// The sum of every sample of `name` whose labels include each
    /// `key="value"` of `labels`, in the current exposition.
    pub(crate) fn value(name: &str, labels: &[(&str, &str)]) -> f64 {
        let text = installed().render();
        text.lines()
            .filter(|l| {
                l.strip_prefix(name)
                    .is_some_and(|rest| rest.starts_with('{') || rest.starts_with(' '))
            })
            .filter(|l| {
                labels
                    .iter()
                    .all(|(k, v)| l.contains(&format!("{k}=\"{v}\"")))
            })
            .filter_map(|l| l.rsplit(' ').next()?.parse::<f64>().ok())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Json;

    const OUTCOMES: [&str; 4] = ["ok", "refused", "limited", "error"];

    fn answer(status: StatusCode) -> Result<Json<()>, StatusCode> {
        if status.is_success() {
            Ok(Json(()))
        } else {
            Err(status)
        }
    }

    const STATUSES: [StatusCode; 4] = [
        StatusCode::OK,
        StatusCode::UNAUTHORIZED,
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::SERVICE_UNAVAILABLE,
    ];

    /// Call every helper with every label value it can take.
    fn record_every_series() {
        for status in STATUSES {
            for step in [SignupStep::Start, SignupStep::Finish] {
                assert_eq!(signup(step, answer(status)).status(), status);
            }
            for kind in LoginKind::ALL {
                assert_eq!(login(kind, answer(status)).status(), status);
            }
            assert_eq!(key_reset(answer(status)).status(), status);
        }
        let _ = session_refresh(Ok::<_, ApiError>(Json(())));
        for e in [
            ApiError::RefreshInvalid,
            ApiError::KeyGenerationSuperseded,
            ApiError::DeviceRevoked,
            ApiError::StatementStale { server_time_ms: 1 },
            ApiError::Internal,
        ] {
            let _ = session_refresh(Err::<Json<()>, _>(e));
        }
        let limited = LimitError::Limited {
            code: crate::limits::CODE_TOO_MANY_ATTEMPTS,
            retry_after_secs: 1,
        };
        for scope in Scope::ALL {
            scope_refused(scope, &limited);
            scope_refused(scope, &LimitError::Unavailable);
        }
        handle_change_too_soon();
        backup_head_mint_limited();
        for category in Category::ALL {
            address_refused(category);
        }
        for context in TokenContext::ALL {
            anon_tokens_issued(context, 3);
            anon_token_quota_exhausted(context);
        }
        fresh_auth_refused();
        for job in ALL_JOBS {
            lakko_metrics::job_registered(job);
            job_ran(job, true);
            job_ran(job, false);
        }
    }

    const ALL_JOBS: [&str; 8] = [
        jobs::LOGIN_STATE_PRUNE,
        jobs::LIMITS_PRUNE,
        jobs::WEBAUTHN_STATE_PRUNE,
        jobs::SESSION_FLOOR_PRUNE,
        jobs::RECALL_PRUNE,
        jobs::RECOVERY_EMAIL_PRUNE,
        jobs::ACCOUNT_NOTICE_PRUNE,
        jobs::UNCLAIMED_ACCOUNT_PRUNE,
    ];

    /// Every series line [`record_every_series`] must have produced.
    fn expected_series() -> Vec<String> {
        let mut out = Vec::new();
        for outcome in OUTCOMES {
            for step in ["start", "finish"] {
                out.push(format!(
                    "{SIGNUPS_TOTAL}{{step=\"{step}\",outcome=\"{outcome}\"}}"
                ));
            }
            for kind in LoginKind::ALL {
                out.push(format!(
                    "{LOGINS_TOTAL}{{kind=\"{}\",outcome=\"{outcome}\"}}",
                    kind.as_str()
                ));
            }
            out.push(format!("{KEY_RESETS_TOTAL}{{outcome=\"{outcome}\"}}"));
        }
        for outcome in ["ok", "invalid", "superseded", "revoked", "stale", "error"] {
            out.push(format!(
                "{SESSION_REFRESHES_TOTAL}{{outcome=\"{outcome}\"}}"
            ));
        }
        for scope in Scope::ALL {
            for outcome in ["limited", "unavailable"] {
                out.push(format!(
                    "{LIMIT_REFUSALS_TOTAL}{{scope=\"{}\",outcome=\"{outcome}\"}}",
                    scope.as_str()
                ));
            }
        }
        for scope in [SCOPE_HANDLE_CHANGE_INTERVAL, SCOPE_BACKUP_HEAD_MINT] {
            out.push(format!(
                "{LIMIT_REFUSALS_TOTAL}{{scope=\"{scope}\",outcome=\"limited\"}}"
            ));
        }
        for category in Category::ALL {
            out.push(format!(
                "{ADDRESS_LIMIT_REFUSALS_TOTAL}{{category=\"{}\"}}",
                category.as_str()
            ));
        }
        for context in TokenContext::ALL {
            out.push(format!(
                "{ANON_TOKENS_ISSUED_TOTAL}{{context=\"{}\"}}",
                context.name()
            ));
            out.push(format!(
                "{ANON_TOKEN_QUOTA_EXHAUSTED_TOTAL}{{context=\"{}\"}}",
                context.name()
            ));
        }
        out.push(format!("{FRESH_AUTH_REFUSALS_TOTAL} "));
        for job in ALL_JOBS {
            for outcome in ["ok", "error"] {
                out.push(format!(
                    "lakko_job_runs_total{{task=\"{job}\",outcome=\"{outcome}\"}}"
                ));
            }
        }
        out
    }

    /// Every helper with every label value it can take, rendered and
    /// checked against ADR-0096 section 1.
    #[tokio::test]
    async fn every_series_is_recorded_and_the_exposition_is_clean() {
        let metrics = lakko_metrics::install("auth", "0.0.0").expect("install");
        record_every_series();
        let text = metrics.render();
        lakko_metrics::exposition::assert_clean(
            &text,
            &["kind", "step", "scope", "category", "context"],
            &[],
        );
        for needle in expected_series() {
            assert!(
                text.lines().any(|l| l.starts_with(&needle)),
                "missing series {needle}\n{text}"
            );
        }
    }

    #[test]
    fn outcomes_follow_the_status_class() {
        assert_eq!(outcome_of(StatusCode::OK), "ok");
        assert_eq!(outcome_of(StatusCode::NO_CONTENT), "ok");
        assert_eq!(outcome_of(StatusCode::UNAUTHORIZED), "refused");
        assert_eq!(outcome_of(StatusCode::TOO_MANY_REQUESTS), "limited");
        assert_eq!(outcome_of(StatusCode::SERVICE_UNAVAILABLE), "error");
    }

    #[test]
    fn every_scope_and_category_has_a_distinct_snake_case_label() {
        let snake = |s: &str| s.chars().all(|c| c.is_ascii_lowercase() || c == '_');
        let mut seen = std::collections::HashSet::new();
        for s in Scope::ALL
            .iter()
            .map(|s| s.as_str())
            .chain([SCOPE_HANDLE_CHANGE_INTERVAL, SCOPE_BACKUP_HEAD_MINT])
        {
            assert!(snake(s), "{s}");
            assert!(seen.insert(s), "duplicate scope label {s}");
        }
        let mut seen = std::collections::HashSet::new();
        for c in Category::ALL.iter().map(|c| c.as_str()) {
            assert!(snake(c), "{c}");
            assert!(seen.insert(c), "duplicate category label {c}");
        }
    }

    #[test]
    fn every_api_error_body_code_is_the_one_its_answer_carries() {
        for (e, code) in [
            (ApiError::FreshAuthRequired, "fresh-auth-required"),
            (ApiError::RefreshInvalid, "refresh-invalid"),
            (
                ApiError::Coded(StatusCode::GONE, "ticket-refused"),
                "ticket-refused",
            ),
            (
                ApiError::Limit(LimitError::Unavailable),
                crate::limits::CODE_LIMITER_UNAVAILABLE,
            ),
        ] {
            let response = e.into_response();
            assert_eq!(
                response.extensions().get::<lakko_metrics::ErrorCode>(),
                Some(&lakko_metrics::ErrorCode(code))
            );
        }
        let plain = ApiError::Internal.into_response();
        assert!(plain
            .extensions()
            .get::<lakko_metrics::ErrorCode>()
            .is_none());
    }
}
