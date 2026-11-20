// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Supporter codes ([ADR-0097] section 1).
//!
//! A supporter code is 80 random bits the maintainer mails to a backer. It
//! is redeemed, without a session, for ONE evaluation of a `beta-place`
//! token, which the first username bind spends past the beta account cap.
//!
//! # Text form
//!
//! Crockford base32 (`0123456789ABCDEFGHJKMNPQRSTVWXYZ`), 16 symbols, shown
//! in four groups of four (`K7QM-2XRA-9DPF-4WNB`). Parsing is forgiving the
//! way a person types: any case, spaces and dashes ignored, `O` read as `0`,
//! `I` and `L` as `1`. `U` is not a symbol. Exactly 16 symbols, else
//! [`AnonTokenError::BadSupporterCode`].
//!
//! # One code, one evaluation
//!
//! The token's nonce and blinding factor are derived from the code and the
//! epoch ([`SupporterCode::token_request`]), so a client that lost the
//! redeem response sends the same blinded element again and the server
//! answers with the evaluation it stored. A different epoch gives a
//! different request, as it must: the epoch is inside the token input.
//!
//! Stated cost: whoever holds a code can recompute its token, hence its
//! spend identifier. The server stores only a keyed hash of each code; the
//! maintainer's mailing list holds the codes, so with a copy of auth's
//! database it can tell which codes were spent, not by which account.
//!
//! [ADR-0097]: ../../../docs/adr/0097-supporter-codes-and-list.md

use rand_core::{CryptoRng, RngCore};
use zeroize::Zeroize;

use crate::client::TokenRequest;
use crate::context::TokenContext;
use crate::error::{AnonTokenError, Result};

/// Bytes in a supporter code (80 bits).
pub const SUPPORTER_CODE_LEN: usize = 10;

/// Base32 symbols in a supporter code (80 / 5).
pub const SUPPORTER_CODE_SYMBOLS: usize = 16;

/// The Crockford base32 alphabet, in value order.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// BLAKE3 derive-key context of the token nonce.
const NONCE_DOMAIN: &str = "lakko/supporter-code/nonce/v1";

/// BLAKE3 derive-key context of the key whose XOF stream is the blinding
/// factor's randomness.
const BLIND_DOMAIN: &str = "lakko/supporter-code/blind/v1";

/// A supporter code. Wiped on drop; its `Debug` prints nothing of it.
#[derive(Clone, PartialEq, Eq)]
pub struct SupporterCode([u8; SUPPORTER_CODE_LEN]);

impl core::fmt::Debug for SupporterCode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("SupporterCode(..)")
    }
}

impl Drop for SupporterCode {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl SupporterCode {
    /// A fresh code from the operating system's CSPRNG (the mint command).
    #[must_use]
    pub fn generate() -> Self {
        let mut b = [0u8; SUPPORTER_CODE_LEN];
        lakko_crypto::random::fill(&mut b);
        Self(b)
    }

    /// Wrap 10 raw bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; SUPPORTER_CODE_LEN]) -> Self {
        Self(bytes)
    }

