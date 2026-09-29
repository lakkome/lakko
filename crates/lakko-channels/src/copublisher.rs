// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Owner-appointed co-publisher keys ([ADR-0059] section 1, decided
//! 2026-09-29).
//!
//! A co-publisher (ADR-0028 OQ3) cannot derive the channel's owner key, yet
//! its rotation fanout needs the subscriber list the owner key gates. So each
//! co-publisher has a key of its own, which the owner appoints on
//! `services/channels`; a request signed by an appointed key is accepted
//! where the rotation fanout needs it and nowhere else.
//!
//! ## The key
//!
//! ```text
//! copublisher_seed = BLAKE3-derive-key("lakko/channels/copublisher/v1", account_seed || channel_id)
//! copublisher_key  = Ed25519 from copublisher_seed
//! ```
//!
//! The co-publisher's own account seed, so a second device of the same
//! account derives the same key and one key serves one (account, channel)
//! pair. A separate label from the owner key: the owner and a co-publisher
//! never collide even for the same account.
//!
//! ## The binding
//!
//! The owner learns the key from the co-publisher, signed by the
//! co-publisher's **account identity key** - the key the owner already
//! authenticated through MLS, since it is the leaf signature key of the
//! external-join proposal it approves:
//!
//! ```text
//! domain "lakko/channels/copublisher-binding/v1\0" (38) || version 0x01
//!   || channel_id(16) || account_pubkey(32) || copublisher_pubkey(32) || sig(64)
//! sig = Ed25519_account(domain || version || channel_id || account_pubkey || copublisher_pubkey)
//! ```
//!
//! 183 bytes, fixed. It travels as one more payload of the joiner's knock
//! (ADR-0053), beside the proposal and the attribution. The carrier is the
//! server; the authenticity is not: the owner accepts a binding only when its
//! `account_pubkey` equals the joiner key it approved, so neither the DS nor
//! anyone who can knock can substitute a key.
//!
//! ## The request proof
//!
//! The owner-proof shape under its own domain, with the signer's key in the
//! header so the server looks up one key instead of trying every appointed
//! one:
//!
//! ```text
//! X-Lakko-Channel-Copublisher-Proof: v1.<copublisher_pubkey_b64url>.<unix_ms>.<nonce_b64url>.<sig_b64url>
//! canonical bytes = the owner proof's, with domain "lakko/channels/copublisher-proof/v1"
//! ```
//!
//! The server accepts it only if the key is appointed for the channel named
//! in the path, and only on the routes the fanout needs.
//!
//! [ADR-0059]: ../../../docs/adr/0059-channel-discover.md

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_crypto::hash;
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey};
use thiserror::Error;

use crate::owner::{
    proof_canonical_bytes, OwnerProof, OwnerProofError, OwnerRequest, ACCOUNT_SEED_LEN,
    OWNER_PROOF_NONCE_LEN, OWNER_PROOF_WINDOW_MS, VERSION,
};
use crate::CHANNEL_ID_LEN;

/// BLAKE3 derive-key context of the co-publisher seed. Frozen.
pub const COPUBLISHER_KEY_DOMAIN: &str = "lakko/channels/copublisher/v1";
/// Domain separator that opens the co-publisher proof's canonical bytes.
/// Frozen.
pub const COPUBLISHER_PROOF_DOMAIN: &[u8] = b"lakko/channels/copublisher-proof/v1";
/// The request header the co-publisher proof travels in (lowercase).
pub const COPUBLISHER_PROOF_HEADER_NAME: &str = "x-lakko-channel-copublisher-proof";
/// Upper bound on the header's length; anything longer is refused unparsed.
pub const COPUBLISHER_PROOF_MAX_HEADER_LEN: usize = 256;
/// How many co-publisher keys one channel may have appointed (chosen, not
/// measured).
pub const MAX_COPUBLISHERS: usize = 32;

