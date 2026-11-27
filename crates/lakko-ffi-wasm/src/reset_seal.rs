// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for the reset seal ([ADR-0101] section 10) and the
//! R.2 verdict with its rule 4 (ADR-0073, amended 2026-11-26 the second
//! time). Mirror of `crates/lakko-ffi-uniffi/src/reset_seal.rs`.
//!
//! - `ResetSealKey.fromPasswordExportKey(exportKey)`,
//!   `.fromPasskeyPrf(prfOutput)`, `.fromMnemonic(mnemonic)`; `kind`
//!   (`"password"`, `"passkey"`, `"phrase"`), `publicKey`,
//!   `announcementEntry(credentialId?)`, `signResetSeal(accountId,
//!   announcementSeq, fromGeneration, newSignaturePubkey)`.
//! - `resetSealPasskeyPrfSalt()`, `resetSealPasskeyTag(credentialId)`.
//! - `signSealAnnouncement(accountIdentity, accountId, keyGeneration, seq,
//!   issuedAtMs, kinds, tags, sealPks)` (parallel lists: kind names, 16-byte
//!   tags and 32-byte keys flat), `decodeSealAnnouncement(bytes)`,
//!   `openSealAnnouncement(bytes, accountKey)` -> `SealAnnouncement`.
//! - `checkResetSeal(...)` -> `SealCheck`; `verifySealAnnouncementInLog(...)`
//!   -> boolean; `resetVerdict(...)` -> `ResetVerdict` (`kind` is
//!   `"sealed"`, `"unsealed"`, `"refused"` or `"no-verdict"`).
//!
//! Every refusal throws a string `"resetSeal.<code>: <reason>"`.
//!
//! [ADR-0101]: ../../../docs/adr/0101-reaching-a-reset-account.md

use lakko_attest::reset_seal::{
    check_reset_seal as rust_check, passkey_prf_salt, passkey_seal_key, passkey_seal_tag,
    password_seal_key, phrase_seal_key, ResetSeal, ResetSealError, SealAnnouncement as RustAnn,
    SealCheckInput, SealEntry, SealKind,
};
use lakko_contacts::reset_verdict::{
    reset_verdict as rust_verdict, AttestCheck, LogCheck, LogStatus, ResetRefusal,
    ResetVerdict as RustVerdict, ResetVerdictInput, SealLog, SealSignature,
};
use lakko_crypto::signing::SigningKey;
use wasm_bindgen::prelude::*;

use crate::mnemonic::RecoveryMnemonic;

fn err(code: &str, reason: impl core::fmt::Display) -> String {
    format!("resetSeal.{code}: {reason}")
}

fn seal_err(e: ResetSealError) -> String {
    err(e.code(), e)
}

fn array<const N: usize>(bytes: &[u8], what: &str) -> Result<[u8; N], String> {
    <[u8; N]>::try_from(bytes).map_err(|_| {
        err(
            "keyLength",
            format!("{what} must be {N} bytes, got {}", bytes.len()),
        )
    })
}

fn account_ref(account_id: &[u8]) -> Result<[u8; 32], String> {
    Ok(lakko_attest::key_reset_account_ref(&array::<16>(
        account_id,
        "accountId",
    )?))
}

const fn kind_name(kind: SealKind) -> &'static str {
    match kind {
        SealKind::Password => "password",
        SealKind::Passkey => "passkey",
        SealKind::Phrase => "phrase",
    }
}

fn kind_from_name(name: &str) -> Result<SealKind, String> {
    match name {
        "password" => Ok(SealKind::Password),
        "passkey" => Ok(SealKind::Passkey),
        "phrase" => Ok(SealKind::Phrase),
        other => Err(err("unknownKind", format!("unknown seal kind {other:?}"))),
    }
}

/// A seal key. Holds the secret; nothing returns it.
#[wasm_bindgen]
pub struct ResetSealKey {
    key: SigningKey,
    kind: SealKind,
}

