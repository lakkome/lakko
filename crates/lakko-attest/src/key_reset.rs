// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Key-reset attestation ([ADR-0073] section 2.2).
//!
//! A *key-reset attestation* is a compact, domain-separated, Ed25519-signed
//! statement by `services/auth` that the account behind `account_ref` moved to
//! key generation `key_generation` and presented `new_signature_pubkey` as the
//! identity key of that generation. `services/auth` mints it inside the one
//! transaction of `POST /account/keys/reset/finish`; discovery requires it
//! before it lets a published identity key change without a supersession
//! proof.
//!
//! Until ADR-0078 phase 6 it also named the account's DeviceGroup journal id
//! and the Delivery Service accepted it to let a stranger re-root that
//! journal. Since phase 6 the DeviceGroup id is derived from the account seed
//! (`lakko_mls::DeviceGroup::group_id_for`), which auth does not hold and must
//! not learn: an attestation naming it would put the account id and its
//! journal in one request. A reset changes the seed, so the re-founded
//! DeviceGroup lives at a fresh journal the resetting device claims plainly;
//! nothing is re-rooted and the DS sees no attestation.
//!
//! Read the semantics plainly, as with the genesis attestation: this is a
//! **binding of the authenticated account to a client-asserted key at
//! generation `g`**, not an independent proof the key is right (auth never
//! sees key material, invariant #2). It stops a holder of the attestation from
//! presenting a different key under it, and lets discovery refuse a bundle
//! whose key is not the attested one.
//!
//! ```text
//! wire           = version(1) = 2 ‖ account_ref(32)
//!                  ‖ key_generation(8, big-endian u64) ‖ new_signature_pubkey(32)
//!                  ‖ signed_at_unix_millis(8, big-endian i64) ‖ Ed25519_sig(64)
//!                = 145 bytes
//! signing input  = DOMAIN ‖ version ‖ account_ref
//!                  ‖ key_generation ‖ new_signature_pubkey ‖ signed_at
//! DOMAIN         = b"lakko/key-reset-attest/v1"
//! account_ref    = BLAKE3-derive-key("lakko/key-reset/account-ref/v1", account_id)
//! ```
//!
//! Why these fields:
//!
//! - **`account_ref`, not `account_id`.** A one-way per-account value only
//!   auth and discovery can relate to an account; a fresh domain rather than
//!   the KT-log `username_hash`, so the attestation, which discovery may
//!   serve, links to no public log entry by equality.
//! - **`new_signature_pubkey`.** Binds the attestation to the identity the
//!   reset installs.
//! - **`signed_at`.** For audit, and for the one freshness check that exists:
//!   a client bounds how old an attestation may be when it first applies it.
//!
//! Two verify entry points. Neither checks the generation against stored
//! state: that comparison belongs to the party that holds the stored value.
//!
//! - [`verify_key_reset_binding`] — signature plus the two field bindings,
//!   **no freshness**. Discovery uses this: the generation's strict
//!   monotonicity makes a replay harmless (an applied generation cannot be
//!   applied again), and a reset upload may legitimately be retried days after
//!   the mint when the resetting device crashed between the server reset and
//!   the publish.
//! - [`verify_key_reset_fresh`] — the same plus a caller-supplied age bound on
//!   `signed_at`, for a client applying its own reset (ADR-0073 section 4.2
//!   bounds it at 30 days; older, the person signs in again first).
//!
//! [ADR-0073]: ../../../docs/adr/0073-account-key-reset.md

use lakko_crypto::hash;
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN, SIGNATURE_LEN};
use thiserror::Error;

/// Domain separator mixed into the signing input. Distinct from every other
/// `lakko-attest` kind so the five are non-interchangeable even under the
/// shared auth attest key. Bumping this string (and [`VERSION`]) invalidates
/// every previously-issued key-reset attestation.
const DOMAIN: &[u8] = b"lakko/key-reset-attest/v1";

/// Wire-format version. The verifier refuses any other value. `2` since
/// ADR-0078 phase 6 dropped the DeviceGroup journal id (flag day).
const VERSION: u8 = 2;

