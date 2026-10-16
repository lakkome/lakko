// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for the key rotation that keeps the other devices
//! (ADR-0074 part 3, Stage A; sprint T5).
//!
//! Mirror of `crates/lakko-ffi-uniffi/src/key_rotation.rs` for the web
//! client. Two things a client needs and must not re-derive:
//!
//! - the **key-rotation envelope** ([`lakko_contacts::key_rotation`]): the
//!   rotating device composes it and posts it in the account's DeviceGroup;
//!   a kept device decodes and CHECKS it with `verifyKeyRotationEnvelope`
//!   before it stores it as pending;
//! - the envelope's length and pending-age rule, exported rather than
//!   hand-copied.
//!
//! The supersession proof itself is signed with the existing
//! `signSupersessionProof`, and the kept device's session upgrade with
//! `signSessionUpgradeStatement`.
//!
//! # The order the rotating device owes (ADR-0074 section 3.2)
//!
//! `composeKeyRotationEnvelope` produces bytes that hand the new account to
//! whoever can read them. They are sent ONLY in the account's DeviceGroup,
//! and ONLY after the device being removed is out of it and the commit that
//! removed it has won the journal, so that the message is sealed at an epoch
//! that device never held. Nothing in this module can check either.
//!
//! # Errors
//!
//! Every function throws an `Error` whose message is `"<code>: <reason>"`
//! with the stable codes `wrongDomain`, `truncated`, `unknownVersion`,
//! `badLength` (the envelope's wire shape), `bad-entropy-length`,
//! `bad-proof-length`, `bad-key-length`, `bad-seed-length`, `proof-refused`,
//! `entropy-mismatch`, `same-key`.

use lakko_contacts::key_rotation::{
    compose_key_rotation as rust_compose, decode_key_rotation as rust_decode,
    KeyRotation as RustKeyRotation, KeyRotationError, KEY_ROTATION_ENTROPY_LEN,
    KEY_ROTATION_ENVELOPE_LEN, KEY_ROTATION_PENDING_MAX_AGE_MS, KEY_ROTATION_PROOF_LEN,
};
use lakko_mnemonic::Mnemonic;
use wasm_bindgen::prelude::*;

// The envelope crate repeats the proof's length because it does not depend
// on the transparency crate; this crate depends on both.
const _: () = assert!(KEY_ROTATION_PROOF_LEN == lakko_transparency::SUPERSESSION_PROOF_LEN);
const _: () = assert!(KEY_ROTATION_ENTROPY_LEN == lakko_mnemonic::ENTROPY_LEN);

fn refused(code: &str, reason: &str) -> String {
    format!("{code}: {reason}")
}

fn wire_error(e: KeyRotationError) -> String {
    format!("{}: {e}", e.code())
}

/// A decoded key-rotation envelope. **`newEntropy` is the new account**:
/// never log it, store it only under the device key, and adopt it only after
/// `verifyKeyRotationEnvelope` accepted the envelope AND the server-side
/// change is visible (ADR-0074 section 3.3).
#[wasm_bindgen(js_name = KeyRotation)]
#[derive(Clone, PartialEq, Eq)]
pub struct WasmKeyRotation {
    inner: RustKeyRotation,
}

impl core::fmt::Debug for WasmKeyRotation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.inner.fmt(f)
    }
}

impl From<RustKeyRotation> for WasmKeyRotation {
    fn from(inner: RustKeyRotation) -> Self {
        Self { inner }
    }
}

#[wasm_bindgen(js_class = KeyRotation)]
impl WasmKeyRotation {
    /// Rotating-device clock, UNIX milliseconds, as a `bigint`. Ages a
    /// pending envelope out; orders and authorises nothing.
    #[wasm_bindgen(getter, js_name = preparedAtMs)]
    pub fn prepared_at_ms(&self) -> u64 {
        self.inner.prepared_at_ms
    }

    /// The 16 bytes of BIP39 entropy of the NEW recovery phrase
    /// (`RecoveryMnemonic.fromEntropy`).
    #[wasm_bindgen(getter, js_name = newEntropy)]
    pub fn new_entropy(&self) -> Vec<u8> {
        self.inner.new_entropy.to_vec()
    }

