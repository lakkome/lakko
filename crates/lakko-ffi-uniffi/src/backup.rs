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

use lakko_attest::{
    verify_backup_head as inner_verify_backup_head,
    verify_backup_head_binding as inner_verify_backup_head_binding, BackupHeadError,
};
use lakko_backup::{
    frame_backup_blob as inner_frame_backup_blob, open as inner_open,
    parse_backup_blob as inner_parse_backup_blob, payload::AcceptedContactRecord,
    seal as inner_seal, BackupBlob, BackupError, BackupPayload, HeadFrameError,
};
use lakko_crypto::signing::VerifyingKey;

use crate::FfiError;

impl From<BackupError> for FfiError {
    fn from(e: BackupError) -> Self {
        FfiError::Backup(e.to_string())
    }
}

impl From<HeadFrameError> for FfiError {
    fn from(e: HeadFrameError) -> Self {
        // Both framing failures (a wrong-length head on frame, a truncated
        // framed blob on parse) are hard fail-closed signals (ADR-0037 §6
        // "Invalid") with no legitimate cause, so they fold into the generic
        // hard-fail Backup class — no user override.
        FfiError::Backup(e.to_string())
    }
}

impl From<BackupHeadError> for FfiError {
    fn from(e: BackupHeadError) -> Self {
        match e {
            // Staleness (ADR-0037 §6) is a policy signal, not a tamper signal:
            // the head verified authentically but sits outside the freshness
            // window, so the caller MAY offer a user-acknowledged override.
            // Kept a distinct variant so Swift / Kotlin branch on the enum,
            // not on a message string.
            BackupHeadError::Stale => FfiError::BackupHeadStale,
            // Every other variant (bad length / version / signature, account or
            // envelope-hash mismatch) is an unambiguous tamper / corruption
            // signal with no legitimate cause — hard fail-closed, folded into
            // the generic Backup class.
            other => FfiError::Backup(other.to_string()),
        }
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
    /// The peer's 16-byte account_id (ADR-0021), or `None` when the
    /// contact predates it. Round-trips so a restored native contact
    /// resolves in a group-call roster — mirrors the wasm surface.
    pub account_id: Option<Vec<u8>>,
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
    /// The MLS `group_id`s of the conversations the account's devices knew
    /// when the backup was taken. Empty for an account that knew none.
    ///
    /// Names and nothing more: the backup carries no MLS state, so a
    /// restored device learns from these which conversations exist before
    /// any member has admitted it. Membership comes from the other members
    /// (ADR-0050 section 2.5a), never from this list.
    pub known_group_ids: Vec<Vec<u8>>,
    /// The ADR-0058 folded synced state, as `SyncedState.toBytes()` wrote
    /// it; restore with `SyncedState.fromBytes(...)`. Empty = none held.
    pub synced_state: Vec<u8>,
}

// ============================================================================
// Free functions
// ============================================================================

/// Build the structured backup payload wire bytes that feed
/// [`seal_backup`].
///
/// The payload carries no MLS state. An OpenMLS provider snapshot belongs to
/// the device that made it, so it is never backed up: a restored device is
/// admitted to its conversations by other members (ADR-0050 section 2.5a).
///
/// # Errors
///
/// * [`FfiError::Backup`] on wrong-length `account_id` (≠ 16) /
///   `ed25519_seed` (≠ 32), wrong-length contact pubkey
///   (must be 32 bytes each), over-cap display name, an over-long
///   `group_id`, or a `synced_state` the fold refuses.
#[uniffi::export(default(synced_state = None))]
pub fn build_backup_payload(
    account_id: Vec<u8>,
    created_at_ms: u64,
    ed25519_seed: Vec<u8>,
    contacts: Vec<BackupContactRecord>,
    // The MLS `group_id`s of the conversations this account's devices know
    // about, so a restored device knows which conversations exist before
    // any member has admitted it.
    known_group_ids: Vec<Vec<u8>>,
    // ADR-0058: `SyncedState.toBytes()`, or `None` / empty when none is
    // held. Defaults to `None`, so a five-argument call is valid.
    synced_state: Option<Vec<u8>>,
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
        // ADR-0021: round-trip the per-contact account_id (16 bytes) so a
        // restored native contact resolves in a group-call roster, matching
        // the wasm surface. A malformed length is a hard error rather than a
        // silent drop, so the caller learns its contact list is corrupt.
        let account_id = match c.account_id {
            None => None,
            Some(id) if id.len() == 16 => {
                let mut a = [0u8; 16];
                a.copy_from_slice(&id);
                Some(a)
            },
            Some(id) => {
                return Err(FfiError::Backup(format!(
                    "contact account_id must be 16 bytes, got {}",
                    id.len()
                )));
            },
        };
        accepted_contacts.push(AcceptedContactRecord {
            ed25519_pubkey: e,
            x25519_pubkey: x,
            display_name: c.display_name,
            account_id,
        });
    }

