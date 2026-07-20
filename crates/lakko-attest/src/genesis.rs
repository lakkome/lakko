// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Genesis attestation (G-30 finding 2, [ADR-0036]).
//!
//! A *genesis attestation* is a compact, domain-separated, Ed25519-signed proof
//! that the party which OPAQUE-authenticated as `account_id` presented THIS
//! identity pubkey as its genesis (first-ever) key for `username_hash`, at the
//! time the attestation was minted. It closes KT-log **first-entry squatting**:
//! ADR-0019's genesis self-signature only DETECTS a fabricated first binding
//! (via CONIKS self-monitoring), whereas a genesis attestation PREVENTS it — the
//! transparency append gate refuses a genesis leaf whose attestation is absent
//! or does not verify, and only the account holder (the sole party who can
//! OPAQUE-authenticate as `account_id`) can mint one.
//!
//! Read the semantics plainly: this is a **binding of an OPAQUE-authenticated
//! account to a client-asserted key**, not an independently-verified claim that
//! the pubkey is the "right" key for the account — auth has no such ground truth
//! under the zero-trust model (the identity key is client-generated, invariant
//! #2). That binding is exactly sufficient to prevent squatting, because a
//! squatter cannot authenticate as the victim's `account_id` and therefore
//! cannot obtain the auth service's signature over the victim's fields.
//!
//! The format deliberately mirrors [`ActionAttestation`](crate::ActionAttestation):
//! a small fixed-layout binary blob (not a JWT), so the verifier stays trivial
//! to read, bounded, and fuzzable, and pulls no JWS / JWKS-fetch dependency into
//! the core. The trusted verifying key is supplied by the caller (the client
//! holds it from pinned / bootstrap trust config; the transparency gate holds it
//! from `LAKKO_TRANSPARENCY_GENESIS_ATTEST_KEY`).
//!
//! ```text
//! wire           = version(1) ‖ account_id(16) ‖ username_hash(32) ‖ pubkey(32)
//!                  ‖ expires_at_unix_secs(8, big-endian i64) ‖ Ed25519_sig(64)
//! signing input  = DOMAIN ‖ version ‖ account_id ‖ username_hash ‖ pubkey
//!                  ‖ expires_at
//! DOMAIN         = b"lakko/genesis-attest/v1"   (NEW — never reuse device-attest/v1)
//! ```
//!
//! The domain string is distinct from [`ActionAttestation`]'s
//! `lakko/device-attest/v1` so a device-removal attestation can never be replayed
//! as a genesis attestation (or vice versa) even when signed by the same key
//! (ADR-0036 §3 reuses the auth attest key across both roles).
//!
//! Two verify entry points — the freshness split is load-bearing (ADR-0036 §1):
//!
//! - [`verify_genesis_binding`] — signature + account + username_hash + pubkey
//!   binding, with **no freshness check**. The **client** uses this on first
//!   observation: the attestation is a permanent historical artifact, so
//!   enforcing an expiry would make every genesis leaf unverifiable once the mint
//!   TTL lapses.
//! - [`verify_genesis_fresh`] — the binding check on the fields transparency has
//!   (it never learns `account_id`) PLUS freshness. The **transparency append
//!   gate** uses this: the expiry bounds the mint→append replay window, not the
//!   leaf's lifetime.
//!
//! [ADR-0036]: ../../../docs/adr/0036-auth-attested-genesis.md

use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN, SIGNATURE_LEN};
use thiserror::Error;

/// Domain separator mixed into the signing input. Distinct from
/// [`ActionAttestation`](crate::ActionAttestation)'s `lakko/device-attest/v1`
/// so the two attestation kinds are cryptographically non-interchangeable even
/// under a shared signing key. Bumping this string (and [`VERSION`]) invalidates
/// every previously-issued genesis attestation.
const DOMAIN: &[u8] = b"lakko/genesis-attest/v1";

/// Wire-format version. The verifier refuses any other value.
const VERSION: u8 = 1;

/// Length of the account id (raw UUIDv7 bytes).
const ACCOUNT_LEN: usize = 16;

/// Length of the transparency `username_hash`
/// (`BLAKE3("lakko/transparency/account/v1" ‖ account_id)`).
const USERNAME_HASH_LEN: usize = 32;

/// Length of the genesis identity pubkey (Ed25519).
const PUBKEY_LEN: usize = PUBLIC_KEY_LEN;

/// Length of the big-endian `i64` expiry.
const EXPIRES_LEN: usize = 8;

