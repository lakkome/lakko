// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Low-level key-value store abstraction and two backends.
//!
//! [`Store`] is the bedrock trait every higher-level facade
//! ([`crate::messages::MessageStore`], [`crate::groups::GroupStateStore`])
//! sits on top of. It is intentionally tiny:
//!
//!   * `put / get / delete / range` over `(table, key) → value`
//!   * synchronous (clients aren't tokio-bound; FFI prefers sync;
//!     async-needing callers wrap with `spawn_blocking`)
//!   * stores **opaque bytes** — encryption is one layer above (see
//!     [`crate::cipher::EncryptedStore`])
//!
//! Two implementations:
//!
//!   * [`InMemoryStore`] — `Mutex<HashMap>`, perfect for unit tests and
//!     ephemeral dev sessions.
//!   * [`SqliteStore`] — rusqlite with the `bundled` feature, so the
//!     binary embeds libsqlite and there's no system dependency to
//!     cross-compile against (iOS / Android / Tauri desktop / …).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

use crate::error::Result;

/// Logical bucket name. Tables are namespaces, not SQL tables — both
/// backends store everything in a single row-keyed-by-table-and-key
/// flat index. New tables are added by extending this enum.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Table {
    /// `MessageStore` rows: `key = group_id ‖ created_at_be ‖ msg_id`,
    /// `value = serialised LocalMessage`.
    Messages,
    /// `GroupStateStore` rows: `key = group_id`, `value = serialised
    /// MLS GroupContext`.
    GroupState,
    /// `ContactStore` rows: `key = identity_pubkey`, `value =
    /// serialised LocalContact`.
    Contacts,
    /// Arbitrary single-row settings; `key = "<setting_name>"`.
    Settings,
}

impl Table {
    /// Stable string used as the SQL table name AND as part of the
    /// AEAD AAD. Renaming a variant requires a migration.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Table::Messages => "lakko_messages",
            Table::GroupState => "lakko_group_state",
            Table::Contacts => "lakko_contacts",
            Table::Settings => "lakko_settings",
        }
    }

    /// All known tables. Used for schema bootstrap on `SqliteStore::open`.
    pub(crate) const ALL: &'static [Table] = &[
        Table::Messages,
        Table::GroupState,
        Table::Contacts,
        Table::Settings,
    ];
}

// ============================================================================
// Trait
// ============================================================================

/// Bedrock KV store. Implementations must be `Send + Sync` so the
/// store can live inside an `Arc<dyn Store>` and be shared across
/// threads (e.g. the UI thread reading while a background task writes).
pub trait Store: Send + Sync {
    /// Insert or replace `(table, key) → value`.
    fn put(&self, table: Table, key: &[u8], value: &[u8]) -> Result<()>;

    /// Fetch the value for `(table, key)`. Returns `Ok(None)` if the
    /// key doesn't exist (this is **not** an error).
    fn get(&self, table: Table, key: &[u8]) -> Result<Option<Vec<u8>>>;

    /// Delete the row, if any. Idempotent — deleting a missing key is
    /// not an error.
    fn delete(&self, table: Table, key: &[u8]) -> Result<()>;

