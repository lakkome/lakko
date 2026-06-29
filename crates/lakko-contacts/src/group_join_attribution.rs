// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Group-join attribution envelope — ADR-0028 OQ2 (G-3).
//!
//! When a non-member requests to join a group via a `/g/` link
//! ([`crate::group_invite::GroupInviteCapability`]), the admin who approves the
//! request (ADR-0027) sees only the joiner's MLS leaf signature key — NOT
//! *which member's* link the request arrived on. The issuer pubkey lives in the
//! capability, which only the joiner holds.
//!
//! This envelope carries it: the joiner posts `(joiner_signature_key,
//! capability)` to the group's DS journal alongside its external-join proposal.
//! The admin's poll loop matches the envelope to a pending request by the
//! joiner signature key, re-verifies the embedded capability (Ed25519 — a
//! joiner cannot fabricate an issuer who did not sign), and surfaces the issuer
//! fingerprint + whether that issuer is currently in the group's roster
//! (`issuer_pubkey ∈ roster`, ADR-0028 OQ2).
//!
//! The attribution is **advisory** metadata for the human admin — the security
//! boundary stays the role-gated approval gate (ADR-0027). A spoofed
//! attribution (a wrong joiner key, or a replayed third-party capability) can
//! only mis-display issuer info; it grants nothing. A joiner self-signature
//! (binding the attribution to the proposing leaf) is a noted future hardening.
//!
//! ## Wire format (manual codec, frozen at v1, all big-endian)
//!
//! ```text
//! u8     schema version (= 1)
//! u16be  joiner_sig_key_len                — bounded by MAX_SIG_KEY_LEN
//! [..]   joiner_signature_key
//! u16be  cap_len                           — bounded by MAX_CAPABILITY_LEN
//! [..]   capability                        — a GroupInviteCapability wire blob
//! ```
//!
//! The DS stores this opaque (invariant #2). The decoder is attacker-touchable
//! (a malicious poster / the zero-trust DS can craft arbitrary journal entries),
//! so it is bounded + fail-closed + panic-free, and the embedded capability is
//! re-verified before any field is surfaced.

use crate::error::{ContactError, Result};
use crate::group_invite::{decode_and_verify as decode_group_invite, GroupInviteCapability};

/// Envelope wire version.
const VERSION: u8 = 1;

/// Upper bound on the joiner signature key (Ed25519 is 32; the bound is
/// generous but rejects an absurd length a hostile entry could claim).
const MAX_SIG_KEY_LEN: usize = 256;

/// Upper bound on the embedded capability. A [`GroupInviteCapability`] tops out
/// around 363 bytes (a 256-byte `group_id`); 512 matches the discovery ledger
/// column width and leaves headroom.
const MAX_CAPABILITY_LEN: usize = 512;

/// A group-join attribution decoded from a journal envelope (ADR-0028 OQ2).
#[derive(Debug, Clone)]
pub struct GroupJoinAttribution {
    /// The joiner's MLS leaf signature key — the rendezvous tag the admin
    /// matches against the pending external-join request.
    pub joiner_signature_key: Vec<u8>,
    /// The verified capability the joiner used. Its `issuer_signature_pubkey`
    /// is the member who minted the link; `group_id` is the target the admin
    /// checks against its own group before trusting the attribution.
    pub capability: GroupInviteCapability,
}

