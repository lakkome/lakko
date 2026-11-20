// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The token: its fixed wire layout, the VOPRF input it commits to, the spend
//! identifier, and the verifier.

use subtle::ConstantTimeEq as _;

use crate::context::{epoch_accepted, epoch_at, TokenContext};
use crate::error::{AnonTokenError, Result};
use crate::issuer::{IssuerKey, MasterSecret};
use crate::{NONCE_LEN, OUTPUT_LEN, TOKEN_LEN, TOKEN_VERSION};

/// Prefix of the VOPRF input a token commits to.
const INPUT_DOMAIN: &[u8] = b"lakko/anon-token/v1 input";

/// BLAKE3 derive-key context for the spend identifier.
const SPEND_ID_DOMAIN: &str = "lakko/anon-token/v1 spend-id";

/// Length of a spend identifier.
pub const SPEND_ID_LEN: usize = 32;

/// The identifier a verifier records in its spent-set.
///
/// A deterministic function of `(version, context, epoch, nonce)` - the fields
/// that determine a token's validity - so two encodings of the same valid
/// token cannot both be spent, and nothing about the account enters it.
pub type SpendId = [u8; SPEND_ID_LEN];

/// A finalized anonymous token.
///
/// Wire layout, fixed at [`TOKEN_LEN`] (106) bytes:
///
/// ```text
/// version(1) || context(1) || epoch(8, BE) || nonce(32) || output(64)
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct Token {
    /// What the token pays for.
    pub context: TokenContext,
    /// The issuance epoch.
    pub epoch: u64,
    /// The client-chosen random nonce.
    pub nonce: [u8; NONCE_LEN],
    /// The VOPRF output over [`token_input`].
    pub output: [u8; OUTPUT_LEN],
}

impl core::fmt::Debug for Token {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The nonce and output are what make a token spendable; never print them.
        f.debug_struct("Token")
            .field("context", &self.context)
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

impl Token {
    /// Encode to the fixed wire layout.
    #[must_use]
    pub fn encode(&self) -> [u8; TOKEN_LEN] {
        let mut out = [0u8; TOKEN_LEN];
        out[0] = TOKEN_VERSION;
        out[1] = self.context.as_u8();
        out[2..10].copy_from_slice(&self.epoch.to_be_bytes());
        out[10..10 + NONCE_LEN].copy_from_slice(&self.nonce);
        out[10 + NONCE_LEN..].copy_from_slice(&self.output);
        out
    }

    /// Decode the fixed wire layout. Bounded and fail-closed: the length is
    /// checked first, then the version, then the context byte; nothing is
    /// allocated.
    ///
    /// # Errors
    ///
    /// [`AnonTokenError::Malformed`] (length), [`AnonTokenError::UnknownVersion`],
    /// [`AnonTokenError::UnknownContext`].
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let bytes: &[u8; TOKEN_LEN] = bytes.try_into().map_err(|_| AnonTokenError::Malformed)?;
        if bytes[0] != TOKEN_VERSION {
            return Err(AnonTokenError::UnknownVersion);
        }
        let context = TokenContext::from_u8(bytes[1])?;
        let mut epoch = [0u8; 8];
        epoch.copy_from_slice(&bytes[2..10]);
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&bytes[10..10 + NONCE_LEN]);
        let mut output = [0u8; OUTPUT_LEN];
        output.copy_from_slice(&bytes[10 + NONCE_LEN..]);
        Ok(Self {
            context,
            epoch: u64::from_be_bytes(epoch),
            nonce,
            output,
        })
    }

    /// The spend identifier for this token.
    #[must_use]
    pub fn spend_id(&self) -> SpendId {
        spend_id(self.context, self.epoch, &self.nonce)
    }
}

/// The VOPRF input a token commits to:
/// `"lakko/anon-token/v1 input" || version || context || epoch_be || nonce`.
#[must_use]
pub fn token_input(context: TokenContext, epoch: u64, nonce: &[u8; NONCE_LEN]) -> Vec<u8> {
    let mut input = Vec::with_capacity(INPUT_DOMAIN.len() + 1 + 1 + 8 + NONCE_LEN);
    input.extend_from_slice(INPUT_DOMAIN);
    input.push(TOKEN_VERSION);
    input.push(context.as_u8());
    input.extend_from_slice(&epoch.to_be_bytes());
    input.extend_from_slice(nonce);
    input
}

fn spend_id(context: TokenContext, epoch: u64, nonce: &[u8; NONCE_LEN]) -> SpendId {
    let mut material = [0u8; 1 + 1 + 8 + NONCE_LEN];
    material[0] = TOKEN_VERSION;
    material[1] = context.as_u8();
    material[2..10].copy_from_slice(&epoch.to_be_bytes());
    material[10..].copy_from_slice(nonce);
    lakko_crypto::hash::blake3_derive_key_from_material(SPEND_ID_DOMAIN, &material)
}