/// Total serialized genesis-attestation length:
/// `version + account_id + username_hash + pubkey + expires_at + signature`.
pub const GENESIS_ATTESTATION_LEN: usize =
    1 + ACCOUNT_LEN + USERNAME_HASH_LEN + PUBKEY_LEN + EXPIRES_LEN + SIGNATURE_LEN;

/// Why a genesis attestation was rejected. Returned by
/// [`GenesisAttestation::open`], [`verify_genesis_binding`], and
/// [`verify_genesis_fresh`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum GenesisAttestError {
    /// The byte slice is not exactly [`GENESIS_ATTESTATION_LEN`] long.
    #[error("genesis attestation has wrong length: {got} bytes")]
    BadLength {
        /// The actual length received.
        got: usize,
    },
    /// The version byte is not [`VERSION`].
    #[error("unsupported genesis attestation version: {0}")]
    BadVersion(u8),
    /// The Ed25519 signature did not verify (tampered field, wrong key, or
    /// malformed signature bytes).
    #[error("genesis attestation signature is invalid")]
    BadSignature,
    /// The attested account id does not match the expected one.
    #[error("genesis attestation account id does not match the expected account")]
    AccountMismatch,
    /// The attested `username_hash` does not match the expected one.
    #[error("genesis attestation username hash does not match the expected username")]
    UsernameMismatch,
    /// The attested identity pubkey does not match the expected one.
    #[error("genesis attestation pubkey does not match the expected key")]
    PubkeyMismatch,
    /// The attestation's expiry is in the past relative to `now`.
    #[error("genesis attestation has expired")]
    Expired,
}

/// The authenticated claims carried by a genesis attestation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenesisAttestation {
    /// The account the OPAQUE session authenticated (raw 16-byte id). Carried so
    /// the client can bind it; the transparency gate never uses it (it never
    /// learns `account_id`).
    pub account_id: [u8; ACCOUNT_LEN],
    /// The transparency `username_hash` bridging the attestation to the KT-log
    /// leaf without exposing `account_id`.
    pub username_hash: [u8; USERNAME_HASH_LEN],
    /// The client-generated genesis identity pubkey (Ed25519) being attested.
    pub pubkey: [u8; PUBKEY_LEN],
    /// Absolute expiry, unix seconds. Bounds the mint→append replay window; the
    /// client (holding a permanent artifact) does not enforce it.
    pub expires_at_unix_secs: i64,
}

impl GenesisAttestation {
    /// Construct claims to be signed.
    #[must_use]
    pub fn new(
        account_id: [u8; ACCOUNT_LEN],
        username_hash: [u8; USERNAME_HASH_LEN],
        pubkey: [u8; PUBKEY_LEN],
        expires_at_unix_secs: i64,
    ) -> Self {
        Self {
            account_id,
            username_hash,
            pubkey,
            expires_at_unix_secs,
        }
    }

    /// The exact bytes the signature covers (domain-separated).
    fn signing_input(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(
            DOMAIN.len() + 1 + ACCOUNT_LEN + USERNAME_HASH_LEN + PUBKEY_LEN + EXPIRES_LEN,
        );
        buf.extend_from_slice(DOMAIN);
        buf.push(VERSION);
        buf.extend_from_slice(&self.account_id);
        buf.extend_from_slice(&self.username_hash);
        buf.extend_from_slice(&self.pubkey);
        buf.extend_from_slice(&self.expires_at_unix_secs.to_be_bytes());
        buf
    }

    /// Serialize + sign these claims into the [`GENESIS_ATTESTATION_LEN`]-byte
    /// wire form. Called by `services/auth` on `POST /auth/genesis/attest`.
    #[must_use]
    pub fn sign(&self, signing: &SigningKey) -> [u8; GENESIS_ATTESTATION_LEN] {
        let sig = signing.sign(&self.signing_input());
        let mut out = [0u8; GENESIS_ATTESTATION_LEN];
        out[0] = VERSION;
        let mut off = 1;
        out[off..off + ACCOUNT_LEN].copy_from_slice(&self.account_id);
        off += ACCOUNT_LEN;
        out[off..off + USERNAME_HASH_LEN].copy_from_slice(&self.username_hash);
        off += USERNAME_HASH_LEN;
        out[off..off + PUBKEY_LEN].copy_from_slice(&self.pubkey);
        off += PUBKEY_LEN;
        out[off..off + EXPIRES_LEN].copy_from_slice(&self.expires_at_unix_secs.to_be_bytes());
        off += EXPIRES_LEN;
        out[off..off + SIGNATURE_LEN].copy_from_slice(&sig.to_bytes());
        out
    }