#[wasm_bindgen]
impl ResetSealKey {
    /// The password seal key from the 64-byte OPAQUE export key.
    ///
    /// # Errors
    ///
    /// `resetSeal.keyLength`.
    #[wasm_bindgen(js_name = fromPasswordExportKey)]
    pub fn from_password_export_key(export_key: &[u8]) -> Result<ResetSealKey, String> {
        Ok(Self {
            key: password_seal_key(&array::<64>(export_key, "exportKey")?),
            kind: SealKind::Password,
        })
    }

    /// A passkey seal key from the 32-byte WebAuthn `prf` output at
    /// `resetSealPasskeyPrfSalt()`.
    ///
    /// # Errors
    ///
    /// `resetSeal.keyLength`.
    #[wasm_bindgen(js_name = fromPasskeyPrf)]
    pub fn from_passkey_prf(prf_output: &[u8]) -> Result<ResetSealKey, String> {
        Ok(Self {
            key: passkey_seal_key(&array::<32>(prf_output, "prfOutput")?),
            kind: SealKind::Passkey,
        })
    }

    /// The phrase seal key from the recovery phrase.
    #[wasm_bindgen(js_name = fromMnemonic)]
    #[must_use]
    pub fn from_mnemonic(mnemonic: &RecoveryMnemonic) -> ResetSealKey {
        let seed = mnemonic.inner().to_seed();
        let mut bytes = [0u8; 64];
        bytes.copy_from_slice(seed.as_ref());
        let key = phrase_seal_key(&bytes);
        bytes.fill(0);
        Self {
            key,
            kind: SealKind::Phrase,
        }
    }

    /// `"password"`, `"passkey"` or `"phrase"`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn kind(&self) -> String {
        kind_name(self.kind).to_string()
    }

    /// The 32-byte seal public key.
    #[wasm_bindgen(getter, js_name = publicKey)]
    #[must_use]
    pub fn public_key(&self) -> Vec<u8> {
        self.key.verifying_key().to_bytes().to_vec()
    }

    /// This key's 16-byte announcement tag: from the credential id for a
    /// passkey key, all zero for the others (which take none).
    ///
    /// # Errors
    ///
    /// `resetSeal.credentialId`.
    #[wasm_bindgen(js_name = announcementTag)]
    pub fn announcement_tag(&self, credential_id: Option<Vec<u8>>) -> Result<Vec<u8>, String> {
        match (self.kind, credential_id) {
            (SealKind::Passkey, Some(id)) => Ok(passkey_seal_tag(&id).to_vec()),
            (SealKind::Passkey, None) => Err(err(
                "credentialId",
                "a passkey seal entry needs its credential id",
            )),
            (_, Some(_)) => Err(err(
                "credentialId",
                "only a passkey seal entry carries a credential id",
            )),
            (_, None) => Ok(vec![0u8; 16]),
        }
    }

    /// Sign the reset seal from `fromGeneration` to `fromGeneration + 1`.
    ///
    /// # Errors
    ///
    /// `resetSeal.keyLength`, `resetSeal.badGenerations`, `resetSeal.badKey`.
    #[wasm_bindgen(js_name = signResetSeal)]
    pub fn sign_reset_seal(
        &self,
        account_id: &[u8],
        announcement_seq: u64,
        from_generation: u64,
        new_signature_pubkey: &[u8],
    ) -> Result<Vec<u8>, String> {
        let seal = ResetSeal {
            account_ref: account_ref(account_id)?,
            announcement_seq,
            from_generation,
            to_generation: from_generation
                .checked_add(1)
                .ok_or_else(|| seal_err(ResetSealError::BadGenerations))?,
            new_signature_pubkey: array::<32>(new_signature_pubkey, "newSignaturePubkey")?,
            kind: self.kind,
            seal_pk: self.key.verifying_key().to_bytes(),
        };
        Ok(seal.sign(&self.key).map_err(seal_err)?.to_vec())
    }
}

/// The WebAuthn `prf` salt a passkey seal key is evaluated at.
#[wasm_bindgen(js_name = resetSealPasskeyPrfSalt)]
#[must_use]
pub fn reset_seal_passkey_prf_salt() -> Vec<u8> {
    passkey_prf_salt().to_vec()
}

