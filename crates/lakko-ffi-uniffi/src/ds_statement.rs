// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI exposure of the [ADR-0057] journal-ownership surface: the root and
//! registrar derivations, a signer for every ownership statement the
//! Delivery Service takes, and the registrar-seed envelope a root sends a
//! member.
//!
//! A client never spells a statement, a route or a JSON field: each signer
//! method returns a [`DsStatementRequest`] - the service path and the JSON
//! body of one `PUT`. Clock and nonce are read here.
//!
//! - **Root:** [`ds_journal_root_signer`] from the owner's ACCOUNT identity
//!   (the ADR-0017 account key, the same on every device of the account) and
//!   the journal's 32-byte group id.
//! - **Registrar:** the root derives [`ds_journal_registrar_seed`] per member
//!   account key, registers the public key with `registrars`, and sends the
//!   seed with [`compose_registrar_seed_envelope`]; the member opens it with
//!   [`open_registrar_seed_envelope`] and signs with [`ds_statement_signer`].
//!
//! [ADR-0057]: ../../../docs/adr/0057-journal-ownership.md

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use lakko_contacts::registrar_seed::{
    compose_registrar_seed, decode_registrar_seed, RegistrarSeedRank, REGISTRAR_SEED_ENVELOPE_LEN,
};
use lakko_crypto::agreement::{PublicKey as X25519PublicKey, SecretKey as X25519SecretKey};
use lakko_ds_proof::statement::{
    journal_registrar_seed, journal_root_seed, Rank, RegisterMode, RegistrarChange, RegistrarEntry,
    SignedStatement, StatementBody, StatementSigner, JOURNAL_ID_LEN, KEY_LEN,
};
use lakko_ds_proof::NONCE_LEN;

use crate::{FfiError, Identity};

/// A registrar's rank (the root is not a registrar and outranks both).
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsRegistrarRank {
    /// A member that may commit self-service changes.
    Member,
    /// One of the owner's admins.
    Admin,
}

impl From<DsRegistrarRank> for Rank {
    fn from(r: DsRegistrarRank) -> Self {
        match r {
            DsRegistrarRank::Member => Self::Member,
            DsRegistrarRank::Admin => Self::Admin,
        }
    }
}

impl From<DsRegistrarRank> for RegistrarSeedRank {
    fn from(r: DsRegistrarRank) -> Self {
        match r {
            DsRegistrarRank::Member => Self::Member,
            DsRegistrarRank::Admin => Self::Admin,
        }
    }
}

/// The rank a registrar-seed envelope carries: a registrar rank, or a root
/// handover (ADR-0057, ownership transfer).
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsSeedEnvelopeRank {
    /// A member registrar seed.
    Member,
    /// An admin registrar seed.
    Admin,
    /// The outgoing owner's journal root seed, sealed to the new owner.
    Root,
}

impl From<RegistrarSeedRank> for DsSeedEnvelopeRank {
    fn from(r: RegistrarSeedRank) -> Self {
        match r {
            RegistrarSeedRank::Member => Self::Member,
            RegistrarSeedRank::Admin => Self::Admin,
            RegistrarSeedRank::Root => Self::Root,
        }
    }
}

/// One registrar of a registrar set (a reset's, or `registrarsSet`'s).
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct DsRegistrarEntry {
    /// The registrar's 32-byte Ed25519 public key.
    pub public_key: Vec<u8>,
    /// Its rank.
    pub rank: DsRegistrarRank,
}

/// One change of a registrar-management statement.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct DsRegistrarChange {
    /// The registrar's 32-byte Ed25519 public key.
    pub public_key: Vec<u8>,
    /// The rank to set, or `None` to remove the registrar.
    pub rank: Option<DsRegistrarRank>,
}

/// One `PUT` to the Delivery Service: `path` is the service path
/// (`/ds/group/<hex>/<segment>`, no reverse-proxy prefix), `body_json` the
/// exact request body (`Content-Type: application/json`). No proof header.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct DsStatementRequest {
    /// The service path.
    pub path: String,
    /// The JSON body.
    pub body_json: String,
}

