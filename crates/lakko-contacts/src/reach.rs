// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Reach keys ([ADR-0060](../../../docs/adr/0060-scoped-discovery.md) §1): the
//! Ed25519 keys an identity mailbox accepts a write under.
//!
//! ## What a reach key is
//!
//! Knowing a person's username or account id does not let anyone write to
//! them. A claimed mailbox accepts a write only when it is signed with a
//! **reach key** the owner registered, and reach keys are handed out, never
//! looked up: to contacts, to link holders, to members of groups the owner
//! lets find them, to contacts of contacts. This module derives them.
//!
//! ## Derivation
//!
//! ```text
//! reach_seed = BLAKE3-derive-key("lakko/reach/v1",
//!                  account_seed(32) || class(1) || scope_len(1) || scope_id || version(4, BE))
//! reach_key  = Ed25519 key pair from reach_seed
//! ```
//!
//! Every seed is derived from the account seed, so all of a person's devices
//! compute the same keys and only the version counters need to travel
//! between them (ADR-0060 §7). `scope_len` is a length prefix, so two
//! `(scope_id, version)` pairs can never produce the same material.
//!
//! A **shared** key per class and scope, never one per contact: a per-contact
//! key would tell the server how many contacts a person has (ADR-0060 §1).
//!
//! ## The scope shape is checked
//!
//! `contact` and `fof` are account-wide and carry an **empty** scope; every
//! other class names something (a link id, a request id, a group or channel
//! id) and carries 1 to [`MAX_REACH_SCOPE_ID_LEN`] bytes.
//! [`ReachClass::check_scope`] enforces it for the derivation and for
//! [`crate::visibility::ReachState`], so a caller cannot derive a `group` key
//! for "no group" by mistake.
//!
//! ## The continuity key
//!
//! One reach key is **not** derived from the account seed:
//! [`continuity_reach_seed`] ([ADR-0073](../../../docs/adr/0073-account-key-reset.md)
//! R.4). After an account key reset the new identity knows none of its former
//! contacts and so can hand nobody a grant, while the peers hold only grants
//! for a mailbox that no longer exists. Both sides can compute this seed from
//! the account id and the key generation the reset moved to, so the resetting
//! device registers its public key and a peer that accepted the reset signs
//! its Welcome with it. It is known to anyone who knows the account id and
//! the generation; it is a reach key, not a credential.
//!
//! ## A grant is a secret
//!
//! A [`ReachGrant`] carries the **seed**, not the public key: whoever holds it
//! can sign mailbox writes. It travels only inside E2EE carriers (a profile
//! envelope in a group, an introduction record sealed under an OPRF output),
//! never to a server (invariant #2). The seed is held in
//! [`lakko_crypto::SecretBytes`], which zeroes it on drop, compares in
//! constant time and never prints it.

use core::fmt;
use core::result::Result as CoreResult;

use lakko_crypto::hash::blake3_derive_key_from_material;
use lakko_crypto::signing::SigningKey;
use lakko_crypto::SecretBytes;
use thiserror::Error;

/// BLAKE3 derive-key context for reach seeds. Frozen: changing it changes
/// every reach key of every account.
pub const REACH_SEED_DOMAIN: &str = "lakko/reach/v1";

/// BLAKE3 derive-key context for the continuity reach seed (ADR-0073 R.4).
/// Frozen: changing it strands every peer of every account that reset.
pub const CONTINUITY_REACH_SEED_DOMAIN: &str = "lakko/reach/continuity/v1";

/// Length of an account id, the first input of [`continuity_reach_seed`].
pub const CONTINUITY_ACCOUNT_ID_LEN: usize = 16;

/// Length of an account seed, a reach seed and an Ed25519 public key.
pub const REACH_SEED_LEN: usize = 32;

/// Longest scope id a reach key may be derived over (a group, channel, link
/// or request id).
pub const MAX_REACH_SCOPE_ID_LEN: usize = 64;

/// Wire length of a [`ReachGrant`]: `class(1) || version(4, BE) || seed(32)`.
pub const REACH_GRANT_LEN: usize = 1 + 4 + REACH_SEED_LEN;

