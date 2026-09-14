// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! [`ContactRequest`] — the cold-contact envelope per ADR-0005 §13.2.
//!
//! Sender Alice composes a request for recipient Bob:
//!
//! 1. Encrypts her optional profile (ADR-0048) and the optional intro
//!    message to Bob's identity public key via [`crate::envelope`]
//!    sealed-box (HPKE-equivalent over X25519 + ChaCha20-Poly1305).
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
//! 3. Opens the sealed envelope with his identity X25519 private key,
//!    recovering Alice's profile (if any) and the intro message.
//! 4. Surfaces the request to UI: Accept / Decline / Block / Ignore.
//!
//! ## Sealed plaintext (ADR-0048 §2)
//!
//! The bytes inside `sealed_envelope` are a length-prefixed pair:
//!
//! ```text
//! u16be        profile_len          — 0 = no profile
//! [..]         profile envelope     — crate::profile wire, `profile_len` bytes
//! [..]         intro message        — everything that remains (may be empty)
//! ```
//!
//! There is no dual format: nothing is deployed, so a sealed plaintext
//! without the prefix is simply malformed.
//!
//! **Who the profile belongs to.** The envelope carries no identity.
//! The recipient binds the profile to `from_pubkey`, which the request
//! signature authenticates, and treats every field as that sender's
//! claim — the username is shown only after discovery verification
//! (ADR-0048 §3).
//!
//! **A malformed profile does not drop the request.** When the framing
//! is intact but the profile bytes fail [`crate::profile::decode_profile`]
//! (an out-of-bounds name, a look-alike username, a future version),
//! [`open_envelope`] returns the intro with the profile `None`: the
//! request itself is still authentic and still wanted, and the recipient
//! falls back to the fingerprint exactly as it would for a sender with
//! no profile. A **framing** error is different — a `profile_len` that
//! overruns the plaintext, or a plaintext shorter than the prefix, leaves
//! no defined boundary for the intro, so the open fails with
//! [`ContactError::InvalidEncoding`]. Only a sender with the recipient's
//! public key and its own signing key can produce either, since the
//! plaintext is AEAD-authenticated inside a signed request.
//!
//! ## Wire format (manual codec)
//!
//! **v3** (current — Phase 1.9b2):
//!
//! ```text
//! u8           schema version (= 3)
//! [32 bytes]   from_pubkey                           — Ed25519 verifying key
//! [16 bytes]   from_account_id                       — sender's UUIDv7
//! [32 bytes]   from_pseudonym                        — fresh per-recipient pseudonym
//! u64be        valid_after_ms                        — UNIX millis; reject if too old
//! u16be        privacy_pass_token length             — 0 = no token (dev / legacy)
//! [..]         privacy_pass_token bytes              — opaque blob (RFC 9576)
//! u32be        sealed_envelope length
//! [..]         sealed_envelope bytes
//! [64 bytes]   signature                             — Ed25519 over the prefix above
//! ```
//!
//! **v2** (legacy — accepted on receive only — Phase 1.16e3b
//! through 1.9b1): identical to v3 minus the
//! `privacy_pass_token` length+bytes pair. Receivers decode v2
//! envelopes for backwards-compat with the in-flight rolling-
//! upgrade window; the resulting [`ContactRequest`] carries
//! `privacy_pass_token: Vec::new()`. No `compose` path emits v2
//! any more.
//!
//! **v1** (legacy — accepted on receive only): identical to v2
//! minus the 16-byte `from_account_id` field. Receivers decode
//! v1; the resulting [`ContactRequest`] carries
//! `from_account_id: None` and `privacy_pass_token: Vec::new()`.
//!
//! The `from_account_id` extension (v2) lets the receiver drive
//! the Accept-side of the group-creation handshake: the receiver
//! needs `account_id` to fetch the sender's KeyPackage, but a v1
//! ContactRequest only carried the Ed25519 pubkey.
//!
//! The `privacy_pass_token` extension (v3 — Phase 1.9b2) carries
//! a Privacy Pass / App-Attest / Play-Integrity attestation blob
//! through the envelope so `services/ds`'s ingest layer can lift
//! the per-stranger rate-limit ceiling for clients that prove they
//! run on attested hardware (RFC 9576). The token is opaque to
//! `lakko-contacts` itself — the verification keypair and batch-
//! size choreography live in the DS sub-sprint. In dev and on
//! web builds the token field is empty bytes; production mobile
//! builds populate it from the platform attestation API.
//!
//! Signature covers everything from the schema-version byte through
//! the sealed-envelope bytes — i.e. the entire encoding except the
//! trailing 64-byte signature itself. Bound to the token so a
//! man-in-the-middle cannot strip / forge attestation.
//!
//! ## What's NOT in 1.9b2
//!
//! The plan also calls for `pow_token` (Equix proof-of-work). PoW
//! is an anti-spam mechanism at the `services/ds` ingest path;
//! gating it in the wire format itself is straightforward
//! (extra field with its own length prefix), but spoke
//! implementations belong in the PoW sub-sprint. The current
//! wire format is structured so adding optional fields at the
//! end stays backwards-compatible — a v4 schema bumps the
//! version byte.

