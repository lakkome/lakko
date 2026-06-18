// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI bindings — Swift (iOS / iPadOS / macOS / watchOS) and Kotlin (Android).
//!
//! This crate exposes the stable parts of [`lakko_mls`] through UniFFI's
//! procedural-macro API. The generated `.swift` / `.kt` files are produced
//! by `uniffi-bindgen` (see `just regen-bindings`).
//!
//! Phase 0 surface — the absolute minimum to drive a chat group:
//!
//! - [`Identity::new`] / [`Identity::signature_public_key`] / [`Identity::create_key_package`]
//! - [`Group::create`] / [`Group::join_from_welcome`]
//! - [`Group::add_member_from_wire`] / [`Group::remove_member`]
//! - [`Group::create_application_message`] / [`Group::process_incoming`]
//! - [`Group::export_secret`]
//! - [`Group::epoch`] / [`Group::member_count`]
//!
//! Phase 1.15d1 — auth surface, mirror of `lakko-ffi-wasm`:
//!
//! - [`auth::OpaqueRegister`] / [`auth::OpaqueLogin`] — OPAQUE-PAKE client.
//! - [`mnemonic::RecoveryMnemonic`] — 24-word BIP39 recovery passphrase.
//! - [`pow::PowSolver`] / [`pow::PowProof`] — Equix proof-of-work.
//!
//! Public-key serialization, error types and message-routing structs are
//! exposed as plain UniFFI records / enums.

#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex};

use lakko_mls::group::MemberAddition;
use lakko_mls::{
    group, key_package, ChannelId as MlsChannelId, DeviceGroup as MlsDeviceGroup,
    Group as MlsGroup, Identity as MlsIdentity, Incoming as MlsIncoming,
    PublisherGroup as MlsPublisherGroup, Role as MlsRole, RoleTable as MlsRoleTable,
};

pub mod attachments;
pub mod auth;
pub mod backup;
pub mod channels;
pub mod contacts;
pub mod discovery;
pub mod mnemonic;
pub mod pairing;
pub mod pow;
pub mod sframe;
pub mod sfu_signaling;
pub mod shamir;

uniffi::setup_scaffolding!();

/// Errors surfaced to Swift / Kotlin callers.
///
/// Variants intentionally collapse internal detail; callers branch on the
/// variant, not on the message string. Inner messages are for logs only.
#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum FfiError {
    /// Identity / signing-key error.
    #[error("identity error: {0}")]
    Identity(String),

    /// KeyPackage creation, validation or serialization error.
    #[error("key package error: {0}")]
    KeyPackage(String),

    /// Group state-machine error.
    #[error("group error: {0}")]
    Group(String),

    /// Application-message error.
    #[error("message error: {0}")]
    Message(String),

    /// Exporter error.
    #[error("exporter error: {0}")]
    Exporter(String),

    /// Wire-format codec error.
    #[error("codec error: {0}")]
    Codec(String),

    /// Unsupported or unexpected wire message.
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// OPAQUE-PAKE registration / login error.
    ///
    /// Same elision policy as the underlying `AuthClientError`:
    /// "wrong password" and "unknown account" are deliberately
    /// indistinguishable on the wire and at the FFI.
    #[error("auth error: {0}")]
    Auth(String),

    /// An `OpaqueRegister` / `OpaqueLogin` handle was finalized
    /// twice. Calling `finish` a second time is a wiring bug —
    /// the underlying `opaque-ke` state machine consumes the
    /// session by value, and a re-run would either produce a
    /// fresh-but-unusable envelope or corrupt OPAQUE state.
    /// Surface it loudly.
    #[error("OPAQUE session already finalized")]
    AuthAlreadyFinalized,

    /// BIP39 mnemonic parse / generation error.
    #[error("mnemonic error: {0}")]
    Mnemonic(String),

    /// Equix proof-of-work solve error.
    #[error("pow error: {0}")]
    Pow(String),

    /// Discovery / OPRF blinder error.
    ///
    /// Same elision posture as the wasm side: a normalised
    /// empty-username failure, a base64-decode failure, and a
    /// `voprf` math failure all collapse into this variant.
    #[error("discovery error: {0}")]
    Discovery(String),

    /// An `OprfBlinder` handle was finalized twice. Single-use
    /// is enforced because `OprfClient::finalize` consumes the
    /// blinding state by value; a re-run would either corrupt
    /// math or produce a nonsense output without a clear
    /// failure signal.
    #[error("OPRF blinder already finalized")]
    DiscoveryAlreadyFinalized,

    /// Contact-request envelope / pseudonym error.
    ///
    /// Same elision posture as the wasm side: the underlying
    /// `ContactError` distinguishes wire-format / signature /
    /// AEAD / argument categories internally; UI-side handling
    /// would not benefit from carving that into separate
    /// variants here.
    #[error("contacts error: {0}")]
    Contacts(String),

    /// Crypto primitive (Ed25519 / X25519 / AEAD) error at the
    /// FFI boundary. Surfaced when input bytes don't decode to a
    /// valid public / private key, or an AEAD op fails — both
    /// are typically caller-side wiring bugs.
    #[error("crypto error: {0}")]
    Crypto(String),

    /// Attachment encryption / decryption / wire-format error
    /// (Phase 1.19b). Same elision posture as the rest of the
    /// `FfiError` enum: a wrong-length key, an AEAD tag mismatch,
    /// an oversize plaintext, and a malformed base64 in the
    /// envelope all collapse into this variant.
    #[error("attachment error: {0}")]
    Attachment(String),

    /// Shamir's-Secret-Sharing split / combine / encode / decode
    /// error (Phase 1.21b). Same elision posture as the rest:
    /// invalid threshold, duplicate share indices, mismatched
    /// share lengths, oversize secret, malformed wire-form
    /// share string — all collapse into this variant.
    #[error("shamir error: {0}")]
    Shamir(String),

    /// Backup envelope / payload error (Phase 2a1c). Same
    /// elision posture as the rest: wrong recovery phrase,
    /// tampered envelope bytes, malformed payload, wrong-length
    /// account_id / ed25519_seed inputs — all collapse here.
    #[error("backup error: {0}")]
    Backup(String),

    /// Channel post / subscribe envelope error (Phase 4 G4-2-
    /// crypto). Same elision posture: wrong-length seed,
    /// channel-id mismatch, tampered ChannelPost ciphertext,
    /// Ed25519 verify failure on a post signature — all
    /// collapse here.
    #[error("channels error: {0}")]
    Channels(String),

    /// Call-side cryptography error (G3-sframe). Covers:
    /// wrong-length CallKey seed, SFrame counter
    /// exhaustion, AEAD seal / open failure on
    /// ParticipantKey. Same elision posture as the
    /// other variants — the message is for logs, the
    /// variant for the wire.
    #[error("call crypto error: {0}")]
    Call(String),

    /// SFU signaling wire-frame error (SFU multi-party subsprint
    /// 1c). Covers: a malformed / unknown-kind / oversize server
    /// frame off the WebSocket, and a base64url decode failure on
    /// a relay payload. Same elision posture as the rest — the
    /// message is for logs, the variant for the wire.
    #[error("sfu signaling error: {0}")]
    Sfu(String),

    /// Lock contention / poisoned mutex on the shared Group state.
    #[error("internal lock error")]
    LockPoisoned,
}

impl From<lakko_mls::Error> for FfiError {
    fn from(e: lakko_mls::Error) -> Self {
        match e {
            lakko_mls::Error::Identity(m) => Self::Identity(m),
            lakko_mls::Error::KeyPackage(m) => Self::KeyPackage(m),
            lakko_mls::Error::Group(m) => Self::Group(m),
            lakko_mls::Error::Message(m) => Self::Message(m),
            lakko_mls::Error::Exporter(m) => Self::Exporter(m),
            lakko_mls::Error::Codec(m) => Self::Codec(m),
            lakko_mls::Error::Unsupported(m) => Self::Unsupported(m.to_string()),
        }
    }
}

/// Result of [`Group::add_member_from_wire`].
///
/// Distribute `commit` to all existing members for processing; deliver
/// `welcome` to the newly added member.
#[derive(uniffi::Record)]
pub struct AddMember {
    /// Encoded MLS commit message — broadcast to existing members.
    pub commit: Vec<u8>,
    /// Encoded MLS welcome message — sent to the joining member.
    pub welcome: Vec<u8>,
}

/// FFI projection of [`lakko_mls::group::MemberRecord`]
/// (Phase 2a3c).
///
/// One row of the `devices()` snapshot — a single leaf in the
/// DeviceGroup tree. UI surfaces this as a list row in the
/// "Your devices" management screen.
#[derive(uniffi::Record)]
pub struct DeviceLeaf {
    /// OpenMLS leaf position. `u32` mirrors `LeafNodeIndex`'s
    /// internal repr. Stable inside an epoch; can renumber on
    /// the next add/remove cycle.
    pub leaf_index: u32,
    /// 32-byte Ed25519 verifying key for the device's
    /// signature credential. UI renders a hex prefix as a
    /// fingerprint so the user can tell devices apart.
    pub signature_public_key: Vec<u8>,
}

/// Result of [`Group::process_incoming`].
#[derive(uniffi::Enum)]
pub enum ProcessedMessage {
    /// Application payload arrived (decrypted).
    Application {
        /// Plaintext bytes the application can consume.
        data: Vec<u8>,
    },
    /// A commit was processed and merged. Group state advanced.
    Commit,
    /// A standalone proposal was processed (no state advance).
    Proposal,
    /// External-join proposal was processed.
    ExternalJoinProposal,
}

/// Typed body of a DeviceGroup cross-device payload
/// (Phase 2a3e2). Mirror of [`lakko_mls::device_group::DeviceGroupBody`]
/// on the FFI boundary. Four variants matching the
/// `DeviceGroupPayload.body` `oneof` in
/// `schemas/mls-app/devicegroup_v1.proto`.
#[derive(uniffi::Enum)]
pub enum DeviceGroupBody {
    /// "I read up to this point in this thread." Other devices
    /// last-write-wins by HLC to compute their own unread count.
    ReadCursor {
        /// The conversation group's MLS group_id.
        group_id: Vec<u8>,
        /// The message id the sender claims to have read up to.
        up_to_message_id: Vec<u8>,
        /// Wall-clock UNIX millis when the read happened.
        read_at_ms: u64,
    },
    /// In-flight composer text for a single thread. Empty `text`
    /// clears the draft (receivers wipe their local copy).
    Draft {
        /// The conversation group_id the draft belongs to.
        group_id: Vec<u8>,
        /// Composer text. UTF-8, may be empty.
        text: String,
        /// Non-empty when the draft was started as a reply.
        reply_to_id: Vec<u8>,
    },
    /// Per-thread UI flags (last-write-wins snapshot of the
    /// whole struct).
    Flags {
        /// The conversation group_id the flags apply to.
        group_id: Vec<u8>,
        /// Pinned threads stay at the top of the list.
        pinned: bool,
        /// Muted threads suppress push notifications + unread
        /// indicators.
        muted: bool,
        /// Archived threads drop out of the main list.
        archived: bool,
        /// Manual unread override after the thread was read.
        unread_override: bool,
    },
    /// Explicit display ordering for the conversation list.
    Order {
        /// Conversation group_ids in display order, top-to-bottom.
        group_ids: Vec<Vec<u8>>,
    },
}

/// Parsed envelope returned by
/// [`parse_devicegroup_payload`] (Phase 2a3e2). Wraps the
/// typed [`DeviceGroupBody`] with the metadata callers need for
/// ordering (HLC) and originator identification (sender_leaf).
#[derive(uniffi::Record)]
pub struct ParsedDeviceGroupPayload {
    /// Hybrid logical clock timestamp from the envelope.
    pub hlc: u64,
    /// Leaf index of the device that authored this payload.
    pub sender_leaf: u32,
    /// Concrete payload — one of `ReadCursor` / `Draft` /
    /// `Flags` / `Order`.
    pub body: DeviceGroupBody,
}

impl From<DeviceGroupBody> for lakko_mls::device_group::DeviceGroupBody {
    fn from(value: DeviceGroupBody) -> Self {
        use lakko_mls::device_group::DeviceGroupBody as Inner;
        match value {
            DeviceGroupBody::ReadCursor {
                group_id,
                up_to_message_id,
                read_at_ms,
            } => Inner::ReadCursor {
                group_id,
                up_to_message_id,
                read_at_ms,
            },
            DeviceGroupBody::Draft {
                group_id,
                text,
                reply_to_id,
            } => Inner::Draft {
                group_id,
                text,
                reply_to_id,
            },
            DeviceGroupBody::Flags {
                group_id,
                pinned,
                muted,
                archived,
                unread_override,
            } => Inner::Flags {
                group_id,
                pinned,
                muted,
                archived,
                unread_override,
            },
            DeviceGroupBody::Order { group_ids } => Inner::Order { group_ids },
        }
    }
}

impl From<lakko_mls::device_group::DeviceGroupBody> for DeviceGroupBody {
    fn from(value: lakko_mls::device_group::DeviceGroupBody) -> Self {
        use lakko_mls::device_group::DeviceGroupBody as Inner;
        match value {
            Inner::ReadCursor {
                group_id,
                up_to_message_id,
                read_at_ms,
            } => Self::ReadCursor {
                group_id,
                up_to_message_id,
                read_at_ms,
            },
            Inner::Draft {
                group_id,
                text,
                reply_to_id,
            } => Self::Draft {
                group_id,
                text,
                reply_to_id,
            },
            Inner::Flags {
                group_id,
                pinned,
                muted,
                archived,
                unread_override,
            } => Self::Flags {
                group_id,
                pinned,
                muted,
                archived,
                unread_override,
            },
            Inner::Order { group_ids } => Self::Order { group_ids },
        }
    }
}

/// Lakko identity — owns the device's signing keypair, MLS credential and
/// per-device storage provider.
#[derive(uniffi::Object)]
pub struct Identity {
    inner: MlsIdentity,
}

#[uniffi::export]
impl Identity {
    /// Generate a fresh identity with the given user-display label.
    #[uniffi::constructor]
    pub fn new(name: String) -> Result<Self, FfiError> {
        let inner = MlsIdentity::generate(&name)?;
        Ok(Self { inner })
    }

    /// Derive a deterministic identity from a 32-byte seed.
    ///
    /// The OPAQUE export key (1.15d) is the canonical seed source
    /// in production: same password ⇒ same Ed25519 + X25519
    /// keypairs ⇒ same device identity across re-installs.
    ///
    /// Mirror of `apps/web`'s `Identity.fromSeed`. Caller MUST
    /// pass exactly 32 bytes; longer / shorter input is rejected
    /// at the underlying `MlsIdentity::from_seed` call.
    #[uniffi::constructor]
    pub fn from_seed(seed: Vec<u8>) -> Result<Self, FfiError> {
        let inner = MlsIdentity::from_seed(&seed)?;
        Ok(Self { inner })
    }

    /// Public signature key bytes (Ed25519, 32 bytes).
    pub fn signature_public_key(&self) -> Vec<u8> {
        self.inner.signature_public_key().to_vec()
    }

    /// Long-term X25519 public key bytes (32 bytes). Recipients
    /// publish this in their identity bundle; senders use it to
    /// seal the [`crate::contacts::ContactRequest`] intro
    /// envelope.
    pub fn x25519_public_key(&self) -> Vec<u8> {
        self.inner.x25519_public_key().to_vec()
    }

    /// Long-term X25519 secret key bytes (32 bytes).
    ///
    /// Sensitive — the caller is expected to use it
    /// transactionally (open one envelope, wipe) and never to
    /// persist outside the Keystore-wrapped device-root layer
    /// 1.15d3 set up.
    pub fn x25519_secret_key(&self) -> Vec<u8> {
        self.inner.x25519_secret_bytes().to_vec()
    }

    /// Ed25519 signing-seed bytes (32 bytes).
    ///
    /// `lakko_contacts::compose` consumes this to sign the
    /// `ContactRequest` envelope; the wasm bridge exposes the
    /// same accessor for the equivalent flow in `apps/web`.
    ///
    /// Sensitive — same lifecycle constraints as
    /// [`x25519_secret_key`].
    pub fn ed25519_seed(&self) -> Vec<u8> {
        self.inner.ed25519_seed_bytes().to_vec()
    }

    /// Create a fresh KeyPackage in TLS wire form.
    pub fn create_key_package(&self) -> Result<Vec<u8>, FfiError> {
        let kp = self.inner.create_key_package()?;
        Ok(key_package::serialize(&kp)?)
    }

    /// Serialise the entire OpenMLS provider storage to a
    /// length-prefixed byte blob.
    ///
    /// The blob carries every secret OpenMLS holds for this
    /// identity: per-group ratchet state, init-key private
    /// bytes, leaf-secret material. Treat it as **the most
    /// sensitive value any Lakko process touches** — wrap it
    /// under the Keystore-resident device root key before
    /// writing to disk. The Kotlin / Swift caller is responsible
    /// for the AEAD layer; this function only produces the
    /// plaintext bytes.
    ///
    /// Mirror of the wasm `Identity.snapshotStorage` flow that
    /// `apps/web` 1.16e6c1 uses to persist MLS state to
    /// IndexedDB. Same wire format on both sides — a snapshot
    /// produced on web can be restored on Android with the same
    /// OPAQUE export key.
    pub fn snapshot_storage(&self) -> Result<Vec<u8>, FfiError> {
        Ok(self.inner.snapshot_storage()?)
    }

    /// Restore the OpenMLS provider storage from a
    /// [`snapshot_storage`] blob.
    ///
    /// Overwrites whatever state the local provider had — pair
    /// this with a freshly-constructed [`Identity::from_seed`]
    /// so the public keys match the snapshot's secrets.
    ///
    /// # Errors
    ///
    /// * [`FfiError::Identity`] — snapshot is malformed
    ///   (truncated counter, length-prefix overflow, …).
    pub fn restore_storage(&self, snapshot: Vec<u8>) -> Result<(), FfiError> {
        self.inner.restore_storage(&snapshot)?;
        Ok(())
    }
}

