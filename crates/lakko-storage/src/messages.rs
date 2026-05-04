// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Per-group message log on top of [`EncryptedStore`].
//!
//! Stores **decrypted** application-layer messages — i.e. what the
//! UI actually displays. The raw MLS ciphertext that arrived from
//! `services/ds` has already been decrypted; this module is the
//! offline-first source of truth for the UI message list.
//!
//! ## Key layout
//!
//! `key = group_id (32 bytes) ‖ created_at_be (8 bytes) ‖ msg_id (16 bytes)`
//!
//! Sorting raw bytes ascending matches `(created_at, msg_id)` order,
//! so [`Store::range`] over the per-group prefix returns messages in
//! send order. `created_at` is server-assigned (the journal `id` from
//! `services/ds` mapped to a millisecond timestamp on receipt) — using
//! it as the leading sort term aligns the local view with what every
//! other group member sees.
//!
//! ## Value layout
//!
//! Values are serialised manually with a fixed-shape header rather
//! than through `serde` to keep the on-disk format frozen and easy
//! to reason about during forensic inspection. Schema:
//!
//! ```text
//! u8       schema version (currently 1)
//! u8       kind (0=text, 1=attachment_pointer, ...)
//! u32be    sender_pseudonym length (always 32 for v1)
//! [..]     sender_pseudonym bytes
//! u32be    body length
//! [..]     body bytes
//! ```

use crate::cipher::EncryptedStore;
use crate::error::{Result, StorageError};
use crate::store::Table;

const SCHEMA_VERSION: u8 = 1;

/// Length of `group_id` and `sender_pseudonym` in bytes.
const PSEUDONYM_LEN: usize = 32;
/// Length of `created_at` (big-endian milliseconds since epoch).
const CREATED_AT_LEN: usize = 8;
/// Length of `msg_id` — local UUIDv7 raw bytes.
const MSG_ID_LEN: usize = 16;

/// What kind of body the message carries. Adding a variant requires
/// a `SCHEMA_VERSION` bump.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum MessageKind {
    /// UTF-8 text body.
    Text = 0,
    /// JSON-encoded attachment manifest (SHA-256 hash + AEAD key).
    /// The actual ciphertext blob lives in `services/storage`; the
    /// body here is the manifest the UI uses to download + decrypt.
    Attachment = 1,
}

impl MessageKind {
    const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(MessageKind::Text),
            1 => Some(MessageKind::Attachment),
            _ => None,
        }
    }
}

/// One decrypted message ready for the UI list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalMessage {
    /// Local UUIDv7 — sortable, embeds creation timestamp on the
    /// receiving device.
    pub msg_id: [u8; MSG_ID_LEN],
    /// Server-assigned `created_at` from `services/ds`, in milliseconds
    /// since epoch. Forms the leading sort key.
    pub created_at_ms: u64,
    /// Pseudonym of the sender within the group (per-relationship,
    /// not the global identity pubkey — see ADR-0005 §13.7).
    pub sender_pseudonym: [u8; PSEUDONYM_LEN],
    /// Body kind tag.
    pub kind: MessageKind,
    /// Raw body bytes — UTF-8 for [`MessageKind::Text`], JSON for
    /// [`MessageKind::Attachment`].
    pub body: Vec<u8>,
}

/// Per-group message log facade.
pub struct MessageStore {
    inner: EncryptedStore,
}

impl MessageStore {
    /// Wrap an existing `EncryptedStore`. Multiple high-level facades
    /// can share the same `EncryptedStore` — they're keyed by
    /// [`Table`].
    #[must_use]
    pub fn new(inner: EncryptedStore) -> Self {
        Self { inner }
    }

    /// Append a message to a group's local log. Idempotent on
    /// `(group_id, created_at_ms, msg_id)` — re-inserting the exact
    /// same row is a no-op overwrite.
    pub fn append(&self, group_id: &[u8; PSEUDONYM_LEN], msg: &LocalMessage) -> Result<()> {
        let key = build_key(group_id, msg.created_at_ms, &msg.msg_id);
        let value = encode(msg)?;
        self.inner.put(Table::Messages, &key, &value)
    }