    /// The 145-byte supersession proof, old account key over new.
    #[wasm_bindgen(getter, js_name = supersessionProof)]
    pub fn supersession_proof(&self) -> Vec<u8> {
        self.inner.supersession_proof.to_vec()
    }
}

/// What `verifyKeyRotationEnvelope` established about an envelope.
#[wasm_bindgen(js_name = VerifiedKeyRotation)]
#[derive(Clone, PartialEq, Eq)]
pub struct WasmVerifiedKeyRotation {
    rotation: RustKeyRotation,
    new_signature_pubkey: [u8; 32],
    proof_expires_at_unix_secs: i64,
}

impl core::fmt::Debug for WasmVerifiedKeyRotation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("VerifiedKeyRotation")
            .field("rotation", &self.rotation)
            .field("new_signature_pubkey", &self.new_signature_pubkey)
            .finish_non_exhaustive()
    }
}

#[wasm_bindgen(js_class = VerifiedKeyRotation)]
impl WasmVerifiedKeyRotation {
    /// As on `KeyRotation`.
    #[wasm_bindgen(getter, js_name = preparedAtMs)]
    pub fn prepared_at_ms(&self) -> u64 {
        self.rotation.prepared_at_ms
    }

    /// As on `KeyRotation`. Checked: the account identity it derives has
    /// exactly `newSignaturePubkey`.
    #[wasm_bindgen(getter, js_name = newEntropy)]
    pub fn new_entropy(&self) -> Vec<u8> {
        self.rotation.new_entropy.to_vec()
    }

    /// The account identity public key the proof authorises and the entropy
    /// derives. A pending envelope is keyed on this: the device adopts the
    /// one whose key the published bundle (or the server) shows as the
    /// account's.
    #[wasm_bindgen(getter, js_name = newSignaturePubkey)]
    pub fn new_signature_pubkey(&self) -> Vec<u8> {
        self.new_signature_pubkey.to_vec()
    }

    /// The proof, verified under the account key the caller holds.
    #[wasm_bindgen(getter, js_name = supersessionProof)]
    pub fn supersession_proof(&self) -> Vec<u8> {
        self.rotation.supersession_proof.to_vec()
    }

    /// When the proof stops being accepted, UNIX seconds, as a `bigint`.
    #[wasm_bindgen(getter, js_name = proofExpiresAtUnixSecs)]
    pub fn proof_expires_at_unix_secs(&self) -> i64 {
        self.proof_expires_at_unix_secs
    }
}

fn compose_inner(
    prepared_at_ms: u64,
    new_entropy: &[u8],
    supersession_proof: &[u8],
) -> Result<Vec<u8>, String> {
    let new_entropy: [u8; KEY_ROTATION_ENTROPY_LEN] = new_entropy
        .try_into()
        .map_err(|_| refused("bad-entropy-length", "the new entropy must be 16 bytes"))?;
    let supersession_proof: [u8; KEY_ROTATION_PROOF_LEN] =
        supersession_proof.try_into().map_err(|_| {
            refused(
                "bad-proof-length",
                "the supersession proof must be 145 bytes",
            )
        })?;
    Ok(rust_compose(&RustKeyRotation {
        prepared_at_ms,
        new_entropy,
        supersession_proof,
    }))
}

fn verify_inner(
    plaintext: &[u8],
    held_account_pubkey: &[u8],
    now_unix_secs: i64,
) -> Result<WasmVerifiedKeyRotation, String> {
    let held: [u8; 32] = held_account_pubkey
        .try_into()
        .map_err(|_| refused("bad-key-length", "the held account key must be 32 bytes"))?;
    let rotation = rust_decode(plaintext).map_err(wire_error)?;
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
    let proof_expires_at_unix_secs =
        lakko_transparency::SupersessionProof::open(&rotation.supersession_proof)
            .map_err(|e| refused("proof-refused", &e.to_string()))?
            .expires_at_unix_secs;
    Ok(WasmVerifiedKeyRotation {
        rotation,
        new_signature_pubkey: authorised,
        proof_expires_at_unix_secs,
    })
}