/// BLAKE3 derive-key context of [`key_reset_account_ref`].
pub const ACCOUNT_REF_DOMAIN: &str = "lakko/key-reset/account-ref/v1";

/// Length of a raw account id (UUIDv7 bytes).
pub const ACCOUNT_ID_LEN: usize = 16;

/// Length of `account_ref` (a BLAKE3 output).
pub const ACCOUNT_REF_LEN: usize = 32;

/// Length of the big-endian `u64` key generation.
const GENERATION_LEN: usize = 8;

/// Length of the new identity signature pubkey (Ed25519).
const PUBKEY_LEN: usize = PUBLIC_KEY_LEN;

/// Length of the big-endian `i64` mint timestamp.
const SIGNED_AT_LEN: usize = 8;

/// Clock-skew tolerance for the freshness check: a `signed_at` this far in
/// the future of `now` is still accepted (the same tolerance the backup head
/// applies).
const MAX_CLOCK_SKEW_MILLIS: i64 = 5 * 60 * 1_000;

/// Total serialized key-reset-attestation length:
/// `version + account_ref + generation + pubkey + signed_at + sig`.
pub const KEY_RESET_ATTESTATION_LEN: usize =
    1 + ACCOUNT_REF_LEN + GENERATION_LEN + PUBKEY_LEN + SIGNED_AT_LEN + SIGNATURE_LEN;

/// Derive the per-account pseudonym a key-reset attestation names:
/// `BLAKE3-derive-key("lakko/key-reset/account-ref/v1", account_id)`.
/// One-way; auth and discovery recompute it from the session's account id.
#[must_use]
pub fn key_reset_account_ref(account_id: &[u8; ACCOUNT_ID_LEN]) -> [u8; ACCOUNT_REF_LEN] {
    hash::blake3_derive_key_from_material(ACCOUNT_REF_DOMAIN, account_id)
}

/// Why a key-reset attestation was rejected.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum KeyResetAttestError {
    /// The byte slice is not exactly [`KEY_RESET_ATTESTATION_LEN`] long.
    #[error("key-reset attestation has wrong length: {got} bytes")]
    BadLength {
        /// The actual length received.
        got: usize,
    },
    /// The version byte is not [`VERSION`].
    #[error("unsupported key-reset attestation version: {0}")]
    BadVersion(u8),
    /// The Ed25519 signature did not verify (tampered field, wrong key, or
    /// malformed signature bytes).
    #[error("key-reset attestation signature is invalid")]
    BadSignature,
    /// The attested `account_ref` does not match the expected one.
    #[error("key-reset attestation account_ref does not match the expected account")]
    AccountRefMismatch,
    /// The attested new signature pubkey does not match the expected one.
    #[error("key-reset attestation pubkey does not match the expected key")]
    PubkeyMismatch,
    /// `signed_at` lies outside the caller's freshness window.
    #[error("key-reset attestation is stale (signed_at outside the freshness window)")]
    Stale,
}

/// The authenticated claims carried by a key-reset attestation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyResetAttestation {
    /// [`key_reset_account_ref`] of the account that reset its keys.
    pub account_ref: [u8; ACCOUNT_REF_LEN],
    /// The key generation the reset moved the account to (`1` is genesis, so
    /// a reset mints `2` or above).
    pub key_generation: u64,
    /// The client-asserted identity signature pubkey of this generation.
    pub new_signature_pubkey: [u8; PUBKEY_LEN],
    /// Mint time, Unix milliseconds.
    pub signed_at_unix_millis: i64,
}

impl KeyResetAttestation {
    /// Construct claims to be signed.
    #[must_use]
    pub const fn new(
        account_ref: [u8; ACCOUNT_REF_LEN],
        key_generation: u64,
        new_signature_pubkey: [u8; PUBKEY_LEN],
        signed_at_unix_millis: i64,
    ) -> Self {
        Self {
            account_ref,
            key_generation,
            new_signature_pubkey,
            signed_at_unix_millis,
        }
    }