    /// Parse the wire form and verify the Ed25519 signature against `verifying`.
    /// Returns the authenticated claims.
    ///
    /// This does **not** check the account / username / pubkey binding or
    /// freshness — callers should use [`verify_genesis_binding`] (client, no
    /// freshness) or [`verify_genesis_fresh`] (transparency gate, with freshness).
    ///
    /// # Errors
    ///
    /// [`GenesisAttestError::BadLength`] / [`GenesisAttestError::BadVersion`] on a
    /// malformed envelope; [`GenesisAttestError::BadSignature`] if the signature
    /// is malformed or does not verify.
    pub fn open(bytes: &[u8], verifying: &VerifyingKey) -> Result<Self, GenesisAttestError> {
        if bytes.len() != GENESIS_ATTESTATION_LEN {
            return Err(GenesisAttestError::BadLength { got: bytes.len() });
        }
        if bytes[0] != VERSION {
            return Err(GenesisAttestError::BadVersion(bytes[0]));
        }
        let mut off = 1;
        let mut account_id = [0u8; ACCOUNT_LEN];
        account_id.copy_from_slice(&bytes[off..off + ACCOUNT_LEN]);
        off += ACCOUNT_LEN;
        let mut username_hash = [0u8; USERNAME_HASH_LEN];
        username_hash.copy_from_slice(&bytes[off..off + USERNAME_HASH_LEN]);
        off += USERNAME_HASH_LEN;
        let mut pubkey = [0u8; PUBKEY_LEN];
        pubkey.copy_from_slice(&bytes[off..off + PUBKEY_LEN]);
        off += PUBKEY_LEN;
        let mut exp = [0u8; EXPIRES_LEN];
        exp.copy_from_slice(&bytes[off..off + EXPIRES_LEN]);
        off += EXPIRES_LEN;
        let expires_at_unix_secs = i64::from_be_bytes(exp);

        let sig = Signature::from_bytes(&bytes[off..off + SIGNATURE_LEN])
            .map_err(|_| GenesisAttestError::BadSignature)?;
        let claims = Self {
            account_id,
            username_hash,
            pubkey,
            expires_at_unix_secs,
        };
        verifying
            .verify(&claims.signing_input(), &sig)
            .map_err(|_| GenesisAttestError::BadSignature)?;
        Ok(claims)
    }

    /// `true` when `now_unix_secs` is strictly past the expiry.
    #[must_use]
    pub fn is_expired(&self, now_unix_secs: i64) -> bool {
        now_unix_secs > self.expires_at_unix_secs
    }
}

/// Verify a genesis attestation's signature and full account / username / pubkey
/// binding, **without** a freshness check. This is the entry point the **client**
/// uses on a first observation: it knows the target's `account_id` (from the
/// `username → account_id` discovery lookup), recomputes `username_hash`, and
/// checks all four bindings. The attestation is a permanent historical artifact,
/// so its expiry is deliberately not enforced here (ADR-0036 §1).
///
/// # Errors
///
/// Propagates [`GenesisAttestation::open`] errors, then
/// [`GenesisAttestError::AccountMismatch`] /
/// [`GenesisAttestError::UsernameMismatch`] /
/// [`GenesisAttestError::PubkeyMismatch`] if the (now-authenticated) claims do
/// not match what the caller expects.
pub fn verify_genesis_binding(
    bytes: &[u8],
    verifying: &VerifyingKey,
    expected_account: &[u8; ACCOUNT_LEN],
    expected_username_hash: &[u8; USERNAME_HASH_LEN],
    expected_pubkey: &[u8; PUBKEY_LEN],
) -> Result<GenesisAttestation, GenesisAttestError> {
    let att = GenesisAttestation::open(bytes, verifying)?;
    if &att.account_id != expected_account {
        return Err(GenesisAttestError::AccountMismatch);
    }
    if &att.username_hash != expected_username_hash {
        return Err(GenesisAttestError::UsernameMismatch);
    }
    if &att.pubkey != expected_pubkey {
        return Err(GenesisAttestError::PubkeyMismatch);
    }
    Ok(att)
}

