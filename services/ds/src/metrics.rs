// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The Delivery Service's own metrics (ADR-0096 section 3).
//!
//! Every label value below is a `&'static str` from a closed set in this
//! file. None names a journal, a mailbox, a directory, a key, a nonce or a
//! row: a metric labelled by any of them would be the social graph with
//! timestamps (invariant #5, ADR-0078 section 11). The handlers call the
//! helpers at the branch that decides, one line each.
//!
//! The uniform `401` a failed proof answers stays uniform on the wire; the
//! reason it failed is a closed set known only to this process, and it is
//! what [`proof_refused`] counts.

use lakko_metrics::metrics::{counter, gauge};

use crate::journal::Queue;

/// Rows a publish accepted, labelled `queue` (`group`, `mailbox`, `knock`,
/// `rendezvous`) and `outcome` (`stored`; `dropped` for a proofless write to
/// a closed mailbox, answered as stored and not kept). A commit row counts
/// as a `group` row.
pub(crate) const APPENDS_TOTAL: &str = "lakko_ds_appends_total";

/// Answers of `POST /ds/group/{hex}/commit` that the transaction decided,
/// labelled `outcome`: `appended` (201), `resent` (200, an identical
/// re-send), `conflict` (409, the epoch is taken or is not the next one).
pub(crate) const COMMITS_TOTAL: &str = "lakko_ds_commits_total";

/// The uniform `401 proof required`, labelled `reason`: see
/// [`ProofRefusal::as_str`] for the set.
pub(crate) const PROOF_REFUSALS_TOTAL: &str = "lakko_ds_proof_refusals_total";

/// Wakes a group publish or a commit asked for, labelled `outcome`: see
/// [`WakeOutcome::as_str`] for the set.
pub(crate) const WAKES_TOTAL: &str = "lakko_ds_wakes_total";

/// Long-poll reads waiting right now, labelled `queue` (`group`, `mailbox`,
/// `knock`, `rendezvous`). One series per queue kind, never per journal.
pub(crate) const LONG_POLLS_OPEN: &str = "lakko_ds_long_polls_open";

/// Rows the hourly pruner deleted, labelled `kind`: see [`Pruned::as_str`].
pub(crate) const PRUNED_ROWS_TOTAL: &str = "lakko_ds_pruned_rows_total";

/// The job name of the hourly pruner (`spawn_pruner` in `src/journal.rs`).
pub(crate) const JOB_PRUNER: &str = "ds_pruner";

/// The label names this module uses beyond the base set, for
/// `lakko_metrics::exposition::assert_clean`.
#[cfg(test)]
pub(crate) const EXTRA_LABELS: &[&str] = &["queue", "reason", "kind"];

/// The label value of a queue kind.
pub(crate) const fn queue_label(queue: Queue) -> &'static str {
    match queue {
        Queue::Group => "group",
        Queue::Mailbox => "mailbox",
        Queue::Knock => "knock",
        Queue::Rendezvous => "rendezvous",
    }
}

/// One row stored in `queue`.
pub(crate) fn appended(queue: Queue) {
    counter!(APPENDS_TOTAL, "queue" => queue_label(queue), "outcome" => "stored").increment(1);
}

/// A proofless write to a closed mailbox, answered as stored and dropped.
pub(crate) fn mailbox_write_dropped() {
    counter!(APPENDS_TOTAL, "queue" => "mailbox", "outcome" => "dropped").increment(1);
}

/// What the commit route answered after its transaction committed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommitOutcome {
    /// `201`: the epoch was registered and its row appended.
    Appended,
    /// `200`: an identical re-send, answered with the original row.
    Resent,
    /// `409`: the epoch is registered already or is not the next one.
    Conflict,
}

impl CommitOutcome {
    /// The label value.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Appended => "appended",
            Self::Resent => "resent",
            Self::Conflict => "conflict",
        }
    }
}

