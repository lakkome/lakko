// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Sealed sender for personal mailboxes ([ADR-0078] section 1, "Phase 1 as
//! designed").
//!
//! ## Why
//!
//! A person's mailbox on the Delivery Service receives contact requests,
//! contact acks and Welcomes. Before ADR-0078 the mailbox was named after
//! the account's identity key (so the server could join it to discovery's
//! identity bundles), the request carried its sender's identity key and
//! account id outside its seal, the ack was not sealed at all, and every row
//! led with a clear kind byte. The server could read who wrote to whom.
//!
//! This module removes all of that from what the server sees:
//!
//! * **A mailbox key that is not the identity key.** Every device of an
//!   account derives the same Ed25519 key pair from the account seed
//!   ([`mailbox_seed`]); it signs the claim, every read and every reach-key
//!   registration, and its X25519 image is what writers seal to. It changes
//!   with the account seed (an ADR-0073 reset, an ADR-0074 rotation).
//! * **An address that is not a function of the identity key.** The mailbox
//!   is addressed as [`mailbox_address`] of the mailbox public key, so the
//!   server's mailbox table no longer joins to anything that names a person.
//! * **A binding a writer verifies.** A writer must learn that a mailbox key
//!   belongs to the person it means to reach, or a member of a shared group
//!   could hand out its own mailbox under that person's name and swallow
//!   their Welcomes. [`MailboxBinding`] is the account identity key's
//!   signature over the mailbox key; it travels with every mailbox key, and
//!   a writer verifies it against the account key it holds pinned **before**
//!   it writes ([`MailboxBinding::verify`]). Decoding a binding does not
//!   verify it: a decoder does not know which account key to expect.
//! * **One sealed envelope for every row.** [`seal_mailbox_row`] seals
//!   `kind ‖ body_len ‖ body ‖ zeros` to the mailbox key, padded to a size
//!   bucket, so the server sees the mailbox, a size bucket and the reach
//!   proof it already required, and nothing about the kind or the sender.
//!
//! ## Wire formats
//!
//! ```text
//! mailbox_seed    = BLAKE3-derive-key("lakko/mailbox/v1", account_seed(32))
//! mailbox_key     = Ed25519 key pair from mailbox_seed
//! mailbox_address = BLAKE3-derive-key("lakko/mailbox/address/v1", mailbox_pk(32))
//!
//! binding (97)    = version(1)=0x01 ‖ mailbox_pk(32) ‖ sig(64)
//!   sig = Ed25519_identity("lakko/mailbox/binding/v1\0" ‖ mailbox_pk)
//!
//! row             = version(1)=0x01 ‖ eph_x25519_pk(32) ‖ AEAD(plaintext) (+16 tag)
//!   plaintext     = kind(1) ‖ body_len(4 BE) ‖ body ‖ zeros, length = a bucket
//!   AAD           = "lakko/mailbox-envelope/v1\0" ‖ version ‖ mailbox_address
//! ```
//!
//! The AEAD is the [`crate::envelope`] sealed box (ephemeral X25519, HKDF-SHA-256,
//! ChaCha20-Poly1305) under its own HKDF labels, so a mailbox row and a
//! contact request's inner seal can never be opened as each other.
//!
//! **Buckets** ([`bucket_for`]): powers of two from 1 KiB to 64 KiB, then
//! 64 KiB steps up to the largest bucket whose row fits the Delivery
//! Service's 1 MiB message cap ([`MAILBOX_MAX_BUCKET`]). Chosen, not
//! measured. A Welcome, a request and an ack of similar size land in one
//! bucket and are told apart by nothing the server sees.
//!
//! **Kinds** ([`MailboxKind`]): `0x01` contact request, `0x02` Welcome,
//! `0x03` contact ack, `0x04` Welcome rendezvous. An unknown kind is refused
//! by [`open_mailbox_row`] and the caller drops the row. The inner request
//! and ack wires are unchanged, so their signature checks do not move.
//!
//! [ADR-0078]: ../../../docs/adr/0078-servers-cannot-link-accounts.md

use core::fmt;
use core::result::Result as CoreResult;

use lakko_crypto::agreement::SecretKey as X25519SecretKey;
use lakko_crypto::hash::blake3_derive_key_from_material;
use lakko_crypto::signing::{Signature, SigningKey, VerifyingKey};
use lakko_crypto::SecretBytes;
use thiserror::Error;

use crate::envelope::{self, SealLabels, EPH_PUB_LEN, TAG_LEN};

/// BLAKE3 derive-key context for the mailbox seed. Frozen: changing it moves
/// every mailbox of every account.
pub const MAILBOX_SEED_DOMAIN: &str = "lakko/mailbox/v1";

/// BLAKE3 derive-key context for the mailbox address. Frozen.
pub const MAILBOX_ADDRESS_DOMAIN: &str = "lakko/mailbox/address/v1";

/// Domain separator the identity key signs a mailbox key under. Frozen.
pub const MAILBOX_BINDING_DOMAIN: &[u8] = b"lakko/mailbox/binding/v1\x00";

/// Domain separator that opens a mailbox row's AAD. Frozen.
pub const MAILBOX_ENVELOPE_AAD_DOMAIN: &[u8] = b"lakko/mailbox-envelope/v1\x00";

/// Length of an account seed, a mailbox seed, a mailbox public key and a
/// mailbox address.
pub const MAILBOX_KEY_LEN: usize = 32;

/// Binding schema version.
pub const MAILBOX_BINDING_VERSION_V1: u8 = 0x01;

/// Ed25519 signature length.
const SIGNATURE_LEN: usize = 64;

