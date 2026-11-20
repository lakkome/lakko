// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The group's knock public key, handed to every member ([ADR-0091] phase 2,
//! section 3, route 3).
//!
//! A member that fell behind a group with no other device of its account in
//! it asks to come back through the group's knock queue, sealed to the knock
//! key (ADR-0087 section 5). Only the owner and the admins hold the knock
//! seeds ([`crate::knock_seed`]); a plain member did not hold even the public
//! key. The owner therefore sends this envelope in the group, re-sent on a
//! version change and on growth like the group name.
//!
//! ## Wire format (v1)
//!
//! ```text
//! KNOCK_KEY_ENVELOPE_DOMAIN_V1 ("lakko/group-knock-key/v1\0")
//!   ‖ version(1)=0x01 ‖ knock_version(2, BE) ‖ knock_pk(32)
//! ```
//!
//! Fixed length [`KNOCK_KEY_ENVELOPE_LEN`] (60).
//!
//! ## The receiver's rule
//!
//! A member keeps the key only when the MLS sender resolves at receipt (C-68)
//! to the group's owner or an admin, and only a strictly newer
//! `knock_version` ([`knock_key_is_newer`]) replaces the one it holds. A key
//! from anyone else is dropped. The key is public: holding a wrong one costs
//! only a request nobody opens.
//!
//! [ADR-0091]: ../../../docs/adr/0091-conversation-rejoin.md

use core::result::Result as CoreResult;

use thiserror::Error;

use crate::knock::KNOCK_KEY_LEN;

/// Domain separator. Frozen: the dispatcher matches on it.
pub const KNOCK_KEY_ENVELOPE_DOMAIN_V1: &[u8] = b"lakko/group-knock-key/v1\x00";

/// Schema-version byte carried first after the separator.
pub const KNOCK_KEY_VERSION_V1: u8 = 0x01;

/// Bytes after the separator: version, knock version, key.
pub const KNOCK_KEY_PAYLOAD_LEN: usize = 1 + 2 + KNOCK_KEY_LEN;

/// The one well-formed length.
pub const KNOCK_KEY_ENVELOPE_LEN: usize =
    KNOCK_KEY_ENVELOPE_DOMAIN_V1.len() + KNOCK_KEY_PAYLOAD_LEN;

/// A group's knock public key and its version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnockKey {
    /// The knock-key version (ADR-0087 section 5).
    pub knock_version: u16,
    /// The X25519 knock public key of that version.
    pub knock_pk: [u8; KNOCK_KEY_LEN],
}

/// Why a knock-key envelope was refused. A reason code; nothing from the
/// payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum KnockKeyError {
    /// No domain separator. Only from [`decode_knock_key`].
    #[error("plaintext does not carry the knock-key domain separator")]
    WrongDomain,
    /// The payload is not exactly the fixed length.
    #[error("knock-key envelope has the wrong length")]
    BadLength,
    /// The version byte is not [`KNOCK_KEY_VERSION_V1`].
    #[error("knock-key envelope carries unknown schema version {0:#04x}")]
    UnknownVersion(u8),
}

impl KnockKeyError {
    /// Stable, language-neutral reason code. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::WrongDomain => "wrongDomain",
            Self::BadLength => "badLength",
            Self::UnknownVersion(_) => "unknownVersion",
        }
    }
}

/// Compose the envelope the owner sends in the group.
#[must_use]
pub fn compose_knock_key(key: &KnockKey) -> Vec<u8> {
    let mut out = Vec::with_capacity(KNOCK_KEY_ENVELOPE_LEN);
    out.extend_from_slice(KNOCK_KEY_ENVELOPE_DOMAIN_V1);
    out.push(KNOCK_KEY_VERSION_V1);
    out.extend_from_slice(&key.knock_version.to_be_bytes());
    out.extend_from_slice(&key.knock_pk);
    out
}

/// Discriminated unwrap for the [`crate::incoming_envelope`] dispatcher:
/// `Ok(None)` when the prefix does not match.
///
/// # Errors
///
/// [`KnockKeyError::UnknownVersion`] (checked before the length) and
/// [`KnockKeyError::BadLength`].
pub fn try_unwrap_knock_key(plaintext: &[u8]) -> CoreResult<Option<KnockKey>, KnockKeyError> {
    let Some(payload) = plaintext.strip_prefix(KNOCK_KEY_ENVELOPE_DOMAIN_V1) else {
        return Ok(None);
    };
    let (&version, rest) = payload.split_first().ok_or(KnockKeyError::BadLength)?;
    if version != KNOCK_KEY_VERSION_V1 {
        return Err(KnockKeyError::UnknownVersion(version));
    }
    if rest.len() != 2 + KNOCK_KEY_LEN {
        return Err(KnockKeyError::BadLength);
    }
    let knock_version = u16::from_be_bytes([rest[0], rest[1]]);
    let mut knock_pk = [0u8; KNOCK_KEY_LEN];
    knock_pk.copy_from_slice(&rest[2..]);
    Ok(Some(KnockKey {
        knock_version,
        knock_pk,
    }))
}

