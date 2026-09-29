// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The channel owner key and the owner request proof
//! ([ADR-0059 §1](../../../docs/adr/0059-channel-discover.md)).
//!
//! ## The key
//!
//! ```text
//! owner_seed = BLAKE3-derive-key("lakko/channels/owner/v1", account_seed || channel_id)
//! owner_key  = Ed25519 from owner_seed
//! ```
//!
//! `account_seed` is the 32-byte Ed25519 seed of the owner's account
//! identity (the same input ADR-0057 derives a journal root from). Only the
//! account that created the channel can derive the key; a co-publisher
//! cannot. The label is separate from ADR-0057's
//! `lakko/ds/journal-root/v1`, so one key never signs for two services.
//! The public key is registered with `services/channels` when the channel
//! is created, and it does not link the channel to the account: without the
//! account seed the two keys are unrelated.
//!
//! ## The request proof
//!
//! The ADR-0053 request-proof shape under its own domain, carried in one
//! header:
//!
//! ```text
//! X-Lakko-Channel-Owner-Proof: v1.<unix_ms>.<nonce_b64url>.<sig_b64url>
//! ```
//!
//! Canonical bytes:
//!
//! ```text
//! "lakko/channels/owner-proof/v1" || 0x00 || method || 0x00 || canonical_path || 0x00
//!   || query || 0x00 || BLAKE3(body) || unix_ms(8 BE) || nonce(16)
//! ```
//!
//! It differs from `lakko-ds-proof` in two places only: the domain, so a DS
//! proof can never be replayed here (or the reverse), and the absence of an
//! epoch, since an owner key does not rotate. The channel is bound through
//! the path (`/v1/channels/<id>/...`), which the server passes as the
//! canonical path it routed, never an nginx prefix.
//!
//! The verifier checks the window before the signature. Resolving the stored
//! owner key and refusing a replayed nonce are the server's (both are state);
//! this module is pure.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_crypto::hash;
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey};
use thiserror::Error;

use crate::CHANNEL_ID_LEN;

/// BLAKE3 derive-key context of the owner seed. Frozen.
pub const OWNER_KEY_DOMAIN: &str = "lakko/channels/owner/v1";
/// Domain separator that opens the owner proof's canonical bytes. Frozen.
pub const OWNER_PROOF_DOMAIN: &[u8] = b"lakko/channels/owner-proof/v1";
/// The request header the owner proof travels in (lowercase).
pub const OWNER_PROOF_HEADER_NAME: &str = "x-lakko-channel-owner-proof";
/// Nonce length in bytes.
pub const OWNER_PROOF_NONCE_LEN: usize = 16;
/// Ed25519 signature length in bytes.
pub const OWNER_PROOF_SIGNATURE_LEN: usize = 64;
/// How far from the server's clock a proof's `unix_ms` may lie, either way.
/// The ADR-0053 window.
pub const OWNER_PROOF_WINDOW_MS: u64 = 120_000;
/// Upper bound on the header's length; anything longer is refused unparsed.
pub const OWNER_PROOF_MAX_HEADER_LEN: usize = 192;

const ACCOUNT_SEED_LEN: usize = 32;
const VERSION: &str = "v1";
const SEP: u8 = 0;

/// Derive the owner seed of `channel_id` from the owner's account seed.
#[must_use]
pub fn channel_owner_seed(
    account_seed: &[u8; ACCOUNT_SEED_LEN],
    channel_id: &[u8; CHANNEL_ID_LEN],
) -> [u8; 32] {
    let mut material = [0u8; ACCOUNT_SEED_LEN + CHANNEL_ID_LEN];
    material[..ACCOUNT_SEED_LEN].copy_from_slice(account_seed);
    material[ACCOUNT_SEED_LEN..].copy_from_slice(channel_id);
    let out = hash::blake3_derive_key_from_material(OWNER_KEY_DOMAIN, &material);
    material.fill(0);
    out
}

/// The parts of a request the owner proof binds.
#[derive(Clone, Copy, Debug)]
pub struct OwnerRequest<'a> {
    /// HTTP method, upper case.
    pub method: &'a str,
    /// The service path, without any reverse-proxy prefix.
    pub canonical_path: &'a str,
    /// The raw query string without the `?`, or empty.
    pub query: &'a str,
    /// The request body, or empty.
    pub body: &'a [u8],
}