/// Who a reach key is handed to (ADR-0060 §1). The byte values are frozen:
/// they enter the key derivation and every wire that names a class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReachClass {
    /// Every contact. Empty scope.
    Contact,
    /// Whoever holds one invite link. Scope: the capability id.
    Link,
    /// The one party a request of the owner's went to. Scope: a random id.
    Reply,
    /// Members of a group the owner lets find them. Scope: the group id.
    Group,
    /// Every co-member of a group, whatever the setting (device
    /// re-admission Welcomes). Scope: the group id.
    Member,
    /// Members of a channel the owner lets find them. Scope: the channel id.
    Channel,
    /// Contacts of contacts. Empty scope.
    Fof,
}

impl ReachClass {
    /// Every class, in wire-byte order.
    pub const ALL: [Self; 7] = [
        Self::Contact,
        Self::Link,
        Self::Reply,
        Self::Group,
        Self::Member,
        Self::Channel,
        Self::Fof,
    ];

    /// The frozen wire byte.
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        match self {
            Self::Contact => 1,
            Self::Link => 2,
            Self::Reply => 3,
            Self::Group => 4,
            Self::Member => 5,
            Self::Channel => 6,
            Self::Fof => 7,
        }
    }

    /// Parse a wire byte, fail-closed.
    ///
    /// # Errors
    ///
    /// [`ReachError::UnknownClass`] for any byte that is not a class.
    pub const fn from_byte(byte: u8) -> CoreResult<Self, ReachError> {
        match byte {
            1 => Ok(Self::Contact),
            2 => Ok(Self::Link),
            3 => Ok(Self::Reply),
            4 => Ok(Self::Group),
            5 => Ok(Self::Member),
            6 => Ok(Self::Channel),
            7 => Ok(Self::Fof),
            other => Err(ReachError::UnknownClass(other)),
        }
    }

    /// `true` for the account-wide classes, whose scope id is empty.
    #[must_use]
    pub const fn is_account_wide(self) -> bool {
        matches!(self, Self::Contact | Self::Fof)
    }

    /// Check that `scope_id` has the shape this class requires: empty for an
    /// account-wide class, 1 to [`MAX_REACH_SCOPE_ID_LEN`] bytes otherwise.
    ///
    /// # Errors
    ///
    /// * [`ReachError::ScopeIdTooLong`] — longer than the maximum.
    /// * [`ReachError::ScopeIdNotEmpty`] — an account-wide class with a scope.
    /// * [`ReachError::ScopeIdEmpty`] — a scoped class without one.
    pub const fn check_scope(self, scope_id: &[u8]) -> CoreResult<(), ReachError> {
        if scope_id.len() > MAX_REACH_SCOPE_ID_LEN {
            return Err(ReachError::ScopeIdTooLong {
                len: scope_id.len(),
            });
        }
        if self.is_account_wide() {
            if !scope_id.is_empty() {
                return Err(ReachError::ScopeIdNotEmpty);
            }
        } else if scope_id.is_empty() {
            return Err(ReachError::ScopeIdEmpty);
        }
        Ok(())
    }
}

/// Why a reach derivation or a grant was refused. A reason code; never a
/// seed byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ReachError {
    /// The class byte is not a [`ReachClass`].
    #[error("unknown reach class {0:#04x}")]
    UnknownClass(u8),
    /// The scope id is longer than [`MAX_REACH_SCOPE_ID_LEN`].
    #[error("reach scope id is {len} bytes, at most {MAX_REACH_SCOPE_ID_LEN} allowed")]
    ScopeIdTooLong {
        /// The offending length.
        len: usize,
    },
    /// An account-wide class (`contact`, `fof`) was given a scope id.
    #[error("an account-wide reach class takes no scope id")]
    ScopeIdNotEmpty,
    /// A scoped class was given an empty scope id.
    #[error("a scoped reach class needs a scope id")]
    ScopeIdEmpty,
    /// A grant is not exactly [`REACH_GRANT_LEN`] bytes.
    #[error("reach grant is {actual} bytes, expected {REACH_GRANT_LEN}")]
    BadLength {
        /// The length received.
        actual: usize,
    },
}

