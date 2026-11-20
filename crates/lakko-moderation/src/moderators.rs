// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The moderator list ([ADR-0099] section 3), signed by the offline root.
//!
//! ```text
//! list  = DOMAIN ‖ version(1) ‖ list_version(8) ‖ issued_at_ms(8)
//!         ‖ rp_id_len(1) ‖ rp_id ‖ origin_len(1) ‖ origin
//!         ‖ count(1) ‖ entry × count ‖ root_sig(64)
//! entry = moderator_id(16) ‖ name_len(1) ‖ name ‖ key_alg(1) ‖ key
//!         ‖ credential_id_len(2) ‖ credential_id ‖ x25519(32)
//!         ‖ added_at_ms(8) ‖ revoked_at_ms(8, 0 = not revoked)
//! ```
//!
//! `root_sig` is Ed25519 by the root key over every byte before it. `key` is
//! the moderator's WebAuthn credential public key in raw form: 32 bytes for
//! Ed25519 (`key_alg` 1), the 65-byte uncompressed SEC1 point for P-256
//! (`key_alg` 2). The offline tool converts the COSE key at enrolment, so no
//! verifier parses CBOR.
//!
//! A list is monotone by `list_version`: whoever pins the root refuses a
//! lower version than one it accepted. That rule is the caller's.
//!
//! [ADR-0099]: ../../../docs/adr/0099-reporting-and-moderation.md

use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, SIGNATURE_LEN};

use crate::seal::{Recipient, MODERATOR_ID_LEN};
use crate::wire::{header, put16, put8, Reader};
use crate::ModerationError;

/// The list's domain separator. Frozen.
pub const MODERATORS_DOMAIN: &[u8] = b"lakko/moderation/moderators/v1\0";
/// The only list version byte.
pub const MODERATORS_VERSION: u8 = 0x01;
/// The most moderators a list holds.
pub const MAX_MODERATORS: usize = 64;
/// The longest moderator name in bytes.
pub const MAX_NAME_BYTES: usize = 64;
/// The longest WebAuthn credential id (the WebAuthn bound).
pub const MAX_CREDENTIAL_ID: usize = 1023;

/// A moderator's WebAuthn credential public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModeratorKey {
    /// COSE EdDSA (-8): an Ed25519 key.
    Ed25519([u8; 32]),
    /// COSE ES256 (-7): an uncompressed P-256 point, `0x04 ‖ x ‖ y`.
    P256([u8; 65]),
}

impl ModeratorKey {
    fn write(&self, out: &mut Vec<u8>) {
        match self {
            Self::Ed25519(k) => {
                out.push(1);
                out.extend_from_slice(k);
            },
            Self::P256(k) => {
                out.push(2);
                out.extend_from_slice(k);
            },
        }
    }

    fn read(r: &mut Reader<'_>) -> Result<Self, ModerationError> {
        Ok(match r.u8()? {
            1 => Self::Ed25519(r.array()?),
            2 => {
                let k: [u8; 65] = r.array()?;
                if k[0] != 0x04 {
                    return Err(ModerationError::Malformed("p-256 point form"));
                }
                Self::P256(k)
            },
            _ => return Err(ModerationError::UnknownValue("key algorithm")),
        })
    }
}

/// One moderator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeratorEntry {
    /// The moderator id.
    pub moderator_id: [u8; MODERATOR_ID_LEN],
    /// The name the panel shows.
    pub name: String,
    /// The security key's credential public key.
    pub key: ModeratorKey,
    /// The WebAuthn credential id.
    pub credential_id: Vec<u8>,
    /// The X25519 key reports are sealed to.
    pub x25519_public: [u8; 32],
    /// When the moderator was added (unix ms).
    pub added_at_ms: u64,
    /// When the moderator was removed (unix ms); `0` when still active.
    pub revoked_at_ms: u64,
}

/// The signed moderator list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeratorList {
    /// The monotone list version.
    pub list_version: u64,
    /// When the root signed it (unix ms).
    pub issued_at_ms: u64,
    /// The panel's WebAuthn relying-party id.
    pub rp_id: String,
    /// The panel's origin, as WebAuthn reports it in client data.
    pub origin: String,
    /// The moderators.
    pub entries: Vec<ModeratorEntry>,
}

