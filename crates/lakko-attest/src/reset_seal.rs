// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! The reset seal ([ADR-0101] section 10, [ADR-0073] "Amended 2026-11-26
//! (second)").
//!
//! A key reset is accepted by a contact's device (ADR-0073 R.2) on three
//! things the operator controls: auth's key-reset attestation, the key
//! generation and the KT log entry. A **sealed** reset adds a fourth the
//! operator does not control: the new account key is signed by a **seal
//! key** the account announced, under its old account key, before the reset.
//! A contact's device moves conversations, history, the profile and channel
//! subscriptions only for a sealed reset; an unsealed one is a new account
//! under the same username (ADR-0101 section 11).
//!
//! Two formats live here, both client-signed and client-verified:
//!
//! - the **seal announcement** ([`SealAnnouncement`]), signed by the current
//!   account identity key, listing the account's seal public keys;
//! - the **reset seal** ([`ResetSeal`]), signed by one announced seal key,
//!   naming the new account key of the reset.
//!
//! ## Seal keys
//!
//! Each is an Ed25519 key whose seed comes from a secret the server never
//! holds:
//!
//! ```text
//! kind 1 password  seed = BLAKE3-derive-key("lakko/account/reset-seal/password/v1", export_key(64))
//! kind 2 passkey   seed = BLAKE3-derive-key("lakko/account/reset-seal/passkey/v1",  prf_output(32))
//! kind 3 phrase    seed = BLAKE3-derive-key("lakko/account/reset-seal/phrase/v1",   phrase_seed(64))
//! ```
//!
//! `export_key` is the 64-byte OPAQUE export key of the account's password
//! credential; `prf_output` is the passkey's WebAuthn `prf` output for the
//! salt [`passkey_prf_salt`] = `SHA-256("lakko/account/reset-seal/passkey-prf/v1")`;
//! `phrase_seed` is the 64-byte BIP39 seed. A passkey entry's `tag` is
//! [`passkey_seal_tag`], the first 16 bytes of `SHA-256(credential_id)`.
//!
//! ## The seal announcement
//!
//! ```text
//! wire = "lakko/account/reset-seals/v1\0" ‖ version(1)=0x01 ‖ account_ref(32)
//!        ‖ key_generation(8 BE) ‖ seq(8 BE) ‖ issued_at_ms(8 BE)
//!        ‖ count(1, 1..=10) ‖ count × (kind(1) ‖ tag(16) ‖ seal_pk(32))
//!        ‖ sig(64)
//! sig  = Ed25519 by the account identity key over every byte before it
//! ```
//!
//! At most [`SEAL_ANNOUNCEMENT_MAX_LEN`] = 641 bytes. Exactly one password
//! entry, at most eight passkey entries, at most one phrase entry; the tag
//! is all zero for kinds 1 and 3. **Chosen (2026-11-26, not in the ADR):**
//! the entries are strictly ascending by `(kind, tag)`, so an announcement
//! has one encoding and two passkey entries cannot share a tag; a seal key
//! that is not a valid Ed25519 point, or is of small order, is refused.
//!
//! ## The reset seal
//!
//! ```text
//! wire = "lakko/account/reset-seal/v1\0" ‖ version(1)=0x01 ‖ account_ref(32)
//!        ‖ announcement_seq(8 BE) ‖ from_generation(8 BE) ‖ to_generation(8 BE)
//!        ‖ new_signature_pubkey(32) ‖ kind(1) ‖ seal_pk(32) ‖ sig(64)
//! sig  = Ed25519 by seal_pk over every byte before it
//! ```
//!
//! Always [`RESET_SEAL_LEN`] = 214 bytes. **Chosen:** `to_generation` must be
//! exactly `from_generation + 1` (the resetting device signs over the next
//! generation, ADR-0101 section 10).
//!
//! The domain separators are on the wire (ADR-0101 counts them in its 641
//! bytes), unlike the auth-signed kinds of this crate; each is distinct from
//! every other separator of the crate and prefixes none.
//!
//! ## The peer's check
//!
//! [`check_reset_seal`] is ADR-0073 R.2 rule 4 without the KT-log half: the
//! previous announcement verifies under the account key this device holds,
//! names this account and the generation it accepted, and its `seq` is at
//! least the highest this device has seen; the reset seal names that account
//! and `seq`, `from` = the accepted generation, `to` = the attested
//! generation, and the fetched key, its key is listed under its kind, and
//! its signature verifies. The KT-log half ("the newest announcement leaf of
//! `g-1` before the reset leaf") is
//! `lakko_transparency::reset_seals`; the verdict that combines them is
//! `lakko_contacts::reset_verdict`.
//!
//! [ADR-0101]: ../../../docs/adr/0101-reaching-a-reset-account.md
//! [ADR-0073]: ../../../docs/adr/0073-account-key-reset.md

use lakko_crypto::hash;
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey, PUBLIC_KEY_LEN, SIGNATURE_LEN};
use thiserror::Error;

use crate::key_reset::ACCOUNT_REF_LEN;

/// Domain separator opening every seal announcement. Frozen.
pub const SEAL_ANNOUNCEMENT_DOMAIN: &[u8] = b"lakko/account/reset-seals/v1\0";

/// Domain separator opening every reset seal. Frozen.
pub const RESET_SEAL_DOMAIN: &[u8] = b"lakko/account/reset-seal/v1\0";

/// BLAKE3 derive-key context of the password seal key's seed. Frozen.
pub const SEAL_KEY_PASSWORD_CONTEXT: &str = "lakko/account/reset-seal/password/v1";

/// BLAKE3 derive-key context of a passkey seal key's seed. Frozen.
pub const SEAL_KEY_PASSKEY_CONTEXT: &str = "lakko/account/reset-seal/passkey/v1";

/// BLAKE3 derive-key context of the phrase seal key's seed. Frozen.
pub const SEAL_KEY_PHRASE_CONTEXT: &str = "lakko/account/reset-seal/phrase/v1";