    for id in &known_group_ids {
        if id.len() > lakko_backup::payload::MAX_GROUP_ID_LEN {
            return Err(FfiError::Backup(format!(
                "group_id length {} exceeds cap {}",
                id.len(),
                lakko_backup::payload::MAX_GROUP_ID_LEN,
            )));
        }
    }

    let payload = BackupPayload {
        account_id: acct,
        created_at_ms,
        ed25519_seed: seed,
        accepted_contacts,
        known_group_ids,
        synced_state: synced_state.unwrap_or_default(),
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
            account_id: c.account_id.map(|id| id.to_vec()),
        })
        .collect();
    Ok(ParsedBackupPayload {
        account_id: inner.account_id.to_vec(),
        created_at_ms: inner.created_at_ms,
        ed25519_seed: inner.ed25519_seed.to_vec(),
        accepted_contacts,
        known_group_ids: inner.known_group_ids,
        synced_state: inner.synced_state,
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

// ============================================================================
// Backup-head verify + envelope-hash helper (ADR-0037, G-11)
// ============================================================================
//
// The rollback defence for a fresh-install restore. An auth-signed `BackupHead`
// commits an account's LATEST backup envelope (highest monotone
// `backup_version`); the client verifies it against the CLIENT-PINNED auth
// attest verifying key (the same `DeviceAttestKey` it already pins for device /
// genesis attestations — ADR-0037 §3 reuses the key across all three roles,
// kept safe by distinct domain separators). Verify only — signing is auth-only,
// server-side (same posture as `verify_genesis_attestation`).
//
// The error split is load-bearing for the §6 restore policy: `BackupHeadStale`
// (the freshness gate) is a user-acknowledged override, every other failure is
// `Backup` (hard fail-closed). The success record carries `backup_version` so
// the caller applies its own persisted monotonic floor (that check needs
// client-persisted state, so it stays with the caller — ADR-0037 §6).

/// The authenticated commitment a verified backup head carries — the UniFFI
/// projection of `lakko_attest::BackupHead`. Returned by [`verify_backup_head`]
/// and [`verify_backup_head_binding`] on success. `Debug` is safe: every field
/// is a public commitment (no secret / key material).
#[derive(uniffi::Record, Clone, Debug)]
pub struct VerifiedBackupHead {
    /// The auth-assigned monotone backup version. Strictly increasing per
    /// account; the caller refuses any head below its persisted floor
    /// (`last_known_version`) as a user-acknowledged rollback override.
    pub backup_version: u64,
    /// Raw 32-byte `BLAKE3` of the envelope portion the head commits to. Binds
    /// the head to exactly the served envelope.
    pub envelope_hash: Vec<u8>,
    /// The mint timestamp, Unix **milliseconds**. The client applies its own
    /// staleness window against this (it is not an expiry).
    pub signed_at_millis: i64,
}

impl From<lakko_attest::BackupHead> for VerifiedBackupHead {
    fn from(h: lakko_attest::BackupHead) -> Self {
        Self {
            backup_version: h.backup_version,
            envelope_hash: h.envelope_hash.to_vec(),
            signed_at_millis: h.signed_at_unix_millis,
        }
    }
}

/// Parse the auth attest verifying-key bytes into a [`VerifyingKey`], mapping a
/// bad key to the hard-fail-closed [`FfiError::Backup`] class.
fn parse_attest_key(bytes: &[u8]) -> Result<VerifyingKey, FfiError> {
    VerifyingKey::from_bytes(bytes)
        .map_err(|_| FfiError::Backup("attest verifying key is not a valid Ed25519 key".into()))
}

/// Verify a backup head's signature, `account_id` binding, `envelope_hash`
/// binding, AND freshness — the full check a client applies on restore
/// (ADR-0037 §6). Returns the parsed [`VerifiedBackupHead`] so the caller reads
/// `backup_version` for its own monotonic-floor comparison against its persisted
/// `last_known_version`.
///
/// `head` is the raw backup-head bytes (the container prefix of the served
/// backup blob). `attest_verifying_key` is the 32-byte client-pinned auth
/// attest key. `expected_account_id` is the client's authenticated 16-byte
/// account id; `expected_envelope_hash` is the 32-byte [`backup_envelope_hash`]
/// of the served envelope. `now_millis` and `max_staleness_millis` are Unix
/// milliseconds — the staleness window is a client policy tracking the
/// auto-backup cadence.
///
/// # Errors
///
/// [`FfiError::BackupHeadStale`] — the head verified authentically but its
/// `signed_at` falls outside the freshness window (user-acknowledged override).
/// [`FfiError::Backup`] — hard fail-closed: a malformed / wrong-length /
/// wrong-version head, a signature that does not verify, or an `account_id` /
/// `envelope_hash` mismatch, or a bad verifying key / wrong-length
/// `expected_account_id` (≠ 16) / `expected_envelope_hash` (≠ 32).
#[uniffi::export]
pub fn verify_backup_head(
    head: Vec<u8>,
    attest_verifying_key: Vec<u8>,
    expected_account_id: Vec<u8>,
    expected_envelope_hash: Vec<u8>,
    now_millis: i64,
    max_staleness_millis: i64,
) -> Result<VerifiedBackupHead, FfiError> {
    let verifying = parse_attest_key(&attest_verifying_key)?;
    let expected_account: [u8; 16] = expected_account_id
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Backup("expected_account_id must be 16 bytes".into()))?;
    let expected_envelope_hash: [u8; 32] = expected_envelope_hash
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Backup("expected_envelope_hash must be 32 bytes".into()))?;
    let head = inner_verify_backup_head(
        &head,
        &verifying,
        &expected_account,
        &expected_envelope_hash,
        now_millis,
        max_staleness_millis,
    )?;
    Ok(head.into())
}

