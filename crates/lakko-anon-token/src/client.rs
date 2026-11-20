// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The client side: pick nonces, blind, and finalize the issuer's response
//! into tokens after checking its DLEQ proof.
//!
//! The issuer sees only blinded elements, so it cannot link a token it later
//! verifies to the issuance that produced it. The DLEQ proof is what stops the
//! issuer defeating that by evaluating one account under a key of its own: the
//! client checks the proof against the public key it expects for the
//! `(context, epoch)` - which the caller must obtain from the published key
//! endpoint, not from the same response it is checking.

use rand_core::{CryptoRng, OsRng, RngCore};
use voprf::{EvaluationElement, Group, Proof, Ristretto255, VoprfClient};

use crate::context::TokenContext;
use crate::error::{AnonTokenError, Result};
use crate::token::{token_input, Token};
use crate::wire::{decode_b64url_fixed, encode_b64url};
use crate::{
    BLINDED_ELEMENT_LEN, EVALUATION_ELEMENT_LEN, MAX_BATCH, NONCE_LEN, PROOF_LEN, PUBLIC_KEY_LEN,
};

/// A batch of blinded token requests awaiting the issuer's response.
///
/// Holds the blinding factors, so it is as sensitive as the tokens it will
/// become; it is consumed by [`Self::finalize`].
pub struct TokenRequest {
    context: TokenContext,
    epoch: u64,
    nonces: Vec<[u8; NONCE_LEN]>,
    clients: Vec<VoprfClient<Ristretto255>>,
    blinded: Vec<[u8; BLINDED_ELEMENT_LEN]>,
}

impl core::fmt::Debug for TokenRequest {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TokenRequest")
            .field("context", &self.context)
            .field("epoch", &self.epoch)
            .field("count", &self.nonces.len())
            .finish_non_exhaustive()
    }
}

impl TokenRequest {
    /// Start a request for `count` tokens for `(context, epoch)`: pick a fresh
    /// random nonce per token and blind its input.
    ///
    /// # Errors
    ///
    /// [`AnonTokenError::BadBatchSize`] unless `1 <= count <= MAX_BATCH`;
    /// [`AnonTokenError::Internal`] on an unreachable blinding failure.
    pub fn new(context: TokenContext, epoch: u64, count: usize) -> Result<Self> {
        if count == 0 || count > MAX_BATCH {
            return Err(AnonTokenError::BadBatchSize);
        }
        let mut nonces = Vec::with_capacity(count);
        let mut clients = Vec::with_capacity(count);
        let mut blinded = Vec::with_capacity(count);
        for _ in 0..count {
            let mut nonce = [0u8; NONCE_LEN];
            lakko_crypto::random::fill(&mut nonce);
            let input = token_input(context, epoch, &nonce);
            let res = VoprfClient::<Ristretto255>::blind(&input, &mut OsRng)
                .map_err(|_| AnonTokenError::Internal)?;
            let mut b = [0u8; BLINDED_ELEMENT_LEN];
            b.copy_from_slice(&res.message.serialize());
            blinded.push(b);
            nonces.push(nonce);
            clients.push(res.state);
        }
        Ok(Self {
            context,
            epoch,
            nonces,
            clients,
            blinded,
        })
    }

    /// A request for ONE token whose nonce and blinding factor the caller
    /// supplies: `nonce` as is, the blinding factor drawn from `blind_rng`.
    /// With a deterministic `blind_rng` the same inputs give the same
    /// blinded element, which is what lets a supporter code be redeemed
    /// again after a lost response without a second evaluation (ADR-0097,
    /// [`crate::SupporterCode::token_request`]).
    pub(crate) fn single_with<R: RngCore + CryptoRng>(
        context: TokenContext,
        epoch: u64,
        nonce: [u8; NONCE_LEN],
        blind_rng: &mut R,
    ) -> Result<Self> {
        let input = token_input(context, epoch, &nonce);
        let res = VoprfClient::<Ristretto255>::blind(&input, blind_rng)
            .map_err(|_| AnonTokenError::Internal)?;
        let mut b = [0u8; BLINDED_ELEMENT_LEN];
        b.copy_from_slice(&res.message.serialize());
        Ok(Self {
            context,
            epoch,
            nonces: vec![nonce],
            clients: vec![res.state],
            blinded: vec![b],
        })
    }

