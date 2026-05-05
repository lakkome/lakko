// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! [`ContactRequest`] — the cold-contact envelope per ADR-0005 §13.2.
//!
//! Sender Alice composes a request for recipient Bob:
//!
//! 1. Encrypts the optional intro message to Bob's identity public
//!    key via [`crate::envelope`] sealed-box (HPKE-equivalent over
//!    X25519 + ChaCha20-Poly1305).
//! 2. Wraps the result in a `ContactRequest` carrying her own
//!    identity public key, a per-recipient pseudonym, and a
//!    `valid_after_ms` freshness anchor.
//! 3. Signs the canonical encoding with her Ed25519 identity key.
//! 4. Sends the encoded bytes into Bob's stranger inbox via
//!    `services/ds`.
//!
//! On receipt Bob:
//!
//! 1. Decodes the wire format.
//! 2. Verifies the signature against `from_pubkey`.
//! 3. Opens the sealed envelope with his identity X25519 private key.
//! 4. Surfaces the request to UI: Accept / Decline / Block / Ignore.
//!
//! ## Wire format (manual codec)
//!
//! **v2** (current — Phase 1.16e3b):
//!
//! ```text
//! u8           schema version (= 2)
//! [32 bytes]   from_pubkey                           — Ed25519 verifying key
//! [16 bytes]   from_account_id                       — sender's UUIDv7 (v2 only)
//! [32 bytes]   from_pseudonym                        — fresh per-recipient pseudonym
//! u64be        valid_after_ms                        — UNIX millis; reject if too old
//! u32be        sealed_envelope length
//! [..]         sealed_envelope bytes
//! [64 bytes]   signature                             — Ed25519 over the prefix above
//! ```
//!
//! **v1** (legacy — accepted on receive only): identical to v2
//! minus the 16-byte `from_account_id` field. Receivers decode
//! either version; the resulting [`ContactRequest`] carries
//! `from_account_id: None` for v1 inputs.
//!
//! The `from_account_id` extension lets the receiver drive the
//! Accept-side of the group-creation handshake (Phase 1.16e3b):
//! the receiver needs `account_id` to fetch the sender's
//! KeyPackage, but a v1 ContactRequest only carries the Ed25519
//! pubkey. Bumping to v2 closes the gap. Senders are always v2;
//! v1 acceptance is purely a backwards-compat hatch for
//! envelopes already in flight when a deployment upgrades.
//!
//! Signature covers everything from the schema-version byte through
//! the sealed-envelope bytes — i.e. the entire encoding except the
//! trailing 64-byte signature itself.
//!
//! ## What's NOT in 1.9a
//!
//! The plan also calls for `pow_token` (Equix proof-of-work) and
//! `privacy_pass_token` (App Attest / Play Integrity attestation) to
//! be carried in the same envelope. Those are anti-spam mechanisms
//! at the `services/ds` ingest path; gating them in the wire format
//! itself is straightforward (extra fields with their own length
//! prefixes), but spoke implementations belong in 1.1f and the
//! Privacy Pass sub-sprint. The current wire format is structured so
//! adding optional fields at the end stays backwards-compatible —
//! a v2 schema bumps the version byte.

use lakko_crypto::agreement::{PublicKey as X25519PublicKey, SecretKey as X25519SecretKey};
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN, SIGNATURE_LEN};

use crate::envelope;
use crate::error::{ContactError, Result};
use crate::pseudonym::{decode_pseudonym, PSEUDONYM_LEN};

/// Current `compose`d schema version.
const SCHEMA_VERSION: u8 = 2;

/// Legacy schema version. Receivers still decode it for
/// backwards-compat with envelopes in-flight at the moment of a
/// rolling upgrade; no caller in this codebase should produce v1
/// any more.
const LEGACY_SCHEMA_VERSION_V1: u8 = 1;

/// `from_account_id` length in bytes (UUIDv7 raw, matches
/// `services/auth.accounts.id` and the rest of the Lakko stack).
pub const ACCOUNT_ID_LEN: usize = 16;

/// AAD bound to the sealed envelope. Domain-separates this envelope
/// from any future non-contact-request use of the same primitive.
const ENVELOPE_AAD: &[u8] = b"lakko-contacts/v1/contact-request";