impl ReachError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnknownClass(_) => "unknownClass",
            Self::ScopeIdTooLong { .. } => "scopeIdTooLong",
            Self::ScopeIdNotEmpty => "scopeIdNotEmpty",
            Self::ScopeIdEmpty => "scopeIdEmpty",
            Self::BadLength { .. } => "badLength",
        }
    }
}

/// Derive the reach seed for `(class, scope_id, version)` from the account
/// seed.
///
/// The derivation material holds the account seed; it is built in a heap
/// buffer that is overwritten before it is dropped. That overwrite is best
/// effort: this crate carries no `zeroize` dependency (it is linked into the
/// size-budgeted iOS extension), so the compiler is free to elide it. The
/// caller holds the account seed for longer than this function does in any
/// case.
///
/// # Errors
///
/// Every error of [`ReachClass::check_scope`].
pub fn reach_seed(
    account_seed: &[u8; REACH_SEED_LEN],
    class: ReachClass,
    scope_id: &[u8],
    version: u32,
) -> CoreResult<SecretBytes<REACH_SEED_LEN>, ReachError> {
    class.check_scope(scope_id)?;
    let scope_len = u8::try_from(scope_id.len()).map_err(|_| ReachError::ScopeIdTooLong {
        len: scope_id.len(),
    })?;
    let mut material = Vec::with_capacity(REACH_SEED_LEN + 1 + 1 + scope_id.len() + 4);
    material.extend_from_slice(account_seed);
    material.push(class.to_byte());
    material.push(scope_len);
    material.extend_from_slice(scope_id);
    material.extend_from_slice(&version.to_be_bytes());
    let seed = blake3_derive_key_from_material(REACH_SEED_DOMAIN, &material);
    material.fill(0);
    Ok(SecretBytes::new(seed))
}

/// Derive the continuity reach seed of an account at a key generation
/// (ADR-0073 R.4):
///
/// ```text
/// BLAKE3-derive-key("lakko/reach/continuity/v1",
///     account_id(16) || key_generation(8, BE))
/// ```
///
/// Unlike [`reach_seed`] no input is secret: the account id and the
/// generation are what a peer verified in the key-reset attestation, and the
/// resetting device knows both. Whoever knows them can write to the mailbox
/// while the owner keeps the key registered; that is the stated scope of the
/// key, and why the owner registers it with an expiry. The fixed-width
/// material cannot collide with a [`reach_seed`] input, and the two use
/// different derive-key contexts in any case.
///
/// The result is wrapped in [`SecretBytes`] for a uniform type with
/// [`reach_seed`], not because it is confidential.
#[must_use]
pub fn continuity_reach_seed(
    account_id: &[u8; CONTINUITY_ACCOUNT_ID_LEN],
    key_generation: u64,
) -> SecretBytes<REACH_SEED_LEN> {
    let mut material = [0u8; CONTINUITY_ACCOUNT_ID_LEN + 8];
    material[..CONTINUITY_ACCOUNT_ID_LEN].copy_from_slice(account_id);
    material[CONTINUITY_ACCOUNT_ID_LEN..].copy_from_slice(&key_generation.to_be_bytes());
    SecretBytes::new(blake3_derive_key_from_material(
        CONTINUITY_REACH_SEED_DOMAIN,
        &material,
    ))
}

/// The Ed25519 public key of a reach seed: what the owner registers with the
/// Delivery Service and what a mailbox write is verified against.
#[must_use]
pub fn reach_verifying_key(seed: &[u8; REACH_SEED_LEN]) -> [u8; REACH_SEED_LEN] {
    SigningKey::from_seed(seed).verifying_key().to_bytes()
}

