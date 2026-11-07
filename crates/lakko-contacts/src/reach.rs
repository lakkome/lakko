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
//! **Retired `contact` keys (maintainer decision, 2026-11-06).** Removing a
//! contact rotates the shared `contact` key (version + 1) and republishes the
//! profile with the new grant in every remaining contact's one-to-one. A
//! contact offline across a rotation still holds an old grant, so the owner
//! keeps the last [`RETIRED_CONTACT_VERSIONS_KEPT`] versions registered
//! ([`contact_key_versions`]). The Delivery Service reports which key admitted
//! each mailbox row; [`classify_contact_key`] places it, and
//! [`contact_key_row_verdict`] is the one rule: a row under a retired key from
//! a current contact is processed and answered with the current grant, from
//! anyone else it is dropped. A key older than the window is unregistered, so
//! its writer is refused at the sender.
//!
//! ## The scope shape is checked
//!
//! `contact`, `group`, `member`, `channel` and `fof` are account-wide and
//! carry an **empty** scope; `link` and `reply` name something (a link id, a
//! request id) and carry 1 to [`MAX_REACH_SCOPE_ID_LEN`] bytes.
//! [`ReachClass::check_scope`] enforces it for the derivation and for
//! [`crate::visibility::ReachState`].
//!
//! ## One key per class, not per group ([ADR-0078] section 9)
//!
//! Until ADR-0078 a `group`, `member` or `channel` key was derived per group
//! or channel id, so every mailbox write under one carried a stable label
//! naming the group its writer shares with the owner: writes under one label
//! were writes from co-members of one group, readable from the Delivery
//! Service's table alone. These three classes are now **account-wide**: one
//! key per class, handed out in every group and channel the setting allows.
//! The server learns "a co-member of some group of this person wrote", not
//! which group. The cost is revocation: leaving one group (or narrowing the
//! setting) cannot revoke that group's key alone, so the client bumps the
//! class version and republishes its profile in every remaining group.
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
//! ## A grant carries its mailbox ([ADR-0078] section 1)
//!
//! Since ADR-0078 a mailbox is addressed by a mailbox key that is not the
//! identity key ([`crate::mailbox`]), so a grant holder could not tell where
//! to write from the seed alone. A grant therefore carries the owner's
//! [`MailboxBinding`]: the mailbox public key signed by the owner's account
//! identity key. **Decoding a grant does not verify the binding** (the
//! decoder does not know the owner); a writer calls
//! [`ReachGrant::mailbox_pk_verified`] with the account key it holds pinned
//! for the owner, and writes only when that succeeds. A carrier that itself
//! names the owner's account key (a link capability, a contact request)
//! verifies it at decode; every other carrier leaves it to the writer.
//!
//! [ADR-0078]: ../../../docs/adr/0078-servers-cannot-link-accounts.md
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

use crate::mailbox::{mailbox_address, MailboxBinding, MailboxError, MAILBOX_BINDING_LEN};

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

