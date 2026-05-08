// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! [`InviteCapability`] — capability-based contact invites per
//! ADR-0005 §13.3 / §13.4 (Phase 1.18a).
//!
//! Owner Alice generates a capability, embeds the wire bytes
//! (base64url) into a `https://lakko.me/i/<...>` link, shares the
//! link out-of-band. Joiner Bob opens the link, the web client
//! decodes + verifies the capability, and uses it to seed the
//! first ContactRequest back to Alice — short-circuiting the
//! `@username` lookup step from scenario A.
//!
//! ## What this V1 ships and what it doesn't
//!
//! ADR-0005 §13.3 specifies a richer `InviteCapability` than what
//! lands here:
//!
//!   * `owner_transparency_proof` (KT-log inclusion) — anchors
//!     `owner_signature_pubkey` to a verifiable identity record.
//!     Deferred: `lakko-transparency` does not yet expose
//!     inclusion proofs end-to-end. Without it, accepting a link
//!     is TOFU on the inviter's pubkey.
//!   * `consumed_pubkey_log: HmacBloom` — server-side bloom over
//!     joiners that have already consumed the link, paired with a
//!     `max_uses` enforcer. Deferred: needs a `services/discovery`
//!     `/invite/{blob_id}` endpoint that V1 does not have. The
//!     `max_uses` field is preserved on the wire as a **hint**
//!     for the joiner UI ("link can be used N more times") but
//!     no enforcement happens until 1.18d.
//!   * `blob_id` storage indirection — the ADR puts the
//!     capability bytes in `services/storage` and only embeds a
//!     short pointer in the URL. V1 puts the bytes **directly**
//!     in the URL. Trade-off: the URL is ~150-220 chars long
//!     instead of ~30, but no server dependency and no extra
//!     round-trip. We can swap to the indirection without a
//!     wire-format break — the capability struct stays the same;
//!     only the URL shape changes.
//!   * `ephemeral_prekey` — ADR keeps it for forward secrecy on
//!     the owner→joiner direction. Our flow has joiner→owner as
//!     the first sealed message, and the joiner generates a
//!     fresh ephemeral per envelope (`crate::envelope::seal`),
//!     so we don't need the owner-side prekey here.
//!
//! ## Wire format (manual codec, frozen at v1)
//!
//! ```text
//! u8           schema version (= 1)
//! [32 bytes]   owner_signature_pubkey            — Ed25519 verifying key
//! [16 bytes]   owner_account_id                  — UUIDv7 raw
//! [32 bytes]   owner_x25519_pubkey               — long-term HPKE init key
//! u64be        expiry_ms                          — UNIX millis; 0 means "no expiry"
//! u32be        max_uses                           — 0 means "single-use"
//! u8           bootstrap_referral                 — 0/1; flags signup deep-link
//! u16be        label_len
//! [label_len]  label_bytes (UTF-8, ≤ MAX_LABEL_LEN)
//! [64 bytes]   signature                          — Ed25519 over the above
//! ```
//!
//! Signature covers everything from the schema-version byte through
//! `label_bytes` inclusive — i.e. the entire encoding except the
//! trailing 64-byte signature itself. Mirrors `request.rs`'s
//! pattern exactly.
//!
//! All multi-byte integers are big-endian to match the rest of
//! Lakko's wire formats.
//!
//! ## Canonical use
//!
//! Sender side:
//!
//! ```ignore
//! let bytes = lakko_contacts::invite::compose(
//!     &owner_signing_key,
//!     &owner_account_id,
//!     &owner_x25519_pub,
//!     expiry_ms,
//!     max_uses,
//!     bootstrap_referral,
//!     "Alice's home",
//! )?;
//! let url = format!("https://lakko.me/i/{}", base64_url(&bytes));
//! ```
//!
//! Receiver side:
//!
//! ```ignore
//! let bytes = base64_url_decode(slug)?;
//! let cap = lakko_contacts::invite::decode_and_verify(&bytes)?;
//! if !cap.is_valid_now(now_ms) { /* expired */ }
//! // Use `cap.owner_signature_pubkey`, `cap.owner_account_id`,
//! // `cap.owner_x25519_pubkey` to seed the first ContactRequest.
//! ```

use lakko_crypto::agreement::{PublicKey as X25519PublicKey, PUBLIC_KEY_LEN as X25519_PUB_LEN};
use lakko_crypto::signing::{
    Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN as ED25519_PUB_LEN, SIGNATURE_LEN,
};

use crate::error::{ContactError, Result};
use crate::request::ACCOUNT_ID_LEN;

/// Current schema version on the wire.
const SCHEMA_VERSION: u8 = 1;

