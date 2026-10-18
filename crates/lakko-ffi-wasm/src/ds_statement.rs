// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen exposure of the [ADR-0057] journal-ownership surface.
//! Mirror of `lakko-ffi-uniffi/src/ds_statement.rs`: the root and registrar
//! derivations, a signer for every ownership statement, and the
//! registrar-seed envelope.
//!
//! Lists cross the boundary flat, as elsewhere on this surface: keys are
//! concatenated 32-byte values, ranks one byte each - `1` member, `2` admin,
//! and in a registrar-change list `0` removes. Epochs are `bigint`s.
//!
//! [ADR-0057]: ../../../docs/adr/0057-journal-ownership.md

use lakko_contacts::registrar_seed::{
    compose_registrar_seed, decode_registrar_seed, RegistrarSeedRank, REGISTRAR_SEED_ENVELOPE_LEN,
};
use lakko_crypto::agreement::{PublicKey as X25519PublicKey, SecretKey as X25519SecretKey};
use lakko_ds_proof::statement::{
    journal_registrar_seed, journal_root_seed, Rank, RegisterMode, RegistrarChange, RegistrarEntry,
    StatementBody, StatementSigner, JOURNAL_ID_LEN, KEY_LEN,
};
use lakko_ds_proof::NONCE_LEN;
use wasm_bindgen::prelude::*;

use crate::mls::Identity;

#[cfg(target_arch = "wasm32")]
fn now_ms() -> u64 {
    let now = js_sys::Date::now();
    if now.is_finite() && now >= 0.0 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let ms = now as u64;
        ms
    } else {
        0
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn fresh_nonce() -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    lakko_crypto::random::fill(&mut nonce);
    nonce
}

fn err(msg: &str) -> JsError {
    JsError::new(msg)
}

fn key32(bytes: &[u8], what: &str) -> Result<[u8; KEY_LEN], JsError> {
    bytes
        .try_into()
        .map_err(|_| err(&format!("{what} must be 32 bytes")))
}

fn journal32(bytes: &[u8]) -> Result<[u8; JOURNAL_ID_LEN], JsError> {
    key32(bytes, "journal id")
}

fn flat_keys(flat: &[u8], what: &str) -> Result<Vec<[u8; KEY_LEN]>, JsError> {
    if flat.len() % KEY_LEN != 0 {
        return Err(err(&format!("{what} must be concatenated 32-byte keys")));
    }
    flat.chunks_exact(KEY_LEN).map(|c| key32(c, what)).collect()
}

fn rank_of(byte: u8) -> Result<Rank, JsError> {
    Rank::from_byte(byte).ok_or_else(|| err("rank must be 1 (member) or 2 (admin)"))
}

/// Flat keys + one rank byte each (`1` member, `2` admin) as registrar
/// entries.
fn flat_entries(keys: &[u8], ranks: &[u8]) -> Result<Vec<RegistrarEntry>, JsError> {
    let keys = flat_keys(keys, "registrar keys")?;
    if keys.len() != ranks.len() {
        return Err(err("one rank per registrar key"));
    }
    keys.into_iter()
        .zip(ranks)
        .map(|(key, r)| {
            Ok(RegistrarEntry {
                key,
                rank: rank_of(*r)?,
            })
        })
        .collect()
}

/// Lowercase hex, as the DS path takes it.
fn lower_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    out
}

/// One `PUT` to the Delivery Service: `path` is the service path
/// (`/ds/group/<hex>/<segment>`), `bodyJson` the exact JSON body. No proof
/// header.
#[wasm_bindgen]
pub struct DsStatementRequest {
    path: String,
    body_json: String,
}

#[wasm_bindgen]
impl DsStatementRequest {
    /// The service path.
    #[wasm_bindgen(getter)]
    pub fn path(&self) -> String {
        self.path.clone()
    }

    /// The JSON body.
    #[wasm_bindgen(getter, js_name = bodyJson)]
    pub fn body_json(&self) -> String {
        self.body_json.clone()
    }
}