/// A composed `ContactRequest`, ready to wire-encode.
///
/// Sender side: produced by [`compose`].
/// Recipient side: produced by [`decode_and_verify`].
#[derive(Clone, Debug)]
pub struct ContactRequest {
    /// Sender's long-term Ed25519 identity public key.
    pub from_pubkey: VerifyingKey,
    /// Sender's `account_id` (UUIDv7 raw bytes). Present for
    /// v2 wire envelopes, `None` for v1. v1 acceptance exists
    /// purely for backwards-compat — see the file-level
    /// docstring; new senders always supply this so the
    /// receiver can drive Accept-side group creation
    /// (Phase 1.16e3b).
    pub from_account_id: Option<[u8; ACCOUNT_ID_LEN]>,
    /// Fresh per-recipient pseudonym chosen by the sender. Server
    /// uses it for routing; recipient stores it as the canonical
    /// pseudonym for any reply they want to send before the contact
    /// is fully accepted.
    pub from_pseudonym: [u8; PSEUDONYM_LEN],
    /// Anti-replay anchor: UNIX milliseconds at which the request
    /// becomes valid. Recipients reject requests too far in the
    /// past (e.g. > 7 days) at the application layer.
    pub valid_after_ms: u64,
    /// Sealed envelope, opaque to anyone but the recipient.
    pub sealed_envelope: Vec<u8>,
}

/// Compose a `ContactRequest` and produce its signed wire-format bytes.
///
/// The sender supplies their Ed25519 signing key (for authentication),
/// the recipient's X25519 public key (for sealed-envelope encryption),
/// and an optional intro message body. `from_pseudonym` should be
/// freshly generated per-recipient (use
/// [`crate::pseudonym::PerRelationship::derive`] with a one-shot
/// seed, or just 32 random bytes — a pseudonym is just routing
/// metadata).
///
/// # Errors
///
/// Returns [`ContactError::Crypto`] on AEAD failure (essentially
/// impossible for valid inputs).
pub fn compose(
    sender_signing: &SigningKey,
    sender_account_id: &[u8; ACCOUNT_ID_LEN],
    recipient_x25519_pub: &X25519PublicKey,
    from_pseudonym: [u8; PSEUDONYM_LEN],
    valid_after_ms: u64,
    intro_message: &[u8],
) -> Result<Vec<u8>> {
    let sealed_envelope = envelope::seal(recipient_x25519_pub, ENVELOPE_AAD, intro_message)?;

    let from_pubkey = sender_signing.verifying_key();
    let mut wire = encode_signing_prefix(
        &from_pubkey,
        sender_account_id,
        &from_pseudonym,
        valid_after_ms,
        &sealed_envelope,
    );
    let signature = sender_signing.sign(&wire);
    wire.extend_from_slice(&signature.to_bytes());
    Ok(wire)
}