    /// Construct claims for `account_id`, deriving `account_ref`. The
    /// auth-side constructor.
    #[must_use]
    pub fn for_account(
        account_id: &[u8; ACCOUNT_ID_LEN],
        key_generation: u64,
        new_signature_pubkey: [u8; PUBKEY_LEN],
        signed_at_unix_millis: i64,
    ) -> Self {
        Self::new(
            key_reset_account_ref(account_id),
            key_generation,
            new_signature_pubkey,
            signed_at_unix_millis,
        )
    }

    /// The exact bytes the signature covers (domain-separated).
    fn signing_input(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(
            DOMAIN.len() + 1 + ACCOUNT_REF_LEN + GENERATION_LEN + PUBKEY_LEN + SIGNED_AT_LEN,
        );
        buf.extend_from_slice(DOMAIN);
        buf.push(VERSION);
        buf.extend_from_slice(&self.account_ref);
        buf.extend_from_slice(&self.key_generation.to_be_bytes());
        buf.extend_from_slice(&self.new_signature_pubkey);
        buf.extend_from_slice(&self.signed_at_unix_millis.to_be_bytes());
        buf
    }

    /// Serialize + sign these claims into the
    /// [`KEY_RESET_ATTESTATION_LEN`]-byte wire form. Called by `services/auth`
    /// inside `POST /account/keys/reset/finish`.
    #[must_use]
    pub fn sign(&self, signing: &SigningKey) -> [u8; KEY_RESET_ATTESTATION_LEN] {
        let sig = signing.sign(&self.signing_input());
        let mut out = [0u8; KEY_RESET_ATTESTATION_LEN];
        out[0] = VERSION;
        let mut off = 1;
        out[off..off + ACCOUNT_REF_LEN].copy_from_slice(&self.account_ref);
        off += ACCOUNT_REF_LEN;
        out[off..off + GENERATION_LEN].copy_from_slice(&self.key_generation.to_be_bytes());
        off += GENERATION_LEN;
        out[off..off + PUBKEY_LEN].copy_from_slice(&self.new_signature_pubkey);
        off += PUBKEY_LEN;
        out[off..off + SIGNED_AT_LEN].copy_from_slice(&self.signed_at_unix_millis.to_be_bytes());
        off += SIGNED_AT_LEN;
        out[off..off + SIGNATURE_LEN].copy_from_slice(&sig.to_bytes());
        out
    }

    /// Parse the wire form and verify the Ed25519 signature against
    /// `verifying`. Returns the authenticated claims.
    ///
    /// This does **not** check any binding or freshness; callers use
    /// [`verify_key_reset_binding`] or [`verify_key_reset_fresh`].
    ///
    /// # Errors
    ///
    /// [`KeyResetAttestError::BadLength`] / [`KeyResetAttestError::BadVersion`]
    /// on a malformed envelope; [`KeyResetAttestError::BadSignature`] if the
    /// signature is malformed or does not verify.
    pub fn open(bytes: &[u8], verifying: &VerifyingKey) -> Result<Self, KeyResetAttestError> {
        if bytes.len() != KEY_RESET_ATTESTATION_LEN {
            return Err(KeyResetAttestError::BadLength { got: bytes.len() });
        }
        if bytes[0] != VERSION {
            return Err(KeyResetAttestError::BadVersion(bytes[0]));
        }
        let mut off = 1;
        let mut account_ref = [0u8; ACCOUNT_REF_LEN];
        account_ref.copy_from_slice(&bytes[off..off + ACCOUNT_REF_LEN]);
        off += ACCOUNT_REF_LEN;
        let mut gen_bytes = [0u8; GENERATION_LEN];
        gen_bytes.copy_from_slice(&bytes[off..off + GENERATION_LEN]);
        off += GENERATION_LEN;
        let mut new_signature_pubkey = [0u8; PUBKEY_LEN];
        new_signature_pubkey.copy_from_slice(&bytes[off..off + PUBKEY_LEN]);
        off += PUBKEY_LEN;
        let mut signed_at = [0u8; SIGNED_AT_LEN];
        signed_at.copy_from_slice(&bytes[off..off + SIGNED_AT_LEN]);
        off += SIGNED_AT_LEN;

        let sig = Signature::from_bytes(&bytes[off..off + SIGNATURE_LEN])
            .map_err(|_| KeyResetAttestError::BadSignature)?;
        let claims = Self {
            account_ref,
            key_generation: u64::from_be_bytes(gen_bytes),
            new_signature_pubkey,
            signed_at_unix_millis: i64::from_be_bytes(signed_at),
        };
        verifying
            .verify(&claims.signing_input(), &sig)
            .map_err(|_| KeyResetAttestError::BadSignature)?;
        Ok(claims)
    }

