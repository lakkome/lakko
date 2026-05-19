// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for `lakko-backup` (Phase 2a1c).
//!
//! Exposes three primitives to the web client:
//!
//! - [`build_backup_payload`] — assemble the structured
//!   `BackupPayload` from the parts the client has at hand
//!   (account_id, ed25519_seed, accepted-contacts list). Returns
//!   the wire bytes ready to feed into [`seal_backup`].
//!
//! - [`seal_backup`] — encrypt payload bytes under a KEK derived
//!   from the BIP39 recovery phrase. Returns the on-the-wire
//!   envelope ready to PUT to `services/storage`'s `/backup`
//!   endpoint.
//!
//! - [`open_backup`] — inverse of [`seal_backup`]. Recovers the
//!   payload bytes given the phrase + the envelope. The web
//!   client follows up with [`parse_backup_payload`] to extract
//!   structured fields for the restore flow.
//!
//! The split (`build / seal` vs `open / parse`) mirrors how the
//! payload + envelope are layered in `lakko-backup`: the envelope
//! is a pure-AEAD wrapper, the payload is a structured wire
//! shape. Web callers can sequence them at the JS layer without
//! the wasm boundary hiding the layering.

use lakko_backup::{
    open as inner_open, payload::AcceptedContactRecord, seal as inner_seal, BackupPayload,
};
use wasm_bindgen::prelude::*;

fn err_to_js(e: lakko_backup::BackupError) -> JsError {
    JsError::new(&format!("{e}"))
}

// ============================================================================
// Payload assembly + parsing
// ============================================================================

/// One row in the accepted-contacts list passed to
/// [`build_backup_payload`]. Plain JS-friendly fields; the wasm
/// boundary copies bytes in / out.
///
/// We expose this as a wasm-bindgen `struct` (vs a plain JS
/// object) so the field-length contracts are enforced at the
/// boundary — wrong-length pubkeys fail constructor validation
/// instead of producing a corrupt payload further down the line.
#[wasm_bindgen]
pub struct BackupContact {
    ed25519_pubkey: Vec<u8>,
    x25519_pubkey: Vec<u8>,
    display_name: String,
}

#[wasm_bindgen]
impl BackupContact {
    /// Construct a contact record. `ed25519_pubkey` and
    /// `x25519_pubkey` must both be 32 bytes; `display_name` is
    /// any UTF-8 string up to
    /// `lakko_backup::payload::MAX_DISPLAY_NAME_LEN` bytes.
    ///
    /// # Errors
    ///
    /// `JsError` on wrong-length pubkey or over-cap display name.
    #[wasm_bindgen(constructor)]
    pub fn new(
        ed25519_pubkey: Vec<u8>,
        x25519_pubkey: Vec<u8>,
        display_name: String,
    ) -> Result<BackupContact, JsError> {
        if ed25519_pubkey.len() != 32 {
            return Err(JsError::new("ed25519_pubkey must be 32 bytes"));
        }
        if x25519_pubkey.len() != 32 {
            return Err(JsError::new("x25519_pubkey must be 32 bytes"));
        }
        if display_name.len() > lakko_backup::payload::MAX_DISPLAY_NAME_LEN {
            return Err(JsError::new(&format!(
                "display_name length {} exceeds cap {}",
                display_name.len(),
                lakko_backup::payload::MAX_DISPLAY_NAME_LEN,
            )));
        }
        Ok(Self {
            ed25519_pubkey,
            x25519_pubkey,
            display_name,
        })
    }

    /// Raw 32-byte Ed25519 verifying key.
    #[wasm_bindgen(getter, js_name = ed25519Pubkey)]
    pub fn ed25519_pubkey(&self) -> Vec<u8> {
        self.ed25519_pubkey.clone()
    }

    /// Raw 32-byte X25519 HPKE public key.
    #[wasm_bindgen(getter, js_name = x25519Pubkey)]
    pub fn x25519_pubkey(&self) -> Vec<u8> {
        self.x25519_pubkey.clone()
    }

    /// User-chosen display label (UTF-8, may be empty).
    #[wasm_bindgen(getter, js_name = displayName)]
    pub fn display_name(&self) -> String {
        self.display_name.clone()
    }
}

impl BackupContact {
    /// Convert into the inner record shape `lakko-backup` expects.
    /// `pub(crate)` so other wasm modules can build records without
    /// re-stating the field-length contracts.
    fn into_inner(self) -> AcceptedContactRecord {
        let mut e = [0u8; 32];
        e.copy_from_slice(&self.ed25519_pubkey);
        let mut x = [0u8; 32];
        x.copy_from_slice(&self.x25519_pubkey);
        AcceptedContactRecord {
            ed25519_pubkey: e,
            x25519_pubkey: x,
            display_name: self.display_name,
        }
    }
}

