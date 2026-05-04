// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Mirror of `lakko-storage`: in-memory KV + AEAD wrapper + the
//! `MessageStore` and `GroupStateStore` facades. **No persistent
//! backend on Web yet** — `SqliteStore` is host-only (libsqlite C
//! source can't compile for `wasm32-unknown-unknown`); the
//! IndexedDB-backed `Store` impl arrives in 1.13c.
//!
//! For now Web clients use [`InMemoryStore`] (state lost on reload).
//! That's acceptable for the 1.16 apps/web skeleton (in-memory MLS
//! state is a known limitation documented in the UI), and 1.13c
//! upgrades it without touching the FFI surface — `MessageStore` and
//! friends only ever reach the underlying [`Store`] through the
//! trait, so swapping `InMemoryStore` for `IndexedDbStore` is a
//! one-line change.

use std::sync::Arc;

use lakko_storage::{
    EncryptedStore as RustEncryptedStore, GroupStateStore as RustGroupStateStore,
    InMemoryStore as RustInMemoryStore, LocalMessage as RustLocalMessage,
    MessageKind as RustMessageKind, MessageStore as RustMessageStore, StorageError, Store,
    Table as RustTable,
};
use wasm_bindgen::prelude::*;

fn storage_err(e: StorageError) -> JsError {
    JsError::new(&format!("{e}"))
}

const PSEUDONYM_LEN: usize = 32;
const MSG_ID_LEN: usize = 16;
const ROOT_KEY_LEN: usize = 32;

// ============================================================================
// Table
// ============================================================================

/// Logical bucket name. Mirror of [`lakko_storage::Table`].
#[wasm_bindgen]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Table {
    /// `MessageStore` rows.
    Messages,
    /// `GroupStateStore` rows.
    GroupState,
    /// Contacts cache.
    Contacts,
    /// Arbitrary single-row settings.
    Settings,
}

impl From<Table> for RustTable {
    fn from(t: Table) -> Self {
        match t {
            Table::Messages => RustTable::Messages,
            Table::GroupState => RustTable::GroupState,
            Table::Contacts => RustTable::Contacts,
            Table::Settings => RustTable::Settings,
        }
    }
}

// ============================================================================
// InMemoryStore
// ============================================================================

/// `Mutex<HashMap>` per-table store. Lives entirely in RAM; everything
/// is dropped when the page unloads.
///
/// Pass to [`EncryptedStore::new`] to wrap with AEAD encryption.
#[wasm_bindgen]
pub struct InMemoryStore {
    inner: Arc<RustInMemoryStore>,
}

