// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Journal ownership statements ([ADR-0057]).
//!
//! A claimed group journal has a **root** key held by the group's owner and
//! a set of **registrars** the root appoints, ranked `member` or `admin`.
//! Every change to who may register epochs - and every epoch registration -
//! is a signed statement, defined here once so the server and every client
//! agree on the bytes.
//!
//! # Keys
//!
//! - **Root:** `root_seed = BLAKE3-derive-key("lakko/ds/journal-root/v1",
//!   account_identity_seed || journal_id)`, the Ed25519 key from that seed.
//!   Deterministic on every device of the owner's account, unlinkable across
//!   groups.
//! - **Registrar:** `registrar_seed = BLAKE3-derive-key(
//!   "lakko/ds/journal-registrar/v1", root_seed || member_account_key)`.
//!   The root derives it and sends the seed to that member, sealed to the
//!   member's account (`lakko_contacts::registrar_seed`).
//!
//! # Wire format
//!
//! One binary blob, sent as `{"statement_b64":"<standard base64>"}`:
//!
//! ```text
//! STATEMENT_DOMAIN || kind(1) || journal_id(32) || signer(32) || unix_ms(8 BE)
//!   || nonce(16) || fields || signature(64)
//! ```
//!
//! The signature is Ed25519 by `signer` over everything before it. `fields`
//! per kind:
//!
//! | kind | fields |
//! |---|---|
//! | 1 claim | `epoch(8) epoch_key(32)` - the signer is the root |
//! | 2 reset | `epoch(8) epoch_key(32) has_set(1) [count(2) (key(32) rank(1))*]` |
//! | 3 register epoch | `epoch(8) epoch_key(32) count(2) revoke_key(32)*` |
//! | 4 registrars | `count(2) (key(32) change(1))*` - change 0 removes |
//! | 5 root transfer | `new_root(32)` |
//! | 6 push binding | `push_gid(32)` |
//!
//! Ranks: `1` member, `2` admin (the root is not a registrar and outranks
//! both). Lists hold at most [`MAX_LIST_ENTRIES`] distinct keys. Every key
//! except a `push_gid` and a revoked key must be a valid Ed25519 point. The
//! decoder is bounded ([`MAX_STATEMENT_LEN`]) and requires the input to be
//! consumed exactly, so one statement has one encoding.
//!
//! # What the verifier checks
//!
//! [`SignedStatement::verify`]: the time window ([`crate::PROOF_WINDOW_MS`])
//! first, then the signature under the embedded signer. Whether that signer
//! may make the statement - is it the journal's root, which rank does it
//! hold - and the nonce replay cache are the server's, since both are state.
//!
//! [ADR-0057]: ../../../docs/adr/0057-journal-ownership.md

use base64::engine::general_purpose::STANDARD as B64_STD;
use base64::Engine as _;
use lakko_crypto::hash;

use crate::{Signature, SigningKey, VerifyingKey, NONCE_LEN, PROOF_WINDOW_MS, SIGNATURE_LEN};

/// Domain separator that opens every statement.
pub const STATEMENT_DOMAIN: &[u8] = b"lakko/ds/statement/v1\x00";
/// BLAKE3 derive-key context of a journal's root seed.
pub const JOURNAL_ROOT_DOMAIN: &str = "lakko/ds/journal-root/v1";
/// BLAKE3 derive-key context of a registrar seed.
pub const JOURNAL_REGISTRAR_DOMAIN: &str = "lakko/ds/journal-registrar/v1";
/// A journal id: the 32-byte group id the DS path names.
pub const JOURNAL_ID_LEN: usize = 32;
/// An Ed25519 public key, a seed, or a `push_gid`.
pub const KEY_LEN: usize = 32;
/// Most keys in one statement list (a registrar set, a change list, a
/// revocation list). Chosen, not measured: an owner appointing registrars for
/// a large group sends several statements.
pub const MAX_LIST_ENTRIES: usize = 256;

const HEADER_LEN: usize = STATEMENT_DOMAIN.len() + 1 + JOURNAL_ID_LEN + KEY_LEN + 8 + NONCE_LEN;
const ENTRY_LEN: usize = KEY_LEN + 1;

/// Upper bound on an encoded statement; anything longer is refused unparsed.
/// The largest kind is a reset carrying a full registrar set.
pub const MAX_STATEMENT_LEN: usize =
    HEADER_LEN + 8 + KEY_LEN + 1 + 2 + MAX_LIST_ENTRIES * ENTRY_LEN + SIGNATURE_LEN;

/// Derive a journal's root seed from the owner's account identity seed (the
/// 32-byte Ed25519 seed of the ADR-0017 account identity) and the journal id.
#[must_use]
pub fn journal_root_seed(
    account_identity_seed: &[u8; KEY_LEN],
    journal_id: &[u8; JOURNAL_ID_LEN],
) -> [u8; KEY_LEN] {
    let mut material = [0u8; KEY_LEN + JOURNAL_ID_LEN];
    material[..KEY_LEN].copy_from_slice(account_identity_seed);
    material[KEY_LEN..].copy_from_slice(journal_id);
    let out = hash::blake3_derive_key_from_material(JOURNAL_ROOT_DOMAIN, &material);
    material.fill(0);
    out
}