/// A decoded registrar-seed envelope; the seed is still sealed.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiRegistrarSeedEnvelope {
    /// The journal (group id) the registrar key is for.
    pub journal_id: Vec<u8>,
    /// The addressee's account Ed25519 public key.
    pub member_account_key: Vec<u8>,
    /// The rank the root registered the key at, or a root handover.
    pub rank: DsSeedEnvelopeRank,
    /// The registrar's Ed25519 public key (32 bytes), readable without
    /// opening: the key the Delivery Service knows the registrar by (for a
    /// root handover, the root's). Every member records
    /// `member_account_key -> registrar_public_key` from an envelope whose
    /// MLS sender resolved to the owner, and revokes it when it commits that
    /// account's removal. Bound to the seal; [`open_registrar_seed_envelope`]
    /// refuses a seed that does not derive it.
    pub registrar_public_key: Vec<u8>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn fresh_nonce() -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    lakko_crypto::random::fill(&mut nonce);
    nonce
}

fn key32(bytes: &[u8], what: &str) -> Result<[u8; KEY_LEN], FfiError> {
    bytes
        .try_into()
        .map_err(|_| FfiError::Codec(format!("{what} must be 32 bytes")))
}

fn journal32(bytes: &[u8]) -> Result<[u8; JOURNAL_ID_LEN], FfiError> {
    key32(bytes, "journal id")
}

/// `BLAKE3-derive-key("lakko/ds/journal-root/v1", account_identity_seed ||
/// journal_id)`: the journal's root seed. `account_identity_seed` is the
/// 32-byte Ed25519 seed of the ACCOUNT identity (`Identity.ed25519Seed()` of
/// the account, not of a device).
///
/// # Errors
///
/// [`FfiError::Codec`] when an input is not 32 bytes.
#[uniffi::export]
pub fn ds_journal_root_seed(
    account_identity_seed: Vec<u8>,
    journal_id: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let seed = key32(&account_identity_seed, "account identity seed")?;
    Ok(journal_root_seed(&seed, &journal32(&journal_id)?).to_vec())
}

/// `BLAKE3-derive-key("lakko/ds/journal-registrar/v1", root_seed ||
/// member_account_key)`: the registrar seed the root appoints for a member.
///
/// # Errors
///
/// [`FfiError::Codec`] when an input is not 32 bytes.
#[uniffi::export]
pub fn ds_journal_registrar_seed(
    root_seed: Vec<u8>,
    member_account_key: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let root = key32(&root_seed, "root seed")?;
    let member = key32(&member_account_key, "member account key")?;
    Ok(journal_registrar_seed(&root, &member).to_vec())
}

/// A root or registrar signer. See the module doc.
#[derive(uniffi::Object, Debug)]
pub struct DsStatementSigner {
    inner: StatementSigner,
}

/// The journal's root signer for the owner's ACCOUNT identity.
///
/// # Errors
///
/// [`FfiError::Codec`] when `journal_id` is not 32 bytes.
#[uniffi::export]
pub fn ds_journal_root_signer(
    account_identity: &Identity,
    journal_id: Vec<u8>,
) -> Result<Arc<DsStatementSigner>, FfiError> {
    Ok(Arc::new(DsStatementSigner {
        inner: StatementSigner::root(
            &account_identity.inner.ed25519_seed_bytes(),
            &journal32(&journal_id)?,
        ),
    }))
}

/// The signer of a root or registrar seed (a registrar opens its seed with
/// [`open_registrar_seed_envelope`]).
///
/// # Errors
///
/// [`FfiError::Codec`] when `seed` is not 32 bytes.
#[uniffi::export]
pub fn ds_statement_signer(seed: Vec<u8>) -> Result<Arc<DsStatementSigner>, FfiError> {
    Ok(Arc::new(DsStatementSigner {
        inner: StatementSigner::from_seed(&key32(&seed, "seed")?),
    }))
}