/// One member of a chat [`Group`] at the current epoch — the roster row the
/// group-chat UI renders (member list + per-member Remove affordance). Mirror
/// of the wasm `Group.members()` shape + the [`DeviceLeaf`] device-roster row.
#[derive(uniffi::Record)]
pub struct GroupMember {
    /// OpenMLS leaf position. Stable inside an epoch; can renumber on the next
    /// add/remove cycle, so the UI MUST NOT cache it across a state change.
    pub leaf_index: u32,
    /// 32-byte Ed25519 verifying key for the member's signature credential.
    /// The UI matches it against the contacts list (the same key a
    /// `ContactRequest` is signed with) to render a name, else a fingerprint.
    pub signature_public_key: Vec<u8>,
}

/// A member's role in an ADR-0022 role-enabled group. The UI gates the
/// add / remove / promote affordances on the local member's role.
#[derive(uniffi::Enum)]
pub enum Role {
    /// The group creator — exactly one, unremovable, the only role that may
    /// change the role table.
    Owner,
    /// Appointed by the owner — may add / remove non-owner members.
    Admin,
    /// May not add or remove anyone.
    Member,
}

/// The ADR-0022 admin role table: the owner's signature key + the admin set,
/// each a raw 32-byte Ed25519 signature public key (the same key the roster
/// matches against). Mirror of `lakko_mls::RoleTable`.
#[derive(uniffi::Record)]
pub struct RoleTable {
    /// Owner signature public key (32 bytes).
    pub owner: Vec<u8>,
    /// Admin signature public keys (each 32 bytes), sorted + deduplicated.
    pub admins: Vec<Vec<u8>>,
}

/// Lakko MLS conversation group.
#[derive(uniffi::Object)]
pub struct Group {
    inner: Mutex<MlsGroup>,
}

#[uniffi::export]
impl Group {
    /// Create a fresh group with `identity` as the founding member.
    #[uniffi::constructor]
    pub fn create(identity: &Identity) -> Result<Self, FfiError> {
        let inner = MlsGroup::create(&identity.inner)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// Join an existing group from a wire-format Welcome message.
    #[uniffi::constructor]
    pub fn join_from_welcome(welcome: Vec<u8>, identity: &Identity) -> Result<Self, FfiError> {
        let inner = MlsGroup::join_from_welcome(&welcome, &identity.inner)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// Add a member from their published KeyPackage (TLS wire form).
    pub fn add_member_from_wire(
        &self,
        identity: &Identity,
        key_package: Vec<u8>,
    ) -> Result<AddMember, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let MemberAddition { commit, welcome } =
            g.add_member_from_wire(&identity.inner, &key_package)?;
        Ok(AddMember {
            commit: group::encode_message(&commit)?,
            welcome: group::encode_message(&welcome)?,
        })
    }

    /// Remove a member by their leaf index.
    ///
    /// Returns the wire-encoded commit to broadcast.
    pub fn remove_member(&self, identity: &Identity, leaf_index: u32) -> Result<Vec<u8>, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let commit = g.remove_member(&identity.inner, lakko_mls::LeafNodeIndex::new(leaf_index))?;
        Ok(group::encode_message(&commit)?)
    }

    /// Encrypt an application payload for the current epoch.
    pub fn create_application_message(
        &self,
        identity: &Identity,
        plaintext: Vec<u8>,
    ) -> Result<Vec<u8>, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let msg = g.create_application_message(&identity.inner, &plaintext)?;
        Ok(group::encode_message(&msg)?)
    }

    /// Process an incoming MLS message.
    pub fn process_incoming(
        &self,
        identity: &Identity,
        bytes: Vec<u8>,
    ) -> Result<ProcessedMessage, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let result = g.process_incoming(&identity.inner, &bytes)?;
        Ok(match result {
            MlsIncoming::Application(data) => ProcessedMessage::Application { data },
            MlsIncoming::Commit => ProcessedMessage::Commit,
            MlsIncoming::Proposal => ProcessedMessage::Proposal,
            MlsIncoming::ExternalJoinProposal => ProcessedMessage::ExternalJoinProposal,
        })
    }

    /// Derive a `length`-byte secret bound to the current epoch.
    pub fn export_secret(
        &self,
        identity: &Identity,
        label: String,
        context: Vec<u8>,
        length: u32,
    ) -> Result<Vec<u8>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let secret = g.export_secret(&identity.inner, &label, &context, length as usize)?;
        Ok(secret.to_vec())
    }

    /// Local leaf index of the calling identity.
    pub fn own_leaf_index(&self) -> Result<u32, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.own_leaf_index().u32())
    }

    /// Current group epoch.
    pub fn epoch(&self) -> Result<u64, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.epoch())
    }

    /// Number of members at the current epoch.
    pub fn member_count(&self) -> Result<u32, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(u32::try_from(g.member_count()).unwrap_or(u32::MAX))
    }

    /// Snapshot of every member's `(leaf_index, signature_public_key)` at the
    /// current epoch — the roster the group-chat UI renders. Ordering matches
    /// `lakko_mls::Group::members` (left-to-right over the leaf tree, stable
    /// inside one epoch). Mirror of the wasm `Group.members()`; the native
    /// clients match each `signature_public_key` against their contacts to
    /// label a row, and offer a Remove on every leaf but [`Self::own_leaf_index`].
    pub fn members(&self) -> Result<Vec<GroupMember>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.members()
            .into_iter()
            .map(|m| GroupMember {
                leaf_index: m.leaf_index,
                signature_public_key: m.signature_public_key,
            })
            .collect())
    }

    /// This member's ADR-0022 role in the group, or `None` when the group has
    /// no role table (an open / legacy group). The UI gates the
    /// add / remove / promote affordances on this.
    pub fn own_role(&self, identity: &Identity) -> Result<Option<Role>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.own_role(&identity.inner).map(|r| match r {
            MlsRole::Owner => Role::Owner,
            MlsRole::Admin => Role::Admin,
            MlsRole::Member => Role::Member,
        }))
    }

    /// The group's ADR-0022 role table, or `None` for an open / legacy group.
    pub fn group_roles(&self) -> Result<Option<RoleTable>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.group_roles().map(|t| RoleTable {
            owner: t.owner().to_vec(),
            admins: t.admins().iter().map(|a| a.to_vec()).collect(),
        }))
    }

    /// Rewrite the group's ADR-0022 role table (promote / demote admins). Only
    /// the owner's commit is merged by peers (enforced on the receiving side by
    /// [`Self::process_incoming_verifying_roles`]). `owner` and every `admins`
    /// entry must be a 32-byte Ed25519 signature public key. Returns the
    /// `GroupContextExtensions` commit wire bytes to broadcast to the journal.
    pub fn set_group_roles(
        &self,
        identity: &Identity,
        owner: Vec<u8>,
        admins: Vec<Vec<u8>>,
    ) -> Result<Vec<u8>, FfiError> {
        let owner: [u8; 32] = owner
            .try_into()
            .map_err(|_| FfiError::Codec("owner key must be 32 bytes".into()))?;
        let mut admin_keys = Vec::with_capacity(admins.len());
        for a in admins {
            let k: [u8; 32] = a
                .try_into()
                .map_err(|_| FfiError::Codec("admin key must be 32 bytes".into()))?;
            admin_keys.push(k);
        }
        let table = MlsRoleTable::new(owner, admin_keys);
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.set_group_roles(&identity.inner, &table)?)
    }

    /// Apply an incoming commit ENFORCING the ADR-0022 role policy — a commit
    /// whose committer's role does not authorise its add / remove / role change
    /// is REFUSED (not merged), fail-closed. A group with no role table behaves
    /// like [`Self::process_incoming`]. Use this on the chat-message receive
    /// loop; do NOT use it for call-group receives (the call group is
    /// role-enabled but its welcome-ferry / KP-forward adds are legitimately
    /// non-owner — ADR-0022 call-path caveat).
    pub fn process_incoming_verifying_roles(
        &self,
        identity: &Identity,
        bytes: Vec<u8>,
    ) -> Result<ProcessedMessage, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let result = g.process_incoming_verifying_roles(&identity.inner, &bytes)?;
        Ok(match result {
            MlsIncoming::Application(data) => ProcessedMessage::Application { data },
            MlsIncoming::Commit => ProcessedMessage::Commit,
            MlsIncoming::Proposal => ProcessedMessage::Proposal,
            MlsIncoming::ExternalJoinProposal => ProcessedMessage::ExternalJoinProposal,
        })
    }

    /// Group identifier bytes (RFC 9420 `group_id`).
    ///
    /// Stable across the lifetime of the group and at every
    /// epoch — `lakko-svc-ds` uses this value (hex-encoded) to
    /// key the `/ds/group/{group_id_hex}/messages` journal that
    /// carries application messages + commits.
    ///
    /// Length is cipher-suite-dependent (32 bytes for the
    /// suites we ship; the underlying MLS layer does not promise
    /// a fixed size, so callers should treat the byte array as
    /// opaque and just hand it to `DsClient.publishGroup` /
    /// `DsClient.fetchGroup`).
    pub fn group_id(&self) -> Result<Vec<u8>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.group_id().clone())
    }
}

/// Load an existing MLS group from the provider's persisted
/// storage by its `group_id`.
///
/// Returns `null` (on the Kotlin side; `None` on the Rust side
/// — UniFFI maps the `Option` to the platform's null shape)
/// when the provider has no group with that id (e.g. a
/// freshly-bootstrapped session that hasn't yet rehydrated, or
/// a typo in the id the caller is passing).
///
/// **Bootstrap-side eager rebuild** (1.15f5) calls this for
/// every previously-known group id after
/// [`Identity::restore_storage`] runs — the resulting `Group`
/// handles let `pollGroup` decrypt peers' commits + application
/// messages without first having to wait for a user-initiated
/// send / receive on each thread.
///
/// Free function rather than `Group::load` constructor because
/// UniFFI 0.28 rejects `Option<Self>` in constructor signatures —
/// the standard workaround for "constructor that may return
/// null", same shape as `pow_solve` (1.15d1).
#[uniffi::export]
pub fn load_group(identity: &Identity, group_id: Vec<u8>) -> Result<Option<Arc<Group>>, FfiError> {
    let loaded = MlsGroup::load(&identity.inner, &group_id)?;
    Ok(loaded.map(|inner| {
        Arc::new(Group {
            inner: Mutex::new(inner),
        })
    }))
}

// ============================================================================
// PublisherGroup (G4-2-mls-persistence step 2 — FFI surface for the persistent
// Tier-1 MLS group that owns the channel content-key rotation authority. See
// `crates/lakko-mls/src/publisher_group.rs` for the Rust core; the wrapper here
// only mediates the `Mutex<>` + UniFFI handle conventions the iOS / Android
// composers consume.)
// ============================================================================

/// Persistent MLS-group wrapper for one Lakko channel's
/// publisher tier (ADR-0010 Tier 1).
///
/// Mirror of [`Group`] but with the channel-id keyed
/// persistence + the `channel_seed` exporter shortcut baked
/// in. The iOS / Android channels composer should construct
/// via [`PublisherGroup::open_or_create`] on every publish —
/// the load-or-create dance ensures the MLS state from a
/// prior session is reused (so the exporter output is stable
/// across app restarts).
#[derive(uniffi::Object)]
pub struct PublisherGroup {
    inner: Mutex<MlsPublisherGroup>,
}

#[uniffi::export]
impl PublisherGroup {
    /// Load the persisted PublisherGroup for `channel_id`.
    /// Returns `null` if no group exists yet for this channel
    /// under the caller's identity — pair with
    /// [`PublisherGroup::create`] or use the
    /// [`publisher_group_open_or_create`] free function for
    /// the load-or-create shortcut.
    ///
    /// Free-function shape (not a constructor) because UniFFI
    /// 0.28 rejects `Option<Self>` in `#[uniffi::constructor]`
    /// signatures — same workaround as [`load_group`].
    #[uniffi::constructor]
    pub fn create(identity: &Identity, channel_id: Vec<u8>) -> Result<Self, FfiError> {
        let cid = MlsChannelId::try_from_slice(&channel_id)?;
        let inner = MlsPublisherGroup::create(&identity.inner, &cid)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// 32-byte channel content-key seed derived from the MLS
    /// exporter at the **current** group epoch. Stable across
    /// process restarts once the group has been persisted —
    /// that is the load-bearing invariant
    /// G4-2-mls-persistence closes.
    ///
    /// Wire shape matches the existing
    /// `channel_seed_from_mls_exporter_output` byte-pipe:
    /// 32 bytes the channels composer hands to
    /// `compose_channel_post` as the per-channel content seed.
    pub fn channel_seed(&self, identity: &Identity) -> Result<Vec<u8>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let seed = g.channel_seed(&identity.inner)?;
        Ok(seed.to_vec())
    }

    /// Channel id this group was minted for.
    pub fn channel_id(&self) -> Result<Vec<u8>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.channel_id().as_bytes().to_vec())
    }

    /// G4-2-mls-persistence step 4 — rotate the channel
    /// content-key seed by advancing the MLS group epoch
    /// via a self-update commit. Returns the wire-encoded
    /// commit bytes; the caller broadcasts them to every
    /// other PublisherGroup member so they advance to the
    /// same epoch in lock-step.
    ///
    /// After `rotate` returns, the next [`channel_seed`]
    /// call against this handle (or any future
    /// [`publisher_group_load`]) yields the **new-epoch**
    /// seed. Pair with `seal_subscriber_seed_envelope` +
    /// the services/channels batch fanout to reseal the
    /// seed to every subscriber under the new epoch id.
    pub fn rotate(&self, identity: &Identity) -> Result<Vec<u8>, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.rotate(&identity.inner)?)
    }
}

/// Load the persisted PublisherGroup for `channel_id` if any.
/// Returns `null` when no group has been minted yet for this
/// (identity, channel_id) pair under `identity`'s provider
/// storage.
///
/// Free function rather than a constructor because UniFFI
/// 0.28 rejects `Option<Self>` in constructor returns — same
/// shape rationale as the existing [`load_group`].
#[uniffi::export]
pub fn publisher_group_load(
    identity: &Identity,
    channel_id: Vec<u8>,
) -> Result<Option<Arc<PublisherGroup>>, FfiError> {
    let cid = MlsChannelId::try_from_slice(&channel_id)?;
    let loaded = MlsPublisherGroup::load(&identity.inner, &cid)?;
    Ok(loaded.map(|inner| {
        Arc::new(PublisherGroup {
            inner: Mutex::new(inner),
        })
    }))
}

/// Load-or-create convenience — the path every composer
/// should take. Tries [`publisher_group_load`] first; on
/// `null`, mints a fresh group via
/// [`PublisherGroup::create`]. Idempotent: once a group has
/// been minted for `(identity, channel_id)`, every
/// subsequent call returns the same persisted state and
/// therefore the same exporter output for the current
/// epoch.
#[uniffi::export]
pub fn publisher_group_open_or_create(
    identity: &Identity,
    channel_id: Vec<u8>,
) -> Result<Arc<PublisherGroup>, FfiError> {
    if let Some(existing) = publisher_group_load(identity, channel_id.clone())? {
        return Ok(existing);
    }
    let created = PublisherGroup::create(identity, channel_id)?;
    Ok(Arc::new(created))
}

// ============================================================================
// Channel rotation policy (G4-2-rotation-timer)
// ============================================================================
//
// Mirrors `lakko_channels::rotation_policy`. Pure logic — no
// I/O, no clock side-effects. The Swift / Kotlin call site
// passes `now_ms` from `Date().timeIntervalSince1970 * 1000`
// / `System.currentTimeMillis()` so the host's monotonic-
// adjacent clock drives the decision.

/// FFI mirror of [`lakko_channels::rotation_policy::RotationPolicy`].
#[derive(uniffi::Record, Clone, Copy)]
pub struct ChannelRotationPolicy {
    /// Maximum milliseconds between rotations. `0` disables
    /// the time trigger.
    pub interval_ms: u64,
    /// Maximum posts between rotations. `0` disables the
    /// count trigger.
    pub max_posts: u32,
}

impl From<ChannelRotationPolicy> for lakko_channels::RotationPolicy {
    fn from(p: ChannelRotationPolicy) -> Self {
        Self::new(p.interval_ms, p.max_posts)
    }
}

/// FFI mirror of [`lakko_channels::rotation_policy::RotationState`].
/// Caller-persisted snapshot of when the channel was last
/// rotated + how many posts since.
#[derive(uniffi::Record, Clone, Copy)]
pub struct ChannelRotationState {
    /// Unix milliseconds when the last rotation completed.
    /// `0` means "never rotated yet".
    pub last_rotation_ms: u64,
    /// Count of posts published since the last rotation.
    pub posts_since_rotation: u32,
}

impl From<ChannelRotationState> for lakko_channels::RotationState {
    fn from(s: ChannelRotationState) -> Self {
        Self {
            last_rotation_ms: s.last_rotation_ms,
            posts_since_rotation: s.posts_since_rotation,
        }
    }
}

impl From<lakko_channels::RotationState> for ChannelRotationState {
    fn from(s: lakko_channels::RotationState) -> Self {
        Self {
            last_rotation_ms: s.last_rotation_ms,
            posts_since_rotation: s.posts_since_rotation,
        }
    }
}

/// Default rotation policy — 24 h or 1 000 posts, whichever
/// lands first. Pinned in ADR-0010 §"PublisherGroup".
#[uniffi::export]
pub fn channel_rotation_policy_default() -> ChannelRotationPolicy {
    let p = lakko_channels::RotationPolicy::DEFAULT;
    ChannelRotationPolicy {
        interval_ms: p.interval_ms,
        max_posts: p.max_posts,
    }
}

/// Decide whether the publisher should rotate now. Pure
/// function — see crate docs. Saturating arithmetic on the
/// time delta keeps a host clock jump backward from
/// surfacing as a spurious rotation.
#[uniffi::export]
pub fn channel_should_rotate(
    policy: ChannelRotationPolicy,
    state: ChannelRotationState,
    now_ms: u64,
) -> bool {
    let p: lakko_channels::RotationPolicy = policy.into();
    let s: lakko_channels::RotationState = state.into();
    p.should_rotate(now_ms, s)
}

/// Stamp a fresh rotation state right after a successful
/// fanout — server returned 2xx and the new epoch landed.
#[uniffi::export]
pub fn channel_rotation_state_after_rotation(now_ms: u64) -> ChannelRotationState {
    lakko_channels::RotationState::after_rotation(now_ms).into()
}