/// One answer of the commit route.
pub(crate) fn commit(outcome: CommitOutcome) {
    counter!(COMMITS_TOTAL, "outcome" => outcome.as_str()).increment(1);
}

/// Why a request got the uniform `401`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProofRefusal {
    /// No proof (or no delegation) header, or one that is not ASCII.
    Missing,
    /// A header or statement that did not parse.
    Malformed,
    /// The proof names an epoch with no key this request may use: not
    /// registered or retained, outside the write tier, not the fixed epoch
    /// of the route.
    WrongEpoch,
    /// There is no key the request could verify under: an unclaimed mailbox
    /// or directory, a reach key or delegation issuer that is not
    /// registered, a delegation for another mailbox or delegate.
    UnknownKey,
    /// The proof, statement or delegation is outside its time window.
    OutsideWindow,
    /// The signature does not verify.
    BadSignature,
    /// The nonce was spent already.
    Replay,
    /// A statement signer who is not heard on this kind of statement (not
    /// the root, not a registrar).
    NotHeard,
}

impl ProofRefusal {
    /// The label value.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Malformed => "malformed",
            Self::WrongEpoch => "wrong_epoch",
            Self::UnknownKey => "unknown_key",
            Self::OutsideWindow => "outside_window",
            Self::BadSignature => "bad_signature",
            Self::Replay => "replay",
            Self::NotHeard => "not_heard",
        }
    }

    /// The reason a request proof failed to verify.
    pub(crate) const fn of_proof(e: &lakko_ds_proof::ProofError) -> Self {
        match e {
            lakko_ds_proof::ProofError::Malformed | lakko_ds_proof::ProofError::UnknownVersion => {
                Self::Malformed
            },
            lakko_ds_proof::ProofError::OutsideWindow => Self::OutsideWindow,
            lakko_ds_proof::ProofError::BadSignature => Self::BadSignature,
        }
    }

    /// The reason a statement failed to verify.
    pub(crate) const fn of_statement(e: lakko_ds_proof::StatementError) -> Self {
        match e {
            lakko_ds_proof::StatementError::OutsideWindow => Self::OutsideWindow,
            lakko_ds_proof::StatementError::BadSignature => Self::BadSignature,
            lakko_ds_proof::StatementError::Malformed
            | lakko_ds_proof::StatementError::ListBounds
            | lakko_ds_proof::StatementError::DuplicateKey
            | lakko_ds_proof::StatementError::InvalidKey
            | lakko_ds_proof::StatementError::SignerListed => Self::Malformed,
        }
    }

    /// The reason a delegation, or the delegate's proof, failed.
    pub(crate) const fn of_delegation(e: &lakko_ds_proof::DelegationError) -> Self {
        match e {
            lakko_ds_proof::DelegationError::Malformed
            | lakko_ds_proof::DelegationError::UnknownVersion => Self::Malformed,
            lakko_ds_proof::DelegationError::WrongMailbox
            | lakko_ds_proof::DelegationError::NotTheDelegate => Self::UnknownKey,
            lakko_ds_proof::DelegationError::Expired
            | lakko_ds_proof::DelegationError::TtlTooLong => Self::OutsideWindow,
            lakko_ds_proof::DelegationError::BadSignature
            | lakko_ds_proof::DelegationError::BadProof => Self::BadSignature,
        }
    }
}

/// One uniform `401`.
pub(crate) fn proof_refused(reason: ProofRefusal) {
    counter!(PROOF_REFUSALS_TOTAL, "reason" => reason.as_str()).increment(1);
}

/// What became of a wake a publish asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WakeOutcome {
    /// The journal is unclaimed, so its publisher proved nothing: dropped.
    Unclaimed,
    /// Over the journal's wake cap: dropped.
    Capped,
    /// No push-gw is configured: dropped.
    Dormant,
    /// Every forward slot is in use: dropped.
    Busy,
    /// push-gw answered 2xx.
    Forwarded,
    /// push-gw answered something else (a wake it cannot open, a replay, its
    /// own per-group cap).
    Refused,
    /// The forward did not complete (connection, timeout).
    Failed,
}

