// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The file-backed blob index: one append-only log file, replayed into
//! memory at startup and rewritten compactly there and after each sweep.
//!
//! For development, tests and a single instance with no database. It cannot
//! coordinate two instances (each would replay its own copy).
//!
//! # Log format
//!
//! One record per line, space-separated, ASCII:
//!
//! ```text
//! b <id hex> <size> <node> <created_day> <last_access_day> <delete_pk hex | ->
//! t <id hex> <day>
//! d <id hex>
//! i <node> <day>
//! ```
//!
//! `b` writes a whole row, `t` raises a row's last access, `d` removes a
//! row, `i` marks a node's import done. Node names carry no whitespace
//! (checked at configuration). Row and removal records are flushed to disk
//! before the call returns; an access record is not, since losing one only
//! makes a blob look a day or so older.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;

use super::{BlobId, BlobIndex, BlobRow, IndexError, UploadOutcome};

/// The index state and its open log.
struct Inner {
    rows: HashMap<BlobId, BlobRow>,
    imported: HashMap<String, u32>,
    log: File,
}

/// The file-backed index.
pub(crate) struct FileIndex {
    path: PathBuf,
    inner: Mutex<Inner>,
}

fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    let v = hex::decode(s).ok()?;
    <[u8; 32]>::try_from(v.as_slice()).ok()
}

fn row_line(r: &BlobRow) -> String {
    let key = r.delete_pk.map_or_else(|| "-".to_string(), hex::encode);
    format!(
        "b {} {} {} {} {} {key}\n",
        hex::encode(r.id),
        r.size,
        r.node,
        r.created_day,
        r.last_access_day
    )
}

/// The rows and the per-node import days a replay rebuilds.
type Replayed = (HashMap<BlobId, BlobRow>, HashMap<String, u32>);

fn replay(path: &Path) -> Result<Replayed, IndexError> {
    let mut rows = HashMap::new();
    let mut imported = HashMap::new();
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((rows, imported)),
        Err(e) => return Err(e.into()),
    };
    for (n, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        let f: Vec<&str> = line.split(' ').collect();
        let bad = || IndexError::Corrupt(n + 1);
        match f.as_slice() {
            [] | [""] => {},
            ["b", id, size, node, created, access, key] => {
                let id = parse_hex32(id).ok_or_else(bad)?;
                let delete_pk = if *key == "-" {
                    None
                } else {
                    Some(parse_hex32(key).ok_or_else(bad)?)
                };
                rows.insert(
                    id,
                    BlobRow {
                        id,
                        size: size.parse().map_err(|_| bad())?,
                        node: (*node).to_string(),
                        created_day: created.parse().map_err(|_| bad())?,
                        last_access_day: access.parse().map_err(|_| bad())?,
                        delete_pk,
                    },
                );
            },
            ["t", id, day] => {
                let id = parse_hex32(id).ok_or_else(bad)?;
                let day: u32 = day.parse().map_err(|_| bad())?;
                if let Some(r) = rows.get_mut(&id) {
                    r.last_access_day = r.last_access_day.max(day);
                }
            },
            ["d", id] => {
                rows.remove(&parse_hex32(id).ok_or_else(bad)?);
            },
            ["i", node, day] => {
                imported.insert((*node).to_string(), day.parse().map_err(|_| bad())?);
            },
            _ => return Err(bad()),
        }
    }
    Ok((rows, imported))
}

/// Write the compact form of the state to `path` atomically and return the
/// log reopened for appending.
fn compact(
    path: &Path,
    rows: &HashMap<BlobId, BlobRow>,
    imported: &HashMap<String, u32>,
) -> Result<File, IndexError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .prefix(".lakko-index-")
        .tempfile_in(dir)?;
    {
        let w = tmp.as_file_mut();
        for (node, day) in imported {
            w.write_all(format!("i {node} {day}\n").as_bytes())?;
        }
        for r in rows.values() {
            w.write_all(row_line(r).as_bytes())?;
        }
        w.sync_data()?;
    }
    tmp.persist(path).map_err(|e| IndexError::Io(e.error))?;
    Ok(OpenOptions::new().append(true).open(path)?)
}