use lakko_crypto::agreement::{PublicKey as X25519PublicKey, SecretKey as X25519SecretKey};
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN, SIGNATURE_LEN};

use crate::envelope;
use crate::error::{ContactError, Result};
use crate::profile::{compose_profile, decode_profile, Profile};
use crate::pseudonym::{decode_pseudonym, PSEUDONYM_LEN};

/// Current `compose`d schema version (Phase 1.9b2 — Privacy
/// Pass token field).
const SCHEMA_VERSION: u8 = 3;

/// Legacy schema version (Phase 1.16e3b — `from_account_id`).
/// Receivers still decode v2 for backwards-compat with envelopes
/// in-flight at the moment of a rolling upgrade; no caller in
/// this codebase should produce v2 any more.
const LEGACY_SCHEMA_VERSION_V2: u8 = 2;

/// Earliest legacy schema version. Receivers still decode v1 for
/// backwards-compat with envelopes in-flight at the moment of a
/// rolling upgrade; no caller in this codebase should produce v1
/// any more.
const LEGACY_SCHEMA_VERSION_V1: u8 = 1;

/// `from_account_id` length in bytes (UUIDv7 raw, matches
/// `services/auth.accounts.id` and the rest of the Lakko stack).
pub const ACCOUNT_ID_LEN: usize = 16;

/// Hard upper bound on the Privacy Pass token blob (1.9b2). Real
/// RFC 9576 tokens land around 80–160 bytes; the cap is a sanity
/// gate against pathologically large attestation payloads
/// crashing parsers. The wire field is `u16` so any value up to
/// 65535 would otherwise decode — we cap earlier so callers see
/// a clean [`ContactError::InvalidEncoding`] instead.
pub const MAX_PRIVACY_PASS_TOKEN_LEN: usize = 2048;

/// Byte length of the `profile_len` prefix at the head of the sealed
/// plaintext (see the module docs).
pub const PROFILE_LEN_PREFIX_LEN: usize = 2;

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
    /// Privacy Pass (RFC 9576) attestation token (Phase 1.9b2).
    /// Carried as an opaque blob — `lakko-contacts` does not
    /// interpret it. The DS ingest layer at
    /// `services/ds/src/handlers/mailbox.rs` lifts the per-
    /// stranger rate-limit ceiling for senders that supply a
    /// verifiable token.
    ///
    /// **Empty `Vec` means "no token"** — accepted on the receive
    /// path (v1/v2 envelopes always decode here; v3 envelopes
    /// from clients without an attestation API also land empty
    /// in dev). Production builds populate this from App Attest
    /// (iOS) / Play Integrity (Android) / a server-issued token
    /// (web fallback).
    pub privacy_pass_token: Vec<u8>,
    /// Sealed envelope, opaque to anyone but the recipient.
    pub sealed_envelope: Vec<u8>,
}

