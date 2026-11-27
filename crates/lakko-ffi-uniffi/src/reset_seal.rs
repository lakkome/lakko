// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for the reset seal ([ADR-0101] section 10) and the R.2
//! verdict with its rule 4 (ADR-0073, amended 2026-11-26 the second time).
//! Mirror of `crates/lakko-ffi-wasm/src/reset_seal.rs`.
//!
//! - Seal keys: [`ResetSealKey`], made from the password's OPAQUE export key,
//!   a passkey's WebAuthn `prf` output at [`reset_seal_passkey_prf_salt`], or
//!   the recovery phrase. It signs the reset seal and gives its announcement
//!   entry.
//! - The seal announcement: [`sign_seal_announcement`] (by the account
//!   identity), [`decode_seal_announcement`], [`open_seal_announcement`].
//! - The peer's check: [`check_reset_seal`] (the signature half of rule 4),
//!   [`verify_seal_announcement_in_log`] (its KT-log half), and
//!   [`reset_verdict`] (R.2 with rule 4, the one table the clients used to
//!   write three times).
//!
//! Refusals are [`FfiError::ResetSeal`] with `"<code>: <reason>"`.
//!
//! [ADR-0101]: ../../../docs/adr/0101-reaching-a-reset-account.md

use std::sync::Arc;

use lakko_attest::reset_seal::{
    check_reset_seal as rust_check, passkey_prf_salt, passkey_seal_key, passkey_seal_tag,
    password_seal_key, phrase_seal_key, ResetSeal, ResetSealError, SealAnnouncement,
    SealCheckInput, SealEntry, SealKind,
};
use lakko_contacts::reset_verdict::{
    reset_verdict as rust_verdict, AttestCheck, LogCheck, LogStatus, ResetRefusal, ResetVerdict,
    ResetVerdictInput, SealLog, SealSignature,
};
use lakko_crypto::signing::SigningKey;

use crate::mnemonic::RecoveryMnemonic;
use crate::{FfiError, Identity};

fn err(code: &str, reason: impl core::fmt::Display) -> FfiError {
    FfiError::ResetSeal(format!("{code}: {reason}"))
}

fn seal_err(e: ResetSealError) -> FfiError {
    err(e.code(), e)
}

fn array<const N: usize>(bytes: &[u8], what: &str) -> Result<[u8; N], FfiError> {
    bytes.try_into().map_err(|_| {
        err(
            "keyLength",
            format!("{what} must be {N} bytes, got {}", bytes.len()),
        )
    })
}

fn account_ref(account_id: &[u8]) -> Result<[u8; 32], FfiError> {
    Ok(lakko_attest::key_reset_account_ref(&array::<16>(
        account_id,
        "accountId",
    )?))
}

/// What a seal key comes from.
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiSealKind {
    /// The password's OPAQUE export key.
    Password,
    /// A passkey's WebAuthn `prf` output.
    Passkey,
    /// The recovery phrase.
    Phrase,
}

impl From<SealKind> for FfiSealKind {
    fn from(k: SealKind) -> Self {
        match k {
            SealKind::Password => Self::Password,
            SealKind::Passkey => Self::Passkey,
            SealKind::Phrase => Self::Phrase,
        }
    }
}

impl From<FfiSealKind> for SealKind {
    fn from(k: FfiSealKind) -> Self {
        match k {
            FfiSealKind::Password => Self::Password,
            FfiSealKind::Passkey => Self::Passkey,
            FfiSealKind::Phrase => Self::Phrase,
        }
    }
}

/// One announced seal key.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiSealEntry {
    /// What it comes from.
    pub kind: FfiSealKind,
    /// 16 bytes: the passkey's tag, all zero for a password or phrase key.
    pub tag: Vec<u8>,
    /// The 32-byte seal public key.
    pub seal_pk: Vec<u8>,
}

impl TryFrom<&FfiSealEntry> for SealEntry {
    type Error = FfiError;
    fn try_from(e: &FfiSealEntry) -> Result<Self, FfiError> {
        Ok(Self {
            kind: e.kind.into(),
            tag: array::<16>(&e.tag, "tag")?,
            seal_pk: array::<32>(&e.seal_pk, "sealPk")?,
        })
    }
}