/// A passkey's 16-byte tag in the announcement.
#[wasm_bindgen(js_name = resetSealPasskeyTag)]
#[must_use]
pub fn reset_seal_passkey_tag(credential_id: &[u8]) -> Vec<u8> {
    passkey_seal_tag(credential_id).to_vec()
}

/// A decoded seal announcement.
#[wasm_bindgen]
pub struct SealAnnouncement {
    inner: RustAnn,
}

#[wasm_bindgen]
impl SealAnnouncement {
    /// `keyResetAccountRef(accountId)`.
    #[wasm_bindgen(getter, js_name = accountRef)]
    #[must_use]
    pub fn account_ref(&self) -> Vec<u8> {
        self.inner.account_ref.to_vec()
    }

    /// The generation it was made at.
    #[wasm_bindgen(getter, js_name = keyGeneration)]
    #[must_use]
    pub fn key_generation(&self) -> u64 {
        self.inner.key_generation
    }

    /// Its `seq`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn seq(&self) -> u64 {
        self.inner.seq
    }

    /// When it was made, unix milliseconds.
    #[wasm_bindgen(getter, js_name = issuedAtMs)]
    #[must_use]
    pub fn issued_at_ms(&self) -> u64 {
        self.inner.issued_at_ms
    }

    /// The entries' kinds, in order.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn kinds(&self) -> Vec<String> {
        self.inner
            .entries
            .iter()
            .map(|e| kind_name(e.kind).to_string())
            .collect()
    }

    /// The entries' 16-byte tags, concatenated.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn tags(&self) -> Vec<u8> {
        self.inner.entries.iter().flat_map(|e| e.tag).collect()
    }

    /// The entries' 32-byte seal keys, concatenated.
    #[wasm_bindgen(getter, js_name = sealPks)]
    #[must_use]
    pub fn seal_pks(&self) -> Vec<u8> {
        self.inner.entries.iter().flat_map(|e| e.seal_pk).collect()
    }
}

fn entries(kinds: &[String], tags: &[u8], seal_pks: &[u8]) -> Result<Vec<SealEntry>, String> {
    if tags.len() != kinds.len() * 16 || seal_pks.len() != kinds.len() * 32 {
        return Err(err(
            "keyLength",
            "tags and sealPks must hold 16 and 32 bytes per kind",
        ));
    }
    kinds
        .iter()
        .enumerate()
        .map(|(i, k)| {
            Ok(SealEntry {
                kind: kind_from_name(k)?,
                tag: array::<16>(&tags[i * 16..(i + 1) * 16], "tag")?,
                seal_pk: array::<32>(&seal_pks[i * 32..(i + 1) * 32], "sealPk")?,
            })
        })
        .collect()
}

/// Sign the account's seal announcement with the account identity.
///
/// # Errors
///
/// `resetSeal.<code>` of the codec, `resetSeal.keyLength`,
/// `resetSeal.unknownKind`.
#[wasm_bindgen(js_name = signSealAnnouncement)]
#[allow(clippy::too_many_arguments)]
pub fn sign_seal_announcement(
    account_identity: &crate::mls::Identity,
    account_id: &[u8],
    key_generation: u64,
    seq: u64,
    issued_at_ms: u64,
    kinds: Vec<String>,
    tags: &[u8],
    seal_pks: &[u8],
) -> Result<Vec<u8>, String> {
    let announcement = RustAnn::new(
        account_ref(account_id)?,
        key_generation,
        seq,
        issued_at_ms,
        entries(&kinds, tags, seal_pks)?,
    );
    let key = SigningKey::from_seed(&account_identity.mls_identity().ed25519_seed_bytes());
    announcement.sign(&key).map_err(seal_err)
}

/// Decode an announcement without checking its signature.
///
/// # Errors
///
/// `resetSeal.<code>` of the codec.
#[wasm_bindgen(js_name = decodeSealAnnouncement)]
pub fn decode_seal_announcement(announcement: &[u8]) -> Result<SealAnnouncement, String> {
    RustAnn::decode_unverified(announcement)
        .map(|inner| SealAnnouncement { inner })
        .map_err(seal_err)
}

