// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Pieces shared by the ADR-0090 content codecs: the error vocabulary, the
//! bounded byte reader, the display-text rule and the author signature.
//!
//! Every content codec in this crate is strict and canonical: a decoder
//! refuses anything its encoder would not have produced, so a decoded value
//! re-encodes to the identical bytes and an encoder refuses exactly what the
//! decoder refuses.

use lakko_contacts::{check_display_name, is_format_char};
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, SIGNATURE_LEN};

/// Length of a conversation's MLS group id. Every author signature in
/// ADR-0090 binds the group id, and a fixed width keeps
/// `domain || group_id || body` unambiguous.
pub const GROUP_ID_LEN: usize = 32;

/// Length of an account signature key.
pub const ACCOUNT_KEY_LEN: usize = 32;

/// Length of an author signature (Ed25519).
pub const AUTHOR_SIG_LEN: usize = SIGNATURE_LEN;

/// Why a piece of text was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TextError {
    /// Empty where at least one byte is required.
    #[error("empty")]
    Empty,
    /// More bytes than the field allows.
    #[error("too long")]
    TooLong,
    /// Not valid UTF-8 (decode only).
    #[error("not UTF-8")]
    NotUtf8,
    /// A control character (`Cc`, line breaks included) or a format
    /// character (`Cf`) from the profile table.
    #[error("forbidden character")]
    ForbiddenCharacter,
    /// Breaks the profile display-name rule (contact cards only).
    #[error("not a display name")]
    NotADisplayName,
    /// Not a canonical username (contact cards only).
    #[error("not a canonical username")]
    NotAUsername,
}

impl TextError {
    /// Stable, language-neutral code for the FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::TooLong => "tooLong",
            Self::NotUtf8 => "notUtf8",
            Self::ForbiddenCharacter => "forbiddenCharacter",
            Self::NotADisplayName => "notADisplayName",
            Self::NotAUsername => "notAUsername",
        }
    }
}

/// Why an ADR-0090 content payload (poll, checklist, location, contact card,
/// article) or op was refused. The whole payload is dropped on any of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ContentError {
    /// The bytes ended before the layout did.
    #[error("truncated")]
    Truncated,
    /// Bytes after the layout ended.
    #[error("trailing bytes")]
    TrailingBytes,
    /// The domain separator does not match.
    #[error("wrong domain")]
    WrongDomain,
    /// A version byte this build does not read.
    #[error("unknown version {0}")]
    UnknownVersion(u8),
    /// A flag or mark bit this layout does not define.
    #[error("unknown flag bits")]
    UnknownFlags,
    /// A text field broke its rule.
    #[error("{field}: {reason}")]
    Text {
        /// Which field.
        field: &'static str,
        /// What was wrong with it.
        reason: TextError,
    },
    /// A number or count outside its bound.
    #[error("{field} out of range")]
    OutOfRange {
        /// Which field.
        field: &'static str,
    },
    /// Valid bytes in a form the encoder never produces.
    #[error("not canonical: {what}")]
    NotCanonical {
        /// What was not canonical.
        what: &'static str,
    },
    /// The encoded payload is larger than its bound.
    #[error("too large")]
    TooLarge,
    /// The payload carries no author signature where one is required.
    #[error("author signature missing")]
    SignatureMissing,
    /// The author signature does not verify under the given key.
    #[error("author signature invalid")]
    SignatureInvalid,
    /// The JSON shape around the payload is wrong (unknown field, wrong
    /// type, bad base64).
    #[error("malformed: {what}")]
    Malformed {
        /// What was wrong.
        what: &'static str,
    },
}

