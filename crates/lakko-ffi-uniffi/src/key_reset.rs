// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for `lakko-attest`'s key-reset attestation (ADR-0065 S5).
//!
//! Mirror of `crates/lakko-ffi-wasm/src/key_reset.rs` for iOS + Android.
//! **Verify only**: `services/auth` signs the attestation inside
//! `POST /account/keys/reset/finish`, and a client verifies the one it is
//! handed before carrying it to discovery (the identity upload) and to the
//! Delivery Service (the DeviceGroup journal reset), the same posture as the
//! backup-head and genesis FFIs (ADR-0036 / ADR-0037: signing is off-device).
//!
//! The client supplies its own 16-byte account id; the two per-account values
//! the attestation names, `account_ref` and the DeviceGroup journal id, are
//! derived here from it so no client re-implements either hash. The
//! derivations are also exposed on their own, [`key_reset_account_ref`] and
//! [`device_group_journal_id`], for a client that wants to compare them with
//! what a server answered.
//!
//! **Error posture (the backup-head split).** A stale attestation is a
//! distinct [`FfiError::KeyResetAttestationStale`]: ADR-0065 section 4.2 has a
//! client apply a weeks-old reset only after a fresh sign-in, which is a
//! prompt rather than a refusal, so the client must tell staleness from a
//! forgery. Every other failure is the hard fail-closed
//! [`FfiError::KeyResetAttestation`].

use lakko_attest::{
    device_group_journal_id as inner_device_group_journal_id,
    key_reset_account_ref as inner_key_reset_account_ref,
    verify_key_reset_binding as inner_verify_key_reset_binding,
    verify_key_reset_fresh as inner_verify_key_reset_fresh, KeyResetAttestError,
    KeyResetAttestation as InnerKeyResetAttestation,
};
use lakko_crypto::signing::VerifyingKey;

use crate::FfiError;

impl From<KeyResetAttestError> for FfiError {
    fn from(e: KeyResetAttestError) -> Self {
        match e {
            KeyResetAttestError::Stale => Self::KeyResetAttestationStale,
            other => Self::KeyResetAttestation(other.to_string()),
        }
    }
}

/// The authenticated claims a verified key-reset attestation carries — the
/// UniFFI projection of [`lakko_attest::KeyResetAttestation`] minus the two
/// derived per-account values the caller supplied the input for. `Debug` is
/// safe: every field is a public, signed commitment.
#[derive(uniffi::Record, Clone, Debug)]
pub struct VerifiedKeyResetAttestation {
    /// The key generation the reset moved the account to.
    pub key_generation: u64,
    /// The mint timestamp, Unix milliseconds.
    pub signed_at_millis: i64,
}

impl From<InnerKeyResetAttestation> for VerifiedKeyResetAttestation {
    fn from(a: InnerKeyResetAttestation) -> Self {
        Self {
            key_generation: a.key_generation,
            signed_at_millis: a.signed_at_unix_millis,
        }
    }
}

fn parse_inputs(
    attest_verifying_key: &[u8],
    expected_account_id: &[u8],
    expected_pubkey: &[u8],
) -> Result<(VerifyingKey, [u8; 16], [u8; 32]), FfiError> {
    let verifying = VerifyingKey::from_bytes(attest_verifying_key).map_err(|_| {
        FfiError::KeyResetAttestation("attest verifying key is not a valid Ed25519 key".into())
    })?;
    let account: [u8; 16] = expected_account_id.try_into().map_err(|_| {
        FfiError::KeyResetAttestation("expected_account_id must be 16 bytes".into())
    })?;
    let pubkey: [u8; 32] = expected_pubkey
        .try_into()
        .map_err(|_| FfiError::KeyResetAttestation("expected_pubkey must be 32 bytes".into()))?;
    Ok((verifying, account, pubkey))
}