impl From<&SealEntry> for FfiSealEntry {
    fn from(e: &SealEntry) -> Self {
        Self {
            kind: e.kind.into(),
            tag: e.tag.to_vec(),
            seal_pk: e.seal_pk.to_vec(),
        }
    }
}

/// A seal announcement's content.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiSealAnnouncement {
    /// `keyResetAccountRef(accountId)`.
    pub account_ref: Vec<u8>,
    /// The generation it was made at.
    pub key_generation: u64,
    /// Strictly increasing per account.
    pub seq: u64,
    /// When it was made, unix milliseconds.
    pub issued_at_ms: u64,
    /// The seal keys, ascending by kind and tag.
    pub entries: Vec<FfiSealEntry>,
}

impl From<&SealAnnouncement> for FfiSealAnnouncement {
    fn from(a: &SealAnnouncement) -> Self {
        Self {
            account_ref: a.account_ref.to_vec(),
            key_generation: a.key_generation,
            seq: a.seq,
            issued_at_ms: a.issued_at_ms,
            entries: a.entries.iter().map(Into::into).collect(),
        }
    }
}

/// A seal key. Holds the secret; nothing returns it.
#[derive(uniffi::Object)]
pub struct ResetSealKey {
    key: SigningKey,
    kind: SealKind,
}

#[uniffi::export]
impl ResetSealKey {
    /// The password seal key from the 64-byte OPAQUE export key of the
    /// account password (registration or a password sign-in).
    ///
    /// # Errors
    ///
    /// [`FfiError::ResetSeal`] `keyLength`.
    #[uniffi::constructor]
    pub fn from_password_export_key(export_key: Vec<u8>) -> Result<Arc<Self>, FfiError> {
        Ok(Arc::new(Self {
            key: password_seal_key(&array::<64>(&export_key, "exportKey")?),
            kind: SealKind::Password,
        }))
    }

    /// A passkey seal key from the passkey's 32-byte WebAuthn `prf` output
    /// at [`reset_seal_passkey_prf_salt`].
    ///
    /// # Errors
    ///
    /// [`FfiError::ResetSeal`] `keyLength`.
    #[uniffi::constructor]
    pub fn from_passkey_prf(prf_output: Vec<u8>) -> Result<Arc<Self>, FfiError> {
        Ok(Arc::new(Self {
            key: passkey_seal_key(&array::<32>(&prf_output, "prfOutput")?),
            kind: SealKind::Passkey,
        }))
    }

    /// The phrase seal key from the recovery phrase (its 64-byte BIP39
    /// seed, which stays here).
    #[uniffi::constructor]
    #[must_use]
    pub fn from_mnemonic(mnemonic: Arc<RecoveryMnemonic>) -> Arc<Self> {
        let seed = mnemonic.inner().to_seed();
        let mut bytes = [0u8; 64];
        bytes.copy_from_slice(seed.as_ref());
        let key = phrase_seal_key(&bytes);
        bytes.fill(0);
        Arc::new(Self {
            key,
            kind: SealKind::Phrase,
        })
    }

    /// What it comes from.
    #[must_use]
    pub fn kind(&self) -> FfiSealKind {
        self.kind.into()
    }

    /// The 32-byte seal public key.
    #[must_use]
    pub fn public_key(&self) -> Vec<u8> {
        self.key.verifying_key().to_bytes().to_vec()
    }

    /// This key's announcement entry. A passkey key needs the passkey's
    /// credential id (its tag is derived from it); a password or phrase key
    /// takes none.
    ///
    /// # Errors
    ///
    /// [`FfiError::ResetSeal`] `credentialId` when a passkey key has no
    /// credential id or another key has one.
    pub fn announcement_entry(
        &self,
        credential_id: Option<Vec<u8>>,
    ) -> Result<FfiSealEntry, FfiError> {
        let tag = match (self.kind, credential_id) {
            (SealKind::Passkey, Some(id)) => passkey_seal_tag(&id),
            (SealKind::Passkey, None) => {
                return Err(err(
                    "credentialId",
                    "a passkey seal entry needs its credential id",
                ))
            },
            (_, Some(_)) => {
                return Err(err(
                    "credentialId",
                    "only a passkey seal entry carries a credential id",
                ))
            },
            (_, None) => [0u8; 16],
        };
        Ok(FfiSealEntry {
            kind: self.kind.into(),
            tag: tag.to_vec(),
            seal_pk: self.public_key(),
        })
    }