#[wasm_bindgen]
impl InMemoryStore {
    /// Construct an empty store.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RustInMemoryStore::new()),
        }
    }

    /// Insert or replace `(table, key) → value`.
    #[wasm_bindgen]
    pub fn put(&self, table: Table, key: &[u8], value: &[u8]) -> Result<(), JsError> {
        self.inner
            .put(table.into(), key, value)
            .map_err(storage_err)
    }

    /// Fetch the value for `(table, key)`. Returns `None` if absent.
    #[wasm_bindgen]
    pub fn get(&self, table: Table, key: &[u8]) -> Result<Option<Vec<u8>>, JsError> {
        self.inner.get(table.into(), key).map_err(storage_err)
    }

    /// Delete the row. Idempotent.
    #[wasm_bindgen]
    pub fn delete(&self, table: Table, key: &[u8]) -> Result<(), JsError> {
        self.inner.delete(table.into(), key).map_err(storage_err)
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// EncryptedStore
// ============================================================================

/// AEAD wrapper around an `InMemoryStore`. Mirror of
/// [`lakko_storage::EncryptedStore`].
///
/// Phase 1.13b only takes [`InMemoryStore`] as the inner backend; the
/// IndexedDB-backed alternative arrives in 1.13c.
#[wasm_bindgen]
pub struct EncryptedStore {
    inner: Arc<RustEncryptedStore>,
}

#[wasm_bindgen]
impl EncryptedStore {
    /// Wrap an [`InMemoryStore`] with AEAD encryption keyed by
    /// `rootKey` (32 bytes).
    #[wasm_bindgen(constructor)]
    pub fn new(inner: &InMemoryStore, root_key: &[u8]) -> Result<EncryptedStore, JsError> {
        if root_key.len() != ROOT_KEY_LEN {
            return Err(JsError::new(&format!(
                "rootKey must be {ROOT_KEY_LEN} bytes",
            )));
        }
        let mut key = [0u8; ROOT_KEY_LEN];
        key.copy_from_slice(root_key);
        let backend: Arc<dyn Store> = inner.inner.clone();
        Ok(Self {
            inner: Arc::new(RustEncryptedStore::new(backend, key)),
        })
    }

    /// Encrypted put.
    #[wasm_bindgen]
    pub fn put(&self, table: Table, key: &[u8], value: &[u8]) -> Result<(), JsError> {
        self.inner
            .put(table.into(), key, value)
            .map_err(storage_err)
    }

    /// Encrypted get.
    #[wasm_bindgen]
    pub fn get(&self, table: Table, key: &[u8]) -> Result<Option<Vec<u8>>, JsError> {
        self.inner.get(table.into(), key).map_err(storage_err)
    }

    /// Delete the row.
    #[wasm_bindgen]
    pub fn delete(&self, table: Table, key: &[u8]) -> Result<(), JsError> {
        self.inner.delete(table.into(), key).map_err(storage_err)
    }
}

// ============================================================================
// MessageStore + LocalMessage
// ============================================================================

/// Body kind for a stored message. Mirror of
/// [`lakko_storage::MessageKind`].
#[wasm_bindgen]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MessageKind {
    /// UTF-8 text body.
    Text,
    /// JSON-encoded attachment manifest.
    Attachment,
}

impl From<MessageKind> for RustMessageKind {
    fn from(k: MessageKind) -> Self {
        match k {
            MessageKind::Text => RustMessageKind::Text,
            MessageKind::Attachment => RustMessageKind::Attachment,
        }
    }
}

impl From<RustMessageKind> for MessageKind {
    fn from(k: RustMessageKind) -> Self {
        match k {
            RustMessageKind::Text => MessageKind::Text,
            RustMessageKind::Attachment => MessageKind::Attachment,
        }
    }
}

/// One stored message ready for the UI list. Mirror of
/// [`lakko_storage::LocalMessage`].
#[wasm_bindgen]
pub struct LocalMessage {
    inner: RustLocalMessage,
}

#[wasm_bindgen]
impl LocalMessage {
    /// Construct a fresh `LocalMessage`. `msgId` must be 16 bytes,
    /// `senderPseudonym` must be 32 bytes.
    #[wasm_bindgen(constructor)]
    pub fn new(
        msg_id: &[u8],
        created_at_ms: u64,
        sender_pseudonym: &[u8],
        kind: MessageKind,
        body: &[u8],
    ) -> Result<LocalMessage, JsError> {
        if msg_id.len() != MSG_ID_LEN {
            return Err(JsError::new(&format!("msgId must be {MSG_ID_LEN} bytes")));
        }
        if sender_pseudonym.len() != PSEUDONYM_LEN {
            return Err(JsError::new(&format!(
                "senderPseudonym must be {PSEUDONYM_LEN} bytes",
            )));
        }
        let mut id = [0u8; MSG_ID_LEN];
        id.copy_from_slice(msg_id);
        let mut sender = [0u8; PSEUDONYM_LEN];
        sender.copy_from_slice(sender_pseudonym);
        Ok(Self {
            inner: RustLocalMessage {
                msg_id: id,
                created_at_ms,
                sender_pseudonym: sender,
                kind: kind.into(),
                body: body.to_vec(),
            },
        })
    }