/// Hard cap on the human-visible label. 64 bytes UTF-8 is enough
/// for "Alice — work account" without bloating the URL beyond a
/// few hundred chars.
pub const MAX_LABEL_LEN: usize = 64;

/// Sentinel meaning "never expires". Honest value here is
/// problematic in a security-critical surface (a forever invite
/// is a stalking risk per ADR-0005 §13.3 — discouraged in UI),
/// but we accept it on the wire so an enterprise / pinned-bio
/// UX can opt into it explicitly.
pub const NEVER_EXPIRES: u64 = 0;

/// Sentinel meaning "single-use".
pub const SINGLE_USE: u32 = 0;

/// A composed invite capability, ready to wire-encode into a URL.
///
/// Sender side: produced by [`compose`].
/// Recipient side: produced by [`decode_and_verify`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InviteCapability {
    /// Inviter's long-term Ed25519 verifying key. The wire
    /// signature is verified against this; the joiner can
    /// independently look up the same pubkey via
    /// `/discovery/identity/{owner_account_id}` and confirm
    /// they match (TOFU check until KT-log inclusion proofs
    /// land).
    pub owner_signature_pubkey: VerifyingKey,
    /// Inviter's UUIDv7 raw bytes. Used by the joiner to
    /// fetch the inviter's KeyPackage from `services/discovery`
    /// when bootstrapping the MLS group.
    pub owner_account_id: [u8; ACCOUNT_ID_LEN],
    /// Inviter's long-term X25519 public key. The joiner seals
    /// their first ContactRequest envelope under this. Embedding
    /// it in the capability lets the joiner act on the link
    /// without first hitting `/discovery/identity` — useful when
    /// the discovery service is censored or temporarily down.
    pub owner_x25519_pubkey: X25519PublicKey,
    /// UNIX milliseconds when the invite stops being valid.
    /// `NEVER_EXPIRES` (= 0) means no expiry — UI MUST surface
    /// this as a stalking-risk warning (ADR-0005 §13.3).
    pub expiry_ms: u64,
    /// Maximum number of times the invite can be consumed.
    /// `SINGLE_USE` (= 0) is the typical case. Best-effort hint
    /// in V1 — server-side enforcement arrives in 1.18d.
    pub max_uses: u32,
    /// `true` for "this link is a referral for someone who
    /// doesn't have a Lakko account yet"; the signup flow uses
    /// it to auto-send a ContactRequest back to the owner once
    /// the new account is provisioned (ADR-0005 §13.4).
    pub bootstrap_referral: bool,
    /// Optional human-visible label, UTF-8, capped at
    /// [`MAX_LABEL_LEN`] bytes. Empty string is allowed — the
    /// UI fills in a default from the inviter's display name.
    pub label: String,
}

impl InviteCapability {
    /// `true` iff the capability has not expired at `now_ms`.
    /// `NEVER_EXPIRES` capabilities always return `true` —
    /// the stalking risk is the UI's responsibility to surface,
    /// not the codec's to enforce.
    #[must_use]
    pub fn is_valid_now(&self, now_ms: u64) -> bool {
        self.expiry_ms == NEVER_EXPIRES || now_ms < self.expiry_ms
    }
}

/// Compose an invite capability and produce its signed wire-format
/// bytes. The caller-supplied `label` is truncated-or-rejected per
/// the `label_too_long` path (we reject rather than truncate to
/// keep the wire format predictable for the receiver).
///
/// # Errors
///
/// - [`ContactError::InvalidArgument`] when `label.len() > MAX_LABEL_LEN`.
pub fn compose(
    owner_signing: &SigningKey,
    owner_account_id: &[u8; ACCOUNT_ID_LEN],
    owner_x25519_pubkey: &X25519PublicKey,
    expiry_ms: u64,
    max_uses: u32,
    bootstrap_referral: bool,
    label: &str,
) -> Result<Vec<u8>> {
    if label.len() > MAX_LABEL_LEN {
        return Err(ContactError::InvalidArgument(format!(
            "invite label is {} bytes, max is {}",
            label.len(),
            MAX_LABEL_LEN,
        )));
    }
    let owner_pubkey = owner_signing.verifying_key();
    let mut wire = encode_signing_prefix(
        &owner_pubkey,
        owner_account_id,
        owner_x25519_pubkey,
        expiry_ms,
        max_uses,
        bootstrap_referral,
        label,
    );
    let signature = owner_signing.sign(&wire);
    wire.extend_from_slice(&signature.to_bytes());
    Ok(wire)
}