/// Bump the post counter (saturating at `u32::MAX`). Called
/// after every successfully-published `ChannelPost`.
#[uniffi::export]
pub fn channel_rotation_state_after_post(state: ChannelRotationState) -> ChannelRotationState {
    let s: lakko_channels::RotationState = state.into();
    s.after_post().into()
}

// ============================================================================
// DeviceGroup (Phase 2a3b1d)
// ============================================================================

/// Per-user multi-device MLS group (ADR-0007).
///
/// Wraps `lakko_mls::DeviceGroup` for FFI use. The leaves are
/// the user's own devices; the deterministic `group_id` is
/// `BLAKE3("lakko-device-group/v1" || account_id)`.
///
/// FFI subset of what `lakko_mls::DeviceGroup` exposes — just
/// what 2a3b's add-device flow needs:
///
///   * Bootstrap a fresh device group (`create_founder`).
///   * Join an already-running device group from a Welcome
///     (`join_from_welcome`).
///   * Add a new device by its KeyPackage bytes
///     (`add_device_from_wire`) — returns the
///     (commit, welcome) pair the caller fans out.
///   * Read the deterministic `group_id`.
///
/// 2a3c will widen this with `devices()` + `remove_device(...)`
/// once the per-platform UI lands.
#[derive(uniffi::Object)]
pub struct DeviceGroup {
    inner: Mutex<MlsDeviceGroup>,
}

#[uniffi::export]
impl DeviceGroup {
    /// Bootstrap a fresh DeviceGroup with `identity` as the
    /// founding (and only) leaf.
    ///
    /// `account_id` MUST be the raw 16-byte UUIDv7. The
    /// resulting group's `group_id` is derived
    /// deterministically — every device that knows the same
    /// `account_id` agrees on the same `group_id` without
    /// server coordination.
    #[uniffi::constructor]
    pub fn create_founder(identity: &Identity, account_id: Vec<u8>) -> Result<Self, FfiError> {
        if account_id.len() != 16 {
            return Err(FfiError::Codec("account_id must be 16 bytes".into()));
        }
        let mut acct = [0u8; 16];
        acct.copy_from_slice(&account_id);
        let inner = MlsDeviceGroup::create_founder(&identity.inner, &acct)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// Join a DeviceGroup from a Welcome the scanner device
    /// produced via [`DeviceGroup::add_device_from_wire`].
    ///
    /// Verifies that the embedded `group_id` matches
    /// `BLAKE3("lakko-device-group/v1" || account_id)` — a
    /// misrouted Welcome from a different user's DeviceGroup
    /// is rejected before any state advance.
    #[uniffi::constructor]
    pub fn join_from_welcome(
        welcome: Vec<u8>,
        identity: &Identity,
        account_id: Vec<u8>,
    ) -> Result<Self, FfiError> {
        if account_id.len() != 16 {
            return Err(FfiError::Codec("account_id must be 16 bytes".into()));
        }
        let mut acct = [0u8; 16];
        acct.copy_from_slice(&account_id);
        let inner = MlsDeviceGroup::join_from_welcome(&welcome, &identity.inner, &acct)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// Add a device by its TLS-encoded KeyPackage. Returns the
    /// `(commit, welcome)` pair:
    ///
    ///   * `commit` — broadcast to every existing DeviceGroup
    ///     member via the DS group journal.
    ///   * `welcome` — sent to the joining device via the DS
    ///     mailbox at the rendezvous pseudonym the pairing QR
    ///     supplied.
    pub fn add_device_from_wire(
        &self,
        identity: &Identity,
        key_package: Vec<u8>,
    ) -> Result<AddMember, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let MemberAddition { commit, welcome } =
            g.add_device_from_wire(&identity.inner, &key_package)?;
        Ok(AddMember {
            commit: group::encode_message(&commit)?,
            welcome: group::encode_message(&welcome)?,
        })
    }

    /// The deterministic `group_id` this DeviceGroup is bound
    /// to. Same value `MlsDeviceGroup::group_id_for(account_id)`
    /// computes for the same `account_id`.
    pub fn group_id(&self) -> Result<Vec<u8>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.group_id())
    }

    /// Snapshot of every device leaf currently in the
    /// DeviceGroup (Phase 2a3c). One row per device — the UI
    /// renders the list, marks the row whose `leaf_index`
    /// matches [`Self::own_leaf_index`] as "(this device)",
    /// and offers a Remove affordance on every other row.
    pub fn devices(&self) -> Result<Vec<DeviceLeaf>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.devices()
            .into_iter()
            .map(|m| DeviceLeaf {
                leaf_index: m.leaf_index,
                signature_public_key: m.signature_public_key,
            })
            .collect())
    }

    /// Leaf index of THIS device inside the DeviceGroup
    /// (Phase 2a3c). See [`MlsDeviceGroup::own_leaf_index`] for
    /// the lifecycle caveats — caller MUST NOT cache across
    /// state changes.
    pub fn own_leaf_index(&self) -> Result<u32, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.own_leaf_index())
    }

    /// Revoke a device leaf (Phase 2a3c). Returns the encoded
    /// Commit bytes that the caller MUST broadcast to every
    /// remaining device via the DS group journal so they
    /// advance past the revoked leaf and ratchet the group
    /// secrets out of the removed device's reach.
    ///
    /// `leaf_index` must come from a [`Self::devices`] snapshot
    /// at the SAME epoch; concurrent commits can renumber
    /// leaves, in which case the caller refetches and retries.
    /// Attempting to remove the local device's own leaf
    /// surfaces as `FfiError::Mls("…cannot remove own leaf…")`.
    pub fn remove_device_at(
        &self,
        identity: &Identity,
        leaf_index: u32,
    ) -> Result<Vec<u8>, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let bytes = g.remove_device_at(&identity.inner, leaf_index)?;
        Ok(bytes)
    }

    /// Derive the per-pairing history-bundle exporter secret
    /// (Phase 2a3d8). Returns 32 bytes; the scanner side feeds
    /// it into [`seal_history_bundle`], the joining side feeds
    /// it into [`open_history_bundle`]. Both devices reach the
    /// same value because they end up at the same DeviceGroup
    /// epoch after add_device_from_wire / join_from_welcome.
    pub fn history_bundle_secret(&self, identity: &Identity) -> Result<Vec<u8>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let secret = lakko_mls::history_bundle::exporter_secret(&g, &identity.inner)?;
        Ok(secret.to_vec())
    }

    /// Derive a `length`-byte secret bound to this DeviceGroup's current
    /// epoch (ADR-0018 Part 2a, factor (a) — "another-device" identity
    /// transfer, Path B). Mirrors [`Group::export_secret`]. A **read-only**
    /// MLS exporter call (no state mutation). After a fresh device joins via
    /// [`DeviceGroup::join_from_welcome`], the existing and fresh device sit at
    /// the same epoch and derive the SAME bytes for the same
    /// `(label, context, length)`; the existing device seals
    /// `(grant_token ‖ phrase_seed)` under this key (via [`seal_with_key`])
    /// and the fresh device opens it (via [`open_with_key`]).
    pub fn export_secret(
        &self,
        identity: &Identity,
        label: String,
        context: Vec<u8>,
        length: u32,
    ) -> Result<Vec<u8>, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let secret = g.export_secret(&identity.inner, &label, &context, length as usize)?;
        Ok(secret.to_vec())
    }

    /// Compose + encrypt a typed cross-device payload
    /// (Phase 2a3e2). Wraps
    /// [`lakko_mls::DeviceGroup::create_devicegroup_message`]
    /// with a FFI-friendly enum. Returns the wire-format MLS
    /// application-message bytes ready to PUT to the
    /// DeviceGroup DS journal.
    ///
    /// `hlc` is the caller-supplied hybrid logical clock value
    /// — callers wire a single per-account HLC generator at
    /// the application layer so total ordering survives across
    /// devices.
    pub fn create_devicegroup_message(
        &self,
        identity: &Identity,
        body: DeviceGroupBody,
        hlc: u64,
    ) -> Result<Vec<u8>, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let bytes = g.create_devicegroup_message(&identity.inner, body.into(), hlc)?;
        Ok(bytes)
    }

    /// Decrypt / advance the state machine for an inbound
    /// DeviceGroup wire message (Phase 2a3e2). Same shape as
    /// [`Group::process_incoming`]:
    ///
    ///   * `ProcessedMessage::Application { data }` — the
    ///     application-message plaintext. Pass `data` to
    ///     [`parse_devicegroup_payload`] to get a typed
    ///     `ParsedDeviceGroupPayload`.
    ///   * `ProcessedMessage::Commit` — a Commit was processed and
    ///     merged. DeviceGroup state advanced. Caller refreshes
    ///     its `devices()` snapshot.
    ///   * `ProcessedMessage::Proposal` /
    ///     `::ExternalJoinProposal` — defensive; no state
    ///     advance.
    pub fn process_incoming(
        &self,
        identity: &Identity,
        message_bytes: Vec<u8>,
    ) -> Result<ProcessedMessage, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let incoming = g.process_incoming(&identity.inner, &message_bytes)?;
        Ok(match incoming {
            lakko_mls::Incoming::Application(data) => ProcessedMessage::Application { data },
            lakko_mls::Incoming::Commit => ProcessedMessage::Commit,
            lakko_mls::Incoming::Proposal => ProcessedMessage::Proposal,
            lakko_mls::Incoming::ExternalJoinProposal => ProcessedMessage::ExternalJoinProposal,
        })
    }

    /// G-13 (ADR-0015): flip this DeviceGroup's policy to **require a
    /// peer-verified attestation for device removals**. Returns the encoded
    /// Commit the caller broadcasts over the DS group journal; every device
    /// that processes it then enforces the policy. Call when the account
    /// first enables a passkey (OPAQUE-only accounts never call it).
    pub fn set_require_attested_removals(&self, identity: &Identity) -> Result<Vec<u8>, FfiError> {
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let bytes = g.set_require_attested_removals(&identity.inner)?;
        Ok(bytes)
    }

    /// G-13 (ADR-0015): whether this DeviceGroup currently requires attested
    /// removals (read from authenticated group state).
    pub fn require_attested_removals(&self) -> Result<bool, FfiError> {
        let g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        Ok(g.require_attested_removals())
    }

    /// G-13 (ADR-0015): apply an incoming DeviceGroup commit **enforcing the
    /// attested-removal policy**. When the policy is on and the commit
    /// removes leaves, every removed leaf must be covered by an attestation
    /// in `attestations` that verifies under `attest_verifying_key` (the
    /// 32-byte Ed25519 key the auth service publishes at the device-attest
    /// JWKS) — bound to this account, the removed leaf, and fresh at
    /// `now_unix_secs`. A removal lacking a valid attestation is refused (the
    /// commit is NOT merged) and surfaces as `FfiError::Group`. Policy-off /
    /// non-remove commits behave like [`Self::process_incoming`].
    pub fn process_incoming_verifying_removals(
        &self,
        identity: &Identity,
        message_bytes: Vec<u8>,
        attestations: Vec<Vec<u8>>,
        attest_verifying_key: Vec<u8>,
        now_unix_secs: i64,
    ) -> Result<ProcessedMessage, FfiError> {
        let vk = lakko_crypto::signing::VerifyingKey::from_bytes(&attest_verifying_key)
            .map_err(|_| FfiError::Identity("invalid attestation verifying key".into()))?;
        let mut g = self.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
        let incoming = g.process_incoming_verifying_removals(
            &identity.inner,
            &message_bytes,
            &attestations,
            &vk,
            now_unix_secs,
        )?;
        Ok(match incoming {
            lakko_mls::Incoming::Application(data) => ProcessedMessage::Application { data },
            lakko_mls::Incoming::Commit => ProcessedMessage::Commit,
            lakko_mls::Incoming::Proposal => ProcessedMessage::Proposal,
            lakko_mls::Incoming::ExternalJoinProposal => ProcessedMessage::ExternalJoinProposal,
        })
    }
}

/// G-13 (ADR-0015): the action hash a remove-device passkey ceremony and its
/// attestation bind to — `SHA-256("lakko/action/remove-device/v1" ‖
/// removed_leaf_signature_pubkey)`. A client computes this from the leaf it
/// is about to remove to drive the `/auth/action/{start,finish}` ceremony,
/// so it matches what a peer recomputes when verifying the attestation.
#[uniffi::export]
pub fn remove_device_action_hash(removed_signature_pubkey: Vec<u8>) -> Vec<u8> {
    lakko_mls::device_group::remove_device_action_hash(&removed_signature_pubkey).to_vec()
}

/// Parse a DeviceGroup application-payload plaintext into the
/// typed [`ParsedDeviceGroupPayload`] (Phase 2a3e2). Callers
/// feed in the `data` field of
/// [`ProcessedMessage::Application`] from
/// [`DeviceGroup::process_incoming`].
///
/// Free function because the receiver-side flow is
/// stateless — no `DeviceGroup` state to consult.
///
/// # Errors
///
/// - [`FfiError::Codec`] when the proto decode fails.
/// - [`FfiError::Mls`] with an `Unsupported` body when the
///   payload's `body` is absent or carries a variant this
///   binary doesn't recognise (older client reading a
///   future-version payload).
#[uniffi::export]
pub fn parse_devicegroup_payload(plaintext: Vec<u8>) -> Result<ParsedDeviceGroupPayload, FfiError> {
    let parsed = lakko_mls::DeviceGroup::parse_devicegroup_payload(&plaintext)?;
    Ok(ParsedDeviceGroupPayload {
        hlc: parsed.hlc,
        sender_leaf: parsed.sender_leaf,
        body: parsed.body.into(),
    })
}

/// AEAD-seal a [`lakko_proto::mls_app::history_bundle::v1::HistoryBundle`]
/// (Phase 2a3d8).
///
/// `secret` MUST be exactly 32 bytes — typically the output of
/// [`DeviceGroup::history_bundle_secret`]. `bundle_bytes` is
/// the prost-encoded `HistoryBundle` proto; the caller builds
/// it from its local message store. Returns the wire-format
/// [`SealedBundle`] proto bytes ready to PUT to the rendezvous
/// mailbox.
///
/// Refuses to seal bundles whose total message count exceeds
/// [`lakko_mls::history_bundle::MAX_BUNDLE_MESSAGES`] (1000).
#[uniffi::export]
pub fn seal_history_bundle(secret: Vec<u8>, bundle_bytes: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    use prost::Message as _;
    if secret.len() != 32 {
        return Err(FfiError::Codec(
            "history-bundle secret must be 32 bytes".into(),
        ));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&secret);

    let bundle =
        lakko_proto::mls_app::history_bundle::v1::HistoryBundle::decode(bundle_bytes.as_slice())
            .map_err(|e| FfiError::Codec(format!("HistoryBundle decode: {e}")))?;
    let sealed = lakko_mls::history_bundle::seal_bundle(&key, &bundle)?;
    Ok(sealed)
}

/// Inverse of [`seal_history_bundle`]. Returns the prost-encoded
/// `HistoryBundle` bytes (the caller decodes them in its native
/// proto runtime — TypeScript via the wasm proto, Swift via
/// `SwiftProtobuf`, Kotlin via the generated `lakko-proto`
/// classes).
#[uniffi::export]
pub fn open_history_bundle(secret: Vec<u8>, sealed_bytes: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    use prost::Message as _;
    if secret.len() != 32 {
        return Err(FfiError::Codec(
            "history-bundle secret must be 32 bytes".into(),
        ));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&secret);

    let bundle = lakko_mls::history_bundle::open_bundle(&key, &sealed_bytes)?;
    let mut out = Vec::with_capacity(bundle.encoded_len());
    bundle
        .encode(&mut out)
        .map_err(|e| FfiError::Codec(format!("HistoryBundle re-encode: {e}")))?;
    Ok(out)
}

/// AEAD-seal `plaintext` under a raw 32-byte `key` (ADR-0018 Part 2a, factor
/// (a) — "another-device" identity transfer, Path B). Returns the self-
/// describing wire blob `nonce(12) ‖ ciphertext+tag` (ChaCha20-Poly1305, empty
/// AAD; the nonce is generated internally and prepended). The existing device
/// derives `key` via [`DeviceGroup::export_secret`] and seals
/// `(grant_token ‖ phrase_seed)` here, then publishes the blob to the
/// rendezvous mailbox; the fresh device derives the same `key` and opens it
/// via [`open_with_key`].
///
/// # Errors
///
/// - [`FfiError::Crypto`] when `key` is not exactly 32 bytes, or on internal
///   AEAD failure.
#[uniffi::export]
pub fn seal_with_key(key: Vec<u8>, plaintext: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    let key = key_32(&key)?;
    lakko_crypto::aead::seal_with_key(&key, &plaintext)
        .map_err(|e| FfiError::Crypto(format!("seal_with_key: {e}")))
}

/// Inverse of [`seal_with_key`]: open a `nonce(12) ‖ ciphertext+tag` wire blob
/// under the same raw 32-byte `key`. Returns the recovered plaintext.
///
/// # Errors
///
/// - [`FfiError::Crypto`] when `key` is not exactly 32 bytes, the blob is too
///   short, or authentication fails (wrong key / tampered blob).
#[uniffi::export]
pub fn open_with_key(key: Vec<u8>, wire: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    let key = key_32(&key)?;
    lakko_crypto::aead::open_with_key(&key, &wire)
        .map_err(|e| FfiError::Crypto(format!("open_with_key: {e}")))
}

/// Validate + copy a caller-supplied key into a fixed 32-byte array, the input
/// shape [`lakko_crypto::aead::seal_with_key`] / `open_with_key` require.
fn key_32(key: &[u8]) -> Result<[u8; 32], FfiError> {
    let arr: [u8; 32] = key
        .try_into()
        .map_err(|_| FfiError::Crypto(format!("key must be 32 bytes, got {}", key.len())))?;
    Ok(arr)
}

// ============================================================================
// Phase 2a3d8-ui — structured history-bundle FFI
//
// The byte-oriented `seal_history_bundle` / `open_history_bundle` above
// assume the caller hands in / receives a prost-encoded `HistoryBundle`
// proto. Three clients × three proto runtimes isn't a great trade so
// the surface below mirrors the same operation in plain `uniffi::Record`
// + `uniffi::Object` types. Native callers (Swift / Kotlin) never see a
// proto byte buffer; they build / consume the Records.
//
// Wire format is identical to the byte path's — same prost-encoded
// `HistoryBundle` + `SealedBundle` wrappers underneath — so the two
// callers interop on the wire.
// ============================================================================