/// The text whose SHA-256 is the WebAuthn `prf` salt a passkey seal key is
/// evaluated at. Frozen; distinct from the passkey vault's salt.
pub const PASSKEY_PRF_SALT_LABEL: &[u8] = b"lakko/account/reset-seal/passkey-prf/v1";

/// Wire version of both formats.
const VERSION: u8 = 1;

/// Length of an OPAQUE export key.
pub const EXPORT_KEY_LEN: usize = 64;
/// Length of a WebAuthn `prf` output.
pub const PRF_OUTPUT_LEN: usize = 32;
/// Length of a BIP39 seed.
pub const PHRASE_SEED_LEN: usize = 64;
/// Length of a seal entry's tag.
pub const SEAL_TAG_LEN: usize = 16;

/// Most passkey entries in one announcement.
pub const MAX_PASSKEY_SEALS: usize = 8;
/// Most entries in one announcement: one password, eight passkeys, one phrase.
pub const MAX_SEAL_ENTRIES: usize = 1 + MAX_PASSKEY_SEALS + 1;

const KEY_LEN: usize = PUBLIC_KEY_LEN;
const ENTRY_LEN: usize = 1 + SEAL_TAG_LEN + KEY_LEN;
const ANNOUNCEMENT_FIXED_LEN: usize =
    SEAL_ANNOUNCEMENT_DOMAIN.len() + 1 + ACCOUNT_REF_LEN + 8 + 8 + 8 + 1;

/// Shortest seal announcement (one entry).
pub const SEAL_ANNOUNCEMENT_MIN_LEN: usize = ANNOUNCEMENT_FIXED_LEN + ENTRY_LEN + SIGNATURE_LEN;
/// Longest seal announcement (ten entries): 641 bytes.
pub const SEAL_ANNOUNCEMENT_MAX_LEN: usize =
    ANNOUNCEMENT_FIXED_LEN + MAX_SEAL_ENTRIES * ENTRY_LEN + SIGNATURE_LEN;
/// Length of a reset seal: 214 bytes.
pub const RESET_SEAL_LEN: usize = RESET_SEAL_DOMAIN.len()
    + 1
    + ACCOUNT_REF_LEN
    + 8
    + 8
    + 8
    + KEY_LEN
    + 1
    + KEY_LEN
    + SIGNATURE_LEN;

const _: () = assert!(SEAL_ANNOUNCEMENT_MAX_LEN == 641);
const _: () = assert!(RESET_SEAL_LEN == 214);

/// What kind of secret a seal key comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SealKind {
    /// The account password's OPAQUE export key. Wire `1`.
    Password,
    /// A passkey's WebAuthn `prf` output. Wire `2`.
    Passkey,
    /// The recovery phrase's BIP39 seed. Wire `3`.
    Phrase,
}

impl SealKind {
    /// The wire byte. Frozen.
    #[must_use]
    pub const fn to_wire(self) -> u8 {
        match self {
            Self::Password => 1,
            Self::Passkey => 2,
            Self::Phrase => 3,
        }
    }

    /// Read a wire byte; anything else is `None`.
    #[must_use]
    pub const fn from_wire(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Password),
            2 => Some(Self::Passkey),
            3 => Some(Self::Phrase),
            _ => None,
        }
    }
}

/// Why a seal announcement or a reset seal did not decode, sign or verify.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum ResetSealError {
    /// The bytes are not a length this format can have.
    #[error("wrong length: {got} bytes")]
    BadLength {
        /// The length received.
        got: usize,
    },
    /// The bytes do not open with this format's domain separator.
    #[error("wrong domain separator")]
    BadDomain,
    /// The version byte is not `1`.
    #[error("unsupported version {0}")]
    BadVersion(u8),
    /// An entry's kind byte is not 1, 2 or 3.
    #[error("unknown seal kind {0}")]
    UnknownKind(u8),
    /// The entry count is 0, above ten, or does not match the length.
    #[error("entry count out of range")]
    BadCount,
    /// Not exactly one password entry, more than eight passkey entries or
    /// more than one phrase entry.
    #[error("entry kinds out of range")]
    BadEntries,
    /// A password or phrase entry carries a non-zero tag.
    #[error("a password or phrase entry carries a tag")]
    UnexpectedTag,
    /// The entries are not strictly ascending by kind and tag.
    #[error("entries out of order or repeated")]
    NotCanonical,
    /// A seal key is not a usable Ed25519 public key.
    #[error("unusable seal key")]
    BadKey,
    /// `to_generation` is not `from_generation + 1`.
    #[error("generations do not follow")]
    BadGenerations,
    /// The signature does not verify.
    #[error("signature invalid")]
    BadSignature,
    /// The signing key given does not match the seal key named.
    #[error("signing key does not match the named seal key")]
    KeyMismatch,
}

impl ResetSealError {
    /// Stable, language-neutral reason code. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::BadLength { .. } => "badLength",
            Self::BadDomain => "badDomain",
            Self::BadVersion(_) => "badVersion",
            Self::UnknownKind(_) => "unknownKind",
            Self::BadCount => "badCount",
            Self::BadEntries => "badEntries",
            Self::UnexpectedTag => "unexpectedTag",
            Self::NotCanonical => "notCanonical",
            Self::BadKey => "badKey",
            Self::BadGenerations => "badGenerations",
            Self::BadSignature => "badSignature",
            Self::KeyMismatch => "keyMismatch",
        }
    }
}

/// The password seal key from the account password's 64-byte OPAQUE export
/// key.
#[must_use]
pub fn password_seal_key(export_key: &[u8; EXPORT_KEY_LEN]) -> SigningKey {
    seal_key(SEAL_KEY_PASSWORD_CONTEXT, export_key)
}