/// Verify a key-reset attestation's signature and its binding to
/// `expected_account_id` (through the derived `account_ref` and DeviceGroup
/// journal id) and to `expected_pubkey`, with **no** freshness check
/// (ADR-0065 section 2.2). Returns the authenticated generation and mint
/// time.
///
/// `attestation` is the raw 177-byte blob (`key_reset_attest_b64` decoded).
/// `attest_verifying_key` is the 32-byte client-pinned auth attest key.
///
/// # Errors
///
/// [`FfiError::KeyResetAttestation`] — a bad verifying key / wrong-length
/// account id or pubkey, a malformed / wrong-length / wrong-version blob, a
/// signature that does not verify, or an `account_ref` / journal / pubkey
/// mismatch. Never [`FfiError::KeyResetAttestationStale`].
#[uniffi::export]
pub fn verify_key_reset_attestation(
    attestation: Vec<u8>,
    attest_verifying_key: Vec<u8>,
    expected_account_id: Vec<u8>,
    expected_pubkey: Vec<u8>,
) -> Result<VerifiedKeyResetAttestation, FfiError> {
    let (verifying, account, pubkey) = parse_inputs(
        &attest_verifying_key,
        &expected_account_id,
        &expected_pubkey,
    )?;
    let att = inner_verify_key_reset_binding(
        &attestation,
        &verifying,
        &inner_key_reset_account_ref(&account),
        &inner_device_group_journal_id(&account),
        &pubkey,
    )?;
    Ok(att.into())
}

/// [`verify_key_reset_attestation`] plus a freshness bound: `signed_at` must
/// lie within `max_age_millis` of `now_millis` (and no further in the future
/// than a five-minute skew). The entry point for a client applying its own
/// reset from persisted `publish-owed` state (ADR-0065 section 4.2, 30 days
/// chosen there).
///
/// # Errors
///
/// [`FfiError::KeyResetAttestationStale`] when the attestation verified but
/// is outside the window; otherwise as [`verify_key_reset_attestation`].
#[uniffi::export]
pub fn verify_key_reset_attestation_fresh(
    attestation: Vec<u8>,
    attest_verifying_key: Vec<u8>,
    expected_account_id: Vec<u8>,
    expected_pubkey: Vec<u8>,
    now_millis: i64,
    max_age_millis: i64,
) -> Result<VerifiedKeyResetAttestation, FfiError> {
    let (verifying, account, pubkey) = parse_inputs(
        &attest_verifying_key,
        &expected_account_id,
        &expected_pubkey,
    )?;
    let att = inner_verify_key_reset_fresh(
        &attestation,
        &verifying,
        &inner_key_reset_account_ref(&account),
        &inner_device_group_journal_id(&account),
        &pubkey,
        now_millis,
        max_age_millis,
    )?;
    Ok(att.into())
}

/// `BLAKE3-derive-key("lakko/key-reset/account-ref/v1", account_id)`: the
/// one-way per-account value a key-reset attestation names instead of the
/// account id. Returns the 32-byte value.
///
/// # Errors
///
/// [`FfiError::KeyResetAttestation`] when `account_id` is not 16 bytes.
#[uniffi::export]
pub fn key_reset_account_ref(account_id: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    let account: [u8; 16] = account_id
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::KeyResetAttestation("account_id must be 16 bytes".into()))?;
    Ok(inner_key_reset_account_ref(&account).to_vec())
}

