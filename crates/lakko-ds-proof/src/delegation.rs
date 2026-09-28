// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Reach-key delegations ([ADR-0058] section 2).
//!
//! A holder of a reach key for a closed identity mailbox may authorise one
//! write by another key: an ADR-0045 committer delivering the Welcome for an
//! add a contact proposed, or a sweep delivering to a `/g/` joiner. The
//! issuer signs, with the reach key, a statement naming the mailbox, the
//! delegate's public key, an expiry and a nonce; the delegate then signs the
//! request itself with the delegate key.
//!
//! # Wire format
//!
//! A fixed [`DELEGATION_LEN`]-byte blob:
//!
//! ```text
//! version(1) = 1 || mailbox(32) || delegate_pk(32) || expires_at_ms(8 BE)
//!   || nonce(16) || issuer_pk(32) || sig(64)
//! ```
//!
//! carried in the [`DELEGATION_HEADER_NAME`] header as base64url without
//! padding ([`DELEGATION_HEADER_VALUE_LEN`] characters, nothing else).
//!
//! # Signed bytes
//!
//! ```text
//! "lakko/ds/delegation/v1" || 0x00 || mailbox || delegate_pk
//!   || expires_at_ms(8 BE) || nonce
//! ```
//!
//! The issuer key is bound by the signature itself (it verifies only under
//! `issuer_pk`), and the domain is distinct from the request proof's
//! `lakko/ds/proof/v1`, so a delegation signature can never be presented as
//! a request proof or the other way round.
//!
//! # The contract `services/ds` implements
//!
//! On `POST /ds/mailbox/{hex}/messages` to a claimed, closed mailbox that
//! carries both an `x-lakko-ds-delegation` and an `x-lakko-ds-proof` header:
//!
//! 1. [`Delegation::parse_header_value`] (bounded, fail-closed);
//! 2. [`verify_delegation`] against the mailbox the path names and the
//!    server's clock: the mailbox matches, the delegation has not expired,
//!    its remaining lifetime is at most [`MAX_DELEGATION_TTL_MS`], and the
//!    signature verifies under `issuer_pk`;
//! 3. `issuer_pk` is a reach key registered for that mailbox and not
//!    expired (the server's state, so not this crate's);
//! 4. the request proof parses, carries [`crate::REACH_EPOCH`], names
//!    `delegate_pk` as its reach key (the proof's sixth field, inside its
//!    signed bytes), and verifies under it - [`verify_delegated_proof`]; the
//!    delegate signs with [`crate::DsProofSigner::for_reach`] over its own
//!    delegate seed, which names the delegate key automatically;
//! 5. the delegation nonce is consumed (a second write with the same
//!    delegation is refused - a delegation is good for one write), and the
//!    proof nonce is consumed as for any proof.
//!
//! Every failure is the one `401` of ADR-0053; the variants of
//! [`DelegationError`] exist for tests and logs, not for the response.
//!
//! [ADR-0058]: ../../../docs/adr/0058-scoped-discovery.md

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;

use crate::{Signature, SigningKey, VerifyingKey, NONCE_LEN, SIGNATURE_LEN};

/// The request header a delegation travels in (lowercase, as HTTP/2 carries
/// it).
pub const DELEGATION_HEADER_NAME: &str = "x-lakko-ds-delegation";
/// Domain separator that opens the signed bytes of a delegation.
pub const DELEGATION_DOMAIN: &[u8] = b"lakko/ds/delegation/v1";
/// The only wire version this crate speaks.
pub const DELEGATION_VERSION: u8 = 1;
/// Length of a mailbox pseudonym and of an Ed25519 public key, in bytes.
const KEY_LEN: usize = 32;
/// Fixed wire length of a delegation, in bytes:
/// `1 + 32 + 32 + 8 + 16 + 32 + 64`.
pub const DELEGATION_LEN: usize = 1 + KEY_LEN + KEY_LEN + 8 + NONCE_LEN + KEY_LEN + SIGNATURE_LEN;
/// Length of the header value: [`DELEGATION_LEN`] bytes in base64url without
/// padding.
pub const DELEGATION_HEADER_VALUE_LEN: usize = (DELEGATION_LEN * 4).div_ceil(3);
/// The longest a delegation may still have to live when a server checks it.
///
/// 24 hours. Chosen, not measured - long enough for a committer that sweeps
/// the next day, short enough that a leaked delegation is not a standing
/// grant.
pub const MAX_DELEGATION_TTL_MS: u64 = 86_400_000;

