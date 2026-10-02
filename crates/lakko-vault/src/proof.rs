// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The request proof of the storage routes (ADR-0064 section 1, ADR-0065
//! section 6).
//!
//! A vault's writes, reads and delete are signed with the vault's write key
//! ([`crate::keys::VaultRoot::vault_write_seed`]); `DELETE /blob/{hex}` is
//! signed with the blob's delete key
//! ([`crate::keys::VaultRoot::blob_delete_seed`]). The server holds only the
//! public half. No session token is accepted on these routes, so nothing in
//! a request names the account.
//!
//! ## Wire
//!
//! One request header:
//!
//! ```text
//! x-lakko-storage-proof: v1.<unix_ms>.<sig_b64url>
//! ```
//!
//! `unix_ms` is an ASCII decimal without a sign or a leading zero; the
//! signature is 64 bytes in base64url without padding. One proof has exactly
//! one header value: whatever parses re-encodes to the identical text.
//!
//! ## Canonical bytes
//!
//! ```text
//! "lakko/storage/proof/v1" || 0x00 || method || 0x00 || path || 0x00
//!   || BLAKE3(body)(32) || unix_ms(u64 BE)
//! ```
//!
//! `path` is the service path (`/vault/<hex>/head`), never a reverse-proxy
//! prefix, so every client signs the same bytes. The NUL separators keep
//! `method || path` unambiguous; a method or path that itself contains a NUL
//! is refused on both sides rather than signed. The body enters as its
//! BLAKE3 digest. The storage routes take no query string, so none is bound.
//!
//! ## No nonce
//!
//! Unlike the Delivery Service proof there is no nonce and the server keeps
//! no replay cache. Within the window a captured request can be sent again,
//! and ADR-0064 section 1 states why that is accepted: a replayed write is
//! refused by its version (a write is accepted only at `current + 1`), a
//! replayed read returns ciphertext to someone who already saw the request,
//! and a replayed delete deletes what is already gone.
//!
//! ## What the verifier checks, in order
//!
//! 1. the header parses (bounded, fail-closed);
//! 2. `unix_ms` lies within [`PROOF_WINDOW_MS`] of the verifier's clock;
//! 3. the signature verifies under the stored public key.
//!
//! The window comes before the signature so a stale proof costs no signature
//! check. This module is pure: the caller supplies the clock.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use lakko_crypto::hash;
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, SIGNATURE_LEN};
use thiserror::Error;

/// The request header the proof travels in (lowercase, as HTTP/2 carries it).
pub const PROOF_HEADER_NAME: &str = "x-lakko-storage-proof";

/// Domain separator that opens the canonical bytes. Frozen.
pub const PROOF_DOMAIN: &[u8] = b"lakko/storage/proof/v1";

/// How far from the verifier's clock a proof's `unix_ms` may lie, either
/// way: 120 seconds, the window of the Delivery Service proof.
pub const PROOF_WINDOW_MS: u64 = 120_000;

/// Upper bound on the header value's length; anything longer is refused
/// unparsed. The longest valid value is 110 bytes (`v1.`, 20 digits, `.`,
/// 86 base64url characters).
pub const MAX_HEADER_LEN: usize = 128;

/// Length of an Ed25519 public key.
pub const PUBLIC_KEY_LEN: usize = 32;

const VERSION: &str = "v1";
const SEP: u8 = 0;
/// A `u64` in decimal is at most 20 digits.
const MAX_DECIMAL_DIGITS: usize = 20;

/// Why a proof was not produced or was refused. On the server every variant
/// maps to one response, so a caller learns nothing from which one it hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProofError {
    /// The method or the path contains a NUL byte, which would make the
    /// canonical bytes ambiguous.
    #[error("request method or path cannot be signed")]
    RequestNotCanonical,
    /// The header did not parse (wrong shape, wrong lengths, bad base64).
    #[error("storage proof header malformed")]
    Malformed,
    /// A version this crate does not speak.
    #[error("storage proof header version unknown")]
    UnknownVersion,
    /// `unix_ms` is outside the window around the verifier's clock.
    #[error("storage proof outside the time window")]
    OutsideWindow,
    /// The signature does not verify under the public key, or the public
    /// key is not a usable Ed25519 key.
    #[error("storage proof signature invalid")]
    BadSignature,
}

impl ProofError {
    /// Stable, language-neutral reason code for the FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::RequestNotCanonical => "requestNotCanonical",
            Self::Malformed => "malformed",
            Self::UnknownVersion => "unknownVersion",
            Self::OutsideWindow => "outsideWindow",
            Self::BadSignature => "badSignature",
        }
    }
}

