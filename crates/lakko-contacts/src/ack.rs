// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `ContactAck` reply path — closes the gap from ADR-0005 §13.2.
//!
//! When Alice publishes a [`ContactRequest`](crate::ContactRequest)
//! to Bob's stranger inbox, the server happily routes it but
//! never reports back to Alice whether Bob accepted, declined, or
//! ignored it. Without an explicit acknowledgement, Alice's UI
//! sits forever on "pending — no reply". `ContactAck` is Bob's
//! "yes, accepted" reply, routed back to Alice via the
//! per-relationship pseudonym she chose at request time.
//!
//! ## Wire shape (v1)
//!
//! ```text
//! u8       version       = 0x01
//! 32       ack_to_pseudonym  — Alice's `from_pseudonym` from the
//!                              original request; Bob copies it
//!                              into the ack so Alice's poll layer
//!                              can match against her pending-sent
//!                              table.
//! 32       accepter_pubkey   — Bob's Ed25519 identity public key.
//!                              Alice already has this from the
//!                              discovery lookup that built the
//!                              original request; the ack
//!                              re-asserts it for sig verification.
//! 16       accepter_account_id  — Bob's UUIDv7. Lets Alice surface
//!                                 "@bob accepted" without a second
//!                                 discovery round-trip.
//! 32       accepter_x25519_pubkey — Bob's HPKE init key. Same as
//!                                   above — already known via
//!                                   discovery but bundled here so
//!                                   the ack is self-contained for
//!                                   the future fast-path where
//!                                   Alice's first encrypted reply
//!                                   piggybacks on the ack receipt.
//! u64 BE   accepted_at_ms — UNIX milliseconds. Anti-replay anchor;
//!                            same semantics as
//!                            `ContactRequest.valid_after_ms`.
//! 64       signature      — Ed25519 over the bytes above
//!                            (version through accepted_at_ms).
//! ```
//!
//! Total: 185 bytes. No optional fields, no sealed envelope —
//! every byte is either public-key material the sender already
//! had or a routing pseudonym. The ack itself doesn't carry an
//! intro message; the first "real" encrypted message from Bob
//! flows through the freshly-created 1×1 MLS group instead.
//!
//! ## Threat model
//!
//! - **Authentication**: the Ed25519 signature on the canonical
//!   prefix proves the ack was minted by the holder of
//!   `accepter_pubkey`. Alice cross-checks `accepter_pubkey`
//!   against the recipient pubkey she recorded when composing
//!   the original request — anyone with PoW can publish to
//!   Alice's mailbox, but only Bob's signing key produces a
//!   valid ack.
//! - **Replay**: `ack_to_pseudonym` is fresh per request (Alice
//!   generated it from `PerRelationship::derive`), so an
//!   attacker replaying an ack against a different pending
//!   request would either miss the recipient pubkey check or
//!   fail to match any open request in Alice's pending-sent
//!   table.
//! - **Privacy**: the ack carries no plaintext intro; only
//!   public-key material that's already cross-referenced
//!   between sender + recipient via the discovery service.

use lakko_crypto::agreement::PublicKey as X25519PublicKey;
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN, SIGNATURE_LEN};

use crate::error::{ContactError, Result};
use crate::pseudonym::PSEUDONYM_LEN;
use crate::request::ACCOUNT_ID_LEN;

/// Schema version byte. Bump only on a non-backwards-compatible
/// wire change; additive fields ride a length-prefix extension.
const SCHEMA_VERSION: u8 = 1;

/// Validated `ContactAck`. Produced by [`decode_and_verify_ack`].
#[derive(Clone, Debug)]
pub struct ContactAck {
    /// The original [`ContactRequest::from_pseudonym`] Alice put
    /// on the wire; Bob copies it into the ack so Alice's poll
    /// layer can match the ack against her pending-sent table.
    pub ack_to_pseudonym: [u8; PSEUDONYM_LEN],
    /// Bob's Ed25519 identity public key.
    pub accepter_pubkey: VerifyingKey,
    /// Bob's UUIDv7 account id.
    pub accepter_account_id: [u8; ACCOUNT_ID_LEN],
    /// Bob's X25519 HPKE init key.
    pub accepter_x25519_pubkey: X25519PublicKey,
    /// UNIX milliseconds at which the ack was minted.
    pub accepted_at_ms: u64,
}

/// Compose a `ContactAck` and produce its signed wire-format
/// bytes. The recipient (Bob) signs the canonical prefix with
/// his identity key.
///
/// # Errors
///
/// Infallible at the API level: every input is bounds-checked
/// by its type. The `Result` return is kept for symmetry with
/// the request side so a future format bump can introduce an
/// error path without churning callers.
pub fn compose_ack(
    accepter_signing: &SigningKey,
    accepter_account_id: &[u8; ACCOUNT_ID_LEN],
    accepter_x25519_pub: &X25519PublicKey,
    ack_to_pseudonym: [u8; PSEUDONYM_LEN],
    accepted_at_ms: u64,
) -> Result<Vec<u8>> {
    let accepter_pubkey = accepter_signing.verifying_key();
    let mut wire = encode_signing_prefix(
        &accepter_pubkey,
        accepter_account_id,
        accepter_x25519_pub,
        &ack_to_pseudonym,
        accepted_at_ms,
    );
    let signature = accepter_signing.sign(&wire);
    wire.extend_from_slice(&signature.to_bytes());
    Ok(wire)
}