/// A reach-key holder's authorisation for one write by `delegate_pk` to
/// `mailbox`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delegation {
    /// The mailbox pseudonym the write may go to.
    pub mailbox: [u8; KEY_LEN],
    /// The key that signs the delegated request.
    pub delegate_pk: [u8; KEY_LEN],
    /// When the delegation stops being honoured, unix milliseconds.
    pub expires_at_ms: u64,
    /// Fresh per delegation; the server consumes it on use.
    pub nonce: [u8; NONCE_LEN],
    /// The reach public key that signed this delegation.
    pub issuer_pk: [u8; KEY_LEN],
    /// Ed25519 over [`delegation_signed_bytes`] under `issuer_pk`.
    pub sig: [u8; SIGNATURE_LEN],
}

/// Why a delegation was refused. The server maps every variant to the one
/// ADR-0053 `401`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DelegationError {
    /// The blob or header did not parse (wrong length, bad base64).
    #[error("delegation malformed")]
    Malformed,
    /// A wire version this crate does not speak.
    #[error("delegation version unknown")]
    UnknownVersion,
    /// The delegation names a different mailbox than the request's.
    #[error("delegation is for another mailbox")]
    WrongMailbox,
    /// The delegation's expiry is at or before the verifier's clock.
    #[error("delegation expired")]
    Expired,
    /// The delegation lives longer than [`MAX_DELEGATION_TTL_MS`] from the
    /// verifier's clock.
    #[error("delegation lifetime exceeds the maximum")]
    TtlTooLong,
    /// The signature does not verify under `issuer_pk`, or `issuer_pk` is
    /// not a valid key.
    #[error("delegation signature invalid")]
    BadSignature,
    /// The request proof does not name the delegate key, or is not a reach
    /// proof.
    #[error("request proof is not the delegate's")]
    NotTheDelegate,
    /// The request proof names the delegate key but does not verify under it
    /// (or is outside the time window).
    #[error("request proof invalid")]
    BadProof,
}

/// Step 4 of the contract: `proof` is a reach proof that names the
/// delegation's `delegate_pk` and verifies under it over `req` at `now_ms`.
///
/// Does not check the delegation itself ([`verify_delegation`]) or consume
/// any nonce.
///
/// # Errors
///
/// [`DelegationError::NotTheDelegate`] when the proof is not a reach proof or
/// names another key; [`DelegationError::BadProof`] when it names the
/// delegate but does not verify (including an invalid delegate key).
pub fn verify_delegated_proof(
    delegation: &Delegation,
    proof: &crate::Proof,
    req: &crate::RequestParts<'_>,
    now_ms: u64,
) -> Result<(), DelegationError> {
    if proof.epoch != crate::REACH_EPOCH || proof.reach_key != Some(delegation.delegate_pk) {
        return Err(DelegationError::NotTheDelegate);
    }
    let delegate =
        VerifyingKey::from_bytes(&delegation.delegate_pk).map_err(|_| DelegationError::BadProof)?;
    crate::verify(proof, req, &delegate, now_ms).map_err(|_| DelegationError::BadProof)
}

/// The bytes a delegation signs. Fixed length.
#[must_use]
pub fn delegation_signed_bytes(
    mailbox: &[u8; KEY_LEN],
    delegate_pk: &[u8; KEY_LEN],
    expires_at_ms: u64,
    nonce: &[u8; NONCE_LEN],
) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(DELEGATION_DOMAIN.len() + 1 + KEY_LEN + KEY_LEN + 8 + NONCE_LEN);
    out.extend_from_slice(DELEGATION_DOMAIN);
    out.push(0);
    out.extend_from_slice(mailbox);
    out.extend_from_slice(delegate_pk);
    out.extend_from_slice(&expires_at_ms.to_be_bytes());
    out.extend_from_slice(nonce);
    out
}