/// Decode the wire bytes, verify the Ed25519 signature against
/// the embedded `owner_signature_pubkey`, return the validated
/// [`InviteCapability`].
///
/// Does **not** check `expiry_ms` — call
/// [`InviteCapability::is_valid_now`] separately if you want
/// freshness enforcement. Splitting the two lets a UI render
/// "this invite expired 3 days ago" rather than a generic
/// failure.
///
/// # Errors
///
/// - [`ContactError::InvalidEncoding`] — wire format malformed,
///   wrong schema version, oversized label, or trailing bytes.
/// - [`ContactError::BadSignature`] — Ed25519 verification failed.
/// - [`ContactError::Crypto`] — embedded pubkey bytes did not
///   parse as a valid Ed25519 / X25519 point.
pub fn decode_and_verify(wire: &[u8]) -> Result<InviteCapability> {
    let mut cur = wire;

    let version = take_u8(&mut cur)?;
    if version != SCHEMA_VERSION {
        return Err(ContactError::InvalidEncoding(format!(
            "unsupported InviteCapability schema version {version}",
        )));
    }
    let owner_signature_pubkey = VerifyingKey::from_bytes(take_n(&mut cur, ED25519_PUB_LEN)?)
        .map_err(ContactError::Crypto)?;
    let mut owner_account_id = [0u8; ACCOUNT_ID_LEN];
    owner_account_id.copy_from_slice(take_n(&mut cur, ACCOUNT_ID_LEN)?);
    let owner_x25519_pubkey = X25519PublicKey::from_bytes(take_n(&mut cur, X25519_PUB_LEN)?)
        .map_err(ContactError::Crypto)?;
    let expiry_ms = u64::from_be_bytes(
        take_n(&mut cur, 8)?
            .try_into()
            .expect("take_n returned 8 bytes"),
    );
    let max_uses = u32::from_be_bytes(
        take_n(&mut cur, 4)?
            .try_into()
            .expect("take_n returned 4 bytes"),
    );
    let bootstrap_referral_byte = take_u8(&mut cur)?;
    let bootstrap_referral = match bootstrap_referral_byte {
        0 => false,
        1 => true,
        other => {
            return Err(ContactError::InvalidEncoding(format!(
                "bootstrap_referral byte must be 0 or 1, got {other}",
            )));
        },
    };
    let label_len = u16::from_be_bytes(
        take_n(&mut cur, 2)?
            .try_into()
            .expect("take_n returned 2 bytes"),
    ) as usize;
    if label_len > MAX_LABEL_LEN {
        return Err(ContactError::InvalidEncoding(format!(
            "label_len {label_len} exceeds MAX_LABEL_LEN {MAX_LABEL_LEN}",
        )));
    }
    let label_bytes = take_n(&mut cur, label_len)?;
    let label = std::str::from_utf8(label_bytes)
        .map_err(|e| ContactError::InvalidEncoding(format!("label is not valid UTF-8: {e}")))?
        .to_string();
    let signature_bytes = take_n(&mut cur, SIGNATURE_LEN)?;
    let signature = Signature::from_bytes(signature_bytes).map_err(ContactError::Crypto)?;

    if !cur.is_empty() {
        return Err(ContactError::InvalidEncoding(format!(
            "{} trailing bytes after signature",
            cur.len(),
        )));
    }

    // Re-encode the prefix exactly as the signer did and verify.
    let signed_prefix = encode_signing_prefix(
        &owner_signature_pubkey,
        &owner_account_id,
        &owner_x25519_pubkey,
        expiry_ms,
        max_uses,
        bootstrap_referral,
        &label,
    );
    owner_signature_pubkey
        .verify(&signed_prefix, &signature)
        .map_err(|_| ContactError::BadSignature)?;

    Ok(InviteCapability {
        owner_signature_pubkey,
        owner_account_id,
        owner_x25519_pubkey,
        expiry_ms,
        max_uses,
        bootstrap_referral,
        label,
    })
}

// ============================================================================
// Codec helpers
// ============================================================================