impl DsStatementSigner {
    fn request(
        &self,
        journal_id: &[u8],
        body: StatementBody,
    ) -> Result<DsStatementRequest, FfiError> {
        let journal = journal32(journal_id)?;
        let statement: SignedStatement = self
            .inner
            .sign_at(&journal, body, now_ms(), fresh_nonce())
            .map_err(|e| FfiError::Codec(e.to_string()))?;
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

/// Lowercase hex, as the DS path takes it (no `hex` dependency for one call).
fn lower_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    out
}

fn keys32(list: &[Vec<u8>], what: &str) -> Result<Vec<[u8; KEY_LEN]>, FfiError> {
    list.iter().map(|k| key32(k, what)).collect()
}

fn entries32(set: &[DsRegistrarEntry]) -> Result<Vec<RegistrarEntry>, FfiError> {
    set.iter()
        .map(|e| {
            Ok(RegistrarEntry {
                key: key32(&e.public_key, "registrar key")?,
                rank: e.rank.into(),
            })
        })
        .collect()
}

impl DsStatementSigner {
    fn register(
        &self,
        journal_id: &[u8],
        epoch: u64,
        epoch_key: &[u8],
        mode: RegisterMode,
        revoke: &[Vec<u8>],
    ) -> Result<DsStatementRequest, FfiError> {
        let epoch_key = key32(epoch_key, "epoch key")?;
        let revoke = keys32(revoke, "revoked key")?;
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
}

#[uniffi::export]
impl DsStatementSigner {
    /// The 32-byte public key the DS knows this signer by (what `registrars`
    /// and `root transfer` name).
    pub fn public_key(&self) -> Vec<u8> {
        self.inner.public_key().to_vec()
    }

    /// `PUT .../claim`: the first claim (this signer becomes the root) with
    /// the first epoch's access key. Re-sending the registered claim is
    /// idempotent (204); a different one on a claimed journal is 409.
    ///
    /// # Errors
    ///
    /// [`FfiError::Codec`] on a wrong-length input or an invalid key.
    pub fn claim(
        &self,
        journal_id: Vec<u8>,
        epoch: u64,
        epoch_key: Vec<u8>,
    ) -> Result<DsStatementRequest, FfiError> {
        let epoch_key = key32(&epoch_key, "epoch key")?;
        self.request(&journal_id, StatementBody::Claim { epoch, epoch_key })
    }

    /// `PUT .../reset`: the root replaces the epoch chain with one epoch,
    /// and the registrar set with `registrars` when it is `Some`.
    ///
    /// # Errors
    ///
    /// [`FfiError::Codec`] on a wrong-length input, an invalid key, a
    /// repeated key or more than 256 registrars.
    pub fn reset(
        &self,
        journal_id: Vec<u8>,
        epoch: u64,
        epoch_key: Vec<u8>,
        registrars: Option<Vec<DsRegistrarEntry>>,
    ) -> Result<DsStatementRequest, FfiError> {
        let epoch_key = key32(&epoch_key, "epoch key")?;
        let registrars = registrars.map(|set| entries32(&set)).transpose()?;
        self.request(
            &journal_id,
            StatementBody::Reset {
                epoch,
                epoch_key,
                registrars,
            },
        )
    }

    /// `PUT .../access` in **replace** mode: write `epoch_key` at `epoch`
    /// and drop every epoch above it. Allowed when everything above `epoch`
    /// was registered by a strictly lower rank than this signer's and the
    /// registration at `epoch`, if any, was too or carries this same key
    /// (`403` otherwise; a member never replaces). A skip past the next
    /// epoch is the root's. Revokes `revoke` in the same transaction. An
    /// admin whose append met a member's fake next epoch sends
    /// `replaceEpoch(journal, its current epoch, its current key, ...)`.
    ///
    /// # Errors
    ///
    /// [`FfiError::Codec`] on a wrong-length input, an invalid key, a
    /// repeated key or more than 256 revocations.
    pub fn replace_epoch(
        &self,
        journal_id: Vec<u8>,
        epoch: u64,
        epoch_key: Vec<u8>,
        revoke: Vec<Vec<u8>>,
    ) -> Result<DsStatementRequest, FfiError> {
        self.register(
            &journal_id,
            epoch,
            &epoch_key,
            RegisterMode::Replace,
            &revoke,
        )
    }

