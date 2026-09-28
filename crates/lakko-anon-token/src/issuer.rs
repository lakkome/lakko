// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The issuance key: derivation from the master secret, the public key, and
//! batched blind evaluation with one DLEQ proof.
//!
//! There is no key table. Every `(context, epoch)` key is a deterministic
//! function of one 32-byte master secret, so `services/auth` (the issuer) and
//! every verifying service holding the same master derive the same key without
//! coordination, and a key rotates on its own when the epoch rolls over.

use rand_core::OsRng;
use voprf::{BlindedElement, Group, Ristretto255, VoprfServer};
use zeroize::Zeroizing;

use crate::context::TokenContext;
use crate::error::{AnonTokenError, Result};
use crate::{
    BLINDED_ELEMENT_LEN, EVALUATION_ELEMENT_LEN, MAX_BATCH, PROOF_LEN, PUBLIC_KEY_LEN,
    TOKEN_VERSION,
};

/// Length of the master secret.
pub const MASTER_SECRET_LEN: usize = 32;

/// BLAKE3 derive-key context for the per-(context, epoch) key seed.
const SEED_DOMAIN: &str = "lakko/anon-token/v1 issuance-key-seed";

/// The `info` string handed to RFC 9497 `DeriveKeyPair`, before the
/// context byte and epoch.
const DERIVE_INFO_PREFIX: &[u8] = b"lakko/anon-token/v1";

/// The 32-byte secret every issuing and verifying service shares.
///
/// Wiped on drop. Its `Debug` prints nothing of the value.
#[derive(Clone)]
pub struct MasterSecret(Zeroizing<[u8; MASTER_SECRET_LEN]>);

impl core::fmt::Debug for MasterSecret {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("MasterSecret(..)")
    }
}

impl MasterSecret {
    /// Wrap 32 raw bytes.
    #[must_use]
    pub fn from_bytes(bytes: [u8; MASTER_SECRET_LEN]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// Parse 64 hex characters (either case, surrounding whitespace trimmed).
    ///
    /// # Errors
    ///
    /// [`AnonTokenError::InvalidMasterSecret`] for any other length or a
    /// non-hex character.
    pub fn from_hex(hex: &str) -> Result<Self> {
        let hex = hex.trim().as_bytes();
        if hex.len() != MASTER_SECRET_LEN * 2 {
            return Err(AnonTokenError::InvalidMasterSecret);
        }
        let mut out = Zeroizing::new([0u8; MASTER_SECRET_LEN]);
        for (i, pair) in hex.chunks_exact(2).enumerate() {
            let hi = hex_val(pair[0]).ok_or(AnonTokenError::InvalidMasterSecret)?;
            let lo = hex_val(pair[1]).ok_or(AnonTokenError::InvalidMasterSecret)?;
            out[i] = (hi << 4) | lo;
        }
        Ok(Self(out))
    }

    fn as_bytes(&self) -> &[u8; MASTER_SECRET_LEN] {
        &self.0
    }
}

const fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// The result of one issuance: one evaluated element per blinded element, in
/// the same order, and one batched DLEQ proof over all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issuance {
    /// The evaluated elements, one per blinded element, same order.
    pub evaluations: Vec<[u8; EVALUATION_ELEMENT_LEN]>,
    /// The batched DLEQ proof that every evaluation used the key behind
    /// [`IssuerKey::public_key`].
    pub proof: [u8; PROOF_LEN],
}

/// The VOPRF key for one `(context, epoch)`.
pub struct IssuerKey {
    context: TokenContext,
    epoch: u64,
    server: VoprfServer<Ristretto255>,
}

impl core::fmt::Debug for IssuerKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("IssuerKey")
            .field("context", &self.context)
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

