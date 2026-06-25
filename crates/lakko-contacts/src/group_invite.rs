// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! [`GroupInviteCapability`] — link-based group join-ticket (ADR-0028, G-3).
//!
//! The inverse of the admin-adds-by-username flow: an owner/admin mints a
//! signed link that conveys a group's `group_id` to a **non-member**, who then
//! crafts an MLS external `Add` proposal
//! ([`lakko_mls::Identity::create_external_join_proposal`], ADR-0027) and posts
//! it to the group journal for the admin to approve.
//!
//! Sibling of [`crate::call_invite::CallInviteCapability`] (ADR-0009) and
//! [`crate::invite::InviteCapability`] (ADR-0005): the same single-file manual
//! codec + Ed25519-over-the-prefix signature, but a leaner payload — no
//! embedded KeyPackage, no media flags, no account id. The joiner mints its
//! **own** KeyPackage inside `create_external_join_proposal`, and learns the
//! group's current epoch from the journal's public MLS framing (so the ticket
//! carries no epoch — see ADR-0028 §3).
//!
//! ## What the link carries (and does NOT carry)
//!
//! - `group_id` — the target group handle the joiner passes to
//!   `create_external_join_proposal`. Variable-length opaque MLS `GroupId`
//!   bytes (Lakko's `Group::group_id()` is a `Vec<u8>`), length-prefixed here.
//! - `issuer_signature_pubkey` — the owner/admin Ed25519 key that minted the
//!   link. Anti-tamper (the signature binds the `group_id`) + lets the admin
//!   UI surface *which* member's link a pending request arrived on, and lets
//!   the joiner render an anti-phishing fingerprint.
//! - `expiry_ms` — UNIX millis; [`NEVER_EXPIRES`] (= 0) = no expiry.
//!
//! **No epoch** (read from the journal at request time) and **no
//! authorization secret** — the security boundary is the admin's approval gate
//! (ADR-0027 role-gated, peer-verified), not the ticket. A leaked link lets a
//! stranger *ask* to join; it never lets them *in*.
//!
//! ## Wire format (manual codec, frozen at v1)
//!
//! ```text
//! u8           schema version (= 1)
//! u16be        group_id_len                       — bounded by MAX_GROUP_ID_LEN
//! [gid_len]    group_id                            — opaque MLS GroupId bytes
//! [32 bytes]   issuer_signature_pubkey             — Ed25519 verifying key
//! u64be        expiry_ms                           — UNIX millis; 0 = no expiry
//! [64 bytes]   signature                           — Ed25519 over the above
//! ```
//!
//! Multi-byte integers are big-endian. The signature covers everything from
//! the schema-version byte through `expiry_ms` inclusive.

use lakko_crypto::signing::{
    Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN as ED25519_PUB_LEN, SIGNATURE_LEN,
};

use crate::error::{ContactError, Result};
use crate::invite::NEVER_EXPIRES;

/// Current schema version on the wire.
const SCHEMA_VERSION: u8 = 1;

/// Hard cap on the embedded `group_id` bytes. An MLS `GroupId` is opaque
/// variable-length; Lakko's are short (16–32 bytes in practice), but the codec
/// bounds at 256 to keep URL lengths sane and parse-time work bounded without
/// pinning an exact upstream upper bound.
pub const MAX_GROUP_ID_LEN: usize = 256;

/// Re-export so callers that only `use crate::group_invite::*` get a coherent
/// surface. `GROUP_INVITE_NEVER_EXPIRES` (= 0) means no expiry.
pub use crate::invite::NEVER_EXPIRES as GROUP_INVITE_NEVER_EXPIRES;

// ============================================================================
// GroupInviteCapability — wire shape
// ============================================================================

/// Capability granted by a group owner/admin to a prospective joiner. Decoded
/// and verified from the base64url-encoded slug at the end of the
/// `https://lakko.me/g/<slug>` URL (ADR-0028 §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupInviteCapability {
    /// The target group's opaque MLS `GroupId` bytes — what the joiner passes
    /// to `Identity::create_external_join_proposal`.
    pub group_id: Vec<u8>,
    /// The issuer's long-term Ed25519 verifying key. The signature over the
    /// canonical bytes is verified against this; the admin UI can additionally
    /// check it is a current group member before approving (a forged link
    /// names a non-member issuer).
    pub issuer_signature_pubkey: VerifyingKey,
    /// UNIX milliseconds when the link stops being valid. [`NEVER_EXPIRES`]
    /// (= 0) means no expiry — UI SHOULD surface a non-expiring link as a
    /// standing-exposure note.
    pub expiry_ms: u64,
}