    /// Sign the reset seal: the account `account_id` leaves
    /// `from_generation` for `from_generation + 1` with
    /// `new_signature_pubkey`, under the announcement numbered
    /// `announcement_seq` that lists this key.
    ///
    /// # Errors
    ///
    /// [`FfiError::ResetSeal`]: `keyLength`, `badGenerations` (no next
    /// generation), `badKey` (an unusable new key).
    pub fn sign_reset_seal(
        &self,
        account_id: Vec<u8>,
        announcement_seq: u64,
        from_generation: u64,
        new_signature_pubkey: Vec<u8>,
    ) -> Result<Vec<u8>, FfiError> {
        let seal = ResetSeal {
            account_ref: account_ref(&account_id)?,
            announcement_seq,
            from_generation,
            to_generation: from_generation
                .checked_add(1)
                .ok_or_else(|| seal_err(ResetSealError::BadGenerations))?,
            new_signature_pubkey: array::<32>(&new_signature_pubkey, "newSignaturePubkey")?,
            kind: self.kind,
            seal_pk: self.key.verifying_key().to_bytes(),
        };
        Ok(seal.sign(&self.key).map_err(seal_err)?.to_vec())
    }
}

/// The WebAuthn `prf` salt a passkey seal key is evaluated at (32 bytes).
#[uniffi::export]
#[must_use]
pub fn reset_seal_passkey_prf_salt() -> Vec<u8> {
    passkey_prf_salt().to_vec()
}

/// A passkey's tag in the announcement: the first 16 bytes of
/// `SHA-256(credential_id)`.
#[uniffi::export]
#[must_use]
pub fn reset_seal_passkey_tag(credential_id: Vec<u8>) -> Vec<u8> {
    passkey_seal_tag(&credential_id).to_vec()
}

/// Sign the account's seal announcement with the account identity.
///
/// # Errors
///
/// [`FfiError::ResetSeal`]: `keyLength`, `badCount`, `badEntries`,
/// `unexpectedTag`, `notCanonical` (two entries share a kind and tag),
/// `badKey`.
#[uniffi::export]
pub fn sign_seal_announcement(
    account_identity: Arc<Identity>,
    account_id: Vec<u8>,
    key_generation: u64,
    seq: u64,
    issued_at_ms: u64,
    entries: Vec<FfiSealEntry>,
) -> Result<Vec<u8>, FfiError> {
    let entries = entries
        .iter()
        .map(SealEntry::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    let announcement = SealAnnouncement::new(
        account_ref(&account_id)?,
        key_generation,
        seq,
        issued_at_ms,
        entries,
    );
    let key = SigningKey::from_seed(&account_identity.inner.ed25519_seed_bytes());
    announcement.sign(&key).map_err(seal_err)
}

/// Decode an announcement without checking its signature (a device reading
/// its own account's current announcement to see whether its key is listed).
///
/// # Errors
///
/// [`FfiError::ResetSeal`] with the codec's code.
#[uniffi::export]
pub fn decode_seal_announcement(announcement: Vec<u8>) -> Result<FfiSealAnnouncement, FfiError> {
    SealAnnouncement::decode_unverified(&announcement)
        .map(|a| (&a).into())
        .map_err(seal_err)
}

/// Decode an announcement and verify it under `account_key`.
///
/// # Errors
///
/// [`FfiError::ResetSeal`] with the codec's code or `badSignature`.
#[uniffi::export]
pub fn open_seal_announcement(
    announcement: Vec<u8>,
    account_key: Vec<u8>,
) -> Result<FfiSealAnnouncement, FfiError> {
    let key = lakko_crypto::signing::VerifyingKey::from_bytes(&account_key)
        .map_err(|_| err("badKey", "account key is not an Ed25519 key"))?;
    SealAnnouncement::open(&announcement, &key)
        .map(|a| (&a).into())
        .map_err(seal_err)
}

/// What [`check_reset_seal`] found.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiSealCheck {
    /// Rule 4's signature half holds.
    pub valid: bool,
    /// The previous announcement's `seq` when valid (0 otherwise); raise the
    /// kept floor to it on a sealed verdict.
    pub announcement_seq: u64,
    /// The kind of seal key that signed, when valid.
    pub kind: Option<FfiSealKind>,
    /// The refusal code when not valid (`noAnnouncement`, `noSeal`,
    /// `announcementInvalid`, `announcementAccount`,
    /// `announcementGeneration`, `announcementRolledBack`, `sealInvalid`,
    /// `sealMismatch`, `sealKeyNotAnnounced`), for logs.
    pub refusal: Option<String>,
}

