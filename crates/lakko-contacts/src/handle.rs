// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The handle key of a group or channel, and the statements it signs to
//! discovery ([ADR-0087] section 3).
//!
//! # The key
//!
//! A group or channel holds its `@name` with a key that names no account:
//!
//! ```text
//! handle_seed = BLAKE3-derive-key("lakko/handle/write/v1", journal_root_seed)
//! ```
//!
//! `journal_root_seed` is ADR-0057's (`lakko_ds_proof::journal_root_seed`),
//! derived from the owner's account seed and the journal id, so every device
//! of the owner derives the same key with nothing to transfer, two handles of
//! one owner are not linkable, and a restore from the phrase gets it back.
//! Only the owner holds it.
//!
//! # Statements
//!
//! Every write to discovery is one signed binary statement, sent as standard
//! base64 in a JSON body, with no session:
//!
//! ```text
//! HANDLE_STATEMENT_DOMAIN ‖ kind(1) ‖ lookup_key(64) ‖ write_pk(32)
//!   ‖ unix_ms(8 BE) ‖ nonce(16) ‖ fields ‖ signature(64)
//! ```
//!
//! The signature is Ed25519 by `write_pk` over everything before it.
//! `lookup_key` names the row the statement acts on (for a claim, the name
//! being taken). `fields` per kind:
//!
//! | kind | route | fields |
//! |---|---|---|
//! | 1 claim | `POST /discovery/handle/claim` | `visibility(1) content_hash(32)` |
//! | 2 update | `PUT /discovery/handle` | `visibility(1) content_hash(32)` |
//! | 3 replace | `POST /discovery/handle/replace` | `new_lookup_key(64) visibility(1) content_hash(32)` |
//! | 4 rekey | `POST /discovery/handle/rekey` | `new_write_pk(32)` |
//! | 5 retire | `DELETE /discovery/handle` | none |
//!
//! `visibility` is `0` private, `1` public. `content_hash` is
//! [`handle_content_hash`] over the public record and the search terms the
//! request body carries beside the statement (empty for a private name), so
//! the statement covers what is published without carrying it. Every kind
//! has exactly one length; the decoder refuses any other, so one statement
//! has one encoding.
//!
//! # What the verifier checks
//!
//! [`HandleStatement::verify`]: the time window ([`HANDLE_STATEMENT_WINDOW_MS`])
//! first, then the signature under the embedded `write_pk`. Whether that key
//! holds the row, and the nonce replay cache, are discovery's, since both are
//! state.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use core::result::Result as CoreResult;

use lakko_crypto::hash::{blake3, blake3_derive_key_from_material};
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, SIGNATURE_LEN};
use thiserror::Error;

/// BLAKE3 derive-key context of the handle seed. Frozen.
pub const HANDLE_WRITE_DOMAIN: &str = "lakko/handle/write/v1";

/// Domain separator that opens every handle statement. Frozen.
pub const HANDLE_STATEMENT_DOMAIN: &[u8] = b"lakko/handle/statement/v1\x00";

/// Domain of the content hash a statement carries. Frozen.
pub const HANDLE_CONTENT_DOMAIN: &[u8] = b"lakko/handle/content/v1\x00";

/// Length of a lookup key (the 64-byte value discovery keys names on).
pub const LOOKUP_KEY_LEN: usize = 64;

/// Length of an Ed25519 public key or seed.
pub const HANDLE_KEY_LEN: usize = 32;

/// Length of a statement nonce.
pub const HANDLE_NONCE_LEN: usize = 16;

/// Length of a content hash.
pub const CONTENT_HASH_LEN: usize = 32;

/// How far a statement's `unix_ms` may be from the verifier's clock, either
/// way. The ADR-0057 window; chosen, not measured.
pub const HANDLE_STATEMENT_WINDOW_MS: u64 = 120_000;

/// Most search terms a public record is published under: the `@name` plus
/// 64 name terms (ADR-0060). Mirrors
/// [`crate::introduction::MAX_INTRODUCTION_TERMS`].
pub const MAX_HANDLE_TERMS: usize = crate::introduction::MAX_INTRODUCTION_TERMS;

const HEADER_LEN: usize =
    HANDLE_STATEMENT_DOMAIN.len() + 1 + LOOKUP_KEY_LEN + HANDLE_KEY_LEN + 8 + HANDLE_NONCE_LEN;