/// Decode an announcement and verify it under `accountKey`.
///
/// # Errors
///
/// `resetSeal.<code>` of the codec, or `resetSeal.badSignature`.
#[wasm_bindgen(js_name = openSealAnnouncement)]
pub fn open_seal_announcement(
    announcement: &[u8],
    account_key: &[u8],
) -> Result<SealAnnouncement, String> {
    let key = lakko_crypto::signing::VerifyingKey::from_bytes(account_key)
        .map_err(|_| err("badKey", "account key is not an Ed25519 key"))?;
    RustAnn::open(announcement, &key)
        .map(|inner| SealAnnouncement { inner })
        .map_err(seal_err)
}

/// What `checkResetSeal` found.
#[wasm_bindgen]
pub struct SealCheck {
    valid: bool,
    announcement_seq: u64,
    kind: Option<SealKind>,
    refusal: Option<&'static str>,
}

#[wasm_bindgen]
impl SealCheck {
    /// Rule 4's signature half holds.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn valid(&self) -> bool {
        self.valid
    }

    /// The previous announcement's `seq` when valid, else 0.
    #[wasm_bindgen(getter, js_name = announcementSeq)]
    #[must_use]
    pub fn announcement_seq(&self) -> u64 {
        self.announcement_seq
    }

    /// The signing key's kind when valid.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn kind(&self) -> Option<String> {
        self.kind.map(|k| kind_name(k).to_string())
    }

    /// The refusal code when not valid, for logs.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn refusal(&self) -> Option<String> {
        self.refusal.map(str::to_string)
    }
}

/// ADR-0073 R.2 rule 4, its signature half.
///
/// # Errors
///
/// `resetSeal.keyLength` for a wrong-length argument.
#[wasm_bindgen(js_name = checkResetSeal)]
#[allow(clippy::too_many_arguments)]
pub fn check_reset_seal(
    previous_announcement: Option<Vec<u8>>,
    reset_seal: Option<Vec<u8>>,
    held_account_key: &[u8],
    account_id: &[u8],
    accepted_generation: u64,
    attested_generation: u64,
    fetched_key: &[u8],
    seq_floor: u64,
) -> Result<SealCheck, String> {
    let held = array::<32>(held_account_key, "heldAccountKey")?;
    let fetched = array::<32>(fetched_key, "fetchedKey")?;
    let account_ref = account_ref(account_id)?;
    Ok(
        match rust_check(&SealCheckInput {
            previous_announcement: previous_announcement.as_deref(),
            reset_seal: reset_seal.as_deref(),
            held_account_key: &held,
            account_ref: &account_ref,
            accepted_generation,
            attested_generation,
            fetched_key: &fetched,
            seq_floor,
        }) {
            Ok(s) => SealCheck {
                valid: true,
                announcement_seq: s.announcement_seq,
                kind: Some(s.kind),
                refusal: None,
            },
            Err(r) => SealCheck {
                valid: false,
                announcement_seq: 0,
                kind: None,
                refusal: Some(r.code()),
            },
        },
    )
}