/// A reach key handed to someone: its class, its version, and the seed.
///
/// The scope is implicit in where the grant travels (the group a profile is
/// published in, the channel a member card belongs to), so it is not on the
/// wire. `Debug` never prints the seed.
#[derive(Clone, PartialEq, Eq)]
pub struct ReachGrant {
    /// Who the key is for.
    pub class: ReachClass,
    /// The key version, bumped on every rotation.
    pub version: u32,
    /// The Ed25519 seed. Secret: whoever holds it can write to the mailbox.
    pub seed: SecretBytes<REACH_SEED_LEN>,
}

impl fmt::Debug for ReachGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReachGrant")
            .field("class", &self.class)
            .field("version", &self.version)
            .field("seed", &"<redacted>")
            .finish()
    }
}

impl ReachGrant {
    /// Derive the grant for `(class, scope_id, version)` from the account seed.
    ///
    /// # Errors
    ///
    /// Every error of [`reach_seed`].
    pub fn derive(
        account_seed: &[u8; REACH_SEED_LEN],
        class: ReachClass,
        scope_id: &[u8],
        version: u32,
    ) -> CoreResult<Self, ReachError> {
        Ok(Self {
            class,
            version,
            seed: reach_seed(account_seed, class, scope_id, version)?,
        })
    }

    /// The public key the holder signs mailbox writes under.
    #[must_use]
    pub fn verifying_key(&self) -> [u8; REACH_SEED_LEN] {
        reach_verifying_key(self.seed.as_bytes())
    }

    /// Encode as `class(1) || version(4, BE) || seed(32)`.
    ///
    /// The output holds the seed; the caller seals it before it leaves the
    /// device.
    #[must_use]
    pub fn encode(&self) -> [u8; REACH_GRANT_LEN] {
        let mut out = [0u8; REACH_GRANT_LEN];
        out[0] = self.class.to_byte();
        out[1..5].copy_from_slice(&self.version.to_be_bytes());
        out[5..].copy_from_slice(self.seed.as_bytes());
        out
    }