/// Compose a key-rotation envelope: the plaintext of ONE MLS application
/// message in the account's DeviceGroup (inside the `ServiceEnvelope`
/// carrier). `newEntropy` is the new phrase's 16-byte entropy;
/// `supersessionProof` is `signSupersessionProof(old, new, ...)`.
/// `preparedAtMs` is a `bigint`.
///
/// **Send it only after the removed device is out of the DeviceGroup and the
/// epoch has advanced** (the module docs).
///
/// # Errors
///
/// Throws `bad-entropy-length` / `bad-proof-length`.
#[wasm_bindgen(js_name = composeKeyRotationEnvelope)]
pub fn compose_key_rotation_envelope(
    prepared_at_ms: u64,
    new_entropy: &[u8],
    supersession_proof: &[u8],
) -> Result<Vec<u8>, JsError> {
    compose_inner(prepared_at_ms, new_entropy, supersession_proof).map_err(|m| JsError::new(&m))
}

/// Strictly decode a key-rotation envelope. Shape only: this proves nothing
/// about who made it. Use `verifyKeyRotationEnvelope` before keeping one.
///
/// # Errors
///
/// Throws the envelope's code (`wrongDomain`, `truncated`, `unknownVersion`,
/// `badLength`).
#[wasm_bindgen(js_name = decodeKeyRotationEnvelope)]
pub fn decode_key_rotation_envelope(plaintext: &[u8]) -> Result<WasmKeyRotation, JsError> {
    rust_decode(plaintext)
        .map(Into::into)
        .map_err(|e| JsError::new(&wire_error(e)))
}

/// The three checks a kept device makes before it stores an envelope as
/// pending (ADR-0074 section 3.3), in one call so no client re-derives them:
///
/// 1. the supersession proof is signed by `heldAccountPubkey` (the account
///    identity key THIS device holds), has a validity window the format
///    allows, and has not expired at `nowUnixSecs` (a `bigint`);
/// 2. the account identity derived from the entropy (the phrase, its 64-byte
///    seed, `Identity.fromSeed`) has exactly the key the proof authorises;
/// 3. the new key is not the held one.
///
/// On success the device stores the result as PENDING, keyed on
/// `newSignaturePubkey`, and changes nothing. It adopts when it sees the
/// server-side commit: the published bundle carries that key, or its own
/// refresh is answered `key-generation-superseded`.
///
/// # Errors
///
/// Throws the envelope's own codes, `bad-key-length`, `proof-refused`,
/// `entropy-mismatch`, `same-key`.
#[wasm_bindgen(js_name = verifyKeyRotationEnvelope)]
pub fn verify_key_rotation_envelope(
    plaintext: &[u8],
    held_account_pubkey: &[u8],
    now_unix_secs: i64,
) -> Result<WasmVerifiedKeyRotation, JsError> {
    verify_inner(plaintext, held_account_pubkey, now_unix_secs).map_err(|m| JsError::new(&m))
}

/// The one length of a key-rotation envelope (192 bytes).
#[wasm_bindgen(js_name = keyRotationEnvelopeLen)]
#[must_use]
pub fn key_rotation_envelope_len() -> u32 {
    u32::try_from(KEY_ROTATION_ENVELOPE_LEN).unwrap_or(u32::MAX)
}

/// How long a pending envelope is kept before it is dropped, in
/// milliseconds (14 days), as a `bigint`.
#[wasm_bindgen(js_name = keyRotationPendingMaxAgeMs)]
#[must_use]
pub fn key_rotation_pending_max_age_ms() -> u64 {
    KEY_ROTATION_PENDING_MAX_AGE_MS
}