/// ADR-0073 R.2 rule 4, its signature half
/// (`lakko_attest::check_reset_seal`).
///
/// # Errors
///
/// [`FfiError::ResetSeal`] `keyLength` for a wrong-length argument; the
/// outcome itself is in the record.
#[uniffi::export]
#[allow(clippy::too_many_arguments)]
pub fn check_reset_seal(
    previous_announcement: Option<Vec<u8>>,
    reset_seal: Option<Vec<u8>>,
    held_account_key: Vec<u8>,
    account_id: Vec<u8>,
    accepted_generation: u64,
    attested_generation: u64,
    fetched_key: Vec<u8>,
    seq_floor: u64,
) -> Result<FfiSealCheck, FfiError> {
    let held = array::<32>(&held_account_key, "heldAccountKey")?;
    let fetched = array::<32>(&fetched_key, "fetchedKey")?;
    let account_ref = account_ref(&account_id)?;
    let outcome = rust_check(&SealCheckInput {
        previous_announcement: previous_announcement.as_deref(),
        reset_seal: reset_seal.as_deref(),
        held_account_key: &held,
        account_ref: &account_ref,
        accepted_generation,
        attested_generation,
        fetched_key: &fetched,
        seq_floor,
    });
    Ok(match outcome {
        Ok(sealed) => FfiSealCheck {
            valid: true,
            announcement_seq: sealed.announcement_seq,
            kind: Some(sealed.kind.into()),
            refusal: None,
        },
        Err(refusal) => FfiSealCheck {
            valid: false,
            announcement_seq: 0,
            kind: None,
            refusal: Some(refusal.code().to_string()),
        },
    })
}

/// ADR-0073 R.2 rule 4, its KT-log half
/// (`lakko_transparency::verify_announcement_before_reset`): the served
/// announcement is in the operator-signed root at `leaf_index`, below the
/// reset leaf. The caller has checked the root's no-rollback and witness
/// gates; the operator signature is checked here again.
///
/// # Errors
///
/// [`FfiError::ResetSeal`] `keyLength` / `badKey` for malformed arguments.
/// A proof that does not hold is `Ok(false)` (rule 4 unsealed).
#[uniffi::export]
#[allow(clippy::too_many_arguments)]
pub fn verify_seal_announcement_in_log(
    operator_pubkey: Vec<u8>,
    tree_root: Vec<u8>,
    log_size: u64,
    timestamp_unix: u64,
    root_signature: Vec<u8>,
    leaf_index: u64,
    audit_path: Vec<u8>,
    reset_leaf_index: u64,
    account_id: Vec<u8>,
    key_generation: u64,
    seq: u64,
    announcement: Vec<u8>,
) -> Result<bool, FfiError> {
    let operator = lakko_crypto::signing::VerifyingKey::from_bytes(&operator_pubkey)
        .map_err(|_| err("badKey", "operator key is not an Ed25519 key"))?;
    let signature = lakko_crypto::signing::Signature::from_bytes(&root_signature)
        .map_err(|_| err("keyLength", "rootSignature must be 64 bytes"))?;
    if audit_path.len() % 32 != 0 {
        return Err(err("keyLength", "auditPath must be a multiple of 32 bytes"));
    }
    let audit_path = audit_path
        .chunks_exact(32)
        .map(|c| {
            let mut h = [0u8; 32];
            h.copy_from_slice(c);
            h
        })
        .collect();
    let root = lakko_transparency::SignedRoot {
        tree_root: array::<32>(&tree_root, "treeRoot")?,
        log_size,
        timestamp_unix,
        signature,
    };
    if root.verify(&operator).is_err() {
        return Ok(false);
    }
    let account = array::<16>(&account_id, "accountId")?;
    let leaf = lakko_transparency::AnnouncementLeaf::for_announcement(
        lakko_transparency::account_username_hash(&account),
        key_generation,
        seq,
        &announcement,
    );
    let proof = lakko_transparency::InclusionProof {
        leaf_index,
        audit_path,
    };
    Ok(
        lakko_transparency::verify_announcement_before_reset(
            &leaf,
            &proof,
            reset_leaf_index,
            &root,
        )
        .is_ok(),
    )
}