/// Wire length of a [`ReachGrant`]: `class(1) || version(4, BE) || seed(32)
/// || binding(97)` = 134.
pub const REACH_GRANT_LEN: usize = 1 + 4 + REACH_SEED_LEN + MAILBOX_BINDING_LEN;

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
    /// Members of every group the owner lets find them. Empty scope: one
    /// key for all of them (ADR-0078 section 9).
    Group,
    /// Every co-member of every group, whatever the setting (device
    /// re-admission Welcomes). Empty scope.
    Member,
    /// Members of every channel the owner lets find them. Empty scope.
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

    /// `true` for the account-wide classes, whose scope id is empty: every
    /// class but `link` and `reply` (ADR-0078 section 9 made `group`,
    /// `member` and `channel` account-wide).
    #[must_use]
    pub const fn is_account_wide(self) -> bool {
        !matches!(self, Self::Link | Self::Reply)
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
    /// An account-wide class (every class but `link` and `reply`) was given
    /// a scope id.
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
    /// The grant's mailbox binding does not decode (layout or version).
    #[error("reach grant carries a malformed mailbox binding")]
    BadBinding,
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
            Self::BadBinding => "badBinding",
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

// =============================================================================
// Retired contact keys (maintainer decision, 2026-11-06)
// =============================================================================

/// How many retired `contact` key versions an owner keeps registered beside
/// the current one: 8. Chosen, not measured. Removing a contact rotates the
/// shared `contact` key; a contact offline across up to this many rotations
/// still writes under the grant it holds, and the owner's device then sends it
/// the current grant. A key older than that is no longer registered, so a
/// write under it is refused at the sender (the Delivery Service's uniform
/// `401`).
pub const RETIRED_CONTACT_VERSIONS_KEPT: u32 = 8;

/// The `contact` key versions the owner keeps registered at the Delivery
/// Service when the current version is `current`: `current` first, then each
/// retired one down to [`RETIRED_CONTACT_VERSIONS_KEPT`] below it (never
/// below 0). Register the public key of each (no expiry) and remove any
/// other `contact` version; every device of the owner computes the same set.
#[must_use]
pub fn contact_key_versions(current: u32) -> Vec<u32> {
    let oldest = current.saturating_sub(RETIRED_CONTACT_VERSIONS_KEPT);
    (oldest..=current).rev().collect()
}

/// What a mailbox row's proving key is, as far as the `contact` class goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactKeyStanding {
    /// The current `contact` key.
    Current,
    /// A retired `contact` key the owner still keeps registered.
    Retired {
        /// Its version.
        version: u32,
    },
    /// Not a `contact` key in the kept window (another class, or none): the
    /// rules of that class apply, this one does not.
    NotContactKey,
}

/// Classify `proving_key`, the reach public key the Delivery Service reports
/// for a mailbox row (`reach_pk_b64`), against the owner's `contact` keys of
/// [`contact_key_versions`]`(current_version)`.
#[must_use]
pub fn classify_contact_key(
    account_seed: &[u8; REACH_SEED_LEN],
    current_version: u32,
    proving_key: &[u8; REACH_SEED_LEN],
) -> ContactKeyStanding {
    for version in contact_key_versions(current_version) {
        let Ok(seed) = reach_seed(account_seed, ReachClass::Contact, &[], version) else {
            continue;
        };
        if &reach_verifying_key(seed.as_bytes()) == proving_key {
            return if version == current_version {
                ContactKeyStanding::Current
            } else {
                ContactKeyStanding::Retired { version }
            };
        }
    }
    ContactKeyStanding::NotContactKey
}

/// What the receiving device does with a mailbox row, by its proving key's
/// standing and its sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactKeyRowVerdict {
    /// Process the row as before.
    Process,
    /// Process the row, and send its sender the current `contact` grant in
    /// the one-to-one (republish the profile there), without anything shown.
    ProcessAndRegrant,
    /// Drop the row without anything shown; log nothing that names anyone.
    Drop,
}

/// The one rule for a row written under a retired `contact` key (maintainer
/// decision, 2026-11-06), so no client re-implements it. `sender_is_contact`
/// is whether the account the row's verified content names as its sender (a
/// contact request's from-key, a Welcome's founder binding, and so on) is a
/// CURRENT contact of the owner.
///
/// * a row under the current key, or under no `contact` key: [`ContactKeyRowVerdict::Process`];
/// * a row under a retired key from a current contact:
///   [`ContactKeyRowVerdict::ProcessAndRegrant`] (the contact was offline
///   across a rotation and holds an old grant);
/// * a row under a retired key from anyone else: [`ContactKeyRowVerdict::Drop`]
///   (a removed contact, or someone a contact passed the old key to).
///
/// No resend is asked of the sender: the Delivery Service accepted the row,
/// so the owner already holds it.
#[must_use]
pub const fn contact_key_row_verdict(
    standing: ContactKeyStanding,
    sender_is_contact: bool,
) -> ContactKeyRowVerdict {
    match standing {
        ContactKeyStanding::Current | ContactKeyStanding::NotContactKey => {
            ContactKeyRowVerdict::Process
        },
        ContactKeyStanding::Retired { .. } if sender_is_contact => {
            ContactKeyRowVerdict::ProcessAndRegrant
        },
        ContactKeyStanding::Retired { .. } => ContactKeyRowVerdict::Drop,
    }
}