impl WakeOutcome {
    /// The label value.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Unclaimed => "unclaimed",
            Self::Capped => "capped",
            Self::Dormant => "dormant",
            Self::Busy => "busy",
            Self::Forwarded => "forwarded",
            Self::Refused => "refused",
            Self::Failed => "failed",
        }
    }
}

/// One wake, by outcome.
pub(crate) fn wake(outcome: WakeOutcome) {
    counter!(WAKES_TOTAL, "outcome" => outcome.as_str()).increment(1);
}

/// A long poll on `queue` started waiting. The returned guard ends it when
/// dropped, however the wait ends (rows, deadline, error, a dropped request).
pub(crate) fn long_poll_open(queue: Queue) -> LongPollGuard {
    gauge!(LONG_POLLS_OPEN, "queue" => queue_label(queue)).increment(1.0);
    LongPollGuard { queue }
}

/// Holds one long poll in [`LONG_POLLS_OPEN`].
pub(crate) struct LongPollGuard {
    queue: Queue,
}

impl Drop for LongPollGuard {
    fn drop(&mut self) {
        gauge!(LONG_POLLS_OPEN, "queue" => queue_label(self.queue)).decrement(1.0);
    }
}

/// What the pruner deleted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pruned {
    /// Group journal rows past the retention.
    GroupMessages,
    /// Mailbox rows past the retention.
    MailboxMessages,
    /// Knock rows past the knock retention.
    Knocks,
    /// Rendezvous rows past the retention.
    RendezvousMessages,
    /// Spent proof nonces past their window.
    ProofNonces,
    /// Read-tier epoch keys past the retention.
    EpochKeys,
    /// Expired reach keys.
    ReachKeys,
    /// Nonces of expired delegations.
    DelegationNonces,
}

impl Pruned {
    /// The label value.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::GroupMessages => "group_messages",
            Self::MailboxMessages => "mailbox_messages",
            Self::Knocks => "knocks",
            Self::RendezvousMessages => "rendezvous_messages",
            Self::ProofNonces => "proof_nonces",
            Self::EpochKeys => "epoch_keys",
            Self::ReachKeys => "reach_keys",
            Self::DelegationNonces => "delegation_nonces",
        }
    }

    /// The journal rows of `queue`.
    pub(crate) const fn of_queue(queue: Queue) -> Self {
        match queue {
            Queue::Group => Self::GroupMessages,
            Queue::Mailbox => Self::MailboxMessages,
            Queue::Knock => Self::Knocks,
            Queue::Rendezvous => Self::RendezvousMessages,
        }
    }
}