/// Derive the registrar seed the root appoints for `member_account_key` (the
/// member's 32-byte account Ed25519 public key).
#[must_use]
pub fn journal_registrar_seed(
    root_seed: &[u8; KEY_LEN],
    member_account_key: &[u8; KEY_LEN],
) -> [u8; KEY_LEN] {
    let mut material = [0u8; KEY_LEN * 2];
    material[..KEY_LEN].copy_from_slice(root_seed);
    material[KEY_LEN..].copy_from_slice(member_account_key);
    let out = hash::blake3_derive_key_from_material(JOURNAL_REGISTRAR_DOMAIN, &material);
    material.fill(0);
    out
}

/// The message a push binding is keyed over, ahead of the `push_gid`.
pub const PUSH_BINDING_DOMAIN: &[u8] = b"lakko/ds/push-binding/v1\x00";

/// What the Delivery Service stores for a journal's push binding:
/// `keyed-BLAKE3(pepper, PUSH_BINDING_DOMAIN || push_gid)`, with `pepper` the
/// server-only `LAKKO_DS_BINDING_PEPPER`. A server-side helper, here so the
/// domain is defined next to the statement that carries the `push_gid`.
#[must_use]
pub fn push_binding_key(pepper: &[u8; KEY_LEN], push_gid: &[u8; KEY_LEN]) -> [u8; KEY_LEN] {
    let mut message = Vec::with_capacity(PUSH_BINDING_DOMAIN.len() + KEY_LEN);
    message.extend_from_slice(PUSH_BINDING_DOMAIN);
    message.extend_from_slice(push_gid);
    hash::blake3_keyed(pepper, &message)
}

/// The Ed25519 signing key of a root or registrar seed.
#[must_use]
pub fn statement_signing_key(seed: &[u8; KEY_LEN]) -> SigningKey {
    SigningKey::from_seed(seed)
}

/// A registrar's rank. The root is not a registrar and outranks both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rank {
    /// A member that may commit self-service changes.
    Member,
    /// One of the owner's admins.
    Admin,
}

impl Rank {
    /// The wire byte (and the value the server stores).
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        match self {
            Self::Member => 1,
            Self::Admin => 2,
        }
    }

    /// Parse a wire byte.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Member),
            2 => Some(Self::Admin),
            _ => None,
        }
    }
}

/// The rank value the server records for a registration made by the root:
/// above every registrar rank.
pub const ROOT_RANK_BYTE: u8 = 3;

/// One registrar in a reset's registrar set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegistrarEntry {
    /// The registrar's Ed25519 public key.
    pub key: [u8; KEY_LEN],
    /// Its rank.
    pub rank: Rank,
}

/// One change in a registrar-management statement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegistrarChange {
    /// The registrar's Ed25519 public key.
    pub key: [u8; KEY_LEN],
    /// The rank to set, or `None` to remove the registrar.
    pub rank: Option<Rank>,
}

/// Which statement a blob carries. The byte values are frozen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StatementKind {
    /// The first claim of a journal, by its root.
    Claim,
    /// The root replaces the journal's epoch chain (and optionally its
    /// registrar set).
    Reset,
    /// A registrar (or the root) registers an epoch's access key.
    RegisterEpoch,
    /// The root adds, re-ranks or removes registrars.
    Registrars,
    /// The root names a new root.
    RootTransfer,
    /// The root or an admin binds the journal to one push routing id.
    PushBinding,
}

impl StatementKind {
    /// The wire byte.
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        match self {
            Self::Claim => 1,
            Self::Reset => 2,
            Self::RegisterEpoch => 3,
            Self::Registrars => 4,
            Self::RootTransfer => 5,
            Self::PushBinding => 6,
        }
    }

    const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Claim),
            2 => Some(Self::Reset),
            3 => Some(Self::RegisterEpoch),
            4 => Some(Self::Registrars),
            5 => Some(Self::RootTransfer),
            6 => Some(Self::PushBinding),
            _ => None,
        }
    }

    /// The last path segment of the `PUT /ds/group/{hex}/<segment>` route
    /// that takes this statement.
    #[must_use]
    pub const fn path_segment(self) -> &'static str {
        match self {
            Self::Claim => "claim",
            Self::Reset => "reset",
            Self::RegisterEpoch => "access",
            Self::Registrars => "registrars",
            Self::RootTransfer => "root",
            Self::PushBinding => "push-binding",
        }
    }
}