    /// Decode exactly [`REACH_GRANT_LEN`] bytes, fail-closed.
    ///
    /// # Errors
    ///
    /// * [`ReachError::BadLength`] — not exactly the grant length.
    /// * [`ReachError::UnknownClass`] — the class byte is not a class.
    pub fn decode(bytes: &[u8]) -> CoreResult<Self, ReachError> {
        if bytes.len() != REACH_GRANT_LEN {
            return Err(ReachError::BadLength {
                actual: bytes.len(),
            });
        }
        let class = ReachClass::from_byte(bytes[0])?;
        let version_bytes: [u8; 4] = bytes[1..5].try_into().map_err(|_| ReachError::BadLength {
            actual: bytes.len(),
        })?;
        let seed: [u8; REACH_SEED_LEN] =
            bytes[5..].try_into().map_err(|_| ReachError::BadLength {
                actual: bytes.len(),
            })?;
        Ok(Self {
            class,
            version: u32::from_be_bytes(version_bytes),
            seed: SecretBytes::new(seed),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCOUNT: [u8; 32] = [0x42; 32];

    fn hex(bytes: &[u8]) -> String {
        use core::fmt::Write as _;
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            let _ = write!(s, "{b:02x}");
        }
        s
    }

    #[test]
    fn class_bytes_are_frozen_and_round_trip() {
        let expected = [1u8, 2, 3, 4, 5, 6, 7];
        for (class, byte) in ReachClass::ALL.iter().zip(expected) {
            assert_eq!(class.to_byte(), byte);
            assert_eq!(ReachClass::from_byte(byte), Ok(*class));
        }
        for byte in [0u8, 8, 0x7F, 0xFF] {
            assert_eq!(
                ReachClass::from_byte(byte),
                Err(ReachError::UnknownClass(byte))
            );
        }
    }

    #[test]
    fn golden_pins_the_derivation() {
        let seed = reach_seed(&ACCOUNT, ReachClass::Group, b"group-1", 3).unwrap();
        assert_eq!(
            hex(seed.as_bytes()),
            "00cad2ad9a1885862187f17ef75e33818653b1b54b9a480dad919d3d3d9d851e",
            "a change here changes every reach key of every account"
        );
        assert_eq!(
            hex(&reach_verifying_key(seed.as_bytes())),
            "e7c2e2aa6ff74083318d5f6b1dfa7939c3cc074549376d9e91c5592a67a35e8f"
        );
    }

    #[test]
    fn golden_pins_the_continuity_derivation() {
        let seed = continuity_reach_seed(&[0x11; 16], 2);
        assert_eq!(
            hex(seed.as_bytes()),
            "764a0db71d87bb535affdc9dc1d51df8648a26c8c670fa4708dcc6f31b5cc228",
            "a change here strands every peer of every account that reset"
        );
        assert_eq!(
            hex(&reach_verifying_key(seed.as_bytes())),
            "c797f30bb7df3eed9ea6a65c183f7d5fc391387022a2a1150adddea3c4caf02c"
        );
    }

    #[test]
    fn continuity_seed_separates_on_every_input_and_is_deterministic() {
        let base = continuity_reach_seed(&[0x11; 16], 2);
        assert_eq!(base, continuity_reach_seed(&[0x11; 16], 2));
        assert_ne!(base, continuity_reach_seed(&[0x12; 16], 2));
        assert_ne!(base, continuity_reach_seed(&[0x11; 16], 3));
        assert_ne!(base, continuity_reach_seed(&[0x11; 16], 2 << 8));
    }

    #[test]
    fn continuity_seed_is_domain_separated_from_reach_seed() {
        // The same 24 bytes of material under the two contexts.
        let account_id = [0x11u8; 16];
        let generation = 2u64;
        let mut material = account_id.to_vec();
        material.extend_from_slice(&generation.to_be_bytes());
        let under_reach_domain = blake3_derive_key_from_material(REACH_SEED_DOMAIN, &material);
        let continuity = continuity_reach_seed(&account_id, generation);
        assert_ne!(continuity.as_bytes(), &under_reach_domain);
        assert_eq!(
            continuity.as_bytes(),
            &blake3_derive_key_from_material(CONTINUITY_REACH_SEED_DOMAIN, &material),
            "the material is account_id(16) || key_generation(8, BE), nothing else"
        );
        assert_ne!(CONTINUITY_REACH_SEED_DOMAIN, REACH_SEED_DOMAIN);

        // And no reach_seed of an account whose seed embeds the account id
        // equals it, for any class.
        let mut account_seed = [0u8; 32];
        account_seed[..16].copy_from_slice(&account_id);
        for class in ReachClass::ALL {
            let scope: &[u8] = if class.is_account_wide() { b"" } else { b"s" };
            for version in [0u32, 2] {
                let other = reach_seed(&account_seed, class, scope, version).unwrap();
                assert_ne!(continuity, other);
            }
        }
    }

    #[test]
    fn every_input_separates_the_seed() {
        let base = reach_seed(&ACCOUNT, ReachClass::Group, b"g", 1).unwrap();
        let others = [
            reach_seed(&[0x43; 32], ReachClass::Group, b"g", 1).unwrap(),
            reach_seed(&ACCOUNT, ReachClass::Member, b"g", 1).unwrap(),
            reach_seed(&ACCOUNT, ReachClass::Group, b"h", 1).unwrap(),
            reach_seed(&ACCOUNT, ReachClass::Group, b"g", 2).unwrap(),
            reach_seed(&ACCOUNT, ReachClass::Group, b"gg", 1).unwrap(),
        ];
        for other in &others {
            assert_ne!(&base, other);
        }
        assert_eq!(
            base,
            reach_seed(&ACCOUNT, ReachClass::Group, b"g", 1).unwrap(),
            "deterministic: every device derives the same key"
        );
    }

    #[test]
    fn scope_ids_that_share_a_prefix_derive_distinct_seeds() {
        let a = reach_seed(&ACCOUNT, ReachClass::Link, b"g\x00\x00\x00", 1).unwrap();
        let b = reach_seed(&ACCOUNT, ReachClass::Link, b"g", 0).unwrap();
        let c = reach_seed(&ACCOUNT, ReachClass::Link, b"g\x00", 0).unwrap();
        assert_ne!(a, b);
        assert_ne!(b, c);
    }

    #[test]
    fn scope_shape_is_enforced() {
        assert_eq!(
            reach_seed(&ACCOUNT, ReachClass::Contact, b"x", 0).err(),
            Some(ReachError::ScopeIdNotEmpty)
        );
        assert_eq!(
            reach_seed(&ACCOUNT, ReachClass::Fof, b"x", 0).err(),
            Some(ReachError::ScopeIdNotEmpty)
        );
        for class in [
            ReachClass::Link,
            ReachClass::Reply,
            ReachClass::Group,
            ReachClass::Member,
            ReachClass::Channel,
        ] {
            assert_eq!(
                reach_seed(&ACCOUNT, class, b"", 0).err(),
                Some(ReachError::ScopeIdEmpty)
            );
            assert!(reach_seed(&ACCOUNT, class, &[1; MAX_REACH_SCOPE_ID_LEN], 0).is_ok());
            assert_eq!(
                reach_seed(&ACCOUNT, class, &[1; MAX_REACH_SCOPE_ID_LEN + 1], 0).err(),
                Some(ReachError::ScopeIdTooLong {
                    len: MAX_REACH_SCOPE_ID_LEN + 1
                })
            );
        }
        assert!(reach_seed(&ACCOUNT, ReachClass::Contact, b"", 0).is_ok());
        assert!(reach_seed(&ACCOUNT, ReachClass::Fof, b"", u32::MAX).is_ok());
    }

    #[test]
    fn grant_round_trips_and_its_layout_is_pinned() {
        for class in ReachClass::ALL {
            let scope: &[u8] = if class.is_account_wide() { b"" } else { b"s" };
            for version in [0, 1, 0x0102_0304, u32::MAX] {
                let grant = ReachGrant::derive(&ACCOUNT, class, scope, version).unwrap();
                let wire = grant.encode();
                assert_eq!(wire.len(), REACH_GRANT_LEN);
                assert_eq!(wire[0], class.to_byte());
                assert_eq!(&wire[1..5], &version.to_be_bytes());
                assert_eq!(&wire[5..], grant.seed.as_bytes());
                assert_eq!(ReachGrant::decode(&wire).unwrap(), grant);
            }
        }
        assert_eq!(REACH_GRANT_LEN, 37);
    }

    #[test]
    fn grant_decode_is_fail_closed() {
        let good = ReachGrant::derive(&ACCOUNT, ReachClass::Contact, b"", 1)
            .unwrap()
            .encode();
        for len in 0..REACH_GRANT_LEN {
            assert_eq!(
                ReachGrant::decode(&good[..len]),
                Err(ReachError::BadLength { actual: len })
            );
        }
        let mut long = good.to_vec();
        long.push(0);
        assert_eq!(
            ReachGrant::decode(&long),
            Err(ReachError::BadLength {
                actual: REACH_GRANT_LEN + 1
            })
        );
        let mut bad_class = good;
        bad_class[0] = 0;
        assert_eq!(
            ReachGrant::decode(&bad_class),
            Err(ReachError::UnknownClass(0))
        );
    }

    #[test]
    fn debug_never_prints_the_seed() {
        let grant = ReachGrant {
            class: ReachClass::Group,
            version: 1,
            seed: SecretBytes::new([0xAB; 32]),
        };
        let dbg = format!("{grant:?}");
        assert!(!dbg.contains("ab") && !dbg.contains("AB") && !dbg.contains("171"));
        assert!(dbg.contains("redacted"));
    }

    #[test]
    fn the_verifying_key_is_the_ed25519_public_key_of_the_seed() {
        let grant = ReachGrant::derive(&ACCOUNT, ReachClass::Fof, b"", 9).unwrap();
        let signing = SigningKey::from_seed(grant.seed.as_bytes());
        let sig = signing.sign(b"mailbox write");
        let vk = lakko_crypto::signing::VerifyingKey::from_bytes(&grant.verifying_key()).unwrap();
        assert!(vk.verify(b"mailbox write", &sig).is_ok());
    }
}