    /// Return every `(key, value)` whose key starts with `prefix`,
    /// sorted ascending by raw byte order (the same order MariaDB and
    /// SQLite both use natively for `BLOB` columns).
    ///
    /// This is the building block for `MessageStore::list_after(...)`
    /// and similar range scans.
    fn range(&self, table: Table, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>>;
}

// ============================================================================
// InMemoryStore
// ============================================================================

/// `(table, key) → value` map for the in-memory backend.
type InMemoryTable = HashMap<Vec<u8>, Vec<u8>>;

/// `Mutex<HashMap>` per-table store. Lives entirely in RAM; everything
/// is dropped when the process exits.
#[derive(Default)]
pub struct InMemoryStore {
    inner: Mutex<HashMap<Table, InMemoryTable>>,
}

impl InMemoryStore {
    /// Construct an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Store for InMemoryStore {
    fn put(&self, table: Table, key: &[u8], value: &[u8]) -> Result<()> {
        let mut guard = self.inner.lock().expect("InMemoryStore mutex poisoned");
        guard
            .entry(table)
            .or_default()
            .insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn get(&self, table: Table, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let guard = self.inner.lock().expect("InMemoryStore mutex poisoned");
        Ok(guard.get(&table).and_then(|m| m.get(key)).cloned())
    }

    fn delete(&self, table: Table, key: &[u8]) -> Result<()> {
        let mut guard = self.inner.lock().expect("InMemoryStore mutex poisoned");
        if let Some(m) = guard.get_mut(&table) {
            m.remove(key);
        }
        Ok(())
    }

    fn range(&self, table: Table, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let guard = self.inner.lock().expect("InMemoryStore mutex poisoned");
        let Some(m) = guard.get(&table) else {
            return Ok(Vec::new());
        };
        let mut out: Vec<(Vec<u8>, Vec<u8>)> = m
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }
}

// ============================================================================
// SqliteStore
// ============================================================================

/// rusqlite-backed store. One SQLite table per [`Table`] variant —
/// schema is bootstrapped on [`SqliteStore::open`] / [`SqliteStore::open_in_memory`].
pub struct SqliteStore {
    /// Wrapped in `Mutex` because `rusqlite::Connection` is `!Sync`.
    /// Contention is fine — one writer + one reader is the common
    /// pattern, and SQLite itself serialises writes.
    conn: Mutex<Connection>,
}

impl SqliteStore {
    /// Open or create a SQLite database at `path`. If the schema is
    /// missing it is created.
    ///
    /// # Errors
    ///
    /// Any rusqlite open / DDL error.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::bootstrap(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open an in-memory database. Useful for tests and one-shot
    /// dev sessions that don't need to outlive the process.
    ///
    /// # Errors
    ///
    /// Any rusqlite open / DDL error.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::bootstrap(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn bootstrap(conn: &Connection) -> Result<()> {
        // PRAGMAs first: WAL keeps reads non-blocking against a writer;
        // foreign_keys is on by default in modern sqlite but explicit
        // is fine.
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; \
             PRAGMA synchronous = NORMAL; \
             PRAGMA foreign_keys = ON;",
        )?;
        for table in Table::ALL {
            // Each logical table is its own SQL table. Composite PK
            // could've been `(table_name TEXT, key BLOB)` in one
            // physical table, but a separate physical table per
            // logical bucket gives us free index isolation and lets
            // ops add table-specific PRAGMAs (e.g. `WITHOUT ROWID`)
            // later without migration drama.
            let stmt = format!(
                "CREATE TABLE IF NOT EXISTS \"{}\" (\
                    k BLOB NOT NULL PRIMARY KEY, \
                    v BLOB NOT NULL\
                ) WITHOUT ROWID;",
                table.name()
            );
            conn.execute_batch(&stmt)?;
        }
        Ok(())
    }
}

impl Store for SqliteStore {
    fn put(&self, table: Table, key: &[u8], value: &[u8]) -> Result<()> {
        let conn = self.conn.lock().expect("SqliteStore mutex poisoned");
        let stmt = format!(
            "INSERT OR REPLACE INTO \"{}\" (k, v) VALUES (?1, ?2)",
            table.name()
        );
        conn.execute(&stmt, rusqlite::params![key, value])?;
        Ok(())
    }

    fn get(&self, table: Table, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().expect("SqliteStore mutex poisoned");
        let stmt = format!("SELECT v FROM \"{}\" WHERE k = ?1", table.name());
        let mut q = conn.prepare(&stmt)?;
        let mut rows = q.query(rusqlite::params![key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get::<_, Vec<u8>>(0)?)),
            None => Ok(None),
        }
    }

    fn delete(&self, table: Table, key: &[u8]) -> Result<()> {
        let conn = self.conn.lock().expect("SqliteStore mutex poisoned");
        let stmt = format!("DELETE FROM \"{}\" WHERE k = ?1", table.name());
        conn.execute(&stmt, rusqlite::params![key])?;
        Ok(())
    }

    fn range(&self, table: Table, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let conn = self.conn.lock().expect("SqliteStore mutex poisoned");
        // SQLite has no native "key starts with" — but ranges over
        // BLOB are byte-ordered, so `prefix <= k < prefix_upper` is
        // exactly what we want, and it's index-friendly (uses the PK).
        if prefix.is_empty() {
            // No prefix → full scan in PK order.
            let stmt = format!("SELECT k, v FROM \"{}\" ORDER BY k ASC", table.name());
            let mut q = conn.prepare(&stmt)?;
            let mut out = Vec::new();
            let mut rows = q.query([])?;
            while let Some(row) = rows.next()? {
                out.push((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?));
            }
            return Ok(out);
        }
        let upper = next_byte_after(prefix);
        let stmt = match &upper {
            Some(_) => format!(
                "SELECT k, v FROM \"{}\" WHERE k >= ?1 AND k < ?2 ORDER BY k ASC",
                table.name()
            ),
            // No upper bound — `prefix` is `[0xFF, 0xFF, ...]`; just
            // anchor the lower bound and scan to end.
            None => format!(
                "SELECT k, v FROM \"{}\" WHERE k >= ?1 ORDER BY k ASC",
                table.name()
            ),
        };
        let mut q = conn.prepare(&stmt)?;
        let mut out = Vec::new();
        let mut rows = match upper.as_ref() {
            Some(u) => q.query(rusqlite::params![prefix, u.as_slice()])?,
            None => q.query(rusqlite::params![prefix])?,
        };
        while let Some(row) = rows.next()? {
            out.push((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?));
        }
        Ok(out)
    }
}

