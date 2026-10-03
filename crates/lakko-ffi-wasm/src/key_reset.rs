// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for `lakko-attest`'s key-reset attestation (ADR-0065
//! S5).
//!
//! Mirror of `crates/lakko-ffi-uniffi/src/key_reset.rs` for the web client.
//! **Verify only**: `services/auth` signs the attestation inside
//! `POST /account/keys/reset/finish`, and a client verifies the one it is
//! handed before carrying it to discovery (the identity upload) and to the
//! Delivery Service (the DeviceGroup journal reset).
//!
//! The client supplies its own 16-byte account id; the two per-account values
//! the attestation names, `account_ref` and the DeviceGroup journal id, are
//! derived here so no client re-implements either hash; both derivations are
//! also exposed (`keyResetAccountRef`, `deviceGroupJournalId`).
//!
//! **Error posture (the backup-head convention).** Every refusal throws a
//! `JsError` whose message starts with a stable code: `stale: ...` when the
//! attestation verified but `signed_at` is outside the window the fresh entry
//! point was given (ADR-0065 section 4.2 turns that into a sign-in prompt,
//! not a refusal), `invalid: ...` for every hard fail-closed cause.

use lakko_attest::KeyResetAttestError;
use lakko_crypto::signing::VerifyingKey;
use wasm_bindgen::prelude::*;

const STALE: &str = "stale";
const INVALID: &str = "invalid";

fn coded(code: &str, msg: &str) -> String {
    format!("{code}: {msg}")
}

fn map_err(e: KeyResetAttestError) -> String {
    match e {
        KeyResetAttestError::Stale => coded(STALE, &e.to_string()),
        other => coded(INVALID, &other.to_string()),
    }
}

/// The authenticated claims a verified key-reset attestation carries.
/// Returned by `verifyKeyResetAttestation` / `verifyKeyResetAttestationFresh`.
#[wasm_bindgen]
pub struct VerifiedKeyResetAttestation {
    inner: lakko_attest::KeyResetAttestation,
}

#[wasm_bindgen]
impl VerifiedKeyResetAttestation {
    /// The key generation the reset moved the account to (BigInt on the JS
    /// side).
    #[wasm_bindgen(getter, js_name = keyGeneration)]
    pub fn key_generation(&self) -> u64 {
        self.inner.key_generation
    }

    /// The mint timestamp, Unix milliseconds (BigInt on the JS side).
    #[wasm_bindgen(getter, js_name = signedAtMillis)]
    pub fn signed_at_millis(&self) -> i64 {
        self.inner.signed_at_unix_millis
    }
}

fn parse_inputs(
    attest_verifying_key: &[u8],
    expected_account_id: &[u8],
    expected_pubkey: &[u8],
) -> Result<(VerifyingKey, [u8; 16], [u8; 32]), String> {
    let verifying = VerifyingKey::from_bytes(attest_verifying_key)
        .map_err(|_| coded(INVALID, "attest verifying key is not a valid Ed25519 key"))?;
    let account: [u8; 16] = expected_account_id
        .try_into()
        .map_err(|_| coded(INVALID, "expected_account_id must be 16 bytes"))?;
    let pubkey: [u8; 32] = expected_pubkey
        .try_into()
        .map_err(|_| coded(INVALID, "expected_pubkey must be 32 bytes"))?;
    Ok((verifying, account, pubkey))
}

/// The `JsError`-free core of `verifyKeyResetAttestation`.
fn verify_inner(
    attestation: &[u8],
    attest_verifying_key: &[u8],
    expected_account_id: &[u8],
    expected_pubkey: &[u8],
) -> Result<lakko_attest::KeyResetAttestation, String> {
    let (verifying, account, pubkey) =
        parse_inputs(attest_verifying_key, expected_account_id, expected_pubkey)?;
    lakko_attest::verify_key_reset_binding(
        attestation,
        &verifying,
        &lakko_attest::key_reset_account_ref(&account),
        &lakko_attest::device_group_journal_id(&account),
        &pubkey,
    )
    .map_err(map_err)
}