/// Wire length of a [`MailboxBinding`]: `version(1) ‖ mailbox_pk(32) ‖ sig(64)`.
pub const MAILBOX_BINDING_LEN: usize = 1 + MAILBOX_KEY_LEN + SIGNATURE_LEN;

/// Mailbox row schema version.
pub const MAILBOX_ROW_VERSION_V1: u8 = 0x01;

/// What a row adds around its padded plaintext: version, ephemeral key, tag.
pub const MAILBOX_ROW_OVERHEAD: usize = 1 + EPH_PUB_LEN + TAG_LEN;

/// `kind(1) ‖ body_len(4)` at the head of the plaintext.
pub const MAILBOX_PLAINTEXT_HEADER_LEN: usize = 1 + 4;

/// The smallest bucket.
pub const MAILBOX_MIN_BUCKET: usize = 1024;

/// The largest power-of-two bucket; above it buckets grow in
/// [`MAILBOX_BUCKET_STEP`] steps.
pub const MAILBOX_POW2_MAX_BUCKET: usize = 65_536;

/// The step between buckets above [`MAILBOX_POW2_MAX_BUCKET`].
pub const MAILBOX_BUCKET_STEP: usize = 65_536;

/// The longest row the Delivery Service stores (`services/ds`
/// `MAX_MESSAGE_BYTES`, 1 MiB).
pub const MAILBOX_MAX_ROW_LEN: usize = 1024 * 1024;

/// The largest bucket: the largest multiple of [`MAILBOX_BUCKET_STEP`]
/// whose row fits [`MAILBOX_MAX_ROW_LEN`] (983 040).
pub const MAILBOX_MAX_BUCKET: usize =
    (MAILBOX_MAX_ROW_LEN - MAILBOX_ROW_OVERHEAD) / MAILBOX_BUCKET_STEP * MAILBOX_BUCKET_STEP;

/// The longest body a row carries.
pub const MAILBOX_MAX_BODY_LEN: usize = MAILBOX_MAX_BUCKET - MAILBOX_PLAINTEXT_HEADER_LEN;

/// The shortest row: the smallest bucket.
pub const MAILBOX_MIN_ROW_LEN: usize = MAILBOX_MIN_BUCKET + MAILBOX_ROW_OVERHEAD;

/// The HKDF labels of the mailbox seal (distinct from the contact request's).
const MAILBOX_SEAL_LABELS: SealLabels = SealLabels {
    key: b"lakko/mailbox-envelope/v1/key",
    nonce: b"lakko/mailbox-envelope/v1/nonce",
};

/// Why a mailbox operation was refused. A reason code; never a key byte or
/// a body byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MailboxError {
    /// A binding or a row has a length its format does not allow.
    #[error("mailbox wire has a length its format does not allow")]
    BadLength,
    /// A binding or a row carries an unknown schema version.
    #[error("unknown mailbox wire version {0:#04x}")]
    UnknownVersion(u8),
    /// The mailbox key is not a valid Ed25519 public key, or is of small
    /// order (its X25519 image would give every writer the same shared
    /// secret).
    #[error("mailbox key is not a usable public key")]
    BadKey,
    /// The binding's signature does not verify under the account key given.
    #[error("mailbox binding does not verify under this account key")]
    BadBinding,
    /// A body longer than [`MAILBOX_MAX_BODY_LEN`].
    #[error("mailbox body too long")]
    BodyTooLong,
    /// The row did not open under this mailbox key (another mailbox, a
    /// tampered row, a wrong address).
    #[error("mailbox row did not open")]
    NotOpened,
    /// The opened plaintext names a kind this version does not know.
    #[error("unknown mailbox row kind {0:#04x}")]
    UnknownKind(u8),
    /// The opened plaintext's length field or padding is malformed.
    #[error("mailbox row padding is malformed")]
    BadPadding,
}

impl MailboxError {
    /// Stable, language-neutral reason code for FFI surfaces. Frozen.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::BadLength => "badLength",
            Self::UnknownVersion(_) => "unknownVersion",
            Self::BadKey => "badKey",
            Self::BadBinding => "badBinding",
            Self::BodyTooLong => "bodyTooLong",
            Self::NotOpened => "notOpened",
            Self::UnknownKind(_) => "unknownKind",
            Self::BadPadding => "badPadding",
        }
    }
}

/// What a mailbox row carries. The byte values are frozen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MailboxKind {
    /// A signed `ContactRequest` ([`crate::request`]).
    ContactRequest,
    /// An MLS Welcome.
    Welcome,
    /// A signed `ContactAck` ([`crate::ack`]).
    ContactAck,
    /// A Welcome rendezvous (the channel co-publisher join).
    WelcomeRendezvous,
}

impl MailboxKind {
    /// Every kind, in wire-byte order.
    pub const ALL: [Self; 4] = [
        Self::ContactRequest,
        Self::Welcome,
        Self::ContactAck,
        Self::WelcomeRendezvous,
    ];

    /// The frozen wire byte.
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        match self {
            Self::ContactRequest => 0x01,
            Self::Welcome => 0x02,
            Self::ContactAck => 0x03,
            Self::WelcomeRendezvous => 0x04,
        }
    }

    /// Parse a wire byte, fail-closed.
    ///
    /// # Errors
    ///
    /// [`MailboxError::UnknownKind`] for any other byte.
    pub const fn from_byte(byte: u8) -> CoreResult<Self, MailboxError> {
        match byte {
            0x01 => Ok(Self::ContactRequest),
            0x02 => Ok(Self::Welcome),
            0x03 => Ok(Self::ContactAck),
            0x04 => Ok(Self::WelcomeRendezvous),
            other => Err(MailboxError::UnknownKind(other)),
        }
    }
}