/// ADR-0073 R.2 rule 4, its KT-log half: the served announcement is in the
/// operator-signed root at `leafIndex`, below the reset leaf. `auditPath` is
/// the 32-byte hashes concatenated. `false` when the proof does not hold.
///
/// # Errors
///
/// `resetSeal.keyLength` / `resetSeal.badKey` for malformed arguments.
#[wasm_bindgen(js_name = verifySealAnnouncementInLog)]
#[allow(clippy::too_many_arguments)]
pub fn verify_seal_announcement_in_log(
    operator_pubkey: &[u8],
    tree_root: &[u8],
    log_size: u64,
    timestamp_unix: u64,
    root_signature: &[u8],
    leaf_index: u64,
    audit_path: &[u8],
    reset_leaf_index: u64,
    account_id: &[u8],
    key_generation: u64,
    seq: u64,
    announcement: &[u8],
) -> Result<bool, String> {
    let operator = lakko_crypto::signing::VerifyingKey::from_bytes(operator_pubkey)
        .map_err(|_| err("badKey", "operator key is not an Ed25519 key"))?;
    let signature = lakko_crypto::signing::Signature::from_bytes(root_signature)
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
        tree_root: array::<32>(tree_root, "treeRoot")?,
        log_size,
        timestamp_unix,
        signature,
    };
    if root.verify(&operator).is_err() {
        return Ok(false);
    }
    let leaf = lakko_transparency::AnnouncementLeaf::for_announcement(
        lakko_transparency::account_username_hash(&array::<16>(account_id, "accountId")?),
        key_generation,
        seq,
        announcement,
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

/// The outcome of R.2 with rule 4.
#[wasm_bindgen]
pub struct ResetVerdict {
    inner: RustVerdict,
}

#[wasm_bindgen]
impl ResetVerdict {
    /// `"sealed"`, `"unsealed"`, `"refused"` or `"no-verdict"`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn kind(&self) -> String {
        match self.inner {
            RustVerdict::Sealed { .. } => "sealed",
            RustVerdict::Unsealed { .. } => "unsealed",
            RustVerdict::Refused(_) => "refused",
            RustVerdict::NoVerdict => "no-verdict",
        }
        .to_string()
    }

    /// The attested generation for a sealed or unsealed verdict.
    #[wasm_bindgen(getter, js_name = keyGeneration)]
    #[must_use]
    pub fn key_generation(&self) -> Option<u64> {
        match self.inner {
            RustVerdict::Sealed { key_generation, .. }
            | RustVerdict::Unsealed { key_generation, .. } => Some(key_generation),
            _ => None,
        }
    }

    /// The attestation's mint time for a sealed or unsealed verdict.
    #[wasm_bindgen(getter, js_name = signedAtMs)]
    #[must_use]
    pub fn signed_at_ms(&self) -> Option<i64> {
        match self.inner {
            RustVerdict::Sealed { signed_at_ms, .. }
            | RustVerdict::Unsealed { signed_at_ms, .. } => Some(signed_at_ms),
            _ => None,
        }
    }

    /// The previous announcement's `seq` (the new floor) when sealed.
    #[wasm_bindgen(getter, js_name = announcementSeq)]
    #[must_use]
    pub fn announcement_seq(&self) -> Option<u64> {
        match self.inner {
            RustVerdict::Sealed {
                announcement_seq, ..
            } => Some(announcement_seq),
            _ => None,
        }
    }

    /// `"unexplained"` or `"unconfirmed"` when refused.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn why(&self) -> Option<String> {
        match self.inner {
            RustVerdict::Refused(ResetRefusal::Unexplained) => Some("unexplained".into()),
            RustVerdict::Refused(ResetRefusal::Unconfirmed) => Some("unconfirmed".into()),
            _ => None,
        }
    }
}

