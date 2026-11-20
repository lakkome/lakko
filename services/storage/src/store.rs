// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Where blob bytes rest (ADR-0065, amendment of 2026-11-19).
//!
//! Two backends behind one trait, chosen at startup:
//!
//! - [`FsStore`]: the local directory `LAKKO_BLOB_DIR`, laid out
//!   `<root>/<hex[0:2]>/<hex[2:4]>/<hex>` (development, a single small
//!   server);
//! - [`s3::S3Store`]: an S3-compatible bucket, when the
//!   `LAKKO_STORAGE_S3_*` variables are set (production: Hetzner Object
//!   Storage).
//!
//! The index, the passes, the routes and their statuses do not depend on the
//! backend; only where the bytes rest does. An upload is always streamed to a
//! local temporary file first and checked against its content address; the
//! store then takes that verified file ([`BlobStore::put_file`]). Account
//! backups ([`crate::backup`]) stay on the local disk under either backend.

use std::path::{Path, PathBuf};
use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;

use crate::index::BlobId;

pub(crate) mod s3;

/// A blob's bytes, streamed.
pub(crate) type BlobStream = Pin<Box<dyn Stream<Item = std::io::Result<Bytes>> + Send>>;

/// A failure of the store. The text never carries a URL, a key or a
/// signature: the S3 backend builds it from the status and the error kind.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StoreError {
    /// Local file I/O.
    #[error("store io: {0}")]
    Io(#[from] std::io::Error),
    /// The object store answered an unexpected status or could not be
    /// reached.
    #[error("object store: {0}")]
    Remote(String),
}

/// Where blob bytes rest.
#[async_trait]
pub(crate) trait BlobStore: Send + Sync {
    /// Short backend name for logs (`fs` / `s3`).
    fn name(&self) -> &'static str;

    /// The local directory an upload of `id` is streamed into before it is
    /// handed to [`Self::put_file`]. For [`FsStore`] it is the blob's own
    /// shard directory, so the final step is a rename on one filesystem.
    fn temp_dir(&self, id: &BlobId) -> PathBuf;

    /// Store the verified local file `temp` of `len` bytes as `id`. The
    /// temporary file is consumed: moved into place, or removed after the
    /// bucket write whether it succeeded or not.
    async fn put_file(
        &self,
        id: &BlobId,
        temp: tempfile::NamedTempFile,
        len: u64,
    ) -> Result<(), StoreError>;

    /// Open `id` for streaming: its length and its bytes, or `None` when it
    /// is not stored.
    async fn open(&self, id: &BlobId) -> Result<Option<(u64, BlobStream)>, StoreError>;

    /// The length of `id`, or `None` when it is not stored.
    async fn head(&self, id: &BlobId) -> Result<Option<u64>, StoreError>;

    /// Delete `id`. Deleting what is not stored succeeds.
    async fn delete(&self, id: &BlobId) -> Result<(), StoreError>;

    /// Whether the store can be used now (`/ready`).
    async fn ready(&self) -> Result<(), StoreError>;
}

/// Resolve `<root>/<hex[0:2]>/<hex[2:4]>/<hex>`.
pub(crate) fn blob_path(root: &Path, id: &BlobId) -> PathBuf {
    let hex = hex::encode(id);
    let mut p = root.to_path_buf();
    p.push(&hex[0..2]);
    p.push(&hex[2..4]);
    p.push(&hex);
    p
}

/// Probe that `dir` is writable by creating and deleting a uniquely-named
/// temp file.
pub(crate) async fn probe_writable(dir: &Path) -> std::io::Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    let probe = tempfile::Builder::new()
        .prefix(".lakko-storage-probe-")
        .tempfile_in(dir)?;
    drop(probe);
    Ok(())
}

/// Blob bytes as files under a local root.
pub(crate) struct FsStore {
    root: PathBuf,
}

impl FsStore {
    /// A store over `root`.
    pub(crate) const fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

#[async_trait]
impl BlobStore for FsStore {
    fn name(&self) -> &'static str {
        "fs"
    }

    fn temp_dir(&self, id: &BlobId) -> PathBuf {
        let path = blob_path(&self.root, id);
        path.parent()
            .map_or_else(|| self.root.clone(), Path::to_path_buf)
    }

    async fn put_file(
        &self,
        id: &BlobId,
        temp: tempfile::NamedTempFile,
        _len: u64,
    ) -> Result<(), StoreError> {
        let target = blob_path(&self.root, id);
        tokio::task::spawn_blocking(move || temp.persist(&target))
            .await
            .map_err(|_| StoreError::Remote("persist task failed".into()))?
            .map_err(|e| StoreError::Io(e.error))?;
        Ok(())
    }

    async fn open(&self, id: &BlobId) -> Result<Option<(u64, BlobStream)>, StoreError> {
        let file = match tokio::fs::File::open(blob_path(&self.root, id)).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let len = file.metadata().await?.len();
        let stream: BlobStream = Box::pin(crate::stream::ReaderStream::new(file));
        Ok(Some((len, stream)))
    }

    async fn head(&self, id: &BlobId) -> Result<Option<u64>, StoreError> {
        match tokio::fs::metadata(blob_path(&self.root, id)).await {
            Ok(m) => Ok(Some(m.len())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn delete(&self, id: &BlobId) -> Result<(), StoreError> {
        match tokio::fs::remove_file(blob_path(&self.root, id)).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    async fn ready(&self) -> Result<(), StoreError> {
        probe_writable(&self.root).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt as _;

    #[tokio::test]
    async fn the_file_store_round_trips_and_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsStore::new(dir.path().to_path_buf());
        let id = [0x5a; 32];
        assert!(store.open(&id).await.unwrap().is_none());
        assert_eq!(store.head(&id).await.unwrap(), None);
        let tdir = store.temp_dir(&id);
        std::fs::create_dir_all(&tdir).unwrap();
        let mut temp = tempfile::NamedTempFile::new_in(&tdir).unwrap();
        std::io::Write::write_all(&mut temp, b"hello").unwrap();
        store.put_file(&id, temp, 5).await.unwrap();
        assert_eq!(store.head(&id).await.unwrap(), Some(5));
        let (len, mut s) = store.open(&id).await.unwrap().unwrap();
        assert_eq!(len, 5);
        let mut got = Vec::new();
        while let Some(c) = s.next().await {
            got.extend_from_slice(&c.unwrap());
        }
        assert_eq!(got, b"hello");
        store.delete(&id).await.unwrap();
        store.delete(&id).await.unwrap();
        assert_eq!(store.head(&id).await.unwrap(), None);
    }
}
