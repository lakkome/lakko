// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The heartbeat of a background job (a pruner, a sweep, a reconciler).
//!
//! A job that stops prints nothing until its table grows. Each run calls
//! [`job_succeeded`] or [`job_failed`] with a fixed job name, and an alert
//! fires when `lakko_job_last_success_timestamp_seconds` is older than a few
//! of the job's periods. A job calls [`job_registered`] when it is spawned,
//! so one that never succeeds is reported stale after the same window
//! instead of never appearing.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::names;

/// Record that `job` was started: its last-success time is set to now, so a
/// job that then never succeeds is reported stale once its window passes.
pub fn job_registered(job: &'static str) {
    metrics::gauge!(names::JOB_LAST_SUCCESS_TIMESTAMP_SECONDS, "task" => job).set(unix_now());
}

/// Record a successful run of `job`.
pub fn job_succeeded(job: &'static str) {
    metrics::gauge!(names::JOB_LAST_SUCCESS_TIMESTAMP_SECONDS, "task" => job).set(unix_now());
    metrics::counter!(names::JOB_RUNS_TOTAL, "task" => job, "outcome" => "ok").increment(1);
}

/// Record a failed run of `job`. The last-success time does not move.
pub fn job_failed(job: &'static str) {
    metrics::counter!(names::JOB_RUNS_TOTAL, "task" => job, "outcome" => "error").increment(1);
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}