/// Verify a backup head's signature and `account_id` binding ONLY — no
/// `envelope_hash` or freshness check (ADR-0037 §6). For a caller that applies
/// its own freshness and monotonic-floor policy (e.g. one that wants the parsed
/// `backup_version` before it has hashed the served envelope). Returns the
/// parsed [`VerifiedBackupHead`].
///
/// # Errors
///
/// [`FfiError::Backup`] — hard fail-closed: a malformed / wrong-length /
/// wrong-version head, a signature that does not verify, an `account_id`
/// mismatch, or a bad verifying key / wrong-length `expected_account_id` (≠ 16).
/// This entry point performs no freshness check, so it never returns
/// [`FfiError::BackupHeadStale`].
#[uniffi::export]
pub fn verify_backup_head_binding(
    head: Vec<u8>,
    attest_verifying_key: Vec<u8>,
    expected_account_id: Vec<u8>,
) -> Result<VerifiedBackupHead, FfiError> {
    let verifying = parse_attest_key(&attest_verifying_key)?;
    let expected_account: [u8; 16] = expected_account_id
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Backup("expected_account_id must be 16 bytes".into()))?;
    let head = inner_verify_backup_head_binding(&head, &verifying, &expected_account)?;
    Ok(head.into())
}

/// Compute the `BLAKE3` envelope hash (`lakko_backup::chain::envelope_hash`)
/// over the served backup envelope bytes — the 32-byte value the client feeds as
/// `expected_envelope_hash` to [`verify_backup_head`] at restore, and posts to
/// `POST /auth/backup/head` at mint. Exposed so all three clients compute the
/// identical hash without re-implementing BLAKE3. Returns the 32-byte hash.
#[uniffi::export]
#[must_use]
pub fn backup_envelope_hash(envelope_bytes: Vec<u8>) -> Vec<u8> {
    lakko_backup::chain::envelope_hash(&envelope_bytes).to_vec()
}

// ============================================================================
// Container-blob framing (ADR-0037 §4 / S4, G-11)
// ============================================================================
//
// The auth-signed backup head rides INSIDE the one storage backup blob so the
// head and the envelope it commits swap together in a single atomic PUT:
// `CONTAINER_TAG(0xB1) ‖ head(129) ‖ envelope`. The client frames on upload and
// peeks byte 0 on restore to tell a framed blob from a legacy raw envelope.
// `services/storage` is untouched — the blob stays opaque (invariant #2). This
// surface is FRAMING ONLY; the caller verifies a framed head via
// `verify_backup_head` and applies the §6 fail-open / fail-closed policy.

