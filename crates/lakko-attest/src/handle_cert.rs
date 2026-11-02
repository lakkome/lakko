// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Handle certificate ([ADR-0087] section 3).
//!
//! A group or channel holds its `@name` with a **handle key** of its own, an
//! Ed25519 key derived from the ADR-0057 journal root seed that names no
//! account. When discovery accepts a claim (or a move to a new name, or a
//! move of the key) it answers with a *handle certificate*: its signature,
//! under the same dedicated key that signs username certificates, over the
//! lookup key of the name and the handle key that holds it.
//!
//! Members carry the certificate in the conversation-handle envelope
//! (`lakko_contacts::conversation_handle`). A member verifies it with one
//! OPRF evaluation over the shown name, so a private group, whose name no
//! lookup can check, cannot show its members a name it does not hold.
//!
//! ```text
//! wire           = version(1) ‖ lookup_key(64) ‖ write_pk(32)
//!                  ‖ issued_at_unix_millis(8, big-endian i64) ‖ Ed25519_sig(64)
//!                                                                  = 169 bytes
//! signing input  = DOMAIN ‖ version ‖ lookup_key ‖ write_pk ‖ issued_at
//! DOMAIN         = b"lakko/handle-cert/v1"
//! ```
//!
//! The domain separator is distinct from every other kind in this crate, so
//! a handle certificate and a username certificate are non-interchangeable
//! even though discovery signs both with one key; the lengths differ too
//! (169 and 153 bytes).
//!
//! **No freshness check.** A certificate says the name was held under the
//! key when it was issued. Whether the name is still held is what the
//! envelope's stamp and a later lookup say; a certificate that outlived its
//! name is the residual ADR-0087 records for a lapsed lease.
//!
//! [ADR-0087]: ../../../docs/adr/0087-group-and-channel-handles-and-privacy.md

use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, SIGNATURE_LEN};
use thiserror::Error;

use crate::username_cert::OPRF_HASH_LEN;

/// Domain separator mixed into the signing input. Distinct from every other
/// attestation domain of this crate. Bumping it (and [`VERSION`])
/// invalidates every certificate issued before.
const DOMAIN: &[u8] = b"lakko/handle-cert/v1";

/// Wire-format version. The verifier refuses any other value.
const VERSION: u8 = 1;

/// Length of the handle (write) public key.
pub const HANDLE_WRITE_KEY_LEN: usize = 32;

/// Length of the big-endian `i64` `issued_at` timestamp.
const ISSUED_AT_LEN: usize = 8;

/// Total serialized handle-certificate length.
pub const HANDLE_CERT_LEN: usize =
    1 + OPRF_HASH_LEN + HANDLE_WRITE_KEY_LEN + ISSUED_AT_LEN + SIGNATURE_LEN;

const _: () = assert!(HANDLE_CERT_LEN == 169);

/// Why a handle certificate was rejected.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum HandleCertError {
    /// The byte slice is not exactly [`HANDLE_CERT_LEN`] long.
    #[error("handle certificate has wrong length: {got} bytes")]
    BadLength {
        /// The actual length received.
        got: usize,
    },
    /// The version byte is not the one this crate writes.
    #[error("unsupported handle certificate version: {0}")]
    BadVersion(u8),
    /// The Ed25519 signature did not verify (tampered field, wrong key, a blob
    /// of another kind, or malformed signature bytes).
    #[error("handle certificate signature is invalid")]
    BadSignature,
    /// The certified lookup key is not the lookup key of the shown name.
    #[error("handle certificate does not match the shown name")]
    NameMismatch,
    /// The certified handle key is not the expected one.
    #[error("handle certificate does not match the handle key")]
    KeyMismatch,
}

impl HandleCertError {
    /// Stable, language-neutral reason code, for an FFI surface.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::BadLength { .. } => "badLength",
            Self::BadVersion(_) => "badVersion",
            Self::BadSignature => "badSignature",
            Self::NameMismatch => "nameMismatch",
            Self::KeyMismatch => "keyMismatch",
        }
    }
}

/// The authenticated claims of a handle certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandleCertificate {
    lookup_key: [u8; OPRF_HASH_LEN],
    write_pk: [u8; HANDLE_WRITE_KEY_LEN],
    issued_at_unix_millis: i64,
}