/// Compose the attribution envelope.
///
/// The `capability_bytes` are NOT re-verified here — the joiner composes them
/// from a link it already decoded + verified; [`decode_attribution`]
/// re-verifies on the admin side.
///
/// # Errors
///
/// [`ContactError::InvalidArgument`] when `joiner_signature_key` or
/// `capability_bytes` is empty or exceeds its bound.
pub fn compose_attribution(
    joiner_signature_key: &[u8],
    capability_bytes: &[u8],
) -> Result<Vec<u8>> {
    if joiner_signature_key.is_empty() || joiner_signature_key.len() > MAX_SIG_KEY_LEN {
        return Err(ContactError::InvalidArgument(
            "group-join attribution: joiner signature key length".into(),
        ));
    }
    if capability_bytes.is_empty() || capability_bytes.len() > MAX_CAPABILITY_LEN {
        return Err(ContactError::InvalidArgument(
            "group-join attribution: capability length".into(),
        ));
    }
    // Casts are safe: both lengths bounded above (<= 512).
    #[allow(clippy::cast_possible_truncation)]
    let sig_len = joiner_signature_key.len() as u16;
    #[allow(clippy::cast_possible_truncation)]
    let cap_len = capability_bytes.len() as u16;
    let mut out =
        Vec::with_capacity(1 + 2 + joiner_signature_key.len() + 2 + capability_bytes.len());
    out.push(VERSION);
    out.extend_from_slice(&sig_len.to_be_bytes());
    out.extend_from_slice(joiner_signature_key);
    out.extend_from_slice(&cap_len.to_be_bytes());
    out.extend_from_slice(capability_bytes);
    Ok(out)
}

/// Decode + verify the attribution envelope. Bounded, fail-closed, panic-free;
/// the embedded capability is re-verified via
/// [`crate::group_invite::decode_and_verify`].
///
/// # Errors
///
/// [`ContactError::InvalidEncoding`] on a malformed envelope (bad version, EOF,
/// out-of-bound length, trailing bytes) or when the embedded capability fails
/// signature verification.
pub fn decode_attribution(bytes: &[u8]) -> Result<GroupJoinAttribution> {
    let mut cur = bytes;
    let version = take_u8(&mut cur)?;
    if version != VERSION {
        return Err(ContactError::InvalidEncoding(format!(
            "group-join attribution: unsupported version {version}",
        )));
    }
    let sig_len = usize::from(take_u16(&mut cur)?);
    if sig_len == 0 || sig_len > MAX_SIG_KEY_LEN {
        return Err(ContactError::InvalidEncoding(
            "group-join attribution: joiner signature key length".into(),
        ));
    }
    let joiner_signature_key = take_n(&mut cur, sig_len)?.to_vec();
    let cap_len = usize::from(take_u16(&mut cur)?);
    if cap_len == 0 || cap_len > MAX_CAPABILITY_LEN {
        return Err(ContactError::InvalidEncoding(
            "group-join attribution: capability length".into(),
        ));
    }
    let capability_bytes = take_n(&mut cur, cap_len)?;
    if !cur.is_empty() {
        return Err(ContactError::InvalidEncoding(format!(
            "group-join attribution: {} trailing bytes",
            cur.len(),
        )));
    }
    let capability = decode_group_invite(capability_bytes)?;
    Ok(GroupJoinAttribution {
        joiner_signature_key,
        capability,
    })
}

// ============================================================================
// Internal codec helpers
// ============================================================================

fn take_u8(cur: &mut &[u8]) -> Result<u8> {
    if cur.is_empty() {
        return Err(ContactError::InvalidEncoding(
            "group-join attribution: unexpected EOF".into(),
        ));
    }
    let b = cur[0];
    *cur = &cur[1..];
    Ok(b)
}

