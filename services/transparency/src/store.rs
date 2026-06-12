// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Persistent wrapper around the crate's in-memory append-only log.
//!
//! [`LogStore`] holds a [`InMemoryLog`] behind a [`tokio::sync::Mutex`]
//! so concurrent appends serialize and the in-memory Merkle tree never
//! races itself. The DB ([`log_entries`]) is the durable source of
//! truth: on boot we replay every row in `idx` order to rebuild the
//! identical tree, and every append writes the new leaf row so the tree
//! survives a restart.
//!
//! ## Load-on-boot
//!
//! [`LogStore::load`] SELECTs all `log_entries` ORDER BY `idx`, appends
//! each into a fresh `InMemoryLog`, and **asserts** the index the log
//! returns equals the DB `idx`. A mismatch means the persisted rows are
//! not a contiguous 0-based sequence (a gap, a duplicate, or an
//! out-of-order write) — the Merkle tree would be silently forked from
//! what clients have already proven inclusion against, so we fail loud
//! rather than serve a divergent root.
//!
//! ## Append consistency model
//!
//! We must append in-memory FIRST to learn the index (the crate's
//! `append` allocates it), then INSERT the row carrying that exact
//! index. The two steps are therefore NOT atomic: if the in-memory
//! append succeeds but the DB INSERT fails, the in-memory tree has a
//! leaf the DB does not. That leaves the process in a state where its
//! live root commits to an entry that will VANISH on the next restart
//! (the replay won't see it). We treat that as fatal-ish: we log an
//! `error` and return the failure to the caller (→ HTTP 500). The
//! operator should restart the process, which re-syncs the in-memory
//! tree to the durable DB state. A future hardening could hold the row
//! INSERT inside the same lock as a write-ahead step, but the crate's
//! API hands the index out only on append, so first-write-to-DB is not
//! possible without duplicating the index-allocation logic here.

use lakko_transparency::{InMemoryLog, InclusionProof, LogEntry, SignedRoot};
use sqlx::MySqlPool;
use tokio::sync::Mutex;

use lakko_crypto::signing;

/// Errors the store surfaces to the HTTP layer.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StoreError {
    /// A database error during load or append.
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    /// The persisted rows are not a contiguous 0-based `idx` sequence —
    /// the log is corrupt and the tree cannot be faithfully rebuilt.
    #[error("log corruption: row at db idx {db_idx} mapped to log index {log_idx}")]
    Corrupt {
        /// The `idx` column value of the offending row.
        db_idx: u64,
        /// The index the in-memory log returned for it.
        log_idx: u64,
    },
    /// A wire error from the underlying crate (e.g. index out of bounds
    /// on a proof request).
    #[error("transparency log error: {0}")]
    Log(#[from] lakko_transparency::Error),
}

/// One replayed `log_entries` row: `(idx, username_hash,
/// identity_pubkey, version, is_reset)`. `is_reset` BOOLEAN comes back
/// as MariaDB `TINYINT(1)` → `i8`.
type LeafRow = (u64, Vec<u8>, Vec<u8>, u64, i8);

/// Persistent append-only transparency log.
pub(crate) struct LogStore {
    inner: Mutex<InMemoryLog>,
}

impl LogStore {
    /// Rebuild the log from the durable `log_entries` rows, signing
    /// future roots with `operator`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Db`] on a query failure, or
    /// [`StoreError::Corrupt`] if the rows are not a contiguous 0-based
    /// `idx` sequence (the replay would fork the tree).
    pub(crate) async fn load(
        pool: &MySqlPool,
        operator: signing::SigningKey,
    ) -> Result<Self, StoreError> {
        // Runtime (not compile-time-checked) query — the service builds
        // without a live DB, same posture as the discovery service.
        let rows: Vec<LeafRow> = sqlx::query_as(
            "SELECT idx, username_hash, identity_pubkey, version, is_reset \
             FROM log_entries ORDER BY idx ASC",
        )
        .fetch_all(pool)
        .await?;

        let mut log = InMemoryLog::new(operator);
        for (db_idx, username_hash, identity_pubkey, version, _is_reset) in rows {
            let entry = LogEntry {
                username_hash: clamp_hash(&username_hash),
                identity_pubkey,
                version,
            };
            let assigned = log.append(entry);
            if assigned != db_idx {
                // The replay must reconstruct the SAME index sequence the
                // rows were written under; otherwise the Merkle leaf
                // positions shift and the rebuilt root diverges.
                return Err(StoreError::Corrupt {
                    db_idx,
                    log_idx: assigned,
                });
            }
        }

        tracing::info!(size = log.size(), "transparency log rebuilt from DB");
        Ok(Self {
            inner: Mutex::new(log),
        })
    }

    /// Construct a store directly over an already-built log. Test +
    /// in-memory-only helper; production goes through [`LogStore::load`].
    #[cfg(test)]
    pub(crate) fn from_log(log: InMemoryLog) -> Self {
        Self {
            inner: Mutex::new(log),
        }
    }

