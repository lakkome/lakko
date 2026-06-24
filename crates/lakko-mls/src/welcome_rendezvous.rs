// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Welcome rendezvous envelope — ADR-0027 Phase 3 (G-3).
//!
//! When an owner/admin approves an external-join proposal
//! ([`crate::Group::approve_external_join`]) OpenMLS produces a Welcome for the
//! prospective joiner. The joiner is **not yet a member**, so the Welcome can't
//! ride the normal in-group delivery — it is HPKE-sealed to the joiner's
//! KeyPackage init key (not the group epoch), so it can travel through an opaque
//! relay without the relay learning group state (invariant #2).
//!
//! This envelope is the carrier: the admin wraps `(joiner_signature_key,
//! welcome_bytes)` and posts it to the group's DS journal; the joiner — who
//! knows the `group_id` because they sent their proposal there — polls the
//! journal, decodes each entry, and when an envelope's `joiner_signature_key`
//! matches its own, opens the inner Welcome via
//! [`crate::Group::join_from_welcome`]. The signature key is the rendezvous tag
//! because **both sides already know it** (it is the same handle the admin
//! passes to `approve_external_join`, and the joiner's own key); no extra
//! KeyPackage-ref derivation is needed.
//!
//! Wire format (all big-endian):
//!
//! ```text
//! version(1) ‖ sig_key_len(2) ‖ joiner_signature_key ‖ welcome_bytes
//! ```
//!
//! The DS stores this opaque; only the joiner can open the inner Welcome. The
//! decoder is the attacker-touchable surface (a malicious poster / server can
//! craft arbitrary journal entries), so it is bounded + fail-closed + panic-free.

use crate::error::{Error, Result};

/// Envelope wire version.
const VERSION: u8 = 1;

/// Header length: `version(1) + sig_key_len(2)`.
const HEADER_LEN: usize = 3;

/// Upper bound on the rendezvous-tag signature key (Ed25519 is 32; the bound is
/// generous but rejects an absurd length that a hostile entry could claim).
const MAX_SIG_KEY_LEN: usize = 256;

/// Upper bound on the wrapped Welcome (1 MiB — a Welcome with an embedded
/// ratchet tree for a large group is the worst case; rejects a memory-blowup
/// entry).
const MAX_WELCOME_LEN: usize = 1 << 20;

/// A Welcome addressed to a prospective joiner by their signature key
/// (ADR-0027 Phase 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WelcomeRendezvous {
    /// The joiner's leaf signature public key — the rendezvous tag the joiner
    /// matches against its own key (= the `proposal_ref` the admin approved).
    pub joiner_signature_key: Vec<u8>,
    /// The wire-encoded Welcome ([`crate::group::encode_message`] output) the
    /// joiner opens via [`crate::Group::join_from_welcome`].
    pub welcome: Vec<u8>,
}