fn take_u16(cur: &mut &[u8]) -> Result<u16> {
    let bytes = take_n(cur, 2)?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn take_n<'a>(cur: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
    if cur.len() < n {
        return Err(ContactError::InvalidEncoding(format!(
            "group-join attribution: need {n} bytes, have {}",
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
    use crate::group_invite::compose as compose_group_invite;
    use lakko_crypto::signing::SigningKey;

    fn fixture_capability() -> (Vec<u8>, SigningKey) {
        let issuer = SigningKey::from_seed(&[0x42; 32]);
        let group_id = vec![0xABu8; 32];
        let wire = compose_group_invite(&issuer, &group_id, 0).unwrap();
        (wire, issuer)
    }

    fn fixture_joiner_key() -> Vec<u8> {
        let mut k = vec![0x11u8; 32];
        k[0] = 0xC0;
        k[31] = 0xDE;
        k
    }

    #[test]
    fn round_trips() {
        let (cap, issuer) = fixture_capability();
        let joiner = fixture_joiner_key();
        let wire = compose_attribution(&joiner, &cap).unwrap();
        let parsed = decode_attribution(&wire).unwrap();
        assert_eq!(parsed.joiner_signature_key, joiner);
        assert_eq!(
            parsed.capability.issuer_signature_pubkey.to_bytes(),
            issuer.verifying_key().to_bytes(),
        );
        assert_eq!(parsed.capability.group_id, vec![0xABu8; 32]);
    }

    #[test]
    fn variable_length_joiner_key_round_trips() {
        let (cap, _) = fixture_capability();
        for len in [1usize, 16, 32, MAX_SIG_KEY_LEN] {
            let joiner = vec![0x7Bu8; len];
            let wire = compose_attribution(&joiner, &cap).unwrap();
            let parsed = decode_attribution(&wire).unwrap();
            assert_eq!(parsed.joiner_signature_key, joiner, "len {len}");
        }
    }

    #[test]
    fn tampered_capability_rejected() {
        let (cap, _) = fixture_capability();
        let joiner = fixture_joiner_key();
        let mut wire = compose_attribution(&joiner, &cap).unwrap();
        // Flip a byte inside the embedded capability region (past the header +
        // joiner key + cap_len = 1 + 2 + 32 + 2 = 37; offset 40 is inside the
        // capability's group_id bytes).
        wire[40] ^= 0xFF;
        match decode_attribution(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => {
                assert!(msg.contains("signature"), "{msg}");
            },
            other => panic!("expected capability signature reject, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_version_rejected() {
        let (cap, _) = fixture_capability();
        let mut wire = compose_attribution(&fixture_joiner_key(), &cap).unwrap();
        wire[0] = 2;
        match decode_attribution(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => assert!(msg.contains("version 2"), "{msg}"),
            other => panic!("expected unsupported-version, got {other:?}"),
        }
    }

    #[test]
    fn truncated_rejected() {
        let (cap, _) = fixture_capability();
        let wire = compose_attribution(&fixture_joiner_key(), &cap).unwrap();
        assert!(decode_attribution(&wire[..wire.len() - 1]).is_err());
        assert!(decode_attribution(&wire[..3]).is_err());
        assert!(decode_attribution(&[]).is_err());
    }

    #[test]
    fn trailing_bytes_rejected() {
        let (cap, _) = fixture_capability();
        let mut wire = compose_attribution(&fixture_joiner_key(), &cap).unwrap();
        wire.push(0x00);
        match decode_attribution(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => assert!(msg.contains("trailing"), "{msg}"),
            other => panic!("expected trailing-bytes, got {other:?}"),
        }
    }

    #[test]
    fn empty_inputs_rejected_at_compose() {
        let (cap, _) = fixture_capability();
        assert!(compose_attribution(&[], &cap).is_err());
        assert!(compose_attribution(&fixture_joiner_key(), &[]).is_err());
    }

    #[test]
    fn oversize_inputs_rejected_at_compose() {
        let (cap, _) = fixture_capability();
        let big_key = vec![0u8; MAX_SIG_KEY_LEN + 1];
        assert!(compose_attribution(&big_key, &cap).is_err());
        let big_cap = vec![0u8; MAX_CAPABILITY_LEN + 1];
        assert!(compose_attribution(&fixture_joiner_key(), &big_cap).is_err());
    }

    #[test]
    fn zero_length_fields_rejected_at_decode() {
        // joiner_sig_key_len = 0
        let wire = vec![VERSION, 0x00, 0x00];
        assert!(decode_attribution(&wire).is_err());
    }
}
