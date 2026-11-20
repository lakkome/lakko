// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The storage service's own metrics ([ADR-0096] section 3).
//!
//! Every label value is a `&'static str` from a closed set in this file or
//! [`PassKind::name`]. Nothing here names a blob, a delete key, a pass or an
//! account; no series is per blob.
//!
//! **No stored-size gauge.** Knowing how much the index holds would take a
//! `COUNT` / `SUM` over the whole index, which at Lakko's scale is not
//! acceptable at boot or after every sweep. So the service counts the bytes
//! it adds and removes ([`BYTES_ADDED_TOTAL`], [`BYTES_REMOVED_TOTAL`]),
//! from which the dashboard reads the growth rate, and the absolute size
//! comes from node_exporter's filesystem metrics for the blob directory.
//!
//! [ADR-0096]: ../../../docs/adr/0096-service-observability.md

use lakko_metrics::metrics;

use crate::blob::ApiError;
use crate::pass::PassKind;

/// `PUT /blob/{hex}` answers, labelled `outcome`: `created` (a new row),
/// `existed` (a repeated upload), `refused` (a coded 4xx, or 503 without a
/// pass secret), `error` (500, or the row is on another node).
pub(crate) const UPLOADS_TOTAL: &str = "lakko_storage_uploads_total";

/// `GET /blob/{hex}` answers, labelled `outcome`: `ok`, `not_found`,
/// `refused`, `error`. `HEAD` is not counted here (it is not an access).
pub(crate) const DOWNLOADS_TOTAL: &str = "lakko_storage_downloads_total";

/// `POST /blob/keep` answers, one per request whatever its id count,
/// labelled `outcome`: `ok`, `refused`, `error`.
pub(crate) const KEEPS_TOTAL: &str = "lakko_storage_keeps_total";

/// `DELETE /blob/{hex}` answers, labelled `outcome`: `deleted`, `refused`
/// (every proof refusal is one), `error`.
pub(crate) const DELETES_TOTAL: &str = "lakko_storage_deletes_total";

/// Passes issued after a token was spent, labelled `kind` (`upload`,
/// `read`). Refused issuances are the anonymous-token spend counter's.
pub(crate) const PASSES_ISSUED_TOTAL: &str = "lakko_storage_passes_issued_total";

/// Blobs the lifetime sweep deleted (row and file).
pub(crate) const SWEEP_REMOVED_TOTAL: &str = "lakko_storage_sweep_removed_total";

/// Bytes of new blobs: an upload answered `created` adds its size.
pub(crate) const BYTES_ADDED_TOTAL: &str = "lakko_storage_bytes_added_total";

/// Bytes of blobs removed by an owner delete or a missing-file heal (the
/// row's size is in hand there). The sweep's removals are not in it: the
/// sweep deletes by id without the size, so it is counted in blobs only
/// ([`SWEEP_REMOVED_TOTAL`]).
pub(crate) const BYTES_REMOVED_TOTAL: &str = "lakko_storage_bytes_removed_total";

/// The background job name of the lifetime sweep.
pub(crate) const JOB_BLOB_SWEEP: &str = "blob_sweep";

/// Label names the storage metrics use beyond `lakko-metrics`' base set, and
/// `context` of the shared anonymous-token spend counter.
#[cfg(test)]
pub(crate) const EXTRA_LABELS: &[&str] = &["kind", "context"];

/// What one blob request came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// A download or a keep that was answered.
    Ok,
    /// An upload that wrote a new row.
    Created,
    /// An upload of a blob the index already held.
    Existed,
    /// A delete that removed the row.
    Deleted,
    /// A download of a blob with no row.
    NotFound,
    /// A coded refusal: no or a bad pass, a spent budget, a malformed
    /// request, a refused delete proof.
    Refused,
    /// An index or file failure, or a row on another node.
    Error,
}

impl Outcome {
    /// Every value, for the tests.
    #[cfg(test)]
    pub(crate) const ALL: [Self; 7] = [
        Self::Ok,
        Self::Created,
        Self::Existed,
        Self::Deleted,
        Self::NotFound,
        Self::Refused,
        Self::Error,
    ];