/// A parsed proof header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageProof {
    /// The signer's clock at signing, unix milliseconds.
    pub unix_ms: u64,
    /// Ed25519 over the canonical bytes.
    pub signature: [u8; SIGNATURE_LEN],
}

/// The bytes a proof signs.
///
/// # Errors
///
/// [`ProofError::RequestNotCanonical`] when `method` or `path` contains a
/// NUL byte.
pub fn canonical_bytes(
    method: &str,
    path: &str,
    body: &[u8],
    unix_ms: u64,
) -> Result<Vec<u8>, ProofError> {
    if method.as_bytes().contains(&SEP) || path.as_bytes().contains(&SEP) {
        return Err(ProofError::RequestNotCanonical);
    }
    let digest = hash::blake3(body);
    let mut out =
        Vec::with_capacity(PROOF_DOMAIN.len() + method.len() + path.len() + 3 + digest.len() + 8);
    out.extend_from_slice(PROOF_DOMAIN);
    out.push(SEP);
    out.extend_from_slice(method.as_bytes());
    out.push(SEP);
    out.extend_from_slice(path.as_bytes());
    out.push(SEP);
    out.extend_from_slice(&digest);
    out.extend_from_slice(&unix_ms.to_be_bytes());
    Ok(out)
}

/// The public half of a write or delete seed: what a vault claim and a blob
/// upload hand to the server.
#[must_use]
pub fn public_key(seed: &[u8; 32]) -> [u8; PUBLIC_KEY_LEN] {
    SigningKey::from_seed(seed).verifying_key().to_bytes()
}

/// Whether the server should accept `public_key` at a claim or an upload.
///
/// It must be a valid Ed25519 point that is not of small order. A signature
/// under a small-order key can be forged for any message, and [`verify`]
/// refuses such a key for the same reason.
#[must_use]
pub fn is_usable_public_key(public_key: &[u8; PUBLIC_KEY_LEN]) -> bool {
    usable_key(public_key).is_some()
}

fn usable_key(public_key: &[u8; PUBLIC_KEY_LEN]) -> Option<VerifyingKey> {
    let key = VerifyingKey::from_bytes(public_key).ok()?;
    (!key.is_weak()).then_some(key)
}

/// Sign a request with `seed` at `now_ms` and return the header value.
///
/// `method` is upper case (`GET`, `PUT`, `DELETE`); `path` is the service
/// path; `body` is the request body, or empty.
///
/// # Errors
///
/// [`ProofError::RequestNotCanonical`] when `method` or `path` contains a
/// NUL byte.
pub fn sign(
    seed: &[u8; 32],
    method: &str,
    path: &str,
    body: &[u8],
    now_ms: u64,
) -> Result<String, ProofError> {
    let bytes = canonical_bytes(method, path, body, now_ms)?;
    let proof = StorageProof {
        unix_ms: now_ms,
        signature: SigningKey::from_seed(seed).sign(&bytes).to_bytes(),
    };
    Ok(proof.encode())
}

/// Parse a header value. See [`StorageProof::parse`].
///
/// # Errors
///
/// [`ProofError::UnknownVersion`] or [`ProofError::Malformed`].
pub fn parse(header: &str) -> Result<StorageProof, ProofError> {
    StorageProof::parse(header)
}

/// Verify the header value `header` over a request, under `public_key`, with
/// `now_ms` the verifier's clock.
///
/// # Errors
///
/// [`ProofError::Malformed`] or [`ProofError::UnknownVersion`] when the
/// header does not parse, then [`ProofError::OutsideWindow`], then
/// [`ProofError::RequestNotCanonical`] or [`ProofError::BadSignature`].
pub fn verify(
    public_key: &[u8; PUBLIC_KEY_LEN],
    method: &str,
    path: &str,
    body: &[u8],
    header: &str,
    now_ms: u64,
) -> Result<(), ProofError> {
    StorageProof::parse(header)?.verify(public_key, method, path, body, now_ms)
}

impl StorageProof {
    /// The header value.
    #[must_use]
    pub fn encode(&self) -> String {
        format!(
            "{VERSION}.{}.{}",
            self.unix_ms,
            URL_SAFE_NO_PAD.encode(self.signature)
        )
    }