/// A passkey seal key from the passkey's 32-byte WebAuthn `prf` output at
/// [`passkey_prf_salt`].
#[must_use]
pub fn passkey_seal_key(prf_output: &[u8; PRF_OUTPUT_LEN]) -> SigningKey {
    seal_key(SEAL_KEY_PASSKEY_CONTEXT, prf_output)
}

/// The phrase seal key from the recovery phrase's 64-byte BIP39 seed.
#[must_use]
pub fn phrase_seal_key(phrase_seed: &[u8; PHRASE_SEED_LEN]) -> SigningKey {
    seal_key(SEAL_KEY_PHRASE_CONTEXT, phrase_seed)
}

fn seal_key(context: &str, material: &[u8]) -> SigningKey {
    let mut seed = hash::blake3_derive_key_from_material(context, material);
    let key = SigningKey::from_seed(&seed);
    seed.fill(0);
    key
}

/// The WebAuthn `prf` salt a passkey seal key is evaluated at:
/// `SHA-256("lakko/account/reset-seal/passkey-prf/v1")`.
#[must_use]
pub fn passkey_prf_salt() -> [u8; 32] {
    hash::sha256(PASSKEY_PRF_SALT_LABEL)
}

/// A passkey entry's tag: the first 16 bytes of `SHA-256(credential_id)`.
#[must_use]
pub fn passkey_seal_tag(credential_id: &[u8]) -> [u8; SEAL_TAG_LEN] {
    let digest = hash::sha256(credential_id);
    let mut tag = [0u8; SEAL_TAG_LEN];
    tag.copy_from_slice(&digest[..SEAL_TAG_LEN]);
    tag
}

/// One announced seal key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SealEntry {
    /// Which secret the key comes from.
    pub kind: SealKind,
    /// [`passkey_seal_tag`] for a passkey, all zero otherwise.
    pub tag: [u8; SEAL_TAG_LEN],
    /// The seal public key.
    pub seal_pk: [u8; KEY_LEN],
}

/// A seal announcement's content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealAnnouncement {
    /// `key_reset_account_ref(account_id)`.
    pub account_ref: [u8; ACCOUNT_REF_LEN],
    /// The account's key generation the announcement is made at.
    pub key_generation: u64,
    /// Strictly increasing per account across generations.
    pub seq: u64,
    /// When the device made it, unix milliseconds (for display only).
    pub issued_at_ms: u64,
    /// The seal keys, strictly ascending by kind and tag.
    pub entries: Vec<SealEntry>,
}

fn usable_key(bytes: &[u8; KEY_LEN]) -> Result<VerifyingKey, ResetSealError> {
    let key = VerifyingKey::from_bytes(bytes).map_err(|_| ResetSealError::BadKey)?;
    if key.is_weak() {
        return Err(ResetSealError::BadKey);
    }
    Ok(key)
}

fn check_entries(entries: &[SealEntry]) -> Result<(), ResetSealError> {
    if entries.is_empty() || entries.len() > MAX_SEAL_ENTRIES {
        return Err(ResetSealError::BadCount);
    }
    let mut password = 0usize;
    let mut passkey = 0usize;
    let mut phrase = 0usize;
    for (i, entry) in entries.iter().enumerate() {
        match entry.kind {
            SealKind::Password => password += 1,
            SealKind::Passkey => passkey += 1,
            SealKind::Phrase => phrase += 1,
        }
        if entry.kind != SealKind::Passkey && entry.tag != [0u8; SEAL_TAG_LEN] {
            return Err(ResetSealError::UnexpectedTag);
        }
        if i > 0 {
            let prev = &entries[i - 1];
            if (prev.kind, prev.tag) >= (entry.kind, entry.tag) {
                return Err(ResetSealError::NotCanonical);
            }
        }
        usable_key(&entry.seal_pk)?;
    }
    if password != 1 || passkey > MAX_PASSKEY_SEALS || phrase > 1 {
        return Err(ResetSealError::BadEntries);
    }
    Ok(())
}

impl SealAnnouncement {
    /// The announcement with its entries sorted into the canonical order.
    #[must_use]
    pub fn new(
        account_ref: [u8; ACCOUNT_REF_LEN],
        key_generation: u64,
        seq: u64,
        issued_at_ms: u64,
        mut entries: Vec<SealEntry>,
    ) -> Self {
        entries.sort_by_key(|e| (e.kind, e.tag));
        Self {
            account_ref,
            key_generation,
            seq,
            issued_at_ms,
            entries,
        }
    }

    fn body(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(ANNOUNCEMENT_FIXED_LEN + self.entries.len() * ENTRY_LEN);
        out.extend_from_slice(SEAL_ANNOUNCEMENT_DOMAIN);
        out.push(VERSION);
        out.extend_from_slice(&self.account_ref);
        out.extend_from_slice(&self.key_generation.to_be_bytes());
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.extend_from_slice(&self.issued_at_ms.to_be_bytes());
        // `check_entries` bounds the count at ten before this is called.
        out.push(u8::try_from(self.entries.len()).unwrap_or(u8::MAX));
        for entry in &self.entries {
            out.push(entry.kind.to_wire());
            out.extend_from_slice(&entry.tag);
            out.extend_from_slice(&entry.seal_pk);
        }
        out
    }

    /// Sign with the account identity key.
    ///
    /// # Errors
    ///
    /// The entry rules of the module docs ([`ResetSealError::BadCount`],
    /// [`ResetSealError::BadEntries`], [`ResetSealError::UnexpectedTag`],
    /// [`ResetSealError::NotCanonical`], [`ResetSealError::BadKey`]).
    pub fn sign(&self, account_key: &SigningKey) -> Result<Vec<u8>, ResetSealError> {
        check_entries(&self.entries)?;
        let mut out = self.body();
        let sig = account_key.sign(&out);
        out.extend_from_slice(&sig.to_bytes());
        Ok(out)
    }