/// Recipient-side: decode the wire bytes, verify the Ed25519
/// signature, return the validated [`ContactRequest`].
///
/// **Does not open the sealed envelope** — call [`open_envelope`]
/// after the recipient has decided to act on the request. Splitting
/// this lets the UI render "pending request from `<fingerprint>`"
/// before paying the X25519 + AEAD cost on accept.
///
/// # Errors
///
/// - [`ContactError::InvalidEncoding`] — wire format is malformed.
/// - [`ContactError::BadSignature`] — Ed25519 verification failed.
pub fn decode_and_verify(wire: &[u8]) -> Result<ContactRequest> {
    let mut cur = wire;

    let version = take_u8(&mut cur)?;
    if version != SCHEMA_VERSION && version != LEGACY_SCHEMA_VERSION_V1 {
        return Err(ContactError::InvalidEncoding(format!(
            "unsupported ContactRequest schema version {version}",
        )));
    }
    let from_pubkey_bytes = take_n(&mut cur, PUBLIC_KEY_LEN)?;
    let from_pubkey = VerifyingKey::from_bytes(from_pubkey_bytes).map_err(ContactError::Crypto)?;

    // v2 inserts the 16-byte sender account_id between
    // `from_pubkey` and `from_pseudonym`. v1 omits it.
    let from_account_id: Option<[u8; ACCOUNT_ID_LEN]> = if version == SCHEMA_VERSION {
        let mut buf = [0u8; ACCOUNT_ID_LEN];
        buf.copy_from_slice(take_n(&mut cur, ACCOUNT_ID_LEN)?);
        Some(buf)
    } else {
        None
    };

    let from_pseudonym = decode_pseudonym(take_n(&mut cur, PSEUDONYM_LEN)?)?;
    let valid_after_ms = u64::from_be_bytes(take_n(&mut cur, 8)?.try_into().unwrap());
    let sealed_len = u32::from_be_bytes(take_n(&mut cur, 4)?.try_into().unwrap()) as usize;
    let sealed_envelope = take_n(&mut cur, sealed_len)?.to_vec();
    let signature_bytes = take_n(&mut cur, SIGNATURE_LEN)?;
    let signature = Signature::from_bytes(signature_bytes).map_err(ContactError::Crypto)?;

    if !cur.is_empty() {
        return Err(ContactError::InvalidEncoding(format!(
            "{} trailing bytes after signature",
            cur.len(),
        )));
    }

    // Re-encode the prefix exactly as the signer did and verify.
    let signed_prefix = match from_account_id {
        Some(ref acct) => encode_signing_prefix(
            &from_pubkey,
            acct,
            &from_pseudonym,
            valid_after_ms,
            &sealed_envelope,
        ),
        None => encode_signing_prefix_v1(
            &from_pubkey,
            &from_pseudonym,
            valid_after_ms,
            &sealed_envelope,
        ),
    };
    from_pubkey
        .verify(&signed_prefix, &signature)
        .map_err(|_| ContactError::BadSignature)?;

    Ok(ContactRequest {
        from_pubkey,
        from_account_id,
        from_pseudonym,
        valid_after_ms,
        sealed_envelope,
    })
}

/// Decrypt the sealed envelope on a previously-verified
/// [`ContactRequest`] using the recipient's X25519 private key.
///
/// # Errors
///
/// - [`ContactError::WrongRecipient`] — envelope was sealed for a
///   different recipient (or tampered with after sealing).
/// - [`ContactError::InvalidEncoding`] — envelope wire format is
///   malformed.
pub fn open_envelope(req: &ContactRequest, recipient_priv: &X25519SecretKey) -> Result<Vec<u8>> {
    envelope::open(recipient_priv, ENVELOPE_AAD, &req.sealed_envelope)
}

// ============================================================================
// Codec helpers
// ============================================================================

/// Encode the bytes that get signed for the current (v2) wire
/// format: everything except the trailing signature itself.
fn encode_signing_prefix(
    from_pubkey: &VerifyingKey,
    from_account_id: &[u8; ACCOUNT_ID_LEN],
    from_pseudonym: &[u8; PSEUDONYM_LEN],
    valid_after_ms: u64,
    sealed_envelope: &[u8],
) -> Vec<u8> {
    let sealed_len = u32::try_from(sealed_envelope.len())
        .expect("sealed envelope under u32::MAX bytes — caller invariant");
    let mut out = Vec::with_capacity(
        1 + PUBLIC_KEY_LEN + ACCOUNT_ID_LEN + PSEUDONYM_LEN + 8 + 4 + sealed_envelope.len(),
    );
    out.push(SCHEMA_VERSION);
    out.extend_from_slice(&from_pubkey.to_bytes());
    out.extend_from_slice(from_account_id);
    out.extend_from_slice(from_pseudonym);
    out.extend_from_slice(&valid_after_ms.to_be_bytes());
    out.extend_from_slice(&sealed_len.to_be_bytes());
    out.extend_from_slice(sealed_envelope);
    out
}