/// A reach key handed to someone: its class, its version, the seed, and the
/// owner's mailbox binding (ADR-0078).
///
/// A `link` or `reply` grant's scope is implicit in where it travels (the
/// link it rides in, the request it answers); every other class has none.
/// So no scope is on the wire. `Debug` never prints the seed.
#[derive(Clone, PartialEq, Eq)]
pub struct ReachGrant {
    /// Who the key is for.
    pub class: ReachClass,
    /// The key version, bumped on every rotation.
    pub version: u32,
    /// The Ed25519 seed. Secret: whoever holds it can write to the mailbox.
    pub seed: SecretBytes<REACH_SEED_LEN>,
    /// The owner's mailbox binding: where the key writes to. Not verified on
    /// decode; see [`ReachGrant::mailbox_pk_verified`].
    pub binding: MailboxBinding,
}

impl fmt::Debug for ReachGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReachGrant")
            .field("class", &self.class)
            .field("version", &self.version)
            .field("seed", &"<redacted>")
            .field("binding", &self.binding)
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
            binding: MailboxBinding::for_account_seed(account_seed),
        })
    }

    /// The mailbox key this grant writes to, after verifying its binding
    /// against `account_pk`, the owner's account identity key as the writer
    /// holds it pinned. A writer calls this before every write and writes
    /// nothing when it fails: a binding signed by anyone else would route the
    /// write to a mailbox the owner does not read.
    ///
    /// # Errors
    ///
    /// Every error of [`MailboxBinding::verify`].
    pub fn mailbox_pk_verified(
        &self,
        account_pk: &[u8; REACH_SEED_LEN],
    ) -> CoreResult<[u8; REACH_SEED_LEN], MailboxError> {
        self.binding.verify(account_pk)
    }

    /// The mailbox address this grant writes to, after the same check as
    /// [`ReachGrant::mailbox_pk_verified`].
    ///
    /// # Errors
    ///
    /// Every error of [`MailboxBinding::verify`].
    pub fn mailbox_address_verified(
        &self,
        account_pk: &[u8; REACH_SEED_LEN],
    ) -> CoreResult<[u8; REACH_SEED_LEN], MailboxError> {
        Ok(mailbox_address(&self.mailbox_pk_verified(account_pk)?))
    }

    /// The public key the holder signs mailbox writes under.
    #[must_use]
    pub fn verifying_key(&self) -> [u8; REACH_SEED_LEN] {
        reach_verifying_key(self.seed.as_bytes())
    }

    /// Encode as `class(1) || version(4, BE) || seed(32) || binding(97)`.
    ///
    /// The output holds the seed; the caller seals it before it leaves the
    /// device.
    #[must_use]
    pub fn encode(&self) -> [u8; REACH_GRANT_LEN] {
        let mut out = [0u8; REACH_GRANT_LEN];
        out[0] = self.class.to_byte();
        out[1..5].copy_from_slice(&self.version.to_be_bytes());
        out[5..5 + REACH_SEED_LEN].copy_from_slice(self.seed.as_bytes());
        out[5 + REACH_SEED_LEN..].copy_from_slice(&self.binding.encode());
        out
    }

    /// Decode exactly [`REACH_GRANT_LEN`] bytes, fail-closed.
    ///
    /// # Errors
    ///
    /// * [`ReachError::BadLength`] — not exactly the grant length.
    /// * [`ReachError::UnknownClass`] — the class byte is not a class.
    /// * [`ReachError::BadBinding`] — the binding's layout is malformed (its
    ///   signature is not checked here).
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
            bytes[5..5 + REACH_SEED_LEN]
                .try_into()
                .map_err(|_| ReachError::BadLength {
                    actual: bytes.len(),
                })?;
        let binding = MailboxBinding::decode(&bytes[5 + REACH_SEED_LEN..])
            .map_err(|_| ReachError::BadBinding)?;
        Ok(Self {
            class,
            version: u32::from_be_bytes(version_bytes),
            seed: SecretBytes::new(seed),
            binding,
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
        let seed = reach_seed(&ACCOUNT, ReachClass::Link, b"link-1", 3).unwrap();
        assert_eq!(
            hex(seed.as_bytes()),
            "69e756d3faf95aae33944de2bdb27569fe890279b0817350e3980d23dd137cb6",
            "a change here changes every reach key of every account"
        );
        assert_eq!(
            hex(&reach_verifying_key(seed.as_bytes())),
            "f81b813554d8b791c378ffb6e836d2218f76ddf26e2e8d0be1209b0c2adac4d5"
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
        let base = reach_seed(&ACCOUNT, ReachClass::Link, b"g", 1).unwrap();
        let others = [
            reach_seed(&[0x43; 32], ReachClass::Link, b"g", 1).unwrap(),
            reach_seed(&ACCOUNT, ReachClass::Reply, b"g", 1).unwrap(),
            reach_seed(&ACCOUNT, ReachClass::Link, b"h", 1).unwrap(),
            reach_seed(&ACCOUNT, ReachClass::Link, b"g", 2).unwrap(),
            reach_seed(&ACCOUNT, ReachClass::Link, b"gg", 1).unwrap(),
        ];
        for other in &others {
            assert_ne!(&base, other);
        }
        assert_eq!(
            base,
            reach_seed(&ACCOUNT, ReachClass::Link, b"g", 1).unwrap(),
            "deterministic: every device derives the same key"
        );
    }

    #[test]
    fn group_member_and_channel_keys_carry_no_group_label() {
        // ADR-0078 section 9: one key per class per account. A group id can
        // no longer enter the derivation, so no two groups' co-members write
        // under two different keys.
        for class in [ReachClass::Group, ReachClass::Member, ReachClass::Channel] {
            assert!(class.is_account_wide());
            assert_eq!(
                reach_seed(&ACCOUNT, class, b"group-1", 0).err(),
                Some(ReachError::ScopeIdNotEmpty)
            );
            let a = reach_seed(&ACCOUNT, class, b"", 3).unwrap();
            assert_eq!(a, reach_seed(&ACCOUNT, class, b"", 3).unwrap());
            assert_ne!(a, reach_seed(&ACCOUNT, class, b"", 4).unwrap());
        }
        let group = reach_seed(&ACCOUNT, ReachClass::Group, b"", 0).unwrap();
        let member = reach_seed(&ACCOUNT, ReachClass::Member, b"", 0).unwrap();
        let channel = reach_seed(&ACCOUNT, ReachClass::Channel, b"", 0).unwrap();
        assert_ne!(group, member);
        assert_ne!(group, channel);
        assert_ne!(member, channel);
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
        for class in [ReachClass::Link, ReachClass::Reply] {
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

    /// 2026-11-06: the kept window is the current version and up to eight
    /// below it, every device computes the same one, and a row's key is
    /// classified within it.
    #[test]
    fn retired_contact_keys_are_kept_classified_and_judged() {
        assert_eq!(contact_key_versions(0), vec![0]);
        assert_eq!(contact_key_versions(3), vec![3, 2, 1, 0]);
        assert_eq!(
            contact_key_versions(20),
            (12..=20).rev().collect::<Vec<u32>>()
        );
        assert_eq!(
            contact_key_versions(u32::MAX).len(),
            usize::try_from(RETIRED_CONTACT_VERSIONS_KEPT).unwrap() + 1
        );
        let pk = |v: u32| {
            reach_verifying_key(
                reach_seed(&ACCOUNT, ReachClass::Contact, b"", v)
                    .unwrap()
                    .as_bytes(),
            )
        };
        assert_eq!(
            classify_contact_key(&ACCOUNT, 20, &pk(20)),
            ContactKeyStanding::Current
        );
        assert_eq!(
            classify_contact_key(&ACCOUNT, 20, &pk(12)),
            ContactKeyStanding::Retired { version: 12 }
        );
        // Past the window, and a key of another class or account.
        assert_eq!(
            classify_contact_key(&ACCOUNT, 20, &pk(11)),
            ContactKeyStanding::NotContactKey
        );
        assert_eq!(
            classify_contact_key(&ACCOUNT, 20, &pk(21)),
            ContactKeyStanding::NotContactKey
        );
        let fof = reach_verifying_key(
            reach_seed(&ACCOUNT, ReachClass::Fof, b"", 20)
                .unwrap()
                .as_bytes(),
        );
        assert_eq!(
            classify_contact_key(&ACCOUNT, 20, &fof),
            ContactKeyStanding::NotContactKey
        );
        assert_eq!(
            classify_contact_key(&[0x43; 32], 20, &pk(20)),
            ContactKeyStanding::NotContactKey
        );

        let retired = ContactKeyStanding::Retired { version: 3 };
        for sender_is_contact in [true, false] {
            assert_eq!(
                contact_key_row_verdict(ContactKeyStanding::Current, sender_is_contact),
                ContactKeyRowVerdict::Process
            );
            assert_eq!(
                contact_key_row_verdict(ContactKeyStanding::NotContactKey, sender_is_contact),
                ContactKeyRowVerdict::Process
            );
        }
        assert_eq!(
            contact_key_row_verdict(retired, true),
            ContactKeyRowVerdict::ProcessAndRegrant
        );
        assert_eq!(
            contact_key_row_verdict(retired, false),
            ContactKeyRowVerdict::Drop
        );
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
                assert_eq!(&wire[5..37], grant.seed.as_bytes());
                assert_eq!(&wire[37..], &grant.binding.encode());
                assert_eq!(ReachGrant::decode(&wire).unwrap(), grant);
            }
        }
        assert_eq!(REACH_GRANT_LEN, 134);
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
        let mut bad_binding = good;
        bad_binding[37] = 2;
        assert_eq!(
            ReachGrant::decode(&bad_binding),
            Err(ReachError::BadBinding)
        );
    }

    #[test]
    fn a_derived_grant_names_the_owners_mailbox_and_verifies_only_under_the_owner() {
        let grant = ReachGrant::derive(&ACCOUNT, ReachClass::Contact, b"", 0).unwrap();
        let owner = SigningKey::from_seed(&ACCOUNT).verifying_key().to_bytes();
        let mailbox = crate::mailbox::MailboxKey::from_account_seed(&ACCOUNT);
        assert_eq!(grant.mailbox_pk_verified(&owner), Ok(mailbox.public()));
        assert_eq!(
            grant.mailbox_address_verified(&owner),
            Ok(mailbox.address())
        );
        let stranger = SigningKey::from_seed(&[0x43; 32])
            .verifying_key()
            .to_bytes();
        assert_eq!(
            grant.mailbox_pk_verified(&stranger),
            Err(MailboxError::BadBinding)
        );
        // A grant whose binding was swapped for another account's decodes
        // (decode cannot know the owner) but does not verify under the owner.
        let mut swapped = grant.clone();
        swapped.binding = MailboxBinding::for_account_seed(&[0x43; 32]);
        let decoded = ReachGrant::decode(&swapped.encode()).unwrap();
        assert_eq!(
            decoded.mailbox_pk_verified(&owner),
            Err(MailboxError::BadBinding)
        );
    }

    #[test]
    fn debug_never_prints_the_seed() {
        let grant = ReachGrant {
            class: ReachClass::Group,
            version: 1,
            seed: SecretBytes::new([0xAB; 32]),
            binding: MailboxBinding::for_account_seed(&ACCOUNT),
        };
        let dbg = format!("{grant:?}");
        // The binding (a public key) is printed; the seed never is.
        assert!(!dbg.contains("abab") && !dbg.contains("ABAB") && !dbg.contains("171, 171"));
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