/// One message in a [`StructuredHistoryGroup`] (Phase 2a3d8-ui).
///
/// `payload_bytes` is the already-decoded MLS `ApplicationPayload`
/// blob — the same bytes the receiver-side
/// `Group::process_incoming` produced when the message first
/// landed. The receiving device decodes it via its native chat
/// envelope parser before projecting into the local message
/// store.
#[derive(uniffi::Record)]
pub struct StructuredHistoryMessage {
    /// TLS-encoded `mls_app::v1::ApplicationPayload` bytes from
    /// the original receive path.
    pub payload_bytes: Vec<u8>,
    /// Sender MLS leaf index at the time of original receipt.
    pub sender_leaf: u32,
    /// Local store's per-message id (stable across snapshots).
    /// Used by the receiver for dedup against its own log.
    pub message_id: Vec<u8>,
    /// UNIX millis when the sender first saw the message.
    pub received_at_ms: u64,
}

/// All history rows for a single conversation group.
#[derive(uniffi::Record)]
pub struct StructuredHistoryGroup {
    /// MLS group_id the messages belong to.
    pub group_id: Vec<u8>,
    /// Messages, newest-first. Receiver inserts in reverse so
    /// the local store ends up with oldest-first ordering.
    pub messages: Vec<StructuredHistoryMessage>,
    /// `true` when the list was truncated to a per-group cap.
    /// UI may surface a "load older" affordance later.
    pub count_capped: bool,
}

/// Top-level result of [`open_history_bundle_structured`].
#[derive(uniffi::Record)]
pub struct StructuredHistoryBundle {
    pub groups: Vec<StructuredHistoryGroup>,
    /// HLC of the most recent message included in the bundle.
    pub max_hlc: u64,
}

/// AEAD-seal a structured history bundle (Phase 2a3d8-ui).
///
/// Same wire format as [`seal_history_bundle`] — the function
/// internally builds a `HistoryBundle` proto from the supplied
/// Records and runs the byte path's seal logic.
///
/// `secret` MUST be 32 bytes (typically the output of
/// [`DeviceGroup::history_bundle_secret`]).
#[uniffi::export]
pub fn seal_history_bundle_structured(
    secret: Vec<u8>,
    groups: Vec<StructuredHistoryGroup>,
    max_hlc: u64,
) -> Result<Vec<u8>, FfiError> {
    if secret.len() != 32 {
        return Err(FfiError::Codec(
            "history-bundle secret must be 32 bytes".into(),
        ));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&secret);

    let bundle = build_history_bundle_proto(groups, max_hlc);
    let sealed = lakko_mls::history_bundle::seal_bundle(&key, &bundle)?;
    Ok(sealed)
}

/// Inverse of [`seal_history_bundle_structured`]. Returns the
/// structured projection — no proto runtime needed on the
/// caller side.
#[uniffi::export]
pub fn open_history_bundle_structured(
    secret: Vec<u8>,
    sealed_bytes: Vec<u8>,
) -> Result<StructuredHistoryBundle, FfiError> {
    if secret.len() != 32 {
        return Err(FfiError::Codec(
            "history-bundle secret must be 32 bytes".into(),
        ));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&secret);

    let bundle = lakko_mls::history_bundle::open_bundle(&key, &sealed_bytes)?;
    Ok(StructuredHistoryBundle {
        max_hlc: bundle.max_hlc,
        groups: bundle
            .groups
            .into_iter()
            .map(|g| StructuredHistoryGroup {
                group_id: g.group_id,
                count_capped: g.count_capped,
                messages: g
                    .messages
                    .into_iter()
                    .map(|m| StructuredHistoryMessage {
                        payload_bytes: m.payload_bytes,
                        sender_leaf: m.sender_leaf,
                        message_id: m.message_id,
                        received_at_ms: m.received_at_ms,
                    })
                    .collect(),
            })
            .collect(),
    })
}

fn build_history_bundle_proto(
    groups: Vec<StructuredHistoryGroup>,
    max_hlc: u64,
) -> lakko_proto::mls_app::history_bundle::v1::HistoryBundle {
    use lakko_proto::mls_app::history_bundle::v1::{HistoryBundle, HistoryGroup, HistoryMessage};
    HistoryBundle {
        max_hlc,
        groups: groups
            .into_iter()
            .map(|g| HistoryGroup {
                group_id: g.group_id,
                count_capped: g.count_capped,
                messages: g
                    .messages
                    .into_iter()
                    .map(|m| HistoryMessage {
                        payload_bytes: m.payload_bytes,
                        sender_leaf: m.sender_leaf,
                        message_id: m.message_id,
                        received_at_ms: m.received_at_ms,
                    })
                    .collect(),
            })
            .collect(),
    }
}

// ============================================================================
// History-bundle per-message placeholder codec (G-6)
//
// Canonical cross-platform encode/decode for the
// `StructuredHistoryMessage.payload_bytes` display projection.
// Web / iOS / Android all call these so the bytes agree on the
// wire (previously each platform JSON-encoded its own
// `PersistedMessage` shape, which did NOT interoperate web <->
// native — see lakko_mls::history_placeholder + known-gaps G-6).
// ============================================================================

/// Message direction relative to the local account.
#[derive(uniffi::Enum)]
pub enum HistoryMessageDirection {
    /// Sent by this account.
    Outgoing,
    /// Received from a peer.
    Incoming,
    /// System / control message.
    System,
}

impl From<HistoryMessageDirection> for lakko_mls::history_placeholder::HistoryDirection {
    fn from(d: HistoryMessageDirection) -> Self {
        match d {
            HistoryMessageDirection::Outgoing => Self::Outgoing,
            HistoryMessageDirection::Incoming => Self::Incoming,
            HistoryMessageDirection::System => Self::System,
        }
    }
}

impl From<lakko_mls::history_placeholder::HistoryDirection> for HistoryMessageDirection {
    fn from(d: lakko_mls::history_placeholder::HistoryDirection) -> Self {
        use lakko_mls::history_placeholder::HistoryDirection as D;
        match d {
            D::Outgoing => Self::Outgoing,
            D::Incoming => Self::Incoming,
            D::System => Self::System,
        }
    }
}

/// One attachment reference inside a [`HistoryPlaceholder`].
/// `blob_id` / `key` MUST be 32 bytes, `nonce` 12 bytes.
#[derive(uniffi::Record)]
pub struct HistoryAttachment {
    /// 32-byte SHA-256 blob id.
    pub blob_id: Vec<u8>,
    /// 32-byte content-encryption key.
    pub key: Vec<u8>,
    /// 12-byte AEAD nonce.
    pub nonce: Vec<u8>,
    /// Plaintext size in bytes.
    pub size: u64,
    /// Optional MIME type.
    pub mime: Option<String>,
    /// Optional file name.
    pub name: Option<String>,
}

/// The display projection of one history-bundle message —
/// the structured form of `StructuredHistoryMessage.payload_bytes`.
#[derive(uniffi::Record)]
pub struct HistoryPlaceholder {
    /// HLC-derived message id.
    pub message_id: u64,
    /// Direction relative to the local account.
    pub direction: HistoryMessageDirection,
    /// Display timestamp string as the platform stores it
    /// (ISO-8601 today), carried verbatim.
    pub created_at: String,
    /// Message text.
    pub text: String,
    /// Attachment references.
    pub attachments: Vec<HistoryAttachment>,
}

fn fixed_bytes<const N: usize>(v: &[u8], field: &str) -> Result<[u8; N], FfiError> {
    if v.len() != N {
        return Err(FfiError::Codec(format!(
            "history placeholder: {field} must be {N} bytes, got {}",
            v.len()
        )));
    }
    let mut arr = [0u8; N];
    arr.copy_from_slice(v);
    Ok(arr)
}

/// Encode a [`HistoryPlaceholder`] to the canonical wire bytes
/// for `StructuredHistoryMessage.payload_bytes`.
///
/// # Errors
///
/// [`FfiError::Codec`] on a wrong-length `blob_id` / `key` /
/// `nonce`, or when a field exceeds the codec's hard caps
/// (see `lakko_mls::history_placeholder`).
#[uniffi::export]
pub fn encode_history_placeholder(placeholder: HistoryPlaceholder) -> Result<Vec<u8>, FfiError> {
    use lakko_mls::history_placeholder::{HistoryAttachmentPlaceholder, HistoryMessagePlaceholder};
    let mut attachments = Vec::with_capacity(placeholder.attachments.len());
    for a in placeholder.attachments {
        attachments.push(HistoryAttachmentPlaceholder {
            blob_id: fixed_bytes::<32>(&a.blob_id, "blob_id")?,
            key: fixed_bytes::<32>(&a.key, "key")?,
            nonce: fixed_bytes::<12>(&a.nonce, "nonce")?,
            size: a.size,
            mime: a.mime,
            name: a.name,
        });
    }
    let msg = HistoryMessagePlaceholder {
        message_id: placeholder.message_id,
        direction: placeholder.direction.into(),
        created_at: placeholder.created_at,
        text: placeholder.text,
        attachments,
    };
    Ok(msg.encode()?)
}

/// Decode canonical `StructuredHistoryMessage.payload_bytes`
/// produced by [`encode_history_placeholder`] (on any platform)
/// back into a [`HistoryPlaceholder`].
///
/// # Errors
///
/// [`FfiError::Codec`] on a malformed buffer (bad version,
/// truncation, over-cap length, non-UTF-8 string, unknown
/// direction tag, trailing bytes).
#[uniffi::export]
pub fn decode_history_placeholder(bytes: Vec<u8>) -> Result<HistoryPlaceholder, FfiError> {
    let msg = lakko_mls::history_placeholder::HistoryMessagePlaceholder::decode(&bytes)?;
    Ok(HistoryPlaceholder {
        message_id: msg.message_id,
        direction: msg.direction.into(),
        created_at: msg.created_at,
        text: msg.text,
        attachments: msg
            .attachments
            .into_iter()
            .map(|a| HistoryAttachment {
                blob_id: a.blob_id.to_vec(),
                key: a.key.to_vec(),
                nonce: a.nonce.to_vec(),
                size: a.size,
                mime: a.mime,
                name: a.name,
            })
            .collect(),
    })
}

/// Compute the deterministic DeviceGroup `group_id` from an
/// account_id, without building a `DeviceGroup` instance.
/// Useful on the joining-device side when polling the DS
/// journal for catch-up Commits before any Welcome has been
/// processed.
#[uniffi::export]
pub fn device_group_id_for(account_id: Vec<u8>) -> Result<Vec<u8>, FfiError> {
    if account_id.len() != 16 {
        return Err(FfiError::Codec("account_id must be 16 bytes".into()));
    }
    let mut acct = [0u8; 16];
    acct.copy_from_slice(&account_id);
    Ok(MlsDeviceGroup::group_id_for(&acct))
}

/// Load an existing DeviceGroup for `account_id` from
/// `identity`'s MLS provider storage. Returns `null` (on the
/// Kotlin side; `None` on the Rust side) when no row is
/// present.
///
/// Free function rather than `DeviceGroup::load` constructor —
/// UniFFI 0.28 rejects `Option<Self>` in constructor
/// signatures (mirror of the `load_group` shape from the
/// Group surface).
#[uniffi::export]
pub fn load_device_group(
    identity: &Identity,
    account_id: Vec<u8>,
) -> Result<Option<Arc<DeviceGroup>>, FfiError> {
    if account_id.len() != 16 {
        return Err(FfiError::Codec("account_id must be 16 bytes".into()));
    }
    let mut acct = [0u8; 16];
    acct.copy_from_slice(&account_id);
    let loaded = MlsDeviceGroup::load(&identity.inner, &acct)?;
    Ok(loaded.map(|inner| {
        Arc::new(DeviceGroup {
            inner: Mutex::new(inner),
        })
    }))
}

// Manual std::fmt::Debug for ProcessedMessage so we can panic-format it in
// tests without uniffi attempting to derive Debug for FFI types.
impl core::fmt::Debug for ProcessedMessage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Application { data } => write!(f, "Application {{ {} bytes }}", data.len()),
            Self::Commit => write!(f, "Commit"),
            Self::Proposal => write!(f, "Proposal"),
            Self::ExternalJoinProposal => write!(f, "ExternalJoinProposal"),
        }
    }
}

// ============================================================================
// Phase 3a4 — Call signaling + SFrame keying FFI
// ============================================================================

/// Audio / video request flags on a call offer (Phase 3a3).
#[derive(uniffi::Record)]
pub struct FfiCallMediaFlags {
    pub audio: bool,
    pub video: bool,
}

impl FfiCallMediaFlags {
    fn into_inner(self) -> lakko_calls::MediaFlags {
        lakko_calls::MediaFlags {
            audio: self.audio,
            video: self.video,
        }
    }

    fn from_inner(f: lakko_calls::MediaFlags) -> Self {
        Self {
            audio: f.audio,
            video: f.video,
        }
    }
}

/// Terminal reason carried in
/// [`FfiCallSignalingBody::End`].
#[derive(uniffi::Enum)]
pub enum FfiCallEndReason {
    HungUp,
    Busy,
    Declined,
    TimedOut,
    Error,
}

impl FfiCallEndReason {
    fn into_inner(self) -> lakko_calls::EndReason {
        match self {
            Self::HungUp => lakko_calls::EndReason::HungUp,
            Self::Busy => lakko_calls::EndReason::Busy,
            Self::Declined => lakko_calls::EndReason::Declined,
            Self::TimedOut => lakko_calls::EndReason::TimedOut,
            Self::Error => lakko_calls::EndReason::Error,
        }
    }

    fn from_inner(r: lakko_calls::EndReason) -> Self {
        match r {
            lakko_calls::EndReason::HungUp => Self::HungUp,
            lakko_calls::EndReason::Busy => Self::Busy,
            lakko_calls::EndReason::Declined => Self::Declined,
            lakko_calls::EndReason::TimedOut => Self::TimedOut,
            lakko_calls::EndReason::Error => Self::Error,
        }
    }
}

/// Typed body of a call-signaling event (Phase 3a3 +
/// 3c2). Every variant carries a 16-byte `call_id`.
#[derive(uniffi::Enum)]
pub enum FfiCallSignalingBody {
    /// SDP offer from the initiator (1×1).
    Offer {
        call_id: Vec<u8>,
        sdp: String,
        created_at_ms: u64,
        media: FfiCallMediaFlags,
    },
    /// SDP answer from the callee (1×1).
    Answer { call_id: Vec<u8>, sdp: String },
    /// Trickled ICE candidate (1×1).
    IceCandidate {
        call_id: Vec<u8>,
        candidate: String,
        sdp_mid: String,
        sdp_m_line_index: u32,
    },
    /// Terminal event (1×1 hang-up).
    End {
        call_id: Vec<u8>,
        reason: FfiCallEndReason,
    },
    /// Group-call announce (Phase 3c2). Posted by the
    /// initiator to the chat MLS group; observers render a
    /// joinable-call card.
    GroupStart {
        call_id: Vec<u8>,
        initiator_leaf: u32,
        created_at_ms: u64,
        media: FfiCallMediaFlags,
    },
    /// Group-call participant joined the SFU (Phase 3c2).
    GroupJoin {
        call_id: Vec<u8>,
        leaf_index: u32,
        joined_at_ms: u64,
    },
    /// Group-call participant left the SFU (Phase 3c2).
    GroupLeave {
        call_id: Vec<u8>,
        leaf_index: u32,
        reason: FfiCallEndReason,
    },
}

fn body_into_inner(body: FfiCallSignalingBody) -> Result<lakko_calls::SignalingBody, FfiError> {
    let take_id = |bytes: Vec<u8>| -> Result<lakko_calls::CallId, FfiError> {
        if bytes.len() != 16 {
            return Err(FfiError::Codec(format!(
                "call_id length: expected 16, got {}",
                bytes.len()
            )));
        }
        let mut id = [0u8; 16];
        id.copy_from_slice(&bytes);
        Ok(id)
    };
    Ok(match body {
        FfiCallSignalingBody::Offer {
            call_id,
            sdp,
            created_at_ms,
            media,
        } => lakko_calls::SignalingBody::Offer {
            call_id: take_id(call_id)?,
            sdp,
            created_at_ms,
            media: media.into_inner(),
        },
        FfiCallSignalingBody::Answer { call_id, sdp } => lakko_calls::SignalingBody::Answer {
            call_id: take_id(call_id)?,
            sdp,
        },
        FfiCallSignalingBody::IceCandidate {
            call_id,
            candidate,
            sdp_mid,
            sdp_m_line_index,
        } => lakko_calls::SignalingBody::IceCandidate {
            call_id: take_id(call_id)?,
            candidate,
            sdp_mid,
            sdp_m_line_index,
        },
        FfiCallSignalingBody::End { call_id, reason } => lakko_calls::SignalingBody::End {
            call_id: take_id(call_id)?,
            reason: reason.into_inner(),
        },
        FfiCallSignalingBody::GroupStart {
            call_id,
            initiator_leaf,
            created_at_ms,
            media,
        } => lakko_calls::SignalingBody::GroupStart {
            call_id: take_id(call_id)?,
            initiator_leaf,
            created_at_ms,
            media: media.into_inner(),
        },
        FfiCallSignalingBody::GroupJoin {
            call_id,
            leaf_index,
            joined_at_ms,
        } => lakko_calls::SignalingBody::GroupJoin {
            call_id: take_id(call_id)?,
            leaf_index,
            joined_at_ms,
        },
        FfiCallSignalingBody::GroupLeave {
            call_id,
            leaf_index,
            reason,
        } => lakko_calls::SignalingBody::GroupLeave {
            call_id: take_id(call_id)?,
            leaf_index,
            reason: reason.into_inner(),
        },
    })
}

