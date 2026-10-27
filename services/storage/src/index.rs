// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The storage index (ADR-0065 section 1, the index of ADR-0064 section 1).
//!
//! The index is the coordinator and the lookup; the files under
//! `LAKKO_BLOB_DIR` remain the truth for content. Every blob request is
//! answered from the index: a blob with no row does not exist for a client,
//! whatever lies on disk.
//!
//! Two implementations behind one trait, the `ChannelStore` pattern of
//! `services/channels`:
//!
//! - [`sql::SqlIndex`] over MariaDB (`LAKKO_STORAGE_DATABASE_URL`), what
//!   several instances would share;
//! - [`file::FileIndex`], an append-only log file beside the blobs replayed
//!   into memory at startup, for development, tests and a single instance
//!   with no database.
//!
//! The trait carries the blob rows only. The vault rows of ADR-0064 (one row
//! per vault: its write key, version counter and node; one row per object)
//! are meant to join it as further methods of the same trait and further
//! tables of the same database when ADR-0064 is built.
//!
//! # What a row holds
//!
//! Size, storage node, creation day, last access day and the delete public
//! key. Nothing names an account (invariant #5). Days are unix day numbers
//! (`unix_secs / 86400`), so nothing finer than the day is recorded about
//! when a file is read.

use async_trait::async_trait;

pub(crate) mod file;
pub(crate) mod sql;

/// A blob id: the SHA-256 of its ciphertext.
pub(crate) type BlobId = [u8; 32];

/// Seconds in a day, for [`day_of`].
const SECS_PER_DAY: u64 = 86_400;

/// The unix day number of `unix_secs`.
pub(crate) fn day_of(unix_secs: u64) -> u32 {
    u32::try_from(unix_secs / SECS_PER_DAY).unwrap_or(u32::MAX)
}

/// Today's unix day number by the system clock.
pub(crate) fn today() -> u32 {
    day_of(lakko_anon_token_server::now_unix_secs())
}

/// One blob's index row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlobRow {
    /// The blob id.
    pub(crate) id: BlobId,
    /// Ciphertext length in bytes.
    pub(crate) size: u64,
    /// The storage node holding the file.
    pub(crate) node: String,
    /// Day of the first upload (or of the import).
    pub(crate) created_day: u32,
    /// Day of the last download, keep or repeated upload.
    pub(crate) last_access_day: u32,
    /// The Ed25519 key a `DELETE` must be signed with; `None` for a row that
    /// cannot be deleted (an imported blob).
    pub(crate) delete_pk: Option<[u8; 32]>,
}

/// What [`BlobIndex::record_upload`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UploadOutcome {
    /// A new row was written.
    Created,
    /// A row already existed; its last access was moved to the upload day
    /// and its delete key left as it was (the first uploader's).
    Existed,
}

