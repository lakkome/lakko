// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `BackupPayload` — the structured bytes sealed inside the
//! Phase 2a1 backup envelope.
//!
//! ## Wire format
//!
//! Hand-rolled length-prefixed binary codec. Same pattern
//! `lakko-contacts` uses for its wire types — keeps the parser
//! surface small and fuzz-friendly, and avoids pulling in a
//! schema dep (prost / serde-binary) that would force every
//! caller through a wider attack surface.
//!
//! Two versions are recognised on decode:
//!
//! ### v2 (current, Phase 2a3d1)
//!
//! ```text
//! u8           payload version (= 2)
//! [16 bytes]   account_id (UUIDv7 raw)
//! u64be        created_at_ms (the wall-clock at upload time)
//! [32 bytes]   ed25519_seed (the identity signing seed)
//! u32be        accepted_contacts_count
//! [..]         accepted_contacts_count × AcceptedContactRecord
//! u32be        mls_snapshots_count (per-group; reserved, emit 0)
//! [..]         mls_snapshots_count × { 32-byte group_id, u32be length, bytes }
//! u32be        whole_storage_snapshot_length
//! [..]         whole_storage_snapshot_length bytes
//! ```
//!
//! ### v1 (Phase 2a1; accepted on decode for forward compat)
//!
//! Same as v2, minus the trailing `whole_storage_snapshot`
//! length + bytes. Decoder folds v1 → `whole_storage_snapshot:
//! Vec::new()` so the inner type carries one shape regardless
//! of which version was on the wire. v1 producers are
//! Phase 2a1 clients that pre-date the snapshot work — they
//! still upload, the restore path on a newer client simply
//! has nothing to feed into `Identity::restore_storage`.
//!
//! Each [`AcceptedContactRecord`]:
//!
//! ```text
//! [32 bytes]   ed25519_pubkey
//! [32 bytes]   x25519_pubkey
//! u16be        display_name length (UTF-8 bytes, ≤ MAX_DISPLAY_NAME_LEN)
//! [..]         display_name bytes
//! u8           account_id presence (v3+ only): 0 = absent, 1 = present
//! [16 bytes]   account_id (only when the presence byte is 1)
//! ```
//!
//! The trailing `account_id` (ADR-0021) is written only by v3+ producers;
//! v1/v2 records end after the display name and decode to `None`.
//!
//! ## What's stored, what's not
//!
//! Stored:
//!   - `account_id` — needed to bind the blob to the right
//!     server-side row on restore.
//!   - `created_at_ms` — for staleness display + UI consistency.
//!   - `ed25519_seed` — the identity-signing seed. From this the
//!     client re-derives Ed25519 + X25519 keypairs deterministically
//!     (the same `Identity::from_seed` flow Phase 1 already uses).
//!   - `accepted_contacts` — the local address book. Public bytes
//!     only (pubkeys + display name); no secret material.
//!   - `mls_snapshots` — per-group OpenMLS provider state. Reserved
//!     in v1 — written as zero entries until 2a3 wires the
//!     DeviceGroup-side snapshot publishing.
//!
//! Not stored (intentional):
//!   - Message history. Flows through the 2a3d DeviceGroup
//!     catch-up bundle path. Backup envelope is "small,
//!     frequent-write, fast-restore"; history is "large,
//!     append-only, syncs over MLS-encrypted channels".
//!   - Per-device session tokens. Each device re-authenticates
//!     after restore.
//!   - X25519 secret separately. Derived from `ed25519_seed`
//!     via the existing `Identity::from_seed` flow; no need to
//!     duplicate-store.

use crate::error::{BackupError, Result};

/// Current payload schema version emitted on encode. v3 adds an
/// optional per-contact `account_id` (ADR-0021) after each contact's
/// display name, so a restored contact can be matched to an SFU
/// group-call roster tag. v1/v2 backups (no account_id) still decode.
pub const PAYLOAD_VERSION: u8 = 4;

/// ADR-0021 schema — per-contact account_id, no known-group tail. Accepted on
/// decode; `known_group_ids` maps to an empty `Vec`.
const PAYLOAD_VERSION_V3: u8 = 3;