/// A parsed owner proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerProof {
    /// The signer's clock at signing, unix milliseconds.
    pub unix_ms: u64,
    /// Fresh per request; the server refuses a repeat within the window.
    pub nonce: [u8; OWNER_PROOF_NONCE_LEN],
    /// Ed25519 over the canonical bytes.
    pub signature: [u8; OWNER_PROOF_SIGNATURE_LEN],
}

/// Why an owner proof was refused. The server maps every variant to one
/// response, so a caller learns nothing from which one it hit.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum OwnerProofError {
    /// The header did not parse.
    #[error("owner proof header malformed")]
    Malformed,
    /// A version this crate does not speak.
    #[error("owner proof header version unknown")]
    UnknownVersion,
    /// `unix_ms` is outside the window around the verifier's clock.
    #[error("owner proof outside the time window")]
    OutsideWindow,
    /// The signature does not verify under the stored owner key.
    #[error("owner proof signature invalid")]
    BadSignature,
}

/// The bytes an owner proof signs.
#[must_use]
pub fn owner_proof_canonical_bytes(
    req: &OwnerRequest<'_>,
    unix_ms: u64,
    nonce: &[u8; OWNER_PROOF_NONCE_LEN],
) -> Vec<u8> {
    let digest = hash::blake3(req.body);
    let mut out = Vec::with_capacity(
        OWNER_PROOF_DOMAIN.len()
            + req.method.len()
            + req.canonical_path.len()
            + req.query.len()
            + digest.len()
            + 4
            + 8
            + OWNER_PROOF_NONCE_LEN,
    );
    out.extend_from_slice(OWNER_PROOF_DOMAIN);
    out.push(SEP);
    out.extend_from_slice(req.method.as_bytes());
    out.push(SEP);
    out.extend_from_slice(req.canonical_path.as_bytes());
    out.push(SEP);
    out.extend_from_slice(req.query.as_bytes());
    out.push(SEP);
    out.extend_from_slice(&digest);
    out.extend_from_slice(&unix_ms.to_be_bytes());
    out.extend_from_slice(nonce);
    out
}

/// Verify `proof` over `req` under the stored owner key, with `now_ms` the
/// verifier's clock.
///
/// # Errors
///
/// [`OwnerProofError::OutsideWindow`] or [`OwnerProofError::BadSignature`].
pub fn verify_owner_proof(
    proof: &OwnerProof,
    req: &OwnerRequest<'_>,
    owner_key: &VerifyingKey,
    now_ms: u64,
) -> Result<(), OwnerProofError> {
    if now_ms.abs_diff(proof.unix_ms) > OWNER_PROOF_WINDOW_MS {
        return Err(OwnerProofError::OutsideWindow);
    }
    let bytes = owner_proof_canonical_bytes(req, proof.unix_ms, &proof.nonce);
    let signature =
        Signature::from_bytes(&proof.signature).map_err(|_| OwnerProofError::BadSignature)?;
    owner_key
        .verify(&bytes, &signature)
        .map_err(|_| OwnerProofError::BadSignature)
}

impl OwnerProof {
    /// The header value.
    #[must_use]
    pub fn encode(&self) -> String {
        format!(
            "{VERSION}.{}.{}.{}",
            self.unix_ms,
            URL_SAFE_NO_PAD.encode(self.nonce),
            URL_SAFE_NO_PAD.encode(self.signature),
        )
    }

    /// Parse a header value. Bounded and fail-closed: four dot-separated
    /// fields, a decimal time, a 16-byte nonce and a 64-byte signature in
    /// base64url without padding, nothing else.
    ///
    /// # Errors
    ///
    /// [`OwnerProofError::UnknownVersion`] or [`OwnerProofError::Malformed`].
    pub fn parse(value: &str) -> Result<Self, OwnerProofError> {
        if value.len() > OWNER_PROOF_MAX_HEADER_LEN {
            return Err(OwnerProofError::Malformed);
        }
        let mut parts = value.split('.');
        let version = parts.next().ok_or(OwnerProofError::Malformed)?;
        let unix_ms = parts.next().ok_or(OwnerProofError::Malformed)?;
        let nonce = parts.next().ok_or(OwnerProofError::Malformed)?;
        let signature = parts.next().ok_or(OwnerProofError::Malformed)?;
        if parts.next().is_some() {
            return Err(OwnerProofError::Malformed);
        }
        if version != VERSION {
            return Err(OwnerProofError::UnknownVersion);
        }
        if unix_ms.is_empty() || unix_ms.len() > 20 || !unix_ms.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(OwnerProofError::Malformed);
        }
        let unix_ms = unix_ms
            .parse::<u64>()
            .map_err(|_| OwnerProofError::Malformed)?;
        let nonce: [u8; OWNER_PROOF_NONCE_LEN] = URL_SAFE_NO_PAD
            .decode(nonce)
            .map_err(|_| OwnerProofError::Malformed)?
            .as_slice()
            .try_into()
            .map_err(|_| OwnerProofError::Malformed)?;
        let signature: [u8; OWNER_PROOF_SIGNATURE_LEN] = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| OwnerProofError::Malformed)?
            .as_slice()
            .try_into()
            .map_err(|_| OwnerProofError::Malformed)?;
        Ok(Self {
            unix_ms,
            nonce,
            signature,
        })
    }
}