/// What a statement says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatementBody {
    /// First claim: the signer becomes the root, `epoch_key` the first epoch.
    Claim {
        /// The first registered epoch.
        epoch: u64,
        /// Its access verifying key.
        epoch_key: [u8; KEY_LEN],
    },
    /// Root reset: the chain becomes this one epoch; the registrar set is
    /// replaced when `registrars` is `Some`, kept when `None`.
    Reset {
        /// The epoch the new chain starts at.
        epoch: u64,
        /// Its access verifying key.
        epoch_key: [u8; KEY_LEN],
        /// The new registrar set, or `None` to keep the current one.
        registrars: Option<Vec<RegistrarEntry>>,
    },
    /// Register an epoch, revoking the listed registrars in the same step.
    RegisterEpoch {
        /// The epoch.
        epoch: u64,
        /// Its access verifying key.
        epoch_key: [u8; KEY_LEN],
        /// Registrar keys to revoke (the signer may revoke only strictly
        /// lower ranks).
        revoke: Vec<[u8; KEY_LEN]>,
    },
    /// Add, re-rank or remove registrars.
    Registrars {
        /// The changes, at least one.
        changes: Vec<RegistrarChange>,
    },
    /// Name a new root.
    RootTransfer {
        /// The new root's public key.
        new_root: [u8; KEY_LEN],
    },
    /// Bind the journal to one push routing id.
    PushBinding {
        /// The ADR-0042 `push_gid`.
        push_gid: [u8; KEY_LEN],
    },
}

impl StatementBody {
    /// The kind this body is.
    #[must_use]
    pub const fn kind(&self) -> StatementKind {
        match self {
            Self::Claim { .. } => StatementKind::Claim,
            Self::Reset { .. } => StatementKind::Reset,
            Self::RegisterEpoch { .. } => StatementKind::RegisterEpoch,
            Self::Registrars { .. } => StatementKind::Registrars,
            Self::RootTransfer { .. } => StatementKind::RootTransfer,
            Self::PushBinding { .. } => StatementKind::PushBinding,
        }
    }
}

/// A statement with its signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedStatement {
    /// The journal (group id) this statement is about.
    pub journal_id: [u8; JOURNAL_ID_LEN],
    /// The signer's Ed25519 public key.
    pub signer: [u8; KEY_LEN],
    /// The signer's clock at signing, unix milliseconds.
    pub unix_ms: u64,
    /// Fresh per statement; the server refuses a repeat within the window.
    pub nonce: [u8; NONCE_LEN],
    /// What is stated.
    pub body: StatementBody,
    /// Ed25519 by `signer` over [`Self::signed_bytes`].
    pub signature: [u8; SIGNATURE_LEN],
}

/// Why a statement was refused. A reason code; nothing from the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StatementError {
    /// The blob did not parse (wrong domain, kind, length, count or byte).
    #[error("statement malformed")]
    Malformed,
    /// A list is empty where it must not be, or longer than the bound.
    #[error("statement list out of bounds")]
    ListBounds,
    /// A list names the same key twice.
    #[error("statement list repeats a key")]
    DuplicateKey,
    /// A key that must be an Ed25519 point is not one.
    #[error("statement carries an invalid public key")]
    InvalidKey,
    /// `unix_ms` is outside the window around the verifier's clock.
    #[error("statement outside the time window")]
    OutsideWindow,
    /// The signature does not verify under the embedded signer.
    #[error("statement signature invalid")]
    BadSignature,
}

fn is_point(key: &[u8; KEY_LEN]) -> bool {
    VerifyingKey::from_bytes(key).is_ok()
}

fn check_distinct(keys: impl Iterator<Item = [u8; KEY_LEN]>) -> Result<usize, StatementError> {
    let mut seen: Vec<[u8; KEY_LEN]> = Vec::new();
    for key in keys {
        if seen.contains(&key) {
            return Err(StatementError::DuplicateKey);
        }
        seen.push(key);
    }
    Ok(seen.len())
}

/// The list rules every constructor and the decoder share.
fn validate_body(body: &StatementBody) -> Result<(), StatementError> {
    match body {
        StatementBody::Claim { epoch_key, .. } => {
            if !is_point(epoch_key) {
                return Err(StatementError::InvalidKey);
            }
        },
        StatementBody::Reset {
            epoch_key,
            registrars,
            ..
        } => {
            if !is_point(epoch_key) {
                return Err(StatementError::InvalidKey);
            }
            if let Some(set) = registrars {
                if set.len() > MAX_LIST_ENTRIES {
                    return Err(StatementError::ListBounds);
                }
                check_distinct(set.iter().map(|e| e.key))?;
                if !set.iter().all(|e| is_point(&e.key)) {
                    return Err(StatementError::InvalidKey);
                }
            }
        },
        StatementBody::RegisterEpoch {
            epoch_key, revoke, ..
        } => {
            if !is_point(epoch_key) {
                return Err(StatementError::InvalidKey);
            }
            if revoke.len() > MAX_LIST_ENTRIES {
                return Err(StatementError::ListBounds);
            }
            check_distinct(revoke.iter().copied())?;
        },
        StatementBody::Registrars { changes } => {
            if changes.is_empty() || changes.len() > MAX_LIST_ENTRIES {
                return Err(StatementError::ListBounds);
            }
            check_distinct(changes.iter().map(|c| c.key))?;
            if !changes.iter().all(|c| is_point(&c.key)) {
                return Err(StatementError::InvalidKey);
            }
        },
        StatementBody::RootTransfer { new_root } => {
            if !is_point(new_root) {
                return Err(StatementError::InvalidKey);
            }
        },
        StatementBody::PushBinding { .. } => {},
    }
    Ok(())
}