impl IssuerKey {
    /// Derive the key for `(context, epoch)` from the master secret.
    ///
    /// `seed = BLAKE3-derive-key(SEED_DOMAIN, master || version || context ||
    /// epoch_be)`, then RFC 9497 `DeriveKeyPair(seed, info)` in VOPRF mode with
    /// `info = "lakko/anon-token/v1" || version || context || epoch_be`. Two
    /// layers of domain separation: the BLAKE3 context keeps the seed apart
    /// from any other use of the master, and the `info` keeps the scalar apart
    /// from any other VOPRF key derived from the same seed.
    ///
    /// # Errors
    ///
    /// [`AnonTokenError::Internal`] if `DeriveKeyPair` fails, which happens
    /// only if all 256 of its counter-indexed hash-to-scalar attempts yield
    /// zero.
    pub fn derive(master: &MasterSecret, context: TokenContext, epoch: u64) -> Result<Self> {
        let mut material = Zeroizing::new([0u8; MASTER_SECRET_LEN + 1 + 1 + 8]);
        material[..MASTER_SECRET_LEN].copy_from_slice(master.as_bytes());
        material[MASTER_SECRET_LEN] = TOKEN_VERSION;
        material[MASTER_SECRET_LEN + 1] = context.as_u8();
        material[MASTER_SECRET_LEN + 2..].copy_from_slice(&epoch.to_be_bytes());
        let seed = Zeroizing::new(lakko_crypto::hash::blake3_derive_key_from_material(
            SEED_DOMAIN,
            &material[..],
        ));

        let mut info = Vec::with_capacity(DERIVE_INFO_PREFIX.len() + 1 + 1 + 8);
        info.extend_from_slice(DERIVE_INFO_PREFIX);
        info.push(TOKEN_VERSION);
        info.push(context.as_u8());
        info.extend_from_slice(&epoch.to_be_bytes());

        let server = VoprfServer::<Ristretto255>::new_from_seed(&seed[..], &info)
            .map_err(|_| AnonTokenError::Internal)?;
        Ok(Self {
            context,
            epoch,
            server,
        })
    }

    /// The context this key belongs to.
    #[must_use]
    pub const fn context(&self) -> TokenContext {
        self.context
    }

    /// The epoch this key belongs to.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The public key (a compressed Ristretto255 point) the client checks the
    /// DLEQ proof against. Publishing it lets every client confirm it was
    /// served by the same key as everyone else for this `(context, epoch)`.
    #[must_use]
    pub fn public_key(&self) -> [u8; PUBLIC_KEY_LEN] {
        let mut out = [0u8; PUBLIC_KEY_LEN];
        out.copy_from_slice(&<Ristretto255 as Group>::serialize_elem(
            self.server.get_public_key(),
        ));
        out
    }

    /// Evaluate a batch of blinded elements and prove, with one DLEQ proof,
    /// that every evaluation used this key.
    ///
    /// Every element is deserialized (and the identity refused) before any
    /// evaluation, so a malformed batch is refused whole.
    ///
    /// # Errors
    ///
    /// - [`AnonTokenError::BadBatchSize`] for an empty batch or one larger than
    ///   [`MAX_BATCH`];
    /// - [`AnonTokenError::BadElement`] if any element is not a valid,
    ///   non-identity Ristretto255 encoding;
    /// - [`AnonTokenError::Internal`] if proof generation fails (unreachable
    ///   for a batch within [`MAX_BATCH`]).
    pub fn issue(&self, blinded: &[[u8; BLINDED_ELEMENT_LEN]]) -> Result<Issuance> {
        let elements = parse_blinded_batch(blinded)?;
        let prepared: Vec<_> = self
            .server
            .batch_blind_evaluate_prepare(elements.iter())
            .collect();
        let finished = self
            .server
            .batch_blind_evaluate_finish(&mut OsRng, elements.iter(), &prepared)
            .map_err(|_| AnonTokenError::Internal)?;
        let evaluations = finished
            .messages
            .map(|m| {
                let mut out = [0u8; EVALUATION_ELEMENT_LEN];
                out.copy_from_slice(&m.serialize());
                out
            })
            .collect();
        let mut proof = [0u8; PROOF_LEN];
        proof.copy_from_slice(&finished.proof.serialize());
        Ok(Issuance { evaluations, proof })
    }

    /// The VOPRF output for `input` under this key (RFC 9497 `Evaluate`).
    pub(crate) fn evaluate(&self, input: &[u8]) -> Result<[u8; crate::OUTPUT_LEN]> {
        let output = self
            .server
            .evaluate(input)
            .map_err(|_| AnonTokenError::Internal)?;
        let mut out = [0u8; crate::OUTPUT_LEN];
        out.copy_from_slice(&output);
        Ok(out)
    }
}