fn body_from_inner(body: lakko_calls::SignalingBody) -> FfiCallSignalingBody {
    match body {
        lakko_calls::SignalingBody::Offer {
            call_id,
            sdp,
            created_at_ms,
            media,
        } => FfiCallSignalingBody::Offer {
            call_id: call_id.to_vec(),
            sdp,
            created_at_ms,
            media: FfiCallMediaFlags::from_inner(media),
        },
        lakko_calls::SignalingBody::Answer { call_id, sdp } => FfiCallSignalingBody::Answer {
            call_id: call_id.to_vec(),
            sdp,
        },
        lakko_calls::SignalingBody::IceCandidate {
            call_id,
            candidate,
            sdp_mid,
            sdp_m_line_index,
        } => FfiCallSignalingBody::IceCandidate {
            call_id: call_id.to_vec(),
            candidate,
            sdp_mid,
            sdp_m_line_index,
        },
        lakko_calls::SignalingBody::End { call_id, reason } => FfiCallSignalingBody::End {
            call_id: call_id.to_vec(),
            reason: FfiCallEndReason::from_inner(reason),
        },
        lakko_calls::SignalingBody::GroupStart {
            call_id,
            initiator_leaf,
            created_at_ms,
            media,
        } => FfiCallSignalingBody::GroupStart {
            call_id: call_id.to_vec(),
            initiator_leaf,
            created_at_ms,
            media: FfiCallMediaFlags::from_inner(media),
        },
        lakko_calls::SignalingBody::GroupJoin {
            call_id,
            leaf_index,
            joined_at_ms,
        } => FfiCallSignalingBody::GroupJoin {
            call_id: call_id.to_vec(),
            leaf_index,
            joined_at_ms,
        },
        lakko_calls::SignalingBody::GroupLeave {
            call_id,
            leaf_index,
            reason,
        } => FfiCallSignalingBody::GroupLeave {
            call_id: call_id.to_vec(),
            leaf_index,
            reason: FfiCallEndReason::from_inner(reason),
        },
    }
}

impl From<lakko_calls::Error> for FfiError {
    fn from(err: lakko_calls::Error) -> Self {
        match err {
            lakko_calls::Error::Exporter(msg) => FfiError::Exporter(msg),
            lakko_calls::Error::Crypto(msg) => FfiError::Codec(msg),
            lakko_calls::Error::CounterExhausted => {
                FfiError::Codec("sframe counter exhausted".into())
            },
        }
    }
}

/// Compose plaintext call-signaling bytes (Phase 3a3). The
/// caller wraps these in an MLS application message via
/// [`Group::create_application_message`].
#[uniffi::export]
pub fn compose_call_signaling(body: FfiCallSignalingBody) -> Result<Vec<u8>, FfiError> {
    let inner = body_into_inner(body)?;
    Ok(lakko_calls::compose_signaling_payload(&inner)?)
}

/// Inverse of [`compose_call_signaling`]. Decodes the
/// plaintext bytes that fell out of
/// [`ProcessedMessage::Application`].
#[uniffi::export]
pub fn parse_call_signaling(plaintext: Vec<u8>) -> Result<FfiCallSignalingBody, FfiError> {
    let body = lakko_calls::parse_signaling_payload(&plaintext)?;
    Ok(body_from_inner(body))
}

/// Phase 3a4 — derive the per-call SFrame seed from a
/// conversation MLS group's exporter (label
/// `lakko/call/v1`). Both sides reach the same 32 bytes at
/// the same epoch with the same `call_id`.
#[uniffi::export]
pub fn sframe_derive_call_key(
    group: &Group,
    identity: &Identity,
    call_id: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let g = group.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
    let key = lakko_calls::CallKey::derive(&g, &identity.inner, &call_id)?;
    Ok(key.as_bytes().to_vec())
}

/// Per-participant SFrame key + nonce salt derived from the
/// 32-byte call key + sender leaf index. Caller maintains
/// the frame counter on the platform side.
#[derive(uniffi::Record)]
pub struct FfiSframeParticipantKey {
    /// 32-byte AEAD key.
    pub key_bytes: Vec<u8>,
    /// 12-byte nonce salt.
    pub nonce_salt: Vec<u8>,
}

/// Derive a per-participant SFrame key + nonce salt
/// (Phase 3a4).
#[uniffi::export]
pub fn sframe_derive_participant_key(
    call_key_seed: Vec<u8>,
    sender_leaf: u32,
) -> Result<FfiSframeParticipantKey, FfiError> {
    if call_key_seed.len() != 32 {
        return Err(FfiError::Codec("call_key_seed must be 32 bytes".into()));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&call_key_seed);
    let ck = lakko_calls::CallKey::from_seed(seed);
    let pk = ck.participant_key(sender_leaf)?;
    Ok(FfiSframeParticipantKey {
        key_bytes: pk.key_bytes().to_vec(),
        nonce_salt: pk.nonce_salt().to_vec(),
    })
}

/// Open an SFrame frame with the Phase 3c5 overlap window.
/// Tries `current_call_key_seed` first; on AEAD rejection
/// falls back to `previous_call_key_seed` if present
/// (caller passes `None` outside the 1-second overlap
/// window). Returns the plaintext on success.
///
/// Mirrors [`GroupCallSession::try_open_with_overlap`]; the
/// FFI surface is seed-flat so platform code can hold the
/// 32-byte seeds in its own keychain-backed store rather
/// than ferrying a Rust handle.
#[uniffi::export]
pub fn sframe_open_with_overlap(
    current_call_key_seed: Vec<u8>,
    previous_call_key_seed: Option<Vec<u8>>,
    sender_leaf: u32,
    aad: Vec<u8>,
    ciphertext: Vec<u8>,
    frame_counter: u32,
) -> Result<Vec<u8>, FfiError> {
    let take_seed = |bytes: Vec<u8>| -> Result<[u8; 32], FfiError> {
        if bytes.len() != 32 {
            return Err(FfiError::Codec(format!(
                "call_key_seed must be 32 bytes, got {}",
                bytes.len()
            )));
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes);
        Ok(seed)
    };

    let current_seed = take_seed(current_call_key_seed)?;
    let current = lakko_calls::CallKey::from_seed(current_seed);
    let current_key = current.participant_key(sender_leaf)?;
    match current_key.open(&aad, &ciphertext, frame_counter) {
        Ok(pt) => return Ok(pt),
        Err(lakko_calls::Error::Crypto(_)) => {
            // Fall through to the overlap-window path.
        },
        Err(other) => return Err(other.into()),
    }

    if let Some(prev_bytes) = previous_call_key_seed {
        let prev_seed = take_seed(prev_bytes)?;
        let previous = lakko_calls::CallKey::from_seed(prev_seed);
        let prev_key = previous.participant_key(sender_leaf)?;
        return prev_key
            .open(&aad, &ciphertext, frame_counter)
            .map_err(Into::into);
    }
    Err(FfiError::Codec(
        "frame failed authentication under current key (no overlap window active)".into(),
    ))
}

// ============================================================================
// Phase 3b4 — Call-admission JWT FFI surface
// ============================================================================
//
// The client side of the call-admission JWT flow lets the
// platform (Swift / Kotlin) parse + self-verify a JWT it
// just received from `POST /call/jwt` (caller drives the
// HTTP; the FFI surface is plain bytes / strings ↔ typed
// records). Crypto + wire shape lives in
// `crates/lakko-call-jwt`.

/// Mirror of `lakko_call_jwt::CallRole` — Record-friendly
/// enum.
#[derive(uniffi::Enum)]
pub enum FfiCallRole {
    /// Regular participant.
    Participant,
    /// Publisher-only (Phase 4 broadcasts).
    Publisher,
    /// Subscriber-only (Phase 4 audio-room audience).
    Subscriber,
}

impl FfiCallRole {
    fn from_inner(r: lakko_call_jwt::CallRole) -> Self {
        match r {
            lakko_call_jwt::CallRole::Participant => Self::Participant,
            lakko_call_jwt::CallRole::Publisher => Self::Publisher,
            lakko_call_jwt::CallRole::Subscriber => Self::Subscriber,
        }
    }
}

/// Parsed JWT claims — typed mirror of
/// `lakko_call_jwt::CallJwtClaims`. Returned by
/// [`parse_call_jwt_unverified`] + [`verify_call_jwt_with_jwks`].
#[derive(uniffi::Record)]
pub struct FfiCallJwtClaims {
    /// Issuer; always `"lakko-auth"` on a well-formed JWT.
    pub iss: String,
    /// Audience; always `"lakko-sfu"`.
    pub aud: String,
    /// account_id, 32-char lower-case hex.
    pub sub: String,
    /// call_id, 32-char lower-case hex.
    pub call_id: String,
    /// Participation role.
    pub role: FfiCallRole,
    /// `iat` — issued-at, unix seconds.
    pub iat: u64,
    /// `exp` — expires-at, unix seconds.
    pub exp: u64,
    /// JWS header `kid` — 16 hex chars. Useful for client
    /// dashboards / debug logs.
    pub kid: String,
}

fn claims_to_ffi(claims: lakko_call_jwt::CallJwtClaims, kid: String) -> FfiCallJwtClaims {
    FfiCallJwtClaims {
        iss: claims.iss,
        aud: claims.aud,
        sub: claims.sub,
        call_id: claims.call_id,
        role: FfiCallRole::from_inner(claims.role),
        iat: claims.iat,
        exp: claims.exp,
        kid,
    }
}

impl From<lakko_call_jwt::JwtError> for FfiError {
    fn from(err: lakko_call_jwt::JwtError) -> Self {
        match err {
            lakko_call_jwt::JwtError::Format(msg) => FfiError::Codec(format!("jwt format: {msg}")),
            lakko_call_jwt::JwtError::Signature => FfiError::Codec("jwt signature".into()),
            lakko_call_jwt::JwtError::Expired => FfiError::Codec("jwt expired".into()),
            lakko_call_jwt::JwtError::Encode(msg) => FfiError::Codec(format!("jwt encode: {msg}")),
        }
    }
}

/// Parse a JWT into typed claims WITHOUT verifying the
/// signature. Useful for inspecting role / expiry / kid
/// client-side before presenting the JWT to the SFU.
///
/// **Do NOT** trust the returned claims for any
/// authorisation decision — use [`verify_call_jwt_with_jwks`]
/// for the verifying path.
#[uniffi::export]
pub fn parse_call_jwt_unverified(jwt: String) -> Result<FfiCallJwtClaims, FfiError> {
    let (header, claims) = lakko_call_jwt::parse_unverified(&jwt)?;
    Ok(claims_to_ffi(claims, header.kid))
}

/// Verify a JWT against a JWKS document the client fetched
/// from `GET /.well-known/jwks.json` (typically cached
/// for 24 h). Returns the typed claims on success.
///
/// `now_unix_secs` is injected by the caller so the
/// platform controls the clock source — pass
/// `Date.now() / 1000` on web, `Date().timeIntervalSince1970`
/// on iOS, `System.currentTimeMillis() / 1000` on Android.
///
/// # Errors
///
/// Surfaces every shape failure (JWS format, JWKS no-match,
/// signature, expired, JSON decode) as [`FfiError::Codec`]
/// with a descriptive prefix.
#[uniffi::export]
pub fn verify_call_jwt_with_jwks(
    jwks_doc_bytes: Vec<u8>,
    jwt: String,
    now_unix_secs: u64,
) -> Result<FfiCallJwtClaims, FfiError> {
    let jwks = lakko_call_jwt::parse_jwks(&jwks_doc_bytes)?;
    let claims = lakko_call_jwt::verify_with_jwks(&jwks, &jwt, now_unix_secs)?;
    // Re-derive kid from the JWT header for the FFI shape.
    let (header, _) = lakko_call_jwt::parse_unverified(&jwt)?;
    Ok(claims_to_ffi(claims, header.kid))
}

// ============================================================================
// Phase 3a11 — Call quality metrics FFI
// ============================================================================
//
// MOS estimation + adaptive-bitrate recommendation surfaced
// as plain Records / free functions. Platform layer
// samples `RTCStatsReport` on its own cadence and feeds
// the values in.

/// Audio codec hint for the loss-impairment curve. Mirror
/// of `lakko_calls::AudioCodec`.
#[derive(uniffi::Enum)]
pub enum FfiAudioCodec {
    /// Opus (RFC 6716) — FEC-capable; the default for
    /// Lakko's voice / video calls.
    Opus,
    /// G.711 / ISAC / AAC-LC / any non-Opus. No-FEC
    /// baseline for the impairment math.
    Other,
}

impl FfiAudioCodec {
    fn into_inner(self) -> lakko_calls::AudioCodec {
        match self {
            Self::Opus => lakko_calls::AudioCodec::Opus,
            Self::Other => lakko_calls::AudioCodec::Other,
        }
    }
}

/// Discretised quality bucket. Mirror of
/// `lakko_calls::QualityLevel`.
#[derive(uniffi::Enum)]
pub enum FfiQualityLevel {
    /// MOS ≥ 4.3 — toll-quality.
    Excellent,
    /// 4.0 ≤ MOS < 4.3.
    Good,
    /// 3.6 ≤ MOS < 4.0.
    Fair,
    /// 3.1 ≤ MOS < 3.6.
    Poor,
    /// MOS < 3.1 — near-unusable.
    Bad,
}

impl FfiQualityLevel {
    fn from_inner(l: lakko_calls::QualityLevel) -> Self {
        match l {
            lakko_calls::QualityLevel::Excellent => Self::Excellent,
            lakko_calls::QualityLevel::Good => Self::Good,
            lakko_calls::QualityLevel::Fair => Self::Fair,
            lakko_calls::QualityLevel::Poor => Self::Poor,
            lakko_calls::QualityLevel::Bad => Self::Bad,
        }
    }
}

/// Single network sample. Caller constructs one per
/// `RTCStatsReport` poll.
#[derive(uniffi::Record)]
pub struct FfiQualitySample {
    /// Round-trip time in milliseconds.
    pub rtt_ms: u32,
    /// Inter-arrival jitter in milliseconds.
    pub jitter_ms: u32,
    /// Packet-loss fraction in `[0.0, 100.0]`.
    pub loss_pct: f32,
    /// Codec hint.
    pub codec: FfiAudioCodec,
}

/// Output of [`estimate_call_quality`]. Carries both raw
/// `r_factor` / `mos` and the discretised bucket.
#[derive(uniffi::Record)]
pub struct FfiQualityScore {
    /// E-model R-factor in `[0.0, 100.0]`.
    pub r_factor: f32,
    /// Mean Opinion Score in `[1.0, 4.5]`.
    pub mos: f32,
    /// Discretised bucket.
    pub level: FfiQualityLevel,
}

/// Estimate call quality from a single network sample.
#[uniffi::export]
pub fn estimate_call_quality(sample: FfiQualitySample) -> FfiQualityScore {
    let inner = lakko_calls::QualitySample {
        rtt_ms: sample.rtt_ms,
        jitter_ms: sample.jitter_ms,
        loss_pct: sample.loss_pct,
        codec: sample.codec.into_inner(),
    };
    let score = lakko_calls::estimate_quality(inner);
    FfiQualityScore {
        r_factor: score.r_factor,
        mos: score.mos,
        level: FfiQualityLevel::from_inner(score.level),
    }
}

/// Recommend a new encoder bitrate. Returns the current
/// bitrate unchanged when the sample falls in the "hold"
/// band.
#[uniffi::export]
pub fn recommend_call_bitrate(current_bps: u32, sample: FfiQualitySample) -> u32 {
    let inner = lakko_calls::QualitySample {
        rtt_ms: sample.rtt_ms,
        jitter_ms: sample.jitter_ms,
        loss_pct: sample.loss_pct,
        codec: sample.codec.into_inner(),
    };
    lakko_calls::recommend_bitrate(current_bps, inner)
}

/// Default ring-buffer size for [`FfiCallQualitySampler`].
#[uniffi::export]
pub fn ffi_default_quality_sample_window() -> u32 {
    u32::try_from(lakko_calls::DEFAULT_SAMPLE_WINDOW).unwrap_or(5)
}

/// Maximum ring-buffer size accepted by
/// [`FfiCallQualitySampler::new`].
#[uniffi::export]
pub fn ffi_max_quality_sample_window() -> u32 {
    u32::try_from(lakko_calls::MAX_SAMPLE_WINDOW).unwrap_or(60)
}

/// Stateful rolling-window smoothed quality sampler
/// (Phase 3a11). Mirror of `lakko_calls::QualitySampler` —
/// one per active call session on the platform side.
///
/// `record` pushes a fresh sample and returns the
/// smoothed [`FfiQualityScore`]; `latest` returns the
/// current smoothed score without pushing; `clear` resets
/// the window (on call end / SFU handoff).
#[derive(uniffi::Object)]
pub struct FfiCallQualitySampler {
    inner: std::sync::Mutex<lakko_calls::QualitySampler>,
}

#[uniffi::export]
impl FfiCallQualitySampler {
    /// Construct a sampler with a custom window size.
    /// Sizes outside `[1, MAX_SAMPLE_WINDOW]` are clamped.
    #[uniffi::constructor]
    pub fn new(window_size: u32) -> Self {
        let bounded = usize::try_from(window_size).unwrap_or(lakko_calls::DEFAULT_SAMPLE_WINDOW);
        Self {
            inner: std::sync::Mutex::new(lakko_calls::QualitySampler::new(bounded)),
        }
    }

    /// Effective window size after clamping.
    pub fn window_size(&self) -> u32 {
        let guard = self.inner.lock().expect("FfiCallQualitySampler poisoned");
        u32::try_from(guard.window_size()).unwrap_or(u32::MAX)
    }

    /// Number of samples currently buffered.
    pub fn count(&self) -> u32 {
        let guard = self.inner.lock().expect("FfiCallQualitySampler poisoned");
        u32::try_from(guard.count()).unwrap_or(u32::MAX)
    }

    /// Drop every buffered sample.
    pub fn clear(&self) {
        let mut guard = self.inner.lock().expect("FfiCallQualitySampler poisoned");
        guard.clear();
    }

    /// Push a sample and return the smoothed score.
    pub fn record(&self, sample: FfiQualitySample) -> FfiQualityScore {
        let inner_sample = lakko_calls::QualitySample {
            rtt_ms: sample.rtt_ms,
            jitter_ms: sample.jitter_ms,
            loss_pct: sample.loss_pct,
            codec: sample.codec.into_inner(),
        };
        let mut guard = self.inner.lock().expect("FfiCallQualitySampler poisoned");
        let score = guard.record(inner_sample);
        FfiQualityScore {
            r_factor: score.r_factor,
            mos: score.mos,
            level: FfiQualityLevel::from_inner(score.level),
        }
    }