/// The mailbox seed of an account: `BLAKE3-derive-key("lakko/mailbox/v1",
/// account_seed)`. `account_seed` is the 32-byte Ed25519 seed of the account
/// identity, the same input the reach seeds take ([`crate::reach`]), so every
/// device of the account derives the same mailbox.
#[must_use]
pub fn mailbox_seed(account_seed: &[u8; MAILBOX_KEY_LEN]) -> SecretBytes<MAILBOX_KEY_LEN> {
    SecretBytes::new(blake3_derive_key_from_material(
        MAILBOX_SEED_DOMAIN,
        account_seed,
    ))
}

/// The address a mailbox is stored under on the Delivery Service:
/// `BLAKE3-derive-key("lakko/mailbox/address/v1", mailbox_pk)`.
#[must_use]
pub fn mailbox_address(mailbox_pk: &[u8; MAILBOX_KEY_LEN]) -> [u8; MAILBOX_KEY_LEN] {
    blake3_derive_key_from_material(MAILBOX_ADDRESS_DOMAIN, mailbox_pk)
}

/// Parse a mailbox public key and refuse a small-order one.
fn usable_key(mailbox_pk: &[u8; MAILBOX_KEY_LEN]) -> CoreResult<VerifyingKey, MailboxError> {
    let key = VerifyingKey::from_bytes(mailbox_pk).map_err(|_| MailboxError::BadKey)?;
    if key.is_weak() {
        return Err(MailboxError::BadKey);
    }
    Ok(key)
}

/// An account's mailbox key pair. `Debug` never prints the seed.
#[derive(Clone)]
pub struct MailboxKey {
    signing: SigningKey,
}

impl fmt::Debug for MailboxKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MailboxKey")
            .field("public", &self.public())
            .finish_non_exhaustive()
    }
}

impl MailboxKey {
    /// The mailbox key of the account whose identity seed is `account_seed`.
    #[must_use]
    pub fn from_account_seed(account_seed: &[u8; MAILBOX_KEY_LEN]) -> Self {
        Self::from_mailbox_seed(mailbox_seed(account_seed).as_bytes())
    }

    /// The mailbox key from an already derived [`mailbox_seed`].
    #[must_use]
    pub fn from_mailbox_seed(seed: &[u8; MAILBOX_KEY_LEN]) -> Self {
        Self {
            signing: SigningKey::from_seed(seed),
        }
    }

    /// The 32-byte Ed25519 seed. Secret: whoever holds it reads the mailbox.
    #[must_use]
    pub fn seed(&self) -> SecretBytes<MAILBOX_KEY_LEN> {
        SecretBytes::new(self.signing.to_seed())
    }

    /// The Ed25519 public key: what the claim registers and the binding names.
    #[must_use]
    pub fn public(&self) -> [u8; MAILBOX_KEY_LEN] {
        self.signing.verifying_key().to_bytes()
    }

    /// The address this mailbox is stored under.
    #[must_use]
    pub fn address(&self) -> [u8; MAILBOX_KEY_LEN] {
        mailbox_address(&self.public())
    }

    /// The signing key (the Delivery Service proof signer uses it).
    #[must_use]
    pub const fn signing_key(&self) -> &SigningKey {
        &self.signing
    }

    /// The X25519 secret rows are opened with (the birational image of the
    /// Ed25519 key, `lakko-crypto` `to_x25519_secret`).
    #[must_use]
    pub fn x25519_secret(&self) -> X25519SecretKey {
        self.signing.to_x25519_secret()
    }
}

/// The account identity key's signature over a mailbox key: what tells a
/// writer that the mailbox belongs to the person it means to reach.
///
/// [`MailboxBinding::decode`] checks the layout only; [`MailboxBinding::verify`]
/// checks the signature against an account key the caller trusts, and a
/// writer must call it before it seals or writes anything to the mailbox.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct MailboxBinding {
    mailbox_pk: [u8; MAILBOX_KEY_LEN],
    signature: [u8; SIGNATURE_LEN],
}

impl fmt::Debug for MailboxBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MailboxBinding")
            .field("mailbox_pk", &self.mailbox_pk)
            .finish_non_exhaustive()
    }
}

/// The bytes the identity key signs.
fn binding_signed_bytes(mailbox_pk: &[u8; MAILBOX_KEY_LEN]) -> [u8; 57] {
    let mut out = [0u8; 57];
    out[..MAILBOX_BINDING_DOMAIN.len()].copy_from_slice(MAILBOX_BINDING_DOMAIN);
    out[MAILBOX_BINDING_DOMAIN.len()..].copy_from_slice(mailbox_pk);
    out
}

const _: () = assert!(MAILBOX_BINDING_DOMAIN.len() + MAILBOX_KEY_LEN == 57);

impl MailboxBinding {
    /// Sign `mailbox_pk` with the account identity key.
    #[must_use]
    pub fn sign(identity: &SigningKey, mailbox_pk: &[u8; MAILBOX_KEY_LEN]) -> Self {
        Self {
            mailbox_pk: *mailbox_pk,
            signature: identity.sign(&binding_signed_bytes(mailbox_pk)).to_bytes(),
        }
    }