impl HandleCertificate {
    /// Construct claims to be signed.
    #[must_use]
    pub const fn new(
        lookup_key: [u8; OPRF_HASH_LEN],
        write_pk: [u8; HANDLE_WRITE_KEY_LEN],
        issued_at_unix_millis: i64,
    ) -> Self {
        Self {
            lookup_key,
            write_pk,
            issued_at_unix_millis,
        }
    }

    /// The certified lookup key of the name.
    #[must_use]
    pub const fn lookup_key(&self) -> &[u8; OPRF_HASH_LEN] {
        &self.lookup_key
    }

    /// The handle key that holds the name.
    #[must_use]
    pub const fn write_pk(&self) -> &[u8; HANDLE_WRITE_KEY_LEN] {
        &self.write_pk
    }

    /// When discovery issued the certificate, Unix milliseconds.
    /// Informational only.
    #[must_use]
    pub const fn issued_at_unix_millis(&self) -> i64 {
        self.issued_at_unix_millis
    }

    fn signing_input(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(
            DOMAIN.len() + 1 + OPRF_HASH_LEN + HANDLE_WRITE_KEY_LEN + ISSUED_AT_LEN,
        );
        buf.extend_from_slice(DOMAIN);
        buf.push(VERSION);
        buf.extend_from_slice(&self.lookup_key);
        buf.extend_from_slice(&self.write_pk);
        buf.extend_from_slice(&self.issued_at_unix_millis.to_be_bytes());
        buf
    }

    /// Serialize and sign these claims. Called by `services/discovery`.
    #[must_use]
    pub fn sign(&self, signing: &SigningKey) -> [u8; HANDLE_CERT_LEN] {
        let sig = signing.sign(&self.signing_input());
        let mut out = [0u8; HANDLE_CERT_LEN];
        out[0] = VERSION;
        let mut off = 1;
        out[off..off + OPRF_HASH_LEN].copy_from_slice(&self.lookup_key);
        off += OPRF_HASH_LEN;
        out[off..off + HANDLE_WRITE_KEY_LEN].copy_from_slice(&self.write_pk);
        off += HANDLE_WRITE_KEY_LEN;
        out[off..off + ISSUED_AT_LEN].copy_from_slice(&self.issued_at_unix_millis.to_be_bytes());
        off += ISSUED_AT_LEN;
        out[off..off + SIGNATURE_LEN].copy_from_slice(&sig.to_bytes());
        out
    }

    /// Parse the wire form and verify the signature against `verifying`.
    /// Does not check what the certificate binds; callers use
    /// [`verify_handle_binding`].
    ///
    /// # Errors
    ///
    /// [`HandleCertError::BadLength`] / [`HandleCertError::BadVersion`] on a
    /// malformed blob; [`HandleCertError::BadSignature`] if the signature is
    /// malformed or does not verify.
    pub fn open(bytes: &[u8], verifying: &VerifyingKey) -> Result<Self, HandleCertError> {
        if bytes.len() != HANDLE_CERT_LEN {
            return Err(HandleCertError::BadLength { got: bytes.len() });
        }
        if bytes[0] != VERSION {
            return Err(HandleCertError::BadVersion(bytes[0]));
        }
        let mut off = 1;
        let mut lookup_key = [0u8; OPRF_HASH_LEN];
        lookup_key.copy_from_slice(&bytes[off..off + OPRF_HASH_LEN]);
        off += OPRF_HASH_LEN;
        let mut write_pk = [0u8; HANDLE_WRITE_KEY_LEN];
        write_pk.copy_from_slice(&bytes[off..off + HANDLE_WRITE_KEY_LEN]);
        off += HANDLE_WRITE_KEY_LEN;
        let mut issued = [0u8; ISSUED_AT_LEN];
        issued.copy_from_slice(&bytes[off..off + ISSUED_AT_LEN]);
        off += ISSUED_AT_LEN;
        let sig = Signature::from_bytes(&bytes[off..off + SIGNATURE_LEN])
            .map_err(|_| HandleCertError::BadSignature)?;
        let claims = Self {
            lookup_key,
            write_pk,
            issued_at_unix_millis: i64::from_be_bytes(issued),
        };
        verifying
            .verify(&claims.signing_input(), &sig)
            .map_err(|_| HandleCertError::BadSignature)?;
        Ok(claims)
    }
}