/// Longest statement (a replace).
pub const HANDLE_STATEMENT_MAX_LEN: usize =
    HEADER_LEN + LOOKUP_KEY_LEN + 1 + CONTENT_HASH_LEN + SIGNATURE_LEN;

/// Shortest statement (a retire).
pub const HANDLE_STATEMENT_MIN_LEN: usize = HEADER_LEN + SIGNATURE_LEN;

/// Derive the handle seed from a journal root seed.
#[must_use]
pub fn handle_write_seed(journal_root_seed: &[u8; HANDLE_KEY_LEN]) -> [u8; HANDLE_KEY_LEN] {
    blake3_derive_key_from_material(HANDLE_WRITE_DOMAIN, journal_root_seed)
}

/// The handle (write) public key of a handle seed.
#[must_use]
pub fn handle_write_public_key(handle_seed: &[u8; HANDLE_KEY_LEN]) -> [u8; HANDLE_KEY_LEN] {
    SigningKey::from_seed(handle_seed)
        .verifying_key()
        .to_bytes()
}

/// Whether a name is published (public) or only reserved (private).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HandleVisibility {
    /// Reserves the name and answers no lookup.
    Private,
    /// Published: a record and search terms.
    Public,
}

impl HandleVisibility {
    /// The wire byte.
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        match self {
            Self::Private => 0,
            Self::Public => 1,
        }
    }

    /// From the wire byte; `None` for any other value.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Private),
            1 => Some(Self::Public),
            _ => None,
        }
    }

    /// `true` for [`Self::Public`].
    #[must_use]
    pub const fn is_public(self) -> bool {
        matches!(self, Self::Public)
    }
}

/// What a statement asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleStatementBody {
    /// Take the name the statement names.
    Claim {
        /// Public or private.
        visibility: HandleVisibility,
        /// [`handle_content_hash`] of the record and terms sent with it.
        content_hash: [u8; CONTENT_HASH_LEN],
    },
    /// Replace the record and terms, set the visibility, renew the lease.
    Update {
        /// Public or private.
        visibility: HandleVisibility,
        /// [`handle_content_hash`] of the record and terms sent with it.
        content_hash: [u8; CONTENT_HASH_LEN],
    },
    /// Move to a new name; the named row is retired.
    Replace {
        /// The lookup key of the new name.
        new_lookup_key: [u8; LOOKUP_KEY_LEN],
        /// Public or private.
        visibility: HandleVisibility,
        /// [`handle_content_hash`] of the record and terms for the new name.
        content_hash: [u8; CONTENT_HASH_LEN],
    },
    /// Move the name's rows to a new handle key (an ownership transfer, an
    /// ADR-0074 rotation).
    Rekey {
        /// The key the rows move to.
        new_write_pk: [u8; HANDLE_KEY_LEN],
    },
    /// Retire the name.
    Retire,
}

impl HandleStatementBody {
    /// The kind byte.
    #[must_use]
    pub const fn kind_byte(&self) -> u8 {
        match self {
            Self::Claim { .. } => 1,
            Self::Update { .. } => 2,
            Self::Replace { .. } => 3,
            Self::Rekey { .. } => 4,
            Self::Retire => 5,
        }
    }

    const fn fields_len(kind: u8) -> Option<usize> {
        match kind {
            1 | 2 => Some(1 + CONTENT_HASH_LEN),
            3 => Some(LOOKUP_KEY_LEN + 1 + CONTENT_HASH_LEN),
            4 => Some(HANDLE_KEY_LEN),
            5 => Some(0),
            _ => None,
        }
    }

    fn encode_fields(&self, out: &mut Vec<u8>) {
        match self {
            Self::Claim {
                visibility,
                content_hash,
            }
            | Self::Update {
                visibility,
                content_hash,
            } => {
                out.push(visibility.to_byte());
                out.extend_from_slice(content_hash);
            },
            Self::Replace {
                new_lookup_key,
                visibility,
                content_hash,
            } => {
                out.extend_from_slice(new_lookup_key);
                out.push(visibility.to_byte());
                out.extend_from_slice(content_hash);
            },
            Self::Rekey { new_write_pk } => out.extend_from_slice(new_write_pk),
            Self::Retire => {},
        }
    }