impl GroupInviteCapability {
    /// `true` iff the link has not expired at `now_ms`. A [`NEVER_EXPIRES`]
    /// link always returns `true`.
    #[must_use]
    pub fn is_valid_now(&self, now_ms: u64) -> bool {
        self.expiry_ms == NEVER_EXPIRES || now_ms < self.expiry_ms
    }

    /// Signal-style safety number derived from the issuer's Ed25519 key — the
    /// same anti-phishing fingerprint format as
    /// [`crate::call_invite::CallInviteCapability::owner_fingerprint`]. The
    /// joiner UI shows it so the human can compare it against what the inviter
    /// shared out-of-band before clicking "request to join".
    #[must_use]
    pub fn issuer_fingerprint(&self) -> String {
        crate::safety::safety_number(&self.issuer_signature_pubkey, &self.issuer_signature_pubkey)
    }
}

/// Compose + sign a group join-ticket capability.
///
/// # Errors
///
/// - [`ContactError::InvalidArgument`] when `group_id` is empty or longer than
///   [`MAX_GROUP_ID_LEN`].
pub fn compose(issuer_signing: &SigningKey, group_id: &[u8], expiry_ms: u64) -> Result<Vec<u8>> {
    if group_id.is_empty() {
        return Err(ContactError::InvalidArgument(
            "group-invite group_id must be non-empty".into(),
        ));
    }
    if group_id.len() > MAX_GROUP_ID_LEN {
        return Err(ContactError::InvalidArgument(format!(
            "group-invite group_id is {} bytes, max is {MAX_GROUP_ID_LEN}",
            group_id.len(),
        )));
    }
    let issuer_pubkey = issuer_signing.verifying_key();
    let mut wire = encode_signing_prefix(&issuer_pubkey, group_id, expiry_ms);
    let signature = issuer_signing.sign(&wire);
    wire.extend_from_slice(&signature.to_bytes());
    Ok(wire)
}

/// Decode + verify a group join-ticket capability.
///
/// Does **not** check `expiry_ms` — call [`GroupInviteCapability::is_valid_now`]
/// separately so the UI can render "this link expired" distinctly from a
/// malformed/forged link.
///
/// # Errors
///
/// - [`ContactError::InvalidEncoding`] — wire format malformed, unsupported
///   schema version, EOF, `group_id` length beyond [`MAX_GROUP_ID_LEN`] or
///   zero, trailing bytes, or signature verification failed.
pub fn decode_and_verify(wire: &[u8]) -> Result<GroupInviteCapability> {
    let mut cur = wire;
    let version = take_u8(&mut cur)?;
    if version != SCHEMA_VERSION {
        return Err(ContactError::InvalidEncoding(format!(
            "unsupported GroupInviteCapability schema version {version}",
        )));
    }
    let gid_len_bytes = take_n(&mut cur, 2)?;
    let gid_len = usize::from(u16::from_be_bytes([gid_len_bytes[0], gid_len_bytes[1]]));
    if gid_len == 0 {
        return Err(ContactError::InvalidEncoding(
            "group-invite group_id_len is zero".into(),
        ));
    }
    if gid_len > MAX_GROUP_ID_LEN {
        return Err(ContactError::InvalidEncoding(format!(
            "group-invite group_id_len {gid_len} > MAX_GROUP_ID_LEN {MAX_GROUP_ID_LEN}"
        )));
    }
    let group_id = take_n(&mut cur, gid_len)?.to_vec();
    let issuer_pubkey_bytes = take_n(&mut cur, ED25519_PUB_LEN)?;
    let expiry_ms_bytes = take_n(&mut cur, 8)?;
    let signature_bytes = take_n(&mut cur, SIGNATURE_LEN)?;
    if !cur.is_empty() {
        return Err(ContactError::InvalidEncoding(format!(
            "trailing {} bytes after group-invite signature",
            cur.len(),
        )));
    }

    // Verify the Ed25519 signature over everything before the signature.
    let signing_input = &wire[..wire.len() - SIGNATURE_LEN];
    let issuer_pubkey = VerifyingKey::from_bytes(issuer_pubkey_bytes).map_err(|_| {
        ContactError::InvalidEncoding("group-invite issuer_signature_pubkey malformed".into())
    })?;
    let sig = Signature::from_bytes(signature_bytes)
        .map_err(|_| ContactError::InvalidEncoding("group-invite signature length wrong".into()))?;
    issuer_pubkey
        .verify(signing_input, &sig)
        .map_err(|_| ContactError::InvalidEncoding("group-invite signature invalid".into()))?;

    let expiry_ms = u64::from_be_bytes([
        expiry_ms_bytes[0],
        expiry_ms_bytes[1],
        expiry_ms_bytes[2],
        expiry_ms_bytes[3],
        expiry_ms_bytes[4],
        expiry_ms_bytes[5],
        expiry_ms_bytes[6],
        expiry_ms_bytes[7],
    ]);

    Ok(GroupInviteCapability {
        group_id,
        issuer_signature_pubkey: issuer_pubkey,
        expiry_ms,
    })
}

