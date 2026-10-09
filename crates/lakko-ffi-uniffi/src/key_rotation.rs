// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for the key rotation that keeps the other devices
//! (ADR-0066 part 3, Stage A; sprint T5).
//!
//! Mirror of `crates/lakko-ffi-wasm/src/key_rotation.rs` for iOS + Android.
//! Three things a client needs and must not re-derive:
//!
//! - the **key-rotation envelope** ([`lakko_contacts::key_rotation`]): the
//!   rotating device composes it and posts it in the account's DeviceGroup;
//!   a kept device decodes and CHECKS it with [`verify_key_rotation_envelope`]
//!   before it stores it as pending;
//! - the **root endorsement** ([`lakko_attest::sign_root_endorsement`]) the
//!   Delivery Service requires beside a key-reset attestation;
//! - the envelope's length and pending-age rule, exported rather than
//!   hand-copied.
//!
//! The supersession proof itself is signed with the existing
//! `signSupersessionProof(oldEd25519Seed, newEd25519Seed, issuedAt,
//! expiresAt)`, and the kept device's session upgrade with
//! `signSessionUpgradeStatement` (`crate::account_statement`).
//!
//! # The order the rotating device owes (ADR-0066 section 3.2)
//!
//! [`compose_key_rotation_envelope`] produces bytes that hand the new account
//! to whoever can read them. They are sent ONLY in the account's DeviceGroup,
//! and ONLY after the device being removed is out of it and the commit that
//! removed it has won the journal, so that the message is sealed at an epoch
//! that device never held. Nothing in this module can check either.

use lakko_attest::ROOT_ENDORSEMENT_LEN;
use lakko_contacts::key_rotation::{
    compose_key_rotation as rust_compose, decode_key_rotation as rust_decode, KeyRotation,
    KeyRotationError, KEY_ROTATION_ENTROPY_LEN, KEY_ROTATION_ENVELOPE_LEN,
    KEY_ROTATION_PENDING_MAX_AGE_MS, KEY_ROTATION_PROOF_LEN,
};
use lakko_crypto::signing::SigningKey;
use lakko_mnemonic::Mnemonic;

use crate::FfiError;

// The envelope crate repeats the proof's length because it does not depend
// on the transparency crate; this crate depends on both.
const _: () = assert!(KEY_ROTATION_PROOF_LEN == lakko_transparency::SUPERSESSION_PROOF_LEN);
const _: () = assert!(KEY_ROTATION_ENTROPY_LEN == lakko_mnemonic::ENTROPY_LEN);

fn refused(code: &str, reason: &str) -> FfiError {
    FfiError::KeyRotation(format!("{code}: {reason}"))
}

fn wire_error(e: KeyRotationError) -> FfiError {
    FfiError::KeyRotation(format!("{}: {e}", e.code()))
}

/// A decoded key-rotation envelope. **`new_entropy` is the new account**:
/// never log it, store it only under the device key, and adopt it only after
/// [`verify_key_rotation_envelope`] accepted the envelope AND the server-side
/// change is visible (ADR-0066 section 3.3).
#[derive(uniffi::Record, Clone, PartialEq, Eq)]
pub struct FfiKeyRotation {
    /// Rotating-device clock, UNIX milliseconds, when it was prepared. Ages
    /// a pending envelope out; orders and authorises nothing.
    pub prepared_at_ms: u64,
    /// The 16 bytes of BIP39 entropy of the NEW recovery phrase
    /// (`RecoveryMnemonic.fromEntropy`).
    pub new_entropy: Vec<u8>,
    /// The 145-byte supersession proof, old account key over new.
    pub supersession_proof: Vec<u8>,
}

