// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The lifetime sweep and the one-time import (ADR-0065 sections 1 and 2).
//!
//! - **Sweep.** A blob whose last access day is more than
//!   `LAKKO_STORAGE_BLOB_LIFETIME_DAYS` (default 365, the maintainer's
//!   number) back is deleted: its row first, only if it is still expired at
//!   that moment, then its bytes in the [`BlobStore`] (an object left
//!   behind is unreachable and is reported in the log). Runs at startup and then every
//!   `LAKKO_STORAGE_SWEEP_INTERVAL_SECS` (default one day).
//! - **Import.** On the first start with an index, every blob file already
//!   under `LAKKO_BLOB_DIR` becomes a row dated the import day, with no
//!   delete key. Only `<root>/<2 hex>/<2 hex>/<64 hex>` files with matching
//!   prefixes are taken: `backup/`, the index's own directory, temporary and
//!   probe files are skipped. It runs once per storage node.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::index::{today, BlobId, BlobIndex, BlobRow, IndexError};
use crate::store::BlobStore;

/// Ids taken from the index per sweep round. Chosen.
const SWEEP_BATCH: usize = 500;

/// Delete every blob on `node` past its life on `today`: its own idle days
/// when its upload set them (ADR-0087 section 13), else `lifetime_days`.
/// Returns how many were deleted.
pub(crate) async fn sweep_once(
    index: &dyn BlobIndex,
    store: &dyn BlobStore,
    node: &str,
    today: u32,
    lifetime_days: u32,
) -> Result<usize, IndexError> {
    let mut deleted = 0;
    loop {
        let ids = index
            .expired(node, today, lifetime_days, SWEEP_BATCH)
            .await?;
        if ids.is_empty() {
            break;
        }
        let mut progressed = false;
        for id in ids {
            if !index.remove_if_expired(&id, today, lifetime_days).await? {
                continue;
            }
            progressed = true;
            deleted += 1;
            if let Err(e) = store.delete(&id).await {
                tracing::warn!(error = %e, "sweep: delete expired bytes after the row");
            }
        }
        if !progressed {
            break;
        }
    }
    Ok(deleted)
}

/// Run [`sweep_once`] now and then every `interval`.
pub(crate) fn spawn_sweeper(
    index: Arc<dyn BlobIndex>,
    store: Arc<dyn BlobStore>,
    node: Arc<str>,
    lifetime_days: u32,
    interval: Duration,
    after_sweep: Option<Arc<dyn Fn() + Send + Sync>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        loop {
            tick.tick().await;
            match sweep_once(
                index.as_ref(),
                store.as_ref(),
                &node,
                today(),
                lifetime_days,
            )
            .await
            {
                Ok(0) => {},
                Ok(n) => {
                    tracing::info!(deleted = n, "blob lifetime sweep");
                    if let Some(f) = &after_sweep {
                        f();
                    }
                },
                Err(e) => tracing::warn!(error = %e, "blob lifetime sweep failed"),
            }
        }
    })
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// List the blob files under `root` that have the content-addressed layout.
pub(crate) fn list_blob_files(root: &Path) -> std::io::Result<Vec<(BlobId, u64)>> {
    let mut out = Vec::new();
    for a in std::fs::read_dir(root)? {
        let a = a?;
        let a_name = a.file_name().to_string_lossy().into_owned();
        if !is_hex(&a_name, 2) || !a.file_type()?.is_dir() {
            continue;
        }
        for b in std::fs::read_dir(a.path())? {
            let b = b?;
            let b_name = b.file_name().to_string_lossy().into_owned();
            if !is_hex(&b_name, 2) || !b.file_type()?.is_dir() {
                continue;
            }
            for f in std::fs::read_dir(b.path())? {
                let f = f?;
                let name = f.file_name().to_string_lossy().into_owned();
                if !is_hex(&name, 64)
                    || name[0..2] != a_name
                    || name[2..4] != b_name
                    || !f.file_type()?.is_file()
                {
                    continue;
                }
                let Ok(v) = hex::decode(&name) else { continue };
                let Ok(id) = BlobId::try_from(v.as_slice()) else {
                    continue;
                };
                out.push((id, f.metadata()?.len()));
            }
        }
    }
    Ok(out)
}

