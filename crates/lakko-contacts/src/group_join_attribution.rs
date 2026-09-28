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
//! ## Joiner self-signature (anti-spoof)
//!
//! The envelope is **signed by the joiner** over its whole prefix. The
//! `joiner_signature_key` field is the joiner's MLS leaf Ed25519 verifying key
//! — which in Lakko is the identity's signature key, derived from the same seed
//! ([`lakko_mls::Identity`]), and is exactly the key the admin matches against
//! its pending external-join request (the proposal's KeyPackage leaf key). So
//! the attribution is **self-authenticating**: `decode_attribution` verifies
//! the signature against the embedded key, binding the attribution to the
//! actual proposing leaf. A stranger cannot post an attribution claiming
//! someone else's joiner key — they don't hold that key's private half.
//!
//! It remains **advisory** metadata for the human admin — the security boundary
//! stays the role-gated approval gate (ADR-0027). The self-signature closes the
//! prior spoofability (a wrong joiner key mis-displaying the source) but never
//! grants anything; a replayed third-party capability still only shows whose
//! valid link was used.
//!
//! ## Wire format (manual codec, frozen at v2, all big-endian)
//!
//! ```text
//! u8        schema version (= 2)
//! [32]      joiner_signature_key                 — Ed25519 leaf verifying key
//! u16be     cap_len                              — bounded by MAX_CAPABILITY_LEN
//! [cap_len] capability                           — a GroupInviteCapability blob
//! [64]      signature                            — Ed25519 by the joiner over
//!                                                   DOMAIN ‖ (everything above)
//! ```
//!
//! The DS stores this opaque (invariant #2). The decoder is attacker-touchable
//! (a malicious poster / the zero-trust DS can craft arbitrary journal entries),
//! so it is bounded + fail-closed + panic-free, the joiner signature is
//! verified, and the embedded capability is re-verified before any field is
//! surfaced.

use lakko_crypto::signing::{
    Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN as ED25519_PUB_LEN, SIGNATURE_LEN,
};

use crate::error::{ContactError, Result};
use crate::group_invite::{decode_and_verify as decode_group_invite, GroupInviteCapability};

/// Envelope wire version. Bumped to 2 when the joiner self-signature landed —
/// an unsigned v1 envelope is now refused (it carried no spoof resistance).
const VERSION: u8 = 2;

/// Domain separator for the joiner's self-signature, so a signature minted for
/// this purpose cannot be replayed as any other Lakko Ed25519 artefact.
const ATTRIBUTION_SIG_DOMAIN: &[u8] = b"lakko/group-join-attribution/v1";

/// Upper bound on the embedded capability: the longest wire the group-invite
/// codec can compose (908 bytes, a 256-byte `group_id` and a maximal sealed
/// preview). It was 512, sized for the v1 capability, so a joiner holding a
/// link whose preview named a long group or inviter could not post its
/// attribution; found and fixed in the 2026-09-28 security pass.
const MAX_CAPABILITY_LEN: usize = crate::group_invite::MAX_WIRE_LEN;

/// A group-join attribution decoded from a journal envelope (ADR-0028 OQ2).
#[derive(Debug, Clone)]
pub struct GroupJoinAttribution {
    /// The joiner's MLS leaf signature key (32-byte Ed25519). The envelope's
    /// self-signature is verified against this; it is the rendezvous tag the
    /// admin matches against the pending external-join request.
    pub joiner_signature_key: Vec<u8>,
    /// The verified capability the joiner used. Its `issuer_signature_pubkey`
    /// is the member who minted the link; `group_id` is the target the admin
    /// checks against its own group before trusting the attribution.
    pub capability: GroupInviteCapability,
}