/// Re-encode a legacy v1 signing prefix for signature
/// verification on receive. Mirrors the pre-1.16e3b encoding
/// — used only by `decode_and_verify` when the wire byte is
/// `LEGACY_SCHEMA_VERSION_V1`. No `compose` path produces v1
/// any more.
fn encode_signing_prefix_v1(
    from_pubkey: &VerifyingKey,
    from_pseudonym: &[u8; PSEUDONYM_LEN],
    valid_after_ms: u64,
    sealed_envelope: &[u8],
) -> Vec<u8> {
    let sealed_len = u32::try_from(sealed_envelope.len())
        .expect("sealed envelope under u32::MAX bytes — caller invariant");
    let mut out =
        Vec::with_capacity(1 + PUBLIC_KEY_LEN + PSEUDONYM_LEN + 8 + 4 + sealed_envelope.len());
    out.push(LEGACY_SCHEMA_VERSION_V1);
    out.extend_from_slice(&from_pubkey.to_bytes());
    out.extend_from_slice(from_pseudonym);
    out.extend_from_slice(&valid_after_ms.to_be_bytes());
    out.extend_from_slice(&sealed_len.to_be_bytes());
    out.extend_from_slice(sealed_envelope);
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

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_crypto::agreement::SecretKey as X25519SecretKey;
    use lakko_crypto::signing::SigningKey;

    fn sender() -> SigningKey {
        SigningKey::from_seed(&[7u8; 32])
    }

    /// Stable test `account_id`. Real callers pass a real
    /// UUIDv7; the wire format is byte-agnostic.
    fn sender_account_id() -> [u8; ACCOUNT_ID_LEN] {
        [0x77; ACCOUNT_ID_LEN]
    }

    fn recipient_x25519() -> X25519SecretKey {
        // X25519 keypair (separate from the recipient's Ed25519
        // identity key — in practice the client derives one from
        // the other or maintains both side by side).
        X25519SecretKey::from_bytes([0xCD; 32])
    }

    #[test]
    fn compose_and_decode_roundtrip() {
        let alice = sender();
        let bob = recipient_x25519();
        let pseudonym = [0xAA; PSEUDONYM_LEN];

        let wire = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            pseudonym,
            1_000_000,
            b"hi bob",
        )
        .unwrap();
        let req = decode_and_verify(&wire).unwrap();

        assert_eq!(req.from_pubkey.to_bytes(), alice.verifying_key().to_bytes());
        assert_eq!(req.from_account_id, Some(sender_account_id()));
        assert_eq!(req.from_pseudonym, pseudonym);
        assert_eq!(req.valid_after_ms, 1_000_000);

        let intro = open_envelope(&req, &bob).unwrap();
        assert_eq!(intro, b"hi bob");
    }

    /// Phase 1.16e3b: v2 wire layout. `from_account_id` lands
    /// between `from_pubkey` (offset 1..33) and `from_pseudonym`
    /// (offset 1+32+16 = 49 in v2). Pin the offset down with a
    /// direct byte read so a future schema reshuffle surfaces
    /// here.
    #[test]
    fn v2_wire_carries_account_id_at_fixed_offset() {
        let alice = sender();
        let bob = recipient_x25519();
        let acct = [0xA5u8; ACCOUNT_ID_LEN];

        let wire = compose(
            &alice,
            &acct,
            &bob.public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            b"",
        )
        .unwrap();

        // Byte 0: schema version.
        assert_eq!(wire[0], SCHEMA_VERSION);
        // Bytes 1..=32: from_pubkey.
        assert_eq!(
            &wire[1..=PUBLIC_KEY_LEN],
            &alice.verifying_key().to_bytes()[..]
        );
        // Bytes 33..=48: from_account_id.
        let acct_start = 1 + PUBLIC_KEY_LEN;
        let acct_end = acct_start + ACCOUNT_ID_LEN;
        assert_eq!(&wire[acct_start..acct_end], &acct[..]);
    }

    /// Phase 1.16e3b: v1 envelopes (no `from_account_id`) still
    /// decode for backwards-compat. The synthesised v1 wire is
    /// produced by stripping the schema-version byte and the
    /// 16-byte account_id from a v2 wire and re-signing under
    /// the v1 prefix shape.
    #[test]
    fn v1_envelopes_still_decode() {
        let alice = sender();
        let bob = recipient_x25519();
        let pseudonym = [0xBBu8; PSEUDONYM_LEN];
        let valid_after_ms = 12345_u64;
        let intro = b"legacy hi";

        let sealed_envelope = envelope::seal(&bob.public_key(), ENVELOPE_AAD, intro).unwrap();
        let from_pubkey = alice.verifying_key();
        let signed_prefix =
            encode_signing_prefix_v1(&from_pubkey, &pseudonym, valid_after_ms, &sealed_envelope);
        let signature = alice.sign(&signed_prefix);
        let mut wire = signed_prefix;
        wire.extend_from_slice(&signature.to_bytes());

        let req = decode_and_verify(&wire).unwrap();
        assert_eq!(req.from_account_id, None);
        assert_eq!(req.from_pseudonym, pseudonym);
        assert_eq!(req.valid_after_ms, valid_after_ms);
        let opened = open_envelope(&req, &bob).unwrap();
        assert_eq!(opened, intro);
    }

    #[test]
    fn empty_intro_message_is_supported() {
        let alice = sender();
        let bob = recipient_x25519();
        let pseudonym = [0u8; PSEUDONYM_LEN];

        let wire = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            pseudonym,
            0,
            b"",
        )
        .unwrap();
        let req = decode_and_verify(&wire).unwrap();
        let intro = open_envelope(&req, &bob).unwrap();
        assert_eq!(intro, b"");
    }

    #[test]
    fn tampered_signature_rejected() {
        let alice = sender();
        let bob = recipient_x25519();

        let mut wire = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            b"intro",
        )
        .unwrap();
        // Flip a bit in the trailing signature.
        let last = wire.len() - 1;
        wire[last] ^= 0x01;

        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::BadSignature));
    }

    #[test]
    fn tampered_pseudonym_rejected() {
        let alice = sender();
        let bob = recipient_x25519();
        let pseudonym = [0xAB; PSEUDONYM_LEN];

        let mut wire = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            pseudonym,
            0,
            b"intro",
        )
        .unwrap();
        // Flip a byte inside the v2 signed prefix. Offset is
        // schema-version + from_pubkey + from_account_id, i.e.
        // the first byte of from_pseudonym.
        wire[1 + PUBLIC_KEY_LEN + ACCOUNT_ID_LEN] ^= 0xFF;

        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::BadSignature));
    }

    #[test]
    fn tampered_envelope_breaks_open_not_verify() {
        let alice = sender();
        let bob = recipient_x25519();
        let pseudonym = [0u8; PSEUDONYM_LEN];

        let mut wire = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            pseudonym,
            0,
            b"secret",
        )
        .unwrap();
        // The signature covers the envelope, so any tamper is
        // signature-detected first. Confirm that.
        let mid = wire.len() / 2;
        wire[mid] ^= 0xFF;
        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::BadSignature));
    }

    #[test]
    fn wrong_recipient_fails_open_envelope() {
        let alice = sender();
        let bob = recipient_x25519();
        let attacker = X25519SecretKey::from_bytes([0xEF; 32]);

        let wire = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            b"private",
        )
        .unwrap();
        let req = decode_and_verify(&wire).unwrap();
        let err = open_envelope(&req, &attacker).unwrap_err();
        assert!(matches!(err, ContactError::WrongRecipient));
    }

    #[test]
    fn wrong_schema_version_rejected() {
        let alice = sender();
        let bob = recipient_x25519();

        let mut wire = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            b"x",
        )
        .unwrap();
        wire[0] = 99;

        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    #[test]
    fn truncated_wire_rejected() {
        let alice = sender();
        let bob = recipient_x25519();

        let wire = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            b"x",
        )
        .unwrap();
        let truncated = &wire[..wire.len() - 5];
        let err = decode_and_verify(truncated).unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    #[test]
    fn trailing_bytes_rejected() {
        let alice = sender();
        let bob = recipient_x25519();

        let mut wire = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            b"x",
        )
        .unwrap();
        wire.push(0x00);
        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    #[test]
    fn fresh_compose_produces_distinct_wires() {
        // Same inputs but a fresh ephemeral envelope every call —
        // the bytes must differ.
        let alice = sender();
        let bob = recipient_x25519();
        let pseudonym = [0u8; PSEUDONYM_LEN];

        let w1 = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            pseudonym,
            0,
            b"intro",
        )
        .unwrap();
        let w2 = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            pseudonym,
            0,
            b"intro",
        )
        .unwrap();
        assert_ne!(w1, w2);
    }
}