    /// `true` when `signed_at` lies within the freshness window: at or after
    /// `now - max_age`, and no further in the future than the clock-skew
    /// tolerance ([`MAX_CLOCK_SKEW_MILLIS`]). Unix milliseconds throughout;
    /// saturating arithmetic so extreme values cannot overflow.
    #[must_use]
    pub fn is_fresh(&self, now_unix_millis: i64, max_age_millis: i64) -> bool {
        let lower = now_unix_millis.saturating_sub(max_age_millis);
        let upper = now_unix_millis.saturating_add(MAX_CLOCK_SKEW_MILLIS);
        self.signed_at_unix_millis >= lower && self.signed_at_unix_millis <= upper
    }
}

/// Verify a key-reset attestation's signature and its `account_ref` / new
/// pubkey bindings, **without** a freshness check. The entry point for
/// discovery, which compares the returned
/// `key_generation` with the one they recorded (strictly greater, else
/// refuse) — a replay of an applied generation is harmless, and a legitimate
/// retry may be days old.
///
/// # Errors
///
/// Propagates [`KeyResetAttestation::open`] errors, then
/// [`KeyResetAttestError::AccountRefMismatch`] /
/// [`KeyResetAttestError::PubkeyMismatch`] if the (now-authenticated) claims
/// do not match what the caller expects.
pub fn verify_key_reset_binding(
    bytes: &[u8],
    verifying: &VerifyingKey,
    expected_account_ref: &[u8; ACCOUNT_REF_LEN],
    expected_pubkey: &[u8; PUBKEY_LEN],
) -> Result<KeyResetAttestation, KeyResetAttestError> {
    let att = KeyResetAttestation::open(bytes, verifying)?;
    if &att.account_ref != expected_account_ref {
        return Err(KeyResetAttestError::AccountRefMismatch);
    }
    if &att.new_signature_pubkey != expected_pubkey {
        return Err(KeyResetAttestError::PubkeyMismatch);
    }
    Ok(att)
}