/// Check a batch the way [`IssuerKey::issue`] will, without evaluating it:
/// the size is within `1..=MAX_BATCH` and every element is a valid,
/// non-identity Ristretto255 encoding. Lets the issuer refuse a malformed
/// request before it charges the account's quota.
///
/// # Errors
///
/// [`AnonTokenError::BadBatchSize`] / [`AnonTokenError::BadElement`].
pub fn validate_blinded_batch(blinded: &[[u8; BLINDED_ELEMENT_LEN]]) -> Result<()> {
    parse_blinded_batch(blinded).map(|_| ())
}

fn parse_blinded_batch(
    blinded: &[[u8; BLINDED_ELEMENT_LEN]],
) -> Result<Vec<BlindedElement<Ristretto255>>> {
    if blinded.is_empty() || blinded.len() > MAX_BATCH {
        return Err(AnonTokenError::BadBatchSize);
    }
    blinded
        .iter()
        .map(|b| {
            BlindedElement::<Ristretto255>::deserialize(b).map_err(|_| AnonTokenError::BadElement)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn master() -> MasterSecret {
        MasterSecret::from_bytes([0x42; 32])
    }

    #[test]
    fn derivation_is_deterministic_and_separated() {
        let a = IssuerKey::derive(&master(), TokenContext::DsKnock, 7).unwrap();
        let b = IssuerKey::derive(&master(), TokenContext::DsKnock, 7).unwrap();
        assert_eq!(a.public_key(), b.public_key());

        let other_epoch = IssuerKey::derive(&master(), TokenContext::DsKnock, 8).unwrap();
        let other_ctx = IssuerKey::derive(&master(), TokenContext::PushRegister, 7).unwrap();
        let other_master = IssuerKey::derive(
            &MasterSecret::from_bytes([0x43; 32]),
            TokenContext::DsKnock,
            7,
        )
        .unwrap();
        assert_ne!(a.public_key(), other_epoch.public_key());
        assert_ne!(a.public_key(), other_ctx.public_key());
        assert_ne!(a.public_key(), other_master.public_key());
    }

    #[test]
    fn golden_public_key_is_pinned() {
        // Pins the derivation (domain strings, byte layout, RFC 9497
        // DeriveKeyPair). A change here renumbers every key and must be
        // deliberate: it invalidates every outstanding token.
        let key = IssuerKey::derive(&master(), TokenContext::DiscoveryLookup, 0).unwrap();
        let hex = key.public_key().iter().fold(String::new(), |mut acc, b| {
            use core::fmt::Write as _;
            let _ = write!(acc, "{b:02x}");
            acc
        });
        assert_eq!(hex, GOLDEN_PK_HEX);
    }

    // Computed by this implementation (a regression pin, not an external
    // test vector).
    const GOLDEN_PK_HEX: &str = "4c25b5bef1a399286cfe0d0d4b79fa7955133cbe5b9a794f97a1b3b6a084634e";

    #[test]
    fn master_secret_hex_parsing() {
        let hex = "00112233445566778899aabbccddeeff00112233445566778899AABBCCDDEEFF";
        let m = MasterSecret::from_hex(hex).unwrap();
        assert_eq!(m.as_bytes()[0], 0x00);
        assert_eq!(m.as_bytes()[9], 0x99);
        assert_eq!(m.as_bytes()[31], 0xff);
        assert!(MasterSecret::from_hex(&format!("  {hex}\n")).is_ok());
        assert_eq!(
            MasterSecret::from_hex(&hex[..62]).unwrap_err(),
            AnonTokenError::InvalidMasterSecret
        );
        assert_eq!(
            MasterSecret::from_hex(&hex.replace('a', "g")).unwrap_err(),
            AnonTokenError::InvalidMasterSecret
        );
        assert_eq!(format!("{m:?}"), "MasterSecret(..)");
    }

    #[test]
    fn issue_refuses_bad_batches() {
        let key = IssuerKey::derive(&master(), TokenContext::DsKnock, 1).unwrap();
        assert_eq!(key.issue(&[]).unwrap_err(), AnonTokenError::BadBatchSize);
        let too_many = vec![[0u8; 32]; MAX_BATCH + 1];
        assert_eq!(
            key.issue(&too_many).unwrap_err(),
            AnonTokenError::BadBatchSize
        );
        // The identity encoding (all zero) is refused.
        assert_eq!(
            key.issue(&[[0u8; 32]]).unwrap_err(),
            AnonTokenError::BadElement
        );
        // A non-canonical encoding is refused.
        assert_eq!(
            key.issue(&[[0xff; 32]]).unwrap_err(),
            AnonTokenError::BadElement
        );
    }
}