impl ModeratorList {
    fn body(&self) -> Result<Vec<u8>, ModerationError> {
        if self.entries.is_empty() || self.entries.len() > MAX_MODERATORS {
            return Err(ModerationError::Invalid("moderator count"));
        }
        if self.rp_id.is_empty() || self.origin.is_empty() {
            return Err(ModerationError::Invalid("rp id or origin"));
        }
        for (i, e) in self.entries.iter().enumerate() {
            if e.name.is_empty() || e.credential_id.is_empty() {
                return Err(ModerationError::Invalid("entry"));
            }
            if self.entries[..i]
                .iter()
                .any(|o| o.moderator_id == e.moderator_id || o.credential_id == e.credential_id)
            {
                return Err(ModerationError::Invalid("duplicate moderator"));
            }
        }
        let mut out = Vec::new();
        out.extend_from_slice(MODERATORS_DOMAIN);
        out.push(MODERATORS_VERSION);
        out.extend_from_slice(&self.list_version.to_be_bytes());
        out.extend_from_slice(&self.issued_at_ms.to_be_bytes());
        put8(&mut out, self.rp_id.as_bytes(), 253)?;
        put8(&mut out, self.origin.as_bytes(), 255)?;
        out.push(u8::try_from(self.entries.len()).map_err(|_| ModerationError::Invalid("count"))?);
        for e in &self.entries {
            out.extend_from_slice(&e.moderator_id);
            put8(&mut out, e.name.as_bytes(), MAX_NAME_BYTES)?;
            e.key.write(&mut out);
            put16(&mut out, &e.credential_id, MAX_CREDENTIAL_ID)?;
            out.extend_from_slice(&e.x25519_public);
            out.extend_from_slice(&e.added_at_ms.to_be_bytes());
            out.extend_from_slice(&e.revoked_at_ms.to_be_bytes());
        }
        Ok(out)
    }

    /// Sign with the root key (the offline tool's step).
    ///
    /// # Errors
    ///
    /// [`ModerationError::Invalid`] for a list that breaks a bound.
    pub fn sign(&self, root_seed: &[u8; 32]) -> Result<Vec<u8>, ModerationError> {
        let mut out = self.body()?;
        let sig = SigningKey::from_seed(root_seed).sign(&out).to_bytes();
        out.extend_from_slice(&sig);
        Ok(out)
    }

    /// Decode without checking the root signature. Only for display of a
    /// list a caller already verified, and for fuzzing.
    ///
    /// # Errors
    ///
    /// Any decode variant.
    pub fn decode_unverified(bytes: &[u8]) -> Result<Self, ModerationError> {
        if bytes.len() < SIGNATURE_LEN {
            return Err(ModerationError::Malformed("truncated"));
        }
        let (body, _) = bytes.split_at(bytes.len() - SIGNATURE_LEN);
        let mut r = header(body, MODERATORS_DOMAIN, MODERATORS_VERSION)?;
        let list_version = r.u64()?;
        let issued_at_ms = r.u64()?;
        let rp_id = utf8(r.bytes8(253)?)?;
        let origin = utf8(r.bytes8(255)?)?;
        let count = usize::from(r.u8()?);
        let mut entries = Vec::with_capacity(count.min(MAX_MODERATORS));
        for _ in 0..count {
            let moderator_id = r.array()?;
            let name = utf8(r.bytes8(MAX_NAME_BYTES)?)?;
            let key = ModeratorKey::read(&mut r)?;
            let credential_id = r.bytes16(MAX_CREDENTIAL_ID)?.to_vec();
            entries.push(ModeratorEntry {
                moderator_id,
                name,
                key,
                credential_id,
                x25519_public: r.array()?,
                added_at_ms: r.u64()?,
                revoked_at_ms: r.u64()?,
            });
        }
        r.finish()?;
        let list = Self {
            list_version,
            issued_at_ms,
            rp_id,
            origin,
            entries,
        };
        // The same rules the signer applies; also proves canonical form.
        if list.body()? != body {
            return Err(ModerationError::Malformed("not canonical"));
        }
        Ok(list)
    }