    fn decode_fields(kind: u8, fields: &[u8]) -> CoreResult<Self, HandleStatementError> {
        let visibility_at = |i: usize| {
            fields
                .get(i)
                .copied()
                .and_then(HandleVisibility::from_byte)
                .ok_or(HandleStatementError::BadVisibility)
        };
        let array = |range: core::ops::Range<usize>| -> CoreResult<Vec<u8>, HandleStatementError> {
            fields
                .get(range)
                .map(<[u8]>::to_vec)
                .ok_or(HandleStatementError::BadLength)
        };
        let hash = |at: usize| -> CoreResult<[u8; CONTENT_HASH_LEN], HandleStatementError> {
            array(at..at + CONTENT_HASH_LEN)?
                .try_into()
                .map_err(|_| HandleStatementError::BadLength)
        };
        match kind {
            1 => Ok(Self::Claim {
                visibility: visibility_at(0)?,
                content_hash: hash(1)?,
            }),
            2 => Ok(Self::Update {
                visibility: visibility_at(0)?,
                content_hash: hash(1)?,
            }),
            3 => Ok(Self::Replace {
                new_lookup_key: array(0..LOOKUP_KEY_LEN)?
                    .try_into()
                    .map_err(|_| HandleStatementError::BadLength)?,
                visibility: visibility_at(LOOKUP_KEY_LEN)?,
                content_hash: hash(LOOKUP_KEY_LEN + 1)?,
            }),
            4 => {
                let new_write_pk: [u8; HANDLE_KEY_LEN] = array(0..HANDLE_KEY_LEN)?
                    .try_into()
                    .map_err(|_| HandleStatementError::BadLength)?;
                VerifyingKey::from_bytes(&new_write_pk)
                    .map_err(|_| HandleStatementError::BadKey)?;
                Ok(Self::Rekey { new_write_pk })
            },
            5 => Ok(Self::Retire),
            other => Err(HandleStatementError::UnknownKind(other)),
        }
    }
}

/// A decoded, not yet verified, handle statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandleStatement {
    /// The name the statement acts on.
    pub lookup_key: [u8; LOOKUP_KEY_LEN],
    /// The handle key that signed it.
    pub write_pk: [u8; HANDLE_KEY_LEN],
    /// The signer's clock, Unix milliseconds.
    pub unix_ms: u64,
    /// Single-use nonce; discovery keeps the ones it has seen.
    pub nonce: [u8; HANDLE_NONCE_LEN],
    /// What is asked.
    pub body: HandleStatementBody,
    /// Ed25519 signature by `write_pk` over everything before it.
    pub signature: [u8; SIGNATURE_LEN],
}

/// Why a handle statement was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum HandleStatementError {
    /// Wrong domain, wrong length for its kind, or truncated.
    #[error("handle statement has the wrong length or domain")]
    BadLength,
    /// The kind byte is not one this crate defines.
    #[error("unknown handle statement kind {0}")]
    UnknownKind(u8),
    /// The visibility byte is neither 0 nor 1.
    #[error("handle statement visibility is not 0 or 1")]
    BadVisibility,
    /// A key is not a valid Ed25519 public key, or a rekey names the same key.
    #[error("handle statement carries an unusable key")]
    BadKey,
    /// `unix_ms` is outside the window around the verifier's clock.
    #[error("handle statement is outside the time window")]
    OutsideWindow,
    /// The signature does not verify under `write_pk`.
    #[error("handle statement signature does not verify")]
    BadSignature,
    /// More search terms than [`MAX_HANDLE_TERMS`], or terms with no record.
    #[error("handle content is malformed")]
    BadContent,
}

impl HandleStatementError {
    /// Stable, language-neutral reason code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::BadLength => "badLength",
            Self::UnknownKind(_) => "unknownKind",
            Self::BadVisibility => "badVisibility",
            Self::BadKey => "badKey",
            Self::OutsideWindow => "outsideWindow",
            Self::BadSignature => "badSignature",
            Self::BadContent => "badContent",
        }
    }
}