/// Decode the wire bytes, verify the Ed25519 signature, return
/// the validated [`ContactAck`].
///
/// **Does not match the ack against any pending request** —
/// that's the caller's job. The poll layer compares
/// `accepter_pubkey` against the recipient pubkey it stashed
/// when composing the original request, and rejects the ack on
/// mismatch.
///
/// # Errors
///
/// - [`ContactError::InvalidEncoding`] — wire format is malformed.
/// - [`ContactError::BadSignature`] — Ed25519 verification failed.
/// - [`ContactError::Crypto`] — public-key bytes don't decode to
///   a valid Ed25519 / X25519 key.
pub fn decode_and_verify_ack(wire: &[u8]) -> Result<ContactAck> {
    let mut cur = wire;

    let version = take_u8(&mut cur)?;
    if version != SCHEMA_VERSION {
        return Err(ContactError::InvalidEncoding(format!(
            "unsupported ContactAck schema version {version}",
        )));
    }
    let accepter_pubkey_bytes = take_n(&mut cur, PUBLIC_KEY_LEN)?;
    let accepter_pubkey =
        VerifyingKey::from_bytes(accepter_pubkey_bytes).map_err(ContactError::Crypto)?;
    let mut accepter_account_id = [0u8; ACCOUNT_ID_LEN];
    accepter_account_id.copy_from_slice(take_n(&mut cur, ACCOUNT_ID_LEN)?);
    let accepter_x25519_bytes = take_n(&mut cur, PUBLIC_KEY_LEN)?;
    let accepter_x25519_pubkey =
        X25519PublicKey::from_bytes(accepter_x25519_bytes).map_err(ContactError::Crypto)?;
    let mut ack_to_pseudonym = [0u8; PSEUDONYM_LEN];
    ack_to_pseudonym.copy_from_slice(take_n(&mut cur, PSEUDONYM_LEN)?);
    let accepted_at_ms_bytes: [u8; 8] = take_n(&mut cur, 8)?.try_into().unwrap();
    let accepted_at_ms = u64::from_be_bytes(accepted_at_ms_bytes);
    let signature_bytes = take_n(&mut cur, SIGNATURE_LEN)?;
    let signature = Signature::from_bytes(signature_bytes).map_err(ContactError::Crypto)?;

    if !cur.is_empty() {
        return Err(ContactError::InvalidEncoding(format!(
            "{} trailing bytes after signature",
            cur.len(),
        )));
    }

    // Re-encode the prefix exactly as the signer did + verify.
    let signed_prefix = encode_signing_prefix(
        &accepter_pubkey,
        &accepter_account_id,
        &accepter_x25519_pubkey,
        &ack_to_pseudonym,
        accepted_at_ms,
    );
    accepter_pubkey
        .verify(&signed_prefix, &signature)
        .map_err(|_| ContactError::BadSignature)?;

    Ok(ContactAck {
        ack_to_pseudonym,
        accepter_pubkey,
        accepter_account_id,
        accepter_x25519_pubkey,
        accepted_at_ms,
    })
}

// ============================================================================
// Codec helpers
// ============================================================================

fn encode_signing_prefix(
    accepter_pubkey: &VerifyingKey,
    accepter_account_id: &[u8; ACCOUNT_ID_LEN],
    accepter_x25519_pub: &X25519PublicKey,
    ack_to_pseudonym: &[u8; PSEUDONYM_LEN],
    accepted_at_ms: u64,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        1 + PUBLIC_KEY_LEN + ACCOUNT_ID_LEN + PUBLIC_KEY_LEN + PSEUDONYM_LEN + 8,
    );
    out.push(SCHEMA_VERSION);
    out.extend_from_slice(&accepter_pubkey.to_bytes());
    out.extend_from_slice(accepter_account_id);
    out.extend_from_slice(&accepter_x25519_pub.to_bytes());
    out.extend_from_slice(ack_to_pseudonym);
    out.extend_from_slice(&accepted_at_ms.to_be_bytes());
    out
}

fn take_u8(cur: &mut &[u8]) -> Result<u8> {
    if cur.is_empty() {
        return Err(ContactError::InvalidEncoding("unexpected EOF".to_string()));
    }
    let b = cur[0];
    *cur = &cur[1..];
    Ok(b)
}