    /// The binding of the account whose identity seed is `account_seed`: its
    /// mailbox key signed by its identity key. Deterministic (Ed25519
    /// signatures are), so every device produces the same bytes.
    #[must_use]
    pub fn for_account_seed(account_seed: &[u8; MAILBOX_KEY_LEN]) -> Self {
        let identity = SigningKey::from_seed(account_seed);
        Self::sign(
            &identity,
            &MailboxKey::from_account_seed(account_seed).public(),
        )
    }

    /// The mailbox key this binding names, **not verified**. A writer uses
    /// [`Self::verify`]; this is for a caller that only forwards the binding.
    #[must_use]
    pub const fn unverified_mailbox_pk(&self) -> &[u8; MAILBOX_KEY_LEN] {
        &self.mailbox_pk
    }

    /// Encode as `version ‖ mailbox_pk ‖ sig`.
    #[must_use]
    pub fn encode(&self) -> [u8; MAILBOX_BINDING_LEN] {
        let mut out = [0u8; MAILBOX_BINDING_LEN];
        out[0] = MAILBOX_BINDING_VERSION_V1;
        out[1..=MAILBOX_KEY_LEN].copy_from_slice(&self.mailbox_pk);
        out[1 + MAILBOX_KEY_LEN..].copy_from_slice(&self.signature);
        out
    }

    /// Decode exactly [`MAILBOX_BINDING_LEN`] bytes. Layout only: the
    /// signature is not checked, since the decoder does not know which
    /// account key to expect.
    ///
    /// # Errors
    ///
    /// [`MailboxError::BadLength`], [`MailboxError::UnknownVersion`].
    pub fn decode(bytes: &[u8]) -> CoreResult<Self, MailboxError> {
        if bytes.len() != MAILBOX_BINDING_LEN {
            return Err(MailboxError::BadLength);
        }
        if bytes[0] != MAILBOX_BINDING_VERSION_V1 {
            return Err(MailboxError::UnknownVersion(bytes[0]));
        }
        let mailbox_pk: [u8; MAILBOX_KEY_LEN] = bytes[1..=MAILBOX_KEY_LEN]
            .try_into()
            .map_err(|_| MailboxError::BadLength)?;
        let signature: [u8; SIGNATURE_LEN] = bytes[1 + MAILBOX_KEY_LEN..]
            .try_into()
            .map_err(|_| MailboxError::BadLength)?;
        Ok(Self {
            mailbox_pk,
            signature,
        })
    }

    /// Verify the binding against the account identity key the caller holds
    /// pinned, and return the mailbox key it names.
    ///
    /// # Errors
    ///
    /// * [`MailboxError::BadKey`] — the account key or the mailbox key is not
    ///   a usable public key.
    /// * [`MailboxError::BadBinding`] — the signature does not verify.
    pub fn verify(
        &self,
        account_pk: &[u8; MAILBOX_KEY_LEN],
    ) -> CoreResult<[u8; MAILBOX_KEY_LEN], MailboxError> {
        let account = VerifyingKey::from_bytes(account_pk).map_err(|_| MailboxError::BadKey)?;
        if account.is_weak() {
            return Err(MailboxError::BadKey);
        }
        usable_key(&self.mailbox_pk)?;
        let signature =
            Signature::from_bytes(&self.signature).map_err(|_| MailboxError::BadBinding)?;
        account
            .verify(&binding_signed_bytes(&self.mailbox_pk), &signature)
            .map_err(|_| MailboxError::BadBinding)?;
        Ok(self.mailbox_pk)
    }
}

/// The smallest bucket that holds `plaintext_len` bytes, or `None` when it is
/// longer than [`MAILBOX_MAX_BUCKET`].
#[must_use]
pub const fn bucket_for(plaintext_len: usize) -> Option<usize> {
    if plaintext_len > MAILBOX_MAX_BUCKET {
        return None;
    }
    if plaintext_len <= MAILBOX_MIN_BUCKET {
        return Some(MAILBOX_MIN_BUCKET);
    }
    if plaintext_len <= MAILBOX_POW2_MAX_BUCKET {
        return Some(plaintext_len.next_power_of_two());
    }
    Some(plaintext_len.div_ceil(MAILBOX_BUCKET_STEP) * MAILBOX_BUCKET_STEP)
}

/// Whether `len` is a bucket size.
#[must_use]
pub const fn is_bucket(len: usize) -> bool {
    match bucket_for(len) {
        Some(b) => b == len,
        None => false,
    }
}

/// Whether `row` has the shape of a sealed mailbox row: the version byte and
/// a length that is a bucket plus [`MAILBOX_ROW_OVERHEAD`]. A server can
/// check this without any key; it says nothing about who wrote the row.
#[must_use]
pub fn is_well_formed_row(row: &[u8]) -> bool {
    row.first() == Some(&MAILBOX_ROW_VERSION_V1)
        && row.len() >= MAILBOX_ROW_OVERHEAD
        && is_bucket(row.len() - MAILBOX_ROW_OVERHEAD)
}

/// The AAD of a row addressed to `address`.
fn row_aad(address: &[u8; MAILBOX_KEY_LEN]) -> [u8; 59] {
    let mut out = [0u8; 59];
    let d = MAILBOX_ENVELOPE_AAD_DOMAIN.len();
    out[..d].copy_from_slice(MAILBOX_ENVELOPE_AAD_DOMAIN);
    out[d] = MAILBOX_ROW_VERSION_V1;
    out[d + 1..].copy_from_slice(address);
    out
}

const _: () = assert!(MAILBOX_ENVELOPE_AAD_DOMAIN.len() + 1 + MAILBOX_KEY_LEN == 59);