/// Build the structured backup payload wire bytes that go into
/// [`seal_backup`].
///
/// Caller supplies:
///
/// - `account_id` — owner's 16-byte UUIDv7.
/// - `created_at_ms` — wall-clock UNIX ms at backup-creation time
///   (caller passes `Date.now()` so the server-side rejection of
///   stale uploads — should it ever land — has a stable reference).
/// - `ed25519_seed` — 32-byte identity signing seed.
/// - `contacts` — `Array<BackupContact>` produced by the
///   constructor above. Empty array is fine — degenerate case
///   for a brand-new account with no accepted contacts yet.
///
/// `mls_snapshots` (the per-group reserved slot) stays empty;
/// `whole_storage_snapshot` (Phase 2a3d1) carries the bytes
/// `Identity.snapshotStorage()` emits for the wholesale MLS
/// provider state.
///
/// # Errors
///
/// `JsError` on wrong-length `account_id` (≠ 16) /
/// `ed25519_seed` (≠ 32), or any of the [`BackupPayload::encode`]
/// failure modes (all unreachable in practice given valid input).
#[wasm_bindgen(js_name = buildBackupPayload)]
pub fn build_backup_payload(
    account_id: &[u8],
    created_at_ms: u64,
    ed25519_seed: &[u8],
    contacts: Vec<BackupContact>,
    // Phase 2a3d1 — whole-storage OpenMLS provider snapshot
    // from `Identity.snapshotStorage()`. Pass an empty
    // `Uint8Array` when no MLS state is published yet
    // (signup baseline pre-DeviceGroup-bootstrap, etc.).
    whole_storage_snapshot: &[u8],
) -> Result<Vec<u8>, JsError> {
    if account_id.len() != 16 {
        return Err(JsError::new("account_id must be 16 bytes"));
    }
    if ed25519_seed.len() != 32 {
        return Err(JsError::new("ed25519_seed must be 32 bytes"));
    }
    let mut acct = [0u8; 16];
    acct.copy_from_slice(account_id);
    let mut seed = [0u8; 32];
    seed.copy_from_slice(ed25519_seed);

    let accepted_contacts: Vec<AcceptedContactRecord> = contacts
        .into_iter()
        .map(BackupContact::into_inner)
        .collect();

    let payload = BackupPayload {
        account_id: acct,
        created_at_ms,
        ed25519_seed: seed,
        accepted_contacts,
        mls_snapshots: Vec::new(),
        whole_storage_snapshot: whole_storage_snapshot.to_vec(),
    };
    payload.encode().map_err(err_to_js)
}

/// Structured view of a decoded backup payload. Built on the
/// receive side by [`parse_backup_payload`] so the web client
/// can pull out the fields it needs to restore.
#[wasm_bindgen]
pub struct ParsedBackupPayload {
    inner: BackupPayload,
}

#[wasm_bindgen]
impl ParsedBackupPayload {
    /// Raw 16-byte UUIDv7 account_id from the payload.
    #[wasm_bindgen(getter, js_name = accountId)]
    pub fn account_id(&self) -> Vec<u8> {
        self.inner.account_id.to_vec()
    }

    /// Wall-clock UNIX ms recorded at backup-creation time.
    #[wasm_bindgen(getter, js_name = createdAtMs)]
    pub fn created_at_ms(&self) -> u64 {
        self.inner.created_at_ms
    }

    /// Raw 32-byte identity signing seed.
    #[wasm_bindgen(getter, js_name = ed25519Seed)]
    pub fn ed25519_seed(&self) -> Vec<u8> {
        self.inner.ed25519_seed.to_vec()
    }

    /// Accepted-contacts list as a JS array of `BackupContact`.
    #[wasm_bindgen(getter)]
    pub fn contacts(&self) -> Vec<BackupContact> {
        self.inner
            .accepted_contacts
            .iter()
            .map(|c| BackupContact {
                ed25519_pubkey: c.ed25519_pubkey.to_vec(),
                x25519_pubkey: c.x25519_pubkey.to_vec(),
                display_name: c.display_name.clone(),
            })
            .collect()
    }

    /// Phase 2a3d1 — whole-storage OpenMLS provider snapshot. Empty
    /// `Uint8Array` when the backup pre-dates 2a3d or had no MLS
    /// state published at upload time. The restore path feeds
    /// this into `Identity.restoreStorage(...)`.
    #[wasm_bindgen(getter, js_name = wholeStorageSnapshot)]
    pub fn whole_storage_snapshot(&self) -> Vec<u8> {
        self.inner.whole_storage_snapshot.clone()
    }
}