    /// Decode without checking the signature. Bounded, exact and
    /// fail-closed: every rule but the signature.
    ///
    /// # Errors
    ///
    /// Any [`ResetSealError`] but `BadSignature`, `BadGenerations` and
    /// `KeyMismatch`.
    pub fn decode_unverified(bytes: &[u8]) -> Result<Self, ResetSealError> {
        if bytes.len() < SEAL_ANNOUNCEMENT_MIN_LEN || bytes.len() > SEAL_ANNOUNCEMENT_MAX_LEN {
            return Err(ResetSealError::BadLength { got: bytes.len() });
        }
        let mut cur = Cursor::new(bytes);
        if cur.take(SEAL_ANNOUNCEMENT_DOMAIN.len())? != SEAL_ANNOUNCEMENT_DOMAIN {
            return Err(ResetSealError::BadDomain);
        }
        let version = cur.byte()?;
        if version != VERSION {
            return Err(ResetSealError::BadVersion(version));
        }
        let account_ref = cur.array::<ACCOUNT_REF_LEN>()?;
        let key_generation = cur.u64()?;
        let seq = cur.u64()?;
        let issued_at_ms = cur.u64()?;
        let count = usize::from(cur.byte()?);
        if count == 0
            || count > MAX_SEAL_ENTRIES
            || bytes.len() != ANNOUNCEMENT_FIXED_LEN + count * ENTRY_LEN + SIGNATURE_LEN
        {
            return Err(ResetSealError::BadCount);
        }
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let kind_byte = cur.byte()?;
            let kind =
                SealKind::from_wire(kind_byte).ok_or(ResetSealError::UnknownKind(kind_byte))?;
            let tag = cur.array::<SEAL_TAG_LEN>()?;
            let seal_pk = cur.array::<KEY_LEN>()?;
            entries.push(SealEntry { kind, tag, seal_pk });
        }
        check_entries(&entries)?;
        Ok(Self {
            account_ref,
            key_generation,
            seq,
            issued_at_ms,
            entries,
        })
    }

    /// Decode and verify the signature under the account identity key.
    ///
    /// # Errors
    ///
    /// [`Self::decode_unverified`]'s errors, then
    /// [`ResetSealError::BadSignature`].
    pub fn open(bytes: &[u8], account_key: &VerifyingKey) -> Result<Self, ResetSealError> {
        let claims = Self::decode_unverified(bytes)?;
        verify_trailing_sig(bytes, account_key)?;
        Ok(claims)
    }

    /// Whether `seal_pk` is announced under `kind`.
    #[must_use]
    pub fn lists(&self, kind: SealKind, seal_pk: &[u8; KEY_LEN]) -> bool {
        self.entries
            .iter()
            .any(|e| e.kind == kind && &e.seal_pk == seal_pk)
    }
}

/// A reset seal's content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResetSeal {
    /// `key_reset_account_ref(account_id)`.
    pub account_ref: [u8; ACCOUNT_REF_LEN],
    /// The `seq` of the announcement the seal key was listed in.
    pub announcement_seq: u64,
    /// The generation the reset leaves.
    pub from_generation: u64,
    /// The generation the reset reaches: `from_generation + 1`.
    pub to_generation: u64,
    /// The new account identity key the reset installs.
    pub new_signature_pubkey: [u8; KEY_LEN],
    /// The kind of the seal key that signs.
    pub kind: SealKind,
    /// The seal key that signs.
    pub seal_pk: [u8; KEY_LEN],
}

impl ResetSeal {
    fn body(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(RESET_SEAL_LEN - SIGNATURE_LEN);
        out.extend_from_slice(RESET_SEAL_DOMAIN);
        out.push(VERSION);
        out.extend_from_slice(&self.account_ref);
        out.extend_from_slice(&self.announcement_seq.to_be_bytes());
        out.extend_from_slice(&self.from_generation.to_be_bytes());
        out.extend_from_slice(&self.to_generation.to_be_bytes());
        out.extend_from_slice(&self.new_signature_pubkey);
        out.push(self.kind.to_wire());
        out.extend_from_slice(&self.seal_pk);
        out
    }

    /// Sign with the seal key whose public half is [`Self::seal_pk`].
    ///
    /// # Errors
    ///
    /// [`ResetSealError::KeyMismatch`] when `seal_key` is not that key,
    /// [`ResetSealError::BadGenerations`], [`ResetSealError::BadKey`].
    pub fn sign(&self, seal_key: &SigningKey) -> Result<[u8; RESET_SEAL_LEN], ResetSealError> {
        if seal_key.verifying_key().to_bytes() != self.seal_pk {
            return Err(ResetSealError::KeyMismatch);
        }
        self.check()?;
        let body = self.body();
        let sig = seal_key.sign(&body);
        let mut out = [0u8; RESET_SEAL_LEN];
        out[..body.len()].copy_from_slice(&body);
        out[body.len()..].copy_from_slice(&sig.to_bytes());
        Ok(out)
    }

    fn check(&self) -> Result<(), ResetSealError> {
        if self.from_generation.checked_add(1) != Some(self.to_generation) {
            return Err(ResetSealError::BadGenerations);
        }
        usable_key(&self.seal_pk)?;
        usable_key(&self.new_signature_pubkey)?;
        Ok(())
    }