/// Domain separator that opens a binding. Frozen.
pub const COPUBLISHER_BINDING_DOMAIN_V1: &[u8] = b"lakko/channels/copublisher-binding/v1\0";
/// The binding format version.
pub const COPUBLISHER_BINDING_VERSION_V1: u8 = 0x01;
/// The fixed length of a binding.
pub const COPUBLISHER_BINDING_LEN: usize =
    COPUBLISHER_BINDING_DOMAIN_V1.len() + 1 + CHANNEL_ID_LEN + 32 + 32 + 64;

const PUBKEY_LEN: usize = 32;

/// Derive the co-publisher seed of `channel_id` from the co-publisher's own
/// account seed.
#[must_use]
pub fn channel_copublisher_seed(
    account_seed: &[u8; ACCOUNT_SEED_LEN],
    channel_id: &[u8; CHANNEL_ID_LEN],
) -> [u8; 32] {
    let mut material = [0u8; ACCOUNT_SEED_LEN + CHANNEL_ID_LEN];
    material[..ACCOUNT_SEED_LEN].copy_from_slice(account_seed);
    material[ACCOUNT_SEED_LEN..].copy_from_slice(channel_id);
    let out = hash::blake3_derive_key_from_material(COPUBLISHER_KEY_DOMAIN, &material);
    material.fill(0);
    out
}

/// A parsed co-publisher proof: the signer's key and the owner-proof fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoPublisherProof {
    /// The co-publisher key that signed; the server checks it is appointed.
    pub copublisher_key: [u8; PUBKEY_LEN],
    /// Time, nonce and signature, exactly as in an owner proof.
    pub proof: OwnerProof,
}

impl CoPublisherProof {
    /// The header value.
    #[must_use]
    pub fn encode(&self) -> String {
        format!(
            "{VERSION}.{}.{}.{}.{}",
            URL_SAFE_NO_PAD.encode(self.copublisher_key),
            self.proof.unix_ms,
            URL_SAFE_NO_PAD.encode(self.proof.nonce),
            URL_SAFE_NO_PAD.encode(self.proof.signature),
        )
    }

    /// Parse a header value. Bounded and fail-closed: five dot-separated
    /// fields and nothing else.
    ///
    /// # Errors
    ///
    /// [`OwnerProofError::UnknownVersion`] or [`OwnerProofError::Malformed`].
    pub fn parse(value: &str) -> Result<Self, OwnerProofError> {
        if value.len() > COPUBLISHER_PROOF_MAX_HEADER_LEN {
            return Err(OwnerProofError::Malformed);
        }
        let mut parts = value.split('.');
        let version = parts.next().ok_or(OwnerProofError::Malformed)?;
        let key = parts.next().ok_or(OwnerProofError::Malformed)?;
        let unix_ms = parts.next().ok_or(OwnerProofError::Malformed)?;
        let nonce = parts.next().ok_or(OwnerProofError::Malformed)?;
        let signature = parts.next().ok_or(OwnerProofError::Malformed)?;
        if parts.next().is_some() {
            return Err(OwnerProofError::Malformed);
        }
        if version != VERSION {
            return Err(OwnerProofError::UnknownVersion);
        }
        let copublisher_key: [u8; PUBKEY_LEN] = URL_SAFE_NO_PAD
            .decode(key)
            .map_err(|_| OwnerProofError::Malformed)?
            .as_slice()
            .try_into()
            .map_err(|_| OwnerProofError::Malformed)?;
        Ok(Self {
            copublisher_key,
            proof: OwnerProof::from_fields(unix_ms, nonce, signature)?,
        })
    }
}