/// Compose + sign the attribution envelope.
///
/// `joiner_signing_seed` is the joiner's 32-byte Ed25519 identity seed (the
/// same seed behind its MLS leaf signature key + the `joiner_signature_key` the
/// admin sees in the proposal). The `capability_bytes` are NOT re-verified here
/// — the joiner composes them from a link it already decoded + verified;
/// [`decode_attribution`] re-verifies on the admin side.
///
/// # Errors
///
/// [`ContactError::InvalidArgument`] when `joiner_signing_seed` is not 32 bytes
/// or `capability_bytes` is empty / exceeds its bound.
pub fn compose_attribution(joiner_signing_seed: &[u8], capability_bytes: &[u8]) -> Result<Vec<u8>> {
    if joiner_signing_seed.len() != ED25519_PUB_LEN {
        return Err(ContactError::InvalidArgument(format!(
            "group-join attribution: joiner signing seed must be {ED25519_PUB_LEN} bytes",
        )));
    }
    if capability_bytes.is_empty() || capability_bytes.len() > MAX_CAPABILITY_LEN {
        return Err(ContactError::InvalidArgument(
            "group-join attribution: capability length".into(),
        ));
    }
    let mut seed = [0u8; ED25519_PUB_LEN];
    seed.copy_from_slice(joiner_signing_seed);
    let signing = SigningKey::from_seed(&seed);
    let joiner_signature_key = signing.verifying_key().to_bytes();

    // Cast is safe: bounded by MAX_CAPABILITY_LEN (908) above.
    #[allow(clippy::cast_possible_truncation)]
    let cap_len = capability_bytes.len() as u16;
    let mut prefix =
        Vec::with_capacity(1 + ED25519_PUB_LEN + 2 + capability_bytes.len() + SIGNATURE_LEN);
    prefix.push(VERSION);
    prefix.extend_from_slice(&joiner_signature_key);
    prefix.extend_from_slice(&cap_len.to_be_bytes());
    prefix.extend_from_slice(capability_bytes);

    let signature = signing.sign(&signing_input(&prefix));
    let mut out = prefix;
    out.extend_from_slice(&signature.to_bytes());
    Ok(out)
}

/// Decode + verify the attribution envelope. Bounded, fail-closed, panic-free;
/// the joiner's self-signature is verified against the embedded
/// `joiner_signature_key`, and the embedded capability is re-verified via
/// [`crate::group_invite::decode_and_verify`].
///
/// # Errors
///
/// [`ContactError::InvalidEncoding`] on a malformed envelope (bad version, EOF,
/// out-of-bound length, trailing bytes), an invalid joiner self-signature, or a
/// capability whose own signature does not verify.
pub fn decode_attribution(bytes: &[u8]) -> Result<GroupJoinAttribution> {
    let mut cur = bytes;
    let version = take_u8(&mut cur)?;
    if version != VERSION {
        return Err(ContactError::InvalidEncoding(format!(
            "group-join attribution: unsupported version {version}",
        )));
    }
    let joiner_signature_key = take_n(&mut cur, ED25519_PUB_LEN)?.to_vec();
    let cap_len = usize::from(take_u16(&mut cur)?);
    if cap_len == 0 || cap_len > MAX_CAPABILITY_LEN {
        return Err(ContactError::InvalidEncoding(
            "group-join attribution: capability length".into(),
        ));
    }
    let capability_bytes = take_n(&mut cur, cap_len)?.to_vec();
    let signature_bytes = take_n(&mut cur, SIGNATURE_LEN)?;
    if !cur.is_empty() {
        return Err(ContactError::InvalidEncoding(format!(
            "group-join attribution: {} trailing bytes",
            cur.len(),
        )));
    }

    // Verify the joiner self-signature over everything before the signature.
    let prefix = &bytes[..bytes.len() - SIGNATURE_LEN];
    let joiner_key = VerifyingKey::from_bytes(&joiner_signature_key).map_err(|_| {
        ContactError::InvalidEncoding("group-join attribution: joiner key malformed".into())
    })?;
    let sig = Signature::from_bytes(signature_bytes).map_err(|_| {
        ContactError::InvalidEncoding("group-join attribution: signature length wrong".into())
    })?;
    joiner_key
        .verify(&signing_input(prefix), &sig)
        .map_err(|_| {
            ContactError::InvalidEncoding("group-join attribution: signature invalid".into())
        })?;

    let capability = decode_group_invite(&capability_bytes)?;
    Ok(GroupJoinAttribution {
        joiner_signature_key,
        capability,
    })
}

// ============================================================================
// Internal codec helpers
// ============================================================================