/// The journal's root seed from the owner's ACCOUNT identity seed (32 bytes,
/// `Identity.ed25519Seed` of the account) and the 32-byte journal id.
///
/// # Errors
///
/// When an input is not 32 bytes.
#[wasm_bindgen(js_name = dsJournalRootSeed)]
pub fn ds_journal_root_seed(
    account_identity_seed: &[u8],
    journal_id: &[u8],
) -> Result<Vec<u8>, JsError> {
    let seed = key32(account_identity_seed, "account identity seed")?;
    Ok(journal_root_seed(&seed, &journal32(journal_id)?).to_vec())
}

/// The registrar seed the root appoints for a member account key.
///
/// # Errors
///
/// When an input is not 32 bytes.
#[wasm_bindgen(js_name = dsJournalRegistrarSeed)]
pub fn ds_journal_registrar_seed(
    root_seed: &[u8],
    member_account_key: &[u8],
) -> Result<Vec<u8>, JsError> {
    let root = key32(root_seed, "root seed")?;
    let member = key32(member_account_key, "member account key")?;
    Ok(journal_registrar_seed(&root, &member).to_vec())
}

/// A root or registrar signer.
#[wasm_bindgen]
pub struct DsStatementSigner {
    inner: StatementSigner,
}

/// The journal's root signer for the owner's ACCOUNT identity.
///
/// # Errors
///
/// When `journal_id` is not 32 bytes.
#[wasm_bindgen(js_name = dsJournalRootSigner)]
pub fn ds_journal_root_signer(
    account_identity: &Identity,
    journal_id: &[u8],
) -> Result<DsStatementSigner, JsError> {
    Ok(DsStatementSigner {
        inner: StatementSigner::root(
            &account_identity.mls_identity().ed25519_seed_bytes(),
            &journal32(journal_id)?,
        ),
    })
}

/// The signer of a root or registrar seed.
///
/// # Errors
///
/// When `seed` is not 32 bytes.
#[wasm_bindgen(js_name = dsStatementSigner)]
pub fn ds_statement_signer(seed: &[u8]) -> Result<DsStatementSigner, JsError> {
    Ok(DsStatementSigner {
        inner: StatementSigner::from_seed(&key32(seed, "seed")?),
    })
}

impl DsStatementSigner {
    fn register(
        &self,
        journal_id: &[u8],
        epoch: u64,
        epoch_key: &[u8],
        mode: RegisterMode,
        revoke_keys: &[u8],
    ) -> Result<DsStatementRequest, JsError> {
        let epoch_key = key32(epoch_key, "epoch key")?;
        let revoke = flat_keys(revoke_keys, "revoked keys")?;
        self.request(
            journal_id,
            StatementBody::RegisterEpoch {
                epoch,
                epoch_key,
                mode,
                revoke,
            },
        )
    }

    fn request(
        &self,
        journal_id: &[u8],
        body: StatementBody,
    ) -> Result<DsStatementRequest, JsError> {
        let journal = journal32(journal_id)?;
        let statement = self
            .inner
            .sign_at(&journal, body, now_ms(), fresh_nonce())
            .map_err(|e| err(&e.to_string()))?;
        Ok(DsStatementRequest {
            path: format!(
                "/ds/group/{}/{}",
                lower_hex(&journal),
                statement.path_segment()
            ),
            body_json: statement.body_json(),
        })
    }
}

#[wasm_bindgen]
impl DsStatementSigner {
    /// The 32-byte public key the DS knows this signer by.
    #[wasm_bindgen(js_name = publicKey)]
    pub fn public_key(&self) -> Vec<u8> {
        self.inner.public_key().to_vec()
    }

    /// `PUT .../claim`: the first claim with the first epoch's access key.
    ///
    /// # Errors
    ///
    /// On a wrong-length input or an invalid key.
    pub fn claim(
        &self,
        journal_id: &[u8],
        epoch: u64,
        epoch_key: &[u8],
    ) -> Result<DsStatementRequest, JsError> {
        let epoch_key = key32(epoch_key, "epoch key")?;
        self.request(journal_id, StatementBody::Claim { epoch, epoch_key })
    }