/// Import the blob files under `root` into the index once for `node`.
/// Returns `None` when the import had already run, else the rows inserted.
pub(crate) async fn import_once(
    index: &dyn BlobIndex,
    root: &Path,
    node: &str,
    day: u32,
) -> anyhow::Result<Option<usize>> {
    if index.import_done(node).await? {
        return Ok(None);
    }
    let owned = root.to_path_buf();
    let files = tokio::task::spawn_blocking(move || list_blob_files(&owned)).await??;
    let rows = files
        .into_iter()
        .map(|(id, size)| BlobRow {
            id,
            size,
            node: node.to_string(),
            created_day: day,
            last_access_day: day,
            delete_pk: None,
            idle_days: None,
        })
        .collect();
    Ok(Some(index.import(rows, node, day).await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::file::FileIndex;
    use crate::store::{blob_path, FsStore};

    fn put_file(root: &Path, id: &BlobId, bytes: &[u8]) {
        let p = blob_path(root, id);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }

    #[tokio::test]
    async fn the_sweep_deletes_only_blobs_past_their_lifetime() {
        let dir = tempfile::tempdir().unwrap();
        let index = FileIndex::open(dir.path().join(".lakko-index/blobs.log")).unwrap();
        let old = crate::index::contract::row(1, 1_000, Some(2));
        let edge = crate::index::contract::row(2, 1_000 + 35, None);
        let fresh = crate::index::contract::row(3, 1_300, None);
        for r in [&old, &edge, &fresh] {
            put_file(dir.path(), &r.id, b"x");
            index.record_upload(r.clone()).await.unwrap();
        }
        // Today 1400, lifetime 365: the cutoff is 1035. `edge` was last
        // accessed exactly 365 days back and is kept; `old` is 400 back.
        let n = sweep_once(
            &index,
            &FsStore::new(dir.path().to_path_buf()),
            "local",
            1_400,
            365,
        )
        .await
        .unwrap();
        assert_eq!(n, 1);
        assert_eq!(index.get(&old.id).await.unwrap(), None);
        assert!(!blob_path(dir.path(), &old.id).exists());
        assert!(index.get(&edge.id).await.unwrap().is_some());
        assert!(blob_path(dir.path(), &fresh.id).exists());
    }

    /// ADR-0087 section 13: a blob uploaded with an idle life of 45 days is
    /// swept 46 days after its last access, not 365.
    #[tokio::test]
    async fn the_sweep_honours_a_rows_own_idle_life() {
        let dir = tempfile::tempdir().unwrap();
        let index = FileIndex::open(dir.path().join(".lakko-index/blobs.log")).unwrap();
        let mut channel_file = crate::index::contract::row(4, 1_000, None);
        channel_file.idle_days = Some(45);
        let chat_file = crate::index::contract::row(5, 1_000, None);
        for r in [&channel_file, &chat_file] {
            put_file(dir.path(), &r.id, b"x");
            index.record_upload(r.clone()).await.unwrap();
        }
        assert_eq!(
            sweep_once(
                &index,
                &FsStore::new(dir.path().to_path_buf()),
                "local",
                1_045,
                365
            )
            .await
            .unwrap(),
            0
        );
        assert_eq!(
            sweep_once(
                &index,
                &FsStore::new(dir.path().to_path_buf()),
                "local",
                1_046,
                365
            )
            .await
            .unwrap(),
            1
        );
        assert_eq!(index.get(&channel_file.id).await.unwrap(), None);
        assert!(!blob_path(dir.path(), &channel_file.id).exists());
        assert!(index.get(&chat_file.id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn the_import_takes_blob_files_only_and_runs_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let id = [0xab; 32];
        put_file(root, &id, b"hello");
        // Things the import must skip.
        std::fs::create_dir_all(root.join("backup")).unwrap();
        std::fs::write(root.join("backup").join("00".repeat(16)), b"b").unwrap();
        std::fs::write(root.join(".lakko-storage-probe-x"), b"").unwrap();
        let parent = blob_path(root, &id);
        let parent = parent.parent().unwrap();
        std::fs::write(parent.join(".lakko-blob-tmp"), b"t").unwrap();
        // A hex name in the wrong shard directory.
        std::fs::write(parent.join("cd".repeat(32)), b"w").unwrap();

        let index = FileIndex::open(root.join(".lakko-index/blobs.log")).unwrap();
        assert_eq!(
            import_once(&index, root, "local", 20_000).await.unwrap(),
            Some(1)
        );
        let row = index.get(&id).await.unwrap().unwrap();
        assert_eq!(row.size, 5);
        assert_eq!(row.delete_pk, None);
        assert_eq!(row.created_day, 20_000);
        assert_eq!(
            import_once(&index, root, "local", 20_001).await.unwrap(),
            None
        );
    }
}