/// Phase 2a3d1 schema — whole-storage snapshot tail, no per-contact
/// account_id. Accepted on decode; contacts map to `account_id: None`.
const PAYLOAD_VERSION_V2: u8 = 2;

/// Phase 2a1 schema. Accepted on decode for forward
/// compatibility; mapped to a v2-shaped `BackupPayload` with
/// `whole_storage_snapshot: Vec::new()`.
const PAYLOAD_VERSION_V1: u8 = 1;

/// Account-id length (UUIDv7 raw, mirrors
/// `lakko_contacts::ACCOUNT_ID_LEN`).
const ACCOUNT_ID_LEN: usize = 16;

/// Identity signing-seed length (Ed25519 seed, mirrors
/// `lakko_crypto::signing::SECRET_KEY_LEN`).
const SEED_LEN: usize = 32;

/// Per-pubkey length in the contact records — Ed25519 verifying
/// key and X25519 public key both 32 bytes.
const PUBKEY_LEN: usize = 32;

/// Hard cap on per-contact display-name UTF-8 bytes. Keeps the
/// inner codec's worst-case payload bounded for fuzzing.
pub const MAX_DISPLAY_NAME_LEN: usize = 256;

/// Hard cap on one MLS `group_id`. Lakko mints 32-byte ids; the cap is loose
/// enough to outlive that choice and tight enough that a malformed payload
/// cannot ask for an unbounded read.
pub const MAX_GROUP_ID_LEN: usize = 128;

/// One row in the local "accepted contacts" address book.
/// Mirrors the public fields the web client's `AcceptedContact`
/// type holds — see `apps/web/src/contacts/local-store.ts`. Only
/// public bytes; no secret material.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedContactRecord {
    /// Sender's Ed25519 identity public key.
    pub ed25519_pubkey: [u8; PUBKEY_LEN],
    /// Sender's X25519 HPKE public key. Empty / zeroed if not
    /// known yet (legacy v1 ContactRequest); the receive path
    /// surfaces it as the sealing key for future replies.
    pub x25519_pubkey: [u8; PUBKEY_LEN],
    /// User-chosen display name (UTF-8). Empty string is valid;
    /// `MAX_DISPLAY_NAME_LEN` bytes max.
    pub display_name: String,
    /// Sender's 16-byte account_id (ADR-0021), when known. Persisted
    /// from v3; `None` for a contact restored from a v1/v2 backup or
    /// accepted before the account_id was learned. Lets a restored
    /// contact resolve to an SFU group-call roster `peer_tag`.
    pub account_id: Option<[u8; ACCOUNT_ID_LEN]>,
}

/// The structured state we want to roundtrip through the
/// backup envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupPayload {
    /// Owner of this backup. Bound to the server-side row.
    pub account_id: [u8; ACCOUNT_ID_LEN],
    /// Wall-clock UNIX millis at backup-creation time.
    pub created_at_ms: u64,
    /// The identity signing seed — re-derive Ed25519 + X25519
    /// keypairs on restore via the existing `Identity::from_seed`
    /// flow.
    pub ed25519_seed: [u8; SEED_LEN],
    /// Local "accepted contacts" address book.
    pub accepted_contacts: Vec<AcceptedContactRecord>,
    /// Per-group OpenMLS provider snapshots. Reserved — kept
    /// empty in the current encoder for when OpenMLS exposes a
    /// per-group export surface. Each entry is
    /// `(group_id, raw_snapshot_bytes)`.
    pub mls_snapshots: Vec<(Vec<u8>, Vec<u8>)>,
    /// Whole-storage OpenMLS provider snapshot (Phase 2a3d1).
    ///
    /// Same bytes [`lakko_mls::Identity::snapshot_storage`]
    /// emits — every group + ratchet tree + signing-key row in
    /// one length-prefixed blob. On restore the client's new
    /// `Identity::restore_storage(...)` replaces the freshly-
    /// minted provider with this snapshot so a paired device
    /// lands with the same group state the upload-side device
    /// had at backup time.
    ///
    /// Empty `Vec` when no snapshot was published yet. v1-shaped
    /// payloads decode to `Vec::new()` here.
    pub whole_storage_snapshot: Vec<u8>,
    /// The MLS `group_id`s this device knew about at backup time.
    ///
    /// Carried because [`Self::whole_storage_snapshot`] is not enough on its
    /// own: it restores the provider, which holds every group, but the
    /// provider offers no way to enumerate what is in it, so a client's chat
    /// list is built from a separately persisted index of ids. A restore that
    /// brought back only the snapshot therefore landed a device holding every
    /// group and showing none of them.
    ///
    /// Empty `Vec` for a v1, v2 or v3 payload, and for a device that knew no
    /// groups - the two are indistinguishable here, and neither is a reason to
    /// fail a restore.
    pub known_group_ids: Vec<Vec<u8>>,
}