fn count_u16(len: usize) -> [u8; 2] {
    // Every list is validated to at most MAX_LIST_ENTRIES (< 2^16) first.
    u16::try_from(len).unwrap_or(u16::MAX).to_be_bytes()
}

fn encode_fields(body: &StatementBody, out: &mut Vec<u8>) {
    match body {
        StatementBody::Claim { epoch, epoch_key } => {
            out.extend_from_slice(&epoch.to_be_bytes());
            out.extend_from_slice(epoch_key);
        },
        StatementBody::Reset {
            epoch,
            epoch_key,
            registrars,
        } => {
            out.extend_from_slice(&epoch.to_be_bytes());
            out.extend_from_slice(epoch_key);
            match registrars {
                None => out.push(0),
                Some(set) => {
                    out.push(1);
                    out.extend_from_slice(&count_u16(set.len()));
                    for e in set {
                        out.extend_from_slice(&e.key);
                        out.push(e.rank.to_byte());
                    }
                },
            }
        },
        StatementBody::RegisterEpoch {
            epoch,
            epoch_key,
            revoke,
        } => {
            out.extend_from_slice(&epoch.to_be_bytes());
            out.extend_from_slice(epoch_key);
            out.extend_from_slice(&count_u16(revoke.len()));
            for key in revoke {
                out.extend_from_slice(key);
            }
        },
        StatementBody::Registrars { changes } => {
            out.extend_from_slice(&count_u16(changes.len()));
            for c in changes {
                out.extend_from_slice(&c.key);
                out.push(c.rank.map_or(0, Rank::to_byte));
            }
        },
        StatementBody::RootTransfer { new_root: key }
        | StatementBody::PushBinding { push_gid: key } => {
            out.extend_from_slice(key);
        },
    }
}

fn signed_bytes_of(
    journal_id: &[u8; JOURNAL_ID_LEN],
    signer: &[u8; KEY_LEN],
    unix_ms: u64,
    nonce: &[u8; NONCE_LEN],
    body: &StatementBody,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + 64);
    out.extend_from_slice(STATEMENT_DOMAIN);
    out.push(body.kind().to_byte());
    out.extend_from_slice(journal_id);
    out.extend_from_slice(signer);
    out.extend_from_slice(&unix_ms.to_be_bytes());
    out.extend_from_slice(nonce);
    encode_fields(body, &mut out);
    out
}

/// Sign `body` about `journal_id` with `key`, at `unix_ms` with `nonce`
/// (both the caller's, so this is the same on the host, in the browser and
/// in an extension process).
///
/// # Errors
///
/// The list rules of the body ([`StatementError::ListBounds`],
/// [`StatementError::DuplicateKey`], [`StatementError::InvalidKey`]): a
/// statement the server would refuse is never signed.
pub fn sign_statement(
    key: &SigningKey,
    journal_id: &[u8; JOURNAL_ID_LEN],
    unix_ms: u64,
    nonce: [u8; NONCE_LEN],
    body: StatementBody,
) -> Result<SignedStatement, StatementError> {
    validate_body(&body)?;
    let signer = key.verifying_key().to_bytes();
    let bytes = signed_bytes_of(journal_id, &signer, unix_ms, &nonce, &body);
    Ok(SignedStatement {
        journal_id: *journal_id,
        signer,
        unix_ms,
        nonce,
        body,
        signature: key.sign(&bytes).to_bytes(),
    })
}

/// A bounded forward reader over the blob.
struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], StatementError> {
        if self.rest.len() < n {
            return Err(StatementError::Malformed);
        }
        let (head, tail) = self.rest.split_at(n);
        self.rest = tail;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], StatementError> {
        self.take(N)?
            .try_into()
            .map_err(|_| StatementError::Malformed)
    }

    fn u8(&mut self) -> Result<u8, StatementError> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Result<usize, StatementError> {
        Ok(usize::from(u16::from_be_bytes(self.array::<2>()?)))
    }

    fn u64(&mut self) -> Result<u64, StatementError> {
        Ok(u64::from_be_bytes(self.array::<8>()?))
    }

    /// A list count, checked against the bound and the bytes left before
    /// anything is allocated.
    fn count(&mut self, entry_len: usize) -> Result<usize, StatementError> {
        let n = self.u16()?;
        if n > MAX_LIST_ENTRIES {
            return Err(StatementError::ListBounds);
        }
        if self.rest.len() < n.saturating_mul(entry_len) {
            return Err(StatementError::Malformed);
        }
        Ok(n)
    }
}