impl ContentError {
    /// Stable, language-neutral code for the FFI surfaces. Frozen. A text
    /// error is `"<field>.<reason>"`.
    #[must_use]
    pub fn code(self) -> String {
        match self {
            Self::Truncated => "truncated".into(),
            Self::TrailingBytes => "trailingBytes".into(),
            Self::WrongDomain => "wrongDomain".into(),
            Self::UnknownVersion(_) => "unknownVersion".into(),
            Self::UnknownFlags => "unknownFlags".into(),
            Self::Text { field, reason } => format!("{field}.{}", reason.code()),
            Self::OutOfRange { field } => format!("{field}.outOfRange"),
            Self::NotCanonical { what } => format!("{what}.notCanonical"),
            Self::TooLarge => "tooLarge".into(),
            Self::SignatureMissing => "signatureMissing".into(),
            Self::SignatureInvalid => "signatureInvalid".into(),
            Self::Malformed { what } => format!("{what}.malformed"),
        }
    }
}

/// A bounded big-endian reader over a byte slice. Every read checks the
/// remaining length first; nothing here can panic.
pub struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    /// A reader over `buf`.
    #[must_use]
    pub const fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    /// The next `n` bytes.
    ///
    /// # Errors
    ///
    /// [`ContentError::Truncated`] when fewer than `n` remain.
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], ContentError> {
        if self.buf.len() < n {
            return Err(ContentError::Truncated);
        }
        let (head, tail) = self.buf.split_at(n);
        self.buf = tail;
        Ok(head)
    }

    /// The next `N` bytes as an array.
    ///
    /// # Errors
    ///
    /// [`ContentError::Truncated`].
    pub fn array<const N: usize>(&mut self) -> Result<[u8; N], ContentError> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    /// One byte.
    ///
    /// # Errors
    ///
    /// [`ContentError::Truncated`].
    pub fn u8(&mut self) -> Result<u8, ContentError> {
        Ok(self.array::<1>()?[0])
    }

    /// A big-endian `u16`.
    ///
    /// # Errors
    ///
    /// [`ContentError::Truncated`].
    pub fn u16(&mut self) -> Result<u16, ContentError> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    /// A big-endian `u32`.
    ///
    /// # Errors
    ///
    /// [`ContentError::Truncated`].
    pub fn u32(&mut self) -> Result<u32, ContentError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    /// A big-endian `i64`.
    ///
    /// # Errors
    ///
    /// [`ContentError::Truncated`].
    pub fn i64(&mut self) -> Result<i64, ContentError> {
        Ok(i64::from_be_bytes(self.array()?))
    }

    /// `true` when every byte was read.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Refuse bytes left over.
    ///
    /// # Errors
    ///
    /// [`ContentError::TrailingBytes`].
    pub const fn finish(&self) -> Result<(), ContentError> {
        if self.buf.is_empty() {
            Ok(())
        } else {
            Err(ContentError::TrailingBytes)
        }
    }

    /// UTF-8 text of exactly `len` bytes.
    ///
    /// # Errors
    ///
    /// [`ContentError::Truncated`], or a [`TextError::NotUtf8`] text error
    /// for `field`.
    pub fn str(&mut self, len: usize, field: &'static str) -> Result<&'a str, ContentError> {
        let bytes = self.take(len)?;
        std::str::from_utf8(bytes).map_err(|_| ContentError::Text {
            field,
            reason: TextError::NotUtf8,
        })
    }
}

/// The ADR-0090 display-text rule: `min..=max` bytes of UTF-8 with no control
/// character (`Cc`, so no line break) and no format character from the
/// profile table (`lakko_contacts::FORMAT_CHAR_RANGES`).
///
/// # Errors
///
/// The first [`TextError`] that applies, as a [`ContentError::Text`] for
/// `field`.
pub fn check_display_text(
    text: &str,
    min: usize,
    max: usize,
    field: &'static str,
) -> Result<(), ContentError> {
    let err = |reason| ContentError::Text { field, reason };
    if text.len() < min {
        return Err(err(TextError::Empty));
    }
    if text.len() > max {
        return Err(err(TextError::TooLong));
    }
    if text.chars().any(|c| c.is_control() || is_format_char(c)) {
        return Err(err(TextError::ForbiddenCharacter));
    }
    Ok(())
}