/// Verify a genesis attestation's signature, the binding on the fields
/// transparency has (`username_hash` + `pubkey` — it never learns `account_id`),
/// and freshness. This is the entry point the **transparency append gate** uses:
/// the expiry bounds the mint→append replay window (ADR-0036 §1 / §4).
///
/// # Errors
///
/// Propagates [`GenesisAttestation::open`] errors, then
/// [`GenesisAttestError::UsernameMismatch`] /
/// [`GenesisAttestError::PubkeyMismatch`] if the authenticated claims do not
/// match the leaf, or [`GenesisAttestError::Expired`] if the attestation has
/// lapsed relative to `now_unix_secs`.
pub fn verify_genesis_fresh(
    bytes: &[u8],
    verifying: &VerifyingKey,
    expected_username_hash: &[u8; USERNAME_HASH_LEN],
    expected_pubkey: &[u8; PUBKEY_LEN],
    now_unix_secs: i64,
) -> Result<GenesisAttestation, GenesisAttestError> {
    let att = GenesisAttestation::open(bytes, verifying)?;
    if &att.username_hash != expected_username_hash {
        return Err(GenesisAttestError::UsernameMismatch);
    }
    if &att.pubkey != expected_pubkey {
        return Err(GenesisAttestError::PubkeyMismatch);
    }
    if att.is_expired(now_unix_secs) {
        return Err(GenesisAttestError::Expired);
    }
    Ok(att)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ActionAttestation;

    fn key() -> SigningKey {
        SigningKey::from_seed(&[0x11; 32])
    }

    fn sample() -> GenesisAttestation {
        GenesisAttestation::new([0xAA; 16], [0xBB; 32], [0xCC; 32], 1_000)
    }

    #[test]
    fn sign_then_verify_binding_roundtrips() {
        let sk = key();
        let att = sample();
        let wire = att.sign(&sk);
        assert_eq!(wire.len(), GENESIS_ATTESTATION_LEN);
        assert_eq!(GENESIS_ATTESTATION_LEN, 153);
        let got = verify_genesis_binding(
            &wire,
            &sk.verifying_key(),
            &[0xAA; 16],
            &[0xBB; 32],
            &[0xCC; 32],
        )
        .expect("valid genesis attestation verifies");
        assert_eq!(got, att);
    }

    #[test]
    fn tampering_any_field_breaks_the_signature() {
        let sk = key();
        let wire = sample().sign(&sk);
        let vk = sk.verifying_key();
        // Flip one byte in each region (account, username_hash, pubkey, expiry,
        // sig) and confirm the signature no longer verifies.
        for idx in [1usize, 20, 55, 82, GENESIS_ATTESTATION_LEN - 1] {
            let mut bad = wire;
            bad[idx] ^= 0x01;
            assert_eq!(
                GenesisAttestation::open(&bad, &vk),
                Err(GenesisAttestError::BadSignature),
                "tampering byte {idx} must invalidate the signature"
            );
        }
    }

    #[test]
    fn wrong_key_is_rejected() {
        let wire = sample().sign(&key());
        let other = SigningKey::from_seed(&[0x22; 32]).verifying_key();
        assert_eq!(
            GenesisAttestation::open(&wire, &other),
            Err(GenesisAttestError::BadSignature)
        );
    }

    #[test]
    fn account_username_and_pubkey_binding_is_enforced() {
        let sk = key();
        let wire = sample().sign(&sk);
        let vk = sk.verifying_key();
        assert_eq!(
            verify_genesis_binding(&wire, &vk, &[0x00; 16], &[0xBB; 32], &[0xCC; 32]),
            Err(GenesisAttestError::AccountMismatch)
        );
        assert_eq!(
            verify_genesis_binding(&wire, &vk, &[0xAA; 16], &[0x00; 32], &[0xCC; 32]),
            Err(GenesisAttestError::UsernameMismatch)
        );
        assert_eq!(
            verify_genesis_binding(&wire, &vk, &[0xAA; 16], &[0xBB; 32], &[0x00; 32]),
            Err(GenesisAttestError::PubkeyMismatch)
        );
    }

    #[test]
    fn freshness_is_enforced_only_by_verify_genesis_fresh() {
        let sk = key();
        let wire = sample().sign(&sk); // expires_at = 1000
        let vk = sk.verifying_key();
        // `verify_genesis_fresh` binds username_hash + pubkey (NOT account_id,
        // which transparency never learns) and enforces the expiry.
        assert!(verify_genesis_fresh(&wire, &vk, &[0xBB; 32], &[0xCC; 32], 1_000).is_ok());
        assert_eq!(
            verify_genesis_fresh(&wire, &vk, &[0xBB; 32], &[0xCC; 32], 1_001),
            Err(GenesisAttestError::Expired)
        );
        // `verify_genesis_binding` (the client path) ignores the expiry — a
        // long-past `now` still verifies because it is a permanent artifact.
        assert!(
            verify_genesis_binding(&wire, &vk, &[0xAA; 16], &[0xBB; 32], &[0xCC; 32]).is_ok(),
            "the client binding check must not enforce freshness"
        );
    }

    #[test]
    fn malformed_envelope_is_rejected() {
        let sk = key();
        let vk = sk.verifying_key();
        assert_eq!(
            GenesisAttestation::open(&[0u8; 10], &vk),
            Err(GenesisAttestError::BadLength { got: 10 })
        );
        // An ActionAttestation-length (121-byte) blob is not a genesis blob.
        assert_eq!(
            GenesisAttestation::open(&[0u8; 121], &vk),
            Err(GenesisAttestError::BadLength { got: 121 })
        );
        let mut wire = sample().sign(&sk);
        wire[0] = 9; // bad version
        assert_eq!(
            GenesisAttestation::open(&wire, &vk),
            Err(GenesisAttestError::BadVersion(9))
        );
    }

    #[test]
    fn distinct_pubkeys_do_not_cross_verify() {
        // A genesis attestation minted for pubkey X must not satisfy a check for
        // pubkey Y — the core property that binds an account to its genesis key.
        let sk = key();
        let vk = sk.verifying_key();
        let wire_x = GenesisAttestation::new([0xAA; 16], [0xBB; 32], [0x01; 32], 1_000).sign(&sk);
        assert_eq!(
            verify_genesis_binding(&wire_x, &vk, &[0xAA; 16], &[0xBB; 32], &[0x02; 32]),
            Err(GenesisAttestError::PubkeyMismatch)
        );
    }

    #[test]
    fn domains_do_not_cross_between_action_and_genesis() {
        // Domain separation is load-bearing: even under a SHARED signing key
        // (ADR-0036 §3), an `ActionAttestation` and a `GenesisAttestation` must
        // be cryptographically non-interchangeable. Two mechanisms enforce it —
        // (a) distinct total lengths, and (b) distinct domain-prefixed signing
        // inputs — and this proves BOTH.
        let sk = key();
        let vk = sk.verifying_key();

        // (a) Length: neither parser accepts the other's wire blob.
        let genesis_wire = sample().sign(&sk); // 153 bytes
        let action_wire = ActionAttestation::new([0xAA; 16], [0xBB; 32], 1_000).sign(&sk); // 121 bytes
        assert!(
            matches!(
                crate::ActionAttestation::open(&genesis_wire, &vk),
                Err(crate::AttestError::BadLength { .. })
            ),
            "a genesis blob must not parse as an action attestation"
        );
        assert!(
            matches!(
                GenesisAttestation::open(&action_wire, &vk),
                Err(GenesisAttestError::BadLength { .. })
            ),
            "an action blob must not parse as a genesis attestation"
        );

        // (b) Domain: splice the action attestation's 64-byte signature into a
        // genesis-shaped buffer. The signature was produced over the
        // `lakko/device-attest/v1` signing input, so it must NOT verify under the
        // genesis (`lakko/genesis-attest/v1`) signing input — proving the domain
        // prefix, not merely the length, keeps the two signatures apart.
        let action_sig = &action_wire[action_wire.len() - SIGNATURE_LEN..];
        let mut spliced = sample().sign(&sk);
        spliced[GENESIS_ATTESTATION_LEN - SIGNATURE_LEN..].copy_from_slice(action_sig);
        assert_eq!(
            GenesisAttestation::open(&spliced, &vk),
            Err(GenesisAttestError::BadSignature),
            "an action-domain signature must not verify as a genesis signature"
        );

        // And the reverse: a genesis signature spliced into an action-shaped
        // buffer must not verify under the action signing input.
        let genesis_sig = &genesis_wire[genesis_wire.len() - SIGNATURE_LEN..];
        let mut action_spliced = ActionAttestation::new([0xAA; 16], [0xBB; 32], 1_000).sign(&sk);
        let action_len = action_spliced.len();
        action_spliced[action_len - SIGNATURE_LEN..].copy_from_slice(genesis_sig);
        assert_eq!(
            crate::ActionAttestation::open(&action_spliced, &vk),
            Err(crate::AttestError::BadSignature),
            "a genesis-domain signature must not verify as an action signature"
        );
    }
}