impl BackupPayload {
    /// Encode the payload to the on-the-wire bytes that go into
    /// the AEAD plaintext of [`crate::seal`].
    ///
    /// # Errors
    ///
    /// * [`BackupError::InvalidPayload`] when a per-record length
    ///   would overflow its width (display_name > u16::MAX bytes;
    ///   contact / snapshot count > u32::MAX; snapshot bytes >
    ///   u32::MAX bytes). All unreachable in practice.
    pub fn encode(&self) -> Result<Vec<u8>> {
        // Pre-size estimate keeps reallocs cheap. Display names
        // and snapshot blobs are variable; we round-down to a
        // sensible starting capacity.
        let mut out = Vec::with_capacity(
            1 + ACCOUNT_ID_LEN + 8 + SEED_LEN + 4 + (self.accepted_contacts.len() * 72) + 4,
        );

        out.push(PAYLOAD_VERSION);
        out.extend_from_slice(&self.account_id);
        out.extend_from_slice(&self.created_at_ms.to_be_bytes());
        out.extend_from_slice(&self.ed25519_seed);

        let contact_count = u32::try_from(self.accepted_contacts.len()).map_err(|_| {
            BackupError::InvalidPayload(format!(
                "accepted_contacts count {} overflows u32",
                self.accepted_contacts.len(),
            ))
        })?;
        out.extend_from_slice(&contact_count.to_be_bytes());
        for c in &self.accepted_contacts {
            encode_contact(&mut out, c)?;
        }

        let snapshot_count = u32::try_from(self.mls_snapshots.len()).map_err(|_| {
            BackupError::InvalidPayload(format!(
                "mls_snapshots count {} overflows u32",
                self.mls_snapshots.len(),
            ))
        })?;
        out.extend_from_slice(&snapshot_count.to_be_bytes());
        for (group_id, snapshot) in &self.mls_snapshots {
            encode_snapshot(&mut out, group_id, snapshot)?;
        }

        // Phase 2a3d1 — whole-storage snapshot (v2 tail).
        let snapshot_len = u32::try_from(self.whole_storage_snapshot.len()).map_err(|_| {
            BackupError::InvalidPayload(format!(
                "whole_storage_snapshot length {} overflows u32",
                self.whole_storage_snapshot.len(),
            ))
        })?;
        out.extend_from_slice(&snapshot_len.to_be_bytes());
        out.extend_from_slice(&self.whole_storage_snapshot);

        // v4 tail — the group index the snapshot cannot supply.
        let group_count = u32::try_from(self.known_group_ids.len()).map_err(|_| {
            BackupError::InvalidPayload(format!(
                "known_group_ids count {} overflows u32",
                self.known_group_ids.len(),
            ))
        })?;
        out.extend_from_slice(&group_count.to_be_bytes());
        for id in &self.known_group_ids {
            if id.len() > MAX_GROUP_ID_LEN {
                return Err(BackupError::InvalidPayload(format!(
                    "group_id length {} exceeds MAX_GROUP_ID_LEN {MAX_GROUP_ID_LEN}",
                    id.len(),
                )));
            }
            let id_len = u32::try_from(id.len()).map_err(|_| {
                BackupError::InvalidPayload(format!("group_id length {} overflows u32", id.len()))
            })?;
            out.extend_from_slice(&id_len.to_be_bytes());
            out.extend_from_slice(id);
        }

        Ok(out)
    }