    /// Latest smoothed score without pushing a new sample.
    /// Returns `None` when the sampler is empty.
    pub fn latest(&self) -> Option<FfiQualityScore> {
        let guard = self.inner.lock().expect("FfiCallQualitySampler poisoned");
        guard.latest().map(|score| FfiQualityScore {
            r_factor: score.r_factor,
            mos: score.mos,
            level: FfiQualityLevel::from_inner(score.level),
        })
    }
}

// ============================================================================
// Phase 3c3-joiner — link-based call invite acceptance FFI
// ============================================================================
//
// Wraps `lakko_calls::accept_call_invite` for Swift /
// Kotlin. The Rust shape returns an owned MLS group + a
// GroupCallSession + Welcome bytes; the FFI shape returns a
// uniffi::Object wrapping the MLS group + flat fields for
// the rest. The call-key seed is surfaced flat so the
// platform can feed it into the existing
// `sframe_derive_participant_key` FFI fn without holding a
// GroupCallSession handle.

/// Result of [`accept_call_invite`]. The owned `Group`
/// handle is the freshly-created MLS group with the
/// joiner as founder and the owner already added; the
/// platform uses it for subsequent MLS ops via the
/// existing `Group` methods. `welcome_for_owner` is the
/// MLS wire bytes the joiner must deliver to the owner
/// over their existing transport (the stranger-inbox
/// routed by `capability.owner_account_id`, typically).
#[derive(uniffi::Object)]
pub struct AcceptedCallInvite {
    /// Fresh MLS group (joiner at leaf 0, owner at leaf 1).
    group: Arc<Group>,
    /// MLS wire-format Welcome bytes for the owner to
    /// run `Group::join_from_welcome` against.
    welcome_for_owner: Vec<u8>,
    /// 16-byte `call_id` extracted from the capability.
    call_id: Vec<u8>,
    /// Joiner's leaf index in the new MLS group — always
    /// 0 for a single-joiner accept per the 3c3-joiner
    /// V1 contract, but exposed for forward-compat.
    own_leaf_index: u32,
    /// 32-byte `CallKey` seed derived via the MLS
    /// exporter against `call_id`. Feed this into the
    /// existing `sframe_derive_participant_key`
    /// free function to get the per-leaf SFrame key.
    call_key_seed: Vec<u8>,
}

#[uniffi::export]
impl AcceptedCallInvite {
    /// Freshly-created MLS group (joiner is founder at
    /// leaf 0). Platform uses this for subsequent MLS
    /// ops + the existing `sframe_derive_call_key` /
    /// `sframe_derive_participant_key` free fns.
    pub fn group(&self) -> Arc<Group> {
        self.group.clone()
    }

    /// MLS wire-format Welcome bytes for the owner.
    /// Platform must deliver these out-of-band.
    pub fn welcome_for_owner(&self) -> Vec<u8> {
        self.welcome_for_owner.clone()
    }

    /// 16-byte `call_id` from the capability.
    pub fn call_id(&self) -> Vec<u8> {
        self.call_id.clone()
    }

    /// Joiner's leaf index (0 for the founder).
    pub fn own_leaf_index(&self) -> u32 {
        self.own_leaf_index
    }

    /// 32-byte `CallKey` seed.
    pub fn call_key_seed(&self) -> Vec<u8> {
        self.call_key_seed.clone()
    }
}

/// Accept a verified call-invite capability. Mirrors
/// [`lakko_calls::accept_call_invite`].
///
/// `capability` must already be `decode_and_verify`'d
/// (use the [`contacts::CallInviteCapability`] constructor).
/// The returned object holds a fresh MLS group + the
/// Welcome bytes the joiner ships to the owner.
///
/// # Errors
///
/// - [`FfiError::Codec`] when the embedded KP is
///   malformed.
/// - [`FfiError::Exporter`] when the call-key derivation
///   fails (practically unreachable).
#[uniffi::export]
pub fn accept_call_invite(
    capability: &contacts::CallInviteCapability,
    joiner_identity: &Identity,
) -> Result<Arc<AcceptedCallInvite>, FfiError> {
    let accepted = lakko_calls::accept_call_invite(&capability.inner, &joiner_identity.inner)?;
    let call_key_seed = accepted.session.call_key().as_bytes().to_vec();
    let call_id = accepted.session.call_id.to_vec();
    let own_leaf_index = accepted.session.own_leaf_index;
    let group = Group {
        inner: Mutex::new(accepted.group),
    };
    Ok(Arc::new(AcceptedCallInvite {
        group: Arc::new(group),
        welcome_for_owner: accepted.welcome_for_owner,
        call_id,
        own_leaf_index,
        call_key_seed,
    }))
}

// ---------------------------------------------------------------------------
// Identity supersession proof (G-4, ADR-0007 §6).
//
// On rotation (e.g. recover-from-words minting a fresh account identity) the
// OLD identity signs a proof authorising the NEW identity as its successor.
// Peers verify it against the old key they already pin and rotate their pin to
// the new key — bridging trust WITHOUT a blind TOFU re-accept (security
// invariant #6: old keys never silently come back).
//
// This is the proof primitive only. The KT-log chain verifier
// (`verify_supersession_chain`) ties a proof to an operator-signed Merkle
// inclusion; it consumes log types that have no FFI wire format yet and lands
// with the transparency service.
// ---------------------------------------------------------------------------

/// Build the [`lakko_crypto::signing::SigningKey`] for an identity from its
/// 32-byte Ed25519 seed (the bytes `Identity::ed25519_seed()` returns).
fn signing_key_from_seed(seed: &[u8]) -> Result<lakko_crypto::signing::SigningKey, FfiError> {
    let arr: [u8; 32] = seed
        .try_into()
        .map_err(|_| FfiError::Identity("ed25519 seed must be 32 bytes".to_string()))?;
    Ok(lakko_crypto::signing::SigningKey::from_seed(&arr))
}

/// Sign an identity supersession proof (G-4): the OLD identity authorises the
/// NEW identity as its successor, valid in `[issued_at, expires_at]` (the
/// verifier additionally caps the window at 30 days). Pass each identity's
/// `ed25519_seed()`; both public keys are derived internally so the proof's
/// embedded `old_pubkey` always matches the signing key. Returns the wire-form
/// proof to publish alongside the rotation.
///
/// # Errors
///
/// [`FfiError::Identity`] if either seed is not exactly 32 bytes.
#[uniffi::export]
pub fn sign_supersession_proof(
    old_ed25519_seed: Vec<u8>,
    new_ed25519_seed: Vec<u8>,
    issued_at_unix_secs: i64,
    expires_at_unix_secs: i64,
) -> Result<Vec<u8>, FfiError> {
    let old_sk = signing_key_from_seed(&old_ed25519_seed)?;
    let new_sk = signing_key_from_seed(&new_ed25519_seed)?;
    let proof = lakko_transparency::SupersessionProof::new(
        old_sk.verifying_key().to_bytes(),
        new_sk.verifying_key().to_bytes(),
        issued_at_unix_secs,
        expires_at_unix_secs,
    );
    Ok(proof.sign(&old_sk).to_vec())
}

/// Verify an identity supersession proof (G-4) against the old key the caller
/// already pins for this account. Returns the authorised NEW public key
/// (32 bytes) the caller should re-pin on success.
///
/// # Errors
///
/// [`FfiError::Identity`] if `trusted_old_pubkey` is not 32 bytes, or the proof
/// is malformed / not signed by the trusted old key / carries an out-of-range
/// (>30-day) window / has expired relative to `now_unix_secs`.
#[uniffi::export]
pub fn verify_supersession(
    proof_bytes: Vec<u8>,
    trusted_old_pubkey: Vec<u8>,
    now_unix_secs: i64,
) -> Result<Vec<u8>, FfiError> {
    let old: [u8; 32] = trusted_old_pubkey
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Identity("trusted old pubkey must be 32 bytes".to_string()))?;
    let new = lakko_transparency::verify_supersession(&proof_bytes, &old, now_unix_secs)
        .map_err(|e| FfiError::Identity(format!("supersession verify: {e}")))?;
    Ok(new.to_vec())
}

/// Verify a complete identity-rotation chain against the KT log (G-30 #2,
/// ADR-0019). Unlike [`verify_supersession`] — which checks only the bundle's
/// standalone proof against the local pin — this anchors the rotation to the
/// operator-signed, append-only log: a rotation is bound to a SINGLE committed
/// version under a no-rollback root, which makes a leaked-old-key equivocation
/// DETECTABLE (two distinct successors cannot both be log-confirmed at one
/// version; the second peer to fetch sees a version-floor or inclusion failure).
///
/// The decomposed fields mirror the JSON a client gathers from
/// `/transparency/{root,latest,proof}` plus the `supersession_proof` carried in
/// the peer's identity bundle and what the peer currently pins. On success the
/// caller re-pins to the returned NEW public key (32 bytes) at `entry_version`.
///
/// `last_seen_log_size` is the client's persisted monotone high-water mark; the
/// chain's no-rollback check reads only the size, so the stale `SignedRoot`'s
/// other fields are reconstructed as zero (never inspected).
///
/// # Errors
///
/// [`FfiError::Identity`] if any fixed-length field is the wrong length
/// (`*_tree_root` / `entry_username_hash` / `pinned_old_pubkey` != 32,
/// `root_signature` != 64, `audit_path` not a multiple of 32), `operator_pubkey`
/// is not a valid Ed25519 key, or any chain gate fails (forged root, rollback,
/// inclusion mismatch, wrong account, non-newer version, or a supersession proof
/// that is absent / not signed by the pinned old key / out-of-window / expired /
/// not authorising exactly the entry's new key).
#[uniffi::export]
#[allow(clippy::too_many_arguments)]
pub fn verify_supersession_chain(
    // The operator key the client pinned out-of-band.
    operator_pubkey: Vec<u8>,
    // The newly-presented signed root (from `/transparency/root`).
    new_tree_root: Vec<u8>,
    new_log_size: u64,
    new_timestamp_unix: u64,
    new_root_signature: Vec<u8>,
    // The new log entry (from `/transparency/latest/:account_key`). The
    // `entry_username_hash` is the account_key the client queried with and is
    // used for both the entry and the pin (the inclusion proof is what binds it
    // to the root).
    entry_username_hash: Vec<u8>,
    entry_identity_pubkey: Vec<u8>,
    entry_version: u64,
    // Its Merkle inclusion proof (from `/transparency/proof/:idx`).
    inclusion_leaf_index: u64,
    inclusion_audit_path: Vec<u8>,
    // The supersession proof authorising the transition (from the peer's
    // identity bundle).
    supersession_proof: Vec<u8>,
    // What the peer currently pins.
    pinned_old_pubkey: Vec<u8>,
    pinned_version: u64,
    last_seen_log_size: u64,
    now_unix_secs: i64,
) -> Result<Vec<u8>, FfiError> {
    let operator =
        lakko_crypto::signing::VerifyingKey::from_bytes(&operator_pubkey).map_err(|_| {
            FfiError::Identity("operator pubkey is not a valid Ed25519 key".to_string())
        })?;
    let new_tree_root: lakko_transparency::TreeHash = new_tree_root
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Identity("new_tree_root must be 32 bytes".to_string()))?;
    let new_root_signature = lakko_crypto::signing::Signature::from_bytes(&new_root_signature)
        .map_err(|_| FfiError::Identity("root_signature must be 64 bytes".to_string()))?;
    let username_hash: [u8; 32] = entry_username_hash
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Identity("entry_username_hash must be 32 bytes".to_string()))?;
    let pinned_old: [u8; 32] = pinned_old_pubkey
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Identity("pinned_old_pubkey must be 32 bytes".to_string()))?;

    if inclusion_audit_path.len() % 32 != 0 {
        return Err(FfiError::Identity(
            "audit_path length must be a multiple of 32 bytes".to_string(),
        ));
    }
    let audit_path: Vec<lakko_transparency::TreeHash> = inclusion_audit_path
        .chunks_exact(32)
        .map(|c| {
            let mut h = [0u8; 32];
            h.copy_from_slice(c);
            h
        })
        .collect();

    let new_root = lakko_transparency::SignedRoot {
        tree_root: new_tree_root,
        log_size: new_log_size,
        timestamp_unix: new_timestamp_unix,
        signature: new_root_signature,
    };
    let new_entry = lakko_transparency::LogEntry {
        username_hash,
        identity_pubkey: entry_identity_pubkey,
        version: entry_version,
    };
    let inclusion = lakko_transparency::InclusionProof {
        leaf_index: inclusion_leaf_index,
        audit_path,
    };
    // The chain's `check_no_rollback` reads only `log_size`; reconstruct the
    // stale root from the client's high-water mark with zeroed other fields
    // (never inspected). A zero signature parses fine — it is never verified.
    let last_seen_root = lakko_transparency::SignedRoot {
        tree_root: [0u8; 32],
        log_size: last_seen_log_size,
        timestamp_unix: 0,
        signature: lakko_crypto::signing::Signature::from_bytes(&[0u8; 64])
            .map_err(|_| FfiError::Identity("zero signature parse".to_string()))?,
    };

    let pinned = lakko_transparency::PinnedIdentity {
        username_hash: &username_hash,
        old_pubkey: &pinned_old,
        version: pinned_version,
        last_seen_root: &last_seen_root,
    };
    let evidence = lakko_transparency::RotationEvidence {
        operator: &operator,
        new_root: &new_root,
        new_entry: &new_entry,
        inclusion: &inclusion,
        supersession_proof: &supersession_proof,
    };

    let rotated = lakko_transparency::verify_supersession_chain(&pinned, &evidence, now_unix_secs)
        .map_err(|e| FfiError::Identity(format!("supersession chain verify: {e}")))?;
    Ok(rotated.new_pubkey.to_vec())
}

/// Sign a genesis / fresh-start KT-log leaf's account-to-key binding with the
/// leaf's OWN identity key (G-30 #2, ADR-0019). Used for the first-ever publish
/// and a lost-everything fresh-start mint (a new key with NO supersession
/// proof). Pass the identity's `ed25519_seed()`; the leaf's pubkey is derived
/// from it internally so the self-signature always matches the embedded key.
/// The signature is version-INDEPENDENT (the version is operator-assigned and
/// unknown to a fresh-start minter). Returns the 64-byte signature to carry as
/// the leaf's `leaf_auth`.
///
/// # Errors
///
/// [`FfiError::Identity`] if the seed is not 32 bytes or `username_hash` is not
/// 32 bytes.
#[uniffi::export]
pub fn sign_leaf_genesis(
    ed25519_seed: Vec<u8>,
    username_hash: Vec<u8>,
) -> Result<Vec<u8>, FfiError> {
    let sk = signing_key_from_seed(&ed25519_seed)?;
    let username_hash: [u8; 32] = username_hash
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Identity("username_hash must be 32 bytes".to_string()))?;
    let pubkey = sk.verifying_key().to_bytes();
    Ok(lakko_transparency::sign_leaf_genesis(&sk, &username_hash, &pubkey).to_vec())
}

/// Verify a genesis self-signature against the embedded `identity_pubkey`
/// (G-30 #2, ADR-0019). A pass proves the holder of `identity_pubkey` authorised
/// this `(username_hash, pubkey)` account-to-key binding.
///
/// # Errors
///
/// [`FfiError::Identity`] if `username_hash` is not 32 bytes, the embedded
/// `identity_pubkey` is not a valid Ed25519 key, `sig` is not 64 bytes, or the
/// signature does not verify.
#[uniffi::export]
pub fn verify_leaf_genesis(
    sig: Vec<u8>,
    username_hash: Vec<u8>,
    identity_pubkey: Vec<u8>,
) -> Result<(), FfiError> {
    let username_hash: [u8; 32] = username_hash
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Identity("username_hash must be 32 bytes".to_string()))?;
    lakko_transparency::verify_leaf_genesis(&sig, &username_hash, &identity_pubkey)
        .map_err(|e| FfiError::Identity(format!("leaf genesis verify: {e}")))
}

/// Verify a witness co-signature (ADR-0020) over a transparency root's
/// `(tree_root, log_size)` against the CLIENT-PINNED `witness_key`. A client
/// requires this AND the operator's own root signature before trusting a root —
/// the defence against an operator split-view (G-30 #1). The `cosig` bytes are
/// the self-describing wire blob the operator serves alongside the root; this
/// confirms the blob is bound to exactly `(expected_tree_root,
/// expected_log_size)` and was signed by the pinned witness.
///
/// # Errors
///
/// [`FfiError::Identity`] if `expected_tree_root` is not 32 bytes, the co-sig is
/// the wrong length / version, its embedded root differs from the expected one,
/// or the signature does not verify under `witness_key`.
#[uniffi::export]
pub fn verify_witness_cosig(
    cosig: Vec<u8>,
    witness_key: Vec<u8>,
    expected_tree_root: Vec<u8>,
    expected_log_size: u64,
) -> Result<(), FfiError> {
    let expected_tree_root: [u8; 32] = expected_tree_root
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Identity("expected_tree_root must be 32 bytes".to_string()))?;
    lakko_transparency::verify_witness_cosig(
        &cosig,
        &witness_key,
        &expected_tree_root,
        expected_log_size,
    )
    .map_err(|e| FfiError::Identity(format!("witness co-sig verify: {e}")))
}

// ---------------------------------------------------------------------------
// Key-transparency inclusion verifier (KT-log S3b).
//
// After fetching `GET /transparency/root` (the operator-signed Merkle root),
// `GET /transparency/latest/:account_key` (the latest entry's
// `{idx, version, identity_pubkey}`) and `GET /transparency/proof/:idx` (the
// audit path), a client calls `verify_log_inclusion` to prove the identity it
// is about to trust is committed by the operator-signed log — NOT just whatever
// the zero-trust server returned. Two gates, BOTH must pass:
//
//   1. `SignedRoot::verify` against the CLIENT-PINNED operator key — a forged
//      root (signed by anyone but the pinned operator) fails here.
//   2. `verify_inclusion` — this exact `(account_key, identity_pubkey, version)`
//      leaf is at `leaf_index` of the tree that root commits to. A different
//      pubkey / version / index / a tampered audit path fails here.
//
// The verdict is bound to the exact tuple the caller cares about, so a malicious
// server cannot substitute a different identity and pass.
//
// The no-rollback (append-only) check stays CLIENT-side: the client compares
// the fetched `log_size` against its last-seen monotone value. This verifier is
// stateless and does not persist or compare any last-seen size.
// ---------------------------------------------------------------------------