    /// 16-byte message id.
    #[wasm_bindgen(getter, js_name = msgId)]
    pub fn msg_id(&self) -> Vec<u8> {
        self.inner.msg_id.to_vec()
    }

    /// Server-assigned creation timestamp (milliseconds since UNIX
    /// epoch).
    #[wasm_bindgen(getter, js_name = createdAtMs)]
    pub fn created_at_ms(&self) -> u64 {
        self.inner.created_at_ms
    }

    /// 32-byte sender pseudonym within the group.
    #[wasm_bindgen(getter, js_name = senderPseudonym)]
    pub fn sender_pseudonym(&self) -> Vec<u8> {
        self.inner.sender_pseudonym.to_vec()
    }

    /// Body kind.
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> MessageKind {
        self.inner.kind.into()
    }

    /// Raw body bytes.
    #[wasm_bindgen(getter)]
    pub fn body(&self) -> Vec<u8> {
        self.inner.body.clone()
    }
}

/// Per-group message log facade. Mirror of [`lakko_storage::MessageStore`].
#[wasm_bindgen]
pub struct MessageStore {
    inner: RustMessageStore,
}

#[wasm_bindgen]
impl MessageStore {
    /// Wrap an [`EncryptedStore`].
    #[wasm_bindgen(constructor)]
    pub fn new(encrypted: &EncryptedStore) -> Self {
        // EncryptedStore is owned-move; clone the Arc to share.
        let inner = clone_encrypted_store(&encrypted.inner);
        Self {
            inner: RustMessageStore::new(inner),
        }
    }

    /// Append a message to a group's local log.
    #[wasm_bindgen]
    pub fn append(&self, group_id: &[u8], msg: &LocalMessage) -> Result<(), JsError> {
        let g = decode_group(group_id)?;
        self.inner.append(&g, &msg.inner).map_err(storage_err)
    }

    /// Return messages strictly newer than `afterCreatedAtMs`,
    /// ordered ascending. Pass `0` to fetch the whole log.
    #[wasm_bindgen(js_name = listSince)]
    pub fn list_since(
        &self,
        group_id: &[u8],
        after_created_at_ms: u64,
        limit: usize,
    ) -> Result<JsValue, JsError> {
        let g = decode_group(group_id)?;
        let msgs = self
            .inner
            .list_since(&g, after_created_at_ms, limit)
            .map_err(storage_err)?;
        let arr = js_sys::Array::new_with_length(u32::try_from(msgs.len()).unwrap_or(u32::MAX));
        for (i, m) in msgs.into_iter().enumerate() {
            let wrapped = LocalMessage { inner: m };
            arr.set(u32::try_from(i).unwrap_or(u32::MAX), JsValue::from(wrapped));
        }
        Ok(arr.into())
    }

    /// How many messages stored locally for this group.
    #[wasm_bindgen]
    pub fn count(&self, group_id: &[u8]) -> Result<usize, JsError> {
        let g = decode_group(group_id)?;
        self.inner.count(&g).map_err(storage_err)
    }
}

/// Per-group MLS state facade. Mirror of
/// [`lakko_storage::GroupStateStore`].
#[wasm_bindgen]
pub struct GroupStateStore {
    inner: RustGroupStateStore,
}

#[wasm_bindgen]
impl GroupStateStore {
    /// Wrap an [`EncryptedStore`].
    #[wasm_bindgen(constructor)]
    pub fn new(encrypted: &EncryptedStore) -> Self {
        let inner = clone_encrypted_store(&encrypted.inner);
        Self {
            inner: RustGroupStateStore::new(inner),
        }
    }

    /// Save a serialised MLS state snapshot.
    #[wasm_bindgen]
    pub fn save(&self, group_id: &[u8], state: &[u8]) -> Result<(), JsError> {
        let g = decode_group(group_id)?;
        self.inner.save(&g, state).map_err(storage_err)
    }