impl std::fmt::Debug for FfiKeyRotation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfiKeyRotation")
            .field("prepared_at_ms", &self.prepared_at_ms)
            .field("new_entropy", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl From<KeyRotation> for FfiKeyRotation {
    fn from(r: KeyRotation) -> Self {
        Self {
            prepared_at_ms: r.prepared_at_ms,
            new_entropy: r.new_entropy.to_vec(),
            supersession_proof: r.supersession_proof.to_vec(),
        }
    }
}

/// What [`verify_key_rotation_envelope`] established about an envelope.
#[derive(uniffi::Record, Clone, PartialEq, Eq)]
pub struct FfiVerifiedKeyRotation {
    /// As in [`FfiKeyRotation`].
    pub prepared_at_ms: u64,
    /// As in [`FfiKeyRotation`]. Checked: the account identity it derives
    /// has exactly `new_signature_pubkey`.
    pub new_entropy: Vec<u8>,
    /// The account identity public key the proof authorises and the entropy
    /// derives. A pending envelope is keyed on this: the device adopts the
    /// one whose key the published bundle (or the server) shows as the
    /// account's.
    pub new_signature_pubkey: Vec<u8>,
    /// The proof, verified under the account key the caller holds. The
    /// device re-publishes it with its own bundle only if it is the rotating
    /// device; a kept device keeps it for its peers' checks.
    pub supersession_proof: Vec<u8>,
    /// When the proof stops being accepted, UNIX seconds.
    pub proof_expires_at_unix_secs: i64,
}

impl std::fmt::Debug for FfiVerifiedKeyRotation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfiVerifiedKeyRotation")
            .field("prepared_at_ms", &self.prepared_at_ms)
            .field("new_entropy", &"<redacted>")
            .field("new_signature_pubkey", &self.new_signature_pubkey)
            .finish_non_exhaustive()
    }
}

/// Compose a key-rotation envelope: the plaintext of ONE MLS application
/// message in the account's DeviceGroup (inside the `ServiceEnvelope`
/// carrier). `new_entropy` is `RecoveryMnemonic.toEntropy()` of the new
/// phrase; `supersession_proof` is `signSupersessionProof(old, new, ...)`.
///
/// **Send it only after the removed device is out of the DeviceGroup and the
/// epoch has advanced** (the module docs).
///
/// # Errors
///
/// [`FfiError::KeyRotation`], message `"<code>: <reason>"`, codes
/// `bad-entropy-length` and `bad-proof-length`.
#[uniffi::export]
pub fn compose_key_rotation_envelope(
    prepared_at_ms: u64,
    new_entropy: Vec<u8>,
    supersession_proof: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let new_entropy: [u8; KEY_ROTATION_ENTROPY_LEN] = new_entropy
        .as_slice()
        .try_into()
        .map_err(|_| refused("bad-entropy-length", "the new entropy must be 16 bytes"))?;
    let supersession_proof: [u8; KEY_ROTATION_PROOF_LEN] =
        supersession_proof.as_slice().try_into().map_err(|_| {
            refused(
                "bad-proof-length",
                "the supersession proof must be 145 bytes",
            )
        })?;
    Ok(rust_compose(&KeyRotation {
        prepared_at_ms,
        new_entropy,
        supersession_proof,
    }))
}

/// Strictly decode a key-rotation envelope. Shape only: this proves nothing
/// about who made it. Use [`verify_key_rotation_envelope`] before keeping
/// one.
///
/// # Errors
///
/// [`FfiError::KeyRotation`] with the envelope's code (`wrongDomain`,
/// `truncated`, `unknownVersion`, `badLength`).
#[uniffi::export]
pub fn decode_key_rotation_envelope(plaintext: Vec<u8>) -> Result<FfiKeyRotation, FfiError> {
    rust_decode(&plaintext).map(Into::into).map_err(wire_error)
}

/// The three checks a kept device makes before it stores an envelope as
/// pending (ADR-0066 section 3.3), in one call so no client re-derives them:
///
/// 1. the supersession proof is signed by `held_account_pubkey` (the account
///    identity key THIS device holds), has a validity window the format
///    allows, and has not expired at `now_unix_secs`;
/// 2. the account identity derived from `new_entropy` (the phrase, its
///    64-byte seed, `Identity.fromSeed`) has exactly the key the proof
///    authorises;
/// 3. the new key is not the held one.
///
/// On success the device stores the result as PENDING, keyed on
/// `new_signature_pubkey`, and changes nothing. It adopts when it sees the
/// server-side commit: the published bundle carries that key, or its own
/// refresh is answered `key-generation-superseded`.
///
/// # Errors
///
/// [`FfiError::KeyRotation`], message `"<code>: <reason>"`: the envelope's
/// own codes, `bad-key-length`, `proof-refused` (not signed by the held key,
/// a bad window, expired), `entropy-mismatch` (the entropy does not derive
/// the authorised key), `same-key`.
#[uniffi::export]
pub fn verify_key_rotation_envelope(
    plaintext: Vec<u8>,
    held_account_pubkey: Vec<u8>,
    now_unix_secs: i64,
) -> Result<FfiVerifiedKeyRotation, FfiError> {
    let held: [u8; 32] = held_account_pubkey
        .as_slice()
        .try_into()
        .map_err(|_| refused("bad-key-length", "the held account key must be 32 bytes"))?;
    let rotation = rust_decode(&plaintext).map_err(wire_error)?;
    let authorised =
        lakko_transparency::verify_supersession(&rotation.supersession_proof, &held, now_unix_secs)
            .map_err(|e| refused("proof-refused", &e.to_string()))?;
    if authorised == held {
        return Err(refused("same-key", "the proof names the key already held"));
    }
    let mnemonic = Mnemonic::from_entropy(&rotation.new_entropy)
        .map_err(|e| refused("entropy-mismatch", &e.to_string()))?;
    let identity = lakko_mls::Identity::from_seed(mnemonic.to_seed().as_ref())
        .map_err(|_| refused("entropy-mismatch", "the entropy derives no identity"))?;
    if identity.signature_public_key() != authorised.as_slice() {
        return Err(refused(
            "entropy-mismatch",
            "the entropy does not derive the key the proof authorises",
        ));
    }
    let expires = lakko_transparency::SupersessionProof::open(&rotation.supersession_proof)
        .map_err(|e| refused("proof-refused", &e.to_string()))?
        .expires_at_unix_secs;
    Ok(FfiVerifiedKeyRotation {
        prepared_at_ms: rotation.prepared_at_ms,
        new_entropy: rotation.new_entropy.to_vec(),
        new_signature_pubkey: authorised.to_vec(),
        supersession_proof: rotation.supersession_proof.to_vec(),
        proof_expires_at_unix_secs: expires,
    })
}