/// Decode the wire bytes [`seal_backup`] produced (post-AEAD-open
/// via [`open_backup`]) into a structured `ParsedBackupPayload`.
///
/// # Errors
///
/// `JsError` on malformed wire bytes (wrong version, truncated,
/// trailing bytes, display-name over cap, non-UTF-8 name, …).
#[wasm_bindgen(js_name = parseBackupPayload)]
pub fn parse_backup_payload(wire: &[u8]) -> Result<ParsedBackupPayload, JsError> {
    let inner = BackupPayload::decode(wire).map_err(err_to_js)?;
    Ok(ParsedBackupPayload { inner })
}

// ============================================================================
// Envelope seal / open
// ============================================================================

/// Encrypt `payload_bytes` under a KEK derived from `phrase`
/// (BIP39 recovery phrase, UTF-8 bytes). Returns the on-the-wire
/// envelope ready to PUT to `services/storage`'s `/backup`
/// endpoint.
///
/// `phrase` should be the canonical-form recovery phrase
/// (normalised whitespace + lowercase); this function treats the
/// slice as opaque IKM into Argon2id.
///
/// # Errors
///
/// `JsError` on Argon2id parameter failure (cannot happen for
/// the fixed-params API) or oversize payload (unreachable in
/// practice — the wire field is u32be).
#[wasm_bindgen(js_name = sealBackup)]
pub fn seal_backup(phrase: &[u8], payload_bytes: &[u8]) -> Result<Vec<u8>, JsError> {
    inner_seal(phrase, payload_bytes).map_err(err_to_js)
}

/// Decrypt an on-the-wire backup envelope back to its payload
/// bytes. Wrong phrase / tampered blob folds into a single
/// failure variant — see [`lakko_backup::BackupError`].
///
/// # Errors
///
/// `JsError` for wrong-phrase / tampered-blob / malformed-wire
/// failures.
#[wasm_bindgen(js_name = openBackup)]
pub fn open_backup(phrase: &[u8], wire: &[u8]) -> Result<Vec<u8>, JsError> {
    inner_open(phrase, wire).map_err(err_to_js)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_contact() -> BackupContact {
        BackupContact::new(vec![0x01; 32], vec![0x02; 32], "alice".to_string()).unwrap()
    }

    #[test]
    fn build_then_parse_round_trips() {
        let account_id = vec![0xA1u8; 16];
        let seed = vec![0x42u8; 32];
        let contacts = vec![sample_contact()];
        let snapshot = vec![0xEFu8; 8];
        let wire = build_backup_payload(&account_id, 1_700_000_000_000, &seed, contacts, &snapshot)
            .unwrap();
        let parsed = parse_backup_payload(&wire).unwrap();
        assert_eq!(parsed.account_id(), account_id);
        assert_eq!(parsed.created_at_ms(), 1_700_000_000_000);
        assert_eq!(parsed.ed25519_seed(), seed);
        let recovered = parsed.contacts();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].display_name(), "alice");
        assert_eq!(parsed.whole_storage_snapshot(), snapshot);
    }

    #[test]
    fn seal_then_open_round_trips_through_envelope() {
        let phrase = b"recovery phrase 24 words";
        let payload_bytes = b"opaque payload";
        let wire = seal_backup(phrase, payload_bytes).unwrap();
        let opened = open_backup(phrase, &wire).unwrap();
        assert_eq!(opened, payload_bytes);
    }

    #[test]
    fn end_to_end_through_wasm_surface() {
        let phrase = b"recovery phrase 24 words for the end to end test";
        let account_id = vec![0xCDu8; 16];
        let seed = vec![0x77u8; 32];
        let contacts = vec![sample_contact()];
        let payload = build_backup_payload(&account_id, 99, &seed, contacts, &[]).unwrap();
        let envelope = seal_backup(phrase, &payload).unwrap();
        let opened = open_backup(phrase, &envelope).unwrap();
        let parsed = parse_backup_payload(&opened).unwrap();
        assert_eq!(parsed.account_id(), account_id);
        assert_eq!(parsed.ed25519_seed(), seed);
    }

    // Error-path tests intentionally omitted from the host-side
    // `cargo test` runner. `JsError::new` panics on non-wasm
    // targets (documented wasm-bindgen 0.2 host-runtime stub
    // behaviour); error coverage lives in the underlying
    // `lakko-backup` crate's own unit tests. The
    // `wasm-pack test --node` runner exercises the JS-boundary
    // error paths.
}