/// Verify `proof` over `req` under the co-publisher key it names, with
/// `now_ms` the verifier's clock. Whether that key is appointed for the
/// channel is the server's (state); this function is pure.
///
/// # Errors
///
/// [`OwnerProofError::OutsideWindow`] or [`OwnerProofError::BadSignature`]
/// (a key that is not a curve point included).
pub fn verify_copublisher_proof(
    proof: &CoPublisherProof,
    req: &OwnerRequest<'_>,
    now_ms: u64,
) -> Result<(), OwnerProofError> {
    if now_ms.abs_diff(proof.proof.unix_ms) > OWNER_PROOF_WINDOW_MS {
        return Err(OwnerProofError::OutsideWindow);
    }
    let key = VerifyingKey::from_bytes(&proof.copublisher_key)
        .map_err(|_| OwnerProofError::BadSignature)?;
    let bytes = proof_canonical_bytes(
        COPUBLISHER_PROOF_DOMAIN,
        req,
        proof.proof.unix_ms,
        &proof.proof.nonce,
    );
    let signature =
        Signature::from_bytes(&proof.proof.signature).map_err(|_| OwnerProofError::BadSignature)?;
    key.verify(&bytes, &signature)
        .map_err(|_| OwnerProofError::BadSignature)
}

/// The co-publisher-side signer of one channel. Clock and nonce are the
/// caller's.
pub struct ChannelCoPublisherSigner {
    key: SigningKey,
}

impl core::fmt::Debug for ChannelCoPublisherSigner {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ChannelCoPublisherSigner")
            .field("verifying_key", &self.key.verifying_key().to_bytes())
            .finish_non_exhaustive()
    }
}

impl ChannelCoPublisherSigner {
    /// The co-publisher signer of `channel_id` for the account whose identity
    /// seed is `account_seed`.
    #[must_use]
    pub fn from_account_seed(
        account_seed: &[u8; ACCOUNT_SEED_LEN],
        channel_id: &[u8; CHANNEL_ID_LEN],
    ) -> Self {
        let mut seed = channel_copublisher_seed(account_seed, channel_id);
        let key = SigningKey::from_seed(&seed);
        seed.fill(0);
        Self { key }
    }

    /// The public key the owner appoints.
    #[must_use]
    pub fn verifying_key(&self) -> VerifyingKey {
        self.key.verifying_key()
    }

    /// Sign `req` at `now_ms` with `nonce`.
    #[must_use]
    pub fn sign_at(
        &self,
        req: &OwnerRequest<'_>,
        now_ms: u64,
        nonce: [u8; OWNER_PROOF_NONCE_LEN],
    ) -> CoPublisherProof {
        let bytes = proof_canonical_bytes(COPUBLISHER_PROOF_DOMAIN, req, now_ms, &nonce);
        CoPublisherProof {
            copublisher_key: self.key.verifying_key().to_bytes(),
            proof: OwnerProof {
                unix_ms: now_ms,
                nonce,
                signature: self.key.sign(&bytes).to_bytes(),
            },
        }
    }
}

/// A verified binding of a co-publisher key to an account identity key for
/// one channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoPublisherBinding {
    /// The channel the key is for.
    pub channel_id: [u8; CHANNEL_ID_LEN],
    /// The co-publisher's account identity key, which signed the binding.
    pub account_pubkey: [u8; PUBKEY_LEN],
    /// The co-publisher key to appoint.
    pub copublisher_pubkey: [u8; PUBKEY_LEN],
}

/// Why a binding was refused. Codes never echo the input.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum CoPublisherBindingError {
    /// Not a binding (another domain).
    #[error("not a co-publisher binding")]
    WrongDomain,
    /// A binding of the wrong length.
    #[error("co-publisher binding has the wrong length")]
    BadLength,
    /// A version this crate does not speak.
    #[error("co-publisher binding version unknown")]
    UnknownVersion,
    /// A key that is not a curve point, or a signature that does not verify
    /// under the embedded account key.
    #[error("co-publisher binding signature invalid")]
    BadSignature,
}