/// `BLAKE3("lakko-device-group/v1" || account_id)`: the account's DeviceGroup
/// journal id, byte-identical to `DeviceGroup::group_id_for`. Returns the
/// 32-byte id.
///
/// # Errors
///
/// [`FfiError::KeyResetAttestation`] when `account_id` is not 16 bytes.
#[uniffi::export]
pub fn device_group_journal_id(account_id: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    let account: [u8; 16] = account_id
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::KeyResetAttestation("account_id must be 16 bytes".into()))?;
    Ok(inner_device_group_journal_id(&account).to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::signing::SigningKey;

    const ACCOUNT: [u8; 16] = [0x42; 16];
    const PUBKEY: [u8; 32] = [0x77; 32];

    fn signed(signing: &SigningKey, generation: u64, signed_at: i64) -> Vec<u8> {
        InnerKeyResetAttestation::for_account(&ACCOUNT, generation, PUBKEY, signed_at)
            .sign(signing)
            .to_vec()
    }

    #[test]
    fn verify_roundtrips_and_binds() {
        let signing = SigningKey::from_seed(&[0x11; 32]);
        let vk = signing.verifying_key().to_bytes().to_vec();
        let wire = signed(&signing, 3, 1_000_000);
        let got = verify_key_reset_attestation(
            wire.clone(),
            vk.clone(),
            ACCOUNT.to_vec(),
            PUBKEY.to_vec(),
        )
        .expect("verifies");
        assert_eq!(got.key_generation, 3);
        assert_eq!(got.signed_at_millis, 1_000_000);
        // Another account, another key, another signer: all refused hard.
        assert!(matches!(
            verify_key_reset_attestation(wire.clone(), vk.clone(), vec![1u8; 16], PUBKEY.to_vec()),
            Err(FfiError::KeyResetAttestation(_))
        ));
        assert!(matches!(
            verify_key_reset_attestation(wire.clone(), vk.clone(), ACCOUNT.to_vec(), vec![0u8; 32]),
            Err(FfiError::KeyResetAttestation(_))
        ));
        let other = SigningKey::from_seed(&[0x22; 32])
            .verifying_key()
            .to_bytes()
            .to_vec();
        assert!(matches!(
            verify_key_reset_attestation(wire.clone(), other, ACCOUNT.to_vec(), PUBKEY.to_vec()),
            Err(FfiError::KeyResetAttestation(_))
        ));
        assert!(matches!(
            verify_key_reset_attestation(
                vec![0u8; 10],
                vk.clone(),
                ACCOUNT.to_vec(),
                PUBKEY.to_vec()
            ),
            Err(FfiError::KeyResetAttestation(_))
        ));
        assert!(matches!(
            verify_key_reset_attestation(wire, vk, vec![1u8; 15], PUBKEY.to_vec()),
            Err(FfiError::KeyResetAttestation(_))
        ));
    }

    #[test]
    fn stale_is_a_distinct_variant_only_on_the_fresh_entry_point() {
        let signing = SigningKey::from_seed(&[0x11; 32]);
        let vk = signing.verifying_key().to_bytes().to_vec();
        let wire = signed(&signing, 2, 1_000_000);
        assert!(verify_key_reset_attestation_fresh(
            wire.clone(),
            vk.clone(),
            ACCOUNT.to_vec(),
            PUBKEY.to_vec(),
            1_500_000,
            500_000,
        )
        .is_ok());
        assert!(matches!(
            verify_key_reset_attestation_fresh(
                wire.clone(),
                vk.clone(),
                ACCOUNT.to_vec(),
                PUBKEY.to_vec(),
                1_500_001,
                500_000,
            ),
            Err(FfiError::KeyResetAttestationStale)
        ));
        // The binding entry point never reports staleness.
        assert!(verify_key_reset_attestation(wire, vk, ACCOUNT.to_vec(), PUBKEY.to_vec()).is_ok());
    }

    #[test]
    fn journal_id_matches_the_mls_derivation() {
        // The attest crate repeats `DeviceGroup::group_id_for` so auth and the
        // clients need no MLS crate for it; this crate links both, so pin
        // them equal here.
        for id in [[0u8; 16], [0xAA; 16], [0x42; 16]] {
            assert_eq!(
                device_group_journal_id(id.to_vec()).unwrap(),
                lakko_mls::DeviceGroup::group_id_for(&id)
            );
        }
        assert_ne!(
            key_reset_account_ref(ACCOUNT.to_vec()).unwrap(),
            device_group_journal_id(ACCOUNT.to_vec()).unwrap()
        );
        assert!(key_reset_account_ref(vec![0u8; 15]).is_err());
        assert!(device_group_journal_id(vec![0u8; 17]).is_err());
    }
}