/// An index store failure. Callers answer 500 and do not honour the request.
#[derive(Debug, thiserror::Error)]
pub(crate) enum IndexError {
    /// The database failed.
    #[error("index database error: {0}")]
    Db(#[from] sqlx::Error),
    /// The index file failed.
    #[error("index file error: {0}")]
    Io(#[from] std::io::Error),
    /// The index file holds a line this build cannot read.
    #[error("index file corrupt at line {0}")]
    Corrupt(usize),
}

/// The blob index.
#[async_trait]
pub(crate) trait BlobIndex: Send + Sync {
    /// The row of `id`, if any.
    async fn get(&self, id: &BlobId) -> Result<Option<BlobRow>, IndexError>;

    /// Record a stored upload. A new row is written as `row`; an existing
    /// row keeps its delete key and creation day and has its last access
    /// raised to `row.last_access_day`.
    async fn record_upload(&self, row: BlobRow) -> Result<UploadOutcome, IndexError>;

    /// Raise the last access day of each existing id in `ids` to `day`. Ids
    /// without a row are ignored; a row already at `day` is not written.
    async fn touch(&self, ids: &[BlobId], day: u32) -> Result<(), IndexError>;

    /// Remove the row of `id`. Returns whether a row was removed.
    async fn remove(&self, id: &BlobId) -> Result<bool, IndexError>;

    /// Up to `limit` ids on `node` whose last access day is before
    /// `cutoff_day`.
    async fn expired(
        &self,
        node: &str,
        cutoff_day: u32,
        limit: usize,
    ) -> Result<Vec<BlobId>, IndexError>;

    /// Remove the row of `id` only if its last access day is still before
    /// `cutoff_day`, so a download that raced the sweep keeps the blob.
    /// Returns whether a row was removed.
    async fn remove_if_expired(&self, id: &BlobId, cutoff_day: u32) -> Result<bool, IndexError>;

    /// Whether the one-time import has run for `node`.
    async fn import_done(&self, node: &str) -> Result<bool, IndexError>;

    /// Insert `rows` where no row exists yet, then mark the import done for
    /// `node` on `day`. Returns how many rows were inserted.
    async fn import(&self, rows: Vec<BlobRow>, node: &str, day: u32) -> Result<usize, IndexError>;
}

#[cfg(test)]
pub(crate) mod contract {
    //! The behaviour both implementations must share, run against each.

    use super::{BlobIndex, BlobRow, UploadOutcome};

    pub(crate) fn row(seed: u8, day: u32, key: Option<u8>) -> BlobRow {
        BlobRow {
            id: [seed; 32],
            size: u64::from(seed) * 10,
            node: "local".to_string(),
            created_day: day,
            last_access_day: day,
            delete_pk: key.map(|k| [k; 32]),
        }
    }

    /// Exercise every method once. `base` offsets the ids so a persistent
    /// database can be reused across runs.
    pub(crate) async fn run(index: &dyn BlobIndex, base: u8) {
        let a = row(base, 100, Some(7));
        let b = row(base.wrapping_add(1), 100, None);
        let _ = index.remove(&a.id).await.unwrap();
        let _ = index.remove(&b.id).await.unwrap();

        assert_eq!(index.get(&a.id).await.unwrap(), None);
        assert_eq!(
            index.record_upload(a.clone()).await.unwrap(),
            UploadOutcome::Created
        );
        assert_eq!(index.get(&a.id).await.unwrap(), Some(a.clone()));

        // A re-upload keeps the first key and creation day, raises the access.
        let mut again = a.clone();
        again.created_day = 105;
        again.last_access_day = 105;
        again.delete_pk = Some([9; 32]);
        assert_eq!(
            index.record_upload(again).await.unwrap(),
            UploadOutcome::Existed
        );
        let got = index.get(&a.id).await.unwrap().unwrap();
        assert_eq!(got.delete_pk, Some([7; 32]));
        assert_eq!(got.created_day, 100);
        assert_eq!(got.last_access_day, 105);

        // Touch never lowers the access day and ignores unknown ids.
        index.touch(&[a.id, [0xEE; 32]], 103).await.unwrap();
        assert_eq!(
            index.get(&a.id).await.unwrap().unwrap().last_access_day,
            105
        );
        index.touch(&[a.id], 110).await.unwrap();
        assert_eq!(
            index.get(&a.id).await.unwrap().unwrap().last_access_day,
            110
        );

        index.record_upload(b.clone()).await.unwrap();
        let expired = index.expired("local", 101, 100).await.unwrap();
        assert!(expired.contains(&b.id));
        assert!(!expired.contains(&a.id));
        assert!(!index
            .expired("elsewhere", 101, 100)
            .await
            .unwrap()
            .contains(&b.id));

        // The conditional removal spares a row touched since.
        assert!(!index.remove_if_expired(&a.id, 101).await.unwrap());
        assert!(index.remove_if_expired(&b.id, 101).await.unwrap());
        assert_eq!(index.get(&b.id).await.unwrap(), None);

        assert!(index.remove(&a.id).await.unwrap());
        assert!(!index.remove(&a.id).await.unwrap());
    }
}