/// The profile display-name rule (`lakko_contacts::check_display_name`), as
/// a [`ContentError`] for `field`.
///
/// # Errors
///
/// [`TextError::NotADisplayName`].
pub fn check_display_name_field(text: &str, field: &'static str) -> Result<(), ContentError> {
    check_display_name(text).map_err(|_| ContentError::Text {
        field,
        reason: TextError::NotADisplayName,
    })
}

/// Push `len` as one byte, or refuse.
pub fn push_len_u8(out: &mut Vec<u8>, len: usize, field: &'static str) -> Result<(), ContentError> {
    let byte = u8::try_from(len).map_err(|_| ContentError::OutOfRange { field })?;
    out.push(byte);
    Ok(())
}

/// Push `len` as a big-endian `u16`, or refuse.
pub fn push_len_u16(
    out: &mut Vec<u8>,
    len: usize,
    field: &'static str,
) -> Result<(), ContentError> {
    let v = u16::try_from(len).map_err(|_| ContentError::OutOfRange { field })?;
    out.extend_from_slice(&v.to_be_bytes());
    Ok(())
}

/// The bytes an author signature covers: `domain || group_id || body`.
fn signed_bytes(domain: &[u8], group_id: &[u8; GROUP_ID_LEN], body: &[u8]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(domain.len() + GROUP_ID_LEN + body.len());
    msg.extend_from_slice(domain);
    msg.extend_from_slice(group_id);
    msg.extend_from_slice(body);
    msg
}

/// Sign `body` for `group_id` under `domain` with the author's account seed.
#[must_use]
pub fn sign_author(
    domain: &[u8],
    group_id: &[u8; GROUP_ID_LEN],
    body: &[u8],
    author_seed: &[u8; 32],
) -> [u8; AUTHOR_SIG_LEN] {
    SigningKey::from_seed(author_seed)
        .sign(&signed_bytes(domain, group_id, body))
        .to_bytes()
}

/// Verify an author signature. A weak (small-order) key never verifies.
///
/// # Errors
///
/// [`ContentError::SignatureInvalid`].
pub fn verify_author(
    domain: &[u8],
    group_id: &[u8; GROUP_ID_LEN],
    body: &[u8],
    author_key: &[u8; ACCOUNT_KEY_LEN],
    sig: &[u8; AUTHOR_SIG_LEN],
) -> Result<(), ContentError> {
    let key = VerifyingKey::from_bytes(author_key).map_err(|_| ContentError::SignatureInvalid)?;
    if key.is_weak() {
        return Err(ContentError::SignatureInvalid);
    }
    let sig = Signature::from_bytes(sig).map_err(|_| ContentError::SignatureInvalid)?;
    key.verify(&signed_bytes(domain, group_id, body), &sig)
        .map_err(|_| ContentError::SignatureInvalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_text_refuses_controls_and_profile_format_chars() {
        assert!(check_display_text("hello", 1, 10, "f").is_ok());
        assert!(check_display_text("", 1, 10, "f").is_err());
        assert!(check_display_text("", 0, 10, "f").is_ok());
        assert!(check_display_text("a\nb", 1, 10, "f").is_err());
        assert!(check_display_text("a\u{200D}b", 1, 10, "f").is_err());
        assert!(check_display_text("a\u{202E}b", 1, 10, "f").is_err());
        assert!(check_display_text("abcdefghijk", 1, 10, "f").is_err());
    }

    #[test]
    fn a_signature_binds_domain_group_and_body() {
        let seed = [7u8; 32];
        let key = SigningKey::from_seed(&seed).verifying_key().to_bytes();
        let g = [1u8; GROUP_ID_LEN];
        let sig = sign_author(b"d\0", &g, b"body", &seed);
        assert!(verify_author(b"d\0", &g, b"body", &key, &sig).is_ok());
        assert!(verify_author(b"e\0", &g, b"body", &key, &sig).is_err());
        assert!(verify_author(b"d\0", &[2u8; GROUP_ID_LEN], b"body", &key, &sig).is_err());
        assert!(verify_author(b"d\0", &g, b"bodz", &key, &sig).is_err());
        assert!(verify_author(b"d\0", &g, b"body", &[0u8; 32], &sig).is_err());
    }
}