    /// `PUT .../reset` keeping the registrar set.
    ///
    /// # Errors
    ///
    /// On a wrong-length input or an invalid key.
    pub fn reset(
        &self,
        journal_id: &[u8],
        epoch: u64,
        epoch_key: &[u8],
    ) -> Result<DsStatementRequest, JsError> {
        let epoch_key = key32(epoch_key, "epoch key")?;
        self.request(
            journal_id,
            StatementBody::Reset {
                epoch,
                epoch_key,
                registrars: None,
            },
        )
    }

    /// `PUT .../reset` replacing the registrar set with `keys` (flat 32-byte
    /// keys) at `ranks` (one byte each, `1` member / `2` admin).
    ///
    /// # Errors
    ///
    /// On a wrong-length input, an invalid key, a repeated key, a count
    /// mismatch or more than 256 registrars.
    #[wasm_bindgen(js_name = resetWithRegistrars)]
    pub fn reset_with_registrars(
        &self,
        journal_id: &[u8],
        epoch: u64,
        epoch_key: &[u8],
        keys: &[u8],
        ranks: &[u8],
    ) -> Result<DsStatementRequest, JsError> {
        let epoch_key = key32(epoch_key, "epoch key")?;
        let registrars = flat_entries(keys, ranks)?;
        self.request(
            journal_id,
            StatementBody::Reset {
                epoch,
                epoch_key,
                registrars: Some(registrars),
            },
        )
    }

    /// `PUT .../access` in **replace** mode: write `epochKey` at `epoch` and
    /// drop every epoch above it, when everything above was registered by a
    /// strictly lower rank than this signer's and the registration at
    /// `epoch`, if any, was too or carries this same key (`403` otherwise; a
    /// member never replaces; a skip past the next epoch is the root's).
    /// Revokes the flat `revokeKeys` in the same transaction.
    ///
    /// # Errors
    ///
    /// On a wrong-length input, an invalid key, a repeated key or more than
    /// 256 revocations.
    #[wasm_bindgen(js_name = replaceEpoch)]
    pub fn replace_epoch(
        &self,
        journal_id: &[u8],
        epoch: u64,
        epoch_key: &[u8],
        revoke_keys: &[u8],
    ) -> Result<DsStatementRequest, JsError> {
        self.register(
            journal_id,
            epoch,
            epoch_key,
            RegisterMode::Replace,
            revoke_keys,
        )
    }

    /// ADR-0081 - `POST .../commit`: register `epochKey` at `epoch` in
    /// **append** mode (it must be the journal's next epoch; a member
    /// registrar may append only one epoch past the last admin or root
    /// registration; the flat 32-byte `revokeKeys` are revoked in the same
    /// step) and append `row` - the commit, in the journal's
    /// envelope - in ONE step. The returned request's `path` ends in
    /// `/commit` and its `bodyJson` is the exact body; the caller sends it as
    /// a `POST` with the request proof signed over those bytes by the key of
    /// epoch `epoch - 1` (`DsProofSigner.sign("POST", path, "", body)`).
    /// Re-sending the same row under the same key answers the original
    /// message id (`200`); any other commit at a registered epoch is `409`.
    /// Sign a new request for every attempt: the statement's nonce is spent.
    ///
    /// # Errors
    ///
    /// On a wrong-length input, an invalid key, a repeated key, more than
    /// 256 revocations, or an empty row.
    pub fn commit(
        &self,
        journal_id: &[u8],
        epoch: u64,
        epoch_key: &[u8],
        revoke_keys: &[u8],
        row: &[u8],
    ) -> Result<DsStatementRequest, JsError> {
        let journal = journal32(journal_id)?;
        let epoch_key = key32(epoch_key, "epoch key")?;
        let revoke = flat_keys(revoke_keys, "revoked keys")?;
        let statement = self
            .inner
            .sign_commit_at(&journal, epoch, epoch_key, revoke, now_ms(), fresh_nonce())
            .map_err(|e| err(&e.to_string()))?;
        let body_json =
            lakko_ds_proof::commit_body_json(&statement, row).map_err(|e| err(&e.to_string()))?;
        Ok(DsStatementRequest {
            path: format!(
                "/ds/group/{}/{}",
                lower_hex(&journal),
                lakko_ds_proof::COMMIT_PATH_SEGMENT
            ),
            body_json,
        })
    }

