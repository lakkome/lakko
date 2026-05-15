// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `BackupPayload` — the structured bytes sealed inside the
//! Phase 2a1 backup envelope.
//!
//! ## Wire format (v1)
//!
//! Hand-rolled length-prefixed binary codec. Same pattern
//! `lakko-contacts` uses for its wire types — keeps the parser
//! surface small and fuzz-friendly, and avoids pulling in a
//! schema dep (prost / serde-binary) that would force every
//! caller through a wider attack surface.
//!
//! ```text
//! u8           payload version (= 1)
//! [16 bytes]   account_id (UUIDv7 raw)
//! u64be        created_at_ms (the wall-clock at upload time)
//! [32 bytes]   ed25519_seed (the identity signing seed)
//! u32be        accepted_contacts_count
//! [..]         accepted_contacts_count × AcceptedContactRecord
//! u32be        mls_snapshots_count (reserved for 2a3; emit 0 for now)
//! [..]         mls_snapshots_count × { 32-byte group_id, u32be length, bytes }
//! ```
//!
//! Each [`AcceptedContactRecord`]:
//!
//! ```text
//! [32 bytes]   ed25519_pubkey
//! [32 bytes]   x25519_pubkey
//! u16be        display_name length (UTF-8 bytes, ≤ MAX_DISPLAY_NAME_LEN)
//! [..]         display_name bytes
//! ```
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

/// Current payload schema version.
pub const PAYLOAD_VERSION: u8 = 1;

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
    /// Per-group OpenMLS provider snapshots. Empty in 2a1;
    /// wired in 2a3 once DeviceGroup-side snapshot publishing
    /// lands. Each entry is `(group_id, raw_snapshot_bytes)`;
    /// the snapshot format is the same opaque blob
    /// `lakko-mls` already emits from
    /// `Group::snapshot()` / `Group::load()`.
    pub mls_snapshots: Vec<(Vec<u8>, Vec<u8>)>,
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
        if version != PAYLOAD_VERSION {
            return Err(BackupError::InvalidPayload(format!(
                "unsupported payload version {version} (expected {PAYLOAD_VERSION})",
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
            accepted_contacts.push(decode_contact(&mut cur)?);
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
    Ok(())
}

fn decode_contact(cur: &mut &[u8]) -> Result<AcceptedContactRecord> {
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

    Ok(AcceptedContactRecord {
        ed25519_pubkey,
        x25519_pubkey,
        display_name,
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
                },
                AcceptedContactRecord {
                    ed25519_pubkey: [0x03; PUBKEY_LEN],
                    x25519_pubkey: [0x04; PUBKEY_LEN],
                    display_name: String::new(),
                },
            ],
            mls_snapshots: vec![(b"group-id-bytes".to_vec(), b"opaque-snapshot".to_vec())],
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
        };
        let wire = payload.encode().unwrap();
        let decoded = BackupPayload::decode(&wire).unwrap();
        assert_eq!(decoded, payload);
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
            }],
            mls_snapshots: vec![],
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
            }],
            mls_snapshots: vec![],
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