    /// Parse a header value. Bounded ([`MAX_HEADER_LEN`]) and fail-closed:
    /// exactly three dot-separated fields, the version, a canonical decimal
    /// time and a 64-byte signature in base64url without padding. Nothing
    /// else.
    ///
    /// # Errors
    ///
    /// [`ProofError::UnknownVersion`] on a version this crate does not
    /// speak, [`ProofError::Malformed`] on anything else that does not parse.
    pub fn parse(header: &str) -> Result<Self, ProofError> {
        if header.len() > MAX_HEADER_LEN {
            return Err(ProofError::Malformed);
        }
        let mut parts = header.split('.');
        let version = parts.next().ok_or(ProofError::Malformed)?;
        let unix_ms = parts.next().ok_or(ProofError::Malformed)?;
        let signature = parts.next().ok_or(ProofError::Malformed)?;
        if parts.next().is_some() {
            return Err(ProofError::Malformed);
        }
        if version != VERSION {
            return Err(ProofError::UnknownVersion);
        }
        let unix_ms = parse_decimal(unix_ms)?;
        // The engine refuses padding and non-zero trailing bits, so one
        // signature has one spelling.
        let signature_bytes = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| ProofError::Malformed)?;
        let signature: [u8; SIGNATURE_LEN] = signature_bytes
            .as_slice()
            .try_into()
            .map_err(|_| ProofError::Malformed)?;
        Ok(Self { unix_ms, signature })
    }

    /// Verify this proof over a request, under `public_key`, with `now_ms`
    /// the verifier's clock. The window is checked before the signature.
    ///
    /// # Errors
    ///
    /// [`ProofError::OutsideWindow`], [`ProofError::RequestNotCanonical`] or
    /// [`ProofError::BadSignature`].
    pub fn verify(
        &self,
        public_key: &[u8; PUBLIC_KEY_LEN],
        method: &str,
        path: &str,
        body: &[u8],
        now_ms: u64,
    ) -> Result<(), ProofError> {
        if now_ms.abs_diff(self.unix_ms) > PROOF_WINDOW_MS {
            return Err(ProofError::OutsideWindow);
        }
        let bytes = canonical_bytes(method, path, body, self.unix_ms)?;
        let key = usable_key(public_key).ok_or(ProofError::BadSignature)?;
        let signature =
            Signature::from_bytes(&self.signature).map_err(|_| ProofError::BadSignature)?;
        key.verify(&bytes, &signature)
            .map_err(|_| ProofError::BadSignature)
    }
}