    /// Load the most recent snapshot. Returns `None` for unknown groups.
    #[wasm_bindgen]
    pub fn load(&self, group_id: &[u8]) -> Result<Option<Vec<u8>>, JsError> {
        let g = decode_group(group_id)?;
        self.inner.load(&g).map_err(storage_err)
    }

    /// Drop the snapshot.
    #[wasm_bindgen]
    pub fn forget(&self, group_id: &[u8]) -> Result<(), JsError> {
        let g = decode_group(group_id)?;
        self.inner.forget(&g).map_err(storage_err)
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn decode_group(bytes: &[u8]) -> Result<[u8; PSEUDONYM_LEN], JsError> {
    if bytes.len() != PSEUDONYM_LEN {
        return Err(JsError::new(&format!(
            "groupId must be {PSEUDONYM_LEN} bytes",
        )));
    }
    let mut out = [0u8; PSEUDONYM_LEN];
    out.copy_from_slice(bytes);
    Ok(out)
}

/// Hand out an owned `RustEncryptedStore` that shares state with our
/// `Arc<RustEncryptedStore>`. `EncryptedStore` is `Clone` (every
/// inner field is `Arc`-wrapped), so this is cheap — the
/// `MessageStore` and `GroupStateStore` facades each own their own
/// `EncryptedStore` value but reach the same backend KV.
fn clone_encrypted_store(arc: &Arc<RustEncryptedStore>) -> RustEncryptedStore {
    (**arc).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root_key() -> [u8; 32] {
        [0xABu8; 32]
    }

    #[test]
    fn in_memory_put_get_roundtrip() {
        let s = InMemoryStore::new();
        s.put(Table::Messages, b"k", b"v").unwrap();
        assert_eq!(
            s.get(Table::Messages, b"k").unwrap().as_deref(),
            Some(&b"v"[..])
        );
        s.delete(Table::Messages, b"k").unwrap();
        assert!(s.get(Table::Messages, b"k").unwrap().is_none());
    }

    // Error-path validation tests live in `lakko-storage`. The
    // mirror layer calls `JsError::new`, which panics under host
    // `cargo test`.

    #[test]
    fn encrypted_store_roundtrip() {
        let s = InMemoryStore::new();
        let enc = EncryptedStore::new(&s, &root_key()).unwrap();
        enc.put(Table::Messages, b"k", b"plaintext").unwrap();
        assert_eq!(
            enc.get(Table::Messages, b"k").unwrap().as_deref(),
            Some(&b"plaintext"[..]),
        );
    }

    #[test]
    fn message_store_append_and_count() {
        let s = InMemoryStore::new();
        let enc = EncryptedStore::new(&s, &root_key()).unwrap();
        let store = MessageStore::new(&enc);

        let group = [0xAAu8; 32];
        let msg = LocalMessage::new(
            &[1u8; 16],
            1_000_000,
            &[0xCDu8; 32],
            MessageKind::Text,
            b"hi",
        )
        .unwrap();
        store.append(&group, &msg).unwrap();
        assert_eq!(store.count(&group).unwrap(), 1);
    }

    #[test]
    fn group_state_store_save_and_load() {
        let s = InMemoryStore::new();
        let enc = EncryptedStore::new(&s, &root_key()).unwrap();
        let store = GroupStateStore::new(&enc);

        let group = [0xAAu8; 32];
        store.save(&group, b"opaque snapshot").unwrap();
        assert_eq!(
            store.load(&group).unwrap().as_deref(),
            Some(&b"opaque snapshot"[..]),
        );
        store.forget(&group).unwrap();
        assert!(store.load(&group).unwrap().is_none());
    }

    #[test]
    fn local_message_construct_happy_path() {
        let m = LocalMessage::new(&[1u8; 16], 42, &[0xCD; 32], MessageKind::Text, b"hi").unwrap();
        assert_eq!(m.body(), b"hi".to_vec());
        assert_eq!(m.created_at_ms(), 42);
        assert_eq!(m.kind(), MessageKind::Text);
    }
}