/// Structured view of a parsed backup blob — what [`parse_backup_blob`] returns.
/// The UniFFI projection of `lakko_backup::BackupBlob`. `Debug` is safe: the
/// head is a public auth-signed commitment and the envelope is opaque
/// AEAD-sealed ciphertext (no secret / key material either way).
#[derive(uniffi::Record, Clone, Debug)]
pub struct ParsedBackupBlob {
    /// `true` when the blob carried the `0xB1` container tag and a full head
    /// (`Framed`); `false` for a legacy raw envelope with no head (`Legacy`).
    pub framed: bool,
    /// The raw backup-head bytes when `framed` is `true`, else `None`. Still
    /// **unverified** — feed to [`verify_backup_head`] against the pinned auth
    /// attest key.
    pub head: Option<Vec<u8>>,
    /// The envelope portion (the whole blob when `framed` is `false`; the tail
    /// after the tag + head when `framed` is `true`). Feed to [`open_backup`].
    pub envelope: Vec<u8>,
}

impl From<BackupBlob> for ParsedBackupBlob {
    fn from(b: BackupBlob) -> Self {
        match b {
            BackupBlob::Framed { head, envelope } => Self {
                framed: true,
                head: Some(head.to_vec()),
                envelope,
            },
            BackupBlob::Legacy(envelope) => Self {
                framed: false,
                head: None,
                envelope,
            },
        }
    }
}

/// Frame an auth-signed backup head and its envelope into the single storage
/// blob `CONTAINER_TAG ‖ head ‖ envelope` to PUT to `services/storage` on the
/// mint / upload path (ADR-0037 §4). `head` must be the raw
/// `lakko_attest::BACKUP_HEAD_LEN`-byte head bytes.
///
/// # Errors
///
/// [`FfiError::Backup`] (hard fail-closed) if `head` is the wrong length.
#[uniffi::export]
pub fn frame_backup_blob(head: Vec<u8>, envelope: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    Ok(inner_frame_backup_blob(&head, &envelope)?)
}