impl FileIndex {
    /// Open (or create) the index log at `path`, replay it and compact it.
    pub(crate) fn open(path: PathBuf) -> Result<Self, IndexError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let (rows, imported) = replay(&path)?;
        let log = compact(&path, &rows, &imported)?;
        Ok(Self {
            path,
            inner: Mutex::new(Inner {
                rows,
                imported,
                log,
            }),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // The state is only ever left consistent between statements; a
        // poisoned lock means a panic in a test, so recover the data.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Rewrite the log compactly (after a sweep removed many rows).
    pub(crate) fn compact_now(&self) -> Result<(), IndexError> {
        let mut g = self.lock();
        let log = compact(&self.path, &g.rows, &g.imported)?;
        g.log = log;
        Ok(())
    }
}

fn append(log: &mut File, line: &str, durable: bool) -> Result<(), IndexError> {
    log.write_all(line.as_bytes())?;
    if durable {
        log.sync_data()?;
    }
    Ok(())
}

#[async_trait]
impl BlobIndex for FileIndex {
    async fn get(&self, id: &BlobId) -> Result<Option<BlobRow>, IndexError> {
        Ok(self.lock().rows.get(id).cloned())
    }

    async fn record_upload(&self, row: BlobRow) -> Result<UploadOutcome, IndexError> {
        let mut g = self.lock();
        let inner = &mut *g;
        if let Some(existing) = inner.rows.get_mut(&row.id) {
            if existing.last_access_day < row.last_access_day {
                existing.last_access_day = row.last_access_day;
                let line = format!("t {} {}\n", hex::encode(row.id), row.last_access_day);
                append(&mut inner.log, &line, false)?;
            }
            return Ok(UploadOutcome::Existed);
        }
        append(&mut inner.log, &row_line(&row), true)?;
        inner.rows.insert(row.id, row);
        Ok(UploadOutcome::Created)
    }

    async fn touch(&self, ids: &[BlobId], day: u32) -> Result<(), IndexError> {
        let mut g = self.lock();
        let inner = &mut *g;
        for id in ids {
            if let Some(r) = inner.rows.get_mut(id) {
                if r.last_access_day < day {
                    r.last_access_day = day;
                    let line = format!("t {} {day}\n", hex::encode(id));
                    append(&mut inner.log, &line, false)?;
                }
            }
        }
        Ok(())
    }

    async fn remove(&self, id: &BlobId) -> Result<bool, IndexError> {
        let mut g = self.lock();
        let inner = &mut *g;
        if inner.rows.contains_key(id) {
            append(&mut inner.log, &format!("d {}\n", hex::encode(id)), true)?;
            inner.rows.remove(id);
            return Ok(true);
        }
        Ok(false)
    }

    async fn expired(
        &self,
        node: &str,
        cutoff_day: u32,
        limit: usize,
    ) -> Result<Vec<BlobId>, IndexError> {
        let g = self.lock();
        let mut found: Vec<(u32, BlobId)> = g
            .rows
            .values()
            .filter(|r| r.node == node && r.last_access_day < cutoff_day)
            .map(|r| (r.last_access_day, r.id))
            .collect();
        found.sort_unstable();
        Ok(found.into_iter().take(limit).map(|(_, id)| id).collect())
    }

    async fn remove_if_expired(&self, id: &BlobId, cutoff_day: u32) -> Result<bool, IndexError> {
        let mut g = self.lock();
        let inner = &mut *g;
        match inner.rows.get(id) {
            Some(r) if r.last_access_day < cutoff_day => {
                append(&mut inner.log, &format!("d {}\n", hex::encode(id)), true)?;
                inner.rows.remove(id);
                Ok(true)
            },
            _ => Ok(false),
        }
    }

    async fn import_done(&self, node: &str) -> Result<bool, IndexError> {
        Ok(self.lock().imported.contains_key(node))
    }

    async fn import(&self, rows: Vec<BlobRow>, node: &str, day: u32) -> Result<usize, IndexError> {
        let mut g = self.lock();
        let inner = &mut *g;
        let mut seen = HashSet::new();
        let mut inserted = 0;
        for r in rows {
            if inner.rows.contains_key(&r.id) || !seen.insert(r.id) {
                continue;
            }
            append(&mut inner.log, &row_line(&r), false)?;
            inner.rows.insert(r.id, r);
            inserted += 1;
        }
        append(&mut inner.log, &format!("i {node} {day}\n"), true)?;
        inner.imported.insert(node.to_string(), day);
        Ok(inserted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_file_index_meets_the_contract() {
        let dir = tempfile::tempdir().unwrap();
        let index = FileIndex::open(dir.path().join("idx/blobs.log")).unwrap();
        super::super::contract::run(&index, 1).await;
    }

    #[tokio::test]
    async fn the_file_index_survives_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blobs.log");
        let a = super::super::contract::row(1, 50, Some(3));
        let b = super::super::contract::row(2, 50, None);
        {
            let index = FileIndex::open(path.clone()).unwrap();
            index.record_upload(a.clone()).await.unwrap();
            index.record_upload(b.clone()).await.unwrap();
            index.touch(&[a.id], 60).await.unwrap();
            index.remove(&b.id).await.unwrap();
            index.import(vec![], "local", 50).await.unwrap();
        }
        let index = FileIndex::open(path).unwrap();
        let got = index.get(&a.id).await.unwrap().unwrap();
        assert_eq!(got.last_access_day, 60);
        assert_eq!(got.delete_pk, Some([3; 32]));
        assert_eq!(index.get(&b.id).await.unwrap(), None);
        assert!(index.import_done("local").await.unwrap());
    }

    #[test]
    fn a_corrupt_line_is_refused_not_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blobs.log");
        std::fs::write(&path, "b nothex 1 local 1 1 -\n").unwrap();
        assert!(matches!(FileIndex::open(path), Err(IndexError::Corrupt(1))));
    }
}