    /// Decode wire bytes back into a `BackupPayload`.
    ///
    /// # Errors
    ///
    /// * [`BackupError::InvalidPayload`] when the wire bytes are
    ///   malformed (wrong version, truncated, length-prefix
    ///   overflow, trailing bytes after the structured tail,
    ///   display-name over the cap, non-UTF-8 display name).
    pub fn decode(wire: &[u8]) -> Result<Self> {
        let mut cur = wire;

        let version = take_u8(&mut cur)?;
        if version != PAYLOAD_VERSION
            && version != PAYLOAD_VERSION_V3
            && version != PAYLOAD_VERSION_V2
            && version != PAYLOAD_VERSION_V1
        {
            return Err(BackupError::InvalidPayload(format!(
                "unsupported payload version {version} (expected one of \
                 {PAYLOAD_VERSION_V1}, {PAYLOAD_VERSION_V2}, {PAYLOAD_VERSION_V3}, \
                 {PAYLOAD_VERSION})",
            )));
        }

        let mut account_id = [0u8; ACCOUNT_ID_LEN];
        account_id.copy_from_slice(take_n(&mut cur, ACCOUNT_ID_LEN)?);

        let created_at_ms = u64::from_be_bytes(
            take_n(&mut cur, 8)?
                .try_into()
                .expect("take_n(8) length pinned"),
        );

        let mut ed25519_seed = [0u8; SEED_LEN];
        ed25519_seed.copy_from_slice(take_n(&mut cur, SEED_LEN)?);

        let contact_count = u32::from_be_bytes(
            take_n(&mut cur, 4)?
                .try_into()
                .expect("take_n(4) length pinned"),
        ) as usize;
        let mut accepted_contacts = Vec::with_capacity(contact_count);
        for _ in 0..contact_count {
            accepted_contacts.push(decode_contact(
                &mut cur,
                version == PAYLOAD_VERSION || version == PAYLOAD_VERSION_V3,
            )?);
        }

        let snapshot_count = u32::from_be_bytes(
            take_n(&mut cur, 4)?
                .try_into()
                .expect("take_n(4) length pinned"),
        ) as usize;
        let mut mls_snapshots = Vec::with_capacity(snapshot_count);
        for _ in 0..snapshot_count {
            mls_snapshots.push(decode_snapshot(&mut cur)?);
        }

        // Phase 2a3d1 — v2 tails carry a whole-storage snapshot;
        // v1 tails do not. Distinguish on the version we read at
        // the head, NOT on remaining-bytes heuristics — a v1
        // producer never wrote the field, while a v2 producer
        // always writes at least the 4-byte length prefix
        // (which may be 0 for an empty snapshot).
        let whole_storage_snapshot = if version == PAYLOAD_VERSION_V1 {
            Vec::new()
        } else {
            let snapshot_len = u32::from_be_bytes(
                take_n(&mut cur, 4)?
                    .try_into()
                    .expect("take_n(4) length pinned"),
            ) as usize;
            take_n(&mut cur, snapshot_len)?.to_vec()
        };

        // Same rule as the snapshot tail: decided on the version read at the
        // head, never on remaining-bytes heuristics.
        let known_group_ids = if version == PAYLOAD_VERSION {
            let count = u32::from_be_bytes(
                take_n(&mut cur, 4)?
                    .try_into()
                    .expect("take_n(4) length pinned"),
            ) as usize;
            // Grown rather than pre-sized: the count is read off the wire, and
            // reserving for it would let a malformed payload ask for an
            // allocation no real backup needs.
            let mut ids = Vec::new();
            for _ in 0..count {
                let len = u32::from_be_bytes(
                    take_n(&mut cur, 4)?
                        .try_into()
                        .expect("take_n(4) length pinned"),
                ) as usize;
                if len > MAX_GROUP_ID_LEN {
                    return Err(BackupError::InvalidPayload(format!(
                        "group_id length {len} exceeds MAX_GROUP_ID_LEN {MAX_GROUP_ID_LEN}",
                    )));
                }
                ids.push(take_n(&mut cur, len)?.to_vec());
            }
            ids
        } else {
            Vec::new()
        };

        if !cur.is_empty() {
            return Err(BackupError::InvalidPayload(format!(
                "{} trailing bytes after payload tail",
                cur.len(),
            )));
        }

        Ok(Self {
            account_id,
            created_at_ms,
            ed25519_seed,
            accepted_contacts,
            mls_snapshots,
            whole_storage_snapshot,
            known_group_ids,
        })
    }
}

// ============================================================================
// Per-record codec
// ============================================================================

