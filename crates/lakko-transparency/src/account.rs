// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Transparency-log account-key derivation — the single, shared definition
//! ([ADR-0036] drift-hazard note).
//!
//! The KT-log leaf's `username_hash` field carries
//!
//! ```text
//! BLAKE3("lakko/transparency/account/v1" || account_id)
//! ```
//!
//! — a 32-byte value derivable by the discovery service (on append), the auth
//! service (when it mints a genesis attestation), and a client (on verify),
//! because `account_id` is already public. The transparency service treats the
//! result as 32 opaque bytes and never learns the `account_id`-to-username
//! mapping, so the log leaks nothing the public `account_id` did not already.
//!
//! ## Why this lives in one place
//!
//! Before [ADR-0036] the derivation lived only in
//! `services/discovery/src/transparency_log.rs`. ADR-0036 adds a SECOND
//! producer: `services/auth` derives the same `username_hash` when it signs a
//! [`GenesisAttestation`](../../lakko_attest/genesis/struct.GenesisAttestation.html),
//! and the transparency append gate binds the attestation to the leaf by
//! comparing the two. If auth computed `username_hash` with even a one-byte
//! difference in the domain or the construction, every genesis attestation
//! would fail the gate silently — a fail-closed, hard-to-notice outage of the
//! hardening. Promoting the constant + the derivation here (the crate that
//! already owns the leaf / genesis / supersession primitives) makes drift
//! structurally impossible: there is ONE definition, pinned by
//! [`tests::golden_pins_the_account_username_hash`].
//!
//! [ADR-0036]: ../../../docs/adr/0036-auth-attested-genesis.md

/// Domain-separation string for the account-key derivation. Every producer
/// (discovery append, auth genesis-attestation mint) and every consumer
/// (client verify) MUST use this exact string so the derived `username_hash`
/// matches. Changing it is a wire break — see
/// [`tests::golden_pins_the_account_username_hash`], the drift pin.
pub const ACCOUNT_KEY_DOMAIN: &str = "lakko/transparency/account/v1";

/// Derive the transparency-log `username_hash` for an account:
/// `BLAKE3("lakko/transparency/account/v1" || account_id)`.
///
/// The domain string is PREPENDED to the raw `account_id` bytes and the
/// concatenation is hashed with plain (un-keyed) BLAKE3 — `account_id` is
/// public, so the result reveals nothing new; the domain separator only stops
/// this hash from colliding with any other BLAKE3 use of the same
/// `account_id`.
///
/// The parameter is a byte slice (not a fixed `[u8; 16]`) because the two
/// producers reach it from different shapes — discovery holds `account_id` as a
/// `Vec<u8>` read from its DB, auth holds it as a `[u8; 16]` from the session
/// token — and a slice accepts both without a fallible length conversion. In
/// every live call site the slice is the 16-byte account id.
#[must_use]
pub fn account_username_hash(account_id: &[u8]) -> [u8; 32] {
    let mut input = Vec::with_capacity(ACCOUNT_KEY_DOMAIN.len() + account_id.len());
    input.extend_from_slice(ACCOUNT_KEY_DOMAIN.as_bytes());
    input.extend_from_slice(account_id);
    lakko_crypto::hash::blake3(&input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_domain_separated_and_deterministic() {
        let id = [0xABu8; 16];
        let k1 = account_username_hash(&id);
        let k2 = account_username_hash(&id);
        // Deterministic.
        assert_eq!(k1, k2);

        // Domain-separated: hashing the bare account_id (no domain prefix) must
        // NOT collide with the derived key.
        let bare = lakko_crypto::hash::blake3(&id);
        assert_ne!(k1, bare);

        // Sanity: matches an explicit "domain || id" hash.
        let mut expected_input = Vec::new();
        expected_input.extend_from_slice(b"lakko/transparency/account/v1");
        expected_input.extend_from_slice(&id);
        assert_eq!(k1, lakko_crypto::hash::blake3(&expected_input));
    }

    #[test]
    fn distinct_accounts_derive_distinct_keys() {
        assert_ne!(
            account_username_hash(&[0u8; 16]),
            account_username_hash(&[1u8; 16])
        );
    }

    /// Drift pin ([ADR-0036] drift hazard). A fixed `account_id` must always
    /// derive this exact `username_hash`. Auth (genesis-attestation mint),
    /// discovery (log append), and the clients (verify) all derive it the same
    /// way; if this golden ever changes, every auth-attested genesis silently
    /// fails the transparency gate (`attestation.username_hash != leaf`), so
    /// this test is the guardrail that makes such a change fail CI instead.
    #[test]
    fn golden_pins_the_account_username_hash() {
        const GOLDEN_HEX: &str = "a5c9754b6ee1c04e65d76b50f1e68a61951ccb250d482c6016fdd5fa78b89229";
        // A fixed, recognizable account_id: bytes 0x00..0x0f.
        let account_id: [u8; 16] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        // Also pin the domain string itself — a change here is a wire break.
        assert_eq!(ACCOUNT_KEY_DOMAIN, "lakko/transparency/account/v1");

        let got = account_username_hash(&account_id);
        assert_eq!(
            hex::encode(got),
            GOLDEN_HEX,
            "account_username_hash drift: the derivation changed — every \
             auth-attested genesis would fail the transparency gate. If this \
             change is intentional, update the golden AND every producer/consumer."
        );
    }
}