    /// Decode without checking the signature.
    ///
    /// # Errors
    ///
    /// [`ResetSealError::BadLength`], [`ResetSealError::BadDomain`],
    /// [`ResetSealError::BadVersion`], [`ResetSealError::UnknownKind`],
    /// [`ResetSealError::BadGenerations`], [`ResetSealError::BadKey`].
    pub fn decode_unverified(bytes: &[u8]) -> Result<Self, ResetSealError> {
        if bytes.len() != RESET_SEAL_LEN {
            return Err(ResetSealError::BadLength { got: bytes.len() });
        }
        let mut cur = Cursor::new(bytes);
        if cur.take(RESET_SEAL_DOMAIN.len())? != RESET_SEAL_DOMAIN {
            return Err(ResetSealError::BadDomain);
        }
        let version = cur.byte()?;
        if version != VERSION {
            return Err(ResetSealError::BadVersion(version));
        }
        let account_ref = cur.array::<ACCOUNT_REF_LEN>()?;
        let announcement_seq = cur.u64()?;
        let from_generation = cur.u64()?;
        let to_generation = cur.u64()?;
        let new_signature_pubkey = cur.array::<KEY_LEN>()?;
        let kind_byte = cur.byte()?;
        let kind = SealKind::from_wire(kind_byte).ok_or(ResetSealError::UnknownKind(kind_byte))?;
        let seal_pk = cur.array::<KEY_LEN>()?;
        let seal = Self {
            account_ref,
            announcement_seq,
            from_generation,
            to_generation,
            new_signature_pubkey,
            kind,
            seal_pk,
        };
        seal.check()?;
        Ok(seal)
    }

    /// Decode and verify the signature under the seal key the seal names.
    /// Whether that key is announced is [`check_reset_seal`]'s question.
    ///
    /// # Errors
    ///
    /// [`Self::decode_unverified`]'s errors, then
    /// [`ResetSealError::BadSignature`].
    pub fn open(bytes: &[u8]) -> Result<Self, ResetSealError> {
        let seal = Self::decode_unverified(bytes)?;
        let key = usable_key(&seal.seal_pk)?;
        verify_trailing_sig(bytes, &key)?;
        Ok(seal)
    }
}

fn verify_trailing_sig(bytes: &[u8], key: &VerifyingKey) -> Result<(), ResetSealError> {
    let split = bytes
        .len()
        .checked_sub(SIGNATURE_LEN)
        .ok_or(ResetSealError::BadLength { got: bytes.len() })?;
    let (body, sig) = bytes.split_at(split);
    let sig = Signature::from_bytes(sig).map_err(|_| ResetSealError::BadSignature)?;
    key.verify(body, &sig)
        .map_err(|_| ResetSealError::BadSignature)
}

/// Why [`check_reset_seal`] found a reset unsealed. Every variant means the
/// same thing to the verdict (unsealed); the variant is for logs and tests.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum SealRefusal {
    /// No previous announcement was served.
    #[error("no previous seal announcement")]
    NoAnnouncement,
    /// No reset seal was served.
    #[error("no reset seal")]
    NoSeal,
    /// The previous announcement did not decode or verify under the held key.
    #[error("previous announcement invalid: {0}")]
    AnnouncementInvalid(ResetSealError),
    /// The announcement names another account.
    #[error("previous announcement names another account")]
    AnnouncementAccount,
    /// The announcement is not of the generation this device accepted.
    #[error("previous announcement is of another generation")]
    AnnouncementGeneration,
    /// The announcement's `seq` is below the highest this device has seen.
    #[error("previous announcement is older than one already seen")]
    AnnouncementRolledBack,
    /// The reset seal did not decode or verify.
    #[error("reset seal invalid: {0}")]
    SealInvalid(ResetSealError),
    /// The seal names another account, another announcement, other
    /// generations or another key than the reset.
    #[error("reset seal does not name this reset")]
    SealMismatch,
    /// The seal's key is not listed in the announcement under its kind.
    #[error("reset seal key is not announced")]
    SealKeyNotAnnounced,
}

impl SealRefusal {
    /// Stable, language-neutral reason code. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NoAnnouncement => "noAnnouncement",
            Self::NoSeal => "noSeal",
            Self::AnnouncementInvalid(_) => "announcementInvalid",
            Self::AnnouncementAccount => "announcementAccount",
            Self::AnnouncementGeneration => "announcementGeneration",
            Self::AnnouncementRolledBack => "announcementRolledBack",
            Self::SealInvalid(_) => "sealInvalid",
            Self::SealMismatch => "sealMismatch",
            Self::SealKeyNotAnnounced => "sealKeyNotAnnounced",
        }
    }
}

/// What a sealed reset was sealed with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SealedReset {
    /// The previous announcement's `seq`: the caller raises its kept floor
    /// for the account to at least this.
    pub announcement_seq: u64,
    /// The kind of seal key that signed.
    pub kind: SealKind,
}

/// The inputs of [`check_reset_seal`].
#[derive(Clone, Copy, Debug)]
pub struct SealCheckInput<'a> {
    /// The previous generation's announcement as served (`previous_reset_seals`),
    /// or `None` when none was.
    pub previous_announcement: Option<&'a [u8]>,
    /// The reset seal as served (`reset_seal`), or `None` when none was.
    pub reset_seal: Option<&'a [u8]>,
    /// The account key this device holds for the account (`K`).
    pub held_account_key: &'a [u8; KEY_LEN],
    /// `key_reset_account_ref(account_id)` of the account.
    pub account_ref: &'a [u8; ACCOUNT_REF_LEN],
    /// The generation this device accepted (`g-1`).
    pub accepted_generation: u64,
    /// The attested generation of the reset (`g`).
    pub attested_generation: u64,
    /// The fetched new account key.
    pub fetched_key: &'a [u8; KEY_LEN],
    /// The highest announcement `seq` this device has seen for the account
    /// (0 when none).
    pub seq_floor: u64,
}

