// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Identity proof ([ADR-0098] security fixes, F3).
//!
//! An *identity proof* is a 64-byte Ed25519 signature by an account
//! identity key over the account id and that key's own public half. Auth
//! records an account's identity key (`accounts.identity_pubkey`) at the
//! genesis mint, a key reset, a key rotation and a recall, and a moderator
//! statement finds an account by that key. Before this proof the key was
//! client-asserted: an account could record another person's public key and
//! be found, suspended or not, as that person. With it, auth records a key
//! only when the caller holds its private half, and a key belongs to one
//! account.
//!
//! ```text
//! message = "lakko/auth/identity-proof/v1" ‖ 0x00 ‖ account_id(16) ‖ pubkey(32)
//! proof   = Ed25519_sign(identity key, message)            = 64 bytes
//! ```
//!
//! The message has no time and no nonce: a proof is a standing claim ("this
//! key is held by whoever runs this account"), not a request, so replaying
//! it records the same key for the same account again and nothing else. The
//! domain is distinct from every other use of an account identity key
//! (account statements, device bindings, supersession proofs, DS proofs);
//! none of them signs a message that starts with it.
//!
//! [ADR-0098]: ../../../docs/adr/0098-reporting-and-moderation.md

use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN, SIGNATURE_LEN};
use thiserror::Error;

/// The domain, followed by one `0x00` byte in the message.
const DOMAIN: &[u8] = b"lakko/auth/identity-proof/v1";

/// Length of an account id.
pub const IDENTITY_PROOF_ACCOUNT_ID_LEN: usize = 16;

/// Length of an identity proof.
pub const IDENTITY_PROOF_LEN: usize = SIGNATURE_LEN;

/// Why an identity proof was refused.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum IdentityProofError {
    /// The proof is not [`IDENTITY_PROOF_LEN`] bytes.
    #[error("identity proof has wrong length: {got} bytes")]
    BadLength {
        /// The length received.
        got: usize,
    },
    /// The public key is not a valid Ed25519 key.
    #[error("identity public key is invalid")]
    BadKey,
    /// The signature does not verify for this account and key.
    #[error("identity proof does not verify")]
    BadSignature,
}

/// The signed message: `DOMAIN ‖ 0x00 ‖ account_id ‖ pubkey`.
#[must_use]
pub fn identity_proof_message(
    account_id: &[u8; IDENTITY_PROOF_ACCOUNT_ID_LEN],
    pubkey: &[u8; PUBLIC_KEY_LEN],
) -> Vec<u8> {
    let mut message =
        Vec::with_capacity(DOMAIN.len() + 1 + IDENTITY_PROOF_ACCOUNT_ID_LEN + PUBLIC_KEY_LEN);
    message.extend_from_slice(DOMAIN);
    message.push(0);
    message.extend_from_slice(account_id);
    message.extend_from_slice(pubkey);
    message
}

/// Sign the identity proof for `account_id` with the identity key whose
/// 32-byte Ed25519 seed is `seed`. The public key signed over is the seed's
/// own.
#[must_use]
pub fn sign_identity_proof(
    seed: &[u8; 32],
    account_id: &[u8; IDENTITY_PROOF_ACCOUNT_ID_LEN],
) -> [u8; IDENTITY_PROOF_LEN] {
    let signing = SigningKey::from_seed(seed);
    let pubkey = signing.verifying_key().to_bytes();
    signing
        .sign(&identity_proof_message(account_id, &pubkey))
        .to_bytes()
}

/// Verify that `proof` is `pubkey`'s signature over the identity proof
/// message for `account_id`.
///
/// # Errors
///
/// [`IdentityProofError::BadLength`], [`IdentityProofError::BadKey`],
/// [`IdentityProofError::BadSignature`].
pub fn verify_identity_proof(
    proof: &[u8],
    account_id: &[u8; IDENTITY_PROOF_ACCOUNT_ID_LEN],
    pubkey: &[u8; PUBLIC_KEY_LEN],
) -> Result<(), IdentityProofError> {
    if proof.len() != IDENTITY_PROOF_LEN {
        return Err(IdentityProofError::BadLength { got: proof.len() });
    }
    let verifying = VerifyingKey::from_bytes(pubkey).map_err(|_| IdentityProofError::BadKey)?;
    let signature = Signature::from_bytes(proof).map_err(|_| IdentityProofError::BadSignature)?;
    verifying
        .verify(&identity_proof_message(account_id, pubkey), &signature)
        .map_err(|_| IdentityProofError::BadSignature)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn pubkey(seed: &[u8; 32]) -> [u8; 32] {
        SigningKey::from_seed(seed).verifying_key().to_bytes()
    }

    #[test]
    fn a_proof_verifies_for_its_account_and_key_only() {
        let seed = [3u8; 32];
        let account = [7u8; 16];
        let proof = sign_identity_proof(&seed, &account);
        assert_eq!(
            verify_identity_proof(&proof, &account, &pubkey(&seed)),
            Ok(())
        );
        assert_eq!(
            verify_identity_proof(&proof, &[8u8; 16], &pubkey(&seed)),
            Err(IdentityProofError::BadSignature)
        );
        assert_eq!(
            verify_identity_proof(&proof, &account, &pubkey(&[4u8; 32])),
            Err(IdentityProofError::BadSignature)
        );
        let mut flipped = proof;
        flipped[10] ^= 1;
        assert_eq!(
            verify_identity_proof(&flipped, &account, &pubkey(&seed)),
            Err(IdentityProofError::BadSignature)
        );
        assert_eq!(
            verify_identity_proof(&proof[..63], &account, &pubkey(&seed)),
            Err(IdentityProofError::BadLength { got: 63 })
        );
    }

    #[test]
    fn the_message_is_frozen() {
        let m = identity_proof_message(&[1; 16], &[2; 32]);
        assert!(m.starts_with(b"lakko/auth/identity-proof/v1\0"));
        assert_eq!(m.len(), 29 + 16 + 32);
        assert_eq!(&m[29..45], &[1; 16]);
        assert_eq!(&m[45..], &[2; 32]);
    }

    #[test]
    fn an_account_statement_is_not_an_identity_proof() {
        // An account statement signed by the same key over the same account
        // does not verify as an identity proof.
        let seed = [3u8; 32];
        let account = [7u8; 16];
        let st = crate::AccountStatement::new(
            crate::AccountStatementKind::RecallStatus,
            account,
            1,
            [0; 16],
            &[],
        )
        .sign(&SigningKey::from_seed(&seed));
        let sig = &st[st.len() - SIGNATURE_LEN..];
        assert!(verify_identity_proof(sig, &account, &pubkey(&seed)).is_err());
    }
}
