// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for `lakko-backup` (Phase 2a1c).
//!
//! Mirror of `crates/lakko-ffi-wasm/src/backup.rs` for iOS +
//! Android. Same four primitives:
//!
//! - [`build_backup_payload`] — assemble `BackupPayload` wire
//!   bytes from the parts the client has at hand.
//! - [`seal_backup`] — AEAD-encrypt the payload bytes under a
//!   KEK derived from the BIP39 recovery phrase. Returns the
//!   on-the-wire envelope ready to PUT to `services/storage`'s
//!   `/backup` endpoint.
//! - [`open_backup`] — inverse of `seal_backup`.
//! - [`parse_backup_payload`] — decode the payload bytes (post-
//!   `open_backup`) into a structured [`ParsedBackupPayload`].
//!
//! Records (vs Object handles) keep the boundary cheap — the
//! whole flow is read-only after the byte arrays land, and the
//! UniFFI lifetime story matches the rest of the FFI surface
//! (every byte field crosses the boundary as `Vec<u8>` →
//! `Data` / `ByteArray`).

use lakko_backup::{
    open as inner_open, payload::AcceptedContactRecord, seal as inner_seal, BackupError,
    BackupPayload,
};

use crate::FfiError;

impl From<BackupError> for FfiError {
    fn from(e: BackupError) -> Self {
        FfiError::Backup(e.to_string())
    }
}

// ============================================================================
// Records
// ============================================================================

/// One row in the accepted-contacts list inside a backup
/// payload. Public bytes only — no secret material.
#[derive(uniffi::Record, Clone)]
pub struct BackupContactRecord {
    /// Raw 32-byte Ed25519 verifying key.
    pub ed25519_pubkey: Vec<u8>,
    /// Raw 32-byte X25519 HPKE public key.
    pub x25519_pubkey: Vec<u8>,
    /// User-chosen display label (UTF-8, may be empty).
    pub display_name: String,
}

/// Structured view of a decoded backup payload — what
/// [`parse_backup_payload`] returns. Mirror of
/// `BackupPayload` flattened for UniFFI.
#[derive(uniffi::Record, Clone)]
pub struct ParsedBackupPayload {
    /// Raw 16-byte UUIDv7 account id.
    pub account_id: Vec<u8>,
    /// Wall-clock UNIX milliseconds at backup-creation time.
    pub created_at_ms: u64,
    /// Raw 32-byte identity signing seed.
    pub ed25519_seed: Vec<u8>,
    /// Accepted-contacts list.
    pub accepted_contacts: Vec<BackupContactRecord>,
    /// Whole-storage OpenMLS provider snapshot (Phase 2a3d1).
    /// Empty `Vec` when the backup pre-dates 2a3d or no MLS
    /// state had been published at upload time. The restore
    /// path feeds this into `Identity.restoreStorage(...)`.
    pub whole_storage_snapshot: Vec<u8>,
}

// ============================================================================
// Free functions
// ============================================================================

/// Build the structured backup payload wire bytes that feed
/// [`seal_backup`].
///
/// # Errors
///
/// * [`FfiError::Backup`] on wrong-length `account_id` (≠ 16) /
///   `ed25519_seed` (≠ 32), wrong-length contact pubkey
///   (must be 32 bytes each), or over-cap display name.
#[uniffi::export]
pub fn build_backup_payload(
    account_id: Vec<u8>,
    created_at_ms: u64,
    ed25519_seed: Vec<u8>,
    contacts: Vec<BackupContactRecord>,
    // Phase 2a3d1 — whole-storage OpenMLS provider snapshot
    // (the bytes `Identity.snapshotStorage()` emits). Pass an
    // empty `Vec` when there's no MLS state to back up yet
    // (signup baseline before the DeviceGroup-bootstrap path
    // runs, etc.).
    whole_storage_snapshot: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    if account_id.len() != 16 {
        return Err(FfiError::Backup("account_id must be 16 bytes".into()));
    }
    if ed25519_seed.len() != 32 {
        return Err(FfiError::Backup("ed25519_seed must be 32 bytes".into()));
    }
    let mut acct = [0u8; 16];
    acct.copy_from_slice(&account_id);
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&ed25519_seed);

    let mut accepted_contacts = Vec::with_capacity(contacts.len());
    for c in contacts {
        if c.ed25519_pubkey.len() != 32 {
            return Err(FfiError::Backup(
                "contact ed25519_pubkey must be 32 bytes".into(),
            ));
        }
        if c.x25519_pubkey.len() != 32 {
            return Err(FfiError::Backup(
                "contact x25519_pubkey must be 32 bytes".into(),
            ));
        }
        if c.display_name.len() > lakko_backup::payload::MAX_DISPLAY_NAME_LEN {
            return Err(FfiError::Backup(format!(
                "contact display_name length {} exceeds cap {}",
                c.display_name.len(),
                lakko_backup::payload::MAX_DISPLAY_NAME_LEN,
            )));
        }
        let mut e = [0u8; 32];
        e.copy_from_slice(&c.ed25519_pubkey);
        let mut x = [0u8; 32];
        x.copy_from_slice(&c.x25519_pubkey);
        accepted_contacts.push(AcceptedContactRecord {
            ed25519_pubkey: e,
            x25519_pubkey: x,
            display_name: c.display_name,
            // ADR-0021: the per-contact account_id round-trips on the web
            // (wasm) backup surface; the native (Swift/Kotlin) backup does
            // not carry it yet (iOS/Android persist contacts in their own
            // AcceptedContactsStore, not via lakko-backup), so it is None
            // here. Threading it through the UniFFI BackupContact is a
            // parity follow-up if native backup ever round-trips contacts.
            account_id: None,
        });
    }

    let payload = BackupPayload {
        account_id: acct,
        created_at_ms,
        ed25519_seed: seed,
        accepted_contacts,
        mls_snapshots: Vec::new(),
        whole_storage_snapshot,
    };
    Ok(payload.encode()?)
}