/// The `JsError`-free core of `verifyKeyResetAttestationFresh`.
fn verify_fresh_inner(
    attestation: &[u8],
    attest_verifying_key: &[u8],
    expected_account_id: &[u8],
    expected_pubkey: &[u8],
    now_millis: i64,
    max_age_millis: i64,
) -> Result<lakko_attest::KeyResetAttestation, String> {
    let (verifying, account, pubkey) =
        parse_inputs(attest_verifying_key, expected_account_id, expected_pubkey)?;
    lakko_attest::verify_key_reset_fresh(
        attestation,
        &verifying,
        &lakko_attest::key_reset_account_ref(&account),
        &lakko_attest::device_group_journal_id(&account),
        &pubkey,
        now_millis,
        max_age_millis,
    )
    .map_err(map_err)
}

/// Verify a key-reset attestation's signature and its binding to
/// `expectedAccountId` (through the derived `account_ref` and DeviceGroup
/// journal id) and to `expectedPubkey`, with **no** freshness check (ADR-0065
/// section 2.2). Returns the authenticated generation and mint time.
///
/// `attestation` is the raw 177-byte blob (`key_reset_attest_b64` decoded);
/// `attestVerifyingKey` the 32-byte client-pinned auth attest key.
///
/// Throws an `invalid: ...` `JsError` on a bad verifying key, a wrong-length
/// account id or pubkey, a malformed / wrong-length / wrong-version blob, a
/// signature that does not verify, or a binding mismatch. Never `stale:`.
#[wasm_bindgen(js_name = verifyKeyResetAttestation)]
pub fn verify_key_reset_attestation(
    attestation: &[u8],
    attest_verifying_key: &[u8],
    expected_account_id: &[u8],
    expected_pubkey: &[u8],
) -> Result<VerifiedKeyResetAttestation, JsError> {
    verify_inner(
        attestation,
        attest_verifying_key,
        expected_account_id,
        expected_pubkey,
    )
    .map(|inner| VerifiedKeyResetAttestation { inner })
    .map_err(|msg| JsError::new(&msg))
}

/// `verifyKeyResetAttestation` plus a freshness bound: `signed_at` must lie
/// within `maxAgeMillis` of `nowMillis` (both BigInt, Unix milliseconds), and
/// no further in the future than a five-minute skew. The entry point for a
/// client applying its own reset from persisted `publish-owed` state
/// (ADR-0065 section 4.2, 30 days chosen there).
///
/// Throws `stale: ...` when the attestation verified but is outside the
/// window, otherwise as `verifyKeyResetAttestation`.
#[wasm_bindgen(js_name = verifyKeyResetAttestationFresh)]
pub fn verify_key_reset_attestation_fresh(
    attestation: &[u8],
    attest_verifying_key: &[u8],
    expected_account_id: &[u8],
    expected_pubkey: &[u8],
    now_millis: i64,
    max_age_millis: i64,
) -> Result<VerifiedKeyResetAttestation, JsError> {
    verify_fresh_inner(
        attestation,
        attest_verifying_key,
        expected_account_id,
        expected_pubkey,
        now_millis,
        max_age_millis,
    )
    .map(|inner| VerifiedKeyResetAttestation { inner })
    .map_err(|msg| JsError::new(&msg))
}

/// `BLAKE3-derive-key("lakko/key-reset/account-ref/v1", accountId)`: the
/// one-way per-account value a key-reset attestation names instead of the
/// account id. Returns the 32-byte value; throws `invalid: ...` when
/// `accountId` is not 16 bytes.
#[wasm_bindgen(js_name = keyResetAccountRef)]
pub fn key_reset_account_ref(account_id: &[u8]) -> Result<Vec<u8>, JsError> {
    let account: [u8; 16] = account_id
        .try_into()
        .map_err(|_| JsError::new(&coded(INVALID, "account_id must be 16 bytes")))?;
    Ok(lakko_attest::key_reset_account_ref(&account).to_vec())
}