/// A token that verified. The caller must still record [`Self::spend_id`] in
/// its spent-set atomically before honouring the request; verification alone
/// does not stop a second spend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedToken {
    /// The context the token was issued for (equal to the expected one).
    pub context: TokenContext,
    /// The issuance epoch (the current or the previous one).
    pub epoch: u64,
    /// The identifier to insert into the spent-set.
    pub spend_id: SpendId,
}

/// Verify a token presented to a service.
///
/// Checks, in order: the wire layout ([`Token::decode`]), that the context is
/// `expected`, that the epoch is the current or the previous one at
/// `now_unix_secs` under `window_secs` (or the context's fixed window,
/// [`TokenContext::fixed_window_secs`]), and that the output equals the VOPRF
/// evaluation of the token input under the `(context, epoch)` key derived from
/// `master` - compared in constant time. Pure: the double-spend check is the
/// caller's (see [`VerifiedToken`]).
///
/// # Errors
///
/// Every [`Token::decode`] error, [`AnonTokenError::WrongContext`],
/// [`AnonTokenError::InvalidWindow`], [`AnonTokenError::EpochOutOfWindow`],
/// [`AnonTokenError::InvalidToken`], or [`AnonTokenError::Internal`] on an
/// unreachable VOPRF failure.
pub fn verify_token(
    master: &MasterSecret,
    bytes: &[u8],
    expected: TokenContext,
    now_unix_secs: u64,
    window_secs: u64,
) -> Result<VerifiedToken> {
    let token = Token::decode(bytes)?;
    if token.context != expected {
        return Err(AnonTokenError::WrongContext);
    }
    // A context with a window of its own (`beta-place`) ignores the
    // configured one, so every verifier agrees on its epoch.
    let current = epoch_at(now_unix_secs, expected.window_secs(window_secs))?;
    if !epoch_accepted(token.epoch, current) {
        return Err(AnonTokenError::EpochOutOfWindow);
    }
    let key = IssuerKey::derive(master, token.context, token.epoch)?;
    let expected_output = key.evaluate(&token_input(token.context, token.epoch, &token.nonce))?;
    if !bool::from(expected_output.ct_eq(&token.output)) {
        return Err(AnonTokenError::InvalidToken);
    }
    Ok(VerifiedToken {
        context: token.context,
        epoch: token.epoch,
        spend_id: token.spend_id(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Token {
        Token {
            context: TokenContext::InviteConsume,
            epoch: 0x0102_0304_0506_0708,
            nonce: [0xAA; NONCE_LEN],
            output: [0xBB; OUTPUT_LEN],
        }
    }

    #[test]
    fn codec_round_trips_with_the_documented_layout() {
        let t = sample();
        let wire = t.encode();
        assert_eq!(wire.len(), 106);
        assert_eq!(wire[0], TOKEN_VERSION);
        assert_eq!(wire[1], TokenContext::InviteConsume.as_u8());
        assert_eq!(&wire[2..10], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(&wire[10..42], &[0xAA; 32]);
        assert_eq!(&wire[42..], &[0xBB; 64]);
        assert_eq!(Token::decode(&wire), Ok(t));
    }

    #[test]
    fn decode_is_fail_closed() {
        let wire = sample().encode();
        assert_eq!(Token::decode(&[]), Err(AnonTokenError::Malformed));
        assert_eq!(
            Token::decode(&wire[..TOKEN_LEN - 1]),
            Err(AnonTokenError::Malformed)
        );
        let mut long = wire.to_vec();
        long.push(0);
        assert_eq!(Token::decode(&long), Err(AnonTokenError::Malformed));

        let mut bad_version = wire;
        bad_version[0] = 2;
        assert_eq!(
            Token::decode(&bad_version),
            Err(AnonTokenError::UnknownVersion)
        );
        let mut bad_ctx = wire;
        bad_ctx[1] = 0;
        assert_eq!(Token::decode(&bad_ctx), Err(AnonTokenError::UnknownContext));
    }

    #[test]
    fn spend_id_depends_on_every_validity_field_and_not_the_output() {
        let t = sample();
        let mut other_output = t.clone();
        other_output.output = [0; OUTPUT_LEN];
        assert_eq!(t.spend_id(), other_output.spend_id());

        let mut c = t.clone();
        c.context = TokenContext::DsKnock;
        let mut e = t.clone();
        e.epoch += 1;
        let mut n = t.clone();
        n.nonce[31] ^= 1;
        assert_ne!(t.spend_id(), c.spend_id());
        assert_ne!(t.spend_id(), e.spend_id());
        assert_ne!(t.spend_id(), n.spend_id());
    }

    #[test]
    fn debug_never_prints_nonce_or_output() {
        let s = format!("{:?}", sample());
        assert!(!s.contains("170")); // 0xAA
        assert!(!s.contains("187")); // 0xBB
        assert!(!s.to_lowercase().contains("aa"));
    }
}