/// Compute the smallest byte string that's strictly greater than every
/// string starting with `prefix`. Returns `None` if no such string
/// exists in the byte universe (i.e. `prefix` is `[0xFF, 0xFF, ...]`).
fn next_byte_after(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut out = prefix.to_vec();
    while let Some(last) = out.last_mut() {
        if *last < 0xFF {
            *last += 1;
            return Some(out);
        }
        out.pop();
    }
    None
}

// ============================================================================
// Generic Store conformance tests — runs against any `impl Store`
// ============================================================================

#[cfg(test)]
pub(crate) fn run_store_conformance_suite<S: Store>(s: &S) {
    use Table::Messages;

    // put + get
    s.put(Messages, b"a", b"1").unwrap();
    assert_eq!(s.get(Messages, b"a").unwrap().as_deref(), Some(&b"1"[..]));

    // missing key
    assert!(s.get(Messages, b"missing").unwrap().is_none());

    // overwrite
    s.put(Messages, b"a", b"2").unwrap();
    assert_eq!(s.get(Messages, b"a").unwrap().as_deref(), Some(&b"2"[..]));

    // delete
    s.delete(Messages, b"a").unwrap();
    assert!(s.get(Messages, b"a").unwrap().is_none());

    // delete missing is OK
    s.delete(Messages, b"a").unwrap();

    // range over a populated table
    for k in [b"app/0", b"app/1", b"app/2", b"box/0"] {
        s.put(Messages, k, b"v").unwrap();
    }
    let rows = s.range(Messages, b"app/").unwrap();
    let keys: Vec<Vec<u8>> = rows.into_iter().map(|(k, _)| k).collect();
    assert_eq!(
        keys,
        vec![b"app/0".to_vec(), b"app/1".to_vec(), b"app/2".to_vec()],
    );

    // empty prefix == full scan, sorted
    let rows = s.range(Messages, b"").unwrap();
    let keys: Vec<Vec<u8>> = rows.into_iter().map(|(k, _)| k).collect();
    assert_eq!(
        keys,
        vec![
            b"app/0".to_vec(),
            b"app/1".to_vec(),
            b"app/2".to_vec(),
            b"box/0".to_vec(),
        ],
    );

    // table isolation: writes to Messages don't show up in Contacts
    let rows = s.range(Table::Contacts, b"").unwrap();
    assert!(rows.is_empty());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_passes_conformance() {
        run_store_conformance_suite(&InMemoryStore::new());
    }

    #[test]
    fn sqlite_in_memory_passes_conformance() {
        let s = SqliteStore::open_in_memory().expect("open in-memory sqlite");
        run_store_conformance_suite(&s);
    }

    #[test]
    fn sqlite_persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lakko.db");

        {
            let s = SqliteStore::open(&path).unwrap();
            s.put(Table::Settings, b"username", b"alice").unwrap();
        }

        let s = SqliteStore::open(&path).unwrap();
        assert_eq!(
            s.get(Table::Settings, b"username").unwrap().as_deref(),
            Some(&b"alice"[..]),
        );
    }

    #[test]
    fn next_byte_after_increments_last_byte() {
        assert_eq!(next_byte_after(b"abc"), Some(b"abd".to_vec()));
        assert_eq!(next_byte_after(b"a\xff"), Some(b"b".to_vec()));
        assert_eq!(next_byte_after(b"\xff"), None);
        assert_eq!(next_byte_after(b"\xff\xff"), None);
        assert_eq!(next_byte_after(b""), None);
    }
}