fn encode_contact(out: &mut Vec<u8>, c: &AcceptedContactRecord) -> Result<()> {
    out.extend_from_slice(&c.ed25519_pubkey);
    out.extend_from_slice(&c.x25519_pubkey);

    let name_bytes = c.display_name.as_bytes();
    if name_bytes.len() > MAX_DISPLAY_NAME_LEN {
        return Err(BackupError::InvalidPayload(format!(
            "display_name length {} exceeds MAX_DISPLAY_NAME_LEN {}",
            name_bytes.len(),
            MAX_DISPLAY_NAME_LEN,
        )));
    }
    let len = u16::try_from(name_bytes.len()).expect("bounded by MAX_DISPLAY_NAME_LEN above");
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(name_bytes);

    // v3 — optional account_id: a 1-byte presence flag, then 16 bytes
    // when present. (Always emitted; `encode` only ever writes the
    // current PAYLOAD_VERSION = v3.)
    match c.account_id {
        Some(id) => {
            out.push(1);
            out.extend_from_slice(&id);
        },
        None => out.push(0),
    }
    Ok(())
}

fn decode_contact(cur: &mut &[u8], with_account_id: bool) -> Result<AcceptedContactRecord> {
    let mut ed25519_pubkey = [0u8; PUBKEY_LEN];
    ed25519_pubkey.copy_from_slice(take_n(cur, PUBKEY_LEN)?);

    let mut x25519_pubkey = [0u8; PUBKEY_LEN];
    x25519_pubkey.copy_from_slice(take_n(cur, PUBKEY_LEN)?);

    let len =
        u16::from_be_bytes(take_n(cur, 2)?.try_into().expect("take_n(2) length pinned")) as usize;
    if len > MAX_DISPLAY_NAME_LEN {
        return Err(BackupError::InvalidPayload(format!(
            "display_name length {len} exceeds MAX_DISPLAY_NAME_LEN {MAX_DISPLAY_NAME_LEN}",
        )));
    }
    let name_bytes = take_n(cur, len)?;
    let display_name = std::str::from_utf8(name_bytes)
        .map_err(|e| BackupError::InvalidPayload(format!("display_name not utf-8: {e}")))?
        .to_owned();

    // v3 — optional account_id (presence flag + 16 bytes). Absent on
    // v1/v2 records, where `with_account_id` is false → `None`.
    let account_id = if with_account_id {
        match take_u8(cur)? {
            0 => None,
            1 => {
                let mut id = [0u8; ACCOUNT_ID_LEN];
                id.copy_from_slice(take_n(cur, ACCOUNT_ID_LEN)?);
                Some(id)
            },
            other => {
                return Err(BackupError::InvalidPayload(format!(
                    "invalid account_id presence byte {other} (expected 0 or 1)",
                )));
            },
        }
    } else {
        None
    };

    Ok(AcceptedContactRecord {
        ed25519_pubkey,
        x25519_pubkey,
        display_name,
        account_id,
    })
}

fn encode_snapshot(out: &mut Vec<u8>, group_id: &[u8], snapshot: &[u8]) -> Result<()> {
    let gid_len = u32::try_from(group_id.len()).map_err(|_| {
        BackupError::InvalidPayload(format!("group_id length {} overflows u32", group_id.len()))
    })?;
    let snap_len = u32::try_from(snapshot.len()).map_err(|_| {
        BackupError::InvalidPayload(format!("snapshot length {} overflows u32", snapshot.len()))
    })?;
    out.extend_from_slice(&gid_len.to_be_bytes());
    out.extend_from_slice(group_id);
    out.extend_from_slice(&snap_len.to_be_bytes());
    out.extend_from_slice(snapshot);
    Ok(())
}

fn decode_snapshot(cur: &mut &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let gid_len =
        u32::from_be_bytes(take_n(cur, 4)?.try_into().expect("take_n(4) length pinned")) as usize;
    let group_id = take_n(cur, gid_len)?.to_vec();
    let snap_len =
        u32::from_be_bytes(take_n(cur, 4)?.try_into().expect("take_n(4) length pinned")) as usize;
    let snapshot = take_n(cur, snap_len)?.to_vec();
    Ok((group_id, snapshot))
}

// ============================================================================
// Codec helpers
// ============================================================================