    /// The `outcome` label value.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Created => "created",
            Self::Existed => "existed",
            Self::Deleted => "deleted",
            Self::NotFound => "not_found",
            Self::Refused => "refused",
            Self::Error => "error",
        }
    }

    /// The outcome a refusal is counted under.
    pub(crate) const fn of_error(e: ApiError) -> Self {
        match e {
            ApiError::NotFound => Self::NotFound,
            ApiError::Internal | ApiError::NodeUnavailable => Self::Error,
            _ => Self::Refused,
        }
    }
}

/// Count one upload answer.
pub(crate) fn upload(outcome: Outcome) {
    metrics::counter!(UPLOADS_TOTAL, "outcome" => outcome.as_str()).increment(1);
}

/// Count one download answer.
pub(crate) fn download(outcome: Outcome) {
    metrics::counter!(DOWNLOADS_TOTAL, "outcome" => outcome.as_str()).increment(1);
}

/// Count one keep answer.
pub(crate) fn keep(outcome: Outcome) {
    metrics::counter!(KEEPS_TOTAL, "outcome" => outcome.as_str()).increment(1);
}

/// Count one delete answer.
pub(crate) fn delete(outcome: Outcome) {
    metrics::counter!(DELETES_TOTAL, "outcome" => outcome.as_str()).increment(1);
}

/// Count one issued pass.
pub(crate) fn pass_issued(kind: PassKind) {
    metrics::counter!(PASSES_ISSUED_TOTAL, "kind" => kind.name()).increment(1);
}

/// Count the blobs one sweep pass deleted.
pub(crate) fn sweep_removed(n: usize) {
    metrics::counter!(SWEEP_REMOVED_TOTAL).increment(u64::try_from(n).unwrap_or(u64::MAX));
}

/// A new row of `size` bytes was written.
pub(crate) fn bytes_added(size: u64) {
    metrics::counter!(BYTES_ADDED_TOTAL).increment(size);
}

/// A row of `size` bytes was removed outside the sweep.
pub(crate) fn bytes_removed(size: u64) {
    metrics::counter!(BYTES_REMOVED_TOTAL).increment(size);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

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
        let m = lakko_metrics::install("storage", "0.0.0").unwrap();
        for outcome in Outcome::ALL {
            upload(outcome);
            download(outcome);
            keep(outcome);
            delete(outcome);
        }
        for kind in [PassKind::Upload, PassKind::Read] {
            pass_issued(kind);
        }
        sweep_removed(3);
        bytes_added(10);
        bytes_removed(4);

        let text = m.render();
        lakko_metrics::exposition::assert_clean(&text, EXTRA_LABELS, &[]);
        for name in [UPLOADS_TOTAL, DOWNLOADS_TOTAL, KEEPS_TOTAL, DELETES_TOTAL] {
            for outcome in Outcome::ALL {
                assert!(
                    value_of(&text, name, &[("outcome", outcome.as_str())]) >= 1.0,
                    "{name} {}",
                    outcome.as_str()
                );
            }
        }
        for kind in ["upload", "read"] {
            assert!(value_of(&text, PASSES_ISSUED_TOTAL, &[("kind", kind)]) >= 1.0);
        }
        assert!(value_of(&text, SWEEP_REMOVED_TOTAL, &[]) >= 3.0);
        assert!(value_of(&text, BYTES_ADDED_TOTAL, &[]) >= 10.0);
        assert!(value_of(&text, BYTES_REMOVED_TOTAL, &[]) >= 4.0);
    }

    #[test]
    fn refusals_map_to_their_outcome() {
        assert_eq!(Outcome::of_error(ApiError::NotFound), Outcome::NotFound);
        assert_eq!(Outcome::of_error(ApiError::Internal), Outcome::Error);
        assert_eq!(Outcome::of_error(ApiError::NodeUnavailable), Outcome::Error);
        for e in [
            ApiError::PassRequired,
            ApiError::ReadBudget,
            ApiError::HashMismatch,
            ApiError::DeleteRefused,
            ApiError::PassDisabled,
        ] {
            assert_eq!(Outcome::of_error(e), Outcome::Refused);
        }
    }
}