// ============================================================================
// Internal codec helpers
// ============================================================================

fn encode_signing_prefix(
    issuer_signature_pubkey: &VerifyingKey,
    group_id: &[u8],
    expiry_ms: u64,
) -> Vec<u8> {
    let gid_len_u16 = u16::try_from(group_id.len())
        .expect("group_id fits in u16; caller checked MAX_GROUP_ID_LEN");
    let mut out = Vec::with_capacity(1 + 2 + group_id.len() + ED25519_PUB_LEN + 8);
    out.push(SCHEMA_VERSION);
    out.extend_from_slice(&gid_len_u16.to_be_bytes());
    out.extend_from_slice(group_id);
    out.extend_from_slice(&issuer_signature_pubkey.to_bytes());
    out.extend_from_slice(&expiry_ms.to_be_bytes());
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

    fn fixture_signing() -> SigningKey {
        SigningKey::from_seed(&[0x5A; 32])
    }

    fn fixture_group_id() -> Vec<u8> {
        let mut gid = vec![0x11; 32];
        gid[0] = 0xCA;
        gid[31] = 0xFE;
        gid
    }

    #[test]
    fn compose_decode_round_trips() {
        let signing = fixture_signing();
        let gid = fixture_group_id();
        let wire = compose(&signing, &gid, 1_700_000_000_000).unwrap();
        let parsed = decode_and_verify(&wire).unwrap();
        assert_eq!(parsed.group_id, gid);
        assert_eq!(
            parsed.issuer_signature_pubkey.to_bytes(),
            signing.verifying_key().to_bytes()
        );
        assert_eq!(parsed.expiry_ms, 1_700_000_000_000);
    }

    #[test]
    fn variable_length_group_id_round_trips() {
        for len in [1usize, 16, 32, MAX_GROUP_ID_LEN] {
            let gid = vec![0x7Bu8; len];
            let wire = compose(&fixture_signing(), &gid, 0).unwrap();
            let parsed = decode_and_verify(&wire).unwrap();
            assert_eq!(parsed.group_id, gid, "len {len}");
        }
    }

    #[test]
    fn never_expires_is_valid_at_any_time() {
        let wire = compose(
            &fixture_signing(),
            &fixture_group_id(),
            GROUP_INVITE_NEVER_EXPIRES,
        )
        .unwrap();
        let parsed = decode_and_verify(&wire).unwrap();
        assert!(parsed.is_valid_now(0));
        assert!(parsed.is_valid_now(u64::MAX));
    }

    #[test]
    fn expired_link_is_invalid_after_expiry() {
        let wire = compose(&fixture_signing(), &fixture_group_id(), 1_000).unwrap();
        let parsed = decode_and_verify(&wire).unwrap();
        assert!(parsed.is_valid_now(999));
        assert!(!parsed.is_valid_now(1_000));
        assert!(!parsed.is_valid_now(2_000));
    }

    #[test]
    fn tampered_group_id_rejected() {
        let mut wire = compose(&fixture_signing(), &fixture_group_id(), 0).unwrap();
        // Flip a byte inside the group_id region (offset 3 = first gid byte).
        wire[3] ^= 0xFF;
        match decode_and_verify(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => assert!(msg.contains("signature"), "{msg}"),
            other => panic!("expected signature-invalid, got {other:?}"),
        }
    }

    #[test]
    fn cross_signing_key_rejected() {
        let signing_a = SigningKey::from_seed(&[0xAA; 32]);
        let signing_b = SigningKey::from_seed(&[0xBB; 32]);
        let mut wire = compose(&signing_a, &fixture_group_id(), 0).unwrap();
        // Splice B's pubkey over A's (it sits right after version + len + gid).
        let off = 1 + 2 + fixture_group_id().len();
        wire[off..off + ED25519_PUB_LEN].copy_from_slice(&signing_b.verifying_key().to_bytes());
        match decode_and_verify(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => assert!(msg.contains("signature"), "{msg}"),
            other => panic!("expected signature-invalid, got {other:?}"),
        }
    }

    #[test]
    fn truncated_wire_rejected() {
        let wire = compose(&fixture_signing(), &fixture_group_id(), 0).unwrap();
        assert!(decode_and_verify(&wire[..wire.len() - SIGNATURE_LEN - 1]).is_err());
    }

    #[test]
    fn trailing_bytes_rejected() {
        let mut wire = compose(&fixture_signing(), &fixture_group_id(), 0).unwrap();
        wire.push(0x00);
        match decode_and_verify(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => assert!(msg.contains("trailing"), "{msg}"),
            other => panic!("expected trailing-bytes, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_schema_version_rejected() {
        let mut wire = compose(&fixture_signing(), &fixture_group_id(), 0).unwrap();
        wire[0] = 2;
        match decode_and_verify(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => {
                assert!(msg.contains("schema version 2"), "{msg}");
            },
            other => panic!("expected unsupported-version, got {other:?}"),
        }
    }

    #[test]
    fn empty_group_id_rejected_at_compose() {
        let err = compose(&fixture_signing(), &[], 0).unwrap_err();
        assert!(matches!(err, ContactError::InvalidArgument(_)));
    }

    #[test]
    fn oversize_group_id_rejected_at_compose() {
        let too_big = vec![0u8; MAX_GROUP_ID_LEN + 1];
        let err = compose(&fixture_signing(), &too_big, 0).unwrap_err();
        assert!(matches!(err, ContactError::InvalidArgument(_)));
    }

    #[test]
    fn zero_group_id_len_rejected_at_decode() {
        // Hand-craft a wire with group_id_len = 0.
        let mut wire = vec![SCHEMA_VERSION, 0x00, 0x00];
        wire.extend_from_slice(&[0u8; ED25519_PUB_LEN]);
        wire.extend_from_slice(&0u64.to_be_bytes());
        wire.extend_from_slice(&[0u8; SIGNATURE_LEN]);
        match decode_and_verify(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => {
                assert!(msg.contains("group_id_len"), "{msg}")
            },
            other => panic!("expected zero-len reject, got {other:?}"),
        }
    }

    #[test]
    fn issuer_fingerprint_is_deterministic_per_pubkey() {
        let cap_a =
            decode_and_verify(&compose(&fixture_signing(), &fixture_group_id(), 1_000).unwrap())
                .unwrap();
        let mut other_gid = fixture_group_id();
        other_gid[0] = 0x99;
        let cap_b =
            decode_and_verify(&compose(&fixture_signing(), &other_gid, 999_999).unwrap()).unwrap();
        assert_eq!(
            cap_a.issuer_fingerprint(),
            cap_b.issuer_fingerprint(),
            "fingerprint must depend ONLY on the issuer pubkey"
        );
    }

    #[test]
    fn different_issuers_have_different_fingerprints() {
        let cap_a = decode_and_verify(
            &compose(&SigningKey::from_seed(&[0xAA; 32]), &fixture_group_id(), 0).unwrap(),
        )
        .unwrap();
        let cap_b = decode_and_verify(
            &compose(&SigningKey::from_seed(&[0xBB; 32]), &fixture_group_id(), 0).unwrap(),
        )
        .unwrap();
        assert_ne!(cap_a.issuer_fingerprint(), cap_b.issuer_fingerprint());
    }
}