/// The hash a claim, update or replace carries over what it publishes: the
/// public record (`None` for a private name) and the lookup keys of its
/// search terms, in the order the body lists them.
///
/// ```text
/// BLAKE3(HANDLE_CONTENT_DOMAIN ‖ has_record(1) ‖ record_len(4 BE) ‖ record
///        ‖ term_count(2 BE) ‖ term*)
/// ```
///
/// # Errors
///
/// [`HandleStatementError::BadContent`] for more than [`MAX_HANDLE_TERMS`]
/// terms, terms without a record, or a record over `u32::MAX` bytes.
pub fn handle_content_hash(
    record: Option<&[u8]>,
    terms: &[[u8; LOOKUP_KEY_LEN]],
) -> CoreResult<[u8; CONTENT_HASH_LEN], HandleStatementError> {
    if terms.len() > MAX_HANDLE_TERMS || (record.is_none() && !terms.is_empty()) {
        return Err(HandleStatementError::BadContent);
    }
    let record_bytes = record.unwrap_or(&[]);
    let record_len =
        u32::try_from(record_bytes.len()).map_err(|_| HandleStatementError::BadContent)?;
    let count = u16::try_from(terms.len()).map_err(|_| HandleStatementError::BadContent)?;
    let mut buf = Vec::with_capacity(
        HANDLE_CONTENT_DOMAIN.len() + 1 + 4 + record_bytes.len() + 2 + terms.len() * LOOKUP_KEY_LEN,
    );
    buf.extend_from_slice(HANDLE_CONTENT_DOMAIN);
    buf.push(u8::from(record.is_some()));
    buf.extend_from_slice(&record_len.to_be_bytes());
    buf.extend_from_slice(record_bytes);
    buf.extend_from_slice(&count.to_be_bytes());
    for term in terms {
        buf.extend_from_slice(term);
    }
    Ok(blake3(&buf))
}

impl HandleStatement {
    /// Sign a statement with the handle seed, at the caller's clock and
    /// nonce (so one function serves a phone, a browser and a test).
    #[must_use]
    pub fn sign(
        handle_seed: &[u8; HANDLE_KEY_LEN],
        lookup_key: [u8; LOOKUP_KEY_LEN],
        body: HandleStatementBody,
        unix_ms: u64,
        nonce: [u8; HANDLE_NONCE_LEN],
    ) -> Self {
        let key = SigningKey::from_seed(handle_seed);
        let mut statement = Self {
            lookup_key,
            write_pk: key.verifying_key().to_bytes(),
            unix_ms,
            nonce,
            body,
            signature: [0u8; SIGNATURE_LEN],
        };
        statement.signature = key.sign(&statement.signed_bytes()).to_bytes();
        statement
    }