/// Sign a delegation with the reach key whose seed is `issuer_seed`.
///
/// Does not bound the lifetime (it has no clock); a delegation issued for
/// longer than [`MAX_DELEGATION_TTL_MS`] ahead is simply refused by
/// [`verify_delegation`] until it comes within range.
#[must_use]
pub fn sign_delegation(
    issuer_seed: &[u8; 32],
    mailbox: &[u8; KEY_LEN],
    delegate_pk: &[u8; KEY_LEN],
    expires_at_ms: u64,
    nonce: [u8; NONCE_LEN],
) -> Delegation {
    let key = SigningKey::from_seed(issuer_seed);
    let bytes = delegation_signed_bytes(mailbox, delegate_pk, expires_at_ms, &nonce);
    Delegation {
        mailbox: *mailbox,
        delegate_pk: *delegate_pk,
        expires_at_ms,
        nonce,
        issuer_pk: key.verifying_key().to_bytes(),
        sig: key.sign(&bytes).to_bytes(),
    }
}

/// Verify `d` for a write to `expected_mailbox` at the verifier's clock
/// `now_ms`.
///
/// Checks, in order: the mailbox, the expiry, the lifetime bound,
/// then the signature (so a stale or misdirected delegation costs no
/// signature verification).
///
/// Whether `issuer_pk` is a reach key registered for the mailbox, and the
/// nonce replay check, are the server's.
///
/// # Errors
///
/// [`DelegationError::WrongMailbox`], [`DelegationError::Expired`],
/// [`DelegationError::TtlTooLong`] or [`DelegationError::BadSignature`].
pub fn verify_delegation(
    d: &Delegation,
    expected_mailbox: &[u8; KEY_LEN],
    now_ms: u64,
) -> Result<(), DelegationError> {
    if d.mailbox != *expected_mailbox {
        return Err(DelegationError::WrongMailbox);
    }
    if d.expires_at_ms <= now_ms {
        return Err(DelegationError::Expired);
    }
    if d.expires_at_ms - now_ms > MAX_DELEGATION_TTL_MS {
        return Err(DelegationError::TtlTooLong);
    }
    let issuer =
        VerifyingKey::from_bytes(&d.issuer_pk).map_err(|_| DelegationError::BadSignature)?;
    let sig = Signature::from_bytes(&d.sig).map_err(|_| DelegationError::BadSignature)?;
    let bytes = delegation_signed_bytes(&d.mailbox, &d.delegate_pk, d.expires_at_ms, &d.nonce);
    issuer
        .verify(&bytes, &sig)
        .map_err(|_| DelegationError::BadSignature)
}

impl Delegation {
    /// The fixed-length wire bytes.
    #[must_use]
    pub fn encode(&self) -> [u8; DELEGATION_LEN] {
        let mut out = [0u8; DELEGATION_LEN];
        let mut at = 0;
        let mut put = |bytes: &[u8]| {
            let end = at + bytes.len();
            out[at..end].copy_from_slice(bytes);
            at = end;
        };
        put(&[DELEGATION_VERSION]);
        put(&self.mailbox);
        put(&self.delegate_pk);
        put(&self.expires_at_ms.to_be_bytes());
        put(&self.nonce);
        put(&self.issuer_pk);
        put(&self.sig);
        out
    }

    /// Parse the wire bytes: exactly [`DELEGATION_LEN`] bytes, version 1.
    /// Structural only - the signature is checked by [`verify_delegation`].
    ///
    /// # Errors
    ///
    /// [`DelegationError::Malformed`] on a wrong length,
    /// [`DelegationError::UnknownVersion`] on another version.
    pub fn decode(bytes: &[u8]) -> Result<Self, DelegationError> {
        if bytes.len() != DELEGATION_LEN {
            return Err(DelegationError::Malformed);
        }
        let (version, rest) = bytes.split_first().ok_or(DelegationError::Malformed)?;
        if *version != DELEGATION_VERSION {
            return Err(DelegationError::UnknownVersion);
        }
        let (mailbox, rest) = take::<KEY_LEN>(rest)?;
        let (delegate_pk, rest) = take::<KEY_LEN>(rest)?;
        let (expires, rest) = take::<8>(rest)?;
        let (nonce, rest) = take::<NONCE_LEN>(rest)?;
        let (issuer_pk, rest) = take::<KEY_LEN>(rest)?;
        let (sig, rest) = take::<SIGNATURE_LEN>(rest)?;
        if !rest.is_empty() {
            return Err(DelegationError::Malformed);
        }
        Ok(Self {
            mailbox,
            delegate_pk,
            expires_at_ms: u64::from_be_bytes(expires),
            nonce,
            issuer_pk,
            sig,
        })
    }