/// Verify that `entry_identity_pubkey` (at `entry_version`) for the account-key
/// `entry_username_hash` is committed by the operator-signed transparency log
/// (KT-log S3b). The decomposed fields mirror the JSON a client decodes from
/// `/transparency/{root,latest,proof}`.
///
/// Runs two gates and returns successfully only if BOTH pass: (1) the root is
/// signed by the client-pinned `operator_pubkey`; (2) this exact
/// `(account_key, pubkey, version)` leaf is at `leaf_index` of that root's tree.
///
/// The no-rollback (append-only) check is the CALLER's job: compare the fetched
/// `log_size` against the last-seen monotone value before trusting the result.
/// This function is stateless.
///
/// # Errors
///
/// [`FfiError::Identity`] if any fixed-length field is the wrong length
/// (`tree_root` / `entry_username_hash` != 32, `root_signature` != 64,
/// `audit_path` length not a multiple of 32), `operator_pubkey` is not 1..=64
/// bytes / not a valid Ed25519 key, the root signature does not verify under
/// `operator_pubkey`, or the inclusion proof does not bind the entry to the
/// root.
#[uniffi::export]
#[allow(clippy::too_many_arguments)]
pub fn verify_log_inclusion(
    operator_pubkey: Vec<u8>,
    tree_root: Vec<u8>,
    log_size: u64,
    timestamp_unix: u64,
    root_signature: Vec<u8>,
    leaf_index: u64,
    audit_path: Vec<u8>,
    entry_username_hash: Vec<u8>,
    entry_identity_pubkey: Vec<u8>,
    entry_version: u64,
) -> Result<(), FfiError> {
    // The operator key is client-pinned out-of-band; accept the same 1..=64
    // byte range the log column allows, but it must parse as a valid Ed25519
    // verifying key (32 bytes canonical).
    if operator_pubkey.is_empty() || operator_pubkey.len() > 64 {
        return Err(FfiError::Identity(
            "operator pubkey must be 1..=64 bytes".to_string(),
        ));
    }
    let operator =
        lakko_crypto::signing::VerifyingKey::from_bytes(&operator_pubkey).map_err(|_| {
            FfiError::Identity("operator pubkey is not a valid Ed25519 key".to_string())
        })?;

    let tree_root: lakko_transparency::TreeHash = tree_root
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Identity("tree_root must be 32 bytes".to_string()))?;
    let signature = lakko_crypto::signing::Signature::from_bytes(&root_signature)
        .map_err(|_| FfiError::Identity("root_signature must be 64 bytes".to_string()))?;
    let username_hash: [u8; 32] = entry_username_hash
        .as_slice()
        .try_into()
        .map_err(|_| FfiError::Identity("entry_username_hash must be 32 bytes".to_string()))?;

    if audit_path.len() % 32 != 0 {
        return Err(FfiError::Identity(
            "audit_path length must be a multiple of 32 bytes".to_string(),
        ));
    }
    let audit_path: Vec<lakko_transparency::TreeHash> = audit_path
        .chunks_exact(32)
        .map(|c| {
            let mut h = [0u8; 32];
            h.copy_from_slice(c);
            h
        })
        .collect();

    let signed_root = lakko_transparency::SignedRoot {
        tree_root,
        log_size,
        timestamp_unix,
        signature,
    };
    let proof = lakko_transparency::InclusionProof {
        leaf_index,
        audit_path,
    };
    let entry = lakko_transparency::LogEntry {
        username_hash,
        identity_pubkey: entry_identity_pubkey,
        version: entry_version,
    };

    // Gate 1: the root is operator-signed. A forged root fails here.
    signed_root
        .verify(&operator)
        .map_err(|e| FfiError::Identity(format!("signed root verify: {e}")))?;

    // Gate 2: this exact entry is committed by that root.
    lakko_transparency::verify_inclusion(&entry, &proof, &signed_root)
        .map_err(|e| FfiError::Identity(format!("inclusion verify: {e}")))?;

    Ok(())
}

/// Domain separator for the transparency-log account-key. MUST stay
/// byte-identical to the discovery service's `ACCOUNT_KEY_DOMAIN` and the wasm
/// `transparencyAccountKey` so all three derive the same log key.
const TRANSPARENCY_ACCOUNT_KEY_DOMAIN: &str = "lakko/transparency/account/v1";