/// ADR-0073 R.2 rule 4, without its KT-log half (see the module docs).
///
/// # Errors
///
/// A [`SealRefusal`] when the reset is unsealed.
pub fn check_reset_seal(input: &SealCheckInput<'_>) -> Result<SealedReset, SealRefusal> {
    let announcement_bytes = input
        .previous_announcement
        .ok_or(SealRefusal::NoAnnouncement)?;
    let seal_bytes = input.reset_seal.ok_or(SealRefusal::NoSeal)?;
    let held = VerifyingKey::from_bytes(input.held_account_key)
        .map_err(|_| SealRefusal::AnnouncementInvalid(ResetSealError::BadKey))?;
    let announcement = SealAnnouncement::open(announcement_bytes, &held)
        .map_err(SealRefusal::AnnouncementInvalid)?;
    if &announcement.account_ref != input.account_ref {
        return Err(SealRefusal::AnnouncementAccount);
    }
    if announcement.key_generation != input.accepted_generation {
        return Err(SealRefusal::AnnouncementGeneration);
    }
    if announcement.seq < input.seq_floor {
        return Err(SealRefusal::AnnouncementRolledBack);
    }
    let seal = ResetSeal::open(seal_bytes).map_err(SealRefusal::SealInvalid)?;
    if &seal.account_ref != input.account_ref
        || seal.announcement_seq != announcement.seq
        || seal.from_generation != input.accepted_generation
        || seal.to_generation != input.attested_generation
        || &seal.new_signature_pubkey != input.fetched_key
    {
        return Err(SealRefusal::SealMismatch);
    }
    if !announcement.lists(seal.kind, &seal.seal_pk) {
        return Err(SealRefusal::SealKeyNotAnnounced);
    }
    Ok(SealedReset {
        announcement_seq: announcement.seq,
        kind: seal.kind,
    })
}