impl WelcomeRendezvous {
    /// Serialize to the wire envelope.
    ///
    /// # Errors
    ///
    /// [`Error::Codec`] if the signature key or Welcome exceeds the bounds (so
    /// `encode`/`decode` are symmetric — a value that won't decode won't
    /// encode).
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.joiner_signature_key.is_empty() || self.joiner_signature_key.len() > MAX_SIG_KEY_LEN
        {
            return Err(Error::Codec("rendezvous: signature key length".into()));
        }
        if self.welcome.is_empty() || self.welcome.len() > MAX_WELCOME_LEN {
            return Err(Error::Codec("rendezvous: welcome length".into()));
        }
        // Cast is safe: bounded by MAX_SIG_KEY_LEN (256) above.
        #[allow(clippy::cast_possible_truncation)]
        let sig_len = self.joiner_signature_key.len() as u16;
        let mut out =
            Vec::with_capacity(HEADER_LEN + self.joiner_signature_key.len() + self.welcome.len());
        out.push(VERSION);
        out.extend_from_slice(&sig_len.to_be_bytes());
        out.extend_from_slice(&self.joiner_signature_key);
        out.extend_from_slice(&self.welcome);
        Ok(out)
    }

    /// Parse a wire envelope. Bounded + fail-closed + panic-free — the input is
    /// an attacker-controlled journal entry.
    ///
    /// # Errors
    ///
    /// [`Error::Codec`] for a wrong version, a truncated header / body, an
    /// out-of-bounds signature-key length, or an empty / oversized Welcome.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < HEADER_LEN {
            return Err(Error::Codec("rendezvous: truncated header".into()));
        }
        if bytes[0] != VERSION {
            return Err(Error::Codec("rendezvous: unknown version".into()));
        }
        let sig_len = u16::from_be_bytes([bytes[1], bytes[2]]) as usize;
        if sig_len == 0 || sig_len > MAX_SIG_KEY_LEN {
            return Err(Error::Codec("rendezvous: signature key length".into()));
        }
        let body = &bytes[HEADER_LEN..];
        if body.len() < sig_len {
            return Err(Error::Codec("rendezvous: truncated signature key".into()));
        }
        let joiner_signature_key = body[..sig_len].to_vec();
        let welcome = body[sig_len..].to_vec();
        if welcome.is_empty() || welcome.len() > MAX_WELCOME_LEN {
            return Err(Error::Codec("rendezvous: welcome length".into()));
        }
        Ok(Self {
            joiner_signature_key,
            welcome,
        })
    }

    /// `true` if this envelope is addressed to the given signature key — the
    /// joiner's poll-loop match.
    #[must_use]
    pub fn is_for(&self, signature_key: &[u8]) -> bool {
        self.joiner_signature_key == signature_key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> WelcomeRendezvous {
        WelcomeRendezvous {
            joiner_signature_key: vec![7u8; 32],
            welcome: vec![0xAB; 200],
        }
    }

    #[test]
    fn round_trip() {
        let r = sample();
        let bytes = r.encode().expect("encode");
        assert_eq!(WelcomeRendezvous::decode(&bytes).expect("decode"), r);
    }

    #[test]
    fn is_for_matches_signature_key() {
        let r = sample();
        assert!(r.is_for(&[7u8; 32]));
        assert!(!r.is_for(&[9u8; 32]));
    }

    #[test]
    fn rejects_unknown_version() {
        let r = sample();
        let mut bytes = r.encode().expect("encode");
        bytes[0] = 0xFF;
        assert!(WelcomeRendezvous::decode(&bytes).is_err());
    }

    #[test]
    fn rejects_truncated_header() {
        assert!(WelcomeRendezvous::decode(&[]).is_err());
        assert!(WelcomeRendezvous::decode(&[VERSION, 0]).is_err());
    }

    #[test]
    fn rejects_truncated_signature_key() {
        // Claims a 32-byte key but the body is shorter.
        let bytes = [VERSION, 0x00, 0x20, 1, 2, 3];
        assert!(WelcomeRendezvous::decode(&bytes).is_err());
    }

    #[test]
    fn rejects_zero_length_signature_key() {
        let bytes = [VERSION, 0x00, 0x00, 1, 2, 3];
        assert!(WelcomeRendezvous::decode(&bytes).is_err());
    }

    #[test]
    fn rejects_empty_welcome() {
        // Valid header + 32-byte key but no welcome bytes.
        let mut bytes = vec![VERSION, 0x00, 0x20];
        bytes.extend_from_slice(&[7u8; 32]);
        assert!(WelcomeRendezvous::decode(&bytes).is_err());
    }

    #[test]
    fn encode_rejects_empty_fields() {
        assert!(WelcomeRendezvous {
            joiner_signature_key: vec![],
            welcome: vec![1],
        }
        .encode()
        .is_err());
        assert!(WelcomeRendezvous {
            joiner_signature_key: vec![1],
            welcome: vec![],
        }
        .encode()
        .is_err());
    }
}