fn encode_signing_prefix(
    owner_signature_pubkey: &VerifyingKey,
    owner_account_id: &[u8; ACCOUNT_ID_LEN],
    owner_x25519_pubkey: &X25519PublicKey,
    expiry_ms: u64,
    max_uses: u32,
    bootstrap_referral: bool,
    label: &str,
) -> Vec<u8> {
    let label_len_u16 =
        u16::try_from(label.len()).expect("label fits in u16; caller checked MAX_LABEL_LEN");
    let mut out = Vec::with_capacity(
        1 + ED25519_PUB_LEN + ACCOUNT_ID_LEN + X25519_PUB_LEN + 8 + 4 + 1 + 2 + label.len(),
    );
    out.push(SCHEMA_VERSION);
    out.extend_from_slice(&owner_signature_pubkey.to_bytes());
    out.extend_from_slice(owner_account_id);
    out.extend_from_slice(&owner_x25519_pubkey.to_bytes());
    out.extend_from_slice(&expiry_ms.to_be_bytes());
    out.extend_from_slice(&max_uses.to_be_bytes());
    out.push(u8::from(bootstrap_referral));
    out.extend_from_slice(&label_len_u16.to_be_bytes());
    out.extend_from_slice(label.as_bytes());
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

    fn fixture() -> (SigningKey, [u8; ACCOUNT_ID_LEN], X25519PublicKey) {
        let signing = SigningKey::generate();
        let account_id = [0xA1u8; ACCOUNT_ID_LEN];
        let x25519_secret = X25519SecretKey::generate();
        let x25519_pub = x25519_secret.public_key();
        (signing, account_id, x25519_pub)
    }

    #[test]
    fn compose_and_decode_roundtrip() {
        let (signing, account_id, x25519_pub) = fixture();
        let wire = compose(
            &signing,
            &account_id,
            &x25519_pub,
            1_700_000_000_000,
            5,
            false,
            "alice@home",
        )
        .unwrap();

        let cap = decode_and_verify(&wire).unwrap();
        assert_eq!(
            cap.owner_signature_pubkey.to_bytes(),
            signing.verifying_key().to_bytes()
        );
        assert_eq!(cap.owner_account_id, account_id);
        assert_eq!(cap.owner_x25519_pubkey.to_bytes(), x25519_pub.to_bytes());
        assert_eq!(cap.expiry_ms, 1_700_000_000_000);
        assert_eq!(cap.max_uses, 5);
        assert!(!cap.bootstrap_referral);
        assert_eq!(cap.label, "alice@home");
    }

    #[test]
    fn empty_label_round_trips() {
        let (signing, account_id, x25519_pub) = fixture();
        let wire = compose(&signing, &account_id, &x25519_pub, 0, 0, false, "").unwrap();
        let cap = decode_and_verify(&wire).unwrap();
        assert_eq!(cap.label, "");
    }

    #[test]
    fn never_expires_means_always_valid() {
        let (signing, account_id, x25519_pub) = fixture();
        let wire = compose(
            &signing,
            &account_id,
            &x25519_pub,
            NEVER_EXPIRES,
            0,
            false,
            "",
        )
        .unwrap();
        let cap = decode_and_verify(&wire).unwrap();
        assert!(cap.is_valid_now(0));
        assert!(cap.is_valid_now(u64::MAX));
    }

    #[test]
    fn expiry_in_the_past_invalidates() {
        let (signing, account_id, x25519_pub) = fixture();
        let wire = compose(
            &signing,
            &account_id,
            &x25519_pub,
            1_000_000_000,
            0,
            false,
            "",
        )
        .unwrap();
        let cap = decode_and_verify(&wire).unwrap();
        assert!(cap.is_valid_now(999_999_999));
        assert!(!cap.is_valid_now(1_000_000_000)); // boundary: equal = expired
        assert!(!cap.is_valid_now(2_000_000_000));
    }

    #[test]
    fn bootstrap_referral_round_trips() {
        let (signing, account_id, x25519_pub) = fixture();
        let wire = compose(&signing, &account_id, &x25519_pub, 0, 0, true, "").unwrap();
        let cap = decode_and_verify(&wire).unwrap();
        assert!(cap.bootstrap_referral);
    }

    #[test]
    fn label_at_max_len_round_trips() {
        let (signing, account_id, x25519_pub) = fixture();
        let label = "x".repeat(MAX_LABEL_LEN);
        let wire = compose(&signing, &account_id, &x25519_pub, 0, 0, false, &label).unwrap();
        let cap = decode_and_verify(&wire).unwrap();
        assert_eq!(cap.label.len(), MAX_LABEL_LEN);
    }

    #[test]
    fn label_over_max_len_rejected_at_compose() {
        let (signing, account_id, x25519_pub) = fixture();
        let label = "x".repeat(MAX_LABEL_LEN + 1);
        let err = compose(&signing, &account_id, &x25519_pub, 0, 0, false, &label).unwrap_err();
        assert!(matches!(err, ContactError::InvalidArgument(_)));
    }

    #[test]
    fn tampered_signature_rejected() {
        let (signing, account_id, x25519_pub) = fixture();
        let mut wire = compose(&signing, &account_id, &x25519_pub, 0, 0, false, "").unwrap();
        // Flip a bit in the trailing 64-byte signature.
        let len = wire.len();
        wire[len - 1] ^= 0x01;
        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::BadSignature));
    }

    #[test]
    fn tampered_label_rejected() {
        let (signing, account_id, x25519_pub) = fixture();
        let mut wire = compose(&signing, &account_id, &x25519_pub, 0, 0, false, "alice").unwrap();
        // Find the label byte 'a' (after the fixed-position prefix) and flip
        // its high bit. The signature was over the original label, so this
        // must surface as BadSignature, not as a UTF-8 error.
        let label_offset = 1
            + ED25519_PUB_LEN
            + ACCOUNT_ID_LEN
            + X25519_PUB_LEN
            + 8 // expiry_ms
            + 4 // max_uses
            + 1 // bootstrap_referral
            + 2; // label_len
        wire[label_offset] = b'b';
        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::BadSignature));
    }

    #[test]
    fn truncated_wire_rejected() {
        let (signing, account_id, x25519_pub) = fixture();
        let wire = compose(&signing, &account_id, &x25519_pub, 0, 0, false, "").unwrap();
        // Strip the trailing 64-byte signature.
        let truncated = &wire[..wire.len() - 1];
        let err = decode_and_verify(truncated).unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    #[test]
    fn empty_wire_rejected() {
        let err = decode_and_verify(&[]).unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    #[test]
    fn unknown_schema_version_rejected() {
        let mut wire = vec![0xFFu8]; // schema version 255
        wire.extend_from_slice(&[0u8; 200]); // some bytes after
        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    #[test]
    fn invalid_bootstrap_referral_byte_rejected() {
        let (signing, account_id, x25519_pub) = fixture();
        let mut wire = compose(&signing, &account_id, &x25519_pub, 0, 0, false, "").unwrap();
        // bootstrap_referral byte sits at:
        //   1 + 32 + 16 + 32 + 8 + 4 = 93
        wire[93] = 0x42;
        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    #[test]
    fn trailing_bytes_after_signature_rejected() {
        let (signing, account_id, x25519_pub) = fixture();
        let mut wire = compose(&signing, &account_id, &x25519_pub, 0, 0, false, "").unwrap();
        wire.push(0xDE);
        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }

    #[test]
    fn wire_starts_with_schema_version_byte() {
        let (signing, account_id, x25519_pub) = fixture();
        let wire = compose(&signing, &account_id, &x25519_pub, 0, 0, false, "").unwrap();
        assert_eq!(wire[0], SCHEMA_VERSION);
    }

    #[test]
    fn signing_with_a_different_key_does_not_verify() {
        let (signing_real, account_id, x25519_pub) = fixture();
        let signing_attacker = SigningKey::generate();
        // Hand-build a wire with the real owner's pubkey but the
        // attacker's signature — should fail signature check.
        let mut wire = encode_signing_prefix(
            &signing_real.verifying_key(),
            &account_id,
            &x25519_pub,
            0,
            0,
            false,
            "",
        );
        let bad_sig = signing_attacker.sign(&wire);
        wire.extend_from_slice(&bad_sig.to_bytes());

        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::BadSignature));
    }

    #[test]
    fn label_with_invalid_utf8_rejected() {
        let (signing, account_id, x25519_pub) = fixture();
        // Hand-build the wire by emitting raw bytes — `encode_signing_prefix`
        // takes `&str` so we cannot route invalid UTF-8 through it.
        // The signature is over THESE bytes (so signature verification
        // passes), and only the UTF-8 conversion step inside
        // `decode_and_verify` fails — which is what we want to assert.
        let label_bytes: [u8; 3] = [0xFF, 0xFE, 0xFD];
        let label_len_u16 = u16::try_from(label_bytes.len()).unwrap();

        let mut prefix = Vec::new();
        prefix.push(SCHEMA_VERSION);
        prefix.extend_from_slice(&signing.verifying_key().to_bytes());
        prefix.extend_from_slice(&account_id);
        prefix.extend_from_slice(&x25519_pub.to_bytes());
        prefix.extend_from_slice(&0u64.to_be_bytes()); // expiry_ms
        prefix.extend_from_slice(&0u32.to_be_bytes()); // max_uses
        prefix.push(0); // bootstrap_referral
        prefix.extend_from_slice(&label_len_u16.to_be_bytes());
        prefix.extend_from_slice(&label_bytes);

        let sig = signing.sign(&prefix);
        let mut wire = prefix;
        wire.extend_from_slice(&sig.to_bytes());

        let err = decode_and_verify(&wire).unwrap_err();
        assert!(matches!(err, ContactError::InvalidEncoding(_)));
    }
}