/// Seal `body` as a mailbox row of `kind` to the mailbox key `mailbox_pk`.
///
/// `mailbox_pk` must come from [`MailboxBinding::verify`] against the
/// recipient's pinned account key; this function cannot check that.
///
/// # Errors
///
/// * [`MailboxError::BodyTooLong`] — more than [`MAILBOX_MAX_BODY_LEN`].
/// * [`MailboxError::BadKey`] — `mailbox_pk` is not a usable public key.
/// * [`MailboxError::NotOpened`] — the AEAD refused (not expected).
pub fn seal_mailbox_row(
    mailbox_pk: &[u8; MAILBOX_KEY_LEN],
    kind: MailboxKind,
    body: &[u8],
) -> CoreResult<Vec<u8>, MailboxError> {
    if body.len() > MAILBOX_MAX_BODY_LEN {
        return Err(MailboxError::BodyTooLong);
    }
    let key = usable_key(mailbox_pk)?;
    let bucket =
        bucket_for(MAILBOX_PLAINTEXT_HEADER_LEN + body.len()).ok_or(MailboxError::BodyTooLong)?;
    let body_len = u32::try_from(body.len()).map_err(|_| MailboxError::BodyTooLong)?;
    let mut plaintext = vec![0u8; bucket];
    plaintext[0] = kind.to_byte();
    plaintext[1..MAILBOX_PLAINTEXT_HEADER_LEN].copy_from_slice(&body_len.to_be_bytes());
    plaintext[MAILBOX_PLAINTEXT_HEADER_LEN..MAILBOX_PLAINTEXT_HEADER_LEN + body.len()]
        .copy_from_slice(body);
    let sealed = envelope::seal_labeled(
        &MAILBOX_SEAL_LABELS,
        &key.to_x25519_public(),
        &row_aad(&mailbox_address(mailbox_pk)),
        &plaintext,
    )
    .map_err(|_| MailboxError::NotOpened)?;
    plaintext.fill(0);
    let mut row = Vec::with_capacity(1 + sealed.len());
    row.push(MAILBOX_ROW_VERSION_V1);
    row.extend_from_slice(&sealed);
    Ok(row)
}

/// Open a mailbox row with this account's mailbox key. Bounded and
/// fail-closed: an unknown version, a length that is not a bucket, a row for
/// another mailbox, an unknown kind, a body length that overruns the
/// plaintext and non-zero padding are all refused.
///
/// # Errors
///
/// Every [`MailboxError`] but [`MailboxError::BodyTooLong`],
/// [`MailboxError::BadKey`] and [`MailboxError::BadBinding`].
pub fn open_mailbox_row(
    key: &MailboxKey,
    row: &[u8],
) -> CoreResult<(MailboxKind, Vec<u8>), MailboxError> {
    let (&version, sealed) = row.split_first().ok_or(MailboxError::BadLength)?;
    if version != MAILBOX_ROW_VERSION_V1 {
        return Err(MailboxError::UnknownVersion(version));
    }
    if !is_well_formed_row(row) {
        return Err(MailboxError::BadLength);
    }
    let plaintext = envelope::open_labeled(
        &MAILBOX_SEAL_LABELS,
        &key.x25519_secret(),
        &row_aad(&key.address()),
        sealed,
    )
    .map_err(|_| MailboxError::NotOpened)?;
    parse_plaintext(&plaintext)
}

/// Split an opened plaintext into kind and body, checking the length field
/// and the padding.
fn parse_plaintext(plaintext: &[u8]) -> CoreResult<(MailboxKind, Vec<u8>), MailboxError> {
    if plaintext.len() < MAILBOX_PLAINTEXT_HEADER_LEN || !is_bucket(plaintext.len()) {
        return Err(MailboxError::BadLength);
    }
    let kind = MailboxKind::from_byte(plaintext[0])?;
    let len_bytes: [u8; 4] = plaintext[1..MAILBOX_PLAINTEXT_HEADER_LEN]
        .try_into()
        .map_err(|_| MailboxError::BadPadding)?;
    let body_len =
        usize::try_from(u32::from_be_bytes(len_bytes)).map_err(|_| MailboxError::BadPadding)?;
    let body_end = MAILBOX_PLAINTEXT_HEADER_LEN
        .checked_add(body_len)
        .filter(|end| *end <= plaintext.len())
        .ok_or(MailboxError::BadPadding)?;
    if plaintext[body_end..].iter().any(|b| *b != 0) {
        return Err(MailboxError::BadPadding);
    }
    Ok((
        kind,
        plaintext[MAILBOX_PLAINTEXT_HEADER_LEN..body_end].to_vec(),
    ))
}

/// The fuzz oracle behind `fuzz_api::mailbox_row_open`: open `row` under a
/// fixed key; whatever opens must re-seal to a row that opens to the same
/// kind and body. `None` when the open refuses.
pub(crate) fn fuzz_open(row: &[u8]) -> Option<bool> {
    let key = MailboxKey::from_account_seed(&[0x4D; MAILBOX_KEY_LEN]);
    let (kind, body) = open_mailbox_row(&key, row).ok()?;
    let resealed = seal_mailbox_row(&key.public(), kind, &body).ok()?;
    Some(open_mailbox_row(&key, &resealed).ok() == Some((kind, body)))
}

/// The fuzz oracle behind `fuzz_api::mailbox_binding_decode`: whatever
/// decodes re-encodes to the same bytes, and verifying it against a fixed
/// account key never panics. `None` when the decode refuses.
pub(crate) fn fuzz_binding(bytes: &[u8]) -> Option<bool> {
    let binding = MailboxBinding::decode(bytes).ok()?;
    let account = SigningKey::from_seed(&[0x4E; MAILBOX_KEY_LEN])
        .verifying_key()
        .to_bytes();
    let _ = binding.verify(&account);
    Some(binding.encode().as_slice() == bytes)
}