    /// `PUT .../registrars-set`: the root replaces the journal's WHOLE
    /// registrar set with `keys` (flat 32-byte keys) at `ranks` (one byte
    /// each, `1` member / `2` admin); every registrar not listed is removed,
    /// and empty lists remove all. A registrar sending it gets `403`.
    ///
    /// # Errors
    ///
    /// On a wrong-length input, an invalid key, a repeated key, a count
    /// mismatch, an unknown rank, more than 4096 entries, or an entry naming
    /// this signer's own key.
    #[wasm_bindgen(js_name = registrarsSet)]
    pub fn registrars_set(
        &self,
        journal_id: &[u8],
        keys: &[u8],
        ranks: &[u8],
    ) -> Result<DsStatementRequest, JsError> {
        let registrars = flat_entries(keys, ranks)?;
        self.request(journal_id, StatementBody::RegistrarsSet { registrars })
    }

    /// `PUT .../registrars`: `keys` flat 32-byte keys, `ranks` one byte each
    /// (`1` member, `2` admin, `0` removes); 1 to 256 changes.
    ///
    /// # Errors
    ///
    /// On a wrong-length input, an invalid key, a repeated key, a count
    /// mismatch, an unknown rank, or an empty or over-long list.
    pub fn registrars(
        &self,
        journal_id: &[u8],
        keys: &[u8],
        ranks: &[u8],
    ) -> Result<DsStatementRequest, JsError> {
        let keys = flat_keys(keys, "registrar keys")?;
        if keys.len() != ranks.len() {
            return Err(err("one rank per registrar key"));
        }
        let changes = keys
            .into_iter()
            .zip(ranks)
            .map(|(key, r)| {
                let rank = if *r == 0 { None } else { Some(rank_of(*r)?) };
                Ok(RegistrarChange { key, rank })
            })
            .collect::<Result<Vec<_>, JsError>>()?;
        self.request(journal_id, StatementBody::Registrars { changes })
    }

    /// `PUT .../root`: the current root names `newRoot`. Naming the current
    /// root itself is a no-op `204` for the root and the uniform `401` for
    /// any other key: the probe "am I still the root?".
    ///
    /// # Errors
    ///
    /// On a wrong-length input or an invalid key.
    #[wasm_bindgen(js_name = rootTransfer)]
    pub fn root_transfer(
        &self,
        journal_id: &[u8],
        new_root: &[u8],
    ) -> Result<DsStatementRequest, JsError> {
        let new_root = key32(new_root, "new root key")?;
        self.request(journal_id, StatementBody::RootTransfer { new_root })
    }
}

/// A decoded registrar-seed envelope; the seed is still sealed.
#[wasm_bindgen]
pub struct RegistrarSeedEnvelope {
    journal_id: Vec<u8>,
    member_account_key: Vec<u8>,
    rank: u8,
    registrar_public_key: Vec<u8>,
}

#[wasm_bindgen]
impl RegistrarSeedEnvelope {
    /// The journal (group id) the registrar key is for.
    #[wasm_bindgen(getter, js_name = journalId)]
    pub fn journal_id(&self) -> Vec<u8> {
        self.journal_id.clone()
    }

    /// The addressee's account Ed25519 public key.
    #[wasm_bindgen(getter, js_name = memberAccountKey)]
    pub fn member_account_key(&self) -> Vec<u8> {
        self.member_account_key.clone()
    }

