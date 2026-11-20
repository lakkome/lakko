// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The free-space guard (`docs/ux/storage-full.md`).
//!
//! The server disk also holds MariaDB, the logs and the backup staging
//! directory, so uploads must never fill it. Before a blob or a backup is
//! written, the free space on the blob directory's filesystem is read; an
//! upload that would leave less than `LAKKO_STORAGE_MIN_FREE_BYTES` free is
//! refused with `507 {"error":"storage-full"}`, before the upload pass is
//! charged. Messages and calls do not touch this service, so they keep
//! working; only new attachments wait.
//!
//! The check is not a reservation: two uploads at once may both pass it.
//! The margin covers that (one upload is at most 50 MiB).

use std::path::Path;

/// Environment variable holding the free-space floor, in bytes.
pub(crate) const MIN_FREE_VAR: &str = "LAKKO_STORAGE_MIN_FREE_BYTES";

/// Default floor: 10 GiB. Chosen, not measured: room for MariaDB, the logs
/// and a night's backup dumps on an 80 GB server.
pub(crate) const DEFAULT_MIN_FREE_BYTES: u64 = 10 * 1024 * 1024 * 1024;

/// Bytes available to an unprivileged writer on the filesystem holding
/// `path`.
///
/// # Errors
///
/// When the filesystem cannot be queried.
pub(crate) fn free_bytes(path: &Path) -> std::io::Result<u64> {
    let st = rustix::fs::statvfs(path).map_err(std::io::Error::from)?;
    Ok(st.f_bavail.saturating_mul(st.f_frsize))
}

/// Whether an upload of `incoming` bytes leaves at least `min_free` free.
/// A floor of `0` admits everything.
pub(crate) const fn admits(free: u64, incoming: u64, min_free: u64) -> bool {
    min_free == 0 || free >= incoming.saturating_add(min_free)
}

/// Read the free space and decide. A filesystem that cannot be queried
/// refuses (logged): the guard exists so the disk never fills.
pub(crate) async fn check(root: &Path, incoming: u64, min_free: u64) -> bool {
    if min_free == 0 {
        return true;
    }
    let root = root.to_path_buf();
    match tokio::task::spawn_blocking(move || free_bytes(&root)).await {
        Ok(Ok(free)) => {
            let ok = admits(free, incoming, min_free);
            if !ok {
                tracing::warn!(free, min_free, "storage full: upload refused");
            }
            ok
        },
        Ok(Err(e)) => {
            tracing::error!(error = %e, "statvfs on the blob directory failed; upload refused");
            false
        },
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_floor_is_kept_after_the_upload() {
        assert!(admits(110, 10, 100));
        assert!(!admits(109, 10, 100));
        assert!(!admits(50, 0, 100));
    }

    #[test]
    fn a_zero_floor_admits_everything() {
        assert!(admits(0, u64::MAX, 0));
    }

    #[test]
    fn no_overflow_at_the_edges() {
        assert!(!admits(u64::MAX - 1, u64::MAX, 2));
    }

    #[test]
    fn the_real_filesystem_reports_free_space() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(free_bytes(dir.path()).expect("statvfs") > 0);
    }

    #[tokio::test]
    async fn an_impossible_floor_refuses_and_an_unreadable_path_refuses() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(!check(dir.path(), 1, u64::MAX).await);
        assert!(check(dir.path(), 1, 1).await);
        assert!(!check(Path::new("/no/such/lakko/dir"), 1, 1).await);
    }
}
