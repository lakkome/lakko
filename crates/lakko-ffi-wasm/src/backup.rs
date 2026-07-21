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

use lakko_attest::BackupHeadError;
use lakko_backup::{
    open as inner_open, payload::AcceptedContactRecord, seal as inner_seal, BackupBlob,
    BackupPayload,
};
use lakko_crypto::signing::VerifyingKey;
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
    /// Optional 16-byte account_id (ADR-0021); `None` when unknown.
    account_id: Option<Vec<u8>>,
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
        account_id: Option<Vec<u8>>,
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
        if let Some(ref id) = account_id {
            if id.len() != 16 {
                return Err(JsError::new("account_id must be 16 bytes when present"));
            }
        }
        Ok(Self {
            ed25519_pubkey,
            x25519_pubkey,
            display_name,
            account_id,
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

    /// Optional 16-byte account_id (ADR-0021); `undefined` when unknown.
    #[wasm_bindgen(getter, js_name = accountId)]
    pub fn account_id(&self) -> Option<Vec<u8>> {
        self.account_id.clone()
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
        // Constructor validated len == 16 when present; a wrong-length
        // value degrades to None rather than panicking.
        let account_id = self
            .account_id
            .and_then(|v| <[u8; 16]>::try_from(v.as_slice()).ok());
        AcceptedContactRecord {
            ed25519_pubkey: e,
            x25519_pubkey: x,
            display_name: self.display_name,
            account_id,
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
                account_id: c.account_id.map(|id| id.to_vec()),
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

// ============================================================================
// Backup-head verify + envelope-hash helper (ADR-0037, G-11)
// ============================================================================
//
// The rollback defence for a fresh-install restore. An auth-signed `BackupHead`
// commits an account's LATEST backup envelope (highest monotone
// `backup_version`); the web client verifies it against the CLIENT-PINNED auth
// attest verifying key (the same `DeviceAttestKey` it already pins for device /
// genesis attestations — ADR-0037 §3 reuses the key across all three roles,
// kept safe by distinct domain separators). Verify only — signing is auth-only,
// server-side (same posture as `verifyGenesisAttestation`).
//
// **Error discriminability (ADR-0037 §6).** `JsError` carries only a message
// string — there is no structured discriminant on the JS boundary — but the
// restore policy MUST tell a `Stale` head (user-acknowledged override) from an
// `Invalid` one (hard fail-closed). So each thrown error message is prefixed
// with a STABLE machine-readable code — `stale: …` or `invalid: …` — and the
// web S5 client branches on that code (`err.message.startsWith("stale:")`),
// never on the fragile human-readable detail. This is a documented convention
// on the thrown message, not a new error channel.

/// Stable code prefixed onto a thrown backup-head verify error when the head
/// verified authentically but sits outside the freshness window — the
/// user-acknowledged override class (ADR-0037 §6).
const STALE_CODE: &str = "stale";

/// Stable code prefixed onto a thrown backup-head verify error for every
/// hard-fail-closed cause (malformed head, bad signature, account or
/// envelope-hash mismatch, bad key / wrong-length input).
const INVALID_CODE: &str = "invalid";

/// Build a hard-fail-closed (`invalid`) coded error message.
fn invalid(msg: &str) -> String {
    format!("{INVALID_CODE}: {msg}")
}

/// Map a `BackupHeadError` onto a coded message: `Stale` → the override class,
/// everything else → the hard-fail-closed class.
fn map_head_err(e: &BackupHeadError) -> String {
    match e {
        BackupHeadError::Stale => format!("{STALE_CODE}: {e}"),
        other => format!("{INVALID_CODE}: {other}"),
    }
}

/// Parse the auth attest verifying-key bytes into a [`VerifyingKey`], mapping a
/// bad key to the hard-fail-closed (`invalid`) class.
fn parse_attest_key(bytes: &[u8]) -> Result<VerifyingKey, String> {
    VerifyingKey::from_bytes(bytes)
        .map_err(|_| invalid("attest verifying key is not a valid Ed25519 key"))
}

/// Structured view of a verified backup head — the wasm projection of
/// `lakko_attest::BackupHead`. Returned by [`verify_backup_head`] and
/// [`verify_backup_head_binding`] on success.
#[wasm_bindgen]
pub struct VerifiedBackupHead {
    inner: lakko_attest::BackupHead,
}

impl From<lakko_attest::BackupHead> for VerifiedBackupHead {
    fn from(inner: lakko_attest::BackupHead) -> Self {
        Self { inner }
    }
}

#[wasm_bindgen]
impl VerifiedBackupHead {
    /// The auth-assigned monotone backup version. Strictly increasing per
    /// account; the caller refuses any head below its persisted floor
    /// (`lastKnownVersion`) as a user-acknowledged rollback override.
    #[wasm_bindgen(getter, js_name = backupVersion)]
    pub fn backup_version(&self) -> u64 {
        self.inner.backup_version
    }

    /// Raw 32-byte `BLAKE3` of the envelope portion the head commits to. Binds
    /// the head to exactly the served envelope.
    #[wasm_bindgen(getter, js_name = envelopeHash)]
    pub fn envelope_hash(&self) -> Vec<u8> {
        self.inner.envelope_hash.to_vec()
    }

    /// The mint timestamp, Unix **milliseconds** (BigInt on the JS side). The
    /// client applies its own staleness window against this (it is not an
    /// expiry).
    #[wasm_bindgen(getter, js_name = signedAtMillis)]
    pub fn signed_at_millis(&self) -> i64 {
        self.inner.signed_at_unix_millis
    }
}

/// Verify a backup head's signature, `accountId` binding, `envelopeHash`
/// binding, AND freshness — the full check the web client applies on restore
/// (ADR-0037 §6). Returns the parsed [`VerifiedBackupHead`] so the caller reads
/// `backupVersion` for its own monotonic-floor comparison against its persisted
/// `lastKnownVersion`.
///
/// `head` is the raw backup-head bytes (the container prefix of the served
/// backup blob). `attestVerifyingKey` is the 32-byte client-pinned auth attest
/// key. `expectedAccountId` is the client's authenticated 16-byte account id;
/// `expectedEnvelopeHash` is the 32-byte [`backup_envelope_hash`] of the served
/// envelope. `nowMillis` and `maxStalenessMillis` are Unix milliseconds.
///
/// Throws a coded `JsError`: `stale: …` when the head verified authentically but
/// is outside the freshness window (the caller may offer a user-acknowledged
/// override); `invalid: …` for every hard fail-closed cause (a malformed /
/// wrong-length / wrong-version head, a signature that does not verify, an
/// `accountId` / `envelopeHash` mismatch, or a bad key / wrong-length
/// `expectedAccountId` / `expectedEnvelopeHash`). Branch on the code prefix.
#[wasm_bindgen(js_name = verifyBackupHead)]
pub fn verify_backup_head(
    head: &[u8],
    attest_verifying_key: &[u8],
    expected_account_id: &[u8],
    expected_envelope_hash: &[u8],
    now_millis: i64,
    max_staleness_millis: i64,
) -> Result<VerifiedBackupHead, JsError> {
    verify_backup_head_inner(
        head,
        attest_verifying_key,
        expected_account_id,
        expected_envelope_hash,
        now_millis,
        max_staleness_millis,
    )
    .map(VerifiedBackupHead::from)
    .map_err(|msg| JsError::new(&msg))
}

/// The `JsError`-free core of [`verify_backup_head`]. Returns `Err(coded_msg)`
/// on any failure so the negative paths — including the `stale` vs `invalid`
/// code split — are exercisable on the host target (`JsError::new` panics on a
/// non-wasm target per wasm-bindgen-0.2's "non-wasm targets" rule).
fn verify_backup_head_inner(
    head: &[u8],
    attest_verifying_key: &[u8],
    expected_account_id: &[u8],
    expected_envelope_hash: &[u8],
    now_millis: i64,
    max_staleness_millis: i64,
) -> Result<lakko_attest::BackupHead, String> {
    let verifying = parse_attest_key(attest_verifying_key)?;
    let expected_account: [u8; 16] = expected_account_id
        .try_into()
        .map_err(|_| invalid("expected_account_id must be 16 bytes"))?;
    let expected_envelope_hash: [u8; 32] = expected_envelope_hash
        .try_into()
        .map_err(|_| invalid("expected_envelope_hash must be 32 bytes"))?;
    lakko_attest::verify_backup_head(
        head,
        &verifying,
        &expected_account,
        &expected_envelope_hash,
        now_millis,
        max_staleness_millis,
    )
    .map_err(|e| map_head_err(&e))
}

/// Verify a backup head's signature and `accountId` binding ONLY — no
/// `envelopeHash` or freshness check (ADR-0037 §6). For a caller that applies
/// its own freshness and monotonic-floor policy (e.g. one that wants the parsed
/// `backupVersion` before it has hashed the served envelope). Returns the parsed
/// [`VerifiedBackupHead`].
///
/// Throws a coded `invalid: …` `JsError` on a malformed / wrong-length /
/// wrong-version head, a signature that does not verify, an `accountId`
/// mismatch, or a bad key / wrong-length `expectedAccountId`. This entry point
/// performs no freshness check, so it never throws the `stale: …` code.
#[wasm_bindgen(js_name = verifyBackupHeadBinding)]
pub fn verify_backup_head_binding(
    head: &[u8],
    attest_verifying_key: &[u8],
    expected_account_id: &[u8],
) -> Result<VerifiedBackupHead, JsError> {
    verify_backup_head_binding_inner(head, attest_verifying_key, expected_account_id)
        .map(VerifiedBackupHead::from)
        .map_err(|msg| JsError::new(&msg))
}

/// The `JsError`-free core of [`verify_backup_head_binding`] — host-testable,
/// same rationale as [`verify_backup_head_inner`].
fn verify_backup_head_binding_inner(
    head: &[u8],
    attest_verifying_key: &[u8],
    expected_account_id: &[u8],
) -> Result<lakko_attest::BackupHead, String> {
    let verifying = parse_attest_key(attest_verifying_key)?;
    let expected_account: [u8; 16] = expected_account_id
        .try_into()
        .map_err(|_| invalid("expected_account_id must be 16 bytes"))?;
    lakko_attest::verify_backup_head_binding(head, &verifying, &expected_account)
        .map_err(|e| map_head_err(&e))
}

/// Compute the `BLAKE3` envelope hash (`lakko_backup::chain::envelope_hash`)
/// over the served backup envelope bytes — the 32-byte value the web client
/// feeds as `expectedEnvelopeHash` to [`verify_backup_head`] at restore, and
/// posts to `POST /auth/backup/head` at mint. Exposed so all three clients
/// compute the identical hash without re-implementing BLAKE3. Returns the
/// 32-byte hash.
#[wasm_bindgen(js_name = backupEnvelopeHash)]
#[must_use]
pub fn backup_envelope_hash(envelope_bytes: &[u8]) -> Vec<u8> {
    lakko_backup::chain::envelope_hash(envelope_bytes).to_vec()
}

// ============================================================================
// Container-blob framing (ADR-0037 §4 / S4, G-11)
// ============================================================================
//
// The auth-signed backup head rides INSIDE the one storage backup blob so the
// head and the envelope it commits swap together in a single atomic PUT:
// `CONTAINER_TAG(0xB1) ‖ head(129) ‖ envelope`. The web client frames on upload
// and peeks byte 0 on restore to tell a framed blob from a legacy raw envelope.
// `services/storage` is untouched — the blob stays opaque (invariant #2). This
// surface is FRAMING ONLY; the caller verifies a framed head via
// `verifyBackupHead` and applies the §6 fail-open / fail-closed policy.
//
// A truncated framed blob (tag present but too short for a head) throws the
// `invalid: …` coded `JsError` — the hard fail-closed "Invalid" case (§6),
// consistent with the backup-head verify error convention above.

/// Structured view of a parsed backup blob — the wasm projection of
/// `lakko_backup::BackupBlob`. Returned by [`parse_backup_blob`].
#[wasm_bindgen]
#[derive(Debug)]
pub struct ParsedBackupBlob {
    framed: bool,
    head: Option<Vec<u8>>,
    envelope: Vec<u8>,
}

#[wasm_bindgen]
impl ParsedBackupBlob {
    /// `true` when the blob carried the `0xB1` container tag and a full head;
    /// `false` for a legacy raw envelope with no head.
    #[wasm_bindgen(getter)]
    pub fn framed(&self) -> bool {
        self.framed
    }

    /// The raw backup-head bytes when `framed` is `true`, else `undefined`.
    /// Still **unverified** — feed to [`verify_backup_head`] against the pinned
    /// auth attest key.
    #[wasm_bindgen(getter)]
    pub fn head(&self) -> Option<Vec<u8>> {
        self.head.clone()
    }

    /// The envelope portion (the whole blob when `framed` is `false`; the tail
    /// after the tag + head when `framed` is `true`). Feed to [`open_backup`].
    #[wasm_bindgen(getter)]
    pub fn envelope(&self) -> Vec<u8> {
        self.envelope.clone()
    }
}

/// Frame an auth-signed backup head and its envelope into the single storage
/// blob `CONTAINER_TAG ‖ head ‖ envelope` to PUT to `services/storage` on the
/// mint / upload path (ADR-0037 §4). `head` must be the raw
/// `lakko_attest::BACKUP_HEAD_LEN`-byte head bytes.
///
/// # Errors
///
/// Throws a coded `invalid: …` `JsError` if `head` is the wrong length (hard
/// fail-closed).
#[wasm_bindgen(js_name = frameBackupBlob)]
pub fn frame_backup_blob(head: &[u8], envelope: &[u8]) -> Result<Vec<u8>, JsError> {
    frame_backup_blob_inner(head, envelope).map_err(|m| JsError::new(&m))
}

/// The `JsError`-free core of [`frame_backup_blob`] — host-testable, same
/// rationale as [`verify_backup_head_inner`].
fn frame_backup_blob_inner(head: &[u8], envelope: &[u8]) -> Result<Vec<u8>, String> {
    lakko_backup::frame_backup_blob(head, envelope).map_err(|e| invalid(&e.to_string()))
}

/// Peek a stored backup blob's container framing on the restore path, before any
/// head verification or AEAD-open (ADR-0037 §4). Returns a [`ParsedBackupBlob`]
/// carrying the framed-vs-legacy kind, the (unverified) head bytes when framed,
/// and the envelope bytes.
///
/// # Errors
///
/// Throws a coded `invalid: …` `JsError` when the blob carries the `0xB1`
/// container tag but is too short to hold a full head — active tamper /
/// corruption of a framed blob (ADR-0037 §6 "Invalid"). A legacy (non-`0xB1`)
/// or empty blob is **not** an error: it returns a `Legacy` [`ParsedBackupBlob`]
/// (the §6 "Absent → fail-open" lane).
#[wasm_bindgen(js_name = parseBackupBlob)]
pub fn parse_backup_blob(blob: &[u8]) -> Result<ParsedBackupBlob, JsError> {
    parse_backup_blob_inner(blob).map_err(|m| JsError::new(&m))
}

/// The `JsError`-free core of [`parse_backup_blob`] — host-testable, same
/// rationale as [`verify_backup_head_inner`].
fn parse_backup_blob_inner(blob: &[u8]) -> Result<ParsedBackupBlob, String> {
    match lakko_backup::parse_backup_blob(blob).map_err(|e| invalid(&e.to_string()))? {
        BackupBlob::Framed { head, envelope } => Ok(ParsedBackupBlob {
            framed: true,
            head: Some(head.to_vec()),
            envelope,
        }),
        BackupBlob::Legacy(envelope) => Ok(ParsedBackupBlob {
            framed: false,
            head: None,
            envelope,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_contact() -> BackupContact {
        BackupContact::new(vec![0x01; 32], vec![0x02; 32], "alice".to_string(), None).unwrap()
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
    fn account_id_round_trips_through_wasm_surface() {
        // ADR-0021: a contact carrying a 16-byte account_id round-trips
        // through the wasm BackupContact -> build -> parse surface, and a
        // contact without one stays absent.
        let with_id = BackupContact::new(
            vec![0x01; 32],
            vec![0x02; 32],
            "alice".to_string(),
            Some(vec![0xAB; 16]),
        )
        .unwrap();
        let without_id = sample_contact();
        let account_id = vec![0xA1u8; 16];
        let seed = vec![0x42u8; 32];
        let wire =
            build_backup_payload(&account_id, 1, &seed, vec![with_id, without_id], &[]).unwrap();
        let parsed = parse_backup_payload(&wire).unwrap();
        let recovered = parsed.contacts();
        assert_eq!(recovered.len(), 2);
        assert_eq!(recovered[0].account_id(), Some(vec![0xAB; 16]));
        assert_eq!(recovered[1].account_id(), None);
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

    // ------------------------------------------------------------------------
    // Backup-head verify (ADR-0037). Exercised through the `JsError`-free
    // `_inner` cores so the `stale` vs `invalid` code split runs on the host.
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
    fn backup_head_verify_round_trips() {
        let signing = SigningKey::from_seed(&[0x11; 32]);
        let key = signing.verifying_key().to_bytes();
        let account = [0xAA; 16];
        let env_hash = [0xBB; 32];
        let wire = signed_head(&signing, account, 7, env_hash, 1_000_000);
        let head =
            verify_backup_head_inner(&wire, &key, &account, &env_hash, 1_000_000, 10_000).unwrap();
        assert_eq!(head.backup_version, 7);
        // Also exercise the wasm struct projection getters on the host.
        let view = VerifiedBackupHead::from(head);
        assert_eq!(view.backup_version(), 7);
        assert_eq!(view.envelope_hash(), env_hash.to_vec());
        assert_eq!(view.signed_at_millis(), 1_000_000);
    }

    #[test]
    fn backup_head_stale_carries_the_stale_code() {
        let signing = SigningKey::from_seed(&[0x11; 32]);
        let key = signing.verifying_key().to_bytes();
        let account = [0xAA; 16];
        let env_hash = [0xBB; 32];
        let wire = signed_head(&signing, account, 7, env_hash, 1_000_000);
        let err = verify_backup_head_inner(&wire, &key, &account, &env_hash, 9_000_000, 10_000)
            .expect_err("stale head is rejected");
        assert!(
            err.starts_with("stale:"),
            "a stale head must carry the `stale` code, got {err:?}"
        );
    }

    #[test]
    fn backup_head_invalid_causes_carry_the_invalid_code() {
        let signing = SigningKey::from_seed(&[0x11; 32]);
        let key = signing.verifying_key().to_bytes();
        let account = [0xAA; 16];
        let env_hash = [0xBB; 32];
        let wire = signed_head(&signing, account, 7, env_hash, 1_000_000);

        // Wrong verifying key → signature does not verify → invalid.
        let other = SigningKey::from_seed(&[0x22; 32])
            .verifying_key()
            .to_bytes();
        let err = verify_backup_head_inner(&wire, &other, &account, &env_hash, 1_000_000, 10_000)
            .expect_err("wrong key is rejected");
        assert!(err.starts_with("invalid:"), "got {err:?}");

        // Envelope-hash mismatch → tamper signal → invalid, NOT stale.
        let err = verify_backup_head_inner(&wire, &key, &account, &[0x00; 32], 1_000_000, 10_000)
            .expect_err("envelope-hash mismatch is rejected");
        assert!(err.starts_with("invalid:"), "got {err:?}");

        // Bad-length expected account → invalid.
        let err = verify_backup_head_inner(&wire, &key, &[0u8; 8], &env_hash, 1_000_000, 10_000)
            .expect_err("bad account length is rejected");
        assert!(err.starts_with("invalid:"), "got {err:?}");
    }

    #[test]
    fn backup_head_binding_inner_ignores_freshness() {
        let signing = SigningKey::from_seed(&[0x11; 32]);
        let key = signing.verifying_key().to_bytes();
        let account = [0xAA; 16];
        let wire = signed_head(&signing, account, 42, [0xBB; 32], 1_000_000);
        let head = verify_backup_head_binding_inner(&wire, &key, &account)
            .expect("binding check ignores freshness");
        assert_eq!(head.backup_version, 42);
    }

    #[test]
    fn backup_envelope_hash_matches_core() {
        let bytes = vec![0x5Au8; 128];
        assert_eq!(
            backup_envelope_hash(&bytes),
            lakko_backup::chain::envelope_hash(&bytes).to_vec()
        );
    }

    // ------------------------------------------------------------------------
    // Container-blob framing (ADR-0037 §4). Exercised through the `JsError`-free
    // `_inner` cores so the `invalid` error path runs on the host.
    // ------------------------------------------------------------------------

    #[test]
    fn frame_then_parse_round_trips() {
        let head = vec![0xABu8; 129];
        let envelope = vec![0xCDu8; 64];
        let blob = frame_backup_blob_inner(&head, &envelope).expect("frame");
        assert_eq!(blob[0], 0xB1);
        let parsed = parse_backup_blob_inner(&blob).expect("parse");
        assert!(parsed.framed());
        assert_eq!(parsed.head(), Some(head));
        assert_eq!(parsed.envelope(), envelope);
    }

    #[test]
    fn legacy_envelope_parses_as_unframed() {
        // A v1 envelope begins with schema byte 0x01 — not the container tag.
        let raw = vec![0x01u8, 0x02, 0x03];
        let parsed = parse_backup_blob_inner(&raw).expect("parse");
        assert!(!parsed.framed());
        assert_eq!(parsed.head(), None);
        assert_eq!(parsed.envelope(), raw);
    }

    #[test]
    fn truncated_framed_blob_carries_the_invalid_code() {
        // Tag present but too short to hold a head → hard fail-closed `invalid`.
        let mut blob = vec![0xB1u8];
        blob.extend_from_slice(&[0u8; 10]);
        let err = parse_backup_blob_inner(&blob).expect_err("truncated framed blob is rejected");
        assert!(
            err.starts_with("invalid:"),
            "a truncated framed blob must carry the `invalid` code, got {err:?}"
        );
    }

    #[test]
    fn frame_rejects_wrong_length_head_with_invalid_code() {
        let err = frame_backup_blob_inner(&[0u8; 128], &[]).expect_err("wrong-length head");
        assert!(err.starts_with("invalid:"), "got {err:?}");
    }
}