    /// Everything the signature covers.
    #[must_use]
    pub fn signed_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HANDLE_STATEMENT_MAX_LEN);
        out.extend_from_slice(HANDLE_STATEMENT_DOMAIN);
        out.push(self.body.kind_byte());
        out.extend_from_slice(&self.lookup_key);
        out.extend_from_slice(&self.write_pk);
        out.extend_from_slice(&self.unix_ms.to_be_bytes());
        out.extend_from_slice(&self.nonce);
        self.body.encode_fields(&mut out);
        out
    }

    /// The wire form.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = self.signed_bytes();
        out.extend_from_slice(&self.signature);
        out
    }

    /// Decode the wire form, fail-closed: the domain, a known kind, the one
    /// length of that kind, the visibility byte and (for a rekey) a valid new
    /// key. Nothing is verified.
    ///
    /// # Errors
    ///
    /// [`HandleStatementError::BadLength`], [`HandleStatementError::UnknownKind`],
    /// [`HandleStatementError::BadVisibility`], [`HandleStatementError::BadKey`].
    pub fn decode(bytes: &[u8]) -> CoreResult<Self, HandleStatementError> {
        if bytes.len() < HANDLE_STATEMENT_MIN_LEN || bytes.len() > HANDLE_STATEMENT_MAX_LEN {
            return Err(HandleStatementError::BadLength);
        }
        let rest = bytes
            .strip_prefix(HANDLE_STATEMENT_DOMAIN)
            .ok_or(HandleStatementError::BadLength)?;
        let (&kind, rest) = rest.split_first().ok_or(HandleStatementError::BadLength)?;
        let fields_len =
            HandleStatementBody::fields_len(kind).ok_or(HandleStatementError::UnknownKind(kind))?;
        if bytes.len() != HEADER_LEN + fields_len + SIGNATURE_LEN {
            return Err(HandleStatementError::BadLength);
        }
        let (lookup_key, rest) = rest.split_at(LOOKUP_KEY_LEN);
        let (write_pk, rest) = rest.split_at(HANDLE_KEY_LEN);
        let (unix_ms, rest) = rest.split_at(8);
        let (nonce, rest) = rest.split_at(HANDLE_NONCE_LEN);
        let (fields, signature) = rest.split_at(fields_len);
        let to_err = |_| HandleStatementError::BadLength;
        let body = HandleStatementBody::decode_fields(kind, fields)?;
        let write_pk: [u8; HANDLE_KEY_LEN] = write_pk.try_into().map_err(to_err)?;
        if let HandleStatementBody::Rekey { new_write_pk } = body {
            if new_write_pk == write_pk {
                return Err(HandleStatementError::BadKey);
            }
        }
        Ok(Self {
            lookup_key: lookup_key.try_into().map_err(to_err)?,
            write_pk,
            unix_ms: u64::from_be_bytes(unix_ms.try_into().map_err(to_err)?),
            nonce: nonce.try_into().map_err(to_err)?,
            body,
            signature: signature.try_into().map_err(to_err)?,
        })
    }

    /// Check the time window, then the signature under `write_pk`.
    ///
    /// # Errors
    ///
    /// [`HandleStatementError::OutsideWindow`], [`HandleStatementError::BadKey`],
    /// [`HandleStatementError::BadSignature`].
    pub fn verify(&self, now_ms: u64) -> CoreResult<(), HandleStatementError> {
        if now_ms.abs_diff(self.unix_ms) > HANDLE_STATEMENT_WINDOW_MS {
            return Err(HandleStatementError::OutsideWindow);
        }
        let key =
            VerifyingKey::from_bytes(&self.write_pk).map_err(|_| HandleStatementError::BadKey)?;
        let signature = Signature::from_bytes(&self.signature)
            .map_err(|_| HandleStatementError::BadSignature)?;
        key.verify(&self.signed_bytes(), &signature)
            .map_err(|_| HandleStatementError::BadSignature)
    }
}