/// [`verify_key_reset_binding`] plus a freshness bound: `signed_at` must be
/// within `max_age_millis` of `now_unix_millis`. The entry point for a client
/// applying its own reset after a crash, where an attestation older than its
/// policy asks for a fresh sign-in first.
///
/// # Errors
///
/// Propagates [`verify_key_reset_binding`] errors, then
/// [`KeyResetAttestError::Stale`].
pub fn verify_key_reset_fresh(
    bytes: &[u8],
    verifying: &VerifyingKey,
    expected_account_ref: &[u8; ACCOUNT_REF_LEN],
    expected_pubkey: &[u8; PUBKEY_LEN],
    now_unix_millis: i64,
    max_age_millis: i64,
) -> Result<KeyResetAttestation, KeyResetAttestError> {
    let att = verify_key_reset_binding(bytes, verifying, expected_account_ref, expected_pubkey)?;
    if !att.is_fresh(now_unix_millis, max_age_millis) {
        return Err(KeyResetAttestError::Stale);
    }
    Ok(att)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ActionAttestation, AttestError, BackupHead, BackupHeadError, GenesisAttestError,
        GenesisAttestation, ReleaseManifest, ReleaseManifestError, UsernameCertError,
        UsernameCertificate,
    };

    fn key() -> SigningKey {
        SigningKey::from_seed(&[0x11; 32])
    }

    const ACCOUNT: [u8; 16] = [0xAA; 16];

    fn sample() -> KeyResetAttestation {
        KeyResetAttestation::for_account(&ACCOUNT, 2, [0xCC; 32], 1_000_000)
    }

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }

    /// `BLAKE3-derive-key("lakko/key-reset/account-ref/v1", [0xAA; 16])`.
    const GOLDEN_ACCOUNT_REF: &str =
        "303c3e68e91fe8d3c2b063f9280cc11d0e72b02d87e3b9ef351f9e01fa0fb7f1";

    #[test]
    fn wire_layout_is_pinned() {
        assert_eq!(KEY_RESET_ATTESTATION_LEN, 145);
        let wire = sample().sign(&key());
        assert_eq!(wire.len(), 145);
        assert_eq!(wire[0], 2);
        assert_eq!(&wire[1..33], &key_reset_account_ref(&ACCOUNT));
        assert_eq!(&wire[33..41], &2u64.to_be_bytes());
        assert_eq!(&wire[41..73], &[0xCC; 32]);
        assert_eq!(&wire[73..81], &1_000_000i64.to_be_bytes());
    }

    #[test]
    fn derivations_are_pinned() {
        assert_eq!(hex(&key_reset_account_ref(&ACCOUNT)), GOLDEN_ACCOUNT_REF);
        assert_ne!(
            key_reset_account_ref(&ACCOUNT),
            key_reset_account_ref(&[0xAB; 16])
        );
    }

    #[test]
    fn sign_then_verify_binding_roundtrips() {
        let sk = key();
        let att = sample();
        let wire = att.sign(&sk);
        let got = verify_key_reset_binding(
            &wire,
            &sk.verifying_key(),
            &key_reset_account_ref(&ACCOUNT),
            &[0xCC; 32],
        )
        .expect("valid attestation verifies");
        assert_eq!(got, att);
        assert_eq!(got.key_generation, 2);
    }

    #[test]
    fn tampering_any_field_breaks_the_signature() {
        let sk = key();
        let wire = sample().sign(&sk);
        let vk = sk.verifying_key();
        // One byte in each region: account_ref, generation, pubkey,
        // signed_at, signature.
        for idx in [1usize, 36, 50, 76, KEY_RESET_ATTESTATION_LEN - 1] {
            let mut bad = wire;
            bad[idx] ^= 0x01;
            assert_eq!(
                KeyResetAttestation::open(&bad, &vk),
                Err(KeyResetAttestError::BadSignature),
                "tampering byte {idx} must invalidate the signature"
            );
        }
    }

    #[test]
    fn wrong_key_is_rejected() {
        let wire = sample().sign(&key());
        let other = SigningKey::from_seed(&[0x22; 32]).verifying_key();
        assert_eq!(
            KeyResetAttestation::open(&wire, &other),
            Err(KeyResetAttestError::BadSignature)
        );
    }

    #[test]
    fn every_binding_is_enforced() {
        let sk = key();
        let wire = sample().sign(&sk);
        let vk = sk.verifying_key();
        let account_ref = key_reset_account_ref(&ACCOUNT);
        assert_eq!(
            verify_key_reset_binding(&wire, &vk, &[0x00; 32], &[0xCC; 32]),
            Err(KeyResetAttestError::AccountRefMismatch)
        );
        assert_eq!(
            verify_key_reset_binding(&wire, &vk, &account_ref, &[0x00; 32]),
            Err(KeyResetAttestError::PubkeyMismatch)
        );
        // Another account's attestation never binds to this account.
        let other =
            KeyResetAttestation::for_account(&[0xAB; 16], 2, [0xCC; 32], 1_000_000).sign(&sk);
        assert_eq!(
            verify_key_reset_binding(&other, &vk, &account_ref, &[0xCC; 32]),
            Err(KeyResetAttestError::AccountRefMismatch)
        );
    }

    #[test]
    fn freshness_is_enforced_only_by_the_fresh_entry_point() {
        let sk = key();
        let wire = sample().sign(&sk); // signed_at = 1_000_000
        let vk = sk.verifying_key();
        let account_ref = key_reset_account_ref(&ACCOUNT);
        let ok = |now: i64, max_age: i64| {
            verify_key_reset_fresh(&wire, &vk, &account_ref, &[0xCC; 32], now, max_age)
        };
        assert!(ok(1_000_000, 0).is_ok(), "signed exactly now");
        assert!(ok(1_500_000, 500_000).is_ok(), "at the edge of the window");
        assert_eq!(ok(1_500_001, 500_000), Err(KeyResetAttestError::Stale));
        assert!(
            ok(1_000_000 - MAX_CLOCK_SKEW_MILLIS, 0).is_ok(),
            "future within skew"
        );
        assert_eq!(
            ok(1_000_000 - MAX_CLOCK_SKEW_MILLIS - 1, 0),
            Err(KeyResetAttestError::Stale),
            "future beyond skew"
        );
        assert!(ok(i64::MAX, i64::MAX).is_ok(), "saturating arithmetic");
        // The binding entry point ignores signed_at entirely: a reset upload
        // may be retried days after the mint.
        assert!(verify_key_reset_binding(&wire, &vk, &account_ref, &[0xCC; 32]).is_ok());
    }

    #[test]
    fn malformed_envelope_is_rejected() {
        let sk = key();
        let vk = sk.verifying_key();
        assert_eq!(
            KeyResetAttestation::open(&[0u8; 10], &vk),
            Err(KeyResetAttestError::BadLength { got: 10 })
        );
        assert_eq!(
            KeyResetAttestation::open(&[0u8; 144], &vk),
            Err(KeyResetAttestError::BadLength { got: 144 })
        );
        assert_eq!(
            KeyResetAttestation::open(&[0u8; 146], &vk),
            Err(KeyResetAttestError::BadLength { got: 146 })
        );
        // The retired 177-byte version-1 layout is refused by its length.
        assert_eq!(
            KeyResetAttestation::open(&[1u8; 177], &vk),
            Err(KeyResetAttestError::BadLength { got: 177 })
        );
        let mut wire = sample().sign(&sk);
        wire[0] = 1;
        assert_eq!(
            KeyResetAttestation::open(&wire, &vk),
            Err(KeyResetAttestError::BadVersion(1))
        );
    }

    #[test]
    fn generations_and_keys_do_not_cross_verify() {
        // Generation and pubkey are both signed: an attestation for (g=2, K1)
        // is not an attestation for (g=3, K1) or (g=2, K2).
        let sk = key();
        let vk = sk.verifying_key();
        let g2 = KeyResetAttestation::for_account(&ACCOUNT, 2, [0x01; 32], 1_000).sign(&sk);
        let opened = KeyResetAttestation::open(&g2, &vk).unwrap();
        assert_eq!(opened.key_generation, 2);
        let mut forged = g2;
        forged[33..41].copy_from_slice(&3u64.to_be_bytes());
        assert_eq!(
            KeyResetAttestation::open(&forged, &vk),
            Err(KeyResetAttestError::BadSignature)
        );
        assert_eq!(
            verify_key_reset_binding(&g2, &vk, &key_reset_account_ref(&ACCOUNT), &[0x02; 32]),
            Err(KeyResetAttestError::PubkeyMismatch)
        );
    }

    #[test]
    fn domains_do_not_cross_between_every_attestation_kind() {
        // Domain separation is load-bearing: under the SHARED auth attest key a
        // key-reset attestation and each of the four other auth-signed kinds
        // (plus the discovery-signed username certificate) must be
        // non-interchangeable. The length (145) differs from every other kind,
        // so (a) proves the length gate both ways and (b) proves the domain
        // prefix by splicing signatures across kinds.
        let sk = key();
        let vk = sk.verifying_key();

        let reset_wire = sample().sign(&sk);
        let action_wire = ActionAttestation::new([0xAA; 16], [0xBB; 32], 1_000).sign(&sk);
        let genesis_wire =
            GenesisAttestation::new([0xAA; 16], [0xBB; 32], [0xCC; 32], 1_000).sign(&sk);
        let backup_wire = BackupHead::new([0xAA; 16], 7, [0xBB; 32], 1_000_000).sign(&sk);
        let release_wire = ReleaseManifest::new(10, 5, 1_000_000).sign(&sk);
        let cert_wire = UsernameCertificate::new([0xDD; 64], [0xAA; 16], 1_000_000).sign(&sk);

        // (a) Length, both ways.
        for (name, other) in [
            ("action", &action_wire[..]),
            ("genesis", &genesis_wire[..]),
            ("backup-head", &backup_wire[..]),
            ("release-manifest", &release_wire[..]),
            ("username-cert", &cert_wire[..]),
        ] {
            assert_ne!(other.len(), KEY_RESET_ATTESTATION_LEN);
            assert!(
                matches!(
                    KeyResetAttestation::open(other, &vk),
                    Err(KeyResetAttestError::BadLength { .. })
                ),
                "a {name} blob must not parse as a key-reset attestation"
            );
        }
        assert!(matches!(
            ActionAttestation::open(&reset_wire, &vk),
            Err(AttestError::BadLength { .. })
        ));
        assert!(matches!(
            GenesisAttestation::open(&reset_wire, &vk),
            Err(GenesisAttestError::BadLength { .. })
        ));
        assert!(matches!(
            BackupHead::open(&reset_wire, &vk),
            Err(BackupHeadError::BadLength { .. })
        ));
        assert!(matches!(
            ReleaseManifest::open(&reset_wire, &vk),
            Err(ReleaseManifestError::BadLength { .. })
        ));
        assert!(matches!(
            UsernameCertificate::open(&reset_wire, &vk),
            Err(UsernameCertError::BadLength { .. })
        ));

        // (b) Domain: a foreign signature spliced into a reset-shaped buffer,
        // and the reset signature spliced into every foreign buffer, never
        // verifies.
        for (name, foreign) in [
            ("action", &action_wire[..]),
            ("genesis", &genesis_wire[..]),
            ("backup-head", &backup_wire[..]),
            ("release-manifest", &release_wire[..]),
            ("username-cert", &cert_wire[..]),
        ] {
            let sig = &foreign[foreign.len() - SIGNATURE_LEN..];
            let mut spliced = sample().sign(&sk);
            spliced[KEY_RESET_ATTESTATION_LEN - SIGNATURE_LEN..].copy_from_slice(sig);
            assert_eq!(
                KeyResetAttestation::open(&spliced, &vk),
                Err(KeyResetAttestError::BadSignature),
                "a {name}-domain signature must not verify as a key-reset attestation"
            );
        }
        let reset_sig = &reset_wire[KEY_RESET_ATTESTATION_LEN - SIGNATURE_LEN..];
        let splice = |wire: &[u8]| {
            let mut out = wire.to_vec();
            let len = out.len();
            out[len - SIGNATURE_LEN..].copy_from_slice(reset_sig);
            out
        };
        assert_eq!(
            ActionAttestation::open(&splice(&action_wire), &vk),
            Err(AttestError::BadSignature)
        );
        assert_eq!(
            GenesisAttestation::open(&splice(&genesis_wire), &vk),
            Err(GenesisAttestError::BadSignature)
        );
        assert_eq!(
            BackupHead::open(&splice(&backup_wire), &vk),
            Err(BackupHeadError::BadSignature)
        );
        assert_eq!(
            ReleaseManifest::open(&splice(&release_wire), &vk),
            Err(ReleaseManifestError::BadSignature)
        );
        assert_eq!(
            UsernameCertificate::open(&splice(&cert_wire), &vk),
            Err(UsernameCertError::BadSignature)
        );
    }
}