/// Decode payload wire bytes (post-`open_backup`) into a
/// structured `ParsedBackupPayload`.
///
/// # Errors
///
/// [`FfiError::Backup`] on any wire-format failure (wrong
/// version, truncated, trailing bytes, display-name over cap,
/// non-UTF-8 name).
#[uniffi::export]
pub fn parse_backup_payload(wire: Vec<u8>) -> Result<ParsedBackupPayload, FfiError> {
    let inner = BackupPayload::decode(&wire)?;
    let accepted_contacts: Vec<BackupContactRecord> = inner
        .accepted_contacts
        .iter()
        .map(|c| BackupContactRecord {
            ed25519_pubkey: c.ed25519_pubkey.to_vec(),
            x25519_pubkey: c.x25519_pubkey.to_vec(),
            display_name: c.display_name.clone(),
        })
        .collect();
    Ok(ParsedBackupPayload {
        account_id: inner.account_id.to_vec(),
        created_at_ms: inner.created_at_ms,
        ed25519_seed: inner.ed25519_seed.to_vec(),
        accepted_contacts,
        whole_storage_snapshot: inner.whole_storage_snapshot,
    })
}

/// Encrypt `payload_bytes` under a KEK derived from `phrase`
/// (BIP39 recovery phrase, UTF-8 bytes). Returns the
/// on-the-wire envelope ready to PUT to `services/storage`'s
/// `/backup` endpoint.
///
/// # Errors
///
/// [`FfiError::Backup`] on Argon2id parameter failure (cannot
/// happen for the fixed-params API) or oversize payload
/// (unreachable in practice).
#[uniffi::export]
pub fn seal_backup(phrase: Vec<u8>, payload_bytes: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    Ok(inner_seal(&phrase, &payload_bytes)?)
}

/// Decrypt an on-the-wire backup envelope back to its payload
/// bytes. Wrong phrase / tampered blob fold into a single
/// [`FfiError::Backup`] variant.
///
/// # Errors
///
/// [`FfiError::Backup`] for wrong-phrase / tampered-blob /
/// malformed-wire failures.
#[uniffi::export]
pub fn open_backup(phrase: Vec<u8>, wire: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    Ok(inner_open(&phrase, &wire)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_contact() -> BackupContactRecord {
        BackupContactRecord {
            ed25519_pubkey: vec![0x01u8; 32],
            x25519_pubkey: vec![0x02u8; 32],
            display_name: "alice".to_string(),
        }
    }

    #[test]
    fn build_then_parse_round_trips() {
        let account_id = vec![0xA1u8; 16];
        let seed = vec![0x42u8; 32];
        let contacts = vec![sample_contact()];
        let snapshot = vec![0xEFu8; 8];
        let wire = build_backup_payload(
            account_id.clone(),
            1_700_000_000_000,
            seed.clone(),
            contacts,
            snapshot.clone(),
        )
        .expect("build");
        let parsed = parse_backup_payload(wire).expect("parse");
        assert_eq!(parsed.account_id, account_id);
        assert_eq!(parsed.created_at_ms, 1_700_000_000_000);
        assert_eq!(parsed.ed25519_seed, seed);
        assert_eq!(parsed.accepted_contacts.len(), 1);
        assert_eq!(parsed.accepted_contacts[0].display_name, "alice");
        assert_eq!(parsed.whole_storage_snapshot, snapshot);
    }

    #[test]
    fn seal_open_round_trip_through_ffi() {
        let phrase = b"correct horse battery staple".to_vec();
        let payload = b"opaque inner bytes".to_vec();
        let envelope = seal_backup(phrase.clone(), payload.clone()).expect("seal");
        let opened = open_backup(phrase, envelope).expect("open");
        assert_eq!(opened, payload);
    }

    #[test]
    fn end_to_end_through_ffi_surface() {
        let phrase = b"end to end recovery phrase 24 words".to_vec();
        let account_id = vec![0xCDu8; 16];
        let seed = vec![0x77u8; 32];
        let contacts = vec![sample_contact()];
        let payload =
            build_backup_payload(account_id.clone(), 99, seed.clone(), contacts, Vec::new())
                .expect("build");
        let envelope = seal_backup(phrase.clone(), payload).expect("seal");
        let opened = open_backup(phrase, envelope).expect("open");
        let parsed = parse_backup_payload(opened).expect("parse");
        assert_eq!(parsed.account_id, account_id);
        assert_eq!(parsed.ed25519_seed, seed);
    }

    #[test]
    fn wrong_phrase_rejected() {
        let payload = build_backup_payload(vec![0u8; 16], 0, vec![0u8; 32], vec![], Vec::new())
            .expect("build");
        let envelope = seal_backup(b"right".to_vec(), payload).expect("seal");
        let err = open_backup(b"wrong".to_vec(), envelope).expect_err("wrong phrase");
        match err {
            FfiError::Backup(_) => {},
            other => panic!("expected Backup variant, got {other:?}"),
        }
    }

    #[test]
    fn wrong_length_account_id_rejected() {
        let err = build_backup_payload(vec![0u8; 8], 0, vec![0u8; 32], vec![], Vec::new())
            .expect_err("wrong len");
        match err {
            FfiError::Backup(msg) => assert!(msg.contains("16 bytes")),
            other => panic!("expected Backup variant, got {other:?}"),
        }
    }
}
