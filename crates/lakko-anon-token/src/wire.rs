// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The HTTP transport of a token and of the issuance exchange: the header a
//! spending request carries, and the base64url (no padding) text every
//! binary field travels as.
//!
//! One definition for the three verifying services and both FFI surfaces, so
//! no client and no service spells the header name or picks an alphabet.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;

use crate::error::{AnonTokenError, Result};
use crate::token::Token;
use crate::TOKEN_LEN;

/// The request header a spent token travels in:
/// `x-lakko-anon-token: <base64url(token), no padding>`.
pub const TOKEN_HEADER: &str = "x-lakko-anon-token";

/// Length of a header value: base64url without padding of [`TOKEN_LEN`]
/// bytes (106 bytes -> 142 characters).
pub const TOKEN_HEADER_VALUE_LEN: usize = (TOKEN_LEN * 4).div_ceil(3);

/// Base64url without padding - the text form of every binary field of the
/// issuance exchange (blinded elements, evaluations, proof, public key) and
/// of a token in [`TOKEN_HEADER`].
#[must_use]
pub fn encode_b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Decode base64url without padding into exactly `N` bytes; `None` for any
/// other length or a character outside the alphabet. The length is checked
/// on the text before anything is decoded.
#[must_use]
pub fn decode_b64url_fixed<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != (N * 4).div_ceil(3) {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(text).ok()?;
    bytes.as_slice().try_into().ok()
}

impl Token {
    /// The value of [`TOKEN_HEADER`] that spends this token.
    #[must_use]
    pub fn to_header_value(&self) -> String {
        encode_b64url(&self.encode())
    }
}

/// Decode a [`TOKEN_HEADER`] value into the token bytes `verify_token`
/// takes. Bounded: a value that is not exactly [`TOKEN_HEADER_VALUE_LEN`]
/// characters is refused before any decoding, surrounding whitespace is not
/// tolerated, and nothing about the token's content is checked here.
///
/// # Errors
///
/// [`AnonTokenError::Malformed`] for a wrong length or a character outside
/// the base64url alphabet.
pub fn decode_header_value(value: &str) -> Result<[u8; TOKEN_LEN]> {
    decode_b64url_fixed::<TOKEN_LEN>(value).ok_or(AnonTokenError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TokenContext, NONCE_LEN, OUTPUT_LEN};

    fn sample() -> Token {
        Token {
            context: TokenContext::PushRegister,
            epoch: 42,
            nonce: [0xFE; NONCE_LEN],
            output: [0xFB; OUTPUT_LEN],
        }
    }

    #[test]
    fn header_value_round_trips_and_has_the_pinned_length() {
        let t = sample();
        let v = t.to_header_value();
        assert_eq!(v.len(), TOKEN_HEADER_VALUE_LEN);
        assert_eq!(TOKEN_HEADER_VALUE_LEN, 142);
        // URL-safe alphabet: 0xFB / 0xFE bytes produce '-' / '_' , never '+' '/'.
        assert!(!v.contains('+') && !v.contains('/') && !v.contains('='));
        assert_eq!(decode_header_value(&v).unwrap(), t.encode());
        assert_eq!(TOKEN_HEADER, "x-lakko-anon-token");
    }

    #[test]
    fn header_value_decode_is_fail_closed() {
        let v = sample().to_header_value();
        assert_eq!(decode_header_value(""), Err(AnonTokenError::Malformed));
        assert_eq!(
            decode_header_value(&v[..v.len() - 1]),
            Err(AnonTokenError::Malformed)
        );
        assert_eq!(
            decode_header_value(&format!("{v}A")),
            Err(AnonTokenError::Malformed)
        );
        assert_eq!(
            decode_header_value(&format!(" {}", &v[1..])),
            Err(AnonTokenError::Malformed)
        );
        // The standard alphabet's '+' is refused.
        let std_alpha = format!("+{}", &v[1..]);
        assert_eq!(
            decode_header_value(&std_alpha),
            Err(AnonTokenError::Malformed)
        );
    }

    #[test]
    fn fixed_decode_checks_the_length() {
        let text = encode_b64url(&[7u8; 32]);
        assert_eq!(decode_b64url_fixed::<32>(&text), Some([7u8; 32]));
        assert_eq!(decode_b64url_fixed::<31>(&text), None);
        assert_eq!(decode_b64url_fixed::<32>(&text[..42]), None);
        assert_eq!(decode_b64url_fixed::<32>("!".repeat(43).as_str()), None);
    }
}