/// `BLAKE3("lakko-device-group/v1" || accountId)`: the account's DeviceGroup
/// journal id, byte-identical to `DeviceGroup.groupIdFor`. Returns the
/// 32-byte id; throws `invalid: ...` when `accountId` is not 16 bytes.
#[wasm_bindgen(js_name = deviceGroupJournalId)]
pub fn device_group_journal_id(account_id: &[u8]) -> Result<Vec<u8>, JsError> {
    let account: [u8; 16] = account_id
        .try_into()
        .map_err(|_| JsError::new(&coded(INVALID, "account_id must be 16 bytes")))?;
    Ok(lakko_attest::device_group_journal_id(&account).to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::signing::SigningKey;

    const ACCOUNT: [u8; 16] = [0x42; 16];
    const PUBKEY: [u8; 32] = [0x77; 32];

    fn signed(signing: &SigningKey, generation: u64, signed_at: i64) -> Vec<u8> {
        lakko_attest::KeyResetAttestation::for_account(&ACCOUNT, generation, PUBKEY, signed_at)
            .sign(signing)
            .to_vec()
    }

    // The `JsError`-free cores run on the host (`JsError::new` panics on a
    // non-wasm target).
    #[test]
    fn verify_roundtrips_and_refuses_with_the_invalid_code() {
        let signing = SigningKey::from_seed(&[0x11; 32]);
        let vk = signing.verifying_key().to_bytes();
        let wire = signed(&signing, 3, 1_000_000);
        let got = verify_inner(&wire, &vk, &ACCOUNT, &PUBKEY).expect("verifies");
        assert_eq!(got.key_generation, 3);
        assert_eq!(got.signed_at_unix_millis, 1_000_000);
        for (name, err) in [
            (
                "other account",
                verify_inner(&wire, &vk, &[1u8; 16], &PUBKEY).unwrap_err(),
            ),
            (
                "other pubkey",
                verify_inner(&wire, &vk, &ACCOUNT, &[0u8; 32]).unwrap_err(),
            ),
            (
                "other signer",
                verify_inner(
                    &wire,
                    &SigningKey::from_seed(&[0x22; 32])
                        .verifying_key()
                        .to_bytes(),
                    &ACCOUNT,
                    &PUBKEY,
                )
                .unwrap_err(),
            ),
            (
                "malformed",
                verify_inner(&[0u8; 10], &vk, &ACCOUNT, &PUBKEY).unwrap_err(),
            ),
            (
                "short account id",
                verify_inner(&wire, &vk, &[1u8; 15], &PUBKEY).unwrap_err(),
            ),
        ] {
            assert!(err.starts_with("invalid: "), "{name}: {err}");
        }
    }

    #[test]
    fn stale_has_its_own_code_only_on_the_fresh_entry_point() {
        let signing = SigningKey::from_seed(&[0x11; 32]);
        let vk = signing.verifying_key().to_bytes();
        let wire = signed(&signing, 2, 1_000_000);
        assert!(verify_fresh_inner(&wire, &vk, &ACCOUNT, &PUBKEY, 1_500_000, 500_000).is_ok());
        let err =
            verify_fresh_inner(&wire, &vk, &ACCOUNT, &PUBKEY, 1_500_001, 500_000).unwrap_err();
        assert!(err.starts_with("stale: "), "{err}");
        assert!(verify_inner(&wire, &vk, &ACCOUNT, &PUBKEY).is_ok());
    }

    #[test]
    fn derivations_match_the_crate() {
        assert_eq!(
            lakko_attest::device_group_journal_id(&ACCOUNT).to_vec(),
            lakko_mls::DeviceGroup::group_id_for(&ACCOUNT)
        );
        assert_ne!(
            lakko_attest::key_reset_account_ref(&ACCOUNT),
            lakko_attest::device_group_journal_id(&ACCOUNT)
        );
    }
}