/// Domain-separated bytes the joiner signs / the admin verifies.
fn signing_input(prefix: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ATTRIBUTION_SIG_DOMAIN.len() + prefix.len());
    out.extend_from_slice(ATTRIBUTION_SIG_DOMAIN);
    out.extend_from_slice(prefix);
    out
}

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

    fn fixture_capability() -> (Vec<u8>, SigningKey) {
        let issuer = SigningKey::from_seed(&[0x42; 32]);
        let group_id = vec![0xABu8; 32];
        let wire = compose_group_invite(&issuer, &group_id, 0).unwrap();
        (wire, issuer)
    }

    /// The joiner's 32-byte identity signing seed.
    fn fixture_joiner_seed() -> [u8; 32] {
        [0xC0u8; 32]
    }

    fn fixture_joiner_pubkey() -> Vec<u8> {
        SigningKey::from_seed(&fixture_joiner_seed())
            .verifying_key()
            .to_bytes()
            .to_vec()
    }

    #[test]
    fn round_trips_and_self_signature_verifies() {
        let (cap, issuer) = fixture_capability();
        let wire = compose_attribution(&fixture_joiner_seed(), &cap).unwrap();
        let parsed = decode_attribution(&wire).unwrap();
        // The embedded joiner key is the seed's verifying key.
        assert_eq!(parsed.joiner_signature_key, fixture_joiner_pubkey());
        assert_eq!(
            parsed.capability.issuer_signature_pubkey.to_bytes(),
            issuer.verifying_key().to_bytes(),
        );
        assert_eq!(parsed.capability.group_id, vec![0xABu8; 32]);
    }

    #[test]
    fn tampered_capability_rejected() {
        let (cap, _) = fixture_capability();
        let mut wire = compose_attribution(&fixture_joiner_seed(), &cap).unwrap();
        // Flip a byte inside the embedded capability (past version(1) +
        // joiner_key(32) + cap_len(2) = 35; offset 40 lands in its group_id).
        // This breaks the joiner self-signature, which is checked first.
        wire[40] ^= 0xFF;
        match decode_attribution(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => {
                assert!(msg.contains("signature"), "{msg}");
            },
            other => panic!("expected signature reject, got {other:?}"),
        }
    }

    #[test]
    fn wrong_signer_rejected() {
        // Compose with seed A, then splice in seed B's verifying key. The
        // signature (made by A over A's prefix) no longer matches the embedded
        // key B — a stranger cannot post an attribution for a key they do not
        // hold the private half of.
        let (cap, _) = fixture_capability();
        let mut wire = compose_attribution(&fixture_joiner_seed(), &cap).unwrap();
        let other_pubkey = SigningKey::from_seed(&[0x0Bu8; 32])
            .verifying_key()
            .to_bytes();
        wire[1..=ED25519_PUB_LEN].copy_from_slice(&other_pubkey);
        match decode_attribution(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => assert!(msg.contains("signature"), "{msg}"),
            other => panic!("expected signature reject, got {other:?}"),
        }
    }

    #[test]
    fn tampered_signature_rejected() {
        let (cap, _) = fixture_capability();
        let mut wire = compose_attribution(&fixture_joiner_seed(), &cap).unwrap();
        let last = wire.len() - 1;
        wire[last] ^= 0xFF;
        match decode_attribution(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => assert!(msg.contains("signature"), "{msg}"),
            other => panic!("expected signature reject, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_version_rejected() {
        let (cap, _) = fixture_capability();
        let mut wire = compose_attribution(&fixture_joiner_seed(), &cap).unwrap();
        wire[0] = 3;
        match decode_attribution(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => assert!(msg.contains("version 3"), "{msg}"),
            other => panic!("expected unsupported-version, got {other:?}"),
        }
    }

    #[test]
    fn truncated_rejected() {
        let (cap, _) = fixture_capability();
        let wire = compose_attribution(&fixture_joiner_seed(), &cap).unwrap();
        assert!(decode_attribution(&wire[..wire.len() - 1]).is_err());
        assert!(decode_attribution(&wire[..3]).is_err());
        assert!(decode_attribution(&[]).is_err());
    }

    #[test]
    fn trailing_bytes_rejected() {
        let (cap, _) = fixture_capability();
        let mut wire = compose_attribution(&fixture_joiner_seed(), &cap).unwrap();
        wire.push(0x00);
        match decode_attribution(&wire) {
            Err(ContactError::InvalidEncoding(msg)) => assert!(msg.contains("trailing"), "{msg}"),
            other => panic!("expected trailing-bytes, got {other:?}"),
        }
    }

    #[test]
    fn wrong_seed_length_rejected_at_compose() {
        let (cap, _) = fixture_capability();
        assert!(compose_attribution(&[], &cap).is_err());
        assert!(compose_attribution(&[0u8; 31], &cap).is_err());
        assert!(compose_attribution(&[0u8; 33], &cap).is_err());
    }

    #[test]
    fn empty_or_oversize_capability_rejected_at_compose() {
        assert!(compose_attribution(&fixture_joiner_seed(), &[]).is_err());
        let big_cap = vec![0u8; MAX_CAPABILITY_LEN + 1];
        assert!(compose_attribution(&fixture_joiner_seed(), &big_cap).is_err());
    }

    #[test]
    fn short_wire_rejected_at_decode() {
        // version only — the joiner key parse fails.
        assert!(decode_attribution(&[VERSION]).is_err());
    }
}