/// Strict decoder: a plaintext without the separator is a hard error.
///
/// # Errors
///
/// [`KnockKeyError::WrongDomain`] and every error of
/// [`try_unwrap_knock_key`].
pub fn decode_knock_key(bytes: &[u8]) -> CoreResult<KnockKey, KnockKeyError> {
    try_unwrap_knock_key(bytes)?.ok_or(KnockKeyError::WrongDomain)
}

/// `true` when an incoming knock version replaces the stored one: strictly
/// greater. A re-sent envelope of the version held is not an update.
#[must_use]
pub const fn knock_key_is_newer(incoming_version: u16, stored_version: Option<u16>) -> bool {
    match stored_version {
        None => true,
        Some(stored) => incoming_version > stored,
    }
}

/// The fuzz oracle behind `fuzz_api::knock_key_decode`: `None` when the
/// decoder refuses; `Some(true)` when what decoded re-composes to the same
/// bytes at the fixed length.
pub(crate) fn fuzz_decode(bytes: &[u8]) -> Option<bool> {
    let key = decode_knock_key(bytes).ok()?;
    Some(compose_knock_key(&key) == bytes && bytes.len() == KNOCK_KEY_ENVELOPE_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const KEY: KnockKey = KnockKey {
        knock_version: 7,
        knock_pk: [0x4B; KNOCK_KEY_LEN],
    };

    #[test]
    fn compose_decode_round_trips_at_a_fixed_length() {
        let bytes = compose_knock_key(&KEY);
        assert_eq!(KNOCK_KEY_ENVELOPE_DOMAIN_V1.len(), 25);
        assert_eq!(bytes.len(), KNOCK_KEY_ENVELOPE_LEN);
        assert_eq!(KNOCK_KEY_ENVELOPE_LEN, 60);
        assert_eq!(decode_knock_key(&bytes), Ok(KEY));
        assert_eq!(fuzz_decode(&bytes), Some(true));
    }

    #[test]
    fn version_and_length_are_checked() {
        let good = compose_knock_key(&KEY);
        let domain = KNOCK_KEY_ENVELOPE_DOMAIN_V1.len();
        assert_eq!(
            try_unwrap_knock_key(&good[..good.len() - 1]),
            Err(KnockKeyError::BadLength)
        );
        let mut long = good.clone();
        long.push(0);
        assert_eq!(try_unwrap_knock_key(&long), Err(KnockKeyError::BadLength));
        let mut v2 = good;
        v2[domain] = 2;
        assert_eq!(
            try_unwrap_knock_key(&v2),
            Err(KnockKeyError::UnknownVersion(2))
        );
        assert_eq!(
            try_unwrap_knock_key(KNOCK_KEY_ENVELOPE_DOMAIN_V1),
            Err(KnockKeyError::BadLength)
        );
        assert_eq!(try_unwrap_knock_key(b"hello"), Ok(None));
        assert_eq!(decode_knock_key(b"hello"), Err(KnockKeyError::WrongDomain));
    }

    #[test]
    fn only_a_strictly_newer_version_replaces() {
        assert!(knock_key_is_newer(0, None));
        assert!(knock_key_is_newer(3, Some(2)));
        assert!(!knock_key_is_newer(2, Some(2)));
        assert!(!knock_key_is_newer(1, Some(2)));
    }

    #[test]
    fn it_does_not_collide_with_the_knock_seed_envelope() {
        let seed_domain = crate::knock_seed::KNOCK_SEED_ENVELOPE_DOMAIN_V1;
        assert!(!seed_domain.starts_with(KNOCK_KEY_ENVELOPE_DOMAIN_V1));
        assert!(!KNOCK_KEY_ENVELOPE_DOMAIN_V1.starts_with(seed_domain));
        assert!(matches!(
            crate::incoming_envelope::dispatch_incoming_envelope(&compose_knock_key(&KEY)),
            Ok(crate::incoming_envelope::IncomingEnvelope::KnockKey(k)) if k == KEY
        ));
    }

    proptest! {
        #[test]
        fn decode_never_panics_and_round_trips(bytes in proptest::collection::vec(any::<u8>(), 0..80)) {
            let mut framed = KNOCK_KEY_ENVELOPE_DOMAIN_V1.to_vec();
            framed.extend_from_slice(&bytes);
            if let Some(ok) = fuzz_decode(&framed) {
                prop_assert!(ok);
            }
            let _ = fuzz_decode(&bytes);
        }
    }
}
