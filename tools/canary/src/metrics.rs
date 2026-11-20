// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The canary's metrics (ADR-0096 section 5).
//!
//! Labels come from closed sets only: `step` is one of [`Step`], `outcome`
//! one of `ok` / `failed` / `skipped`. No account, group, blob, room or peer
//! value is ever a label.

use lakko_metrics::metrics::{counter, gauge};

/// A step of a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// OPAQUE sign-in of both accounts.
    SignIn,
    /// A sends, B receives and decrypts.
    Message,
    /// Upload, download, delete.
    Attachment,
    /// SFrame-sealed audio from A reaches B through the SFU.
    Call,
}

impl Step {
    /// The label value.
    #[must_use]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::SignIn => "sign_in",
            Self::Message => "message",
            Self::Attachment => "attachment",
            Self::Call => "call",
        }
    }
}

/// How a step ended.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Outcome {
    /// It ran and succeeded, in this many seconds.
    Ok(f64),
    /// It ran (or could not run) and failed.
    Failed,
    /// It was due but not run (young account).
    Skipped,
}

/// Record one step's outcome. `consecutive_failures` is the value after
/// this outcome.
pub(crate) fn record(step: Step, outcome: Outcome, consecutive_failures: u64) {
    let s = step.name();
    match outcome {
        Outcome::Ok(secs) => {
            gauge!("lakko_canary_step_success", "step" => s).set(1.0);
            gauge!("lakko_canary_step_seconds", "step" => s).set(secs);
            gauge!("lakko_canary_step_consecutive_failures", "step" => s).set(0.0);
            counter!("lakko_canary_step_runs_total", "step" => s, "outcome" => "ok").increment(1);
        },
        Outcome::Failed => {
            gauge!("lakko_canary_step_success", "step" => s).set(0.0);
            #[allow(clippy::cast_precision_loss)]
            gauge!("lakko_canary_step_consecutive_failures", "step" => s)
                .set(consecutive_failures as f64);
            counter!("lakko_canary_step_runs_total", "step" => s, "outcome" => "failed")
                .increment(1);
        },
        Outcome::Skipped => {
            counter!("lakko_canary_step_runs_total", "step" => s, "outcome" => "skipped")
                .increment(1);
        },
    }
}

/// Record the end of a run.
pub(crate) fn run_finished(unix_secs: u64) {
    #[allow(clippy::cast_precision_loss)]
    gauge!("lakko_canary_last_run_timestamp_seconds").set(unix_secs as f64);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposition_carries_only_closed_labels() {
        let text = lakko_metrics::exposition::record_locally(|| {
            record(Step::SignIn, Outcome::Ok(0.4), 0);
            record(Step::Message, Outcome::Failed, 3);
            record(Step::Call, Outcome::Skipped, 0);
            run_finished(1_800_000_000);
        });
        lakko_metrics::exposition::assert_clean(&text, &["step", "outcome"], &[]);
        assert!(text.contains(r#"lakko_canary_step_success{step="sign_in"} 1"#));
        assert!(text.contains(r#"lakko_canary_step_consecutive_failures{step="message"} 3"#));
        assert!(text.contains(r#"lakko_canary_step_runs_total{step="call",outcome="skipped"} 1"#));
        assert!(text.contains("lakko_canary_last_run_timestamp_seconds 1800000000"));
        assert!(!text.contains(r#"lakko_canary_step_success{step="call"}"#));
    }
}