/// The owner-side signer of one channel. Clock and nonce are the caller's,
/// so one type serves the host, the browser and a native client.
pub struct ChannelOwnerSigner {
    key: SigningKey,
}

impl core::fmt::Debug for ChannelOwnerSigner {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ChannelOwnerSigner")
            .field("verifying_key", &self.key.verifying_key().to_bytes())
            .finish_non_exhaustive()
    }
}

impl ChannelOwnerSigner {
    /// The owner signer of `channel_id` for the account whose identity seed
    /// is `account_seed`.
    #[must_use]
    pub fn from_account_seed(
        account_seed: &[u8; ACCOUNT_SEED_LEN],
        channel_id: &[u8; CHANNEL_ID_LEN],
    ) -> Self {
        let mut seed = channel_owner_seed(account_seed, channel_id);
        let key = SigningKey::from_seed(&seed);
        seed.fill(0);
        Self { key }
    }

    /// The public key `services/channels` stores as `owner_pubkey`.
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
    ) -> OwnerProof {
        let bytes = owner_proof_canonical_bytes(req, now_ms, &nonce);
        OwnerProof {
            unix_ms: now_ms,
            nonce,
            signature: self.key.sign(&bytes).to_bytes(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_hex(bytes: &[u8]) -> String {
        use core::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
    }

    fn req(body: &[u8]) -> OwnerRequest<'_> {
        OwnerRequest {
            method: "PUT",
            canonical_path: "/v1/channels/00112233445566778899aabbccddeeff/listing",
            query: "",
            body,
        }
    }

    #[test]
    fn golden_pins_the_owner_seed() {
        let seed = channel_owner_seed(&[0x01; 32], &[0x02; 16]);
        assert_eq!(to_hex(&seed), GOLDEN_OWNER_SEED);
        assert_ne!(seed, channel_owner_seed(&[0x01; 32], &[0x03; 16]));
        assert_ne!(seed, channel_owner_seed(&[0x04; 32], &[0x02; 16]));
        // Never the ADR-0057 journal root for the same inputs.
        let mut material = [0u8; 48];
        material[..32].copy_from_slice(&[0x01; 32]);
        material[32..].copy_from_slice(&[0x02; 16]);
        assert_ne!(
            seed,
            hash::blake3_derive_key_from_material("lakko/ds/journal-root/v1", &material)
        );
    }

    /// Computed by this implementation; pins the derivation (label + input
    /// order) so a drift fails here instead of silently orphaning every
    /// registered owner key.
    const GOLDEN_OWNER_SEED: &str =
        "9c3cbcf2c5074209420709fb8910467202e59428c3bb7db53f53e905981ca4cd";

    #[test]
    fn golden_pins_the_canonical_bytes() {
        let bytes = owner_proof_canonical_bytes(&req(b""), 0x0102_0304_0506_0708, &[0xAA; 16]);
        assert_eq!(to_hex(&hash::blake3(&bytes)), GOLDEN_CANONICAL_BYTES_DIGEST);
        assert!(bytes.starts_with(b"lakko/channels/owner-proof/v1\x00PUT\x00/v1/channels/"));
        let expected_len = OWNER_PROOF_DOMAIN.len()
            + 1
            + 3
            + 1
            + req(b"").canonical_path.len()
            + 1
            + 1
            + 32
            + 8
            + 16;
        assert_eq!(bytes.len(), expected_len);
    }

    /// BLAKE3 of the canonical bytes, computed by this implementation.
    const GOLDEN_CANONICAL_BYTES_DIGEST: &str =
        "fe3649ef00598f4e26da0d120deb93dad0442b036110e8606eefac1609e60d52";

    #[test]
    fn sign_encode_parse_verify_round_trips() {
        let signer = ChannelOwnerSigner::from_account_seed(&[7; 32], &[9; 16]);
        let body = br#"{"listing_b64":"x"}"#;
        let proof = signer.sign_at(&req(body), 1_000_000, [3; 16]);
        let header = proof.encode();
        assert!(header.len() <= OWNER_PROOF_MAX_HEADER_LEN);
        let parsed = OwnerProof::parse(&header).unwrap();
        assert_eq!(parsed, proof);
        verify_owner_proof(&parsed, &req(body), &signer.verifying_key(), 1_000_000).unwrap();
    }

    #[test]
    fn verify_refuses_other_body_path_key_and_time() {
        let signer = ChannelOwnerSigner::from_account_seed(&[7; 32], &[9; 16]);
        let other = ChannelOwnerSigner::from_account_seed(&[7; 32], &[8; 16]);
        let proof = signer.sign_at(&req(b"a"), 1_000_000, [3; 16]);
        let key = signer.verifying_key();
        assert_eq!(
            verify_owner_proof(&proof, &req(b"b"), &key, 1_000_000),
            Err(OwnerProofError::BadSignature)
        );
        let mut moved = req(b"a");
        moved.canonical_path = "/v1/channels/ffeeddccbbaa99887766554433221100/listing";
        assert_eq!(
            verify_owner_proof(&proof, &moved, &key, 1_000_000),
            Err(OwnerProofError::BadSignature)
        );
        assert_eq!(
            verify_owner_proof(&proof, &req(b"a"), &other.verifying_key(), 1_000_000),
            Err(OwnerProofError::BadSignature)
        );
        assert_eq!(
            verify_owner_proof(
                &proof,
                &req(b"a"),
                &key,
                1_000_000 + OWNER_PROOF_WINDOW_MS + 1
            ),
            Err(OwnerProofError::OutsideWindow)
        );
        verify_owner_proof(&proof, &req(b"a"), &key, 1_000_000 + OWNER_PROOF_WINDOW_MS).unwrap();
    }

    #[test]
    fn a_ds_shaped_signature_never_verifies_here() {
        // The owner key signs the ADR-0053 bytes (DS domain, epoch 0): the
        // owner verifier refuses, because the domains differ.
        let signer = ChannelOwnerSigner::from_account_seed(&[7; 32], &[9; 16]);
        let mut ds = b"lakko/ds/proof/v1".to_vec();
        for part in ["PUT", req(b"").canonical_path, ""] {
            ds.push(0);
            ds.extend_from_slice(part.as_bytes());
        }
        ds.push(0);
        ds.extend_from_slice(&hash::blake3(b""));
        ds.extend_from_slice(&0u64.to_be_bytes());
        ds.extend_from_slice(&5u64.to_be_bytes());
        ds.extend_from_slice(&[1; 16]);
        let key = SigningKey::from_seed(&channel_owner_seed(&[7; 32], &[9; 16]));
        let forged = OwnerProof {
            unix_ms: 5,
            nonce: [1; 16],
            signature: key.sign(&ds).to_bytes(),
        };
        assert_eq!(
            verify_owner_proof(&forged, &req(b""), &signer.verifying_key(), 5),
            Err(OwnerProofError::BadSignature)
        );
    }

    #[test]
    fn parse_is_fail_closed() {
        let signer = ChannelOwnerSigner::from_account_seed(&[7; 32], &[9; 16]);
        let good = signer.sign_at(&req(b""), 5, [1; 16]).encode();
        assert_eq!(
            OwnerProof::parse(&good.replacen("v1", "v2", 1)),
            Err(OwnerProofError::UnknownVersion)
        );
        for bad in [
            String::new(),
            "v1".to_string(),
            "v1.5".to_string(),
            format!("{good}.x"),
            good.replacen(".5.", ".+5.", 1),
            good.replacen(".5.", "..", 1),
            "x".repeat(OWNER_PROOF_MAX_HEADER_LEN + 1),
        ] {
            assert_eq!(
                OwnerProof::parse(&bad),
                Err(OwnerProofError::Malformed),
                "{bad}"
            );
        }
    }
}