fn take_n<'a>(cur: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
    if cur.len() < n {
        return Err(ContactError::InvalidEncoding(format!(
            "need {n} bytes, have {}",
            cur.len(),
        )));
    }
    let (head, rest) = cur.split_at(n);
    *cur = rest;
    Ok(head)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::agreement::SecretKey as X25519SecretKey;

    fn accepter() -> SigningKey {
        SigningKey::from_seed(&[42u8; 32])
    }

    fn accepter_account_id() -> [u8; ACCOUNT_ID_LEN] {
        [0xBB; ACCOUNT_ID_LEN]
    }

    fn accepter_x25519() -> X25519PublicKey {
        let secret = X25519SecretKey::from_bytes([42u8; 32]);
        secret.public_key()
    }

    #[test]
    fn round_trip_compose_and_verify() {
        let signing = accepter();
        let acct = accepter_account_id();
        let x25519 = accepter_x25519();
        let pseudonym = [0xAAu8; PSEUDONYM_LEN];
        let ts: u64 = 1_700_000_000_000;

        let wire = compose_ack(&signing, &acct, &x25519, pseudonym, ts).unwrap();
        let parsed = decode_and_verify_ack(&wire).unwrap();

        assert_eq!(parsed.ack_to_pseudonym, pseudonym);
        assert_eq!(
            parsed.accepter_pubkey.to_bytes(),
            signing.verifying_key().to_bytes()
        );
        assert_eq!(parsed.accepter_account_id, acct);
        assert_eq!(parsed.accepter_x25519_pubkey.to_bytes(), x25519.to_bytes());
        assert_eq!(parsed.accepted_at_ms, ts);
    }

    #[test]
    fn rejects_tampered_signature() {
        let signing = accepter();
        let acct = accepter_account_id();
        let x25519 = accepter_x25519();
        let pseudonym = [0xCDu8; PSEUDONYM_LEN];
        let ts: u64 = 1_700_000_000_000;

        let mut wire = compose_ack(&signing, &acct, &x25519, pseudonym, ts).unwrap();
        // Flip the last byte (inside the signature).
        let last = wire.len() - 1;
        wire[last] ^= 0x01;

        let err = decode_and_verify_ack(&wire).unwrap_err();
        assert!(matches!(err, ContactError::BadSignature));
    }

    #[test]
    fn rejects_tampered_payload() {
        let signing = accepter();
        let acct = accepter_account_id();
        let x25519 = accepter_x25519();
        let pseudonym = [0xCDu8; PSEUDONYM_LEN];
        let ts: u64 = 1_700_000_000_000;

        let mut wire = compose_ack(&signing, &acct, &x25519, pseudonym, ts).unwrap();
        // Flip a byte in the timestamp (before the signature).
        // SCHEMA_VERSION (1) + pubkey (32) + acct (16) + x25519 (32)
        // + pseudonym (32) = 113 → ts at byte 113. Flip 113.
        wire[113] ^= 0x01;

        let err = decode_and_verify_ack(&wire).unwrap_err();
        assert!(matches!(err, ContactError::BadSignature));
    }

    #[test]
    fn rejects_wrong_version() {
        let signing = accepter();
        let acct = accepter_account_id();
        let x25519 = accepter_x25519();
        let pseudonym = [0xCDu8; PSEUDONYM_LEN];

        let mut wire = compose_ack(&signing, &acct, &x25519, pseudonym, 0).unwrap();
        wire[0] = 0xFF;

        let err = decode_and_verify_ack(&wire).unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    #[test]
    fn rejects_truncated_wire() {
        let signing = accepter();
        let acct = accepter_account_id();
        let x25519 = accepter_x25519();
        let pseudonym = [0xCDu8; PSEUDONYM_LEN];

        let wire = compose_ack(&signing, &acct, &x25519, pseudonym, 0).unwrap();
        // Cut off 4 bytes from the end — signature short.
        let truncated = &wire[..wire.len() - 4];

        let err = decode_and_verify_ack(truncated).unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    #[test]
    fn rejects_trailing_bytes() {
        let signing = accepter();
        let acct = accepter_account_id();
        let x25519 = accepter_x25519();
        let pseudonym = [0xCDu8; PSEUDONYM_LEN];

        let mut wire = compose_ack(&signing, &acct, &x25519, pseudonym, 0).unwrap();
        wire.extend_from_slice(&[0xFF; 4]);

        let err = decode_and_verify_ack(&wire).unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    #[test]
    fn wire_size_is_185_bytes() {
        let signing = accepter();
        let acct = accepter_account_id();
        let x25519 = accepter_x25519();
        let pseudonym = [0u8; PSEUDONYM_LEN];

        let wire = compose_ack(&signing, &acct, &x25519, pseudonym, 0).unwrap();
        // 1 (version) + 32 (pubkey) + 16 (account_id) + 32
        // (x25519) + 32 (pseudonym) + 8 (ts) + 64 (signature)
        assert_eq!(wire.len(), 185);
    }
}