    /// `1` member, `2` admin, `3` root handover (ownership transfer).
    #[wasm_bindgen(getter)]
    pub fn rank(&self) -> u8 {
        self.rank
    }

    /// The registrar's Ed25519 public key (32 bytes), readable without
    /// opening: the key the Delivery Service knows the registrar by (for a
    /// root handover, the root's). Every member records
    /// `memberAccountKey -> registrarPublicKey` from an envelope whose MLS
    /// sender resolved to the owner, and revokes it when it commits that
    /// account's removal. Bound to the seal; `openRegistrarSeedEnvelope`
    /// refuses a seed that does not derive it.
    #[wasm_bindgen(getter, js_name = registrarPublicKey)]
    pub fn registrar_public_key(&self) -> Vec<u8> {
        self.registrar_public_key.clone()
    }
}

const fn seed_rank_byte(rank: RegistrarSeedRank) -> u8 {
    match rank {
        RegistrarSeedRank::Member => 1,
        RegistrarSeedRank::Admin => 2,
        RegistrarSeedRank::Root => 3,
    }
}

/// Compose the registrar-seed envelope a root sends one member inside the
/// group: `registrarSeed` sealed to `memberX25519PublicKey` (the member
/// account's identity-bundle `x25519_pubkey`), addressed by its account key.
/// `rank`: `1` member, `2` admin. The envelope's clear registrar public key
/// is derived from `registrarSeed` inside, so it always matches.
///
/// # Errors
///
/// On a wrong-length input, an invalid key or an unknown rank.
#[wasm_bindgen(js_name = composeRegistrarSeedEnvelope)]
pub fn compose_registrar_seed_envelope(
    journal_id: &[u8],
    member_account_key: &[u8],
    member_x25519_public_key: &[u8],
    rank: u8,
    registrar_seed: &[u8],
) -> Result<Vec<u8>, JsError> {
    let x25519 = X25519PublicKey::from_bytes(member_x25519_public_key)
        .map_err(|_| err("member X25519 key must be 32 bytes"))?;
    let rank = match rank_of(rank)? {
        Rank::Member => RegistrarSeedRank::Member,
        Rank::Admin => RegistrarSeedRank::Admin,
    };
    compose_registrar_seed(
        &journal32(journal_id)?,
        &key32(member_account_key, "member account key")?,
        &x25519,
        rank,
        &key32(registrar_seed, "registrar seed")?,
    )
    .map_err(|e| err(&e.to_string()))
}

/// Compose the root-handover envelope (ADR-0057, ownership transfer): the
/// outgoing owner's journal root seed (`dsJournalRootSeed`) sealed to the new
/// owner's account X25519 key, addressed by its account key. Decodes with
/// rank `3`.
///
/// # Errors
///
/// On a wrong-length input or an invalid key.
#[wasm_bindgen(js_name = composeRootHandoverEnvelope)]
pub fn compose_root_handover_envelope(
    journal_id: &[u8],
    new_owner_account_key: &[u8],
    new_owner_x25519_public_key: &[u8],
    root_seed: &[u8],
) -> Result<Vec<u8>, JsError> {
    let x25519 = X25519PublicKey::from_bytes(new_owner_x25519_public_key)
        .map_err(|_| err("new owner X25519 key must be 32 bytes"))?;
    compose_registrar_seed(
        &journal32(journal_id)?,
        &key32(new_owner_account_key, "new owner account key")?,
        &x25519,
        RegistrarSeedRank::Root,
        &key32(root_seed, "root seed")?,
    )
    .map_err(|e| err(&e.to_string()))
}

/// Strictly decode a registrar-seed envelope (the seed stays sealed).
///
/// # Errors
///
/// When `plaintext` is not a well-formed envelope.
#[wasm_bindgen(js_name = decodeRegistrarSeedEnvelope)]
pub fn decode_registrar_seed_envelope(plaintext: &[u8]) -> Result<RegistrarSeedEnvelope, JsError> {
    let env = decode_registrar_seed(plaintext).map_err(|e| err(&e.to_string()))?;
    Ok(RegistrarSeedEnvelope {
        journal_id: env.journal_id.to_vec(),
        member_account_key: env.member_account_key.to_vec(),
        rank: seed_rank_byte(env.rank),
        registrar_public_key: env.registrar_public_key.to_vec(),
    })
}