/// `true` when a pending envelope prepared at `preparedAtMs` is too old to
/// keep at `nowMs` (both `bigint`). A stamp in the future is not expired.
#[wasm_bindgen(js_name = keyRotationPendingExpired)]
#[must_use]
pub fn key_rotation_pending_expired(prepared_at_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(prepared_at_ms) > KEY_ROTATION_PENDING_MAX_AGE_MS
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::signing::SigningKey;

    const NOW: i64 = 1_800_000_000;

    fn code(message: &str) -> &str {
        message.split(':').next().unwrap_or("")
    }

    fn account(entropy: [u8; 16]) -> (SigningKey, [u8; 32]) {
        let seed = Mnemonic::from_entropy(&entropy).unwrap().to_seed();
        let identity = lakko_mls::Identity::from_seed(seed.as_ref()).unwrap();
        let signing = SigningKey::from_seed(&identity.ed25519_seed_bytes());
        let public = signing.verifying_key().to_bytes();
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
        let wire = compose_inner(1_800_000_000_000, &new_entropy, &p).unwrap();
        assert_eq!(wire.len(), key_rotation_envelope_len() as usize);
        assert_eq!(wire.len(), 192);

        let decoded: WasmKeyRotation = rust_decode(&wire).unwrap().into();
        assert_eq!(decoded.prepared_at_ms(), 1_800_000_000_000);
        assert_eq!(decoded.new_entropy(), new_entropy);
        assert_eq!(decoded.supersession_proof(), p);
        assert!(format!("{decoded:?}").contains("redacted"));

        let verified = verify_inner(&wire, &old_pub, NOW + 60).unwrap();
        assert_eq!(verified.new_signature_pubkey(), new_pub);
        assert_eq!(verified.new_entropy(), new_entropy);
        assert_eq!(verified.proof_expires_at_unix_secs(), NOW + 14 * 86_400);
        assert!(format!("{verified:?}").contains("redacted"));
    }

    #[test]
    fn a_kept_device_refuses_every_envelope_that_is_not_its_own_accounts_rotation() {
        let (old, old_pub) = account([1; 16]);
        let (stranger, _) = account([3; 16]);
        let new_entropy = [2u8; 16];
        let (_, new_pub) = account(new_entropy);
        let envelope = |entropy: [u8; 16], proof: &[u8]| compose_inner(1, &entropy, proof).unwrap();
        let good = proof(&old, &new_pub, NOW, NOW + 86_400);
        let refusal = |wire: &[u8], held: &[u8], now: i64| {
            let message = verify_inner(wire, held, now).unwrap_err();
            code(&message).to_string()
        };

        assert_eq!(
            refusal(
                &envelope(new_entropy, &proof(&stranger, &new_pub, NOW, NOW + 86_400)),
                &old_pub,
                NOW
            ),
            "proof-refused"
        );
        let forged = lakko_transparency::SupersessionProof::new(old_pub, new_pub, NOW, NOW + 60)
            .sign(&stranger)
            .to_vec();
        assert_eq!(
            refusal(&envelope(new_entropy, &forged), &old_pub, NOW),
            "proof-refused"
        );
        assert_eq!(
            refusal(&envelope(new_entropy, &good), &old_pub, NOW + 86_401),
            "proof-refused"
        );
        assert_eq!(
            refusal(&envelope([9; 16], &good), &old_pub, NOW),
            "entropy-mismatch"
        );
        assert_eq!(
            refusal(
                &envelope([1; 16], &proof(&old, &old_pub, NOW, NOW + 60)),
                &old_pub,
                NOW
            ),
            "same-key"
        );
        assert_eq!(
            refusal(&envelope(new_entropy, &good), &[0; 31], NOW),
            "bad-key-length"
        );
        assert_eq!(refusal(b"hello", &old_pub, NOW), "wrongDomain");
        assert_eq!(
            refusal(&envelope(new_entropy, &good)[..100], &old_pub, NOW),
            "badLength"
        );
        assert_eq!(
            code(&compose_inner(1, &[0; 15], &[0; 145]).unwrap_err()),
            "bad-entropy-length"
        );
        assert_eq!(
            code(&compose_inner(1, &[0; 16], &[0; 144]).unwrap_err()),
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
}