/// What the attestation check found.
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiAttestCheck {
    /// No attestation in the bundle.
    Absent,
    /// It did not verify or decode.
    Invalid,
    /// No attest key could be obtained.
    NoKey,
    /// It verified for this account and the fetched key.
    Verified {
        /// The attested generation.
        key_generation: u64,
        /// Its mint time, unix milliseconds.
        signed_at_ms: i64,
    },
}

/// The key-log answer for the fetched key.
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiLogStatus {
    /// The log holds the fetched key as the latest entry.
    Confirmed,
    /// No entry for the account.
    Absent,
    /// The served root is older than one already seen.
    Rollback,
    /// The log disagrees with the fetched key.
    Mismatch,
    /// The log could not be reached.
    Unreachable,
    /// The build has no key log.
    Off,
}

/// The KT-log half of rule 4.
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiSealLog {
    /// [`verify_seal_announcement_in_log`] returned `true`.
    Proven,
    /// It returned `false`, or the log served no announcement.
    NotProven,
    /// The log could not be reached for the announcement.
    Unreachable,
}

/// The inputs of [`reset_verdict`].
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiResetVerdictInput {
    /// The key this device holds for the account.
    pub held_key: Vec<u8>,
    /// The generation this device last accepted (0 when none).
    pub accepted_generation: u64,
    /// The fetched bundle's key.
    pub fetched_key: Vec<u8>,
    /// The fetched bundle's generation.
    pub bundle_generation: u64,
    /// The attestation check.
    pub attest: FfiAttestCheck,
    /// The key-log answer.
    pub log_status: FfiLogStatus,
    /// The latest entry's `is_reset`, when one was read.
    pub log_is_reset: Option<bool>,
    /// [`check_reset_seal`]'s result.
    pub seal: FfiSealCheck,
    /// The seal's KT-log half (ignored when the log is off).
    pub seal_log: FfiSealLog,
}

/// Which refusal copy to show.
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiResetRefusal {
    /// The log shows the new key and records no reset.
    Unexplained,
    /// Everything else that is negative.
    Unconfirmed,
}

/// The outcome of R.2 with rule 4.
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiResetVerdict {
    /// The same person: re-pin and move everything.
    Sealed {
        /// The attested generation.
        key_generation: u64,
        /// The attestation's mint time.
        signed_at_ms: i64,
        /// The previous announcement's `seq`: the new floor.
        announcement_seq: u64,
    },
    /// A new account under the same username (ADR-0101 section 11).
    Unsealed {
        /// The attested generation.
        key_generation: u64,
        /// The attestation's mint time.
        signed_at_ms: i64,
    },
    /// The fail-closed refusal.
    Refused {
        /// Which copy.
        why: FfiResetRefusal,
    },
    /// Nothing can be decided yet.
    NoVerdict,
}