/// Verify a handle certificate and its binding to the lookup key of the
/// shown name (which the member computes with one OPRF evaluation). Returns
/// the claims, whose [`HandleCertificate::write_pk`] is the key the
/// conversation-handle envelope's signature must verify under.
///
/// # Errors
///
/// [`HandleCertificate::open`] errors, then [`HandleCertError::NameMismatch`].
pub fn verify_handle_binding(
    bytes: &[u8],
    verifying: &VerifyingKey,
    expected_lookup_key: &[u8; OPRF_HASH_LEN],
) -> Result<HandleCertificate, HandleCertError> {
    let cert = HandleCertificate::open(bytes, verifying)?;
    if &cert.lookup_key != expected_lookup_key {
        return Err(HandleCertError::NameMismatch);
    }
    Ok(cert)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{UsernameCertError, UsernameCertificate};

    fn key() -> SigningKey {
        SigningKey::from_seed(&[0x21; 32])
    }

    fn sample() -> HandleCertificate {
        HandleCertificate::new([0xAB; 64], [0xCD; 32], 1_700_000_000_000)
    }

    #[test]
    fn sign_then_verify_binding_roundtrips() {
        let sk = key();
        let wire = sample().sign(&sk);
        assert_eq!(wire.len(), HANDLE_CERT_LEN);
        let got =
            verify_handle_binding(&wire, &sk.verifying_key(), &[0xAB; 64]).expect("valid cert");
        assert_eq!(got, sample());
        assert_eq!(got.write_pk(), &[0xCD; 32]);
        assert_eq!(got.issued_at_unix_millis(), 1_700_000_000_000);
    }

    #[test]
    fn wire_layout_is_pinned() {
        let wire = sample().sign(&key());
        assert_eq!(wire[0], 1);
        assert_eq!(&wire[1..65], &[0xAB; 64]);
        assert_eq!(&wire[65..97], &[0xCD; 32]);
        assert_eq!(&wire[97..105], &1_700_000_000_000_i64.to_be_bytes());
    }

    #[test]
    fn tampering_any_byte_breaks_it() {
        let sk = key();
        let wire = sample().sign(&sk);
        for i in 0..HANDLE_CERT_LEN {
            let mut bad = wire;
            bad[i] ^= 0x01;
            assert!(
                HandleCertificate::open(&bad, &sk.verifying_key()).is_err(),
                "byte {i}"
            );
        }
    }

    #[test]
    fn a_wrong_name_or_key_or_length_is_refused() {
        let sk = key();
        let wire = sample().sign(&sk);
        assert_eq!(
            verify_handle_binding(&wire, &sk.verifying_key(), &[0xAC; 64]).err(),
            Some(HandleCertError::NameMismatch)
        );
        assert_eq!(
            verify_handle_binding(
                &wire,
                &SigningKey::from_seed(&[0x22; 32]).verifying_key(),
                &[0xAB; 64]
            )
            .err(),
            Some(HandleCertError::BadSignature)
        );
        assert_eq!(
            HandleCertificate::open(&wire[..168], &sk.verifying_key()).err(),
            Some(HandleCertError::BadLength { got: 168 })
        );
        let mut v2 = wire;
        v2[0] = 2;
        assert_eq!(
            HandleCertificate::open(&v2, &sk.verifying_key()).err(),
            Some(HandleCertError::BadVersion(2))
        );
    }

    /// One discovery key signs both kinds; neither opens as the other.
    #[test]
    fn a_username_certificate_never_opens_as_a_handle_certificate() {
        let sk = key();
        let username = UsernameCertificate::new([0xAB; 64], [0xCD; 16], 1).sign(&sk);
        assert_eq!(
            HandleCertificate::open(&username, &sk.verifying_key()).err(),
            Some(HandleCertError::BadLength { got: 153 })
        );
        // Same length forced: pad a username blob to 169 bytes is not a
        // signature either way; and a handle blob cut to 153 fails the
        // username parser on its signature.
        let handle = sample().sign(&sk);
        let mut cut = [0u8; 153];
        cut.copy_from_slice(&handle[..153]);
        assert_eq!(
            UsernameCertificate::open(&cut, &sk.verifying_key()).err(),
            Some(UsernameCertError::BadSignature)
        );
    }
}