/// The plaintext parser behind `fuzz_api::mailbox_plaintext`, which a
/// fuzzer cannot reach through the AEAD. `None` when it refuses; else
/// whether the accepted body fits the plaintext it came from.
pub(crate) fn fuzz_plaintext(plaintext: &[u8]) -> Option<bool> {
    let (_, body) = parse_plaintext(plaintext).ok()?;
    Some(MAILBOX_PLAINTEXT_HEADER_LEN + body.len() <= plaintext.len())
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

    fn account_pk(seed: &[u8; 32]) -> [u8; 32] {
        SigningKey::from_seed(seed).verifying_key().to_bytes()
    }

    #[test]
    fn golden_pins_the_derivations() {
        let seed = mailbox_seed(&ACCOUNT);
        assert_eq!(
            hex(seed.as_bytes()),
            "b33a0e986f8a8447d3e7798150bade35454108d798542d128f15ab1af60bd4ee",
            "a change here moves every mailbox of every account"
        );
        let key = MailboxKey::from_account_seed(&ACCOUNT);
        assert_eq!(
            hex(&key.public()),
            "27e7befc1fcf42613d287ba29568c556b985f985d055d65d9d44d98eb36e8658"
        );
        assert_eq!(
            hex(&key.address()),
            "4911a04f075640c8eef0d6334ada8ecb095208e3e97d3a4cc40656d6d7f84a18"
        );
        assert_eq!(
            hex(&MailboxBinding::for_account_seed(&ACCOUNT).encode()),
            "0127e7befc1fcf42613d287ba29568c556b985f985d055d65d9d44d98eb36e8658\
             5c598cb5d3d7f6e453c9959734469a1c942e7c549390bea6170ac683675345296250b5bef7ecd7c\
             192ccbdb8ebcd95814c9f7be2f818edcb2b1d7525267c2706"
        );
    }

    #[test]
    fn the_derivations_use_their_own_domains() {
        assert_eq!(
            mailbox_seed(&ACCOUNT).as_bytes(),
            &blake3_derive_key_from_material("lakko/mailbox/v1", &ACCOUNT)
        );
        let pk = MailboxKey::from_account_seed(&ACCOUNT).public();
        assert_eq!(
            mailbox_address(&pk),
            blake3_derive_key_from_material("lakko/mailbox/address/v1", &pk)
        );
        // The mailbox key is not the identity key, and the address is not
        // the ADR-0005 identity pseudonym.
        assert_ne!(pk, account_pk(&ACCOUNT));
        let identity = SigningKey::from_seed(&ACCOUNT).verifying_key();
        assert_ne!(
            mailbox_address(&pk),
            *crate::IdentityPseudonym::from_identity(&identity).as_bytes()
        );
        assert_ne!(
            mailbox_seed(&ACCOUNT),
            mailbox_seed(&[0x43; 32]),
            "distinct accounts, distinct mailboxes"
        );
    }

    #[test]
    fn kind_bytes_are_frozen() {
        let expected = [1u8, 2, 3, 4];
        for (kind, byte) in MailboxKind::ALL.iter().zip(expected) {
            assert_eq!(kind.to_byte(), byte);
            assert_eq!(MailboxKind::from_byte(byte), Ok(*kind));
        }
        for byte in [0u8, 5, 0x7F, 0xFF] {
            assert_eq!(
                MailboxKind::from_byte(byte),
                Err(MailboxError::UnknownKind(byte))
            );
        }
    }

    #[test]
    fn a_binding_verifies_only_under_its_account() {
        let binding = MailboxBinding::for_account_seed(&ACCOUNT);
        let mailbox = MailboxKey::from_account_seed(&ACCOUNT).public();
        assert_eq!(binding.verify(&account_pk(&ACCOUNT)), Ok(mailbox));
        assert_eq!(
            binding.verify(&account_pk(&[0x43; 32])),
            Err(MailboxError::BadBinding)
        );
        // Someone else's mailbox key signed by the attacker does not verify
        // under the victim's account key.
        let attacker = SigningKey::from_seed(&[0x66; 32]);
        let forged = MailboxBinding::sign(&attacker, &mailbox);
        assert_eq!(
            forged.verify(&account_pk(&ACCOUNT)),
            Err(MailboxError::BadBinding)
        );
        // A binding naming another mailbox key under the right signature
        // layout fails once a byte of the key moves.
        let mut wire = binding.encode();
        wire[5] ^= 1;
        let moved = MailboxBinding::decode(&wire).unwrap();
        assert!(moved.verify(&account_pk(&ACCOUNT)).is_err());
    }

    #[test]
    fn a_binding_round_trips_and_its_layout_is_pinned() {
        let binding = MailboxBinding::for_account_seed(&ACCOUNT);
        let wire = binding.encode();
        assert_eq!(wire.len(), 97);
        assert_eq!(MAILBOX_BINDING_LEN, 97);
        assert_eq!(wire[0], 0x01);
        assert_eq!(
            &wire[1..33],
            &MailboxKey::from_account_seed(&ACCOUNT).public()
        );
        assert_eq!(MailboxBinding::decode(&wire), Ok(binding));
        assert_eq!(
            binding,
            MailboxBinding::for_account_seed(&ACCOUNT),
            "deterministic: every device produces the same binding"
        );
    }

    #[test]
    fn binding_decode_is_fail_closed() {
        let wire = MailboxBinding::for_account_seed(&ACCOUNT).encode();
        for len in [0, 1, 32, 96] {
            assert_eq!(
                MailboxBinding::decode(&wire[..len]),
                Err(MailboxError::BadLength)
            );
        }
        let mut long = wire.to_vec();
        long.push(0);
        assert_eq!(MailboxBinding::decode(&long), Err(MailboxError::BadLength));
        let mut bad_version = wire;
        bad_version[0] = 2;
        assert_eq!(
            MailboxBinding::decode(&bad_version),
            Err(MailboxError::UnknownVersion(2))
        );
    }

    #[test]
    fn a_small_order_mailbox_key_is_refused() {
        // The identity point (y = 1) encodes as 0x01 followed by zeros.
        let mut weak = [0u8; 32];
        weak[0] = 1;
        assert_eq!(
            seal_mailbox_row(&weak, MailboxKind::Welcome, b"x"),
            Err(MailboxError::BadKey)
        );
        let identity = SigningKey::from_seed(&ACCOUNT);
        let binding = MailboxBinding::sign(&identity, &weak);
        assert_eq!(
            binding.verify(&account_pk(&ACCOUNT)),
            Err(MailboxError::BadKey)
        );
    }

    #[test]
    fn a_row_round_trips_for_every_kind() {
        let key = MailboxKey::from_account_seed(&ACCOUNT);
        for kind in MailboxKind::ALL {
            for body in [&b""[..], b"hello", &[0xAB; 3000]] {
                let row = seal_mailbox_row(&key.public(), kind, body).unwrap();
                assert!(is_well_formed_row(&row));
                assert_eq!(row[0], MAILBOX_ROW_VERSION_V1);
                assert_eq!(open_mailbox_row(&key, &row), Ok((kind, body.to_vec())));
            }
        }
    }

    #[test]
    fn a_row_opens_only_under_its_mailbox_key() {
        let key = MailboxKey::from_account_seed(&ACCOUNT);
        let other = MailboxKey::from_account_seed(&[0x43; 32]);
        let row = seal_mailbox_row(&key.public(), MailboxKind::Welcome, b"w").unwrap();
        assert_eq!(open_mailbox_row(&other, &row), Err(MailboxError::NotOpened));
    }

    #[test]
    fn a_row_is_bound_to_its_address() {
        // The same X25519 secret with a different address in the AAD refuses:
        // reconstruct the row's sealed part and open it under another address.
        let key = MailboxKey::from_account_seed(&ACCOUNT);
        let row = seal_mailbox_row(&key.public(), MailboxKind::ContactAck, b"a").unwrap();
        let wrong_aad = row_aad(&[0x11; 32]);
        assert!(envelope::open_labeled(
            &MAILBOX_SEAL_LABELS,
            &key.x25519_secret(),
            &wrong_aad,
            &row[1..]
        )
        .is_err());
        assert!(envelope::open_labeled(
            &MAILBOX_SEAL_LABELS,
            &key.x25519_secret(),
            &row_aad(&key.address()),
            &row[1..]
        )
        .is_ok());
        // Nor does it open as a contact-request seal (other HKDF labels).
        assert!(envelope::open(&key.x25519_secret(), &row_aad(&key.address()), &row[1..]).is_err());
    }

    #[test]
    fn tampering_is_refused() {
        let key = MailboxKey::from_account_seed(&ACCOUNT);
        let row = seal_mailbox_row(&key.public(), MailboxKind::ContactRequest, b"r").unwrap();
        for at in [1, 33, row.len() - 1] {
            let mut bad = row.clone();
            bad[at] ^= 1;
            assert!(open_mailbox_row(&key, &bad).is_err(), "byte {at}");
        }
        let mut bad_version = row.clone();
        bad_version[0] = 2;
        assert_eq!(
            open_mailbox_row(&key, &bad_version),
            Err(MailboxError::UnknownVersion(2))
        );
        assert_eq!(
            open_mailbox_row(&key, &row[..row.len() - 1]),
            Err(MailboxError::BadLength)
        );
        assert_eq!(open_mailbox_row(&key, &[]), Err(MailboxError::BadLength));
    }

    #[test]
    fn buckets_are_powers_of_two_then_steps() {
        assert_eq!(bucket_for(0), Some(1024));
        assert_eq!(bucket_for(1024), Some(1024));
        assert_eq!(bucket_for(1025), Some(2048));
        assert_eq!(bucket_for(65_536), Some(65_536));
        assert_eq!(bucket_for(65_537), Some(131_072));
        assert_eq!(bucket_for(131_073), Some(196_608));
        assert_eq!(MAILBOX_MAX_BUCKET, 983_040);
        assert_eq!(bucket_for(MAILBOX_MAX_BUCKET), Some(MAILBOX_MAX_BUCKET));
        assert_eq!(bucket_for(MAILBOX_MAX_BUCKET + 1), None);
        const {
            assert!(MAILBOX_MAX_BUCKET + MAILBOX_ROW_OVERHEAD <= MAILBOX_MAX_ROW_LEN);
            assert!(
                MAILBOX_MAX_BUCKET + MAILBOX_BUCKET_STEP + MAILBOX_ROW_OVERHEAD
                    > MAILBOX_MAX_ROW_LEN
            );
        }
        let mut expected = vec![1024usize, 2048, 4096, 8192, 16_384, 32_768, 65_536];
        let mut b = 131_072;
        while b <= MAILBOX_MAX_BUCKET {
            expected.push(b);
            b += 65_536;
        }
        let all: Vec<usize> = (0..=MAILBOX_MAX_BUCKET).filter(|n| is_bucket(*n)).collect();
        assert_eq!(all, expected);
    }

    #[test]
    fn every_bucket_boundary_seals_to_the_right_size() {
        let key = MailboxKey::from_account_seed(&ACCOUNT);
        let mut boundaries = vec![1024usize, 2048, 4096, 8192, 16_384, 32_768, 65_536];
        boundaries.extend([131_072, 196_608, MAILBOX_MAX_BUCKET]);
        for bucket in boundaries {
            let fits = bucket - MAILBOX_PLAINTEXT_HEADER_LEN;
            let row =
                seal_mailbox_row(&key.public(), MailboxKind::Welcome, &vec![7; fits]).unwrap();
            assert_eq!(row.len(), bucket + MAILBOX_ROW_OVERHEAD, "{bucket} exactly");
            if bucket < MAILBOX_MAX_BUCKET {
                let over =
                    seal_mailbox_row(&key.public(), MailboxKind::Welcome, &vec![7; fits + 1])
                        .unwrap();
                assert!(over.len() > row.len(), "{bucket} plus one moves up");
            }
        }
        assert_eq!(
            seal_mailbox_row(
                &key.public(),
                MailboxKind::Welcome,
                &vec![0; MAILBOX_MAX_BODY_LEN + 1]
            ),
            Err(MailboxError::BodyTooLong)
        );
        let biggest = seal_mailbox_row(
            &key.public(),
            MailboxKind::Welcome,
            &vec![0; MAILBOX_MAX_BODY_LEN],
        )
        .unwrap();
        assert!(biggest.len() <= MAILBOX_MAX_ROW_LEN);
        assert_eq!(
            open_mailbox_row(&key, &biggest).unwrap().1.len(),
            MAILBOX_MAX_BODY_LEN
        );
    }

    #[test]
    fn rows_of_equal_size_look_alike_whatever_their_kind() {
        let key = MailboxKey::from_account_seed(&ACCOUNT);
        let lens: Vec<usize> = MailboxKind::ALL
            .iter()
            .map(|kind| {
                seal_mailbox_row(&key.public(), *kind, &[1; 600])
                    .unwrap()
                    .len()
            })
            .collect();
        assert!(lens.iter().all(|l| *l == lens[0]));
        // A request and a Welcome of different but bucket-equal sizes too.
        let a = seal_mailbox_row(&key.public(), MailboxKind::ContactRequest, &[1; 10]).unwrap();
        let b = seal_mailbox_row(&key.public(), MailboxKind::Welcome, &[1; 900]).unwrap();
        assert_eq!(a.len(), b.len());
        assert_eq!(a[0], b[0], "the only clear byte is the version");
    }

    #[test]
    fn the_plaintext_parser_is_fail_closed() {
        let mut good = vec![0u8; 1024];
        good[0] = 0x02;
        good[4] = 3;
        good[5..8].copy_from_slice(b"abc");
        assert_eq!(
            parse_plaintext(&good),
            Ok((MailboxKind::Welcome, b"abc".to_vec()))
        );
        let mut bad_kind = good.clone();
        bad_kind[0] = 9;
        assert_eq!(
            parse_plaintext(&bad_kind),
            Err(MailboxError::UnknownKind(9))
        );
        let mut nonzero = good.clone();
        nonzero[1023] = 1;
        assert_eq!(parse_plaintext(&nonzero), Err(MailboxError::BadPadding));
        let mut overrun = good.clone();
        overrun[1..5].copy_from_slice(&1020u32.to_be_bytes());
        assert_eq!(parse_plaintext(&overrun), Err(MailboxError::BadPadding));
        overrun[1..5].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(parse_plaintext(&overrun), Err(MailboxError::BadPadding));
        let mut exact = good.clone();
        exact[1..5].copy_from_slice(&1019u32.to_be_bytes());
        assert!(parse_plaintext(&exact).is_ok());
        assert_eq!(parse_plaintext(&good[..1000]), Err(MailboxError::BadLength));
        assert_eq!(parse_plaintext(&[]), Err(MailboxError::BadLength));
    }

    #[test]
    fn the_row_shape_check_needs_no_key() {
        let key = MailboxKey::from_account_seed(&ACCOUNT);
        let row = seal_mailbox_row(&key.public(), MailboxKind::Welcome, b"w").unwrap();
        assert!(is_well_formed_row(&row));
        assert!(!is_well_formed_row(&row[..row.len() - 1]));
        assert!(!is_well_formed_row(b"hello"));
        assert!(!is_well_formed_row(&[]));
        let mut v2 = row;
        v2[0] = 2;
        assert!(!is_well_formed_row(&v2));
    }

    #[test]
    fn the_fuzz_oracles_hold_on_real_rows() {
        let key = MailboxKey::from_account_seed(&[0x4D; 32]);
        let row = seal_mailbox_row(&key.public(), MailboxKind::ContactAck, b"ack").unwrap();
        assert_eq!(fuzz_open(&row), Some(true));
        assert_eq!(fuzz_open(b"junk"), None);
        let binding = MailboxBinding::for_account_seed(&[0x4E; 32]).encode();
        assert_eq!(fuzz_binding(&binding), Some(true));
        assert_eq!(fuzz_binding(&binding[..10]), None);
    }

    #[test]
    fn debug_never_prints_the_seed() {
        let key = MailboxKey::from_account_seed(&ACCOUNT);
        let seed_hex = hex(key.seed().as_bytes());
        let dbg = format!("{key:?}");
        assert!(!dbg.contains(&seed_hex));
    }
}