/// ADR-0073 R.2 with rule 4 (`lakko_contacts::reset_verdict`).
#[uniffi::export]
#[must_use]
pub fn reset_verdict(input: FfiResetVerdictInput) -> FfiResetVerdict {
    let attest = match input.attest {
        FfiAttestCheck::Absent => AttestCheck::Absent,
        FfiAttestCheck::Invalid => AttestCheck::Invalid,
        FfiAttestCheck::NoKey => AttestCheck::NoKey,
        FfiAttestCheck::Verified {
            key_generation,
            signed_at_ms,
        } => AttestCheck::Verified {
            key_generation,
            signed_at_ms,
        },
    };
    let status = match input.log_status {
        FfiLogStatus::Confirmed => LogStatus::Confirmed,
        FfiLogStatus::Absent => LogStatus::Absent,
        FfiLogStatus::Rollback => LogStatus::Rollback,
        FfiLogStatus::Mismatch => LogStatus::Mismatch,
        FfiLogStatus::Unreachable => LogStatus::Unreachable,
        FfiLogStatus::Off => LogStatus::Off,
    };
    let seal_signature = if input.seal.valid {
        SealSignature::Valid {
            announcement_seq: input.seal.announcement_seq,
        }
    } else {
        SealSignature::Invalid
    };
    let seal_log = match input.seal_log {
        FfiSealLog::Proven => SealLog::Proven,
        FfiSealLog::NotProven => SealLog::NotProven,
        FfiSealLog::Unreachable => SealLog::Unreachable,
    };
    match rust_verdict(&ResetVerdictInput {
        held_key: &input.held_key,
        accepted_generation: input.accepted_generation,
        fetched_key: &input.fetched_key,
        bundle_generation: input.bundle_generation,
        attest,
        log: LogCheck {
            status,
            is_reset: input.log_is_reset,
        },
        seal_signature,
        seal_log,
    }) {
        ResetVerdict::Sealed {
            key_generation,
            signed_at_ms,
            announcement_seq,
        } => FfiResetVerdict::Sealed {
            key_generation,
            signed_at_ms,
            announcement_seq,
        },
        ResetVerdict::Unsealed {
            key_generation,
            signed_at_ms,
        } => FfiResetVerdict::Unsealed {
            key_generation,
            signed_at_ms,
        },
        ResetVerdict::Refused(why) => FfiResetVerdict::Refused {
            why: match why {
                ResetRefusal::Unexplained => FfiResetRefusal::Unexplained,
                ResetRefusal::Unconfirmed => FfiResetRefusal::Unconfirmed,
            },
        },
        ResetVerdict::NoVerdict => FfiResetVerdict::NoVerdict,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const ACCOUNT: [u8; 16] = [0x0A; 16];

    fn identity(byte: u8) -> Arc<Identity> {
        Arc::new(Identity::from_seed(vec![byte; 32]).unwrap())
    }

    fn identity_pk(id: &Identity) -> Vec<u8> {
        SigningKey::from_seed(&id.inner.ed25519_seed_bytes())
            .verifying_key()
            .to_bytes()
            .to_vec()
    }

    #[test]
    fn a_sealed_reset_round_trips_through_the_surface() {
        let old = identity(1);
        let new = identity(2);
        let password = ResetSealKey::from_password_export_key(vec![0x10; 64]).unwrap();
        let passkey = ResetSealKey::from_passkey_prf(vec![0x20; 32]).unwrap();
        let phrase = ResetSealKey::from_mnemonic(Arc::new(RecoveryMnemonic::generate()));
        let entries = vec![
            passkey.announcement_entry(Some(b"cred".to_vec())).unwrap(),
            password.announcement_entry(None).unwrap(),
            phrase.announcement_entry(None).unwrap(),
        ];
        assert!(password.announcement_entry(Some(vec![1])).is_err());
        assert!(passkey.announcement_entry(None).is_err());
        let announcement =
            sign_seal_announcement(old.clone(), ACCOUNT.to_vec(), 1, 5, 99, entries).unwrap();
        let opened = open_seal_announcement(announcement.clone(), identity_pk(&old)).unwrap();
        assert_eq!(opened.seq, 5);
        assert_eq!(opened.entries.len(), 3);
        assert_eq!(opened.entries[0].kind, FfiSealKind::Password);
        assert!(open_seal_announcement(announcement.clone(), identity_pk(&new)).is_err());
        assert_eq!(
            decode_seal_announcement(announcement.clone()).unwrap(),
            opened
        );

        for key in [&password, &passkey, &phrase] {
            let seal = key
                .sign_reset_seal(ACCOUNT.to_vec(), 5, 1, identity_pk(&new))
                .unwrap();
            let check = check_reset_seal(
                Some(announcement.clone()),
                Some(seal),
                identity_pk(&old),
                ACCOUNT.to_vec(),
                1,
                2,
                identity_pk(&new),
                5,
            )
            .unwrap();
            assert!(check.valid, "{:?}", check.refusal);
            assert_eq!(check.kind, Some(key.kind()));
            let verdict = reset_verdict(FfiResetVerdictInput {
                held_key: identity_pk(&old),
                accepted_generation: 1,
                fetched_key: identity_pk(&new),
                bundle_generation: 2,
                attest: FfiAttestCheck::Verified {
                    key_generation: 2,
                    signed_at_ms: 3,
                },
                log_status: FfiLogStatus::Off,
                log_is_reset: None,
                seal: check,
                seal_log: FfiSealLog::NotProven,
            });
            assert_eq!(
                verdict,
                FfiResetVerdict::Sealed {
                    key_generation: 2,
                    signed_at_ms: 3,
                    announcement_seq: 5
                }
            );
        }
        // No seal: unsealed.
        let check = check_reset_seal(
            Some(announcement),
            None,
            identity_pk(&old),
            ACCOUNT.to_vec(),
            1,
            2,
            identity_pk(&new),
            0,
        )
        .unwrap();
        assert_eq!(check.refusal.as_deref(), Some("noSeal"));
        assert_eq!(
            reset_verdict(FfiResetVerdictInput {
                held_key: identity_pk(&old),
                accepted_generation: 1,
                fetched_key: identity_pk(&new),
                bundle_generation: 2,
                attest: FfiAttestCheck::Verified {
                    key_generation: 2,
                    signed_at_ms: 3,
                },
                log_status: FfiLogStatus::Off,
                log_is_reset: None,
                seal: check,
                seal_log: FfiSealLog::Proven,
            }),
            FfiResetVerdict::Unsealed {
                key_generation: 2,
                signed_at_ms: 3
            }
        );
    }

    #[test]
    fn the_log_half_proves_an_announcement_before_the_reset() {
        let operator = SigningKey::from_seed(&[0x33; 32]);
        let username = lakko_transparency::account_username_hash(&ACCOUNT);
        let announcement = b"announcement".to_vec();
        let leaf =
            lakko_transparency::AnnouncementLeaf::for_announcement(username, 1, 5, &announcement);
        let leaves = vec![
            b"genesis".to_vec(),
            leaf.canonical_bytes(),
            b"reset".to_vec(),
        ];
        let tree_root = lakko_transparency::merkle::root(&leaves);
        let sig = operator.sign(&lakko_transparency::SignedRoot::signing_payload(
            &tree_root, 3, 1,
        ));
        let path: Vec<u8> = lakko_transparency::merkle::audit_path(&leaves, 1).concat();
        let run = |reset_index: u64, seq: u64| {
            verify_seal_announcement_in_log(
                operator.verifying_key().to_bytes().to_vec(),
                tree_root.to_vec(),
                3,
                1,
                sig.to_bytes().to_vec(),
                1,
                path.clone(),
                reset_index,
                ACCOUNT.to_vec(),
                1,
                seq,
                announcement.clone(),
            )
            .unwrap()
        };
        assert!(run(2, 5));
        assert!(!run(1, 5));
        assert!(!run(2, 6));
        assert_eq!(reset_seal_passkey_prf_salt().len(), 32);
        assert_eq!(reset_seal_passkey_tag(b"x".to_vec()).len(), 16);
    }
}