    /// Return messages strictly newer than `after_created_at_ms`,
    /// ordered ascending. Pass `0` to fetch the whole log.
    pub fn list_since(
        &self,
        group_id: &[u8; PSEUDONYM_LEN],
        after_created_at_ms: u64,
        limit: usize,
    ) -> Result<Vec<LocalMessage>> {
        let rows = self.inner.range(Table::Messages, group_id)?;
        let mut out = Vec::with_capacity(rows.len().min(limit));
        for (k, v) in rows {
            let (_g, ts, _id) = parse_key(&k)?;
            if ts <= after_created_at_ms {
                continue;
            }
            out.push(decode(&v)?);
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    /// Delete a single message. Idempotent.
    pub fn delete(
        &self,
        group_id: &[u8; PSEUDONYM_LEN],
        created_at_ms: u64,
        msg_id: &[u8; MSG_ID_LEN],
    ) -> Result<()> {
        let key = build_key(group_id, created_at_ms, msg_id);
        self.inner.delete(Table::Messages, &key)
    }

    /// How many messages are stored locally for this group. Cheap on
    /// `InMemoryStore`, scans on `SqliteStore` — fine for the small
    /// per-group counts we deal with.
    pub fn count(&self, group_id: &[u8; PSEUDONYM_LEN]) -> Result<usize> {
        Ok(self.inner.range(Table::Messages, group_id)?.len())
    }
}

// ============================================================================
// Codec
// ============================================================================

const KEY_LEN: usize = PSEUDONYM_LEN + CREATED_AT_LEN + MSG_ID_LEN;

fn build_key(
    group_id: &[u8; PSEUDONYM_LEN],
    created_at_ms: u64,
    msg_id: &[u8; MSG_ID_LEN],
) -> [u8; KEY_LEN] {
    let mut key = [0u8; KEY_LEN];
    key[..PSEUDONYM_LEN].copy_from_slice(group_id);
    key[PSEUDONYM_LEN..PSEUDONYM_LEN + CREATED_AT_LEN]
        .copy_from_slice(&created_at_ms.to_be_bytes());
    key[PSEUDONYM_LEN + CREATED_AT_LEN..].copy_from_slice(msg_id);
    key
}

fn parse_key(k: &[u8]) -> Result<([u8; PSEUDONYM_LEN], u64, [u8; MSG_ID_LEN])> {
    if k.len() != KEY_LEN {
        return Err(StorageError::Codec(format!(
            "message key has wrong length (got {}, want {KEY_LEN})",
            k.len(),
        )));
    }
    let mut group = [0u8; PSEUDONYM_LEN];
    group.copy_from_slice(&k[..PSEUDONYM_LEN]);
    let mut ts_bytes = [0u8; CREATED_AT_LEN];
    ts_bytes.copy_from_slice(&k[PSEUDONYM_LEN..PSEUDONYM_LEN + CREATED_AT_LEN]);
    let ts = u64::from_be_bytes(ts_bytes);
    let mut msg = [0u8; MSG_ID_LEN];
    msg.copy_from_slice(&k[PSEUDONYM_LEN + CREATED_AT_LEN..]);
    Ok((group, ts, msg))
}

fn encode(m: &LocalMessage) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(2 + 4 + PSEUDONYM_LEN + 4 + m.body.len());
    out.push(SCHEMA_VERSION);
    out.push(m.kind as u8);
    out.extend_from_slice(
        &u32::try_from(m.sender_pseudonym.len())
            .unwrap()
            .to_be_bytes(),
    );
    out.extend_from_slice(&m.sender_pseudonym);
    let body_len = u32::try_from(m.body.len())
        .map_err(|_| StorageError::Codec("message body exceeds u32::MAX".to_string()))?;
    out.extend_from_slice(&body_len.to_be_bytes());
    out.extend_from_slice(&m.body);
    Ok(out)
}

fn decode(bytes: &[u8]) -> Result<LocalMessage> {
    let mut cur = bytes;
    let mut take = |n: usize| -> Result<&[u8]> {
        if cur.len() < n {
            return Err(StorageError::Codec(format!(
                "message value truncated (need {n} bytes, have {})",
                cur.len(),
            )));
        }
        let (head, rest) = cur.split_at(n);
        cur = rest;
        Ok(head)
    };

    let version = take(1)?[0];
    if version != SCHEMA_VERSION {
        return Err(StorageError::Codec(format!(
            "unsupported message schema version {version}",
        )));
    }
    let kind_byte = take(1)?[0];
    let kind = MessageKind::from_u8(kind_byte)
        .ok_or_else(|| StorageError::Codec(format!("unknown message kind {kind_byte}")))?;

    let sender_len = u32::from_be_bytes(take(4)?.try_into().unwrap()) as usize;
    if sender_len != PSEUDONYM_LEN {
        return Err(StorageError::Codec(format!(
            "v1 sender_pseudonym must be {PSEUDONYM_LEN} bytes, got {sender_len}",
        )));
    }
    let sender_bytes = take(PSEUDONYM_LEN)?;
    let mut sender_pseudonym = [0u8; PSEUDONYM_LEN];
    sender_pseudonym.copy_from_slice(sender_bytes);

    let body_len = u32::from_be_bytes(take(4)?.try_into().unwrap()) as usize;
    let body = take(body_len)?.to_vec();

    if !cur.is_empty() {
        return Err(StorageError::Codec(format!(
            "{} trailing bytes after message body",
            cur.len(),
        )));
    }

    Ok(LocalMessage {
        msg_id: [0u8; MSG_ID_LEN], // filled by caller from the storage key
        created_at_ms: 0,          // ditto
        sender_pseudonym,
        kind,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cipher::EncryptedStore;
    use crate::store::{InMemoryStore, Store};
    use std::sync::Arc;

    fn store() -> MessageStore {
        let inner: Arc<dyn Store> = Arc::new(InMemoryStore::new());
        let enc = EncryptedStore::new(inner, [0xABu8; 32]);
        MessageStore::new(enc)
    }

    fn sample(id_byte: u8, ts: u64) -> LocalMessage {
        LocalMessage {
            msg_id: [id_byte; MSG_ID_LEN],
            created_at_ms: ts,
            sender_pseudonym: [0xCD; PSEUDONYM_LEN],
            kind: MessageKind::Text,
            body: format!("hello {id_byte}").into_bytes(),
        }
    }

    #[test]
    fn append_and_count() {
        let s = store();
        let group = [0xAAu8; PSEUDONYM_LEN];
        s.append(&group, &sample(1, 1000)).unwrap();
        s.append(&group, &sample(2, 2000)).unwrap();
        assert_eq!(s.count(&group).unwrap(), 2);
    }

    #[test]
    fn list_since_filters_by_timestamp() {
        let s = store();
        let group = [0xAAu8; PSEUDONYM_LEN];
        s.append(&group, &sample(1, 1000)).unwrap();
        s.append(&group, &sample(2, 2000)).unwrap();
        s.append(&group, &sample(3, 3000)).unwrap();

        let got = s.list_since(&group, 1500, 100).unwrap();
        assert_eq!(got.len(), 2);
        // Sender pseudonym + body roundtrips; msg_id / created_at_ms
        // are not encoded inside the value so the decoded structs
        // have those fields zeroed.
        assert_eq!(got[0].body, b"hello 2");
        assert_eq!(got[1].body, b"hello 3");
    }

    #[test]
    fn list_since_respects_limit() {
        let s = store();
        let group = [0xAAu8; PSEUDONYM_LEN];
        for i in 1u8..=5 {
            s.append(&group, &sample(i, u64::from(i) * 1000)).unwrap();
        }
        let got = s.list_since(&group, 0, 3).unwrap();
        assert_eq!(got.len(), 3);
    }

    #[test]
    fn list_isolated_per_group() {
        let s = store();
        let g1 = [0x11u8; PSEUDONYM_LEN];
        let g2 = [0x22u8; PSEUDONYM_LEN];
        s.append(&g1, &sample(1, 1000)).unwrap();
        s.append(&g2, &sample(2, 2000)).unwrap();
        assert_eq!(s.count(&g1).unwrap(), 1);
        assert_eq!(s.count(&g2).unwrap(), 1);
        let got = s.list_since(&g1, 0, 100).unwrap();
        assert_eq!(got[0].body, b"hello 1");
    }

    #[test]
    fn delete_removes_message() {
        let s = store();
        let group = [0xAAu8; PSEUDONYM_LEN];
        let m = sample(7, 7000);
        s.append(&group, &m).unwrap();
        assert_eq!(s.count(&group).unwrap(), 1);
        s.delete(&group, m.created_at_ms, &m.msg_id).unwrap();
        assert_eq!(s.count(&group).unwrap(), 0);
    }

    #[test]
    fn codec_roundtrip_in_memory() {
        let original = LocalMessage {
            msg_id: [0u8; MSG_ID_LEN],
            created_at_ms: 0,
            sender_pseudonym: [0xCD; PSEUDONYM_LEN],
            kind: MessageKind::Attachment,
            body: b"{\"hash\":\"...\"}".to_vec(),
        };
        let bytes = encode(&original).unwrap();
        let back = decode(&bytes).unwrap();
        assert_eq!(back.kind, MessageKind::Attachment);
        assert_eq!(back.sender_pseudonym, original.sender_pseudonym);
        assert_eq!(back.body, original.body);
    }

    #[test]
    fn codec_rejects_unknown_version() {
        let mut bytes = encode(&sample(1, 1000)).unwrap();
        bytes[0] = 99;
        assert!(matches!(decode(&bytes), Err(StorageError::Codec(_))));
    }

    #[test]
    fn codec_rejects_truncated() {
        let bytes = encode(&sample(1, 1000)).unwrap();
        let truncated = &bytes[..bytes.len() - 1];
        assert!(matches!(decode(truncated), Err(StorageError::Codec(_))));
    }

    #[test]
    fn codec_rejects_trailing_bytes() {
        let mut bytes = encode(&sample(1, 1000)).unwrap();
        bytes.push(0x00);
        assert!(matches!(decode(&bytes), Err(StorageError::Codec(_))));
    }
}