/// ADR-0073 R.2 with rule 4 (`lakko_contacts::reset_verdict`).
///
/// `attestKind` is `"absent"`, `"invalid"`, `"no-key"` or `"verified"` (then
/// `attestKeyGeneration` and `attestSignedAtMs` count); `logStatus` is
/// `"confirmed"`, `"absent"`, `"rollback"`, `"mismatch"`, `"unreachable"` or
/// `"off"`; `sealValid` / `announcementSeq` are `checkResetSeal`'s; `sealLog`
/// is `"proven"`, `"not-proven"` or `"unreachable"`.
///
/// # Errors
///
/// `resetSeal.unknownValue` for a name not listed.
#[wasm_bindgen(js_name = resetVerdict)]
#[allow(clippy::too_many_arguments)]
pub fn reset_verdict(
    held_key: &[u8],
    accepted_generation: u64,
    fetched_key: &[u8],
    bundle_generation: u64,
    attest_kind: &str,
    attest_key_generation: u64,
    attest_signed_at_ms: i64,
    log_status: &str,
    log_is_reset: Option<bool>,
    seal_valid: bool,
    announcement_seq: u64,
    seal_log: &str,
) -> Result<ResetVerdict, String> {
    let unknown = |what: &str, v: &str| err("unknownValue", format!("unknown {what} {v:?}"));
    let attest = match attest_kind {
        "absent" => AttestCheck::Absent,
        "invalid" => AttestCheck::Invalid,
        "no-key" => AttestCheck::NoKey,
        "verified" => AttestCheck::Verified {
            key_generation: attest_key_generation,
            signed_at_ms: attest_signed_at_ms,
        },
        other => return Err(unknown("attestKind", other)),
    };
    let status = match log_status {
        "confirmed" => LogStatus::Confirmed,
        "absent" => LogStatus::Absent,
        "rollback" => LogStatus::Rollback,
        "mismatch" => LogStatus::Mismatch,
        "unreachable" => LogStatus::Unreachable,
        "off" => LogStatus::Off,
        other => return Err(unknown("logStatus", other)),
    };
    let seal_log = match seal_log {
        "proven" => SealLog::Proven,
        "not-proven" => SealLog::NotProven,
        "unreachable" => SealLog::Unreachable,
        other => return Err(unknown("sealLog", other)),
    };
    Ok(ResetVerdict {
        inner: rust_verdict(&ResetVerdictInput {
            held_key,
            accepted_generation,
            fetched_key,
            bundle_generation,
            attest,
            log: LogCheck {
                status,
                is_reset: log_is_reset,
            },
            seal_signature: if seal_valid {
                SealSignature::Valid { announcement_seq }
            } else {
                SealSignature::Invalid
            },
            seal_log,
        }),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const ACCOUNT: [u8; 16] = [0x0A; 16];

    fn pk(seed: u8) -> Vec<u8> {
        SigningKey::from_seed(&[seed; 32])
            .verifying_key()
            .to_bytes()
            .to_vec()
    }

    #[test]
    fn a_sealed_reset_over_the_wasm_shape() {
        let password = ResetSealKey::from_password_export_key(&[0x10; 64]).unwrap();
        let passkey = ResetSealKey::from_passkey_prf(&[0x20; 32]).unwrap();
        let kinds = vec![password.kind(), passkey.kind()];
        let tags = [
            password.announcement_tag(None).unwrap(),
            passkey.announcement_tag(Some(b"cred".to_vec())).unwrap(),
        ]
        .concat();
        let pks = [password.public_key(), passkey.public_key()].concat();
        let old = SigningKey::from_seed(&[1; 32]);
        let announcement = RustAnn::new(
            lakko_attest::key_reset_account_ref(&ACCOUNT),
            1,
            5,
            9,
            entries(&kinds, &tags, &pks).unwrap(),
        )
        .sign(&old)
        .unwrap();
        let opened = open_seal_announcement(&announcement, &pk(1)).unwrap();
        assert_eq!(opened.seq(), 5);
        assert_eq!(opened.kinds(), vec!["password", "passkey"]);
        let seal = passkey.sign_reset_seal(&ACCOUNT, 5, 1, &pk(2)).unwrap();
        let check = check_reset_seal(
            Some(announcement),
            Some(seal),
            &pk(1),
            &ACCOUNT,
            1,
            2,
            &pk(2),
            0,
        )
        .unwrap();
        assert!(check.valid());
        assert_eq!(check.kind().as_deref(), Some("passkey"));
        let verdict = reset_verdict(
            &pk(1),
            1,
            &pk(2),
            2,
            "verified",
            2,
            3,
            "off",
            None,
            check.valid(),
            check.announcement_seq(),
            "not-proven",
        )
        .unwrap();
        assert_eq!(verdict.kind(), "sealed");
        assert_eq!(verdict.announcement_seq(), Some(5));
        let verdict = reset_verdict(
            &pk(1),
            1,
            &pk(2),
            2,
            "verified",
            2,
            3,
            "off",
            None,
            false,
            0,
            "proven",
        )
        .unwrap();
        assert_eq!(verdict.kind(), "unsealed");
        assert!(reset_verdict(
            &pk(1),
            1,
            &pk(2),
            2,
            "x",
            2,
            3,
            "off",
            None,
            false,
            0,
            "proven"
        )
        .is_err());
        assert!(password.announcement_tag(Some(vec![1])).is_err());
        assert_eq!(reset_seal_passkey_prf_salt().len(), 32);
    }
}