    /// ADR-0081 - `POST .../commit`: register `epoch_key` at `epoch` in
    /// **append** mode (it must be the journal's next epoch; a member
    /// registrar may append only one epoch past the last admin or root
    /// registration; `revoke` is revoked in the same step) and append `row` - the commit, in the journal's
    /// envelope - in ONE step. The returned request's `path` ends in
    /// `/commit` and `body_json` is the exact body; the caller sends it as a
    /// `POST` with the request proof signed over those bytes by the key of
    /// epoch `epoch - 1` (`DsProofSigner.sign("POST", path, "", body)`).
    /// Re-sending the same row under the same key answers the original
    /// message id (`200`); any other commit at a registered epoch is `409`.
    /// Sign a new request for every attempt: the statement's nonce is spent.
    ///
    /// # Errors
    ///
    /// [`FfiError::Codec`] on a wrong-length input, an invalid key, a
    /// repeated key, more than 256 revocations, or an empty row.
    pub fn commit(
        &self,
        journal_id: Vec<u8>,
        epoch: u64,
        epoch_key: Vec<u8>,
        revoke: Vec<Vec<u8>>,
        row: Vec<u8>,
    ) -> Result<DsStatementRequest, FfiError> {
        let journal = journal32(&journal_id)?;
        let epoch_key = key32(&epoch_key, "epoch key")?;
        let revoke = keys32(&revoke, "revoked key")?;
        let statement = self
            .inner
            .sign_commit_at(&journal, epoch, epoch_key, revoke, now_ms(), fresh_nonce())
            .map_err(|e| FfiError::Codec(e.to_string()))?;
        let body_json = lakko_ds_proof::commit_body_json(&statement, &row)
            .map_err(|e| FfiError::Codec(e.to_string()))?;
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
    /// registrar set with `entries`; every registrar not listed is removed
    /// (an empty list removes all). Re-sending the same set is a `204`. A
    /// registrar sending it gets `403`.
    ///
    /// # Errors
    ///
    /// [`FfiError::Codec`] on a wrong-length input, an invalid key, a
    /// repeated key, more than 4096 entries, or an entry naming this
    /// signer's own key (the root is not a registrar).
    pub fn registrars_set(
        &self,
        journal_id: Vec<u8>,
        entries: Vec<DsRegistrarEntry>,
    ) -> Result<DsStatementRequest, FfiError> {
        let registrars = entries32(&entries)?;
        self.request(&journal_id, StatementBody::RegistrarsSet { registrars })
    }

    /// `PUT .../registrars`: the root adds, re-ranks (`rank` set) or removes
    /// (`rank` `None`) registrars; 1 to 256 changes.
    ///
    /// # Errors
    ///
    /// [`FfiError::Codec`] on a wrong-length input, an invalid key, a
    /// repeated key, or an empty or over-long list.
    pub fn registrars(
        &self,
        journal_id: Vec<u8>,
        changes: Vec<DsRegistrarChange>,
    ) -> Result<DsStatementRequest, FfiError> {
        let changes = changes
            .iter()
            .map(|c| {
                Ok(RegistrarChange {
                    key: key32(&c.public_key, "registrar key")?,
                    rank: c.rank.map(Into::into),
                })
            })
            .collect::<Result<Vec<_>, FfiError>>()?;
        self.request(&journal_id, StatementBody::Registrars { changes })
    }

    /// `PUT .../root`: the current root names `new_root` (ADR-0045 ownership
    /// transfer drives it). Naming the current root itself is a no-op `204`
    /// for the root and the uniform `401` for any other key: the probe "am I
    /// still the root?".
    ///
    /// # Errors
    ///
    /// [`FfiError::Codec`] on a wrong-length input or an invalid key.
    pub fn root_transfer(
        &self,
        journal_id: Vec<u8>,
        new_root: Vec<u8>,
    ) -> Result<DsStatementRequest, FfiError> {
        let new_root = key32(&new_root, "new root key")?;
        self.request(&journal_id, StatementBody::RootTransfer { new_root })
    }
}

/// Compose the registrar-seed envelope a root sends one member inside the
/// group (an MLS application message): `registrar_seed` sealed to the
/// member's account X25519 key (`member_x25519_public_key`, the
/// `x25519_pubkey` of its identity bundle), addressed by its account
/// Ed25519 key. The envelope's clear registrar public key is derived from
/// `registrar_seed` inside, so it always matches.
///
/// # Errors
///
/// [`FfiError::Codec`] on a wrong-length input or an invalid X25519 key,
/// [`FfiError::Contacts`] if sealing fails.
#[uniffi::export]
pub fn compose_registrar_seed_envelope(
    journal_id: Vec<u8>,
    member_account_key: Vec<u8>,
    member_x25519_public_key: Vec<u8>,
    rank: DsRegistrarRank,
    registrar_seed: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let x25519 = X25519PublicKey::from_bytes(&member_x25519_public_key)
        .map_err(|_| FfiError::Codec("member X25519 key must be 32 bytes".into()))?;
    compose_registrar_seed(
        &journal32(&journal_id)?,
        &key32(&member_account_key, "member account key")?,
        &x25519,
        rank.into(),
        &key32(&registrar_seed, "registrar seed")?,
    )
    .map_err(|e| FfiError::Contacts(e.to_string()))
}

/// Compose the root-handover envelope (ADR-0057, ownership transfer): the
/// outgoing owner's journal root seed (`dsJournalRootSeed`) sealed to the new
/// owner's account X25519 key, addressed by its account Ed25519 key. Sent
/// once the commit carrying the transfer has landed; the new owner signs the
/// `root` statement with it naming its own root.
///
/// # Errors
///
/// [`FfiError::Codec`] on a wrong-length input or an invalid X25519 key,
/// [`FfiError::Contacts`] if sealing fails.
#[uniffi::export]
pub fn compose_root_handover_envelope(
    journal_id: Vec<u8>,
    new_owner_account_key: Vec<u8>,
    new_owner_x25519_public_key: Vec<u8>,
    root_seed: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let x25519 = X25519PublicKey::from_bytes(&new_owner_x25519_public_key)
        .map_err(|_| FfiError::Codec("new owner X25519 key must be 32 bytes".into()))?;
    compose_registrar_seed(
        &journal32(&journal_id)?,
        &key32(&new_owner_account_key, "new owner account key")?,
        &x25519,
        RegistrarSeedRank::Root,
        &key32(&root_seed, "root seed")?,
    )
    .map_err(|e| FfiError::Contacts(e.to_string()))
}

/// Strictly decode a registrar-seed envelope (the seed stays sealed).
///
/// # Errors
///
/// [`FfiError::Contacts`] when `plaintext` is not a well-formed envelope.
#[uniffi::export]
pub fn decode_registrar_seed_envelope(
    plaintext: Vec<u8>,
) -> Result<FfiRegistrarSeedEnvelope, FfiError> {
    let env = decode_registrar_seed(&plaintext).map_err(|e| FfiError::Contacts(e.to_string()))?;
    Ok(FfiRegistrarSeedEnvelope {
        journal_id: env.journal_id.to_vec(),
        member_account_key: env.member_account_key.to_vec(),
        rank: env.rank.into(),
        registrar_public_key: env.registrar_public_key.to_vec(),
    })
}

/// Open a registrar-seed envelope with the ACCOUNT identity it is addressed
/// to, returning the 32-byte registrar seed (feed it to
/// [`ds_statement_signer`]). The caller also checks that the MLS sender of
/// the message resolved to the group's owner.
///
/// # Errors
///
/// [`FfiError::Contacts`] when the envelope is malformed, is addressed to
/// another account key, does not open under this identity's X25519 key, or
/// opens to a seed that does not derive the envelope's registrar public key.
#[uniffi::export]
pub fn open_registrar_seed_envelope(
    plaintext: Vec<u8>,
    account_identity: &Identity,
) -> Result<Vec<u8>, FfiError> {
    open_registrar_seed(&plaintext, account_identity, account_identity)
}

/// ADR-0074 section 3.7 (2026-11-20) — open a registrar-seed envelope
/// addressed to `account_identity` (the conversation identity: the key the
/// roster names this person by) but sealed to the X25519 key of
/// `sealed_to_identity` (the identity the directory serves after a rotation
/// of the account keys, whose bundle's `x25519_pubkey` the owner sealed to).
/// Both identities are this device's own account; the address check and the
/// seed-to-registrar-key check are those of [`open_registrar_seed_envelope`].
///
/// # Errors
///
/// As [`open_registrar_seed_envelope`], with the X25519 key of
/// `sealed_to_identity` in place of the addressee's.
#[uniffi::export]
pub fn open_registrar_seed_envelope_sealed_to(
    plaintext: Vec<u8>,
    account_identity: &Identity,
    sealed_to_identity: &Identity,
) -> Result<Vec<u8>, FfiError> {
    open_registrar_seed(&plaintext, account_identity, sealed_to_identity)
}

fn open_registrar_seed(
    plaintext: &[u8],
    addressee: &Identity,
    sealed_to: &Identity,
) -> Result<Vec<u8>, FfiError> {
    let env = decode_registrar_seed(plaintext).map_err(|e| FfiError::Contacts(e.to_string()))?;
    let own = addressee.inner.signature_public_key();
    if env.member_account_key.as_slice() != own {
        return Err(FfiError::Contacts(
            "registrar seed is addressed to another account".into(),
        ));
    }
    let secret = X25519SecretKey::from_bytes(sealed_to.inner.x25519_secret_bytes());
    env.open(&secret)
        .map(|seed| seed.to_vec())
        .map_err(|e| FfiError::Contacts(e.to_string()))
}

/// The one well-formed registrar-seed envelope length in bytes.
#[uniffi::export]
#[must_use]
pub fn registrar_seed_envelope_len() -> u32 {
    u32::try_from(REGISTRAR_SEED_ENVELOPE_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine as _;
    use lakko_ds_proof::statement::StatementKind;

    fn statement_of(req: &DsStatementRequest) -> SignedStatement {
        let v: serde_json::Value = serde_json::from_str(&req.body_json).unwrap();
        let blob = B64.decode(v["statement_b64"].as_str().unwrap()).unwrap();
        SignedStatement::decode(&blob).unwrap()
    }

    #[test]
    fn the_root_signer_is_the_derived_root_and_its_requests_verify() {
        let account = Identity::new("owner".into()).unwrap();
        let journal = vec![0x42; 32];
        let signer = ds_journal_root_signer(&account, journal.clone()).unwrap();
        let root_seed =
            ds_journal_root_seed(account.inner.ed25519_seed_bytes().to_vec(), journal.clone())
                .unwrap();
        assert_eq!(
            signer.public_key(),
            ds_statement_signer(root_seed).unwrap().public_key()
        );
        let epoch_key = lakko_ds_proof::SigningKey::from_seed(&[7; 32])
            .verifying_key()
            .to_bytes()
            .to_vec();
        let req = signer.claim(journal.clone(), 3, epoch_key).unwrap();
        assert_eq!(req.path, format!("/ds/group/{}/claim", "42".repeat(32)));
        let s = statement_of(&req);
        assert_eq!(s.body.kind(), StatementKind::Claim);
        assert_eq!(s.signer.to_vec(), signer.public_key());
        s.verify(now_ms()).unwrap();
        let transfer = signer
            .root_transfer(
                journal,
                lakko_ds_proof::SigningKey::from_seed(&[8; 32])
                    .verifying_key()
                    .to_bytes()
                    .to_vec(),
            )
            .unwrap();
        assert!(transfer.path.ends_with("/root"));
        assert!(signer
            .root_transfer(
                vec![1; 31],
                lakko_ds_proof::SigningKey::from_seed(&[8; 32])
                    .verifying_key()
                    .to_bytes()
                    .to_vec()
            )
            .is_err());
    }

    #[test]
    fn a_commit_request_carries_an_append_registration_and_the_row() {
        let signer = ds_statement_signer(vec![0x24; 32]).unwrap();
        let journal = vec![0x47; 32];
        let epoch_key = lakko_ds_proof::SigningKey::from_seed(&[9; 32])
            .verifying_key()
            .to_bytes()
            .to_vec();
        let req = signer
            .commit(
                journal.clone(),
                7,
                epoch_key.clone(),
                vec![vec![0x0B; 32]],
                b"journal row".to_vec(),
            )
            .unwrap();
        assert_eq!(req.path, format!("/ds/group/{}/commit", "47".repeat(32)));
        let s = statement_of(&req);
        s.verify(now_ms()).unwrap();
        assert_eq!(
            s.body,
            StatementBody::RegisterEpoch {
                epoch: 7,
                epoch_key: epoch_key.clone().try_into().unwrap(),
                mode: RegisterMode::Append,
                revoke: vec![[0x0B; 32]],
            }
        );
        let v: serde_json::Value = serde_json::from_str(&req.body_json).unwrap();
        assert_eq!(
            B64.decode(v["row_b64"].as_str().unwrap()).unwrap(),
            b"journal row"
        );
        assert!(signer
            .commit(journal, 7, epoch_key, vec![], Vec::new())
            .is_err());
    }

    #[test]
    fn register_modes_and_the_registrar_set_sign_what_the_ds_routes_take() {
        let signer = ds_statement_signer(vec![0x21; 32]).unwrap();
        let journal = vec![0x45; 32];
        let epoch_key = lakko_ds_proof::SigningKey::from_seed(&[8; 32])
            .verifying_key()
            .to_bytes()
            .to_vec();
        let revoked = vec![vec![0x09; 32]];
        // ADR-0081 (open question 1, decided 2026-10-18): an Append is sent
        // only through `commit`; `/access` takes the Replace.
        {
            let req = signer
                .replace_epoch(journal.clone(), 6, epoch_key.clone(), revoked.clone())
                .unwrap();
            assert_eq!(req.path, format!("/ds/group/{}/access", "45".repeat(32)));
            let s = statement_of(&req);
            s.verify(now_ms()).unwrap();
            assert_eq!(
                s.body,
                StatementBody::RegisterEpoch {
                    epoch: 6,
                    epoch_key: epoch_key.clone().try_into().unwrap(),
                    mode: RegisterMode::Replace,
                    revoke: vec![[0x09; 32]],
                }
            );
        }
        let member = ds_statement_signer(vec![0x22; 32]).unwrap().public_key();
        let admin = ds_statement_signer(vec![0x23; 32]).unwrap().public_key();
        let req = signer
            .registrars_set(
                journal.clone(),
                vec![
                    DsRegistrarEntry {
                        public_key: member.clone(),
                        rank: DsRegistrarRank::Member,
                    },
                    DsRegistrarEntry {
                        public_key: admin.clone(),
                        rank: DsRegistrarRank::Admin,
                    },
                ],
            )
            .unwrap();
        assert!(req.path.ends_with("/registrars-set"));
        let s = statement_of(&req);
        s.verify(now_ms()).unwrap();
        assert_eq!(s.body.kind(), StatementKind::RegistrarsSet);
        let StatementBody::RegistrarsSet { registrars } = s.body else {
            panic!("not a registrars set");
        };
        assert_eq!(registrars.len(), 2);
        assert_eq!(registrars[0].key.to_vec(), member);
        assert_eq!(registrars[1].rank, Rank::Admin);
        // Empty removes all; the signer's own key and a short key are refused.
        let empty = signer.registrars_set(journal.clone(), vec![]).unwrap();
        assert!(matches!(
            statement_of(&empty).body,
            StatementBody::RegistrarsSet { registrars } if registrars.is_empty()
        ));
        assert!(signer
            .registrars_set(
                journal.clone(),
                vec![DsRegistrarEntry {
                    public_key: signer.public_key(),
                    rank: DsRegistrarRank::Admin,
                }],
            )
            .is_err());
        assert!(signer
            .registrars_set(
                journal,
                vec![DsRegistrarEntry {
                    public_key: vec![1; 31],
                    rank: DsRegistrarRank::Member,
                }],
            )
            .is_err());
    }

    #[test]
    fn a_registrar_seed_travels_sealed_to_its_account_only() {
        let root_account = Identity::new("owner".into()).unwrap();
        let member = Identity::new("member".into()).unwrap();
        let journal = vec![0x43; 32];
        let root_seed = ds_journal_root_seed(
            root_account.inner.ed25519_seed_bytes().to_vec(),
            journal.clone(),
        )
        .unwrap();
        let member_key = member.inner.signature_public_key().to_vec();
        let seed = ds_journal_registrar_seed(root_seed, member_key.clone()).unwrap();
        let wire = compose_registrar_seed_envelope(
            journal.clone(),
            member_key.clone(),
            member.inner.x25519_public_key().to_vec(),
            DsRegistrarRank::Admin,
            seed.clone(),
        )
        .unwrap();
        assert_eq!(
            u32::try_from(wire.len()).unwrap(),
            registrar_seed_envelope_len()
        );
        let decoded = decode_registrar_seed_envelope(wire.clone()).unwrap();
        assert_eq!(decoded.journal_id, journal);
        assert_eq!(decoded.member_account_key, member_key);
        assert_eq!(decoded.rank, DsSeedEnvelopeRank::Admin);
        // The clear key is the one the DS knows the registrar by.
        assert_eq!(
            decoded.registrar_public_key,
            ds_statement_signer(seed.clone()).unwrap().public_key()
        );
        assert_eq!(
            open_registrar_seed_envelope(wire.clone(), &member).unwrap(),
            seed
        );
        assert!(open_registrar_seed_envelope(wire, &root_account).is_err());
    }

    /// ADR-0074 section 3.7 (2026-11-20): after a rotation the owner
    /// addresses the envelope to the conversation key the roster names and
    /// seals it to the X25519 key the directory serves (the current
    /// identity's). The member opens it with both of its identities.
    #[test]
    fn a_registrar_seed_addressed_to_the_conversation_key_opens_sealed_to_the_current_one() {
        let conversation = Identity::new("conversation".into()).unwrap();
        let current = Identity::new("current".into()).unwrap();
        let stranger = Identity::new("stranger".into()).unwrap();
        let conversation_key = conversation.inner.signature_public_key().to_vec();
        let seed = ds_journal_registrar_seed(vec![0x51; 32], conversation_key.clone()).unwrap();
        let wire = compose_registrar_seed_envelope(
            vec![0x45; 32],
            conversation_key,
            current.inner.x25519_public_key().to_vec(),
            DsRegistrarRank::Member,
            seed.clone(),
        )
        .unwrap();
        // The conversation identity alone cannot open it: another X25519 key.
        assert!(open_registrar_seed_envelope(wire.clone(), &conversation).is_err());
        assert_eq!(
            open_registrar_seed_envelope_sealed_to(wire.clone(), &conversation, &current).unwrap(),
            seed
        );
        // The address is still checked: the current identity is not the
        // addressee, and a stranger's X25519 key does not open it.
        assert!(open_registrar_seed_envelope_sealed_to(wire.clone(), &current, &current).is_err());
        assert!(open_registrar_seed_envelope_sealed_to(wire, &conversation, &stranger).is_err());
    }

    #[test]
    fn a_root_handover_lets_the_new_owner_sign_as_the_old_root() {
        let old_owner = Identity::new("old owner".into()).unwrap();
        let new_owner = Identity::new("new owner".into()).unwrap();
        let journal = vec![0x44; 32];
        let old_root_seed = ds_journal_root_seed(
            old_owner.inner.ed25519_seed_bytes().to_vec(),
            journal.clone(),
        )
        .unwrap();
        let new_owner_key = new_owner.inner.signature_public_key().to_vec();
        let wire = compose_root_handover_envelope(
            journal.clone(),
            new_owner_key.clone(),
            new_owner.inner.x25519_public_key().to_vec(),
            old_root_seed.clone(),
        )
        .unwrap();
        let decoded = decode_registrar_seed_envelope(wire.clone()).unwrap();
        assert_eq!(decoded.rank, DsSeedEnvelopeRank::Root);
        assert_eq!(decoded.member_account_key, new_owner_key);
        assert_eq!(
            decoded.registrar_public_key,
            ds_journal_root_signer(&old_owner, journal.clone())
                .unwrap()
                .public_key()
        );
        let opened = open_registrar_seed_envelope(wire.clone(), &new_owner).unwrap();
        assert_eq!(opened, old_root_seed);
        assert!(open_registrar_seed_envelope(wire, &old_owner).is_err());
        // The opened seed signs as the old root; the transfer names the new
        // owner's own root.
        let old_root = ds_journal_root_signer(&old_owner, journal.clone()).unwrap();
        let handed = ds_statement_signer(opened).unwrap();
        assert_eq!(handed.public_key(), old_root.public_key());
        let new_root = ds_journal_root_signer(&new_owner, journal.clone()).unwrap();
        let req = handed
            .root_transfer(journal, new_root.public_key())
            .unwrap();
        let s = statement_of(&req);
        assert_eq!(s.body.kind(), StatementKind::RootTransfer);
        assert_eq!(s.signer.to_vec(), old_root.public_key());
        s.verify(now_ms()).unwrap();
    }
}