    /// The 10 raw bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; SUPPORTER_CODE_LEN] {
        &self.0
    }

    /// Parse typed text (see the module docs for what is forgiven).
    ///
    /// # Errors
    ///
    /// [`AnonTokenError::BadSupporterCode`] for a character that is not a
    /// symbol, space or dash, or for any count of symbols but 16.
    pub fn parse(text: &str) -> Result<Self> {
        let mut acc: u128 = 0;
        let mut n = 0usize;
        for c in text.chars() {
            let v = match c.to_ascii_uppercase() {
                ' ' | '-' => continue,
                'O' => 0,
                'I' | 'L' => 1,
                u => {
                    if !u.is_ascii() {
                        return Err(AnonTokenError::BadSupporterCode);
                    }
                    let b = u as u8;
                    ALPHABET
                        .iter()
                        .position(|&a| a == b)
                        .ok_or(AnonTokenError::BadSupporterCode)?
                },
            };
            if n == SUPPORTER_CODE_SYMBOLS {
                return Err(AnonTokenError::BadSupporterCode);
            }
            acc = (acc << 5) | v as u128;
            n += 1;
        }
        if n != SUPPORTER_CODE_SYMBOLS {
            return Err(AnonTokenError::BadSupporterCode);
        }
        let mut out = [0u8; SUPPORTER_CODE_LEN];
        out.copy_from_slice(&acc.to_be_bytes()[16 - SUPPORTER_CODE_LEN..]);
        acc.zeroize();
        Ok(Self(out))
    }

    /// The display form: 16 symbols in four groups of four, joined by `-`.
    #[must_use]
    pub fn to_display(&self) -> String {
        let mut wide = [0u8; 16];
        wide[16 - SUPPORTER_CODE_LEN..].copy_from_slice(&self.0);
        let acc = u128::from_be_bytes(wide);
        let mut out = String::with_capacity(SUPPORTER_CODE_SYMBOLS + 3);
        for i in 0..SUPPORTER_CODE_SYMBOLS {
            if i > 0 && i % 4 == 0 {
                out.push('-');
            }
            let shift = 5 * (SUPPORTER_CODE_SYMBOLS - 1 - i);
            // The mask keeps the index below 32.
            let idx = ((acc >> shift) & 0x1F) as usize;
            out.push(char::from(ALPHABET[idx]));
        }
        wide.zeroize();
        out
    }

    /// The `beta-place` token request for `epoch`, with its nonce and
    /// blinding factor derived from this code and the epoch, so the same
    /// `(code, epoch)` always gives the same blinded element:
    ///
    /// - `nonce = BLAKE3-derive-key("lakko/supporter-code/nonce/v1",
    ///   code ‖ epoch_be)`;
    /// - the blinding factor is the RFC 9497 random scalar drawn from the
    ///   XOF stream keyed by `BLAKE3-derive-key("lakko/supporter-code/blind/v1",
    ///   code ‖ epoch_be)` (64 bytes reduced modulo the group order, a zero
    ///   scalar skipped).
    ///
    /// # Errors
    ///
    /// [`AnonTokenError::Internal`] on an unreachable blinding failure.
    pub fn token_request(&self, epoch: u64) -> Result<TokenRequest> {
        let mut material = [0u8; SUPPORTER_CODE_LEN + 8];
        material[..SUPPORTER_CODE_LEN].copy_from_slice(&self.0);
        material[SUPPORTER_CODE_LEN..].copy_from_slice(&epoch.to_be_bytes());
        let nonce = lakko_crypto::hash::blake3_derive_key_from_material(NONCE_DOMAIN, &material);
        let mut rng = XofRng::new(lakko_crypto::hash::blake3_derive_key_from_material(
            BLIND_DOMAIN,
            &material,
        ));
        material.zeroize();
        TokenRequest::single_with(TokenContext::BetaPlace, epoch, nonce, &mut rng)
    }
}

/// A deterministic byte stream: block `i` is 64 bytes of the BLAKE3 XOF
/// keyed by `key` over `i` (8 bytes, big endian). Used only to draw one
/// blinding scalar reproducibly; its output is as unpredictable as the key,
/// which is derived from the 80-bit code.
struct XofRng {
    key: [u8; 32],
    block: [u8; 64],
    used: usize,
    counter: u64,
}

impl XofRng {
    const fn new(key: [u8; 32]) -> Self {
        Self {
            key,
            block: [0u8; 64],
            used: 64,
            counter: 0,
        }
    }

    fn refill(&mut self) {
        let next = lakko_crypto::hash::blake3_keyed_xof(&self.key, &self.counter.to_be_bytes(), 64);
        self.block.copy_from_slice(&next);
        self.counter = self.counter.wrapping_add(1);
        self.used = 0;
    }
}

impl Drop for XofRng {
    fn drop(&mut self) {
        self.key.zeroize();
        self.block.zeroize();
    }
}

impl RngCore for XofRng {
    fn next_u32(&mut self) -> u32 {
        let mut b = [0u8; 4];
        self.fill_bytes(&mut b);
        u32::from_le_bytes(b)
    }