/// The one length of a key-rotation envelope (192 bytes).
#[uniffi::export]
#[must_use]
pub fn key_rotation_envelope_len() -> u32 {
    u32::try_from(KEY_ROTATION_ENVELOPE_LEN).unwrap_or(u32::MAX)
}

/// How long a pending envelope is kept before it is dropped, in
/// milliseconds (14 days).
#[uniffi::export]
#[must_use]
pub const fn key_rotation_pending_max_age_ms() -> u64 {
    KEY_ROTATION_PENDING_MAX_AGE_MS
}

/// `true` when a pending envelope prepared at `prepared_at_ms` is too old to
/// keep at `now_ms`. A stamp in the future is not expired.
#[uniffi::export]
#[must_use]
pub const fn key_rotation_pending_expired(prepared_at_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(prepared_at_ms) > KEY_ROTATION_PENDING_MAX_AGE_MS
}

/// Sign the **root endorsement** the Delivery Service requires beside a
/// key-reset attestation (ADR-0066 section 4.6): the account's NEW identity
/// key says which key becomes the root of the account's DeviceGroup journal.
/// Sent as `root_endorsement_b64` (standard base64) with
/// `key_reset_attest_b64` on `PUT /ds/group/{hex}/reset` (after a reset or a
/// recall) and on `PUT /ds/group/{hex}/root` (after a rotation).
///
/// - `new_account_ed25519_seed`: the NEW account identity's 32-byte Ed25519
///   seed (`Identity.ed25519Seed()`), the key the attestation names.
/// - `journal_id`: the DeviceGroup journal id (`deviceGroupJournalId`).
/// - `key_generation`: the generation the attestation carries.
/// - `root_key`: the key that becomes the root: the signer of a `reset`
///   statement (`DsStatementSigner.publicKey()` of the NEW root), the
///   `new_root` of a `root` transfer.
///
/// # Errors
///
/// [`FfiError::KeyRotation`] with codes `bad-seed-length`,
/// `bad-journal-id-length`, `bad-key-length`.
#[uniffi::export]
pub fn sign_ds_root_endorsement(
    new_account_ed25519_seed: Vec<u8>,
    journal_id: Vec<u8>,
    key_generation: u64,
    root_key: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let seed: [u8; 32] = new_account_ed25519_seed
        .as_slice()
        .try_into()
        .map_err(|_| refused("bad-seed-length", "the account seed must be 32 bytes"))?;
    let journal: [u8; 32] = journal_id
        .as_slice()
        .try_into()
        .map_err(|_| refused("bad-journal-id-length", "the journal id must be 32 bytes"))?;
    let root: [u8; 32] = root_key
        .as_slice()
        .try_into()
        .map_err(|_| refused("bad-key-length", "the root key must be 32 bytes"))?;
    Ok(lakko_attest::sign_root_endorsement(
        &SigningKey::from_seed(&seed),
        &journal,
        key_generation,
        &root,
    )
    .to_vec())
}