    /// The header value: the wire bytes in base64url without padding.
    #[must_use]
    pub fn to_header_value(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.encode())
    }

    /// Parse a header value. The length is checked before decoding, so an
    /// oversized value costs nothing.
    ///
    /// # Errors
    ///
    /// [`DelegationError::Malformed`] or [`DelegationError::UnknownVersion`].
    pub fn parse_header_value(value: &str) -> Result<Self, DelegationError> {
        if value.len() != DELEGATION_HEADER_VALUE_LEN {
            return Err(DelegationError::Malformed);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| DelegationError::Malformed)?;
        Self::decode(&bytes)
    }
}

/// Split the first `N` bytes off `bytes` as an array.
fn take<const N: usize>(bytes: &[u8]) -> Result<([u8; N], &[u8]), DelegationError> {
    if bytes.len() < N {
        return Err(DelegationError::Malformed);
    }
    let (head, rest) = bytes.split_at(N);
    let arr: [u8; N] = head.try_into().map_err(|_| DelegationError::Malformed)?;
    Ok((arr, rest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::hash;

    const NOW: u64 = 1_700_000_000_000;
    const MAILBOX: [u8; 32] = [0x10; 32];
    const ISSUER_SEED: [u8; 32] = [0x20; 32];

    fn delegate_pk() -> [u8; 32] {
        SigningKey::from_seed(&[0x30u8; 32])
            .verifying_key()
            .to_bytes()
    }

    fn sample() -> Delegation {
        sign_delegation(
            &ISSUER_SEED,
            &MAILBOX,
            &delegate_pk(),
            NOW + 3_600_000,
            [0x40u8; NONCE_LEN],
        )
    }

    #[test]
    fn the_wire_length_is_the_sum_of_its_fields() {
        assert_eq!(DELEGATION_LEN, 185);
        assert_eq!(DELEGATION_HEADER_VALUE_LEN, 247);
        assert_eq!(
            sample().to_header_value().len(),
            DELEGATION_HEADER_VALUE_LEN
        );
    }

    #[test]
    fn round_trips_through_bytes_and_header() {
        let d = sample();
        let bytes = d.encode();
        assert_eq!(bytes[0], DELEGATION_VERSION);
        assert_eq!(Delegation::decode(&bytes), Ok(d.clone()));
        let header = d.to_header_value();
        assert_eq!(Delegation::parse_header_value(&header), Ok(d.clone()));
        verify_delegation(&d, &MAILBOX, NOW).expect("verify");
        assert_eq!(
            d.issuer_pk,
            SigningKey::from_seed(&ISSUER_SEED)
                .verifying_key()
                .to_bytes()
        );
    }

    #[test]
    fn a_delegation_for_another_mailbox_is_refused() {
        assert_eq!(
            verify_delegation(&sample(), &[0x11u8; 32], NOW),
            Err(DelegationError::WrongMailbox)
        );
    }

    #[test]
    fn an_expired_delegation_is_refused() {
        let d = sample();
        assert_eq!(
            verify_delegation(&d, &MAILBOX, d.expires_at_ms),
            Err(DelegationError::Expired)
        );
        assert_eq!(
            verify_delegation(&d, &MAILBOX, d.expires_at_ms + 1),
            Err(DelegationError::Expired)
        );
        verify_delegation(&d, &MAILBOX, d.expires_at_ms - 1).expect("last millisecond");
    }

    #[test]
    fn an_over_long_lifetime_is_refused() {
        let d = sign_delegation(
            &ISSUER_SEED,
            &MAILBOX,
            &delegate_pk(),
            NOW + MAX_DELEGATION_TTL_MS + 1,
            [0x41u8; NONCE_LEN],
        );
        assert_eq!(
            verify_delegation(&d, &MAILBOX, NOW),
            Err(DelegationError::TtlTooLong)
        );
        // At the bound exactly it is accepted.
        verify_delegation(&d, &MAILBOX, NOW + 1).expect("at the bound");
        let far = sign_delegation(
            &ISSUER_SEED,
            &MAILBOX,
            &delegate_pk(),
            u64::MAX,
            [0x42u8; NONCE_LEN],
        );
        assert_eq!(
            verify_delegation(&far, &MAILBOX, 0),
            Err(DelegationError::TtlTooLong)
        );
    }

    #[test]
    fn a_tampered_delegation_is_refused() {
        let d = sample();
        let mut sig = d.clone();
        sig.sig[0] ^= 1;
        assert_eq!(
            verify_delegation(&sig, &MAILBOX, NOW),
            Err(DelegationError::BadSignature)
        );
        let mut delegate = d.clone();
        delegate.delegate_pk[0] ^= 1;
        assert_eq!(
            verify_delegation(&delegate, &MAILBOX, NOW),
            Err(DelegationError::BadSignature)
        );
        let mut nonce = d.clone();
        nonce.nonce[0] ^= 1;
        assert_eq!(
            verify_delegation(&nonce, &MAILBOX, NOW),
            Err(DelegationError::BadSignature)
        );
        let mut expiry = d.clone();
        expiry.expires_at_ms -= 1;
        assert_eq!(
            verify_delegation(&expiry, &MAILBOX, NOW),
            Err(DelegationError::BadSignature)
        );
        let mut issuer = d;
        issuer.issuer_pk = SigningKey::from_seed(&[0x21u8; 32])
            .verifying_key()
            .to_bytes();
        assert_eq!(
            verify_delegation(&issuer, &MAILBOX, NOW),
            Err(DelegationError::BadSignature)
        );
    }

    #[test]
    fn a_wrong_length_or_version_is_refused() {
        let bytes = sample().encode();
        assert_eq!(
            Delegation::decode(&bytes[..DELEGATION_LEN - 1]),
            Err(DelegationError::Malformed)
        );
        let mut longer = bytes.to_vec();
        longer.push(0);
        assert_eq!(Delegation::decode(&longer), Err(DelegationError::Malformed));
        assert_eq!(Delegation::decode(&[]), Err(DelegationError::Malformed));
        let mut v2 = bytes;
        v2[0] = 2;
        assert_eq!(
            Delegation::decode(&v2),
            Err(DelegationError::UnknownVersion)
        );
        let header = sample().to_header_value();
        assert_eq!(
            Delegation::parse_header_value(&header[1..]),
            Err(DelegationError::Malformed)
        );
        assert_eq!(
            Delegation::parse_header_value(&format!("{header}A")),
            Err(DelegationError::Malformed)
        );
        let standard_alphabet = header.replacen(&header[..1], "+", 1);
        assert_eq!(
            Delegation::parse_header_value(&standard_alphabet),
            Err(DelegationError::Malformed)
        );
    }

    const GOLDEN_SIGNED_BYTES_BLAKE3: &str =
        "4de44455b956bcad2f29816316469fe62ab2099f8b8307d952957e29f2e0b55f";

    #[test]
    fn the_signed_bytes_are_pinned() {
        let bytes = delegation_signed_bytes(
            &[0xAAu8; 32],
            &[0xBBu8; 32],
            1_700_000_000_000,
            &[0xCCu8; NONCE_LEN],
        );
        assert_eq!(bytes.len(), DELEGATION_DOMAIN.len() + 1 + 32 + 32 + 8 + 16);
        assert!(bytes.starts_with(b"lakko/ds/delegation/v1\0"));
        assert_eq!(&bytes[bytes.len() - NONCE_LEN..], &[0xCCu8; NONCE_LEN]);
        // A change to the signed layout must fail here, not on a live server
        // refusing every delegated write.
        assert_eq!(hex(&hash::blake3(&bytes)), GOLDEN_SIGNED_BYTES_BLAKE3);
    }

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }
}