/// Open a registrar-seed envelope with the ACCOUNT identity it is addressed
/// to; the 32-byte registrar seed (feed it to `dsStatementSigner`).
///
/// # Errors
///
/// When the envelope is malformed, addressed to another account key, does
/// not open under this identity's X25519 key, or opens to a seed that does
/// not derive the envelope's registrar public key.
#[wasm_bindgen(js_name = openRegistrarSeedEnvelope)]
pub fn open_registrar_seed_envelope(
    plaintext: &[u8],
    account_identity: &Identity,
) -> Result<Vec<u8>, JsError> {
    let env = decode_registrar_seed(plaintext).map_err(|e| err(&e.to_string()))?;
    let identity = account_identity.mls_identity();
    if env.member_account_key.as_slice() != identity.signature_public_key() {
        return Err(err("registrar seed is addressed to another account"));
    }
    let secret = X25519SecretKey::from_bytes(identity.x25519_secret_bytes());
    env.open(&secret)
        .map(|seed| seed.to_vec())
        .map_err(|e| err(&e.to_string()))
}

/// The one well-formed registrar-seed envelope length in bytes.
#[wasm_bindgen(js_name = registrarSeedEnvelopeLen)]
#[must_use]
pub fn registrar_seed_envelope_len() -> u32 {
    u32::try_from(REGISTRAR_SEED_ENVELOPE_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_and_registrar_signers_agree_with_the_derivations() {
        let seed = [0x11u8; 32];
        let journal = [0x22u8; 32];
        let root_seed = ds_journal_root_seed(&seed, &journal).unwrap();
        let root = ds_statement_signer(&root_seed).unwrap();
        let expected = StatementSigner::root(&seed, &journal).public_key();
        assert_eq!(root.public_key(), expected.to_vec());
        let epoch_key = lakko_ds_proof::SigningKey::from_seed(&[3; 32])
            .verifying_key()
            .to_bytes();
        let req = root.claim(&journal, 5, &epoch_key).unwrap();
        assert_eq!(req.path(), format!("/ds/group/{}/claim", "22".repeat(32)));
        assert!(req.body_json().starts_with("{\"statement_b64\":\""));
        let registrar =
            ds_statement_signer(&ds_journal_registrar_seed(&root_seed, &[0x33; 32]).unwrap())
                .unwrap();
        let change = root
            .registrars(&journal, &registrar.public_key(), &[2])
            .unwrap();
        assert!(change.path().ends_with("/registrars"));
    }

    fn statement_of(req: &DsStatementRequest) -> lakko_ds_proof::SignedStatement {
        use base64::Engine as _;
        let body = req.body_json();
        let b64 = body
            .strip_prefix("{\"statement_b64\":\"")
            .and_then(|rest| rest.strip_suffix("\"}"))
            .unwrap();
        let blob = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap();
        lakko_ds_proof::SignedStatement::decode(&blob).unwrap()
    }

    #[test]
    fn a_commit_request_carries_an_append_registration_and_the_row() {
        use base64::Engine as _;
        let signer = ds_statement_signer(&[0x24; 32]).unwrap();
        let journal = [0x47u8; 32];
        let epoch_key = lakko_ds_proof::SigningKey::from_seed(&[9; 32])
            .verifying_key()
            .to_bytes();
        let req = signer
            .commit(&journal, 7, &epoch_key, &[0x0B; 32], b"journal row")
            .unwrap();
        assert_eq!(req.path(), format!("/ds/group/{}/commit", "47".repeat(32)));
        let body = req.body_json();
        let (statement_b64, row_b64) = body
            .strip_prefix("{\"statement_b64\":\"")
            .and_then(|rest| rest.strip_suffix("\"}"))
            .and_then(|rest| rest.split_once("\",\"row_b64\":\""))
            .unwrap();
        let b64 = base64::engine::general_purpose::STANDARD;
        let statement =
            lakko_ds_proof::SignedStatement::decode(&b64.decode(statement_b64).unwrap()).unwrap();
        statement.verify(now_ms()).unwrap();
        assert_eq!(
            statement.body,
            StatementBody::RegisterEpoch {
                epoch: 7,
                epoch_key,
                mode: RegisterMode::Append,
                revoke: vec![[0x0B; 32]],
            }
        );
        assert_eq!(b64.decode(row_b64).unwrap(), b"journal row");
        // The empty-row refusal is `lakko_ds_proof::commit_body_json`'s, tested
        // there: a `JsError` cannot be built off wasm32.
    }

    #[test]
    fn register_modes_and_the_registrar_set_sign_what_the_ds_routes_take() {
        let signer = ds_statement_signer(&[0x21; 32]).unwrap();
        let journal = [0x46u8; 32];
        let epoch_key = lakko_ds_proof::SigningKey::from_seed(&[8; 32])
            .verifying_key()
            .to_bytes();
        let mut revoke = [0x09u8; 64];
        revoke[32..].fill(0x0A);
        // ADR-0081 (open question 1, decided 2026-10-18): an Append is sent
        // only through `commit`; `/access` takes the Replace.
        {
            let req = signer
                .replace_epoch(&journal, 6, &epoch_key, &revoke)
                .unwrap();
            assert_eq!(req.path(), format!("/ds/group/{}/access", "46".repeat(32)));
            let statement = statement_of(&req);
            statement.verify(now_ms()).unwrap();
            assert_eq!(
                statement.body,
                StatementBody::RegisterEpoch {
                    epoch: 6,
                    epoch_key,
                    mode: RegisterMode::Replace,
                    revoke: vec![[0x09; 32], [0x0A; 32]],
                }
            );
        }

        let member = ds_statement_signer(&[0x22; 32]).unwrap().public_key();
        let admin = ds_statement_signer(&[0x23; 32]).unwrap().public_key();
        let keys = [member.clone(), admin.clone()].concat();
        let req = signer.registrars_set(&journal, &keys, &[1, 2]).unwrap();
        assert!(req.path().ends_with("/registrars-set"));
        let statement = statement_of(&req);
        statement.verify(now_ms()).unwrap();
        let StatementBody::RegistrarsSet { registrars } = statement.body else {
            panic!("not a registrars set");
        };
        assert_eq!(
            registrars,
            vec![
                RegistrarEntry {
                    key: member.clone().try_into().unwrap(),
                    rank: Rank::Member,
                },
                RegistrarEntry {
                    key: admin.try_into().unwrap(),
                    rank: Rank::Admin,
                },
            ]
        );
        // Empty removes all.
        let empty = signer.registrars_set(&journal, &[], &[]).unwrap();
        assert_eq!(
            statement_of(&empty).body,
            StatementBody::RegistrarsSet { registrars: vec![] }
        );
    }

    #[test]
    fn a_decoded_registrar_seed_envelope_exposes_the_registrar_public_key() {
        let member_x25519 = X25519SecretKey::from_bytes([0x31; 32]).public_key();
        let seed = [0x32u8; 32];
        let wire = compose_registrar_seed_envelope(
            &[0x47; 32],
            &[0x48; 32],
            &member_x25519.to_bytes(),
            2,
            &seed,
        )
        .unwrap();
        assert_eq!(
            u32::try_from(wire.len()).unwrap(),
            registrar_seed_envelope_len()
        );
        assert_eq!(registrar_seed_envelope_len(), 205);
        let decoded = decode_registrar_seed_envelope(&wire).unwrap();
        assert_eq!(decoded.rank(), 2);
        assert_eq!(
            decoded.registrar_public_key(),
            ds_statement_signer(&seed).unwrap().public_key()
        );
    }
}