    /// The context this request is for.
    #[must_use]
    pub const fn context(&self) -> TokenContext {
        self.context
    }

    /// The epoch this request is for.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The blinded elements to send to the issuer, in order.
    #[must_use]
    pub fn blinded_elements(&self) -> &[[u8; BLINDED_ELEMENT_LEN]] {
        &self.blinded
    }

    /// The blinded elements as base64url without padding - the
    /// `blinded_elements` field of `POST /auth/tokens`, in order.
    #[must_use]
    pub fn blinded_elements_b64url(&self) -> Vec<String> {
        self.blinded.iter().map(|b| encode_b64url(b)).collect()
    }

    /// [`Self::finalize`] over the text the issuer and the key route send:
    /// every field base64url without padding.
    ///
    /// # Errors
    ///
    /// As [`Self::finalize`]; an evaluation that does not decode to 32 bytes
    /// is [`AnonTokenError::BadElement`], a proof that does not decode to 64
    /// bytes [`AnonTokenError::ProofVerification`], a public key that does not
    /// decode to 32 bytes [`AnonTokenError::BadPublicKey`].
    pub fn finalize_b64url<S: AsRef<str>>(
        self,
        evaluations: &[S],
        proof: &str,
        public_key: &str,
    ) -> Result<Vec<Token>> {
        let evaluations = evaluations
            .iter()
            .map(|e| {
                decode_b64url_fixed::<EVALUATION_ELEMENT_LEN>(e.as_ref())
                    .ok_or(AnonTokenError::BadElement)
            })
            .collect::<Result<Vec<_>>>()?;
        let proof =
            decode_b64url_fixed::<PROOF_LEN>(proof).ok_or(AnonTokenError::ProofVerification)?;
        let public_key = decode_b64url_fixed::<PUBLIC_KEY_LEN>(public_key)
            .ok_or(AnonTokenError::BadPublicKey)?;
        self.finalize(&evaluations, &proof, &public_key)
    }

    /// Check the issuer's proof against `public_key` and unblind every
    /// evaluation into a [`Token`].
    ///
    /// All-or-nothing: if the proof does not verify, no token is returned.
    ///
    /// # Errors
    ///
    /// - [`AnonTokenError::CountMismatch`] if the number of evaluations differs
    ///   from the number of blinded elements;
    /// - [`AnonTokenError::BadPublicKey`] / [`AnonTokenError::BadElement`] for
    ///   an invalid or identity encoding;
    /// - [`AnonTokenError::ProofVerification`] for a malformed proof or one
    ///   that does not verify under `public_key` (the issuer used another key);
    /// - [`AnonTokenError::Internal`] on an unreachable finalize failure.
    pub fn finalize(
        self,
        evaluations: &[[u8; EVALUATION_ELEMENT_LEN]],
        proof: &[u8; PROOF_LEN],
        public_key: &[u8; PUBLIC_KEY_LEN],
    ) -> Result<Vec<Token>> {
        if evaluations.len() != self.nonces.len() {
            return Err(AnonTokenError::CountMismatch);
        }
        let pk = <Ristretto255 as Group>::deserialize_elem(public_key)
            .map_err(|_| AnonTokenError::BadPublicKey)?;
        let proof = Proof::<Ristretto255>::deserialize(proof)
            .map_err(|_| AnonTokenError::ProofVerification)?;
        let messages = evaluations
            .iter()
            .map(|e| {
                EvaluationElement::<Ristretto255>::deserialize(e)
                    .map_err(|_| AnonTokenError::BadElement)
            })
            .collect::<Result<Vec<_>>>()?;
        let inputs: Vec<Vec<u8>> = self
            .nonces
            .iter()
            .map(|n| token_input(self.context, self.epoch, n))
            .collect();

        let outputs = VoprfClient::batch_finalize(&inputs, &self.clients, &messages, &proof, pk)
            .map_err(|_| AnonTokenError::ProofVerification)?;

        let mut tokens = Vec::with_capacity(self.nonces.len());
        for (nonce, output) in self.nonces.iter().zip(outputs) {
            let output = output.map_err(|_| AnonTokenError::Internal)?;
            let mut out = [0u8; crate::OUTPUT_LEN];
            out.copy_from_slice(&output);
            tokens.push(Token {
                context: self.context,
                epoch: self.epoch,
                nonce: *nonce,
                output: out,
            });
        }
        Ok(tokens)
    }
}