/// Peek a stored backup blob's container framing on the restore path, before any
/// head verification or AEAD-open (ADR-0037 §4). Returns a [`ParsedBackupBlob`]
/// carrying the framed-vs-legacy kind, the (unverified) head bytes when framed,
/// and the envelope bytes.
///
/// # Errors
///
/// [`FfiError::Backup`] (hard fail-closed) when the blob carries the `0xB1`
/// container tag but is too short to hold a full head — active tamper /
/// corruption of a framed blob (ADR-0037 §6 "Invalid"). A legacy (non-`0xB1`)
/// or empty blob is **not** an error: it returns a `Legacy` [`ParsedBackupBlob`]
/// (the §6 "Absent → fail-open" lane).
#[uniffi::export]
pub fn parse_backup_blob(blob: Vec<u8>) -> Result<ParsedBackupBlob, FfiError> {
    Ok(inner_parse_backup_blob(&blob)?.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_contact() -> BackupContactRecord {
        BackupContactRecord {
            ed25519_pubkey: vec![0x01u8; 32],
            x25519_pubkey: vec![0x02u8; 32],
            display_name: "alice".to_string(),
            account_id: None,
        }
    }

    /// A real, non-empty ADR-0058 synced state: one folder.
    fn sample_synced_state_bytes() -> Vec<u8> {
        use lakko_sync::synced::{FolderMeta, SyncEntry, INCLUDE_GROUPS};
        let mut state = lakko_sync::SyncedState::new();
        state
            .apply(
                7,
                SyncEntry::FolderMeta(FolderMeta {
                    folder_id: [3; 16],
                    name: "Work".into(),
                    include_kinds: INCLUDE_GROUPS,
                    exclude_flags: 0,
                }),
            )
            .expect("apply");
        state.to_bytes()
    }

    #[test]
    fn account_id_round_trips_through_uniffi_surface() {
        // ADR-0021: a contact with a 16-byte account_id round-trips; one
        // without stays absent.
        let with_id = BackupContactRecord {
            ed25519_pubkey: vec![0x01u8; 32],
            x25519_pubkey: vec![0x02u8; 32],
            display_name: "alice".to_string(),
            account_id: Some(vec![0xABu8; 16]),
        };
        let wire = build_backup_payload(
            vec![0xA1u8; 16],
            1,
            vec![0x42u8; 32],
            vec![with_id, sample_contact()],
            Vec::new(),
            None,
        )
        .expect("build");
        let parsed = parse_backup_payload(wire).expect("parse");
        assert_eq!(parsed.accepted_contacts.len(), 2);
        assert_eq!(
            parsed.accepted_contacts[0].account_id,
            Some(vec![0xABu8; 16])
        );
        assert_eq!(parsed.accepted_contacts[1].account_id, None);
    }

    #[test]
    fn bad_account_id_length_is_rejected() {
        let bad = BackupContactRecord {
            ed25519_pubkey: vec![0x01u8; 32],
            x25519_pubkey: vec![0x02u8; 32],
            display_name: "alice".to_string(),
            account_id: Some(vec![0xABu8; 8]), // wrong length
        };
        let err = build_backup_payload(
            vec![0xA1u8; 16],
            1,
            vec![0x42u8; 32],
            vec![bad],
            Vec::new(),
            None,
        );
        assert!(err.is_err(), "an 8-byte account_id must be rejected");
    }

    #[test]
    fn build_then_parse_round_trips() {
        let account_id = vec![0xA1u8; 16];
        let seed = vec![0x42u8; 32];
        let contacts = vec![sample_contact()];
        let wire = build_backup_payload(
            account_id.clone(),
            1_700_000_000_000,
            seed.clone(),
            contacts,
            Vec::new(),
            None,
        )
        .expect("build");
        let parsed = parse_backup_payload(wire).expect("parse");
        assert_eq!(parsed.account_id, account_id);
        assert_eq!(parsed.created_at_ms, 1_700_000_000_000);
        assert_eq!(parsed.ed25519_seed, seed);
        assert_eq!(parsed.accepted_contacts.len(), 1);
        assert_eq!(parsed.accepted_contacts[0].display_name, "alice");
        assert!(parsed.known_group_ids.is_empty());
        assert!(parsed.synced_state.is_empty());
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
        let payload = build_backup_payload(
            account_id.clone(),
            99,
            seed.clone(),
            contacts,
            Vec::new(),
            None,
        )
        .expect("build");
        let envelope = seal_backup(phrase.clone(), payload).expect("seal");
        let opened = open_backup(phrase, envelope).expect("open");
        let parsed = parse_backup_payload(opened).expect("parse");
        assert_eq!(parsed.account_id, account_id);
        assert_eq!(parsed.ed25519_seed, seed);
    }

    #[test]
    fn wrong_phrase_rejected() {
        let payload =
            build_backup_payload(vec![0u8; 16], 0, vec![0u8; 32], vec![], Vec::new(), None)
                .expect("build");
        let envelope = seal_backup(b"right".to_vec(), payload).expect("seal");
        let err = open_backup(b"wrong".to_vec(), envelope).expect_err("wrong phrase");
        match err {
            FfiError::Backup(_) => {},
            other => panic!("expected Backup variant, got {other:?}"),
        }
    }

    #[test]
    fn the_known_group_ids_round_trip_so_a_restore_knows_which_conversations_exist() {
        // The backup carries no MLS state, so these ids are the only way a
        // restored device knows which conversations exist before a member
        // has admitted it.
        let ids = vec![vec![0x11u8; 32], vec![0x22u8; 32]];
        let wire = build_backup_payload(vec![0u8; 16], 0, vec![0u8; 32], vec![], ids.clone(), None)
            .expect("build");
        let parsed = parse_backup_payload(wire).expect("parse");
        assert_eq!(parsed.known_group_ids, ids);
    }

    #[test]
    fn group_ids_and_synced_state_round_trip_together() {
        let ids = vec![vec![0x11u8; 32], vec![0x22u8; 32]];
        let state = sample_synced_state_bytes();
        let wire = build_backup_payload(
            vec![0xA1u8; 16],
            7,
            vec![0x42u8; 32],
            vec![sample_contact()],
            ids.clone(),
            Some(state.clone()),
        )
        .expect("build");
        let parsed = parse_backup_payload(wire).expect("parse");
        assert_eq!(parsed.known_group_ids, ids);
        assert_eq!(parsed.synced_state, state);
        assert_eq!(parsed.accepted_contacts.len(), 1);
    }

    #[test]
    fn the_built_payload_is_wire_v6_and_a_v5_payload_is_refused() {
        let wire = build_backup_payload(vec![0u8; 16], 0, vec![0u8; 32], vec![], Vec::new(), None)
            .expect("build");
        assert_eq!(wire[0], 6);
        assert_eq!(wire[0], lakko_backup::PAYLOAD_VERSION);

        // The retired v5 layout for the same empty payload: two extra
        // zero-length snapshot fields between the contacts and the group
        // ids. Refused as stored, and refused when stamped as current.
        let mut v5 = vec![5u8];
        v5.extend_from_slice(&[0u8; 16 + 8 + 32]);
        v5.extend_from_slice(&[0u8; 4]); // accepted_contacts_count
        v5.extend_from_slice(&[0u8; 4]); // mls_snapshots_count
        v5.extend_from_slice(&[0u8; 4]); // whole_storage_snapshot_length
        v5.extend_from_slice(&[0u8; 4]); // known_group_ids_count
        v5.extend_from_slice(&[0u8; 4]); // synced_state_length
        assert!(
            matches!(parse_backup_payload(v5.clone()), Err(FfiError::Backup(_))),
            "a v5 payload parsed"
        );
        v5[0] = lakko_backup::PAYLOAD_VERSION;
        assert!(
            matches!(parse_backup_payload(v5), Err(FfiError::Backup(_))),
            "a v5 body stamped v6 parsed"
        );
    }

    #[test]
    fn an_over_long_group_id_is_refused() {
        let err = build_backup_payload(
            vec![0u8; 16],
            0,
            vec![0u8; 32],
            vec![],
            vec![vec![0u8; lakko_backup::payload::MAX_GROUP_ID_LEN + 1]],
            None,
        )
        .expect_err("over-long group id accepted");
        assert!(format!("{err}").contains("group_id length"));
    }

    #[test]
    fn wrong_length_account_id_rejected() {
        let err = build_backup_payload(vec![0u8; 8], 0, vec![0u8; 32], vec![], Vec::new(), None)
            .expect_err("wrong len");
        match err {
            FfiError::Backup(msg) => assert!(msg.contains("16 bytes")),
            other => panic!("expected Backup variant, got {other:?}"),
        }
    }

    // ------------------------------------------------------------------------
    // Backup-head verify (ADR-0037)
    // ------------------------------------------------------------------------

    use lakko_crypto::signing::SigningKey;

    fn signed_head(
        signing: &SigningKey,
        account: [u8; 16],
        version: u64,
        env_hash: [u8; 32],
        signed_at_millis: i64,
    ) -> Vec<u8> {
        lakko_attest::BackupHead::new(account, version, env_hash, signed_at_millis)
            .sign(signing)
            .to_vec()
    }

    #[test]
    fn backup_head_verify_round_trips_through_ffi() {
        let signing = SigningKey::from_seed(&[0x11; 32]);
        let key = signing.verifying_key().to_bytes().to_vec();
        let account = [0xAA; 16];
        let env_hash = [0xBB; 32];
        let wire = signed_head(&signing, account, 7, env_hash, 1_000_000);
        let head = verify_backup_head(
            wire,
            key,
            account.to_vec(),
            env_hash.to_vec(),
            1_000_000, // now == signed_at → fresh
            10_000,
        )
        .expect("valid backup head verifies");
        assert_eq!(head.backup_version, 7);
        assert_eq!(head.envelope_hash, env_hash.to_vec());
        assert_eq!(head.signed_at_millis, 1_000_000);
    }

    #[test]
    fn backup_head_stale_maps_to_distinct_override_variant() {
        // A head that verifies authentically but is outside the freshness window
        // must surface as BackupHeadStale (override), NOT the hard-fail Backup
        // class — the load-bearing discriminability for the §6 restore policy.
        let signing = SigningKey::from_seed(&[0x11; 32]);
        let key = signing.verifying_key().to_bytes().to_vec();
        let account = [0xAA; 16];
        let env_hash = [0xBB; 32];
        let wire = signed_head(&signing, account, 7, env_hash, 1_000_000);
        let err = verify_backup_head(
            wire,
            key,
            account.to_vec(),
            env_hash.to_vec(),
            9_000_000, // far past signed_at + window
            10_000,
        )
        .expect_err("stale head is rejected");
        assert!(
            matches!(err, FfiError::BackupHeadStale),
            "stale must be the distinct override variant, got {err:?}"
        );
    }

    #[test]
    fn backup_head_invalid_maps_to_hard_fail_closed_variant() {
        let signing = SigningKey::from_seed(&[0x11; 32]);
        let account = [0xAA; 16];
        let env_hash = [0xBB; 32];
        let wire = signed_head(&signing, account, 7, env_hash, 1_000_000);

        // Wrong verifying key → signature does not verify → hard fail-closed.
        let other_key = SigningKey::from_seed(&[0x22; 32])
            .verifying_key()
            .to_bytes()
            .to_vec();
        let err = verify_backup_head(
            wire.clone(),
            other_key,
            account.to_vec(),
            env_hash.to_vec(),
            1_000_000,
            10_000,
        )
        .expect_err("wrong key is rejected");
        assert!(
            matches!(err, FfiError::Backup(_)),
            "a bad signature must be the hard-fail Backup class, got {err:?}"
        );

        // Envelope-hash mismatch → hard fail-closed (a tamper signal), NOT stale.
        let key = signing.verifying_key().to_bytes().to_vec();
        let err = verify_backup_head(
            wire,
            key,
            account.to_vec(),
            [0x00; 32].to_vec(),
            1_000_000,
            10_000,
        )
        .expect_err("envelope-hash mismatch is rejected");
        assert!(
            matches!(err, FfiError::Backup(_)),
            "an envelope-hash mismatch must be the hard-fail Backup class, got {err:?}"
        );
    }

    #[test]
    fn backup_head_binding_ignores_freshness_and_envelope_hash() {
        // The binding-only entry point performs no freshness / hash check, so a
        // head verify_backup_head would reject as stale still passes here.
        let signing = SigningKey::from_seed(&[0x11; 32]);
        let key = signing.verifying_key().to_bytes().to_vec();
        let account = [0xAA; 16];
        let wire = signed_head(&signing, account, 42, [0xBB; 32], 1_000_000);
        let head = verify_backup_head_binding(wire, key, account.to_vec())
            .expect("binding check ignores freshness");
        assert_eq!(head.backup_version, 42);
    }

    #[test]
    fn backup_envelope_hash_matches_core() {
        let bytes = vec![0x5Au8; 128];
        assert_eq!(
            backup_envelope_hash(bytes.clone()),
            lakko_backup::chain::envelope_hash(&bytes).to_vec()
        );
    }

    // ------------------------------------------------------------------------
    // Container-blob framing (ADR-0037 §4)
    // ------------------------------------------------------------------------

    #[test]
    fn frame_then_parse_round_trips_through_ffi() {
        let head = vec![0xABu8; 129];
        let envelope = vec![0xCDu8; 64];
        let blob = frame_backup_blob(head.clone(), envelope.clone()).expect("frame");
        assert_eq!(blob[0], 0xB1);
        let parsed = parse_backup_blob(blob).expect("parse");
        assert!(parsed.framed);
        assert_eq!(parsed.head, Some(head));
        assert_eq!(parsed.envelope, envelope);
    }

    #[test]
    fn legacy_envelope_parses_as_unframed_through_ffi() {
        // A v1 envelope begins with schema byte 0x01 — not the container tag.
        let raw = vec![0x01u8, 0x02, 0x03];
        let parsed = parse_backup_blob(raw.clone()).expect("parse");
        assert!(!parsed.framed);
        assert_eq!(parsed.head, None);
        assert_eq!(parsed.envelope, raw);
    }

    #[test]
    fn truncated_framed_blob_is_hard_fail_closed_through_ffi() {
        // Tag present but too short to hold a head → hard-fail Backup class.
        let mut blob = vec![0xB1u8];
        blob.extend_from_slice(&[0u8; 10]);
        let err = parse_backup_blob(blob).expect_err("truncated framed blob is rejected");
        assert!(
            matches!(err, FfiError::Backup(_)),
            "a truncated framed blob must be the hard-fail Backup class, got {err:?}"
        );
    }

    #[test]
    fn frame_rejects_wrong_length_head_through_ffi() {
        let err = frame_backup_blob(vec![0u8; 128], vec![]).expect_err("wrong-length head");
        assert!(matches!(err, FfiError::Backup(_)), "got {err:?}");
    }
}