fn decode_fields(kind: StatementKind, r: &mut Reader<'_>) -> Result<StatementBody, StatementError> {
    Ok(match kind {
        StatementKind::Claim => StatementBody::Claim {
            epoch: r.u64()?,
            epoch_key: r.array()?,
        },
        StatementKind::Reset => {
            let epoch = r.u64()?;
            let epoch_key = r.array()?;
            let registrars = match r.u8()? {
                0 => None,
                1 => {
                    let n = r.count(ENTRY_LEN)?;
                    let mut set = Vec::with_capacity(n);
                    for _ in 0..n {
                        let key = r.array()?;
                        let rank = Rank::from_byte(r.u8()?).ok_or(StatementError::Malformed)?;
                        set.push(RegistrarEntry { key, rank });
                    }
                    Some(set)
                },
                _ => return Err(StatementError::Malformed),
            };
            StatementBody::Reset {
                epoch,
                epoch_key,
                registrars,
            }
        },
        StatementKind::RegisterEpoch => {
            let epoch = r.u64()?;
            let epoch_key = r.array()?;
            let n = r.count(KEY_LEN)?;
            let mut revoke = Vec::with_capacity(n);
            for _ in 0..n {
                revoke.push(r.array()?);
            }
            StatementBody::RegisterEpoch {
                epoch,
                epoch_key,
                revoke,
            }
        },
        StatementKind::Registrars => {
            let n = r.count(ENTRY_LEN)?;
            let mut changes = Vec::with_capacity(n);
            for _ in 0..n {
                let key = r.array()?;
                let rank = match r.u8()? {
                    0 => None,
                    byte => Some(Rank::from_byte(byte).ok_or(StatementError::Malformed)?),
                };
                changes.push(RegistrarChange { key, rank });
            }
            StatementBody::Registrars { changes }
        },
        StatementKind::RootTransfer => StatementBody::RootTransfer {
            new_root: r.array()?,
        },
        StatementKind::PushBinding => StatementBody::PushBinding {
            push_gid: r.array()?,
        },
    })
}

impl SignedStatement {
    /// The bytes the signature covers.
    #[must_use]
    pub fn signed_bytes(&self) -> Vec<u8> {
        signed_bytes_of(
            &self.journal_id,
            &self.signer,
            self.unix_ms,
            &self.nonce,
            &self.body,
        )
    }

    /// The wire blob.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = self.signed_bytes();
        out.extend_from_slice(&self.signature);
        out
    }

    /// The JSON body the Delivery Service route takes:
    /// `{"statement_b64":"<standard base64>"}`.
    #[must_use]
    pub fn body_json(&self) -> String {
        format!(
            "{{\"statement_b64\":\"{}\"}}",
            B64_STD.encode(self.encode())
        )
    }

    /// The route segment this statement is sent to
    /// (`PUT /ds/group/{hex}/<segment>`).
    #[must_use]
    pub const fn path_segment(&self) -> &'static str {
        self.body.kind().path_segment()
    }

    /// Parse a blob. Bounded ([`MAX_STATEMENT_LEN`]), exact (every byte is
    /// consumed), fail-closed. Does not verify the signature.
    ///
    /// # Errors
    ///
    /// [`StatementError::Malformed`], [`StatementError::ListBounds`],
    /// [`StatementError::DuplicateKey`] or [`StatementError::InvalidKey`].
    pub fn decode(bytes: &[u8]) -> Result<Self, StatementError> {
        if bytes.len() > MAX_STATEMENT_LEN || bytes.len() < HEADER_LEN + SIGNATURE_LEN {
            return Err(StatementError::Malformed);
        }
        let (covered, signature) = bytes.split_at(bytes.len() - SIGNATURE_LEN);
        let mut r = Reader { rest: covered };
        if r.take(STATEMENT_DOMAIN.len())? != STATEMENT_DOMAIN {
            return Err(StatementError::Malformed);
        }
        let kind = StatementKind::from_byte(r.u8()?).ok_or(StatementError::Malformed)?;
        let journal_id = r.array()?;
        let signer: [u8; KEY_LEN] = r.array()?;
        let unix_ms = r.u64()?;
        let nonce = r.array()?;
        let body = decode_fields(kind, &mut r)?;
        if !r.rest.is_empty() {
            return Err(StatementError::Malformed);
        }
        if !is_point(&signer) {
            return Err(StatementError::InvalidKey);
        }
        validate_body(&body)?;
        let signature: [u8; SIGNATURE_LEN] = signature
            .try_into()
            .map_err(|_| StatementError::Malformed)?;
        Ok(Self {
            journal_id,
            signer,
            unix_ms,
            nonce,
            body,
            signature,
        })
    }

    /// Check the time window, then the signature under the embedded signer.
    /// Who the signer is to the journal is the caller's question.
    ///
    /// # Errors
    ///
    /// [`StatementError::OutsideWindow`] or [`StatementError::BadSignature`].
    pub fn verify(&self, now_ms: u64) -> Result<(), StatementError> {
        if now_ms.abs_diff(self.unix_ms) > PROOF_WINDOW_MS {
            return Err(StatementError::OutsideWindow);
        }
        let key =
            VerifyingKey::from_bytes(&self.signer).map_err(|_| StatementError::BadSignature)?;
        let signature =
            Signature::from_bytes(&self.signature).map_err(|_| StatementError::BadSignature)?;
        key.verify(&self.signed_bytes(), &signature)
            .map_err(|_| StatementError::BadSignature)
    }
}