/// `rows` rows of `kind` deleted by one pruner pass.
pub(crate) fn pruned(kind: Pruned, rows: u64) {
    counter!(PRUNED_ROWS_TOTAL, "kind" => kind.as_str()).increment(rows);
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUEUES: [Queue; 4] = [
        Queue::Group,
        Queue::Mailbox,
        Queue::Knock,
        Queue::Rendezvous,
    ];
    const COMMITS: [CommitOutcome; 3] = [
        CommitOutcome::Appended,
        CommitOutcome::Resent,
        CommitOutcome::Conflict,
    ];
    const REFUSALS: [ProofRefusal; 8] = [
        ProofRefusal::Missing,
        ProofRefusal::Malformed,
        ProofRefusal::WrongEpoch,
        ProofRefusal::UnknownKey,
        ProofRefusal::OutsideWindow,
        ProofRefusal::BadSignature,
        ProofRefusal::Replay,
        ProofRefusal::NotHeard,
    ];
    const WAKES: [WakeOutcome; 7] = [
        WakeOutcome::Unclaimed,
        WakeOutcome::Capped,
        WakeOutcome::Dormant,
        WakeOutcome::Busy,
        WakeOutcome::Forwarded,
        WakeOutcome::Refused,
        WakeOutcome::Failed,
    ];
    const PRUNED: [Pruned; 8] = [
        Pruned::GroupMessages,
        Pruned::MailboxMessages,
        Pruned::Knocks,
        Pruned::RendezvousMessages,
        Pruned::ProofNonces,
        Pruned::EpochKeys,
        Pruned::ReachKeys,
        Pruned::DelegationNonces,
    ];

    #[tokio::test]
    async fn every_helper_renders_clean_closed_set_series() {
        let metrics = lakko_metrics::install("ds", "0.0.0").unwrap();
        let mut guards = Vec::new();
        for queue in QUEUES {
            appended(queue);
            guards.push(long_poll_open(queue));
        }
        mailbox_write_dropped();
        for outcome in COMMITS {
            commit(outcome);
        }
        for reason in REFUSALS {
            proof_refused(reason);
        }
        for outcome in WAKES {
            wake(outcome);
        }
        for kind in PRUNED {
            pruned(kind, 3);
        }
        lakko_metrics::job_registered(JOB_PRUNER);
        lakko_metrics::job_succeeded(JOB_PRUNER);
        lakko_metrics::job_failed(JOB_PRUNER);

        let text = metrics.render();
        lakko_metrics::exposition::assert_clean(&text, EXTRA_LABELS, &[]);

        let has = |needle: &str| {
            assert!(
                text.lines().any(|l| l.starts_with(needle)),
                "missing `{needle}` in:\n{text}"
            );
        };
        for queue in QUEUES {
            let q = queue_label(queue);
            has(&format!(
                r#"{APPENDS_TOTAL}{{queue="{q}",outcome="stored"}}"#
            ));
            has(&format!(r#"{LONG_POLLS_OPEN}{{queue="{q}"}}"#));
        }
        has(&format!(
            r#"{APPENDS_TOTAL}{{queue="mailbox",outcome="dropped"}}"#
        ));
        for outcome in COMMITS {
            has(&format!(
                r#"{COMMITS_TOTAL}{{outcome="{}"}}"#,
                outcome.as_str()
            ));
        }
        for reason in REFUSALS {
            has(&format!(
                r#"{PROOF_REFUSALS_TOTAL}{{reason="{}"}}"#,
                reason.as_str()
            ));
        }
        for outcome in WAKES {
            has(&format!(
                r#"{WAKES_TOTAL}{{outcome="{}"}}"#,
                outcome.as_str()
            ));
        }
        for kind in PRUNED {
            has(&format!(
                r#"{PRUNED_ROWS_TOTAL}{{kind="{}"}}"#,
                kind.as_str()
            ));
        }
        has(r#"lakko_job_runs_total{task="ds_pruner",outcome="ok"}"#);
        has(r#"lakko_job_runs_total{task="ds_pruner",outcome="error"}"#);
        has(r#"lakko_job_last_success_timestamp_seconds{task="ds_pruner"}"#);

        // Dropping the guards takes each queue's gauge back down by one.
        drop(guards);
        let text = metrics.render();
        for queue in QUEUES {
            let q = queue_label(queue);
            let line = text
                .lines()
                .find(|l| l.starts_with(&format!(r#"{LONG_POLLS_OPEN}{{queue="{q}"}}"#)))
                .unwrap();
            let value: f64 = line.rsplit(' ').next().unwrap().parse().unwrap();
            assert!(value >= 0.0, "{line}");
        }
        for (kind, refusal) in [
            ProofRefusal::of_proof(&lakko_ds_proof::ProofError::OutsideWindow),
            ProofRefusal::of_statement(lakko_ds_proof::StatementError::BadSignature),
            ProofRefusal::of_delegation(&lakko_ds_proof::DelegationError::NotTheDelegate),
        ]
        .into_iter()
        .zip([
            ProofRefusal::OutsideWindow,
            ProofRefusal::BadSignature,
            ProofRefusal::UnknownKey,
        ]) {
            assert_eq!(kind, refusal);
        }
    }
}