fn binding_signed_prefix(
    channel_id: &[u8; CHANNEL_ID_LEN],
    account_pubkey: &[u8; PUBKEY_LEN],
    copublisher_pubkey: &[u8; PUBKEY_LEN],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(COPUBLISHER_BINDING_LEN);
    out.extend_from_slice(COPUBLISHER_BINDING_DOMAIN_V1);
    out.push(COPUBLISHER_BINDING_VERSION_V1);
    out.extend_from_slice(channel_id);
    out.extend_from_slice(account_pubkey);
    out.extend_from_slice(copublisher_pubkey);
    out
}

/// Compose the binding: the co-publisher key of `channel_id` for the account
/// whose identity seed is `account_seed`, signed by that account's identity
/// key.
#[must_use]
pub fn compose_copublisher_binding(
    account_seed: &[u8; ACCOUNT_SEED_LEN],
    channel_id: &[u8; CHANNEL_ID_LEN],
) -> Vec<u8> {
    let account = SigningKey::from_seed(account_seed);
    let account_pubkey = account.verifying_key().to_bytes();
    let copublisher_pubkey = ChannelCoPublisherSigner::from_account_seed(account_seed, channel_id)
        .verifying_key()
        .to_bytes();
    let mut out = binding_signed_prefix(channel_id, &account_pubkey, &copublisher_pubkey);
    let sig = account.sign(&out).to_bytes();
    out.extend_from_slice(&sig);
    out
}

