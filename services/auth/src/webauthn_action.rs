// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! WebAuthn action binding — challenge derivation (G-13, ADR-0014).
//!
//! A normal passkey assertion proves "the user has the device" but does
//! not bind to *which* action it authorises. To upgrade that to a
//! cryptographic per-action attestation, the action-bound auth flow issues
//! a WebAuthn challenge that is a deterministic function of the action:
//!
//! ```text
//! challenge = BLAKE3_keyed( derive_key(DOMAIN), action_hash || server_nonce )
//! ```
//!
//! The authenticator signs over `clientDataJSON` (which echoes the
//! challenge), so its signature attests to `action_hash`. The server-side
//! `AuthenticationState` (built by the forked
//! `webauthn-rs-core::generate_challenge_authenticate_with_challenge`)
//! carries this same challenge, so verification fails unless the
//! authenticator signed *this* action's challenge — an assertion produced
//! for action A cannot be replayed onto action B.
//!
//! - `action_hash` is a 32-byte opaque action identifier supplied by the
//!   caller (e.g. `BLAKE3(opaque_message_bytes)` for an OPAQUE-authorised
//!   request). This module treats it as opaque bytes.
//! - `server_nonce` is a fresh 32-byte CSPRNG value per challenge — it
//!   keeps the challenge unpredictable (a bare `H(action_hash)` would be
//!   guessable for a known action) and single-use, mirroring the random
//!   challenge the stock flow uses.
//!
//! The challenge-derivation here is the Lakko-owned crypto; the surrounding
//! WebAuthn assertion *verification* stays in upstream `webauthn-rs-core`
//! (ADR-0014). The `start_action_auth` / `finish_action_auth` wiring + the
//! `/auth/action/{start,finish}` endpoints + `webauthn_states.action_hash`
//! migration are the next ADR-0014 steps; this module lands the derivation
//! primitive + its tests first.

// Consumed by the `/auth/action/{start,finish}` handlers in a follow-up
// ADR-0014 step; the derivation primitive lands (and is tested) first.
#![allow(dead_code)]

use rand::rngs::OsRng;
use rand::RngCore;

/// Domain separator bound into the action-challenge derivation. Bumping
/// this string invalidates every previously-issued action challenge.
pub(crate) const ACTION_CHALLENGE_DOMAIN: &str = "lakko/webauthn-action/v1";

/// Length of the opaque caller-supplied action hash.
pub(crate) const ACTION_HASH_LEN: usize = 32;

/// Length of the per-challenge server nonce.
pub(crate) const ACTION_NONCE_LEN: usize = 32;

/// Generate a fresh 32-byte server nonce for an action-bound challenge.
pub(crate) fn random_action_nonce() -> [u8; ACTION_NONCE_LEN] {
    let mut nonce = [0u8; ACTION_NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    nonce
}

/// Derive the action-bound WebAuthn challenge from a 32-byte action hash
/// and a 32-byte server nonce (ADR-0014).
///
/// `BLAKE3_keyed(derive_key(DOMAIN), action_hash || nonce)` — the
/// domain-derived key gives RFC-style domain separation, and keying the
/// hash (rather than a plain hash) keeps the construction in line with the
/// rest of Lakko's BLAKE3 usage.
pub(crate) fn derive_action_challenge(
    action_hash: &[u8; ACTION_HASH_LEN],
    nonce: &[u8; ACTION_NONCE_LEN],
) -> [u8; 32] {
    let key = lakko_crypto::hash::blake3_derive_key(ACTION_CHALLENGE_DOMAIN);
    let mut input = [0u8; ACTION_HASH_LEN + ACTION_NONCE_LEN];
    input[..ACTION_HASH_LEN].copy_from_slice(action_hash);
    input[ACTION_HASH_LEN..].copy_from_slice(nonce);
    lakko_crypto::hash::blake3_keyed(&key, &input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivation_is_deterministic() {
        let action = [0x11u8; ACTION_HASH_LEN];
        let nonce = [0x22u8; ACTION_NONCE_LEN];
        assert_eq!(
            derive_action_challenge(&action, &nonce),
            derive_action_challenge(&action, &nonce),
            "same (action, nonce) must yield the same challenge"
        );
    }

    #[test]
    fn distinct_action_yields_distinct_challenge() {
        let nonce = [0x22u8; ACTION_NONCE_LEN];
        let a = derive_action_challenge(&[0x11u8; ACTION_HASH_LEN], &nonce);
        let b = derive_action_challenge(&[0x12u8; ACTION_HASH_LEN], &nonce);
        assert_ne!(a, b, "different action hashes must bind to different challenges");
    }

    #[test]
    fn distinct_nonce_yields_distinct_challenge() {
        let action = [0x11u8; ACTION_HASH_LEN];
        let a = derive_action_challenge(&action, &[0x22u8; ACTION_NONCE_LEN]);
        let b = derive_action_challenge(&action, &[0x23u8; ACTION_NONCE_LEN]);
        assert_ne!(a, b, "different nonces must yield different challenges");
    }

    #[test]
    fn challenge_is_32_bytes() {
        let c = derive_action_challenge(&[0u8; ACTION_HASH_LEN], &[0u8; ACTION_NONCE_LEN]);
        assert_eq!(c.len(), 32);
    }

    #[test]
    fn nonce_is_not_constant() {
        // Two fresh nonces must differ (CSPRNG, not a fixed value). A
        // collision here is astronomically unlikely; a constant nonce
        // would make the challenge guessable for a known action.
        assert_ne!(random_action_nonce(), random_action_nonce());
    }

    #[test]
    fn domain_separation_holds() {
        // Re-derive with the real domain vs a different one; the keyed
        // construction must produce different challenges so a challenge
        // minted for the action domain can't collide with another
        // BLAKE3-keyed use of the same (action || nonce) bytes.
        let action = [0x11u8; ACTION_HASH_LEN];
        let nonce = [0x22u8; ACTION_NONCE_LEN];
        let real = derive_action_challenge(&action, &nonce);

        let other_key = lakko_crypto::hash::blake3_derive_key("lakko/other-domain/v1");
        let mut input = [0u8; ACTION_HASH_LEN + ACTION_NONCE_LEN];
        input[..ACTION_HASH_LEN].copy_from_slice(&action);
        input[ACTION_HASH_LEN..].copy_from_slice(&nonce);
        let other = lakko_crypto::hash::blake3_keyed(&other_key, &input);

        assert_ne!(real, other, "domain separation must change the challenge");
    }
}