fn take_u8(cur: &mut &[u8]) -> Result<u8> {
    if cur.is_empty() {
        return Err(BackupError::InvalidPayload("unexpected EOF".to_string()));
    }
    let b = cur[0];
    *cur = &cur[1..];
    Ok(b)
}

fn take_n<'a>(cur: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
    if cur.len() < n {
        return Err(BackupError::InvalidPayload(format!(
            "need {n} bytes, have {}",
            cur.len(),
        )));
    }
    let (head, rest) = cur.split_at(n);
    *cur = rest;
    Ok(head)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_payload() -> BackupPayload {
        BackupPayload {
            account_id: [0xA1; ACCOUNT_ID_LEN],
            created_at_ms: 1_700_000_000_000,
            ed25519_seed: [0x42; SEED_LEN],
            accepted_contacts: vec![
                AcceptedContactRecord {
                    ed25519_pubkey: [0x01; PUBKEY_LEN],
                    x25519_pubkey: [0x02; PUBKEY_LEN],
                    display_name: "alice".to_string(),
                    account_id: Some([0xCC; ACCOUNT_ID_LEN]),
                },
                AcceptedContactRecord {
                    ed25519_pubkey: [0x03; PUBKEY_LEN],
                    x25519_pubkey: [0x04; PUBKEY_LEN],
                    display_name: String::new(),
                    account_id: None,
                },
            ],
            mls_snapshots: vec![(b"group-id-bytes".to_vec(), b"opaque-snapshot".to_vec())],
            whole_storage_snapshot: b"opaque-whole-storage-blob".to_vec(),
            known_group_ids: vec![vec![7u8; 32], vec![9u8; 32]],
        }
    }

    /// A v3 payload: everything v4 carries except the group index. Built by
    /// encoding a v4 one and rewriting the version byte + dropping the tail,
    /// so the fixture cannot drift from the encoder it is testing against.
    fn v3_wire_of(payload: &BackupPayload) -> Vec<u8> {
        let mut wire = payload.encode().unwrap();
        // The v4 tail is `count(4)` plus, per id, `len(4) + bytes`.
        let tail: usize = 4 + payload
            .known_group_ids
            .iter()
            .map(|id| 4 + id.len())
            .sum::<usize>();
        wire.truncate(wire.len() - tail);
        wire[0] = PAYLOAD_VERSION_V3;
        wire
    }

    #[test]
    fn known_group_ids_round_trip() {
        let payload = sample_payload();
        let decoded = BackupPayload::decode(&payload.encode().unwrap()).unwrap();
        assert_eq!(decoded.known_group_ids, vec![vec![7u8; 32], vec![9u8; 32]]);
    }

    #[test]
    fn a_v3_payload_decodes_with_no_group_index() {
        // The version at the head decides, never a remaining-bytes heuristic:
        // a v3 producer never wrote the tail at all.
        let payload = sample_payload();
        let decoded = BackupPayload::decode(&v3_wire_of(&payload)).unwrap();
        assert!(decoded.known_group_ids.is_empty());
        // Everything v3 did carry survives, including the per-contact account
        // id that arrived in that version.
        assert_eq!(decoded.accepted_contacts, payload.accepted_contacts);
        assert_eq!(
            decoded.whole_storage_snapshot,
            payload.whole_storage_snapshot
        );
    }

    #[test]
    fn the_group_index_is_what_a_restore_cannot_recover_from_the_snapshot() {
        // The reason this field exists: the whole-storage snapshot holds every
        // group, but nothing can enumerate them out of it, so a payload that
        // carries the snapshot alone restores a device that shows no chats.
        let payload = sample_payload();
        let v3 = BackupPayload::decode(&v3_wire_of(&payload)).unwrap();
        assert!(!v3.whole_storage_snapshot.is_empty());
        assert!(v3.known_group_ids.is_empty());
    }

    #[test]
    fn an_over_long_group_id_is_refused_on_encode() {
        let mut payload = sample_payload();
        payload.known_group_ids = vec![vec![0u8; MAX_GROUP_ID_LEN + 1]];
        assert!(payload.encode().is_err());
    }

    #[test]
    fn an_over_long_group_id_is_refused_on_decode() {
        // Hand-built, because the encoder refuses to produce one.
        let payload = sample_payload();
        let mut wire = v3_wire_of(&payload);
        wire[0] = PAYLOAD_VERSION;
        wire.extend_from_slice(&1u32.to_be_bytes());
        wire.extend_from_slice(&u32::try_from(MAX_GROUP_ID_LEN + 1).unwrap().to_be_bytes());
        wire.extend_from_slice(&[0u8; MAX_GROUP_ID_LEN + 1]);
        assert!(BackupPayload::decode(&wire).is_err());
    }

    #[test]
    fn a_truncated_group_tail_is_refused_rather_than_read_short() {
        let payload = sample_payload();
        let wire = payload.encode().unwrap();
        for cut in 1..=(4 + 32) {
            assert!(
                BackupPayload::decode(&wire[..wire.len() - cut]).is_err(),
                "a wire cut {cut} bytes short decoded"
            );
        }
    }

    #[test]
    fn round_trip_full_payload() {
        let payload = sample_payload();
        let wire = payload.encode().unwrap();
        let decoded = BackupPayload::decode(&wire).unwrap();
        assert_eq!(decoded, payload);
    }

    #[test]
    fn round_trip_empty_lists() {
        let payload = BackupPayload {
            account_id: [0; ACCOUNT_ID_LEN],
            created_at_ms: 0,
            ed25519_seed: [0; SEED_LEN],
            accepted_contacts: vec![],
            mls_snapshots: vec![],
            whole_storage_snapshot: Vec::new(),
            known_group_ids: Vec::new(),
        };
        let wire = payload.encode().unwrap();
        let decoded = BackupPayload::decode(&wire).unwrap();
        assert_eq!(decoded, payload);
    }

    /// Phase 2a3d1 — a v1-shaped wire payload (no
    /// `whole_storage_snapshot` tail) decodes as a v2-shaped
    /// `BackupPayload` with an empty snapshot. Guarantees
    /// forward compat for backups uploaded by Phase 2a1
    /// clients.
    #[test]
    fn v1_payload_decodes_with_empty_snapshot() {
        // Build the v1 bytes by hand — fixture mirrors the
        // sample_payload() shape minus the trailing snapshot
        // length + bytes.
        let mut wire = Vec::new();
        wire.push(PAYLOAD_VERSION_V1);
        wire.extend_from_slice(&[0xA1; ACCOUNT_ID_LEN]);
        wire.extend_from_slice(&1_700_000_000_000u64.to_be_bytes());
        wire.extend_from_slice(&[0x42; SEED_LEN]);
        wire.extend_from_slice(&0u32.to_be_bytes()); // contact_count = 0
        wire.extend_from_slice(&0u32.to_be_bytes()); // snapshot_count = 0
                                                     // NO whole_storage_snapshot tail.

        let decoded = BackupPayload::decode(&wire).expect("v1 decode");
        assert_eq!(decoded.account_id, [0xA1; ACCOUNT_ID_LEN]);
        assert_eq!(decoded.created_at_ms, 1_700_000_000_000);
        assert_eq!(decoded.ed25519_seed, [0x42; SEED_LEN]);
        assert!(decoded.accepted_contacts.is_empty());
        assert!(decoded.mls_snapshots.is_empty());
        assert!(
            decoded.whole_storage_snapshot.is_empty(),
            "v1 payload should fold to an empty snapshot field"
        );
    }

    #[test]
    fn v2_payload_decodes_contact_without_account_id() {
        // ADR-0021 backwards-compat: a v2 backup (no per-contact
        // account_id byte) must still restore — the contact's account_id
        // folds to None and the v2 whole-storage-snapshot tail is read.
        let mut wire = Vec::new();
        wire.push(PAYLOAD_VERSION_V2);
        wire.extend_from_slice(&[0xB2; ACCOUNT_ID_LEN]);
        wire.extend_from_slice(&1_700_000_000_000u64.to_be_bytes());
        wire.extend_from_slice(&[0x42; SEED_LEN]);
        wire.extend_from_slice(&1u32.to_be_bytes()); // contact_count = 1
                                                     // One contact — v2 layout has NO account_id byte after the name.
        wire.extend_from_slice(&[0x01; PUBKEY_LEN]);
        wire.extend_from_slice(&[0x02; PUBKEY_LEN]);
        wire.extend_from_slice(&5u16.to_be_bytes());
        wire.extend_from_slice(b"alice");
        wire.extend_from_slice(&0u32.to_be_bytes()); // snapshot_count = 0
        wire.extend_from_slice(&3u32.to_be_bytes()); // whole_storage_len = 3
        wire.extend_from_slice(b"abc");

        let decoded = BackupPayload::decode(&wire).expect("v2 decode");
        assert_eq!(decoded.accepted_contacts.len(), 1);
        assert_eq!(decoded.accepted_contacts[0].display_name, "alice");
        assert_eq!(decoded.accepted_contacts[0].account_id, None);
        assert_eq!(decoded.whole_storage_snapshot, b"abc");
    }

    #[test]
    fn empty_whole_storage_snapshot_roundtrips() {
        let mut payload = sample_payload();
        payload.whole_storage_snapshot = Vec::new();
        let wire = payload.encode().unwrap();
        let decoded = BackupPayload::decode(&wire).unwrap();
        assert_eq!(decoded, payload);
        assert!(decoded.whole_storage_snapshot.is_empty());
    }

    #[test]
    fn wrong_version_rejected() {
        let mut wire = sample_payload().encode().unwrap();
        wire[0] = 99;
        let err = BackupPayload::decode(&wire).unwrap_err();
        assert!(matches!(err, BackupError::InvalidPayload(_)));
    }

    #[test]
    fn truncated_payload_rejected() {
        let wire = sample_payload().encode().unwrap();
        let err = BackupPayload::decode(&wire[..wire.len() - 5]).unwrap_err();
        assert!(matches!(err, BackupError::InvalidPayload(_)));
    }

    #[test]
    fn trailing_bytes_rejected() {
        let mut wire = sample_payload().encode().unwrap();
        wire.push(0x00);
        let err = BackupPayload::decode(&wire).unwrap_err();
        assert!(matches!(err, BackupError::InvalidPayload(_)));
    }

    #[test]
    fn display_name_over_cap_rejected_on_encode() {
        let payload = BackupPayload {
            account_id: [0; ACCOUNT_ID_LEN],
            created_at_ms: 0,
            ed25519_seed: [0; SEED_LEN],
            accepted_contacts: vec![AcceptedContactRecord {
                ed25519_pubkey: [0; PUBKEY_LEN],
                x25519_pubkey: [0; PUBKEY_LEN],
                display_name: "x".repeat(MAX_DISPLAY_NAME_LEN + 1),
                account_id: None,
            }],
            mls_snapshots: vec![],
            whole_storage_snapshot: Vec::new(),
            known_group_ids: Vec::new(),
        };
        let err = payload.encode().unwrap_err();
        assert!(matches!(err, BackupError::InvalidPayload(_)));
    }

    #[test]
    fn display_name_at_cap_is_supported() {
        let payload = BackupPayload {
            account_id: [0; ACCOUNT_ID_LEN],
            created_at_ms: 0,
            ed25519_seed: [0; SEED_LEN],
            accepted_contacts: vec![AcceptedContactRecord {
                ed25519_pubkey: [0; PUBKEY_LEN],
                x25519_pubkey: [0; PUBKEY_LEN],
                display_name: "x".repeat(MAX_DISPLAY_NAME_LEN),
                account_id: None,
            }],
            mls_snapshots: vec![],
            whole_storage_snapshot: Vec::new(),
            known_group_ids: Vec::new(),
        };
        let wire = payload.encode().unwrap();
        let decoded = BackupPayload::decode(&wire).unwrap();
        assert_eq!(decoded, payload);
    }

    /// End-to-end: encode the payload, seal it in the envelope,
    /// open + decode. This is the canonical flow the
    /// application layer drives.
    #[test]
    fn round_trip_through_envelope() {
        let phrase = b"recovery phrase 24 words abracadabra";
        let payload = sample_payload();
        let payload_bytes = payload.encode().unwrap();

        let wire = crate::envelope::seal(phrase, &payload_bytes).unwrap();
        let opened = crate::envelope::open(phrase, &wire).unwrap();
        let decoded = BackupPayload::decode(&opened).unwrap();
        assert_eq!(decoded, payload);
    }
}