/// A root or registrar signer: one seed, every statement it may make.
/// Clock and nonce are the caller's, as for [`crate::DsProofSigner`].
#[derive(Clone)]
pub struct StatementSigner {
    key: SigningKey,
}

impl core::fmt::Debug for StatementSigner {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StatementSigner")
            .field("public_key", &self.public_key())
            .finish_non_exhaustive()
    }
}

impl StatementSigner {
    /// The signer for a root or registrar seed.
    #[must_use]
    pub fn from_seed(seed: &[u8; KEY_LEN]) -> Self {
        Self {
            key: statement_signing_key(seed),
        }
    }

    /// The journal's root signer for the owner's account identity seed.
    #[must_use]
    pub fn root(account_identity_seed: &[u8; KEY_LEN], journal_id: &[u8; JOURNAL_ID_LEN]) -> Self {
        Self::from_seed(&journal_root_seed(account_identity_seed, journal_id))
    }

    /// The 32-byte public key the server knows this signer by.
    #[must_use]
    pub fn public_key(&self) -> [u8; KEY_LEN] {
        self.key.verifying_key().to_bytes()
    }

    /// Sign `body` about `journal_id`.
    ///
    /// # Errors
    ///
    /// See [`sign_statement`].
    pub fn sign_at(
        &self,
        journal_id: &[u8; JOURNAL_ID_LEN],
        body: StatementBody,
        now_ms: u64,
        nonce: [u8; NONCE_LEN],
    ) -> Result<SignedStatement, StatementError> {
        sign_statement(&self.key, journal_id, now_ms, nonce, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JOURNAL: [u8; 32] = [0x4A; 32];

    fn pk(seed: u8) -> [u8; 32] {
        SigningKey::from_seed(&[seed; 32])
            .verifying_key()
            .to_bytes()
    }

    fn signer() -> StatementSigner {
        StatementSigner::from_seed(&[0x11; 32])
    }

    fn every_body() -> Vec<StatementBody> {
        vec![
            StatementBody::Claim {
                epoch: 7,
                epoch_key: pk(1),
            },
            StatementBody::Reset {
                epoch: 0,
                epoch_key: pk(2),
                registrars: None,
            },
            StatementBody::Reset {
                epoch: 9,
                epoch_key: pk(2),
                registrars: Some(vec![
                    RegistrarEntry {
                        key: pk(3),
                        rank: Rank::Admin,
                    },
                    RegistrarEntry {
                        key: pk(4),
                        rank: Rank::Member,
                    },
                ]),
            },
            StatementBody::RegisterEpoch {
                epoch: u64::MAX,
                epoch_key: pk(5),
                revoke: vec![[9; 32], pk(6)],
            },
            StatementBody::RegisterEpoch {
                epoch: 1,
                epoch_key: pk(5),
                revoke: vec![],
            },
            StatementBody::Registrars {
                changes: vec![
                    RegistrarChange {
                        key: pk(7),
                        rank: Some(Rank::Member),
                    },
                    RegistrarChange {
                        key: pk(8),
                        rank: None,
                    },
                ],
            },
            StatementBody::RootTransfer { new_root: pk(9) },
            StatementBody::PushBinding {
                push_gid: [0xEE; 32],
            },
        ]
    }

    #[test]
    fn every_statement_round_trips_and_verifies() {
        for body in every_body() {
            let s = signer()
                .sign_at(&JOURNAL, body.clone(), 1_000_000, [7; NONCE_LEN])
                .unwrap();
            let wire = s.encode();
            assert!(wire.len() <= MAX_STATEMENT_LEN);
            let back = SignedStatement::decode(&wire).unwrap();
            assert_eq!(back, s);
            assert_eq!(back.body, body);
            back.verify(1_000_500).unwrap();
            assert_eq!(back.signer, signer().public_key());
        }
    }

    #[test]
    fn every_byte_is_bound_by_the_signature() {
        for body in every_body() {
            let s = signer()
                .sign_at(&JOURNAL, body, 1_000_000, [7; NONCE_LEN])
                .unwrap();
            let wire = s.encode();
            // Flip each signed byte; a flip either fails to parse or fails
            // to verify, never verifies.
            for i in 0..wire.len() - SIGNATURE_LEN {
                let mut tampered = wire.clone();
                tampered[i] ^= 0x01;
                if let Ok(parsed) = SignedStatement::decode(&tampered) {
                    assert!(parsed.verify(1_000_000).is_err(), "byte {i} not bound");
                }
            }
        }
    }

    #[test]
    fn another_signer_or_a_stale_clock_is_refused() {
        let s = signer()
            .sign_at(
                &JOURNAL,
                StatementBody::PushBinding { push_gid: [1; 32] },
                1_000_000,
                [0; NONCE_LEN],
            )
            .unwrap();
        let mut forged = s.clone();
        forged.signer = pk(0x22);
        assert_eq!(forged.verify(1_000_000), Err(StatementError::BadSignature));
        s.verify(1_000_000 + PROOF_WINDOW_MS).unwrap();
        assert_eq!(
            s.verify(1_000_000 + PROOF_WINDOW_MS + 1),
            Err(StatementError::OutsideWindow)
        );
        assert_eq!(
            s.verify(1_000_000 - PROOF_WINDOW_MS - 1),
            Err(StatementError::OutsideWindow)
        );
    }

    #[test]
    fn kinds_do_not_cross() {
        // A claim and a reset with the same epoch fields sign different bytes.
        let claim = signer()
            .sign_at(
                &JOURNAL,
                StatementBody::Claim {
                    epoch: 3,
                    epoch_key: pk(1),
                },
                5,
                [0; NONCE_LEN],
            )
            .unwrap();
        let mut as_reset = claim.encode();
        as_reset[STATEMENT_DOMAIN.len()] = StatementKind::Reset.to_byte();
        as_reset.insert(as_reset.len() - SIGNATURE_LEN, 0);
        let parsed = SignedStatement::decode(&as_reset).unwrap();
        assert_eq!(parsed.body.kind(), StatementKind::Reset);
        assert_eq!(parsed.verify(5), Err(StatementError::BadSignature));
    }

    #[test]
    fn the_signer_refuses_to_sign_what_the_server_would_refuse() {
        let dup = StatementBody::RegisterEpoch {
            epoch: 1,
            epoch_key: pk(1),
            revoke: vec![pk(2), pk(2)],
        };
        assert_eq!(
            signer().sign_at(&JOURNAL, dup, 0, [0; NONCE_LEN]),
            Err(StatementError::DuplicateKey)
        );
        let empty = StatementBody::Registrars { changes: vec![] };
        assert_eq!(
            signer().sign_at(&JOURNAL, empty, 0, [0; NONCE_LEN]),
            Err(StatementError::ListBounds)
        );
        let too_many = StatementBody::RegisterEpoch {
            epoch: 1,
            epoch_key: pk(1),
            revoke: (0..=MAX_LIST_ENTRIES)
                .map(|i| {
                    let mut k = [0u8; 32];
                    k[..8].copy_from_slice(&(i as u64).to_be_bytes());
                    k
                })
                .collect(),
        };
        assert_eq!(
            signer().sign_at(&JOURNAL, too_many, 0, [0; NONCE_LEN]),
            Err(StatementError::ListBounds)
        );
        // Not an Ed25519 point (y = 2 is off the curve).
        let mut bad = [0u8; 32];
        bad[0] = 2;
        let bad_key = StatementBody::RootTransfer { new_root: bad };
        assert_eq!(
            signer().sign_at(&JOURNAL, bad_key, 0, [0; NONCE_LEN]),
            Err(StatementError::InvalidKey)
        );
    }

    #[test]
    fn decode_is_bounded_exact_and_fail_closed() {
        let good = signer()
            .sign_at(
                &JOURNAL,
                StatementBody::RootTransfer { new_root: pk(3) },
                1,
                [2; NONCE_LEN],
            )
            .unwrap()
            .encode();
        assert!(SignedStatement::decode(&good).is_ok());
        assert_eq!(SignedStatement::decode(&[]), Err(StatementError::Malformed));
        let mut long = good.clone();
        long.push(0);
        assert!(SignedStatement::decode(&long).is_err());
        assert!(SignedStatement::decode(&good[..good.len() - 1]).is_err());
        let mut wrong_domain = good.clone();
        wrong_domain[0] ^= 1;
        assert_eq!(
            SignedStatement::decode(&wrong_domain),
            Err(StatementError::Malformed)
        );
        let mut wrong_kind = good.clone();
        wrong_kind[STATEMENT_DOMAIN.len()] = 0x07;
        assert_eq!(
            SignedStatement::decode(&wrong_kind),
            Err(StatementError::Malformed)
        );
        assert_eq!(
            SignedStatement::decode(&vec![0u8; MAX_STATEMENT_LEN + 1]),
            Err(StatementError::Malformed)
        );
        // A count above the bound is refused before any allocation.
        let mut huge = signed_bytes_of(
            &JOURNAL,
            &signer().public_key(),
            1,
            &[0; NONCE_LEN],
            &StatementBody::Registrars {
                changes: vec![RegistrarChange {
                    key: pk(1),
                    rank: None,
                }],
            },
        );
        let count_at = HEADER_LEN;
        huge[count_at..count_at + 2].copy_from_slice(&u16::MAX.to_be_bytes());
        huge.extend_from_slice(&[0u8; SIGNATURE_LEN]);
        assert_eq!(
            SignedStatement::decode(&huge),
            Err(StatementError::ListBounds)
        );
    }

    #[test]
    fn a_body_json_carries_standard_base64_of_the_blob() {
        let s = signer()
            .sign_at(
                &JOURNAL,
                StatementBody::PushBinding { push_gid: [3; 32] },
                1,
                [4; NONCE_LEN],
            )
            .unwrap();
        assert_eq!(
            s.body_json(),
            format!("{{\"statement_b64\":\"{}\"}}", B64_STD.encode(s.encode()))
        );
        assert_eq!(s.path_segment(), "push-binding");
    }

    #[test]
    fn path_segments_are_frozen() {
        let segments: Vec<_> = [
            StatementKind::Claim,
            StatementKind::Reset,
            StatementKind::RegisterEpoch,
            StatementKind::Registrars,
            StatementKind::RootTransfer,
            StatementKind::PushBinding,
        ]
        .iter()
        .map(|k| (k.to_byte(), k.path_segment()))
        .collect();
        assert_eq!(
            segments,
            vec![
                (1, "claim"),
                (2, "reset"),
                (3, "access"),
                (4, "registrars"),
                (5, "root"),
                (6, "push-binding"),
            ]
        );
    }

    // Golden vectors: a change to a derivation domain or its input layout
    // must fail here, not on a live group whose owner can no longer reach
    // its journal.
    const GOLDEN_ROOT_SEED: &str =
        "d44df89ad4d599e7fd1ee7fac9c3bb5f8b7f2cd89041722e83922dfd0568017f";
    const GOLDEN_REGISTRAR_SEED: &str =
        "3a645b6e7e557d5ef293dbe2dc546ee08975efc2e71b1e1f9108a31ab6b30130";
    const GOLDEN_CLAIM_BLAKE3: &str =
        "d5f24a1c868978609ab248b0b1047578c35cac21d05d9a98f4d787a7dc9cf669";

    #[test]
    fn derivations_are_pinned() {
        let root = journal_root_seed(&[0x01; 32], &[0x02; 32]);
        assert_eq!(hex(&root), GOLDEN_ROOT_SEED);
        let registrar = journal_registrar_seed(&root, &[0x03; 32]);
        assert_eq!(hex(&registrar), GOLDEN_REGISTRAR_SEED);
        // Both are BLAKE3 derive-key over the stated domains and inputs.
        let mut material = [0x01u8; 64];
        material[32..].fill(0x02);
        assert_eq!(
            root,
            hash::blake3_derive_key_from_material("lakko/ds/journal-root/v1", &material)
        );
        let mut material = [0u8; 64];
        material[..32].copy_from_slice(&root);
        material[32..].fill(0x03);
        assert_eq!(
            registrar,
            hash::blake3_derive_key_from_material("lakko/ds/journal-registrar/v1", &material)
        );
        // Unlinkable across groups, distinct across members.
        assert_ne!(root, journal_root_seed(&[0x01; 32], &[0x03; 32]));
        assert_ne!(registrar, journal_registrar_seed(&root, &[0x04; 32]));
        // The root signer is the key of the root seed.
        assert_eq!(
            StatementSigner::root(&[0x01; 32], &[0x02; 32]).public_key(),
            SigningKey::from_seed(&root).verifying_key().to_bytes()
        );
    }

    #[test]
    fn a_push_binding_key_depends_on_the_pepper_and_the_gid() {
        let a = push_binding_key(&[1; 32], &[2; 32]);
        assert_eq!(a, push_binding_key(&[1; 32], &[2; 32]));
        assert_ne!(a, push_binding_key(&[3; 32], &[2; 32]));
        assert_ne!(a, push_binding_key(&[1; 32], &[4; 32]));
        let mut message = b"lakko/ds/push-binding/v1\x00".to_vec();
        message.extend_from_slice(&[2; 32]);
        assert_eq!(a, hash::blake3_keyed(&[1; 32], &message));
    }

    #[test]
    fn statement_bytes_are_pinned() {
        let s = StatementSigner::from_seed(&[0x05; 32])
            .sign_at(
                &[0x06; 32],
                StatementBody::Claim {
                    epoch: 3,
                    epoch_key: pk(7),
                },
                1_700_000_000_000,
                [0xAB; NONCE_LEN],
            )
            .unwrap();
        let bytes = s.signed_bytes();
        assert!(bytes.starts_with(b"lakko/ds/statement/v1\x00\x01"));
        assert_eq!(bytes.len(), HEADER_LEN + 8 + 32);
        assert_eq!(hex(&hash::blake3(&bytes)), GOLDEN_CLAIM_BLAKE3);
    }

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }
}