/// The fuzz oracle behind `fuzz_api::handle_statement_decode`: whatever
/// decodes re-encodes to the same bytes and verifies without a panic at its
/// own clock and at the extremes. `None` when the decode refuses.
pub(crate) fn fuzz_decode(bytes: &[u8]) -> Option<bool> {
    let statement = HandleStatement::decode(bytes).ok()?;
    for now in [statement.unix_ms, 0, u64::MAX] {
        let _ = statement.verify(now);
    }
    Some(statement.encode() == bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const SEED: [u8; 32] = [0x31; 32];
    const NOW: u64 = 1_800_000_000_000;

    fn bodies() -> Vec<HandleStatementBody> {
        vec![
            HandleStatementBody::Claim {
                visibility: HandleVisibility::Public,
                content_hash: [1; 32],
            },
            HandleStatementBody::Update {
                visibility: HandleVisibility::Private,
                content_hash: [2; 32],
            },
            HandleStatementBody::Replace {
                new_lookup_key: [3; 64],
                visibility: HandleVisibility::Public,
                content_hash: [4; 32],
            },
            HandleStatementBody::Rekey {
                new_write_pk: handle_write_public_key(&[0x32; 32]),
            },
            HandleStatementBody::Retire,
        ]
    }

    #[test]
    fn every_kind_round_trips_and_verifies() {
        for body in bodies() {
            let s = HandleStatement::sign(&SEED, [9; 64], body, NOW, [7; 16]);
            let wire = s.encode();
            assert!(
                wire.len() >= HANDLE_STATEMENT_MIN_LEN && wire.len() <= HANDLE_STATEMENT_MAX_LEN
            );
            let back = HandleStatement::decode(&wire).expect("decodes");
            assert_eq!(back, s);
            back.verify(NOW + HANDLE_STATEMENT_WINDOW_MS)
                .expect("in window");
            assert_eq!(back.write_pk, handle_write_public_key(&SEED));
        }
    }

    #[test]
    fn the_window_and_the_signature_are_checked() {
        let s = HandleStatement::sign(&SEED, [9; 64], HandleStatementBody::Retire, NOW, [7; 16]);
        assert_eq!(
            s.verify(NOW + HANDLE_STATEMENT_WINDOW_MS + 1),
            Err(HandleStatementError::OutsideWindow)
        );
        assert_eq!(
            s.verify(NOW - HANDLE_STATEMENT_WINDOW_MS - 1),
            Err(HandleStatementError::OutsideWindow)
        );
        let mut wire = s.encode();
        let last = wire.len() - 1;
        wire[last] ^= 1;
        assert_eq!(
            HandleStatement::decode(&wire).unwrap().verify(NOW),
            Err(HandleStatementError::BadSignature)
        );
        // Tampering the fields breaks the signature too.
        let mut wire = s.encode();
        wire[HANDLE_STATEMENT_DOMAIN.len() + 1] ^= 1;
        assert_eq!(
            HandleStatement::decode(&wire).unwrap().verify(NOW),
            Err(HandleStatementError::BadSignature)
        );
    }

    #[test]
    fn malformed_statements_are_refused() {
        let s = HandleStatement::sign(
            &SEED,
            [9; 64],
            HandleStatementBody::Claim {
                visibility: HandleVisibility::Public,
                content_hash: [0; 32],
            },
            NOW,
            [7; 16],
        )
        .encode();
        // Wrong length for the kind.
        assert_eq!(
            HandleStatement::decode(&s[..s.len() - 1]),
            Err(HandleStatementError::BadLength)
        );
        let mut longer = s.clone();
        longer.push(0);
        assert_eq!(
            HandleStatement::decode(&longer),
            Err(HandleStatementError::BadLength)
        );
        // Unknown kind.
        let mut k = s.clone();
        k[HANDLE_STATEMENT_DOMAIN.len()] = 9;
        assert_eq!(
            HandleStatement::decode(&k),
            Err(HandleStatementError::UnknownKind(9))
        );
        // Bad visibility.
        let mut v = s.clone();
        v[HEADER_LEN] = 2;
        assert_eq!(
            HandleStatement::decode(&v),
            Err(HandleStatementError::BadVisibility)
        );
        // Wrong domain.
        let mut d = s;
        d[0] ^= 1;
        assert_eq!(
            HandleStatement::decode(&d),
            Err(HandleStatementError::BadLength)
        );
        // A rekey to the same key.
        let me = handle_write_public_key(&SEED);
        let same = HandleStatement::sign(
            &SEED,
            [9; 64],
            HandleStatementBody::Rekey { new_write_pk: me },
            NOW,
            [7; 16],
        )
        .encode();
        assert_eq!(
            HandleStatement::decode(&same),
            Err(HandleStatementError::BadKey)
        );
    }

    #[test]
    fn the_content_hash_binds_record_and_terms() {
        let a = handle_content_hash(Some(b"record"), &[[1; 64]]).unwrap();
        assert_ne!(a, handle_content_hash(Some(b"recore"), &[[1; 64]]).unwrap());
        assert_ne!(a, handle_content_hash(Some(b"record"), &[[2; 64]]).unwrap());
        assert_ne!(a, handle_content_hash(Some(b"record"), &[]).unwrap());
        // A private name (no record) differs from an empty record.
        assert_ne!(
            handle_content_hash(None, &[]).unwrap(),
            handle_content_hash(Some(b""), &[]).unwrap()
        );
        assert_eq!(
            handle_content_hash(None, &[[1; 64]]),
            Err(HandleStatementError::BadContent)
        );
        let too_many = vec![[0u8; 64]; MAX_HANDLE_TERMS + 1];
        assert_eq!(
            handle_content_hash(Some(b"r"), &too_many),
            Err(HandleStatementError::BadContent)
        );
    }

    #[test]
    fn the_seed_derivation_is_pinned_and_distinct_per_root() {
        let a = handle_write_seed(&[0x42; 32]);
        assert_ne!(a, handle_write_seed(&[0x43; 32]));
        assert_ne!(a, [0x42; 32]);
        assert_eq!(
            a,
            blake3_derive_key_from_material("lakko/handle/write/v1", &[0x42; 32])
        );
    }

    proptest! {
        #[test]
        fn decode_never_panics_and_canonical(bytes in proptest::collection::vec(any::<u8>(), 0..400)) {
            if let Ok(s) = HandleStatement::decode(&bytes) {
                prop_assert_eq!(s.encode(), bytes);
            }
        }
    }
}
