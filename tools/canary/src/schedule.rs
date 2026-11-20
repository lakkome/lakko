// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! When the call step runs.
//!
//! The call step runs every `call_every`-th run. It is skipped while either
//! account is younger than [`YOUNG_ACCOUNT_SECS`]: auth issues a young
//! account only 8 `call-join` tokens a day (`services/auth`,
//! `LAKKO_AUTH_YOUNG_ACCOUNT_AGE_SECS`, 24 h), and the margin of one hour
//! covers clock skew between this host and auth.

/// An account younger than this (25 h) does not join calls.
pub(crate) const YOUNG_ACCOUNT_SECS: u64 = 25 * 3600;

/// What happens to the call step in a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CallPlan {
    /// Not this run's turn: nothing is recorded.
    NotDue,
    /// Due, but an account is too young: recorded as skipped.
    SkipYoung,
    /// Run it.
    Run,
}

/// Decide the call step for the run numbered `run_index` (0-based count of
/// earlier runs).
#[must_use]
pub(crate) const fn call_plan(
    run_index: u64,
    call_every: u64,
    youngest_created_at: u64,
    now_unix: u64,
    ignore_account_age: bool,
) -> CallPlan {
    if call_every == 0 || run_index % call_every != 0 {
        return CallPlan::NotDue;
    }
    if !ignore_account_age && now_unix.saturating_sub(youngest_created_at) < YOUNG_ACCOUNT_SECS {
        return CallPlan::SkipYoung;
    }
    CallPlan::Run
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;

    #[test]
    fn every_nth_run_from_the_first() {
        let old = 0;
        let now = 10 * DAY;
        let plans: Vec<_> = (0..7).map(|i| call_plan(i, 3, old, now, false)).collect();
        assert_eq!(
            plans,
            [
                CallPlan::Run,
                CallPlan::NotDue,
                CallPlan::NotDue,
                CallPlan::Run,
                CallPlan::NotDue,
                CallPlan::NotDue,
                CallPlan::Run
            ]
        );
    }

    #[test]
    fn a_young_account_skips_a_due_call() {
        let now = 10 * DAY;
        let young = now - 24 * 3600;
        assert_eq!(call_plan(0, 1, young, now, false), CallPlan::SkipYoung);
        assert_eq!(
            call_plan(0, 1, now - YOUNG_ACCOUNT_SECS, now, false),
            CallPlan::Run
        );
        assert_eq!(call_plan(1, 2, young, now, false), CallPlan::NotDue);
        assert_eq!(call_plan(0, 1, young, now, true), CallPlan::Run);
        // A creation time in the future (clock skew) is young.
        assert_eq!(call_plan(0, 1, now + 5, now, false), CallPlan::SkipYoung);
    }

    #[test]
    fn call_every_zero_disables_the_call() {
        assert_eq!(call_plan(0, 0, 0, 10 * DAY, true), CallPlan::NotDue);
    }
}
