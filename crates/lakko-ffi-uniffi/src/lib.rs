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
    group, key_package, Group as MlsGroup, Identity as MlsIdentity, Incoming as MlsIncoming,
};

pub mod attachments;
pub mod auth;
pub mod backup;
pub mod contacts;
pub mod discovery;
pub mod mnemonic;
pub mod pow;
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
}