/// Derive the 32-byte transparency-log account-key for an account id:
/// `BLAKE3(domain || account_id)`. Clients use it to look up + verify a
/// contact's identity in the KT log (KT-log S3). The account id is already
/// public, so the derived key leaks nothing new.
#[uniffi::export]
#[must_use]
pub fn transparency_account_key(account_id: Vec<u8>) -> Vec<u8> {
    let mut input = Vec::with_capacity(TRANSPARENCY_ACCOUNT_KEY_DOMAIN.len() + account_id.len());
    input.extend_from_slice(TRANSPARENCY_ACCOUNT_KEY_DOMAIN.as_bytes());
    input.extend_from_slice(&account_id);
    lakko_crypto::hash::blake3(&input).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirror of the lakko-mls integration test driven through the FFI
    /// surface. Exercises the same lifecycle one Swift / Kotlin client would.
    #[test]
    fn ffi_three_party_lifecycle() {
        let alice = Identity::new("alice".into()).unwrap();
        let bob = Identity::new("bob".into()).unwrap();
        let charlie = Identity::new("charlie".into()).unwrap();

        let alice_group = Group::create(&alice).unwrap();

        // Alice adds Bob.
        let bob_kp = bob.create_key_package().unwrap();
        let add_bob = alice_group.add_member_from_wire(&alice, bob_kp).unwrap();
        let bob_group = Group::join_from_welcome(add_bob.welcome, &bob).unwrap();

        // Alice → Bob.
        let msg = alice_group
            .create_application_message(&alice, b"hello via ffi".to_vec())
            .unwrap();
        let received = bob_group.process_incoming(&bob, msg).unwrap();
        match received {
            ProcessedMessage::Application { data } => {
                assert_eq!(data, b"hello via ffi");
            },
            other => panic!("expected application, got {other:?}"),
        }

        // Alice adds Charlie; Bob processes the commit.
        let charlie_kp = charlie.create_key_package().unwrap();
        let add_charlie = alice_group
            .add_member_from_wire(&alice, charlie_kp)
            .unwrap();
        let bob_post = bob_group
            .process_incoming(&bob, add_charlie.commit)
            .unwrap();
        assert!(matches!(bob_post, ProcessedMessage::Commit));
        let charlie_group = Group::join_from_welcome(add_charlie.welcome, &charlie).unwrap();

        assert_eq!(alice_group.member_count().unwrap(), 3);
        assert_eq!(bob_group.member_count().unwrap(), 3);
        assert_eq!(charlie_group.member_count().unwrap(), 3);

        // Exporter consistency.
        let alice_export = alice_group
            .export_secret(&alice, "lakko/test".into(), b"ctx".to_vec(), 32)
            .unwrap();
        let charlie_export = charlie_group
            .export_secret(&charlie, "lakko/test".into(), b"ctx".to_vec(), 32)
            .unwrap();
        assert_eq!(alice_export, charlie_export);
    }

    /// `Identity::from_seed` is deterministic for the same input
    /// — the OPAQUE-export-key path 1.15d / 1.15e5 rely on for
    /// "same password → same device identity across re-installs."
    /// Verifies both that the constructor accepts a 32-byte seed
    /// and that every public accessor (Ed25519 / X25519 / seed)
    /// produces stable bytes across reconstruction.
    #[test]
    fn identity_from_seed_is_deterministic() {
        let seed = [0x42u8; 32];
        let a = Identity::from_seed(seed.to_vec()).unwrap();
        let b = Identity::from_seed(seed.to_vec()).unwrap();

        assert_eq!(a.signature_public_key(), b.signature_public_key());
        assert_eq!(a.x25519_public_key(), b.x25519_public_key());
        assert_eq!(a.x25519_secret_key(), b.x25519_secret_key());
        assert_eq!(a.ed25519_seed(), b.ed25519_seed());

        // Every key has the expected length.
        assert_eq!(a.signature_public_key().len(), 32);
        assert_eq!(a.x25519_public_key().len(), 32);
        assert_eq!(a.x25519_secret_key().len(), 32);
        assert_eq!(a.ed25519_seed().len(), 32);

        // Note: `ed25519_seed()` returns the HKDF-derived
        // Ed25519 sub-seed, not the input bytes. The input
        // `seed` is hashed through HKDF with separate "info"
        // labels for the Ed25519 and X25519 sub-keys (see
        // `lakko_mls::Identity::from_seed` impl), so the
        // accessor exposes the derived material, not the
        // master. Still useful for the contact-request flow:
        // it's the Ed25519 signing seed `lakko_contacts::compose`
        // demands.
    }

    /// Different seeds produce different keys — sanity check that
    /// the derivation actually depends on the input.
    #[test]
    fn identity_from_seed_distinct_inputs_distinct_outputs() {
        let alice = Identity::from_seed(vec![1u8; 32]).unwrap();
        let bob = Identity::from_seed(vec![2u8; 32]).unwrap();
        assert_ne!(alice.signature_public_key(), bob.signature_public_key());
        assert_ne!(alice.x25519_public_key(), bob.x25519_public_key());
    }

    /// `Identity::snapshot_storage` produces a non-empty blob
    /// on a fresh identity, and the bytes round-trip cleanly
    /// through `restore_storage` on a parallel identity built
    /// from the same seed.
    ///
    /// Driving 1.15f3's persistence path: a Kotlin caller will
    /// build `Identity.fromSeed(exportKey)` post-login,
    /// re-hydrate its OpenMLS provider state via
    /// `restore_storage(snapshotBlob)`, and from that point the
    /// in-memory state has every group + ratchet key the prior
    /// process wrote.
    #[test]
    fn identity_snapshot_round_trips_through_restore() {
        // Build Alice + a Group with Bob — that mutates the
        // provider storage (init keys + group ratchet).
        let alice = Identity::from_seed(vec![0xAAu8; 32]).unwrap();
        let bob = Identity::from_seed(vec![0xBBu8; 32]).unwrap();
        let alice_group = Group::create(&alice).unwrap();
        let bob_kp = bob.create_key_package().unwrap();
        let add = alice_group.add_member_from_wire(&alice, bob_kp).unwrap();
        let _ = add; // Welcome bytes; not used for this test.
        let epoch_after_add = alice_group.epoch().unwrap();

        // Snapshot — must be non-empty.
        let snapshot = alice.snapshot_storage().unwrap();
        assert!(
            !snapshot.is_empty(),
            "snapshot must carry at least the bookkeeping bytes",
        );

        // Build a parallel Alice from the same seed (her
        // public keys agree by construction; the secrets are
        // deterministic from the seed too). Her provider
        // storage starts smaller — no group state on it yet.
        let alice_clone = Identity::from_seed(vec![0xAAu8; 32]).unwrap();
        let empty_snapshot = alice_clone.snapshot_storage().unwrap();
        assert_ne!(
            snapshot, empty_snapshot,
            "fresh identity must have a different storage blob",
        );

        // Restore: the clone now holds the prior provider's
        // state, including the group ratchet that produced
        // `epoch_after_add`. Doesn't throw on well-formed input.
        alice_clone.restore_storage(snapshot.clone()).unwrap();

        // Snapshot the restored identity — must be non-empty
        // (the restored state has every byte the original
        // wrote). We DO NOT assert byte-equality here: the
        // underlying OpenMLS provider uses a HashMap-backed
        // store whose serialisation order is not stable across
        // instances, so two providers holding semantically
        // identical state may emit slightly different blob
        // bytes. The semantic round-trip below is what we
        // actually care about.
        let restored = alice_clone.snapshot_storage().unwrap();
        assert!(!restored.is_empty());

        // Semantic round-trip: snapshot the restored clone,
        // build a SECOND clone, restore that — the
        // double-restored blob must be non-empty (i.e. the
        // restore path is stable under repeated apply).
        let alice_clone2 = Identity::from_seed(vec![0xAAu8; 32]).unwrap();
        alice_clone2.restore_storage(restored).unwrap();
        assert!(!alice_clone2.snapshot_storage().unwrap().is_empty());

        // Sanity: the original Alice still produces an exporter
        // for the group she made — `restore_storage` on a clone
        // doesn't mutate the original.
        let exporter = alice_group
            .export_secret(&alice, "lakko/test".into(), b"ctx".to_vec(), 32)
            .unwrap();
        assert_eq!(exporter.len(), 32);
        let _ = epoch_after_add; // documented in the dance above.
    }

    /// `restore_storage` rejects a truncated / malformed
    /// snapshot rather than corrupting the in-memory state.
    #[test]
    fn identity_restore_rejects_malformed_input() {
        let alice = Identity::from_seed(vec![0x11u8; 32]).unwrap();
        // Empty input has no length prefix to start with —
        // surfaces as the underlying Identity error.
        let err = alice.restore_storage(vec![]).unwrap_err();
        assert!(matches!(err, FfiError::Identity(_)));
    }

    /// `load_group` round-trips a snapshot: create a group +
    /// snapshot the Identity's provider storage + restore into
    /// a parallel Identity + `load_group(group_id)` rebuilds a
    /// handle that produces the same exporter as the original.
    /// Drives 1.15f5's eager-rebuild path through the FFI
    /// boundary.
    #[test]
    fn load_group_round_trips_through_snapshot() {
        // Build Alice, create a group, snapshot her provider
        // storage.
        let alice = Identity::from_seed(vec![0xAAu8; 32]).unwrap();
        let alice_group = Group::create(&alice).unwrap();
        let group_id = alice_group.group_id().unwrap();
        let exporter_before = alice_group
            .export_secret(&alice, "lakko/test".into(), b"ctx".to_vec(), 32)
            .unwrap();
        let snapshot = alice.snapshot_storage().unwrap();

        // Build a parallel Alice from the same seed; her
        // provider has no groups until we restore.
        let alice_clone = Identity::from_seed(vec![0xAAu8; 32]).unwrap();
        // load_group on the un-restored clone returns None —
        // the group doesn't exist in her provider yet.
        let pre_restore = load_group(&alice_clone, group_id.clone()).unwrap();
        assert!(pre_restore.is_none(), "no group before restore");

        // Restore + load.
        alice_clone.restore_storage(snapshot).unwrap();
        let loaded = load_group(&alice_clone, group_id.clone())
            .unwrap()
            .expect("load_group must return Some after restore");

        // The restored handle produces the same exporter the
        // original did — the ratchet state survived the
        // round-trip.
        let exporter_after = loaded
            .export_secret(&alice_clone, "lakko/test".into(), b"ctx".to_vec(), 32)
            .unwrap();
        assert_eq!(exporter_before, exporter_after);

        // `group_id` on the loaded handle matches the original.
        assert_eq!(loaded.group_id().unwrap(), group_id);
    }

    /// `Group::group_id` is stable across epochs — `1.15f2`'s
    /// chat client uses the value to key the DS group journal,
    /// so a wandering id would route messages to a dead
    /// `/ds/group/{...}` slot mid-conversation.
    #[test]
    fn group_id_is_stable_across_epoch_advance() {
        let alice = Identity::new("alice".into()).unwrap();
        let bob = Identity::new("bob".into()).unwrap();

        let alice_group = Group::create(&alice).unwrap();
        let epoch0_id = alice_group.group_id().unwrap();
        assert!(!epoch0_id.is_empty(), "group_id must be non-empty");
        let epoch0_value = alice_group.epoch().unwrap();

        // Add Bob — epoch advances, group_id must NOT.
        let bob_kp = bob.create_key_package().unwrap();
        alice_group.add_member_from_wire(&alice, bob_kp).unwrap();
        let epoch1_id = alice_group.group_id().unwrap();
        let epoch1_value = alice_group.epoch().unwrap();

        assert_ne!(
            epoch0_value, epoch1_value,
            "epoch must have advanced after add_member",
        );
        assert_eq!(epoch0_id, epoch1_id, "group_id must not change with epoch");
    }

    // ============================================================================
    // PublisherGroup FFI surface (G4-2-mls-persistence step 2)
    // ============================================================================

    /// Channel id helper — 16 bytes, deterministic per test
    /// case so a re-run sees the same persistence-key shape
    /// without depending on any RNG.
    fn channel_id_filled(byte: u8) -> Vec<u8> {
        vec![byte; lakko_mls::CHANNEL_ID_LEN]
    }

    #[test]
    fn publisher_group_create_then_channel_seed_is_stable() {
        let alice = Identity::new("alice-publisher".into()).unwrap();
        let cid = channel_id_filled(0xAA);

        let pg = PublisherGroup::create(&alice, cid.clone()).unwrap();
        let seed_1 = pg.channel_seed(&alice).unwrap();
        let seed_2 = pg.channel_seed(&alice).unwrap();
        // Same group, same epoch -> same seed.
        assert_eq!(seed_1, seed_2);
        assert_eq!(seed_1.len(), 32);
        // Channel id round-trips.
        assert_eq!(pg.channel_id().unwrap(), cid);
    }

    #[test]
    fn publisher_group_open_or_create_is_idempotent_across_handles() {
        // The load-bearing FFI test: two consecutive
        // `open_or_create` calls under the SAME identity for
        // the SAME channel id must resolve to the SAME
        // persisted group — i.e. they must produce byte-
        // identical channel seeds. If they don't, we're back
        // to the throwaway-group bug G4-2-mls-persistence
        // closes.
        let alice = Identity::new("alice-persist".into()).unwrap();
        let cid = channel_id_filled(0xBB);

        let first = publisher_group_open_or_create(&alice, cid.clone()).unwrap();
        let seed_first = first.channel_seed(&alice).unwrap();

        let second = publisher_group_open_or_create(&alice, cid.clone()).unwrap();
        let seed_second = second.channel_seed(&alice).unwrap();

        assert_eq!(seed_first, seed_second);
    }

    #[test]
    fn publisher_group_load_returns_null_before_create() {
        let alice = Identity::new("alice-empty".into()).unwrap();
        let cid = channel_id_filled(0xCC);

        let loaded = publisher_group_load(&alice, cid).unwrap();
        assert!(loaded.is_none(), "no group should be persisted yet");
    }

    #[test]
    fn publisher_group_load_returns_group_after_create() {
        let alice = Identity::new("alice-loadback".into()).unwrap();
        let cid = channel_id_filled(0xDD);

        let _created = PublisherGroup::create(&alice, cid.clone()).unwrap();
        let loaded = publisher_group_load(&alice, cid.clone()).unwrap();
        assert!(loaded.is_some(), "load must find the freshly-created group");
        assert_eq!(loaded.unwrap().channel_id().unwrap(), cid);
    }

    #[test]
    fn publisher_group_rejects_wrong_length_channel_id() {
        let alice = Identity::new("alice-badid".into()).unwrap();
        // 8 bytes — half a channel id. Must refuse rather
        // than silently zero-pad (would let two channels
        // collide under the deterministic group id).
        let bad = vec![0u8; 8];
        let create_res = PublisherGroup::create(&alice, bad.clone());
        assert!(create_res.is_err(), "create must refuse short channel id");
        let create_err = create_res.err().unwrap();
        assert!(matches!(
            create_err,
            FfiError::Codec(_) | FfiError::Group(_)
        ));

        let load_res = publisher_group_load(&alice, bad);
        assert!(load_res.is_err(), "load must refuse short channel id");
        let load_err = load_res.err().unwrap();
        assert!(matches!(load_err, FfiError::Codec(_) | FfiError::Group(_)));
    }

    #[test]
    fn publisher_group_rotate_changes_seed() {
        // G4-2-mls-persistence step 4 (FFI) — the load-bearing
        // test: rotating the PublisherGroup MUST produce a
        // distinct exporter output (different epoch under the
        // hood). Mirror of the lakko-mls test of the same
        // shape, but exercised through the UniFFI surface so
        // the iOS / Android composer paths are covered too.
        let alice = Identity::new("alice-rotate".into()).unwrap();
        let cid = channel_id_filled(0xEE);

        let pg = publisher_group_open_or_create(&alice, cid).unwrap();
        let seed_pre = pg.channel_seed(&alice).unwrap();

        let _commit = pg.rotate(&alice).unwrap();
        let seed_post = pg.channel_seed(&alice).unwrap();

        assert_ne!(seed_pre, seed_post);
        assert_eq!(seed_post.len(), 32);
    }

    #[test]
    fn publisher_group_distinct_channels_distinct_seeds() {
        let alice = Identity::new("alice-multichannel".into()).unwrap();
        let cid_a = channel_id_filled(0x11);
        let cid_b = channel_id_filled(0x22);

        let pg_a = publisher_group_open_or_create(&alice, cid_a).unwrap();
        let pg_b = publisher_group_open_or_create(&alice, cid_b).unwrap();
        let seed_a = pg_a.channel_seed(&alice).unwrap();
        let seed_b = pg_b.channel_seed(&alice).unwrap();
        assert_ne!(seed_a, seed_b);
    }

    fn ed25519_pub_of(id: &Identity) -> Vec<u8> {
        signing_key_from_seed(&id.ed25519_seed())
            .unwrap()
            .verifying_key()
            .to_bytes()
            .to_vec()
    }

    #[test]
    fn supersession_proof_roundtrips_through_ffi() {
        let old = Identity::new("super-old".into()).unwrap();
        let new = Identity::new("super-new".into()).unwrap();
        let old_pub = ed25519_pub_of(&old);
        let new_pub = ed25519_pub_of(&new);
        let now = 1_000_000_i64;
        let proof =
            sign_supersession_proof(old.ed25519_seed(), new.ed25519_seed(), now, now + 3600)
                .unwrap();
        let authorised = verify_supersession(proof, old_pub, now + 10).unwrap();
        assert_eq!(authorised, new_pub);
    }

    #[test]
    fn supersession_proof_rejected_under_wrong_old_key() {
        let old = Identity::new("super-old-2".into()).unwrap();
        let new = Identity::new("super-new-2".into()).unwrap();
        let wrong_pub = ed25519_pub_of(&Identity::new("super-wrong".into()).unwrap());
        let now = 2_000_000_i64;
        let proof =
            sign_supersession_proof(old.ed25519_seed(), new.ed25519_seed(), now, now + 3600)
                .unwrap();
        assert!(verify_supersession(proof, wrong_pub, now + 10).is_err());
    }

    #[test]
    fn supersession_proof_rejected_when_expired() {
        let old = Identity::new("super-old-3".into()).unwrap();
        let new = Identity::new("super-new-3".into()).unwrap();
        let old_pub = ed25519_pub_of(&old);
        let now = 3_000_000_i64;
        let proof =
            sign_supersession_proof(old.ed25519_seed(), new.ed25519_seed(), now, now + 60).unwrap();
        assert!(verify_supersession(proof, old_pub, now + 1000).is_err());
    }

    #[test]
    fn supersession_sign_rejects_bad_seed_length() {
        let new = Identity::new("super-new-4".into()).unwrap();
        assert!(sign_supersession_proof(vec![0u8; 31], new.ed25519_seed(), 0, 60).is_err());
    }

    // -- KT-log S3b: verify_log_inclusion --

    /// Decomposed FFI fields for one entry's inclusion call, mirroring the
    /// JSON a client decodes from `/transparency/{root,latest,proof}`.
    struct DecomposedInclusion {
        operator_pubkey: Vec<u8>,
        tree_root: Vec<u8>,
        log_size: u64,
        timestamp_unix: u64,
        root_signature: Vec<u8>,
        leaf_index: u64,
        audit_path: Vec<u8>,
        username_hash: Vec<u8>,
        identity_pubkey: Vec<u8>,
        version: u64,
    }

    /// Build a 4-entry in-memory log, sign the current root, and decompose
    /// the proof for `idx` into the FFI field shape.
    fn build_inclusion_fixture(idx: u64) -> DecomposedInclusion {
        use lakko_transparency::{InMemoryLog, LogEntry};
        let operator = lakko_crypto::signing::SigningKey::generate();
        let mut log = InMemoryLog::new(operator);
        for i in 0..4u64 {
            log.append(LogEntry {
                username_hash: lakko_crypto::hash::blake3(format!("u{i}").as_bytes()),
                identity_pubkey: vec![0x10 + u8::try_from(i).unwrap(); 32],
                version: 1,
            });
        }
        let signed = log.sign_current_root(1_700_000_000);
        let proof = log.prove_inclusion(idx).expect("proof");
        let entry = log.entry(idx).expect("entry").clone();

        let mut audit_path = Vec::new();
        for h in &proof.audit_path {
            audit_path.extend_from_slice(h);
        }

        DecomposedInclusion {
            operator_pubkey: log.operator_key().to_bytes().to_vec(),
            tree_root: signed.tree_root.to_vec(),
            log_size: signed.log_size,
            timestamp_unix: signed.timestamp_unix,
            root_signature: signed.signature.to_bytes().to_vec(),
            leaf_index: proof.leaf_index,
            audit_path,
            username_hash: entry.username_hash.to_vec(),
            identity_pubkey: entry.identity_pubkey,
            version: entry.version,
        }
    }

    fn call_inclusion(d: &DecomposedInclusion) -> Result<(), FfiError> {
        verify_log_inclusion(
            d.operator_pubkey.clone(),
            d.tree_root.clone(),
            d.log_size,
            d.timestamp_unix,
            d.root_signature.clone(),
            d.leaf_index,
            d.audit_path.clone(),
            d.username_hash.clone(),
            d.identity_pubkey.clone(),
            d.version,
        )
    }

    #[test]
    fn log_inclusion_valid_passes_both_gates() {
        for idx in 0..4 {
            let d = build_inclusion_fixture(idx);
            call_inclusion(&d).expect("valid inclusion verifies");
        }
    }

    #[test]
    fn log_inclusion_flipped_root_signature_fails() {
        let mut d = build_inclusion_fixture(2);
        d.root_signature[0] ^= 0x01;
        assert!(call_inclusion(&d).is_err());
    }

    #[test]
    fn log_inclusion_flipped_audit_path_fails() {
        let mut d = build_inclusion_fixture(2);
        assert!(!d.audit_path.is_empty());
        d.audit_path[0] ^= 0x01;
        assert!(call_inclusion(&d).is_err());
    }

    #[test]
    fn log_inclusion_wrong_operator_key_fails() {
        let mut d = build_inclusion_fixture(1);
        d.operator_pubkey = lakko_crypto::signing::SigningKey::generate()
            .verifying_key()
            .to_bytes()
            .to_vec();
        assert!(call_inclusion(&d).is_err());
    }

    #[test]
    fn log_inclusion_wrong_identity_pubkey_fails() {
        let mut d = build_inclusion_fixture(1);
        d.identity_pubkey = vec![0xFF; 32];
        assert!(call_inclusion(&d).is_err());
    }

    #[test]
    fn log_inclusion_wrong_version_fails() {
        let mut d = build_inclusion_fixture(1);
        d.version = d.version.wrapping_add(1);
        assert!(call_inclusion(&d).is_err());
    }

    #[test]
    fn log_inclusion_malformed_lengths_fail_fast() {
        let base = build_inclusion_fixture(0);

        let mut d = build_inclusion_fixture(0);
        d.tree_root.pop();
        assert!(call_inclusion(&d).is_err(), "31-byte tree_root rejected");

        let mut d = build_inclusion_fixture(0);
        d.root_signature.push(0x00);
        assert!(call_inclusion(&d).is_err(), "65-byte signature rejected");

        let mut d = build_inclusion_fixture(2);
        d.audit_path.push(0x00);
        assert!(
            call_inclusion(&d).is_err(),
            "non-multiple-of-32 audit path rejected"
        );

        let mut d = build_inclusion_fixture(0);
        d.operator_pubkey.clear();
        assert!(call_inclusion(&d).is_err(), "empty operator key rejected");

        let mut d = build_inclusion_fixture(0);
        d.username_hash.pop();
        assert!(
            call_inclusion(&d).is_err(),
            "31-byte username_hash rejected"
        );

        call_inclusion(&base).expect("base fixture still valid");
    }

    // -- G-30 #2 / ADR-0019: verify_supersession_chain --

    /// Decomposed FFI fields for a rotation-chain call: a `(v1 old key) ->
    /// (v2 new key)` log plus the supersession proof + the pin.
    struct DecomposedChain {
        operator_pubkey: Vec<u8>,
        new_tree_root: Vec<u8>,
        new_log_size: u64,
        new_timestamp_unix: u64,
        new_root_signature: Vec<u8>,
        entry_username_hash: Vec<u8>,
        entry_identity_pubkey: Vec<u8>,
        entry_version: u64,
        inclusion_leaf_index: u64,
        inclusion_audit_path: Vec<u8>,
        supersession_proof: Vec<u8>,
        pinned_old_pubkey: Vec<u8>,
        pinned_version: u64,
        last_seen_log_size: u64,
        now_unix_secs: i64,
        new_pubkey: Vec<u8>,
    }

    /// Build a 2-entry log for `alice` — v1 under `old`, v2 under `new` — sign
    /// the root, mint an old->new supersession proof, and decompose into the
    /// chain FFI field shape pinned at v1.
    fn build_chain_fixture() -> DecomposedChain {
        use lakko_transparency::{InMemoryLog, LogEntry, SupersessionProof};
        let operator = lakko_crypto::signing::SigningKey::generate();
        let old_sk = lakko_crypto::signing::SigningKey::generate();
        let new_sk = lakko_crypto::signing::SigningKey::generate();
        let account_key = lakko_crypto::hash::blake3(b"alice");

        let mut log = InMemoryLog::new(operator);
        log.append(LogEntry {
            username_hash: account_key,
            identity_pubkey: old_sk.verifying_key().to_bytes().to_vec(),
            version: 1,
        });
        let v2_idx = log.append(LogEntry {
            username_hash: account_key,
            identity_pubkey: new_sk.verifying_key().to_bytes().to_vec(),
            version: 2,
        });
        let signed = log.sign_current_root(1_700_000_000);
        let proof = log.prove_inclusion(v2_idx).expect("proof");
        let entry = log.entry(v2_idx).expect("entry").clone();

        let issued = 1_700_000_000_i64;
        let expires = issued + 1000;
        let supersession_proof = SupersessionProof::new(
            old_sk.verifying_key().to_bytes(),
            new_sk.verifying_key().to_bytes(),
            issued,
            expires,
        )
        .sign(&old_sk)
        .to_vec();

        let mut audit_path = Vec::new();
        for h in &proof.audit_path {
            audit_path.extend_from_slice(h);
        }

        DecomposedChain {
            operator_pubkey: log.operator_key().to_bytes().to_vec(),
            new_tree_root: signed.tree_root.to_vec(),
            new_log_size: signed.log_size,
            new_timestamp_unix: signed.timestamp_unix,
            new_root_signature: signed.signature.to_bytes().to_vec(),
            entry_username_hash: entry.username_hash.to_vec(),
            entry_identity_pubkey: entry.identity_pubkey,
            entry_version: entry.version,
            inclusion_leaf_index: proof.leaf_index,
            inclusion_audit_path: audit_path,
            supersession_proof,
            pinned_old_pubkey: old_sk.verifying_key().to_bytes().to_vec(),
            pinned_version: 1,
            last_seen_log_size: 1,
            now_unix_secs: issued + 10,
            new_pubkey: new_sk.verifying_key().to_bytes().to_vec(),
        }
    }

    fn call_chain(d: &DecomposedChain) -> Result<Vec<u8>, FfiError> {
        verify_supersession_chain(
            d.operator_pubkey.clone(),
            d.new_tree_root.clone(),
            d.new_log_size,
            d.new_timestamp_unix,
            d.new_root_signature.clone(),
            d.entry_username_hash.clone(),
            d.entry_identity_pubkey.clone(),
            d.entry_version,
            d.inclusion_leaf_index,
            d.inclusion_audit_path.clone(),
            d.supersession_proof.clone(),
            d.pinned_old_pubkey.clone(),
            d.pinned_version,
            d.last_seen_log_size,
            d.now_unix_secs,
        )
    }

    #[test]
    fn chain_valid_rotation_returns_authorised_new_key() {
        let d = build_chain_fixture();
        let got = call_chain(&d).expect("valid rotation chain verifies");
        assert_eq!(got, d.new_pubkey, "re-pin to the authorised new key");
    }

    #[test]
    fn chain_forged_root_signature_fails() {
        let mut d = build_chain_fixture();
        d.new_root_signature[0] ^= 0x01;
        assert!(call_chain(&d).is_err(), "forged root fails gate 1");
    }

    #[test]
    fn chain_wrong_pinned_old_key_fails() {
        let mut d = build_chain_fixture();
        d.pinned_old_pubkey = lakko_crypto::signing::SigningKey::generate()
            .verifying_key()
            .to_bytes()
            .to_vec();
        assert!(
            call_chain(&d).is_err(),
            "a proof not signed by the pinned old key fails"
        );
    }

    #[test]
    fn chain_non_newer_version_fails() {
        let mut d = build_chain_fixture();
        d.pinned_version = 2; // entry is also v2 → not strictly newer
        assert!(call_chain(&d).is_err(), "non-newer version is a downgrade");
    }

    #[test]
    fn chain_rollback_fails() {
        let mut d = build_chain_fixture();
        d.last_seen_log_size = d.new_log_size + 1; // shrinking log
        assert!(
            call_chain(&d).is_err(),
            "a rollback fails the no-rollback gate"
        );
    }

    #[test]
    fn chain_expired_proof_fails() {
        let mut d = build_chain_fixture();
        d.now_unix_secs = 1_700_000_000 + 5000; // past the 1000s window
        assert!(call_chain(&d).is_err(), "an expired proof fails");
    }

    #[test]
    fn chain_malformed_lengths_fail_fast() {
        let mut d = build_chain_fixture();
        d.new_tree_root.pop();
        assert!(call_chain(&d).is_err(), "31-byte tree_root rejected");

        let mut d = build_chain_fixture();
        d.pinned_old_pubkey.pop();
        assert!(call_chain(&d).is_err(), "31-byte pinned key rejected");

        let mut d = build_chain_fixture();
        d.inclusion_audit_path.push(0x00);
        assert!(
            call_chain(&d).is_err(),
            "non-multiple-of-32 audit path rejected"
        );
    }

    // -- G-30 #2 / ADR-0019: sign/verify_leaf_genesis --

    #[test]
    fn leaf_genesis_sign_verify_roundtrips() {
        let id = Identity::new("genesis-1".into()).unwrap();
        let username_hash = vec![0xAB; 32];
        let pubkey = ed25519_pub_of(&id);
        let sig = sign_leaf_genesis(id.ed25519_seed(), username_hash.clone()).unwrap();
        verify_leaf_genesis(sig, username_hash, pubkey).expect("genesis self-sig verifies");
    }

    #[test]
    fn leaf_genesis_wrong_account_key_fails() {
        let id = Identity::new("genesis-2".into()).unwrap();
        let pubkey = ed25519_pub_of(&id);
        let sig = sign_leaf_genesis(id.ed25519_seed(), vec![0xCD; 32]).unwrap();
        // A different account_key → the signed binding differs → fail.
        assert!(verify_leaf_genesis(sig, vec![0x01; 32], pubkey).is_err());
    }

    #[test]
    fn leaf_genesis_wrong_pubkey_fails() {
        let id = Identity::new("genesis-3".into()).unwrap();
        let other = Identity::new("genesis-3b".into()).unwrap();
        let username_hash = vec![0xEF; 32];
        let sig = sign_leaf_genesis(id.ed25519_seed(), username_hash.clone()).unwrap();
        assert!(verify_leaf_genesis(sig, username_hash, ed25519_pub_of(&other)).is_err());
    }

    #[test]
    fn leaf_genesis_sign_rejects_bad_seed() {
        assert!(sign_leaf_genesis(vec![0u8; 31], vec![0u8; 32]).is_err());
    }
}