/// A bounds-checked reader; every read past the end is `BadLength`.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], ResetSealError> {
        let end = self.at.checked_add(n).ok_or(ResetSealError::BadLength {
            got: self.bytes.len(),
        })?;
        let out = self
            .bytes
            .get(self.at..end)
            .ok_or(ResetSealError::BadLength {
                got: self.bytes.len(),
            })?;
        self.at = end;
        Ok(out)
    }

    fn byte(&mut self) -> Result<u8, ResetSealError> {
        Ok(self.take(1)?[0])
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ResetSealError> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    fn u64(&mut self) -> Result<u64, ResetSealError> {
        Ok(u64::from_be_bytes(self.array::<8>()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const REF: [u8; 32] = [0xAB; 32];

    fn account() -> SigningKey {
        SigningKey::from_seed(&[0x01; 32])
    }

    fn new_key() -> [u8; 32] {
        SigningKey::from_seed(&[0x02; 32])
            .verifying_key()
            .to_bytes()
    }

    fn pw_key() -> SigningKey {
        password_seal_key(&[0x10; 64])
    }

    fn pk_key(n: u8) -> SigningKey {
        passkey_seal_key(&[n; 32])
    }

    fn entry(kind: SealKind, tag: [u8; 16], key: &SigningKey) -> SealEntry {
        SealEntry {
            kind,
            tag,
            seal_pk: key.verifying_key().to_bytes(),
        }
    }

    fn announcement(generation: u64, seq: u64) -> SealAnnouncement {
        SealAnnouncement::new(
            REF,
            generation,
            seq,
            1_700_000_000_000,
            vec![
                entry(SealKind::Phrase, [0; 16], &phrase_seal_key(&[0x30; 64])),
                entry(
                    SealKind::Passkey,
                    passkey_seal_tag(b"cred-b"),
                    &pk_key(0x21),
                ),
                entry(SealKind::Password, [0; 16], &pw_key()),
                entry(
                    SealKind::Passkey,
                    passkey_seal_tag(b"cred-a"),
                    &pk_key(0x20),
                ),
            ],
        )
    }

    fn seal(seq: u64, from: u64, key: &SigningKey, kind: SealKind) -> [u8; RESET_SEAL_LEN] {
        ResetSeal {
            account_ref: REF,
            announcement_seq: seq,
            from_generation: from,
            to_generation: from + 1,
            new_signature_pubkey: new_key(),
            kind,
            seal_pk: key.verifying_key().to_bytes(),
        }
        .sign(key)
        .unwrap()
    }

    fn input<'a>(
        ann: &'a [u8],
        seal: &'a [u8],
        held: &'a [u8; 32],
        fetched: &'a [u8; 32],
    ) -> SealCheckInput<'a> {
        SealCheckInput {
            previous_announcement: Some(ann),
            reset_seal: Some(seal),
            held_account_key: held,
            account_ref: &REF,
            accepted_generation: 1,
            attested_generation: 2,
            fetched_key: fetched,
            seq_floor: 4,
        }
    }

    #[test]
    fn contexts_and_lengths_are_pinned() {
        assert_eq!(SEAL_ANNOUNCEMENT_DOMAIN, b"lakko/account/reset-seals/v1\0");
        assert_eq!(RESET_SEAL_DOMAIN, b"lakko/account/reset-seal/v1\0");
        assert_eq!(SEAL_ANNOUNCEMENT_MIN_LEN, 641 - 9 * 49);
        assert_eq!(SEAL_ANNOUNCEMENT_MAX_LEN, 641);
        assert_eq!(RESET_SEAL_LEN, 214);
        assert_eq!(
            passkey_prf_salt(),
            hash::sha256(b"lakko/account/reset-seal/passkey-prf/v1")
        );
        // The three seal keys of one input differ: the contexts separate them.
        let a = password_seal_key(&[7; 64]).verifying_key();
        let b = phrase_seal_key(&[7; 64]).verifying_key();
        let c = passkey_seal_key(&[7; 32]).verifying_key();
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(b, c);
        assert_eq!(
            password_seal_key(&[7; 64]).to_seed(),
            hash::blake3_derive_key_from_material("lakko/account/reset-seal/password/v1", &[7; 64])
        );
    }

    #[test]
    fn golden_pins_the_derivations_and_the_wire() {
        let hex = |b: &[u8]| {
            use std::fmt::Write as _;
            b.iter().fold(String::new(), |mut out, x| {
                let _ = write!(out, "{x:02x}");
                out
            })
        };
        assert_eq!(
            hex(&password_seal_key(&[0x10; 64]).verifying_key().to_bytes()),
            GOLDEN_PASSWORD_PK
        );
        assert_eq!(
            hex(&passkey_seal_key(&[0x20; 32]).verifying_key().to_bytes()),
            GOLDEN_PASSKEY_PK
        );
        assert_eq!(
            hex(&phrase_seal_key(&[0x30; 64]).verifying_key().to_bytes()),
            GOLDEN_PHRASE_PK
        );
        assert_eq!(hex(&passkey_seal_tag(b"cred-a")), GOLDEN_TAG);
        let wire = seal(5, 1, &pw_key(), SealKind::Password);
        assert_eq!(hex(&hash::blake3(&wire)), GOLDEN_SEAL_HASH);
        let ann = announcement(1, 5).sign(&account()).unwrap();
        assert_eq!(hex(&hash::blake3(&ann)), GOLDEN_ANNOUNCEMENT_HASH);
    }

    const GOLDEN_PASSWORD_PK: &str =
        "146b4c7ec044cf2f3507509479c1968a4c752eda60a857bc61b9bf4381e2cc5c";
    const GOLDEN_PASSKEY_PK: &str =
        "6878c2b639eb60c409e487ec177b401e1dcf8308731623153ca35be54c4da068";
    const GOLDEN_PHRASE_PK: &str =
        "22a824891cceef789f6afe9c0d1af10e88bb957e8cd34ee506f1a91d12aef2a1";
    const GOLDEN_TAG: &str = "dd722eebece24666ffa703d6678b8b05";
    const GOLDEN_SEAL_HASH: &str =
        "af127cc2a1bf481cdc733f15a571e16dc6d2ff4b60a955f87cfd98313fa86375";
    const GOLDEN_ANNOUNCEMENT_HASH: &str =
        "bb0a751c10b8503c9d780dbb17e7be916db62bc0ce6022caff6e52b13745f676";

    #[test]
    fn a_sealed_reset_passes_for_each_kind() {
        let ann = announcement(1, 5).sign(&account()).unwrap();
        let held = account().verifying_key().to_bytes();
        let fetched = new_key();
        for (key, kind) in [
            (pw_key(), SealKind::Password),
            (pk_key(0x20), SealKind::Passkey),
            (pk_key(0x21), SealKind::Passkey),
            (phrase_seal_key(&[0x30; 64]), SealKind::Phrase),
        ] {
            let s = seal(5, 1, &key, kind);
            assert_eq!(
                check_reset_seal(&input(&ann, &s, &held, &fetched)),
                Ok(SealedReset {
                    announcement_seq: 5,
                    kind
                })
            );
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn every_unsealed_case_is_refused() {
        let ann = announcement(1, 5).sign(&account()).unwrap();
        let held = account().verifying_key().to_bytes();
        let fetched = new_key();
        let good = seal(5, 1, &pw_key(), SealKind::Password);
        let base = input(&ann, &good, &held, &fetched);
        let check = |i: SealCheckInput<'_>| check_reset_seal(&i);

        assert_eq!(
            check(SealCheckInput {
                previous_announcement: None,
                ..base
            }),
            Err(SealRefusal::NoAnnouncement)
        );
        assert_eq!(
            check(SealCheckInput {
                reset_seal: None,
                ..base
            }),
            Err(SealRefusal::NoSeal)
        );
        // An announcement under another key (the operator's).
        let other = SigningKey::from_seed(&[0x66; 32]);
        let forged = announcement(1, 5).sign(&other).unwrap();
        assert_eq!(
            check(SealCheckInput {
                previous_announcement: Some(&forged),
                ..base
            }),
            Err(SealRefusal::AnnouncementInvalid(
                ResetSealError::BadSignature
            ))
        );
        let other_ref = SealCheckInput {
            account_ref: &[0xCD; 32],
            ..base
        };
        assert_eq!(check(other_ref), Err(SealRefusal::AnnouncementAccount));
        assert_eq!(
            check(SealCheckInput {
                accepted_generation: 2,
                attested_generation: 3,
                ..base
            }),
            Err(SealRefusal::AnnouncementGeneration)
        );
        assert_eq!(
            check(SealCheckInput {
                seq_floor: 6,
                ..base
            }),
            Err(SealRefusal::AnnouncementRolledBack)
        );
        // The seal: another seq, another from, another to, another key.
        let s = seal(4, 1, &pw_key(), SealKind::Password);
        assert_eq!(
            check(SealCheckInput {
                reset_seal: Some(&s),
                ..base
            }),
            Err(SealRefusal::SealMismatch)
        );
        assert_eq!(
            check(SealCheckInput {
                attested_generation: 3,
                ..base
            }),
            Err(SealRefusal::SealMismatch)
        );
        let wrong = SigningKey::from_seed(&[0x09; 32])
            .verifying_key()
            .to_bytes();
        assert_eq!(
            check(SealCheckInput {
                fetched_key: &wrong,
                ..base
            }),
            Err(SealRefusal::SealMismatch)
        );
        // A seal key the announcement does not list (an older password, a
        // removed passkey), or listed under another kind.
        let s = seal(5, 1, &password_seal_key(&[0x11; 64]), SealKind::Password);
        assert_eq!(
            check(SealCheckInput {
                reset_seal: Some(&s),
                ..base
            }),
            Err(SealRefusal::SealKeyNotAnnounced)
        );
        let s = seal(5, 1, &pk_key(0x20), SealKind::Phrase);
        assert_eq!(
            check(SealCheckInput {
                reset_seal: Some(&s),
                ..base
            }),
            Err(SealRefusal::SealKeyNotAnnounced)
        );
        // A tampered seal.
        let mut bad = good;
        bad[RESET_SEAL_LEN - 1] ^= 1;
        assert_eq!(
            check(SealCheckInput {
                reset_seal: Some(&bad),
                ..base
            }),
            Err(SealRefusal::SealInvalid(ResetSealError::BadSignature))
        );
    }

    #[test]
    fn announcement_rules_are_enforced() {
        let pw = entry(SealKind::Password, [0; 16], &pw_key());
        let with = |entries: Vec<SealEntry>| SealAnnouncement {
            account_ref: REF,
            key_generation: 1,
            seq: 1,
            issued_at_ms: 0,
            entries,
        };
        assert_eq!(with(vec![]).sign(&account()), Err(ResetSealError::BadCount));
        assert_eq!(
            with(vec![entry(SealKind::Phrase, [0; 16], &pw_key())]).sign(&account()),
            Err(ResetSealError::BadEntries)
        );
        assert_eq!(
            with(vec![pw, pw]).sign(&account()),
            Err(ResetSealError::NotCanonical)
        );
        assert_eq!(
            with(vec![entry(SealKind::Password, [1; 16], &pw_key())]).sign(&account()),
            Err(ResetSealError::UnexpectedTag)
        );
        let passkeys: Vec<SealEntry> = (0u8..9)
            .map(|n| entry(SealKind::Passkey, [n; 16], &pk_key(n)))
            .collect();
        let mut nine = vec![pw];
        nine.extend(passkeys);
        assert_eq!(with(nine).sign(&account()), Err(ResetSealError::BadEntries));
        let mut weak = [0u8; 32];
        weak[0] = 1;
        assert_eq!(
            with(vec![SealEntry {
                kind: SealKind::Password,
                tag: [0; 16],
                seal_pk: weak
            }])
            .sign(&account()),
            Err(ResetSealError::BadKey)
        );
        // Out of order on the wire is refused by decode.
        let a = announcement(1, 5);
        let wire = a.sign(&account()).unwrap();
        assert_eq!(SealAnnouncement::decode_unverified(&wire), Ok(a));
        let max: Vec<SealEntry> = std::iter::once(pw)
            .chain((0u8..8).map(|n| entry(SealKind::Passkey, [n; 16], &pk_key(n))))
            .chain(std::iter::once(entry(
                SealKind::Phrase,
                [0; 16],
                &phrase_seal_key(&[1; 64]),
            )))
            .collect();
        assert_eq!(
            with(max).sign(&account()).unwrap().len(),
            SEAL_ANNOUNCEMENT_MAX_LEN
        );
    }

    #[test]
    fn reset_seal_rules_are_enforced() {
        let mut s =
            ResetSeal::decode_unverified(&seal(5, 1, &pw_key(), SealKind::Password)).unwrap();
        assert_eq!(s.sign(&pk_key(1)), Err(ResetSealError::KeyMismatch));
        s.to_generation = 3;
        assert_eq!(s.sign(&pw_key()), Err(ResetSealError::BadGenerations));
        s.from_generation = u64::MAX;
        s.to_generation = 0;
        assert_eq!(s.sign(&pw_key()), Err(ResetSealError::BadGenerations));
        assert_eq!(
            ResetSeal::open(&[0; 10]),
            Err(ResetSealError::BadLength { got: 10 })
        );
        let mut wire = seal(5, 1, &pw_key(), SealKind::Password);
        wire[0] ^= 1;
        assert_eq!(ResetSeal::open(&wire), Err(ResetSealError::BadDomain));
        let mut wire = seal(5, 1, &pw_key(), SealKind::Password);
        wire[RESET_SEAL_DOMAIN.len()] = 2;
        assert_eq!(ResetSeal::open(&wire), Err(ResetSealError::BadVersion(2)));
    }

    #[test]
    fn neither_format_opens_as_the_other() {
        let ann = announcement(1, 5).sign(&account()).unwrap();
        let s = seal(5, 1, &pw_key(), SealKind::Password);
        assert!(ResetSeal::open(&ann).is_err());
        assert!(SealAnnouncement::decode_unverified(&s).is_err());
    }

    #[test]
    fn fuzz_entry_points_hold_on_genuine_input() {
        let ann = announcement(1, 5).sign(&account()).unwrap();
        assert!(!crate::fuzz_api::open_reset_seals(&ann));
        let s = seal(5, 1, &pw_key(), SealKind::Password);
        assert!(crate::fuzz_api::open_reset_seal(&s));
        assert!(!crate::fuzz_api::open_reset_seal(&ann));
    }

    proptest! {
        #[test]
        fn decode_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..700)) {
            let _ = SealAnnouncement::decode_unverified(&bytes);
            let _ = ResetSeal::decode_unverified(&bytes);
            let _ = ResetSeal::open(&bytes);
        }

        #[test]
        fn announcement_round_trips_and_any_flip_is_refused(
            seq in any::<u64>(), generation in any::<u64>(), passkeys in 0usize..=8, phrase in any::<bool>(), flip in any::<prop::sample::Index>()
        ) {
            let mut entries = vec![entry(SealKind::Password, [0; 16], &pw_key())];
            for n in 0..passkeys {
                let n = u8::try_from(n).unwrap();
                entries.push(entry(SealKind::Passkey, passkey_seal_tag(&[n]), &pk_key(n)));
            }
            if phrase {
                entries.push(entry(SealKind::Phrase, [0; 16], &phrase_seal_key(&[9; 64])));
            }
            let a = SealAnnouncement::new(REF, generation, seq, 1, entries);
            let wire = a.sign(&account()).unwrap();
            let vk = account().verifying_key();
            prop_assert_eq!(SealAnnouncement::open(&wire, &vk), Ok(a));
            let mut bad = wire.clone();
            let i = flip.index(bad.len());
            bad[i] ^= 1;
            prop_assert!(SealAnnouncement::open(&bad, &vk).is_err());
        }

        #[test]
        fn seal_round_trips_and_any_flip_is_refused(seq in any::<u64>(), from in 0u64..u64::MAX, flip in any::<prop::sample::Index>()) {
            let wire = seal(seq, from, &pw_key(), SealKind::Password);
            let s = ResetSeal::open(&wire).unwrap();
            prop_assert_eq!(s.announcement_seq, seq);
            prop_assert_eq!(s.from_generation, from);
            let mut bad = wire;
            let i = flip.index(bad.len());
            bad[i] ^= 1;
            prop_assert!(ResetSeal::open(&bad).is_err());
        }
    }
}