/// Decode and verify a binding. The signature is checked against the
/// embedded account key; whether that key is the joiner the caller approved,
/// and whether the channel is the caller's, is the caller's check.
///
/// # Errors
///
/// A [`CoPublisherBindingError`]; checked in the order domain, length,
/// version, signature.
pub fn decode_copublisher_binding(
    bytes: &[u8],
) -> Result<CoPublisherBinding, CoPublisherBindingError> {
    let Some(rest) = bytes.strip_prefix(COPUBLISHER_BINDING_DOMAIN_V1) else {
        return Err(CoPublisherBindingError::WrongDomain);
    };
    if bytes.len() != COPUBLISHER_BINDING_LEN {
        return Err(CoPublisherBindingError::BadLength);
    }
    let (version, rest) = rest
        .split_first()
        .ok_or(CoPublisherBindingError::BadLength)?;
    if *version != COPUBLISHER_BINDING_VERSION_V1 {
        return Err(CoPublisherBindingError::UnknownVersion);
    }
    let (channel_id, rest) = rest.split_at(CHANNEL_ID_LEN);
    let (account, rest) = rest.split_at(PUBKEY_LEN);
    let (copublisher, sig) = rest.split_at(PUBKEY_LEN);
    let channel_id: [u8; CHANNEL_ID_LEN] = channel_id
        .try_into()
        .map_err(|_| CoPublisherBindingError::BadLength)?;
    let account_pubkey: [u8; PUBKEY_LEN] = account
        .try_into()
        .map_err(|_| CoPublisherBindingError::BadLength)?;
    let copublisher_pubkey: [u8; PUBKEY_LEN] = copublisher
        .try_into()
        .map_err(|_| CoPublisherBindingError::BadLength)?;
    VerifyingKey::from_bytes(&copublisher_pubkey)
        .map_err(|_| CoPublisherBindingError::BadSignature)?;
    let account_key = VerifyingKey::from_bytes(&account_pubkey)
        .map_err(|_| CoPublisherBindingError::BadSignature)?;
    let signature =
        Signature::from_bytes(sig).map_err(|_| CoPublisherBindingError::BadSignature)?;
    let signed = binding_signed_prefix(&channel_id, &account_pubkey, &copublisher_pubkey);
    account_key
        .verify(&signed, &signature)
        .map_err(|_| CoPublisherBindingError::BadSignature)?;
    Ok(CoPublisherBinding {
        channel_id,
        account_pubkey,
        copublisher_pubkey,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::owner::{channel_owner_seed, verify_owner_proof, ChannelOwnerSigner};

    fn to_hex(bytes: &[u8]) -> String {
        use core::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
    }

    fn req(body: &[u8]) -> OwnerRequest<'_> {
        OwnerRequest {
            method: "GET",
            canonical_path: "/v1/channels/00112233445566778899aabbccddeeff/subscribers",
            query: "",
            body,
        }
    }

    /// Computed by this implementation; pins the derivation (label + input
    /// order) so a drift fails here instead of silently orphaning every
    /// appointed key.
    const GOLDEN_COPUBLISHER_SEED: &str =
        "620c5ef8f46b3708dca4ac798f5779a2d38270d5870bd47e735f8f519bc9260c";

    #[test]
    fn golden_pins_the_copublisher_seed() {
        let seed = channel_copublisher_seed(&[0x01; 32], &[0x02; 16]);
        assert_eq!(to_hex(&seed), GOLDEN_COPUBLISHER_SEED);
        assert_ne!(seed, channel_owner_seed(&[0x01; 32], &[0x02; 16]));
        assert_ne!(seed, channel_copublisher_seed(&[0x01; 32], &[0x03; 16]));
        assert_ne!(seed, channel_copublisher_seed(&[0x04; 32], &[0x02; 16]));
    }

    #[test]
    fn sign_encode_parse_verify_round_trips() {
        let signer = ChannelCoPublisherSigner::from_account_seed(&[7; 32], &[9; 16]);
        let proof = signer.sign_at(&req(b""), 1_000_000, [3; 16]);
        let header = proof.encode();
        assert!(header.len() <= COPUBLISHER_PROOF_MAX_HEADER_LEN);
        let parsed = CoPublisherProof::parse(&header).unwrap();
        assert_eq!(parsed, proof);
        assert_eq!(
            parsed.copublisher_key,
            signer.verifying_key().to_bytes(),
            "the header names the signing key"
        );
        verify_copublisher_proof(&parsed, &req(b""), 1_000_000).unwrap();
    }

    #[test]
    fn verify_refuses_other_body_path_time_and_a_swapped_key() {
        let signer = ChannelCoPublisherSigner::from_account_seed(&[7; 32], &[9; 16]);
        let other = ChannelCoPublisherSigner::from_account_seed(&[8; 32], &[9; 16]);
        let proof = signer.sign_at(&req(b"a"), 1_000_000, [3; 16]);
        assert_eq!(
            verify_copublisher_proof(&proof, &req(b"b"), 1_000_000),
            Err(OwnerProofError::BadSignature)
        );
        let mut moved = req(b"a");
        moved.canonical_path = "/v1/channels/ffeeddccbbaa99887766554433221100/subscribers";
        assert_eq!(
            verify_copublisher_proof(&proof, &moved, 1_000_000),
            Err(OwnerProofError::BadSignature)
        );
        let mut swapped = proof.clone();
        swapped.copublisher_key = other.verifying_key().to_bytes();
        assert_eq!(
            verify_copublisher_proof(&swapped, &req(b"a"), 1_000_000),
            Err(OwnerProofError::BadSignature)
        );
        assert_eq!(
            verify_copublisher_proof(&proof, &req(b"a"), 1_000_000 + OWNER_PROOF_WINDOW_MS + 1),
            Err(OwnerProofError::OutsideWindow)
        );
    }

    #[test]
    fn owner_and_copublisher_proofs_never_cross() {
        // The same seed used as both an owner key and a co-publisher key:
        // the domains differ, so neither proof verifies as the other.
        let seed = [5u8; 32];
        let as_owner = ChannelOwnerSigner::from_account_seed(&seed, &[9; 16]);
        let key = SigningKey::from_seed(&channel_owner_seed(&seed, &[9; 16]));
        let owner_proof = as_owner.sign_at(&req(b""), 10, [1; 16]);
        let forged = CoPublisherProof {
            copublisher_key: key.verifying_key().to_bytes(),
            proof: owner_proof.clone(),
        };
        assert_eq!(
            verify_copublisher_proof(&forged, &req(b""), 10),
            Err(OwnerProofError::BadSignature)
        );
        let bytes = proof_canonical_bytes(COPUBLISHER_PROOF_DOMAIN, &req(b""), 10, &[1; 16]);
        let copub_shaped = OwnerProof {
            unix_ms: 10,
            nonce: [1; 16],
            signature: key.sign(&bytes).to_bytes(),
        };
        assert_eq!(
            verify_owner_proof(&copub_shaped, &req(b""), &key.verifying_key(), 10),
            Err(OwnerProofError::BadSignature)
        );
        verify_owner_proof(&owner_proof, &req(b""), &key.verifying_key(), 10).unwrap();
    }

    #[test]
    fn parse_is_fail_closed() {
        let signer = ChannelCoPublisherSigner::from_account_seed(&[7; 32], &[9; 16]);
        let good = signer.sign_at(&req(b""), 5, [1; 16]).encode();
        assert_eq!(
            CoPublisherProof::parse(&good.replacen("v1", "v2", 1)),
            Err(OwnerProofError::UnknownVersion)
        );
        let owner = ChannelOwnerSigner::from_account_seed(&[7; 32], &[9; 16])
            .sign_at(&req(b""), 5, [1; 16])
            .encode();
        for bad in [
            String::new(),
            "v1".to_string(),
            owner,
            format!("{good}.x"),
            good.replacen(".5.", ".+5.", 1),
            "x".repeat(COPUBLISHER_PROOF_MAX_HEADER_LEN + 1),
        ] {
            assert_eq!(
                CoPublisherProof::parse(&bad),
                Err(OwnerProofError::Malformed),
                "{bad}"
            );
        }
    }

    #[test]
    fn binding_round_trips_and_names_the_account_key() {
        let seed = [11u8; 32];
        let wire = compose_copublisher_binding(&seed, &[9; 16]);
        assert_eq!(wire.len(), COPUBLISHER_BINDING_LEN);
        assert_eq!(COPUBLISHER_BINDING_LEN, 183);
        let b = decode_copublisher_binding(&wire).unwrap();
        assert_eq!(b.channel_id, [9; 16]);
        assert_eq!(
            b.account_pubkey,
            SigningKey::from_seed(&seed).verifying_key().to_bytes()
        );
        assert_eq!(
            b.copublisher_pubkey,
            ChannelCoPublisherSigner::from_account_seed(&seed, &[9; 16])
                .verifying_key()
                .to_bytes()
        );
    }

    #[test]
    fn binding_refuses_tampering_in_order() {
        let wire = compose_copublisher_binding(&[11; 32], &[9; 16]);
        assert_eq!(
            decode_copublisher_binding(b"lakko/something-else"),
            Err(CoPublisherBindingError::WrongDomain)
        );
        assert_eq!(
            decode_copublisher_binding(&wire[..wire.len() - 1]),
            Err(CoPublisherBindingError::BadLength)
        );
        let d = COPUBLISHER_BINDING_DOMAIN_V1.len();
        let mut v = wire.clone();
        v[d] = 2;
        assert_eq!(
            decode_copublisher_binding(&v),
            Err(CoPublisherBindingError::UnknownVersion)
        );
        // Any flipped byte after the version fails the signature: the
        // channel, either key, or the signature itself.
        for i in (d + 1)..wire.len() {
            let mut t = wire.clone();
            t[i] ^= 0x01;
            assert_eq!(
                decode_copublisher_binding(&t),
                Err(CoPublisherBindingError::BadSignature),
                "byte {i}"
            );
        }
        // A binding re-signed by another account for the victim's key still
        // names the signer, never the victim.
        let other = compose_copublisher_binding(&[12; 32], &[9; 16]);
        let b = decode_copublisher_binding(&other).unwrap();
        assert_ne!(
            b.account_pubkey,
            decode_copublisher_binding(&wire).unwrap().account_pubkey
        );
    }
}