    /// Decode and check the root signature.
    ///
    /// # Errors
    ///
    /// Decode errors, [`ModerationError::BadSignature`].
    pub fn verify(bytes: &[u8], root_public: &[u8; 32]) -> Result<Self, ModerationError> {
        let list = Self::decode_unverified(bytes)?;
        let (body, sig) = bytes.split_at(bytes.len() - SIGNATURE_LEN);
        let key =
            VerifyingKey::from_bytes(root_public).map_err(|_| ModerationError::BadSignature)?;
        let sig = Signature::from_bytes(sig).map_err(|_| ModerationError::BadSignature)?;
        key.verify(body, &sig)
            .map_err(|_| ModerationError::BadSignature)?;
        Ok(list)
    }

    /// The moderator `id`, if it was listed and not revoked at `at_ms`.
    #[must_use]
    pub fn authorised_at(
        &self,
        id: &[u8; MODERATOR_ID_LEN],
        at_ms: u64,
    ) -> Option<&ModeratorEntry> {
        self.entries.iter().find(|e| {
            &e.moderator_id == id
                && e.added_at_ms <= at_ms
                && (e.revoked_at_ms == 0 || at_ms < e.revoked_at_ms)
        })
    }

    /// The moderators a report is sealed to now: every entry not revoked.
    #[must_use]
    pub fn recipients(&self) -> Vec<Recipient> {
        self.entries
            .iter()
            .filter(|e| e.revoked_at_ms == 0)
            .map(|e| Recipient {
                moderator_id: e.moderator_id,
                x25519_public: e.x25519_public,
            })
            .collect()
    }
}

fn utf8(bytes: &[u8]) -> Result<String, ModerationError> {
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| ModerationError::Malformed("utf-8"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const ROOT: [u8; 32] = [42; 32];

    pub(crate) fn root_public() -> [u8; 32] {
        SigningKey::from_seed(&ROOT).verifying_key().to_bytes()
    }

    pub(crate) fn entry(n: u8, key: ModeratorKey) -> ModeratorEntry {
        ModeratorEntry {
            moderator_id: [n; 16],
            name: format!("moderator {n}"),
            key,
            credential_id: vec![n; 64],
            x25519_public: [n; 32],
            added_at_ms: 1_000,
            revoked_at_ms: 0,
        }
    }

    pub(crate) fn list() -> ModeratorList {
        ModeratorList {
            list_version: 3,
            issued_at_ms: 2_000,
            rp_id: "moderation.lakko.internal".into(),
            origin: "https://moderation.lakko.internal".into(),
            entries: vec![
                entry(1, ModeratorKey::Ed25519([1; 32])),
                entry(2, ModeratorKey::P256([4; 65])),
            ],
        }
    }

    #[test]
    fn signed_list_verifies_under_the_root_only() {
        let bytes = list().sign(&ROOT).unwrap();
        assert_eq!(
            ModeratorList::verify(&bytes, &root_public()).unwrap(),
            list()
        );
        let other = SigningKey::from_seed(&[1; 32]).verifying_key().to_bytes();
        assert_eq!(
            ModeratorList::verify(&bytes, &other),
            Err(ModerationError::BadSignature)
        );
        let mut bad = bytes.clone();
        bad[MODERATORS_DOMAIN.len() + 2] ^= 1;
        assert!(ModeratorList::verify(&bad, &root_public()).is_err());
    }

    #[test]
    fn revocation_and_membership() {
        let mut l = list();
        l.entries[0].revoked_at_ms = 5_000;
        assert!(l.authorised_at(&[1; 16], 4_999).is_some());
        assert!(l.authorised_at(&[1; 16], 5_000).is_none());
        assert!(l.authorised_at(&[1; 16], 999).is_none());
        assert!(l.authorised_at(&[9; 16], 4_000).is_none());
        assert_eq!(l.recipients().len(), 1);
    }

    #[test]
    fn duplicates_and_bad_points_are_refused() {
        let mut l = list();
        l.entries[1].moderator_id = [1; 16];
        assert!(l.sign(&ROOT).is_err());
        let bytes = list().sign(&ROOT).unwrap();
        for n in 0..bytes.len() {
            assert!(ModeratorList::decode_unverified(&bytes[..n]).is_err());
        }
    }
}