    fn next_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        self.fill_bytes(&mut b);
        u64::from_le_bytes(b)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for byte in dest.iter_mut() {
            if self.used == self.block.len() {
                self.refill();
            }
            *byte = self.block[self.used];
            self.used += 1;
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> core::result::Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

/// The stream is deterministic, but it is keyed by a secret derived from the
/// code, so to anyone without the code it is as good as random; the only
/// property the protocol needs beyond that is reproducibility for the holder.
impl CryptoRng for XofRng {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{verify_token, IssuerKey, MasterSecret, BETA_PLACE_WINDOW_SECS};
    use proptest::prelude::*;

    #[test]
    fn display_and_parse_round_trip_and_forgive_typing() {
        let code = SupporterCode::from_bytes([0xA5; SUPPORTER_CODE_LEN]);
        let shown = code.to_display();
        assert_eq!(shown.len(), 19);
        assert_eq!(shown.matches('-').count(), 3);
        assert_eq!(SupporterCode::parse(&shown).unwrap(), code);
        let typed = shown.to_lowercase().replace('-', " ");
        assert_eq!(SupporterCode::parse(&typed).unwrap(), code);
        assert_eq!(SupporterCode::parse(&shown.replace('-', "")).unwrap(), code);

        assert_eq!(
            SupporterCode::parse("OOOO-IIII-LLLL-0000").unwrap(),
            SupporterCode::parse("0000-1111-1111-0000").unwrap()
        );
        assert_eq!(
            SupporterCode::parse("0000-0000-0000-000U"),
            Err(AnonTokenError::BadSupporterCode)
        );
        assert_eq!(
            SupporterCode::parse("0000-0000-0000-000"),
            Err(AnonTokenError::BadSupporterCode)
        );
        assert_eq!(
            SupporterCode::parse("0000-0000-0000-00000"),
            Err(AnonTokenError::BadSupporterCode)
        );
        assert_eq!(
            SupporterCode::parse("0000-0000-0000-000\u{00e9}"),
            Err(AnonTokenError::BadSupporterCode)
        );
        assert_eq!(
            SupporterCode::parse(""),
            Err(AnonTokenError::BadSupporterCode)
        );
        assert_eq!(format!("{code:?}"), "SupporterCode(..)");
    }

    #[test]
    fn known_value_is_pinned() {
        assert_eq!(
            SupporterCode::from_bytes([0; SUPPORTER_CODE_LEN]).to_display(),
            "0000-0000-0000-0000"
        );
        assert_eq!(
            SupporterCode::from_bytes([0xFF; SUPPORTER_CODE_LEN]).to_display(),
            "ZZZZ-ZZZZ-ZZZZ-ZZZZ"
        );
        let mut b = [0u8; SUPPORTER_CODE_LEN];
        b[SUPPORTER_CODE_LEN - 1] = 31;
        assert_eq!(
            SupporterCode::from_bytes(b).to_display(),
            "0000-0000-0000-000Z"
        );
    }

    #[test]
    fn same_code_and_epoch_give_the_same_request_and_other_epochs_do_not() {
        let code = SupporterCode::from_bytes([7; SUPPORTER_CODE_LEN]);
        let a = code.token_request(100).unwrap();
        let b = code.token_request(100).unwrap();
        let c = code.token_request(101).unwrap();
        let other = SupporterCode::from_bytes([8; SUPPORTER_CODE_LEN])
            .token_request(100)
            .unwrap();
        assert_eq!(a.blinded_elements(), b.blinded_elements());
        assert_ne!(a.blinded_elements(), c.blinded_elements());
        assert_ne!(a.blinded_elements(), other.blinded_elements());
        assert_eq!(a.context(), TokenContext::BetaPlace);
        assert_eq!(a.blinded_elements().len(), 1);
    }

    #[test]
    fn a_redeemed_code_finalizes_into_a_beta_place_token_that_verifies() {
        let master = MasterSecret::from_bytes([0x24; 32]);
        let epoch = 700;
        let code = SupporterCode::generate();
        let key = IssuerKey::derive(&master, TokenContext::BetaPlace, epoch).unwrap();
        let first = code.token_request(epoch).unwrap();
        let issuance = key.issue(first.blinded_elements()).unwrap();
        // A retry built from the same code finalizes against the stored
        // evaluation and proof.
        let retry = code.token_request(epoch).unwrap();
        let token = retry
            .finalize(&issuance.evaluations, &issuance.proof, &key.public_key())
            .unwrap()
            .remove(0);
        let now = epoch * BETA_PLACE_WINDOW_SECS + 5;
        let v = verify_token(
            &master,
            &token.encode(),
            TokenContext::BetaPlace,
            now,
            86_400,
        )
        .unwrap();
        assert_eq!(v.epoch, epoch);
        // Valid in the next 30-day window, not the one after.
        assert!(verify_token(
            &master,
            &token.encode(),
            TokenContext::BetaPlace,
            now + BETA_PLACE_WINDOW_SECS,
            86_400
        )
        .is_ok());
        assert_eq!(
            verify_token(
                &master,
                &token.encode(),
                TokenContext::BetaPlace,
                now + 2 * BETA_PLACE_WINDOW_SECS,
                86_400
            )
            .unwrap_err(),
            AnonTokenError::EpochOutOfWindow
        );
    }

    proptest! {
        #[test]
        fn any_code_round_trips(bytes in proptest::array::uniform10(any::<u8>())) {
            let code = SupporterCode::from_bytes(bytes);
            let shown = code.to_display();
            prop_assert_eq!(SupporterCode::parse(&shown).unwrap(), code.clone());
            prop_assert_eq!(SupporterCode::parse(&shown.to_lowercase()).unwrap(), code);
        }

        #[test]
        fn parse_never_panics(text in ".{0,40}") {
            let _ = SupporterCode::parse(&text);
        }
    }
}