/// The length of a root endorsement (64 bytes).
#[uniffi::export]
#[must_use]
pub fn ds_root_endorsement_len() -> u32 {
    u32::try_from(ROOT_ENDORSEMENT_LEN).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_attest::{verify_root_endorsement, KeyResetAttestation};

    const NOW: i64 = 1_800_000_000;

    fn code(e: FfiError) -> String {
        match e {
            FfiError::KeyRotation(m) => m.split(':').next().unwrap_or("").to_string(),
            other => panic!("expected KeyRotation, got {other:?}"),
        }
    }

    /// An account identity as a client derives it: entropy -> phrase ->
    /// 64-byte seed -> `Identity::from_seed`.
    fn account(entropy: [u8; 16]) -> (SigningKey, [u8; 32]) {
        let seed = Mnemonic::from_entropy(&entropy).unwrap().to_seed();
        let identity = lakko_mls::Identity::from_seed(seed.as_ref()).unwrap();
        let signing = SigningKey::from_seed(&identity.ed25519_seed_bytes());
        let public = signing.verifying_key().to_bytes();
        assert_eq!(identity.signature_public_key(), public.as_slice());
        (signing, public)
    }

    fn proof(old: &SigningKey, new: &[u8; 32], issued: i64, expires: i64) -> Vec<u8> {
        lakko_transparency::SupersessionProof::new(
            old.verifying_key().to_bytes(),
            *new,
            issued,
            expires,
        )
        .sign(old)
        .to_vec()
    }

    #[test]
    fn a_rotation_envelope_composes_decodes_and_verifies() {
        let (old, old_pub) = account([1; 16]);
        let new_entropy = [2u8; 16];
        let (_, new_pub) = account(new_entropy);
        let p = proof(&old, &new_pub, NOW, NOW + 14 * 86_400);
        let wire =
            compose_key_rotation_envelope(1_800_000_000_000, new_entropy.to_vec(), p.clone())
                .unwrap();
        assert_eq!(wire.len(), key_rotation_envelope_len() as usize);
        assert_eq!(wire.len(), 192);

        let decoded = decode_key_rotation_envelope(wire.clone()).unwrap();
        assert_eq!(decoded.prepared_at_ms, 1_800_000_000_000);
        assert_eq!(decoded.new_entropy, new_entropy);
        assert_eq!(decoded.supersession_proof, p);
        assert!(!format!("{decoded:?}").contains("2, 2"));

        let verified =
            verify_key_rotation_envelope(wire.clone(), old_pub.to_vec(), NOW + 60).unwrap();
        assert_eq!(verified.new_signature_pubkey, new_pub);
        assert_eq!(verified.new_entropy, new_entropy);
        assert_eq!(verified.proof_expires_at_unix_secs, NOW + 14 * 86_400);

        // The dispatcher classifies it as its own kind and hands the same
        // fields over.
        let dispatched = crate::contacts::dispatch_incoming_envelope(wire).unwrap();
        assert_eq!(
            dispatched.kind,
            crate::contacts::FfiIncomingEnvelopeKind::KeyRotation
        );
        assert!(dispatched.payload.is_empty());
        assert_eq!(dispatched.key_rotation, Some(decoded));
    }

    #[test]
    fn a_kept_device_refuses_every_envelope_that_is_not_its_own_accounts_rotation() {
        let (old, old_pub) = account([1; 16]);
        let (stranger, stranger_pub) = account([3; 16]);
        let new_entropy = [2u8; 16];
        let (_, new_pub) = account(new_entropy);
        let envelope = |entropy: [u8; 16], proof: Vec<u8>| {
            compose_key_rotation_envelope(1, entropy.to_vec(), proof).unwrap()
        };
        let good = proof(&old, &new_pub, NOW, NOW + 86_400);

        // A proof by another key (the device holds `old`).
        let by_stranger = proof(&stranger, &new_pub, NOW, NOW + 86_400);
        assert_eq!(
            code(
                verify_key_rotation_envelope(
                    envelope(new_entropy, by_stranger),
                    old_pub.to_vec(),
                    NOW
                )
                .unwrap_err()
            ),
            "proof-refused"
        );
        // The held key named as the old key, signed by someone else.
        let forged = lakko_transparency::SupersessionProof::new(old_pub, new_pub, NOW, NOW + 60)
            .sign(&stranger)
            .to_vec();
        assert_eq!(
            code(
                verify_key_rotation_envelope(envelope(new_entropy, forged), old_pub.to_vec(), NOW)
                    .unwrap_err()
            ),
            "proof-refused"
        );
        // Expired.
        assert_eq!(
            code(
                verify_key_rotation_envelope(
                    envelope(new_entropy, good.clone()),
                    old_pub.to_vec(),
                    NOW + 86_401
                )
                .unwrap_err()
            ),
            "proof-refused"
        );
        // A real proof, with entropy that derives ANOTHER key: someone
        // trying to plant a seed under an honest proof.
        assert_eq!(
            code(
                verify_key_rotation_envelope(
                    envelope([9; 16], good.clone()),
                    old_pub.to_vec(),
                    NOW
                )
                .unwrap_err()
            ),
            "entropy-mismatch"
        );
        // A proof for the stranger's key with the stranger's entropy, by the
        // held key: that one IS a rotation of this account (the held key
        // signed it). It verifies.
        let to_stranger = proof(&old, &stranger_pub, NOW, NOW + 60);
        assert!(verify_key_rotation_envelope(
            envelope([3; 16], to_stranger),
            old_pub.to_vec(),
            NOW
        )
        .is_ok());
        // A proof of the held key over itself.
        let same = proof(&old, &old_pub, NOW, NOW + 60);
        assert_eq!(
            code(
                verify_key_rotation_envelope(envelope([1; 16], same), old_pub.to_vec(), NOW)
                    .unwrap_err()
            ),
            "same-key"
        );
        // Shapes.
        assert_eq!(
            code(
                verify_key_rotation_envelope(envelope(new_entropy, good.clone()), vec![0; 31], NOW)
                    .unwrap_err()
            ),
            "bad-key-length"
        );
        assert_eq!(
            code(
                verify_key_rotation_envelope(b"hello".to_vec(), old_pub.to_vec(), NOW).unwrap_err()
            ),
            "wrongDomain"
        );
        assert_eq!(
            code(
                decode_key_rotation_envelope(envelope(new_entropy, good)[..100].to_vec())
                    .unwrap_err()
            ),
            "badLength"
        );
        assert_eq!(
            code(compose_key_rotation_envelope(1, vec![0; 15], vec![0; 145]).unwrap_err()),
            "bad-entropy-length"
        );
        assert_eq!(
            code(compose_key_rotation_envelope(1, vec![0; 16], vec![0; 144]).unwrap_err()),
            "bad-proof-length"
        );
    }

    #[test]
    fn the_pending_age_rule_matches_the_core() {
        let max = key_rotation_pending_max_age_ms();
        assert_eq!(max, 14 * 24 * 60 * 60 * 1000);
        assert!(!key_rotation_pending_expired(1_000, 1_000 + max));
        assert!(key_rotation_pending_expired(1_000, 1_001 + max));
        assert!(!key_rotation_pending_expired(5_000, 1_000));
    }

    #[test]
    fn a_root_endorsement_verifies_against_the_attestation_it_was_made_for() {
        let account_id = [0xA1u8; 16];
        let identity = SigningKey::from_seed(&[0x44; 32]);
        let attest_key = SigningKey::from_seed(&[0x45; 32]);
        let wire = KeyResetAttestation::for_account(
            &account_id,
            3,
            identity.verifying_key().to_bytes(),
            1_000,
        )
        .sign(&attest_key);
        let attestation = KeyResetAttestation::open(&wire, &attest_key.verifying_key()).unwrap();
        let root = [0x52u8; 32];
        let journal = crate::key_reset::device_group_journal_id(account_id.to_vec()).unwrap();
        let sig =
            sign_ds_root_endorsement(vec![0x44; 32], journal.clone(), 3, root.to_vec()).unwrap();
        assert_eq!(sig.len(), ds_root_endorsement_len() as usize);
        verify_root_endorsement(&attestation, &root, &sig).unwrap();
        // Another generation or another root does not verify.
        let other =
            sign_ds_root_endorsement(vec![0x44; 32], journal.clone(), 4, root.to_vec()).unwrap();
        assert!(verify_root_endorsement(&attestation, &root, &other).is_err());
        assert!(verify_root_endorsement(&attestation, &[0x53; 32], &sig).is_err());

        assert_eq!(
            code(
                sign_ds_root_endorsement(vec![0; 31], journal.clone(), 3, root.to_vec())
                    .unwrap_err()
            ),
            "bad-seed-length"
        );
        assert_eq!(
            code(
                sign_ds_root_endorsement(vec![0x44; 32], vec![0; 31], 3, root.to_vec())
                    .unwrap_err()
            ),
            "bad-journal-id-length"
        );
        assert_eq!(
            code(sign_ds_root_endorsement(vec![0x44; 32], journal, 3, vec![0; 33]).unwrap_err()),
            "bad-key-length"
        );
    }
}