    /// Append `entry` to the log and persist the leaf row.
    ///
    /// Appends in-memory FIRST to learn the index, then INSERTs the row
    /// with that exact index. See the module docs for the consistency
    /// model: a DB failure after the in-memory append is logged at
    /// `error` and returned (→ 500); the in-memory and durable state are
    /// then out of sync until a restart re-syncs them.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Db`] if the INSERT fails.
    pub(crate) async fn append(
        &self,
        pool: &MySqlPool,
        entry: LogEntry,
        is_reset: bool,
    ) -> Result<u64, StoreError> {
        let mut log = self.inner.lock().await;
        // Allocate the index by appending in-memory.
        let idx = log.append(entry.clone());

        // Persist the leaf. If this fails the in-memory tree now holds a
        // leaf the DB does not — fatal-ish, see module docs.
        let insert = sqlx::query(
            "INSERT INTO log_entries (idx, username_hash, identity_pubkey, version, is_reset) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(idx)
        .bind(&entry.username_hash[..])
        .bind(&entry.identity_pubkey[..])
        .bind(entry.version)
        .bind(is_reset)
        .execute(pool)
        .await;

        if let Err(e) = insert {
            tracing::error!(
                idx,
                error = %e,
                "FATAL: log_entries INSERT failed after in-memory append — \
                 in-memory tree and DB are now out of sync; restart to re-sync"
            );
            return Err(StoreError::Db(e));
        }

        Ok(idx)
    }

    /// Sign the current root with the operator key at `timestamp_unix`.
    pub(crate) async fn signed_root(&self, timestamp_unix: u64) -> SignedRoot {
        let log = self.inner.lock().await;
        log.sign_current_root(timestamp_unix)
    }

    /// Build an inclusion proof for the entry at `idx`, returning the
    /// proven entry alongside it (the leaf's canonical bytes are what a
    /// client feeds to `verify_inclusion`).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Log`] if `idx >= size()`.
    pub(crate) async fn proof(&self, idx: u64) -> Result<(LogEntry, InclusionProof), StoreError> {
        let log = self.inner.lock().await;
        let entry = log.entry(idx)?.clone();
        let proof = log.prove_inclusion(idx)?;
        Ok((entry, proof))
    }

    /// The operator's verifying key — clients pin this out-of-band.
    pub(crate) async fn operator_verifying_key(&self) -> signing::VerifyingKey {
        let log = self.inner.lock().await;
        log.operator_key()
    }

    /// Number of entries currently in the log.
    #[cfg(test)]
    pub(crate) async fn size(&self) -> u64 {
        let log = self.inner.lock().await;
        log.size()
    }
}

/// Coerce a stored 32-byte hash into the fixed array. A wrong length
/// here would be DB corruption (the column is `BINARY(32)`); we pad /
/// truncate defensively rather than panic, but in practice the column
/// constraint guarantees 32 bytes.
fn clamp_hash(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let n = bytes.len().min(32);
    out[..n].copy_from_slice(&bytes[..n]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_transparency::verify_inclusion;

    fn make_entry(name: &str, version: u64) -> LogEntry {
        LogEntry {
            username_hash: lakko_crypto::hash::blake3(name.as_bytes()),
            identity_pubkey: vec![0xCC; 32],
            version,
        }
    }

    /// Append several entries to an in-memory-only store (no DB),
    /// then prove inclusion of each against the signed root.
    #[tokio::test]
    async fn append_proof_verify_round_trip_no_db() {
        let operator = signing::SigningKey::generate();
        let mut log = InMemoryLog::new(operator);
        for i in 0..8 {
            log.append(make_entry(&format!("user-{i}"), 1));
        }
        let store = LogStore::from_log(log);

        let root = store.signed_root(1_700_000_000).await;
        root.verify(&store.operator_verifying_key().await)
            .expect("root verifies under operator key");

        for i in 0..8 {
            let (entry, proof) = store.proof(i).await.expect("proof");
            verify_inclusion(&entry, &proof, &root).expect("inclusion verifies");
        }
        assert_eq!(store.size().await, 8);
    }

    /// The proof for index i must NOT verify a different entry — the
    /// store delegates to the crate's binding so this is really a
    /// guard that we hand back the right (entry, proof) pair.
    #[tokio::test]
    async fn proof_is_bound_to_its_entry() {
        let operator = signing::SigningKey::generate();
        let mut log = InMemoryLog::new(operator);
        log.append(make_entry("alice", 1));
        log.append(make_entry("bob", 1));
        let store = LogStore::from_log(log);

        let root = store.signed_root(1_700_000_000).await;
        let (_alice_entry, alice_proof) = store.proof(0).await.unwrap();
        let (bob_entry, _bob_proof) = store.proof(1).await.unwrap();
        // Bob's entry against Alice's proof must fail.
        assert!(verify_inclusion(&bob_entry, &alice_proof, &root).is_err());
    }

    /// Out-of-bounds proof requests surface as a typed error, not a
    /// panic.
    #[tokio::test]
    async fn proof_out_of_bounds_errors() {
        let store = LogStore::from_log(InMemoryLog::new(signing::SigningKey::generate()));
        assert!(matches!(store.proof(0).await, Err(StoreError::Log(_))));
    }
}