/// A canonical decimal: 1..=20 ASCII digits, no sign, no leading zero (the
/// single digit `0` aside), fits a `u64`.
fn parse_decimal(field: &str) -> Result<u64, ProofError> {
    let digits = field.as_bytes();
    let canonical = !digits.is_empty()
        && digits.len() <= MAX_DECIMAL_DIGITS
        && digits.iter().all(u8::is_ascii_digit)
        && (digits.len() == 1 || digits.first() != Some(&b'0'));
    if !canonical {
        return Err(ProofError::Malformed);
    }
    field.parse::<u64>().map_err(|_| ProofError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex;

    const SEED: [u8; 32] = [0x11; 32];
    const PATH: &str = "/vault/00ff/head";
    const NOW: u64 = 1_700_000_000_000;

    #[test]
    fn constants_are_frozen() {
        assert_eq!(PROOF_HEADER_NAME, "x-lakko-storage-proof");
        assert_eq!(PROOF_DOMAIN, b"lakko/storage/proof/v1");
        assert_eq!(PROOF_WINDOW_MS, 120_000);
    }

    #[test]
    fn round_trips_through_the_header() {
        let header = sign(&SEED, "PUT", PATH, b"sealed head", NOW).unwrap();
        assert!(header.starts_with("v1.1700000000000."));
        assert!(header.len() <= MAX_HEADER_LEN);
        let proof = parse(&header).unwrap();
        assert_eq!(proof.unix_ms, NOW);
        assert_eq!(proof.encode(), header);
        verify(
            &public_key(&SEED),
            "PUT",
            PATH,
            b"sealed head",
            &header,
            NOW + 500,
        )
        .unwrap();
    }

    #[test]
    fn the_longest_header_fits_the_bound() {
        let header = sign(&SEED, "GET", PATH, b"", u64::MAX).unwrap();
        assert_eq!(header.len(), 3 + 20 + 1 + 86);
        assert!(header.len() <= MAX_HEADER_LEN);
        assert_eq!(parse(&header).unwrap().unix_ms, u64::MAX);
    }

    #[test]
    fn every_bound_field_is_bound() {
        let pk = public_key(&SEED);
        let header = sign(&SEED, "PUT", PATH, b"body", NOW).unwrap();
        verify(&pk, "PUT", PATH, b"body", &header, NOW).unwrap();
        let bad = Err(ProofError::BadSignature);
        assert_eq!(verify(&pk, "GET", PATH, b"body", &header, NOW), bad);
        assert_eq!(
            verify(&pk, "PUT", "/vault/00ff/chunk/0", b"body", &header, NOW),
            bad
        );
        assert_eq!(verify(&pk, "PUT", PATH, b"bodz", &header, NOW), bad);
        assert_eq!(verify(&pk, "PUT", PATH, b"", &header, NOW), bad);
        // Another key.
        let other = public_key(&[0x22; 32]);
        assert_eq!(verify(&other, "PUT", PATH, b"body", &header, NOW), bad);
        // Another time: the signature covers it.
        let mut moved = parse(&header).unwrap();
        moved.unix_ms += 1;
        assert_eq!(moved.verify(&pk, "PUT", PATH, b"body", NOW), bad);
        // An altered signature.
        let mut altered = parse(&header).unwrap();
        altered.signature[0] ^= 1;
        assert_eq!(altered.verify(&pk, "PUT", PATH, b"body", NOW), bad);
    }

    #[test]
    fn a_vault_proof_is_not_a_blob_delete_proof() {
        // Same key, same time: the method and the path are both bound.
        let pk = public_key(&SEED);
        let header = sign(&SEED, "DELETE", "/vault/00ff", b"", NOW).unwrap();
        verify(&pk, "DELETE", "/vault/00ff", b"", &header, NOW).unwrap();
        assert_eq!(
            verify(&pk, "DELETE", "/blob/00ff", b"", &header, NOW),
            Err(ProofError::BadSignature)
        );
    }

    #[test]
    fn the_window_is_checked_before_the_signature() {
        let pk = public_key(&SEED);
        let header = sign(&SEED, "GET", PATH, b"", NOW).unwrap();
        verify(&pk, "GET", PATH, b"", &header, NOW + PROOF_WINDOW_MS).unwrap();
        verify(&pk, "GET", PATH, b"", &header, NOW - PROOF_WINDOW_MS).unwrap();
        assert_eq!(
            verify(&pk, "GET", PATH, b"", &header, NOW + PROOF_WINDOW_MS + 1),
            Err(ProofError::OutsideWindow)
        );
        assert_eq!(
            verify(&pk, "GET", PATH, b"", &header, NOW - PROOF_WINDOW_MS - 1),
            Err(ProofError::OutsideWindow)
        );
        // A stale proof with a wrong signature and a wrong key reports the
        // window, not the signature.
        let mut stale = parse(&header).unwrap();
        stale.signature = [0; SIGNATURE_LEN];
        assert_eq!(
            stale.verify(&[0u8; 32], "GET", PATH, b"", NOW + PROOF_WINDOW_MS + 1),
            Err(ProofError::OutsideWindow)
        );
        // The extremes of the clock do not overflow.
        assert_eq!(
            verify(&pk, "GET", PATH, b"", &header, 0),
            Err(ProofError::OutsideWindow)
        );
        assert_eq!(
            verify(&pk, "GET", PATH, b"", &header, u64::MAX),
            Err(ProofError::OutsideWindow)
        );
    }

    #[test]
    fn parse_is_bounded_and_fail_closed() {
        let good = sign(&SEED, "GET", PATH, b"", 12).unwrap();
        assert!(parse(&good).is_ok());
        let sig = good.rsplit('.').next().unwrap().to_owned();
        let malformed = |value: &str| assert_eq!(parse(value), Err(ProofError::Malformed));

        malformed("");
        malformed("v1");
        malformed("v1.12");
        malformed(&format!("{good}.extra"));
        malformed(&format!("v1..{sig}"));
        malformed(&format!("v1.+12.{sig}"));
        malformed(&format!("v1.-12.{sig}"));
        malformed(&format!("v1. 12.{sig}"));
        malformed(&format!("v1.012.{sig}"));
        malformed(&format!("v1.00.{sig}"));
        malformed(&format!("v1.18446744073709551616.{sig}"));
        malformed(&format!("v1.999999999999999999999.{sig}"));
        // A signature of the wrong length, with padding, in the standard
        // alphabet, or with stray trailing bits.
        malformed("v1.12.AAAA");
        malformed(&format!("v1.12.{sig}=="));
        malformed(&format!("v1.12.{}", &sig[..sig.len() - 1]));
        malformed(&format!("v1.12.{sig}A"));
        malformed(&format!("v1.12.{}", "+".repeat(86)));
        let mut trailing_bits = "A".repeat(85);
        trailing_bits.push('B');
        malformed(&format!("v1.12.{trailing_bits}"));
        // Over the length bound, however well formed the start.
        malformed(&format!("v1.12.{}", "A".repeat(MAX_HEADER_LEN)));

        assert_eq!(
            parse(&format!("v2.12.{sig}")),
            Err(ProofError::UnknownVersion)
        );
        assert_eq!(
            parse(&format!("V1.12.{sig}")),
            Err(ProofError::UnknownVersion)
        );
        // Zero is the one decimal that starts with a zero.
        assert_eq!(parse(&format!("v1.0.{sig}")).unwrap().unix_ms, 0);
    }

    #[test]
    fn a_nul_in_the_method_or_path_is_refused() {
        assert_eq!(
            sign(&SEED, "GET\0/vault", "/x", b"", NOW),
            Err(ProofError::RequestNotCanonical)
        );
        assert_eq!(
            sign(&SEED, "GET", "/vault\0/x", b"", NOW),
            Err(ProofError::RequestNotCanonical)
        );
        let header = sign(&SEED, "GET", PATH, b"", NOW).unwrap();
        assert_eq!(
            verify(&public_key(&SEED), "GET", "/a\0b", b"", &header, NOW),
            Err(ProofError::RequestNotCanonical)
        );
    }

    #[test]
    fn unusable_public_keys_are_refused() {
        assert!(is_usable_public_key(&public_key(&SEED)));
        // The identity point: a valid encoding of small order.
        let mut identity = [0u8; 32];
        identity[0] = 1;
        assert!(!is_usable_public_key(&identity));
        // The all-zero bytes: a point of order four.
        assert!(!is_usable_public_key(&[0u8; 32]));
        // Bytes that are no point on the curve at all. About half of all
        // encodings are not, so a short search finds some.
        let off_curve: Vec<[u8; 32]> = (2u8..=40)
            .map(|y| {
                let mut bytes = [0u8; 32];
                bytes[0] = y;
                bytes
            })
            .filter(|bytes| VerifyingKey::from_bytes(bytes).is_err())
            .collect();
        assert!(!off_curve.is_empty());
        for bytes in &off_curve {
            assert!(!is_usable_public_key(bytes));
        }
        let header = sign(&SEED, "GET", PATH, b"", NOW).unwrap();
        for key in [identity, [0u8; 32], off_curve[0]] {
            assert_eq!(
                verify(&key, "GET", PATH, b"", &header, NOW),
                Err(ProofError::BadSignature)
            );
        }
    }

    const GOLDEN_CANONICAL: &str =
        "6c616b6b6f2f73746f726167652f70726f6f662f763100505554002f7661756c742f303066662f6368756e6b2f3300af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f32620000018bcfe56800";
    const GOLDEN_HEADER: &str =
        "v1.1700000000000.9T7AVraikGs4TiNroyAoE_2dNNBxZqayh0Uyvfw1PcGM6Iq2HeNGvdvf8YlGpX1s2gN7XQ3pzChFQG1FxW-3AA";

    #[test]
    fn canonical_bytes_are_pinned() {
        let bytes = canonical_bytes("PUT", "/vault/00ff/chunk/3", b"", NOW).unwrap();
        assert_eq!(
            bytes.len(),
            PROOF_DOMAIN.len() + 1 + 3 + 1 + 19 + 1 + 32 + 8
        );
        assert!(bytes.starts_with(b"lakko/storage/proof/v1\0PUT\0/vault/00ff/chunk/3\0"));
        assert_eq!(&bytes[bytes.len() - 8..], &NOW.to_be_bytes());
        assert_eq!(
            &bytes[bytes.len() - 40..bytes.len() - 8],
            &hash::blake3(b"")
        );
        // The pinned golden: a change to the canonical layout must fail
        // here, not on a live server refusing every client.
        assert_eq!(hex(&bytes), GOLDEN_CANONICAL);
        // Ed25519 is deterministic, so the whole header is pinned too.
        assert_eq!(
            sign(&SEED, "PUT", "/vault/00ff/chunk/3", b"", NOW).unwrap(),
            GOLDEN_HEADER
        );
    }
}