/// Compose a `ContactRequest` and produce its signed wire-format bytes.
///
/// The sender supplies their Ed25519 signing key (for authentication),
/// the recipient's X25519 public key (for sealed-envelope encryption),
/// an optional profile (ADR-0048), an optional intro message body, and
/// an optional Privacy Pass
/// attestation token (Phase 1.9b2). `from_pseudonym` should be
/// freshly generated per-recipient (use
/// [`crate::pseudonym::PerRelationship::derive`] with a one-shot
/// seed, or just 32 random bytes — a pseudonym is just routing
/// metadata).
///
/// Pass an **empty slice** as `privacy_pass_token` to omit it
/// (dev environments + web fallback). Production mobile builds
/// supply the platform-attestation blob (App Attest on iOS, Play
/// Integrity on Android).
///
/// # Errors
///
/// * [`ContactError::Crypto`] on AEAD failure (essentially
///   impossible for valid inputs).
/// * [`ContactError::InvalidEncoding`] when
///   `privacy_pass_token.len() > MAX_PRIVACY_PASS_TOKEN_LEN`.
/// * [`ContactError::InvalidArgument`] when `profile` violates a bound —
///   the same bounds the recipient's decoder enforces, so a conforming
///   sender never seals a profile the recipient would drop.
pub fn compose(
    sender_signing: &SigningKey,
    sender_account_id: &[u8; ACCOUNT_ID_LEN],
    recipient_x25519_pub: &X25519PublicKey,
    from_pseudonym: [u8; PSEUDONYM_LEN],
    valid_after_ms: u64,
    privacy_pass_token: &[u8],
    profile: Option<&Profile>,
    intro_message: &[u8],
) -> Result<Vec<u8>> {
    if privacy_pass_token.len() > MAX_PRIVACY_PASS_TOKEN_LEN {
        return Err(ContactError::InvalidEncoding(format!(
            "privacy_pass_token must be <= {MAX_PRIVACY_PASS_TOKEN_LEN} bytes, got {}",
            privacy_pass_token.len(),
        )));
    }
    let sealed_plaintext = frame_sealed_plaintext(profile, intro_message)?;
    let sealed_envelope = envelope::seal(recipient_x25519_pub, ENVELOPE_AAD, &sealed_plaintext)?;

    let from_pubkey = sender_signing.verifying_key();
    let mut wire = encode_signing_prefix(
        &from_pubkey,
        sender_account_id,
        &from_pseudonym,
        valid_after_ms,
        privacy_pass_token,
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
    if version != SCHEMA_VERSION
        && version != LEGACY_SCHEMA_VERSION_V2
        && version != LEGACY_SCHEMA_VERSION_V1
    {
        return Err(ContactError::InvalidEncoding(format!(
            "unsupported ContactRequest schema version {version}",
        )));
    }
    let from_pubkey_bytes = take_n(&mut cur, PUBLIC_KEY_LEN)?;
    let from_pubkey = VerifyingKey::from_bytes(from_pubkey_bytes).map_err(ContactError::Crypto)?;

    // v2+ inserts the 16-byte sender account_id between
    // `from_pubkey` and `from_pseudonym`. v1 omits it.
    let from_account_id: Option<[u8; ACCOUNT_ID_LEN]> = if version == LEGACY_SCHEMA_VERSION_V1 {
        None
    } else {
        let mut buf = [0u8; ACCOUNT_ID_LEN];
        buf.copy_from_slice(take_n(&mut cur, ACCOUNT_ID_LEN)?);
        Some(buf)
    };

    let from_pseudonym = decode_pseudonym(take_n(&mut cur, PSEUDONYM_LEN)?)?;
    let valid_after_ms = u64::from_be_bytes(take_n(&mut cur, 8)?.try_into().unwrap());

    // v3 inserts a u16-length-prefixed Privacy Pass token here.
    // v1/v2 envelopes omit the field — the resulting struct
    // carries `privacy_pass_token: Vec::new()` to preserve the
    // existing receive-side semantics.
    let privacy_pass_token: Vec<u8> = if version == SCHEMA_VERSION {
        let token_len = u16::from_be_bytes(take_n(&mut cur, 2)?.try_into().unwrap()) as usize;
        if token_len > MAX_PRIVACY_PASS_TOKEN_LEN {
            return Err(ContactError::InvalidEncoding(format!(
                "privacy_pass_token over cap ({token_len} > {MAX_PRIVACY_PASS_TOKEN_LEN})",
            )));
        }
        take_n(&mut cur, token_len)?.to_vec()
    } else {
        Vec::new()
    };

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
    // The codec branch is keyed off the schema version we just
    // peeled off the wire — a tampered version byte would have
    // either failed parsing above or driven a mismatched
    // signature here.
    let signed_prefix = if version == SCHEMA_VERSION {
        encode_signing_prefix(
            &from_pubkey,
            // SAFETY: v2/v3 path implies `Some(_)`.
            from_account_id
                .as_ref()
                .ok_or_else(|| ContactError::InvalidEncoding("missing account_id".into()))?,
            &from_pseudonym,
            valid_after_ms,
            &privacy_pass_token,
            &sealed_envelope,
        )
    } else if version == LEGACY_SCHEMA_VERSION_V2 {
        encode_signing_prefix_v2(
            &from_pubkey,
            from_account_id
                .as_ref()
                .ok_or_else(|| ContactError::InvalidEncoding("missing account_id".into()))?,
            &from_pseudonym,
            valid_after_ms,
            &sealed_envelope,
        )
    } else {
        encode_signing_prefix_v1(
            &from_pubkey,
            &from_pseudonym,
            valid_after_ms,
            &sealed_envelope,
        )
    };
    from_pubkey
        .verify(&signed_prefix, &signature)
        .map_err(|_| ContactError::BadSignature)?;

    Ok(ContactRequest {
        from_pubkey,
        from_account_id,
        from_pseudonym,
        valid_after_ms,
        privacy_pass_token,
        sealed_envelope,
    })
}

/// Decrypt the sealed envelope on a previously-verified
/// [`ContactRequest`] using the recipient's X25519 private key.
///
/// Returns `(profile, intro_message)`. The profile, when present, is the
/// sender's claim bound to `req.from_pubkey` — see the module docs. A
/// profile that fails its bounds yields `None` with the intro intact.
///
/// # Errors
///
/// - [`ContactError::WrongRecipient`] — envelope was sealed for a
///   different recipient (or tampered with after sealing).
/// - [`ContactError::InvalidEncoding`] — envelope wire format is
///   malformed, or the sealed plaintext's profile framing is.
pub fn open_envelope(
    req: &ContactRequest,
    recipient_priv: &X25519SecretKey,
) -> Result<(Option<Profile>, Vec<u8>)> {
    let plaintext = envelope::open(recipient_priv, ENVELOPE_AAD, &req.sealed_envelope)?;
    let (profile, intro) = split_sealed_plaintext(&plaintext)?;
    Ok((profile, intro.to_vec()))
}

/// Build the sealed plaintext `profile_len ‖ profile ‖ intro`.
fn frame_sealed_plaintext(profile: Option<&Profile>, intro_message: &[u8]) -> Result<Vec<u8>> {
    let profile_bytes = match profile {
        Some(p) => compose_profile(p)
            .map_err(|e| ContactError::InvalidArgument(format!("profile: {e}")))?,
        None => Vec::new(),
    };
    // A composed profile is at most `PROFILE_ENVELOPE_MAX_LEN` bytes, far
    // below `u16::MAX`; checked rather than cast all the same.
    let profile_len = u16::try_from(profile_bytes.len())
        .map_err(|_| ContactError::InvalidArgument("profile envelope too large".to_string()))?;
    let mut out =
        Vec::with_capacity(PROFILE_LEN_PREFIX_LEN + profile_bytes.len() + intro_message.len());
    out.extend_from_slice(&profile_len.to_be_bytes());
    out.extend_from_slice(&profile_bytes);
    out.extend_from_slice(intro_message);
    Ok(out)
}

/// Split an opened sealed plaintext into `(profile, intro)`.
///
/// Framing errors are errors; a well-framed but invalid profile is
/// `None` (see the module docs for why the two differ).
fn split_sealed_plaintext(plaintext: &[u8]) -> Result<(Option<Profile>, &[u8])> {
    let mut cur = plaintext;
    let len_bytes: [u8; PROFILE_LEN_PREFIX_LEN] = take_n(&mut cur, PROFILE_LEN_PREFIX_LEN)?
        .try_into()
        .map_err(|_| ContactError::InvalidEncoding("profile length prefix".to_string()))?;
    let profile_len = usize::from(u16::from_be_bytes(len_bytes));
    let profile_bytes = take_n(&mut cur, profile_len)?;
    let profile = if profile_bytes.is_empty() {
        None
    } else {
        match decode_profile(profile_bytes) {
            Ok(p) => Some(p),
            Err(e) => {
                // The reason code only: the profile's content is the
                // sender's personal data and never reaches a log.
                tracing::warn!(reason = %e.code(), "contact request carried an invalid profile; opening without it");
                None
            },
        }
    };
    Ok((profile, cur))
}

// ============================================================================
// Codec helpers
// ============================================================================

/// Encode the bytes that get signed for the current (v3) wire
/// format: everything except the trailing signature itself.
fn encode_signing_prefix(
    from_pubkey: &VerifyingKey,
    from_account_id: &[u8; ACCOUNT_ID_LEN],
    from_pseudonym: &[u8; PSEUDONYM_LEN],
    valid_after_ms: u64,
    privacy_pass_token: &[u8],
    sealed_envelope: &[u8],
) -> Vec<u8> {
    let sealed_len = u32::try_from(sealed_envelope.len())
        .expect("sealed envelope under u32::MAX bytes — caller invariant");
    let token_len = u16::try_from(privacy_pass_token.len())
        .expect("privacy_pass_token capped at MAX_PRIVACY_PASS_TOKEN_LEN (<= u16::MAX)");
    let mut out = Vec::with_capacity(
        1 + PUBLIC_KEY_LEN
            + ACCOUNT_ID_LEN
            + PSEUDONYM_LEN
            + 8
            + 2
            + privacy_pass_token.len()
            + 4
            + sealed_envelope.len(),
    );
    out.push(SCHEMA_VERSION);
    out.extend_from_slice(&from_pubkey.to_bytes());
    out.extend_from_slice(from_account_id);
    out.extend_from_slice(from_pseudonym);
    out.extend_from_slice(&valid_after_ms.to_be_bytes());
    out.extend_from_slice(&token_len.to_be_bytes());
    out.extend_from_slice(privacy_pass_token);
    out.extend_from_slice(&sealed_len.to_be_bytes());
    out.extend_from_slice(sealed_envelope);
    out
}

/// Re-encode a legacy v2 signing prefix for signature
/// verification on receive. Mirrors the 1.16e3b encoding — used
/// only by `decode_and_verify` when the wire byte is
/// `LEGACY_SCHEMA_VERSION_V2`. No `compose` path produces v2 any
/// more.
fn encode_signing_prefix_v2(
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
    out.push(LEGACY_SCHEMA_VERSION_V2);
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
            b"",
            None,
            b"hi bob",
        )
        .unwrap();
        let req = decode_and_verify(&wire).unwrap();

        assert_eq!(req.from_pubkey.to_bytes(), alice.verifying_key().to_bytes());
        assert_eq!(req.from_account_id, Some(sender_account_id()));
        assert_eq!(req.from_pseudonym, pseudonym);
        assert_eq!(req.valid_after_ms, 1_000_000);
        assert!(req.privacy_pass_token.is_empty());

        let (profile, intro) = open_envelope(&req, &bob).unwrap();
        assert_eq!(profile, None);
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
            None,
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

        let sealed_envelope = envelope::seal(
            &bob.public_key(),
            ENVELOPE_AAD,
            &frame_sealed_plaintext(None, intro).unwrap(),
        )
        .unwrap();
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
        let (_, opened) = open_envelope(&req, &bob).unwrap();
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
            None,
            b"",
        )
        .unwrap();
        let req = decode_and_verify(&wire).unwrap();
        let (_, intro) = open_envelope(&req, &bob).unwrap();
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
            b"",
            None,
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
            b"",
            None,
            b"intro",
        )
        .unwrap();
        // Flip a byte inside the v3 signed prefix. Offset is
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
            b"",
            None,
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
            b"",
            None,
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
            b"",
            None,
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
            b"",
            None,
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
            b"",
            None,
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
            b"",
            None,
            b"intro",
        )
        .unwrap();
        let w2 = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            pseudonym,
            0,
            b"",
            None,
            b"intro",
        )
        .unwrap();
        assert_ne!(w1, w2);
    }

    // ========================================================
    // 1.9b2 — Privacy Pass token (v3 wire format)
    // ========================================================

    /// A v3 envelope with a non-empty token round-trips, the
    /// signature covers the token, and the parsed
    /// `privacy_pass_token` matches what the sender supplied.
    #[test]
    fn v3_privacy_pass_token_roundtrip() {
        let alice = sender();
        let bob = recipient_x25519();
        let token = b"\x01\x02\x03\x04 token-blob \xFF\xEE".as_slice();

        let wire = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            [0xAB; PSEUDONYM_LEN],
            0,
            token,
            None,
            b"hello",
        )
        .unwrap();

        let req = decode_and_verify(&wire).unwrap();
        assert_eq!(req.privacy_pass_token.as_slice(), token);

        let (_, opened) = open_envelope(&req, &bob).unwrap();
        assert_eq!(opened, b"hello");
    }

    /// Tampering the on-wire token bytes invalidates the
    /// signature — token integrity is bound to the signed
    /// prefix, not stapled on as an afterthought.
    #[test]
    fn v3_tampered_token_breaks_signature() {
        let alice = sender();
        let bob = recipient_x25519();
        let token = b"\x10\x20\x30\x40".as_slice();

        let mut wire = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            token,
            None,
            b"",
        )
        .unwrap();

        // Token lands at offset
        //   1 (version) + 32 (pubkey) + 16 (acct) + 32 (pseudo)
        //   + 8 (valid_after_ms) + 2 (token_len) = 91.
        let token_offset = 1 + PUBLIC_KEY_LEN + ACCOUNT_ID_LEN + PSEUDONYM_LEN + 8 + 2;
        wire[token_offset] ^= 0xFF;

        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::BadSignature));
    }

    /// `MAX_PRIVACY_PASS_TOKEN_LEN` is enforced on `compose`. The
    /// receive path also enforces it (via the u16 length cap
    /// check) so a malicious sender can't smuggle pathologically
    /// large blobs through a permissive composer.
    #[test]
    fn v3_token_over_cap_rejected_at_compose() {
        let alice = sender();
        let bob = recipient_x25519();
        let huge = vec![0u8; MAX_PRIVACY_PASS_TOKEN_LEN + 1];

        let err = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            &huge,
            None,
            b"",
        )
        .unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    /// Phase 1.16e3b's v2 envelopes still decode on the v3
    /// receiver. The `privacy_pass_token` slot lands empty —
    /// matching the dev / web fallback path. Synthesised here
    /// by hand-rolling the v2 signed prefix (the only place
    /// `encode_signing_prefix_v2` is exercised outside its own
    /// private decode branch).
    #[test]
    fn v2_envelopes_still_decode_with_empty_token() {
        let alice = sender();
        let bob = recipient_x25519();
        let acct = [0xC3u8; ACCOUNT_ID_LEN];
        let pseudonym = [0xD4u8; PSEUDONYM_LEN];
        let valid_after_ms = 42_u64;
        let intro = b"v2 envelope";

        let sealed_envelope = envelope::seal(
            &bob.public_key(),
            ENVELOPE_AAD,
            &frame_sealed_plaintext(None, intro).unwrap(),
        )
        .unwrap();
        let from_pubkey = alice.verifying_key();
        let signed_prefix = encode_signing_prefix_v2(
            &from_pubkey,
            &acct,
            &pseudonym,
            valid_after_ms,
            &sealed_envelope,
        );
        let signature = alice.sign(&signed_prefix);
        let mut wire = signed_prefix;
        wire.extend_from_slice(&signature.to_bytes());

        let req = decode_and_verify(&wire).unwrap();
        assert_eq!(req.from_account_id, Some(acct));
        assert_eq!(req.from_pseudonym, pseudonym);
        assert_eq!(req.valid_after_ms, valid_after_ms);
        assert!(
            req.privacy_pass_token.is_empty(),
            "v2 wire should produce an empty token on v3 decode",
        );
        let (_, opened) = open_envelope(&req, &bob).unwrap();
        assert_eq!(opened, intro);
    }

    // ========================================================
    // ADR-0048 — profile in the sealed plaintext
    // ========================================================

    fn alice_profile() -> Profile {
        Profile {
            display_name: Some("Alice Petrova".to_string()),
            username: Some("alice".to_string()),
            updated_at_ms: 1_700_000_000_000,
        }
    }

    /// Seal `plaintext` as-is (bypassing `compose`'s framing) inside an
    /// otherwise-valid, correctly signed v3 request.
    fn signed_request_sealing(plaintext: &[u8]) -> ContactRequest {
        let alice = sender();
        let bob = recipient_x25519();
        let sealed_envelope = envelope::seal(&bob.public_key(), ENVELOPE_AAD, plaintext).unwrap();
        let mut wire = encode_signing_prefix(
            &alice.verifying_key(),
            &sender_account_id(),
            &[0u8; PSEUDONYM_LEN],
            0,
            b"",
            &sealed_envelope,
        );
        let signature = alice.sign(&wire);
        wire.extend_from_slice(&signature.to_bytes());
        decode_and_verify(&wire).unwrap()
    }

    #[test]
    fn profile_and_intro_round_trip() {
        let alice = sender();
        let bob = recipient_x25519();
        let wire = compose(
            &alice,
            &sender_account_id(),
            &bob.public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            b"",
            Some(&alice_profile()),
            b"hi bob",
        )
        .unwrap();
        let req = decode_and_verify(&wire).unwrap();
        let (profile, intro) = open_envelope(&req, &bob).unwrap();
        assert_eq!(profile, Some(alice_profile()));
        assert_eq!(intro, b"hi bob");
    }

    #[test]
    fn profile_with_empty_intro_round_trips() {
        let bob = recipient_x25519();
        let wire = compose(
            &sender(),
            &sender_account_id(),
            &bob.public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            b"",
            Some(&alice_profile()),
            b"",
        )
        .unwrap();
        let (profile, intro) = open_envelope(&decode_and_verify(&wire).unwrap(), &bob).unwrap();
        assert_eq!(profile, Some(alice_profile()));
        assert!(intro.is_empty());
    }

    #[test]
    fn sealed_plaintext_framing_is_pinned() {
        let framed = frame_sealed_plaintext(Some(&alice_profile()), b"intro").unwrap();
        let profile_wire = crate::profile::compose_profile(&alice_profile()).unwrap();
        let len = u16::try_from(profile_wire.len()).unwrap();
        assert_eq!(&framed[..2], &len.to_be_bytes());
        assert_eq!(&framed[2..2 + profile_wire.len()], profile_wire.as_slice());
        assert_eq!(&framed[2 + profile_wire.len()..], b"intro");
        // No profile: a zero prefix, then the intro verbatim.
        assert_eq!(frame_sealed_plaintext(None, b"x").unwrap(), b"\x00\x00x");
    }

    #[test]
    fn compose_refuses_an_out_of_bounds_profile() {
        let mut bad = alice_profile();
        bad.username = Some("p\u{430}vel".to_string());
        let err = compose(
            &sender(),
            &sender_account_id(),
            &recipient_x25519().public_key(),
            [0u8; PSEUDONYM_LEN],
            0,
            b"",
            Some(&bad),
            b"hi",
        )
        .unwrap_err();
        assert!(matches!(err, ContactError::InvalidArgument(_)));
    }

    /// The decision this carrier makes: a well-framed but invalid profile
    /// is dropped, the authentic request and its intro are kept.
    #[test]
    fn a_malformed_profile_opens_the_request_without_it() {
        let bob = recipient_x25519();
        // A profile whose name carries a right-to-left override.
        let name = "Alice\u{202E}".as_bytes();
        let mut bad_profile = crate::profile::PROFILE_ENVELOPE_DOMAIN_V1.to_vec();
        bad_profile.push(crate::profile::PROFILE_VERSION_V1);
        bad_profile.extend_from_slice(&0u64.to_be_bytes());
        bad_profile.push(u8::try_from(name.len()).unwrap());
        bad_profile.extend_from_slice(name);
        bad_profile.push(0);
        // Control: the same bytes are refused by the profile decoder.
        assert!(crate::profile::decode_profile(&bad_profile).is_err());

        for garbage in [bad_profile.clone(), b"not a profile at all".to_vec()] {
            let mut plaintext = u16::try_from(garbage.len()).unwrap().to_be_bytes().to_vec();
            plaintext.extend_from_slice(&garbage);
            plaintext.extend_from_slice(b"hi bob");
            let req = signed_request_sealing(&plaintext);
            let (profile, intro) = open_envelope(&req, &bob).unwrap();
            assert_eq!(profile, None);
            assert_eq!(intro, b"hi bob");
        }
    }

    /// Framing errors, unlike profile errors, leave no intro boundary and
    /// fail the open.
    #[test]
    fn broken_framing_fails_the_open() {
        let bob = recipient_x25519();
        for plaintext in [
            Vec::new(),
            vec![0x00],
            // Declares 10 profile bytes, carries 3.
            vec![0x00, 0x0A, 1, 2, 3],
            // A raw, unframed intro: "le" reads as a 27749-byte profile.
            b"legacy hi".to_vec(),
        ] {
            let req = signed_request_sealing(&plaintext);
            assert!(
                matches!(
                    open_envelope(&req, &bob),
                    Err(ContactError::InvalidEncoding(_))
                ),
                "{plaintext:?}"
            );
        }
    }
}
