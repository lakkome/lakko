// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! MLS-layer wasm-bindgen mirror — `Identity`, `Group`, `AddMember`,
//! `ProcessedMessage`. Phase 1.13a content; the rest of the FFI
//! surface (sync / push / presence / contacts / storage) lives in
//! sibling modules and is wired through `lib.rs`.

use std::sync::Mutex;

use lakko_mls::group::MemberAddition;
use lakko_mls::{
    group, key_package, DeviceGroup as MlsDeviceGroup, Group as MlsGroup, Identity as MlsIdentity,
    Incoming as MlsIncoming,
};
use wasm_bindgen::prelude::*;

/// Map a `lakko_mls::Error` to a `JsError` that surfaces in JS as a
/// thrown `Error` with the message preserved. We deliberately don't
/// expose a structured JS class for errors — adding one is cheap
/// when `apps/web` actually needs to discriminate.
fn mls_err(e: lakko_mls::Error) -> JsError {
    JsError::new(&format!("{e}"))
}

// ============================================================================
// Identity
// ============================================================================

/// Lakko identity — owns the device's signing keypair, MLS credential
/// and per-device storage provider. Mirror of UniFFI's `Identity`.
///
/// JS usage:
///
/// ```js
/// import { Identity } from "@lakko/ffi-wasm";
/// const alice = new Identity("alice");
/// const pk = alice.signaturePublicKey;        // Uint8Array
/// const kp = alice.createKeyPackage();        // Uint8Array
/// ```
#[wasm_bindgen]
pub struct Identity {
    inner: MlsIdentity,
}

#[wasm_bindgen]
impl Identity {
    /// Generate a fresh identity with the given user-display label.
    #[wasm_bindgen(constructor)]
    pub fn new(name: &str) -> Result<Identity, JsError> {
        let inner = MlsIdentity::generate(name).map_err(mls_err)?;
        Ok(Self { inner })
    }

    /// Derive an identity **deterministically** from a seed.
    ///
    /// Same `seed` ⇒ byte-identical Ed25519 + X25519 keys. The
    /// expected caller is the `apps/web` auth layer, which
    /// seeds this with the OPAQUE password export key so a
    /// fresh login on any device reproduces the user's
    /// long-lived public keys without persisting Identity bytes
    /// anywhere.
    #[wasm_bindgen(js_name = fromSeed)]
    pub fn from_seed(seed: &[u8]) -> Result<Identity, JsError> {
        let inner = MlsIdentity::from_seed(seed).map_err(mls_err)?;
        Ok(Self { inner })
    }

    /// Public signature key bytes.
    #[wasm_bindgen(getter, js_name = signaturePublicKey)]
    pub fn signature_public_key(&self) -> Vec<u8> {
        self.inner.signature_public_key().to_vec()
    }

    /// Long-lived X25519 public key (32 bytes) — the HPKE
    /// init_key callers of `lakko-contacts::compose` target when
    /// sealing an envelope to this identity. Pair with the bundle
    /// uploaded to the discovery service via
    /// `POST /discovery/identity` (Phase 1.16d3a).
    #[wasm_bindgen(getter, js_name = x25519PublicKey)]
    pub fn x25519_public_key(&self) -> Vec<u8> {
        self.inner.x25519_public_key().to_vec()
    }

    /// Raw X25519 secret bytes (32 bytes). Used as input to
    /// `ContactRequest.openEnvelope(...)` when decrypting an
    /// incoming envelope.
    ///
    /// **Sensitive.** The caller MUST persist these bytes only
    /// after wrapping them with the device root key (1.16c3c2's
    /// `deriveDeviceRootKey` output). They MUST NOT touch
    /// `localStorage` or any other unencrypted store.
    #[wasm_bindgen(getter, js_name = x25519SecretKey)]
    pub fn x25519_secret_key(&self) -> Vec<u8> {
        self.inner.x25519_secret_bytes().to_vec()
    }

    /// Raw Ed25519 signing-seed bytes (32 bytes). Hand this to
    /// `ContactRequest.compose(...)` as the
    /// `senderSigningSeed` argument when sending a contact
    /// request. Same secret-handling rules as
    /// [`x25519SecretKey`](Self::x25519_secret_key).
    #[wasm_bindgen(getter, js_name = ed25519Seed)]
    pub fn ed25519_seed(&self) -> Vec<u8> {
        self.inner.ed25519_seed_bytes().to_vec()
    }

    /// Serialise the OpenMLS provider's storage to opaque
    /// bytes (Phase 1.16e6b). Pair with
    /// [`Identity::restoreStorage`](Self::restore_storage)
    /// to round-trip MLS state across a page reload.
    ///
    /// The bytes are opaque to the JS caller — the only
    /// supported operation is feeding them back through
    /// `restoreStorage` on a fresh `Identity` derived from the
    /// same seed. apps/web wraps the snapshot under the device
    /// root key (AES-256-GCM, 1.16c3c2) before stashing in
    /// IndexedDB so the on-disk bytes can't be lifted by
    /// page-level JavaScript.
    #[wasm_bindgen(js_name = snapshotStorage)]
    pub fn snapshot_storage(&self) -> Result<Vec<u8>, JsError> {
        self.inner.snapshot_storage().map_err(mls_err)
    }

    /// Replace this identity's MLS provider storage with the
    /// contents of a snapshot produced by `snapshotStorage`
    /// on a previous run. **Replaces** rather than merges —
    /// see the Rust-side docstring for the seed-pairing
    /// requirement.
    #[wasm_bindgen(js_name = restoreStorage)]
    pub fn restore_storage(&self, snapshot: &[u8]) -> Result<(), JsError> {
        self.inner.restore_storage(snapshot).map_err(mls_err)
    }

    /// Create a fresh KeyPackage in TLS wire form.
    #[wasm_bindgen(js_name = createKeyPackage)]
    pub fn create_key_package(&self) -> Result<Vec<u8>, JsError> {
        let kp = self.inner.create_key_package().map_err(mls_err)?;
        key_package::serialize(&kp).map_err(mls_err)
    }
}

// ============================================================================
// AddMember (record)
// ============================================================================

/// Result of [`Group::add_member`].
///
/// Distribute `commit` to all existing members for processing; deliver
/// `welcome` to the newly-added member.
#[wasm_bindgen]
pub struct AddMember {
    commit_bytes: Vec<u8>,
    welcome_bytes: Vec<u8>,
}

#[wasm_bindgen]
impl AddMember {
    /// Encoded MLS commit message — broadcast to existing members.
    #[wasm_bindgen(getter)]
    pub fn commit(&self) -> Vec<u8> {
        self.commit_bytes.clone()
    }

    /// Encoded MLS welcome message — sent to the joining member.
    #[wasm_bindgen(getter)]
    pub fn welcome(&self) -> Vec<u8> {
        self.welcome_bytes.clone()
    }
}

// ============================================================================
// GroupMember (record) — Phase 1.18h
// ============================================================================

/// One member's identity snapshot at the current epoch. Mirror
/// of [`lakko_mls::MemberRecord`].
///
/// The JS layer reads `leafIndex` to feed into
/// `Group.removeMember(...)` for kick UX and
/// `signaturePublicKey` to look the member up in the contacts
/// cache / render a fingerprint. `leafIndex` is stable inside a
/// single epoch but can renumber across an add/remove cycle —
/// callers MUST NOT cache it past a `processIncoming` commit.
#[wasm_bindgen]
pub struct GroupMember {
    leaf_index_val: u32,
    signature_public_key_val: Vec<u8>,
}

#[wasm_bindgen]
impl GroupMember {
    /// OpenMLS leaf position at the current epoch.
    #[wasm_bindgen(getter, js_name = leafIndex)]
    pub fn leaf_index(&self) -> u32 {
        self.leaf_index_val
    }

    /// 32-byte Ed25519 verifying key — the member's long-term
    /// identity signing key.
    #[wasm_bindgen(getter, js_name = signaturePublicKey)]
    pub fn signature_public_key(&self) -> Vec<u8> {
        self.signature_public_key_val.clone()
    }
}

// ============================================================================
// ProcessedMessage (tagged result of processIncoming)
// ============================================================================

/// Discriminator for [`ProcessedMessage`]. wasm-bindgen renders this
/// as a JS number-enum; we expose a string discriminator via
/// [`ProcessedMessage::kind`] for ergonomic `switch` / `case`.
#[wasm_bindgen]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProcessedMessageKind {
    /// Application payload (decrypted plaintext available via
    /// `applicationData`).
    Application,
    /// A commit was processed and merged. Group state advanced.
    Commit,
    /// A standalone proposal was processed (no state advance).
    Proposal,
    /// External-join proposal was processed.
    ExternalJoinProposal,
}

/// Tagged result of [`Group::process_incoming`].
///
/// JS code reads `kind` (a string) to discriminate, then
/// `applicationData` for [`ProcessedMessageKind::Application`].
/// We deliberately don't expose a sum-type on the JS side because
/// wasm-bindgen has no idiomatic story for Rust enums with
/// associated data; a tagged class is what TypeScript would idiom
/// to anyway.
#[wasm_bindgen]
pub struct ProcessedMessage {
    kind: ProcessedMessageKind,
    application_data: Option<Vec<u8>>,
}

#[wasm_bindgen]
impl ProcessedMessage {
    /// Discriminator. `"Application" | "Commit" | "Proposal" |
    /// "ExternalJoinProposal"`. Useful for `switch` / `case` in
    /// TypeScript.
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> String {
        match self.kind {
            ProcessedMessageKind::Application => "Application".into(),
            ProcessedMessageKind::Commit => "Commit".into(),
            ProcessedMessageKind::Proposal => "Proposal".into(),
            ProcessedMessageKind::ExternalJoinProposal => "ExternalJoinProposal".into(),
        }
    }

    /// Application plaintext, present iff `kind == "Application"`.
    #[wasm_bindgen(getter, js_name = applicationData)]
    pub fn application_data(&self) -> Option<Vec<u8>> {
        self.application_data.clone()
    }
}

// ============================================================================
// Group
// ============================================================================

/// Lakko MLS conversation group. Mirror of UniFFI's `Group`.
#[wasm_bindgen]
pub struct Group {
    inner: Mutex<MlsGroup>,
}

#[wasm_bindgen]
impl Group {
    /// Create a fresh group with `identity` as the founding member.
    /// Mirror of UniFFI's `Group::create`.
    #[wasm_bindgen(constructor)]
    pub fn new(identity: &Identity) -> Result<Group, JsError> {
        let inner = MlsGroup::create(&identity.inner).map_err(mls_err)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// Join an existing group from a wire-format Welcome message.
    #[wasm_bindgen(js_name = joinFromWelcome)]
    pub fn join_from_welcome(welcome: &[u8], identity: &Identity) -> Result<Group, JsError> {
        let inner = MlsGroup::join_from_welcome(welcome, &identity.inner).map_err(mls_err)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// Load a previously persisted MLS group from `identity`'s
    /// provider storage (Phase 1.16e6b). Pair with
    /// [`Identity::restoreStorage`](Identity::restore_storage)
    /// — call `restoreStorage` first to seed the provider, then
    /// `Group.load` for each known group_id.
    ///
    /// Returns `undefined` (`Option<Group> -> None`) when
    /// the storage doesn't carry a row for `group_id` —
    /// distinct from a thrown error so the JS side can
    /// distinguish "no group with that id" from "corrupt
    /// storage".
    #[wasm_bindgen(js_name = load)]
    pub fn load(identity: &Identity, group_id: &[u8]) -> Result<Option<Group>, JsError> {
        match MlsGroup::load(&identity.inner, group_id).map_err(mls_err)? {
            Some(inner) => Ok(Some(Self {
                inner: Mutex::new(inner),
            })),
            None => Ok(None),
        }
    }

    /// Add a member from their published KeyPackage (TLS wire form).
    #[wasm_bindgen(js_name = addMember)]
    pub fn add_member(
        &self,
        identity: &Identity,
        key_package: &[u8],
    ) -> Result<AddMember, JsError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        let MemberAddition { commit, welcome } = g
            .add_member_from_wire(&identity.inner, key_package)
            .map_err(mls_err)?;
        Ok(AddMember {
            commit_bytes: group::encode_message(&commit).map_err(mls_err)?,
            welcome_bytes: group::encode_message(&welcome).map_err(mls_err)?,
        })
    }

    /// Remove a member by their leaf index. Returns the wire-encoded
    /// commit to broadcast.
    #[wasm_bindgen(js_name = removeMember)]
    pub fn remove_member(&self, identity: &Identity, leaf_index: u32) -> Result<Vec<u8>, JsError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        let commit = g
            .remove_member(&identity.inner, lakko_mls::LeafNodeIndex::new(leaf_index))
            .map_err(mls_err)?;
        group::encode_message(&commit).map_err(mls_err)
    }

    /// Encrypt an application payload for the current epoch.
    #[wasm_bindgen(js_name = createApplicationMessage)]
    pub fn create_application_message(
        &self,
        identity: &Identity,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, JsError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        let msg = g
            .create_application_message(&identity.inner, plaintext)
            .map_err(mls_err)?;
        group::encode_message(&msg).map_err(mls_err)
    }

    /// Process an incoming MLS message.
    #[wasm_bindgen(js_name = processIncoming)]
    pub fn process_incoming(
        &self,
        identity: &Identity,
        bytes: &[u8],
    ) -> Result<ProcessedMessage, JsError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        let result = g
            .process_incoming(&identity.inner, bytes)
            .map_err(mls_err)?;
        Ok(match result {
            MlsIncoming::Application(data) => ProcessedMessage {
                kind: ProcessedMessageKind::Application,
                application_data: Some(data),
            },
            MlsIncoming::Commit => ProcessedMessage {
                kind: ProcessedMessageKind::Commit,
                application_data: None,
            },
            MlsIncoming::Proposal => ProcessedMessage {
                kind: ProcessedMessageKind::Proposal,
                application_data: None,
            },
            MlsIncoming::ExternalJoinProposal => ProcessedMessage {
                kind: ProcessedMessageKind::ExternalJoinProposal,
                application_data: None,
            },
        })
    }

    /// Derive a `length`-byte secret bound to the current epoch.
    #[wasm_bindgen(js_name = exportSecret)]
    pub fn export_secret(
        &self,
        identity: &Identity,
        label: &str,
        context: &[u8],
        length: u32,
    ) -> Result<Vec<u8>, JsError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        let secret = g
            .export_secret(&identity.inner, label, context, length as usize)
            .map_err(mls_err)?;
        Ok(secret.to_vec())
    }

    /// Local leaf index of the calling identity.
    #[wasm_bindgen(getter, js_name = ownLeafIndex)]
    pub fn own_leaf_index(&self) -> Result<u32, JsError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        Ok(g.own_leaf_index().u32())
    }

    /// Current group epoch.
    #[wasm_bindgen(getter)]
    pub fn epoch(&self) -> Result<u64, JsError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        Ok(g.epoch())
    }

    /// Number of members at the current epoch.
    #[wasm_bindgen(getter, js_name = memberCount)]
    pub fn member_count(&self) -> Result<u32, JsError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        Ok(u32::try_from(g.member_count()).unwrap_or(u32::MAX))
    }

    /// MLS `group_id` (RFC 9420 §6.2). Used by `apps/web` as
    /// the routing key for `POST /ds/group/{group_id_hex}`. The
    /// bytes are opaque — typically 32 random bytes chosen by
    /// OpenMLS at create-time. Identical for every member of
    /// the group at every epoch.
    #[wasm_bindgen(getter, js_name = groupId)]
    pub fn group_id(&self) -> Result<Vec<u8>, JsError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        Ok(g.group_id())
    }

    /// Snapshot of every member's `(leafIndex, signaturePublicKey)`
    /// at the current epoch (Phase 1.18h).
    ///
    /// JS-side shape: returns a flat array of [`GroupMember`]
    /// instances. wasm-bindgen doesn't let us return `Vec<JsValue>`
    /// of a custom struct directly through a getter, so this is
    /// a regular method (`.members()` not `.members`) returning
    /// `Vec<JsValue>` after a per-element `JsValue::from`.
    ///
    /// Ordering matches `lakko_mls::Group::members` — left-to-right
    /// over the leaf tree, stable inside a single epoch.
    #[wasm_bindgen]
    pub fn members(&self) -> Result<Vec<JsValue>, JsError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        Ok(g.members()
            .into_iter()
            .map(|m| {
                JsValue::from(GroupMember {
                    leaf_index_val: m.leaf_index,
                    signature_public_key_val: m.signature_public_key,
                })
            })
            .collect())
    }
}

// ============================================================================
// DeviceGroup (Phase 2a3b1d)
// ============================================================================

/// Per-user multi-device MLS group (ADR-0007). Mirror of
/// UniFFI's `DeviceGroup`.
///
/// FFI subset for 2a3b's add-device flow:
///
///   * `createFounder` — bootstrap on this device.
///   * `joinFromWelcome` — join from a Welcome the scanner
///     produced + delivered via DS mailbox.
///   * `addDeviceFromWire` — add a fresh device by its
///     KeyPackage; returns the (commit, welcome) pair.
///   * `groupId` — the deterministic
///     `BLAKE3("lakko-device-group/v1" || account_id)`.
///
/// 2a3c widens this with `devices` + `removeDevice` once the
/// per-platform UI lands.
#[wasm_bindgen]
pub struct DeviceGroup {
    inner: Mutex<MlsDeviceGroup>,
}

#[wasm_bindgen]
impl DeviceGroup {
    /// Bootstrap a fresh DeviceGroup with `identity` as the
    /// founding leaf. `account_id` MUST be the raw 16-byte
    /// UUIDv7.
    #[wasm_bindgen(js_name = createFounder)]
    pub fn create_founder(identity: &Identity, account_id: &[u8]) -> Result<DeviceGroup, JsError> {
        if account_id.len() != 16 {
            return Err(JsError::new("account_id must be 16 bytes"));
        }
        let mut acct = [0u8; 16];
        acct.copy_from_slice(account_id);
        let inner = MlsDeviceGroup::create_founder(&identity.inner, &acct).map_err(mls_err)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// Join a DeviceGroup from a Welcome the scanner device
    /// produced via [`DeviceGroup::addDeviceFromWire`]. The
    /// deterministic `group_id` check inside
    /// `lakko_mls::DeviceGroup::join_from_welcome` rejects a
    /// misrouted Welcome before any state advance.
    #[wasm_bindgen(js_name = joinFromWelcome)]
    pub fn join_from_welcome(
        welcome: &[u8],
        identity: &Identity,
        account_id: &[u8],
    ) -> Result<DeviceGroup, JsError> {
        if account_id.len() != 16 {
            return Err(JsError::new("account_id must be 16 bytes"));
        }
        let mut acct = [0u8; 16];
        acct.copy_from_slice(account_id);
        let inner =
            MlsDeviceGroup::join_from_welcome(welcome, &identity.inner, &acct).map_err(mls_err)?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// Add a device by its TLS-encoded KeyPackage. Returns
    /// `(commit, welcome)`: broadcast `commit` to existing
    /// members via the DS group journal, deliver `welcome` to
    /// the joining device via DS mailbox at the rendezvous
    /// pseudonym.
    #[wasm_bindgen(js_name = addDeviceFromWire)]
    pub fn add_device_from_wire(
        &self,
        identity: &Identity,
        key_package: &[u8],
    ) -> Result<AddMember, JsError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        let MemberAddition { commit, welcome } = g
            .add_device_from_wire(&identity.inner, key_package)
            .map_err(mls_err)?;
        Ok(AddMember {
            commit_bytes: group::encode_message(&commit).map_err(mls_err)?,
            welcome_bytes: group::encode_message(&welcome).map_err(mls_err)?,
        })
    }

    /// The deterministic `group_id` this DeviceGroup is bound
    /// to. Same value `deviceGroupIdFor(account_id)` returns.
    #[wasm_bindgen(js_name = groupId)]
    pub fn group_id(&self) -> Result<Vec<u8>, JsError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        Ok(g.group_id())
    }

    /// Snapshot of every device leaf currently in the
    /// DeviceGroup (Phase 2a3c). One [`GroupMember`] per row —
    /// the JS layer renders the list, marks the row whose
    /// `leafIndex` matches [`Self::ownLeafIndex`] as
    /// "(this device)", and offers a Remove affordance on every
    /// other row.
    ///
    /// Same wasm-bindgen shape as the conversation
    /// [`Group::members`] — flat `JsValue` array, ordering
    /// matches `lakko_mls::DeviceGroup::devices` (left-to-right
    /// over the leaf tree, stable inside a single epoch).
    #[wasm_bindgen]
    pub fn devices(&self) -> Result<Vec<JsValue>, JsError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        Ok(g.devices()
            .into_iter()
            .map(|m| {
                JsValue::from(GroupMember {
                    leaf_index_val: m.leaf_index,
                    signature_public_key_val: m.signature_public_key,
                })
            })
            .collect())
    }

    /// Leaf index of THIS device inside the DeviceGroup
    /// (Phase 2a3c). UI uses this to flag the "(this device)"
    /// row and disable its Remove button.
    #[wasm_bindgen(js_name = ownLeafIndex)]
    pub fn own_leaf_index(&self) -> Result<u32, JsError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        Ok(g.own_leaf_index())
    }

    /// Revoke a device leaf (Phase 2a3c). Returns the encoded
    /// Commit bytes the caller MUST broadcast to every remaining
    /// device via the DS group journal so they advance past the
    /// revoked leaf and ratchet the group secrets out of the
    /// removed device's reach.
    ///
    /// `leafIndex` must come from a [`Self::devices`] snapshot
    /// at the SAME epoch; concurrent commits can renumber
    /// leaves, in which case the caller refetches and retries.
    /// Attempting to remove the local device's own leaf is
    /// rejected with a wasm-side error.
    #[wasm_bindgen(js_name = removeDeviceAt)]
    pub fn remove_device_at(
        &self,
        identity: &Identity,
        leaf_index: u32,
    ) -> Result<Vec<u8>, JsError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        g.remove_device_at(&identity.inner, leaf_index)
            .map_err(mls_err)
    }

    /// Derive the per-pairing history-bundle exporter secret
    /// (Phase 2a3d8). Returns 32 bytes — the scanner side feeds
    /// it into [`sealHistoryBundle`], the joining side feeds it
    /// into [`openHistoryBundle`]. Both devices reach the same
    /// value after they advance to the post-add-device epoch.
    #[wasm_bindgen(js_name = historyBundleSecret)]
    pub fn history_bundle_secret(&self, identity: &Identity) -> Result<Vec<u8>, JsError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        let secret =
            lakko_mls::history_bundle::exporter_secret(&g, &identity.inner).map_err(mls_err)?;
        Ok(secret.to_vec())
    }

    // ====================================================================
    // 2a3e2 — typed cross-device payload composer / consumer.
    //
    // wasm-bindgen rejects Rust enums with associated data on the
    // public surface, so each typed body variant gets its own
    // composer. The parser returns a flat [`ParsedDeviceGroupPayload`]
    // object whose getters surface a `kind` discriminator + every
    // possible field; the JS layer reads only the fields relevant to
    // the active kind. Mirror of the UniFFI surface (where the
    // FFI-friendly `DeviceGroupBody` Enum sealed-shape is idiomatic).
    // ====================================================================

    /// Compose a `ReadCursor` cross-device payload + return the
    /// encrypted wire bytes ready to PUT to the DeviceGroup DS
    /// journal (Phase 2a3e2). `groupId` / `upToMessageId` are
    /// raw bytes; `readAtMs` / `hlc` are unix-millis / HLC u64s.
    #[wasm_bindgen(js_name = createDeviceGroupReadCursorMessage)]
    pub fn create_devicegroup_read_cursor_message(
        &self,
        identity: &Identity,
        group_id: &[u8],
        up_to_message_id: &[u8],
        read_at_ms: u64,
        hlc: u64,
    ) -> Result<Vec<u8>, JsError> {
        self.create_devicegroup_body(
            identity,
            lakko_mls::device_group::DeviceGroupBody::ReadCursor {
                group_id: group_id.to_vec(),
                up_to_message_id: up_to_message_id.to_vec(),
                read_at_ms,
            },
            hlc,
        )
    }

    /// Compose a `Draft` cross-device payload. Empty `text`
    /// clears the receiver's local draft for the same group.
    #[wasm_bindgen(js_name = createDeviceGroupDraftMessage)]
    pub fn create_devicegroup_draft_message(
        &self,
        identity: &Identity,
        group_id: &[u8],
        text: &str,
        reply_to_id: &[u8],
        hlc: u64,
    ) -> Result<Vec<u8>, JsError> {
        self.create_devicegroup_body(
            identity,
            lakko_mls::device_group::DeviceGroupBody::Draft {
                group_id: group_id.to_vec(),
                text: text.to_string(),
                reply_to_id: reply_to_id.to_vec(),
            },
            hlc,
        )
    }

    /// Compose a `Flags` cross-device payload (pin / mute /
    /// archive / unread-override snapshot — last-write-wins by
    /// HLC).
    #[wasm_bindgen(js_name = createDeviceGroupFlagsMessage)]
    #[allow(clippy::fn_params_excessive_bools)]
    pub fn create_devicegroup_flags_message(
        &self,
        identity: &Identity,
        group_id: &[u8],
        pinned: bool,
        muted: bool,
        archived: bool,
        unread_override: bool,
        hlc: u64,
    ) -> Result<Vec<u8>, JsError> {
        self.create_devicegroup_body(
            identity,
            lakko_mls::device_group::DeviceGroupBody::Flags {
                group_id: group_id.to_vec(),
                pinned,
                muted,
                archived,
                unread_override,
            },
            hlc,
        )
    }

    /// Compose an `Order` cross-device payload — explicit
    /// display ordering for the conversation list. `groupIds`
    /// is the flat concatenation of all group_ids; the lengths
    /// are encoded inline via the leading-length prefix
    /// convention `len(u32 LE) || bytes`. Callers should
    /// prefer the `Vec<Vec<u8>>` shape; wasm-bindgen flattens
    /// it via a JS `Uint8Array[]`.
    #[wasm_bindgen(js_name = createDeviceGroupOrderMessage)]
    pub fn create_devicegroup_order_message(
        &self,
        identity: &Identity,
        group_ids: Vec<js_sys::Uint8Array>,
        hlc: u64,
    ) -> Result<Vec<u8>, JsError> {
        let owned: Vec<Vec<u8>> = group_ids.into_iter().map(|a| a.to_vec()).collect();
        self.create_devicegroup_body(
            identity,
            lakko_mls::device_group::DeviceGroupBody::Order { group_ids: owned },
            hlc,
        )
    }

    /// Decrypt / advance the state machine for an inbound
    /// DeviceGroup wire message (Phase 2a3e2). JS layer reads
    /// `kind`:
    ///
    ///   * `"application"` — pass `data` into
    ///     [`parseDeviceGroupPayload`] to project into a typed
    ///     payload.
    ///   * `"commit"` — DeviceGroup advanced. Caller refreshes
    ///     `devices()`.
    ///   * `"proposal"` / `"externalJoinProposal"` — defensive;
    ///     no state advance.
    #[wasm_bindgen(js_name = processIncoming)]
    pub fn dg_process_incoming(
        &self,
        identity: &Identity,
        message_bytes: &[u8],
    ) -> Result<ProcessedMessage, JsError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        let incoming = g
            .process_incoming(&identity.inner, message_bytes)
            .map_err(mls_err)?;
        Ok(match incoming {
            lakko_mls::Incoming::Application(data) => ProcessedMessage {
                kind: ProcessedMessageKind::Application,
                application_data: Some(data),
            },
            lakko_mls::Incoming::Commit => ProcessedMessage {
                kind: ProcessedMessageKind::Commit,
                application_data: None,
            },
            lakko_mls::Incoming::Proposal => ProcessedMessage {
                kind: ProcessedMessageKind::Proposal,
                application_data: None,
            },
            lakko_mls::Incoming::ExternalJoinProposal => ProcessedMessage {
                kind: ProcessedMessageKind::ExternalJoinProposal,
                application_data: None,
            },
        })
    }

    /// Shared compose plumbing — wire-encodes via OpenMLS +
    /// returns the bytes.
    fn create_devicegroup_body(
        &self,
        identity: &Identity,
        body: lakko_mls::device_group::DeviceGroupBody,
        hlc: u64,
    ) -> Result<Vec<u8>, JsError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| JsError::new("internal lock poisoned"))?;
        g.create_devicegroup_message(&identity.inner, body, hlc)
            .map_err(mls_err)
    }
}

/// Discriminator string for [`ParsedDeviceGroupPayload::kind`].
/// JS-side `switch` matches on this to read the relevant
/// optional fields. Pinned ASCII; any change is a breaking
/// surface change.
const KIND_READ_CURSOR: &str = "readCursor";
const KIND_DRAFT: &str = "draft";
const KIND_FLAGS: &str = "flags";
const KIND_ORDER: &str = "order";

/// JS-side projection of
/// [`lakko_mls::device_group::ParsedDeviceGroupPayload`]
/// (Phase 2a3e2). One flat class with a `kind` discriminator +
/// every possible field as a typed getter — keeps the
/// wasm-bindgen surface simple at the cost of some
/// always-present fields on every variant.
///
/// Each field carries a trailing `_val` to disambiguate from
/// the same-named wasm-bindgen getter; this is mechanically
/// required by wasm-bindgen and outside our control.
#[wasm_bindgen]
#[allow(
    clippy::similar_names,
    clippy::struct_excessive_bools,
    clippy::struct_field_names
)]
pub struct ParsedDeviceGroupPayload {
    hlc_val: u64,
    sender_leaf_val: u32,
    kind_val: &'static str,
    group_id_val: Vec<u8>,
    up_to_message_id_val: Vec<u8>,
    read_at_ms_val: u64,
    text_val: String,
    reply_to_id_val: Vec<u8>,
    pinned_val: bool,
    muted_val: bool,
    archived_val: bool,
    unread_override_val: bool,
    order_group_ids_val: Vec<Vec<u8>>,
}

#[wasm_bindgen]
impl ParsedDeviceGroupPayload {
    /// HLC timestamp from the envelope.
    #[wasm_bindgen(getter)]
    pub fn hlc(&self) -> u64 {
        self.hlc_val
    }

    /// Leaf index of the device that authored this payload.
    #[wasm_bindgen(getter, js_name = senderLeaf)]
    pub fn sender_leaf(&self) -> u32 {
        self.sender_leaf_val
    }

    /// One of `"readCursor"` / `"draft"` / `"flags"` /
    /// `"order"`. JS reads this to decide which other fields
    /// are meaningful.
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> String {
        self.kind_val.to_string()
    }

    /// Conversation group_id. Set for `readCursor`, `draft`,
    /// and `flags`; empty for `order`.
    #[wasm_bindgen(getter, js_name = groupId)]
    pub fn group_id(&self) -> Vec<u8> {
        self.group_id_val.clone()
    }

    /// `readCursor` only: the message-id the sender claims to
    /// have read up to.
    #[wasm_bindgen(getter, js_name = upToMessageId)]
    pub fn up_to_message_id(&self) -> Vec<u8> {
        self.up_to_message_id_val.clone()
    }

    /// `readCursor` only: wall-clock UNIX millis when the read
    /// happened (display only).
    #[wasm_bindgen(getter, js_name = readAtMs)]
    pub fn read_at_ms(&self) -> u64 {
        self.read_at_ms_val
    }

    /// `draft` only: composer text. May be empty (clear).
    #[wasm_bindgen(getter)]
    pub fn text(&self) -> String {
        self.text_val.clone()
    }

    /// `draft` only: the message-id the draft is replying to,
    /// or empty.
    #[wasm_bindgen(getter, js_name = replyToId)]
    pub fn reply_to_id(&self) -> Vec<u8> {
        self.reply_to_id_val.clone()
    }

    /// `flags` only: pinned indicator.
    #[wasm_bindgen(getter)]
    pub fn pinned(&self) -> bool {
        self.pinned_val
    }

    /// `flags` only: muted indicator.
    #[wasm_bindgen(getter)]
    pub fn muted(&self) -> bool {
        self.muted_val
    }

    /// `flags` only: archived indicator.
    #[wasm_bindgen(getter)]
    pub fn archived(&self) -> bool {
        self.archived_val
    }

    /// `flags` only: manual unread-override indicator.
    #[wasm_bindgen(getter, js_name = unreadOverride)]
    pub fn unread_override(&self) -> bool {
        self.unread_override_val
    }

    /// `order` only: conversation group_ids in display order,
    /// top-to-bottom. Empty for non-`order` kinds.
    #[wasm_bindgen(js_name = orderGroupIds)]
    pub fn order_group_ids(&self) -> Vec<js_sys::Uint8Array> {
        self.order_group_ids_val
            .iter()
            .map(|g| js_sys::Uint8Array::from(g.as_slice()))
            .collect()
    }
}

/// Parse a DeviceGroup application-payload plaintext (the
/// `data` field of `ProcessedMessage::Application`) into a
/// typed [`ParsedDeviceGroupPayload`] (Phase 2a3e2).
#[wasm_bindgen(js_name = parseDeviceGroupPayload)]
pub fn parse_devicegroup_payload(plaintext: &[u8]) -> Result<ParsedDeviceGroupPayload, JsError> {
    use lakko_mls::device_group::DeviceGroupBody as Body;
    let parsed = lakko_mls::DeviceGroup::parse_devicegroup_payload(plaintext).map_err(mls_err)?;
    let mut out = ParsedDeviceGroupPayload {
        hlc_val: parsed.hlc,
        sender_leaf_val: parsed.sender_leaf,
        kind_val: KIND_READ_CURSOR,
        group_id_val: Vec::new(),
        up_to_message_id_val: Vec::new(),
        read_at_ms_val: 0,
        text_val: String::new(),
        reply_to_id_val: Vec::new(),
        pinned_val: false,
        muted_val: false,
        archived_val: false,
        unread_override_val: false,
        order_group_ids_val: Vec::new(),
    };
    match parsed.body {
        Body::ReadCursor {
            group_id,
            up_to_message_id,
            read_at_ms,
        } => {
            out.kind_val = KIND_READ_CURSOR;
            out.group_id_val = group_id;
            out.up_to_message_id_val = up_to_message_id;
            out.read_at_ms_val = read_at_ms;
        },
        Body::Draft {
            group_id,
            text,
            reply_to_id,
        } => {
            out.kind_val = KIND_DRAFT;
            out.group_id_val = group_id;
            out.text_val = text;
            out.reply_to_id_val = reply_to_id;
        },
        Body::Flags {
            group_id,
            pinned,
            muted,
            archived,
            unread_override,
        } => {
            out.kind_val = KIND_FLAGS;
            out.group_id_val = group_id;
            out.pinned_val = pinned;
            out.muted_val = muted;
            out.archived_val = archived;
            out.unread_override_val = unread_override;
        },
        Body::Order { group_ids } => {
            out.kind_val = KIND_ORDER;
            out.order_group_ids_val = group_ids;
        },
    }
    Ok(out)
}

/// AEAD-seal a [`lakko_proto::mls_app::history_bundle::v1::HistoryBundle`]
/// (Phase 2a3d8).
///
/// `secret` MUST be 32 bytes. `bundleBytes` is the prost-encoded
/// `HistoryBundle` proto bytes the caller built from its local
/// message store. Returns the wire-format `SealedBundle` proto
/// bytes ready to PUT to the rendezvous mailbox.
///
/// Refuses to seal bundles whose total message count exceeds
/// `lakko_mls::history_bundle::MAX_BUNDLE_MESSAGES` (1000).
#[wasm_bindgen(js_name = sealHistoryBundle)]
pub fn seal_history_bundle(secret: &[u8], bundle_bytes: &[u8]) -> Result<Vec<u8>, JsError> {
    use prost::Message as _;
    if secret.len() != 32 {
        return Err(JsError::new("history-bundle secret must be 32 bytes"));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(secret);

    let bundle = lakko_proto::mls_app::history_bundle::v1::HistoryBundle::decode(bundle_bytes)
        .map_err(|e| JsError::new(&format!("HistoryBundle decode: {e}")))?;
    lakko_mls::history_bundle::seal_bundle(&key, &bundle).map_err(mls_err)
}

/// Inverse of [`sealHistoryBundle`]. Returns the prost-encoded
/// `HistoryBundle` bytes; the caller decodes via its own proto
/// runtime (`protobuf-ts` on TS).
#[wasm_bindgen(js_name = openHistoryBundle)]
pub fn open_history_bundle(secret: &[u8], sealed_bytes: &[u8]) -> Result<Vec<u8>, JsError> {
    use prost::Message as _;
    if secret.len() != 32 {
        return Err(JsError::new("history-bundle secret must be 32 bytes"));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(secret);

    let bundle = lakko_mls::history_bundle::open_bundle(&key, sealed_bytes).map_err(mls_err)?;
    let mut out = Vec::with_capacity(bundle.encoded_len());
    bundle
        .encode(&mut out)
        .map_err(|e| JsError::new(&format!("HistoryBundle re-encode: {e}")))?;
    Ok(out)
}

// ============================================================================
// Phase 2a3d8-ui — structured history-bundle API for the web
//
// wasm-bindgen rejects Rust enums-with-data + nested Vecs of Records in
// the public surface, so the structured API splits into:
//
//   - `HistoryMessageInput` / `HistoryGroupInput` — wasm-bindgen
//     classes the JS caller constructs and passes into the seal path.
//   - `OpenedHistoryGroup` / `OpenedHistoryMessage` — flat
//     wasm-bindgen classes the open path returns; the JS caller
//     iterates `.groups()` etc.
//
// Wire format is identical to the byte path — both surfaces produce /
// consume the same `SealedBundle` proto.
// ============================================================================

/// One message the scanner-side caller adds to a
/// [`HistoryGroupInput`] before sealing the bundle.
/// wasm-bindgen class — JS-side `new HistoryMessageInput(...)`.
#[wasm_bindgen]
#[allow(clippy::struct_field_names)]
pub struct HistoryMessageInput {
    payload_bytes_val: Vec<u8>,
    sender_leaf_val: u32,
    message_id_val: Vec<u8>,
    received_at_ms_val: u64,
}

#[wasm_bindgen]
impl HistoryMessageInput {
    /// JS constructor — one call per included message.
    #[wasm_bindgen(constructor)]
    pub fn new(
        payload_bytes: &[u8],
        sender_leaf: u32,
        message_id: &[u8],
        received_at_ms: u64,
    ) -> HistoryMessageInput {
        HistoryMessageInput {
            payload_bytes_val: payload_bytes.to_vec(),
            sender_leaf_val: sender_leaf,
            message_id_val: message_id.to_vec(),
            received_at_ms_val: received_at_ms,
        }
    }
}

/// One group's worth of history rows for the seal path.
/// wasm-bindgen class — JS-side `new HistoryGroupInput(...)`.
#[wasm_bindgen]
#[allow(clippy::struct_field_names)]
pub struct HistoryGroupInput {
    group_id_val: Vec<u8>,
    messages_val: Vec<HistoryMessageInput>,
    count_capped_val: bool,
}

#[wasm_bindgen]
impl HistoryGroupInput {
    /// JS constructor. `messages` is a flat `Array<HistoryMessageInput>`;
    /// the binding consumes ownership.
    #[wasm_bindgen(constructor)]
    pub fn new(
        group_id: &[u8],
        messages: Vec<HistoryMessageInput>,
        count_capped: bool,
    ) -> HistoryGroupInput {
        HistoryGroupInput {
            group_id_val: group_id.to_vec(),
            messages_val: messages,
            count_capped_val: count_capped,
        }
    }
}

/// AEAD-seal a structured history bundle (Phase 2a3d8-ui).
///
/// `secret` MUST be 32 bytes (typically the output of
/// [`DeviceGroup::historyBundleSecret`]). `groups` is an
/// `Array<HistoryGroupInput>`.
#[wasm_bindgen(js_name = sealHistoryBundleStructured)]
pub fn seal_history_bundle_structured(
    secret: &[u8],
    groups: Vec<HistoryGroupInput>,
    max_hlc: u64,
) -> Result<Vec<u8>, JsError> {
    if secret.len() != 32 {
        return Err(JsError::new("history-bundle secret must be 32 bytes"));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(secret);

    let bundle = build_history_bundle_proto(groups, max_hlc);
    lakko_mls::history_bundle::seal_bundle(&key, &bundle).map_err(mls_err)
}

fn build_history_bundle_proto(
    groups: Vec<HistoryGroupInput>,
    max_hlc: u64,
) -> lakko_proto::mls_app::history_bundle::v1::HistoryBundle {
    use lakko_proto::mls_app::history_bundle::v1::{HistoryBundle, HistoryGroup, HistoryMessage};
    HistoryBundle {
        max_hlc,
        groups: groups
            .into_iter()
            .map(|g| HistoryGroup {
                group_id: g.group_id_val,
                count_capped: g.count_capped_val,
                messages: g
                    .messages_val
                    .into_iter()
                    .map(|m| HistoryMessage {
                        payload_bytes: m.payload_bytes_val,
                        sender_leaf: m.sender_leaf_val,
                        message_id: m.message_id_val,
                        received_at_ms: m.received_at_ms_val,
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// One message after the open path — flat getters for the JS
/// caller. wasm-bindgen doesn't expose tuple-style returns, so
/// each opened message is a class instance.
#[wasm_bindgen]
#[allow(clippy::struct_field_names)]
pub struct OpenedHistoryMessage {
    payload_bytes_val: Vec<u8>,
    sender_leaf_val: u32,
    message_id_val: Vec<u8>,
    received_at_ms_val: u64,
}

#[allow(missing_docs)]
#[wasm_bindgen]
impl OpenedHistoryMessage {
    #[wasm_bindgen(getter, js_name = payloadBytes)]
    pub fn payload_bytes(&self) -> Vec<u8> {
        self.payload_bytes_val.clone()
    }

    #[wasm_bindgen(getter, js_name = senderLeaf)]
    pub fn sender_leaf(&self) -> u32 {
        self.sender_leaf_val
    }

    #[wasm_bindgen(getter, js_name = messageId)]
    pub fn message_id(&self) -> Vec<u8> {
        self.message_id_val.clone()
    }

    #[wasm_bindgen(getter, js_name = receivedAtMs)]
    pub fn received_at_ms(&self) -> u64 {
        self.received_at_ms_val
    }
}

/// One group's worth of opened history rows for the receive
/// path. wasm-bindgen class — JS reads `.groupId`, `.messages()`,
/// `.countCapped`.
#[wasm_bindgen]
#[allow(clippy::struct_field_names)]
pub struct OpenedHistoryGroup {
    group_id_val: Vec<u8>,
    messages_val: Vec<OpenedHistoryMessage>,
    count_capped_val: bool,
}

#[wasm_bindgen]
impl OpenedHistoryGroup {
    /// MLS group_id this group's history rows belong to.
    #[wasm_bindgen(getter, js_name = groupId)]
    pub fn group_id(&self) -> Vec<u8> {
        self.group_id_val.clone()
    }

    /// Flat array of [`OpenedHistoryMessage`] — JS iterates via
    /// `Array.from(g.messages())`. wasm-bindgen owns the
    /// inner allocations; the caller must not retain references
    /// past the parent `OpenedHistoryGroup`'s lifetime.
    #[wasm_bindgen]
    pub fn messages(&self) -> Result<Vec<JsValue>, JsError> {
        Ok(self
            .messages_val
            .iter()
            .map(|m| {
                JsValue::from(OpenedHistoryMessage {
                    payload_bytes_val: m.payload_bytes_val.clone(),
                    sender_leaf_val: m.sender_leaf_val,
                    message_id_val: m.message_id_val.clone(),
                    received_at_ms_val: m.received_at_ms_val,
                })
            })
            .collect())
    }

    /// `true` when the per-group message list was truncated
    /// to the producer-side cap. UI surfaces a "load older"
    /// affordance later.
    #[wasm_bindgen(getter, js_name = countCapped)]
    pub fn count_capped(&self) -> bool {
        self.count_capped_val
    }
}

/// Top-level result of [`openHistoryBundleStructured`].
/// wasm-bindgen class — JS reads `.groups()` and `.maxHlc`.
#[wasm_bindgen]
#[allow(clippy::struct_field_names)]
pub struct OpenedHistoryBundle {
    groups_val: Vec<OpenedHistoryGroup>,
    max_hlc_val: u64,
}

#[wasm_bindgen]
impl OpenedHistoryBundle {
    /// Flat array of [`OpenedHistoryGroup`]. Same JS-iter
    /// pattern as `OpenedHistoryGroup.messages()`.
    #[wasm_bindgen]
    pub fn groups(&self) -> Result<Vec<JsValue>, JsError> {
        Ok(self
            .groups_val
            .iter()
            .map(|g| {
                JsValue::from(OpenedHistoryGroup {
                    group_id_val: g.group_id_val.clone(),
                    messages_val: g
                        .messages_val
                        .iter()
                        .map(|m| OpenedHistoryMessage {
                            payload_bytes_val: m.payload_bytes_val.clone(),
                            sender_leaf_val: m.sender_leaf_val,
                            message_id_val: m.message_id_val.clone(),
                            received_at_ms_val: m.received_at_ms_val,
                        })
                        .collect(),
                    count_capped_val: g.count_capped_val,
                })
            })
            .collect())
    }

    /// HLC of the most recent message included in the bundle.
    #[wasm_bindgen(getter, js_name = maxHlc)]
    pub fn max_hlc(&self) -> u64 {
        self.max_hlc_val
    }
}

/// Inverse of [`sealHistoryBundleStructured`]. Returns a flat
/// JS object the caller iterates through getters; no proto
/// runtime needed.
#[wasm_bindgen(js_name = openHistoryBundleStructured)]
pub fn open_history_bundle_structured(
    secret: &[u8],
    sealed_bytes: &[u8],
) -> Result<OpenedHistoryBundle, JsError> {
    if secret.len() != 32 {
        return Err(JsError::new("history-bundle secret must be 32 bytes"));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(secret);

    let bundle = lakko_mls::history_bundle::open_bundle(&key, sealed_bytes).map_err(mls_err)?;
    Ok(OpenedHistoryBundle {
        max_hlc_val: bundle.max_hlc,
        groups_val: bundle
            .groups
            .into_iter()
            .map(|g| OpenedHistoryGroup {
                group_id_val: g.group_id,
                count_capped_val: g.count_capped,
                messages_val: g
                    .messages
                    .into_iter()
                    .map(|m| OpenedHistoryMessage {
                        payload_bytes_val: m.payload_bytes,
                        sender_leaf_val: m.sender_leaf,
                        message_id_val: m.message_id,
                        received_at_ms_val: m.received_at_ms,
                    })
                    .collect(),
            })
            .collect(),
    })
}

/// Compute the deterministic DeviceGroup `group_id` from an
/// account_id, without constructing a `DeviceGroup` handle.
/// Useful on the joining-device side when polling the DS
/// journal for catch-up Commits before any Welcome has been
/// processed.
#[wasm_bindgen(js_name = deviceGroupIdFor)]
pub fn device_group_id_for(account_id: &[u8]) -> Result<Vec<u8>, JsError> {
    if account_id.len() != 16 {
        return Err(JsError::new("account_id must be 16 bytes"));
    }
    let mut acct = [0u8; 16];
    acct.copy_from_slice(account_id);
    Ok(MlsDeviceGroup::group_id_for(&acct))
}

/// Load an existing DeviceGroup for `account_id` from
/// `identity`'s MLS provider storage. Pair with
/// `Identity.restoreStorage` on app startup: restore the
/// snapshot first, then call this to get the live handle.
///
/// Returns `undefined` when the provider has no row under the
/// deterministic group_id — typical case: a freshly-derived
/// Identity that hasn't joined the DeviceGroup yet.
///
/// Free function instead of a `DeviceGroup::load` constructor
/// because wasm-bindgen's constructor surface doesn't tolerate
/// `Result<Option<Self>, _>` returns cleanly — the same shape
/// `Group::load` uses works fine as a free function.
#[wasm_bindgen(js_name = loadDeviceGroup)]
pub fn load_device_group(
    identity: &Identity,
    account_id: &[u8],
) -> Result<Option<DeviceGroup>, JsError> {
    if account_id.len() != 16 {
        return Err(JsError::new("account_id must be 16 bytes"));
    }
    let mut acct = [0u8; 16];
    acct.copy_from_slice(account_id);
    match MlsDeviceGroup::load(&identity.inner, &acct).map_err(mls_err)? {
        Some(inner) => Ok(Some(DeviceGroup {
            inner: Mutex::new(inner),
        })),
        None => Ok(None),
    }
}

// ============================================================================
// Tests
// ============================================================================
//
// ============================================================================
// Phase 3a4 — Call signaling + SFrame keying FFI for the web
// ============================================================================

/// One composed call-signaling event — flat class with a
/// `kind` discriminator + every possible field, mirroring
/// the `ParsedDeviceGroupPayload` shape from 2a3e2a.
#[wasm_bindgen]
#[allow(
    clippy::similar_names,
    clippy::struct_excessive_bools,
    clippy::struct_field_names
)]
pub struct ParsedCallSignaling {
    kind_val: &'static str,
    call_id_val: Vec<u8>,
    sdp_val: String,
    created_at_ms_val: u64,
    media_audio_val: bool,
    media_video_val: bool,
    candidate_val: String,
    sdp_mid_val: String,
    sdp_m_line_index_val: u32,
    end_reason_val: &'static str,
    // Phase 3c2 — group-call variants.
    leaf_index_val: u32,
    initiator_leaf_val: u32,
    joined_at_ms_val: u64,
}

const CALL_KIND_OFFER: &str = "offer";
const CALL_KIND_ANSWER: &str = "answer";
const CALL_KIND_ICE_CANDIDATE: &str = "iceCandidate";
const CALL_KIND_END: &str = "end";
const CALL_KIND_GROUP_START: &str = "groupStart";
const CALL_KIND_GROUP_JOIN: &str = "groupJoin";
const CALL_KIND_GROUP_LEAVE: &str = "groupLeave";

#[wasm_bindgen]
impl ParsedCallSignaling {
    /// One of `"offer"` / `"answer"` / `"iceCandidate"` /
    /// `"end"`. JS reads this to decide which other fields
    /// are meaningful.
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> String {
        self.kind_val.to_string()
    }

    /// 16-byte call_id (every kind carries one).
    #[wasm_bindgen(getter, js_name = callId)]
    pub fn call_id(&self) -> Vec<u8> {
        self.call_id_val.clone()
    }

    /// `offer` / `answer` only: SDP body.
    #[wasm_bindgen(getter)]
    pub fn sdp(&self) -> String {
        self.sdp_val.clone()
    }

    /// `offer` only: wall-clock UNIX millis.
    #[wasm_bindgen(getter, js_name = createdAtMs)]
    pub fn created_at_ms(&self) -> u64 {
        self.created_at_ms_val
    }

    /// `offer` only: audio media flag.
    #[wasm_bindgen(getter, js_name = mediaAudio)]
    pub fn media_audio(&self) -> bool {
        self.media_audio_val
    }

    /// `offer` only: video media flag.
    #[wasm_bindgen(getter, js_name = mediaVideo)]
    pub fn media_video(&self) -> bool {
        self.media_video_val
    }

    /// `iceCandidate` only: ICE candidate SDP fragment.
    #[wasm_bindgen(getter)]
    pub fn candidate(&self) -> String {
        self.candidate_val.clone()
    }

    /// `iceCandidate` only: SDP m-section identifier.
    #[wasm_bindgen(getter, js_name = sdpMid)]
    pub fn sdp_mid(&self) -> String {
        self.sdp_mid_val.clone()
    }

    /// `iceCandidate` only: zero-based SDP m-section index.
    #[wasm_bindgen(getter, js_name = sdpMLineIndex)]
    pub fn sdp_m_line_index(&self) -> u32 {
        self.sdp_m_line_index_val
    }

    /// `end` / `groupLeave` only: one of `"hungUp"` /
    /// `"busy"` / `"declined"` / `"timedOut"` / `"error"`.
    /// For `groupLeave` only `"hungUp"` / `"timedOut"` /
    /// `"error"` are meaningful.
    #[wasm_bindgen(getter, js_name = endReason)]
    pub fn end_reason(&self) -> String {
        self.end_reason_val.to_string()
    }

    /// `groupStart` only: MLS leaf index of the initiator.
    #[wasm_bindgen(getter, js_name = initiatorLeaf)]
    pub fn initiator_leaf(&self) -> u32 {
        self.initiator_leaf_val
    }

    /// `groupJoin` / `groupLeave` only: MLS leaf index of
    /// the joining / leaving participant.
    #[wasm_bindgen(getter, js_name = leafIndex)]
    pub fn leaf_index(&self) -> u32 {
        self.leaf_index_val
    }

    /// `groupJoin` only: wall-clock UNIX millis when the
    /// join landed on the chat journal.
    #[wasm_bindgen(getter, js_name = joinedAtMs)]
    pub fn joined_at_ms(&self) -> u64 {
        self.joined_at_ms_val
    }
}

/// Compose an Offer call-signaling event into plaintext
/// bytes (Phase 3a3).
#[wasm_bindgen(js_name = composeCallOfferSignaling)]
pub fn compose_call_offer_signaling(
    call_id: &[u8],
    sdp: &str,
    created_at_ms: u64,
    media_audio: bool,
    media_video: bool,
) -> Result<Vec<u8>, JsError> {
    let id = take_call_id(call_id)?;
    let body = lakko_calls::SignalingBody::Offer {
        call_id: id,
        sdp: sdp.to_string(),
        created_at_ms,
        media: lakko_calls::MediaFlags {
            audio: media_audio,
            video: media_video,
        },
    };
    lakko_calls::compose_signaling_payload(&body).map_err(calls_err)
}

/// Compose an Answer call-signaling event.
#[wasm_bindgen(js_name = composeCallAnswerSignaling)]
pub fn compose_call_answer_signaling(call_id: &[u8], sdp: &str) -> Result<Vec<u8>, JsError> {
    let id = take_call_id(call_id)?;
    let body = lakko_calls::SignalingBody::Answer {
        call_id: id,
        sdp: sdp.to_string(),
    };
    lakko_calls::compose_signaling_payload(&body).map_err(calls_err)
}

/// Compose an IceCandidate call-signaling event.
#[wasm_bindgen(js_name = composeCallIceCandidateSignaling)]
pub fn compose_call_ice_candidate_signaling(
    call_id: &[u8],
    candidate: &str,
    sdp_mid: &str,
    sdp_m_line_index: u32,
) -> Result<Vec<u8>, JsError> {
    let id = take_call_id(call_id)?;
    let body = lakko_calls::SignalingBody::IceCandidate {
        call_id: id,
        candidate: candidate.to_string(),
        sdp_mid: sdp_mid.to_string(),
        sdp_m_line_index,
    };
    lakko_calls::compose_signaling_payload(&body).map_err(calls_err)
}

/// Compose an End call-signaling event. `reason` is one of
/// `"hungUp"` / `"busy"` / `"declined"` / `"timedOut"` /
/// `"error"`; unknown values fold to `"hungUp"`.
#[wasm_bindgen(js_name = composeCallEndSignaling)]
pub fn compose_call_end_signaling(call_id: &[u8], reason: &str) -> Result<Vec<u8>, JsError> {
    let id = take_call_id(call_id)?;
    let body = lakko_calls::SignalingBody::End {
        call_id: id,
        reason: parse_end_reason(reason),
    };
    lakko_calls::compose_signaling_payload(&body).map_err(calls_err)
}

/// Compose a `GroupCallStart` call-signaling event
/// (Phase 3c2). Posted by the initiator to the chat MLS
/// group; observers render a joinable-call card.
#[wasm_bindgen(js_name = composeGroupCallStartSignaling)]
pub fn compose_group_call_start_signaling(
    call_id: &[u8],
    initiator_leaf: u32,
    created_at_ms: u64,
    media_audio: bool,
    media_video: bool,
) -> Result<Vec<u8>, JsError> {
    let id = take_call_id(call_id)?;
    let body = lakko_calls::SignalingBody::GroupStart {
        call_id: id,
        initiator_leaf,
        created_at_ms,
        media: lakko_calls::MediaFlags {
            audio: media_audio,
            video: media_video,
        },
    };
    lakko_calls::compose_signaling_payload(&body).map_err(calls_err)
}

/// Compose a `GroupCallJoin` event (Phase 3c2). Sent by
/// each participant after their per-participant SFU
/// session reaches `connected`.
#[wasm_bindgen(js_name = composeGroupCallJoinSignaling)]
pub fn compose_group_call_join_signaling(
    call_id: &[u8],
    leaf_index: u32,
    joined_at_ms: u64,
) -> Result<Vec<u8>, JsError> {
    let id = take_call_id(call_id)?;
    let body = lakko_calls::SignalingBody::GroupJoin {
        call_id: id,
        leaf_index,
        joined_at_ms,
    };
    lakko_calls::compose_signaling_payload(&body).map_err(calls_err)
}

/// Compose a `GroupCallLeave` event (Phase 3c2). `reason`
/// accepts the same values as `composeCallEndSignaling`;
/// only `"hungUp"` / `"timedOut"` / `"error"` are
/// meaningful for group-call leave.
#[wasm_bindgen(js_name = composeGroupCallLeaveSignaling)]
pub fn compose_group_call_leave_signaling(
    call_id: &[u8],
    leaf_index: u32,
    reason: &str,
) -> Result<Vec<u8>, JsError> {
    let id = take_call_id(call_id)?;
    let body = lakko_calls::SignalingBody::GroupLeave {
        call_id: id,
        leaf_index,
        reason: parse_end_reason(reason),
    };
    lakko_calls::compose_signaling_payload(&body).map_err(calls_err)
}

/// Parse plaintext bytes (the `Application` payload from
/// `processIncoming`) into a typed
/// [`ParsedCallSignaling`] (Phase 3a3).
#[wasm_bindgen(js_name = parseCallSignaling)]
pub fn parse_call_signaling(plaintext: &[u8]) -> Result<ParsedCallSignaling, JsError> {
    let body = lakko_calls::parse_signaling_payload(plaintext).map_err(calls_err)?;
    let mut out = ParsedCallSignaling {
        kind_val: CALL_KIND_OFFER,
        call_id_val: Vec::new(),
        sdp_val: String::new(),
        created_at_ms_val: 0,
        media_audio_val: false,
        media_video_val: false,
        candidate_val: String::new(),
        sdp_mid_val: String::new(),
        sdp_m_line_index_val: 0,
        end_reason_val: "hungUp",
        leaf_index_val: 0,
        initiator_leaf_val: 0,
        joined_at_ms_val: 0,
    };
    match body {
        lakko_calls::SignalingBody::Offer {
            call_id,
            sdp,
            created_at_ms,
            media,
        } => {
            out.kind_val = CALL_KIND_OFFER;
            out.call_id_val = call_id.to_vec();
            out.sdp_val = sdp;
            out.created_at_ms_val = created_at_ms;
            out.media_audio_val = media.audio;
            out.media_video_val = media.video;
        },
        lakko_calls::SignalingBody::Answer { call_id, sdp } => {
            out.kind_val = CALL_KIND_ANSWER;
            out.call_id_val = call_id.to_vec();
            out.sdp_val = sdp;
        },
        lakko_calls::SignalingBody::IceCandidate {
            call_id,
            candidate,
            sdp_mid,
            sdp_m_line_index,
        } => {
            out.kind_val = CALL_KIND_ICE_CANDIDATE;
            out.call_id_val = call_id.to_vec();
            out.candidate_val = candidate;
            out.sdp_mid_val = sdp_mid;
            out.sdp_m_line_index_val = sdp_m_line_index;
        },
        lakko_calls::SignalingBody::End { call_id, reason } => {
            out.kind_val = CALL_KIND_END;
            out.call_id_val = call_id.to_vec();
            out.end_reason_val = end_reason_str(reason);
        },
        lakko_calls::SignalingBody::GroupStart {
            call_id,
            initiator_leaf,
            created_at_ms,
            media,
        } => {
            out.kind_val = CALL_KIND_GROUP_START;
            out.call_id_val = call_id.to_vec();
            out.initiator_leaf_val = initiator_leaf;
            out.created_at_ms_val = created_at_ms;
            out.media_audio_val = media.audio;
            out.media_video_val = media.video;
        },
        lakko_calls::SignalingBody::GroupJoin {
            call_id,
            leaf_index,
            joined_at_ms,
        } => {
            out.kind_val = CALL_KIND_GROUP_JOIN;
            out.call_id_val = call_id.to_vec();
            out.leaf_index_val = leaf_index;
            out.joined_at_ms_val = joined_at_ms;
        },
        lakko_calls::SignalingBody::GroupLeave {
            call_id,
            leaf_index,
            reason,
        } => {
            out.kind_val = CALL_KIND_GROUP_LEAVE;
            out.call_id_val = call_id.to_vec();
            out.leaf_index_val = leaf_index;
            out.end_reason_val = end_reason_str(reason);
        },
    }
    Ok(out)
}

fn take_call_id(bytes: &[u8]) -> Result<lakko_calls::CallId, JsError> {
    if bytes.len() != 16 {
        return Err(JsError::new(&format!(
            "call_id length: expected 16, got {}",
            bytes.len()
        )));
    }
    let mut id = [0u8; 16];
    id.copy_from_slice(bytes);
    Ok(id)
}

fn parse_end_reason(s: &str) -> lakko_calls::EndReason {
    match s {
        "busy" => lakko_calls::EndReason::Busy,
        "declined" => lakko_calls::EndReason::Declined,
        "timedOut" => lakko_calls::EndReason::TimedOut,
        "error" => lakko_calls::EndReason::Error,
        _ => lakko_calls::EndReason::HungUp,
    }
}

fn end_reason_str(r: lakko_calls::EndReason) -> &'static str {
    match r {
        lakko_calls::EndReason::HungUp => "hungUp",
        lakko_calls::EndReason::Busy => "busy",
        lakko_calls::EndReason::Declined => "declined",
        lakko_calls::EndReason::TimedOut => "timedOut",
        lakko_calls::EndReason::Error => "error",
    }
}

fn calls_err(err: lakko_calls::Error) -> JsError {
    JsError::new(&format!("{err}"))
}

/// Per-participant SFrame key + nonce salt (Phase 3a4).
/// wasm-bindgen class with two getters.
#[wasm_bindgen]
#[allow(clippy::struct_field_names)]
pub struct SframeParticipantKey {
    key_bytes_val: Vec<u8>,
    nonce_salt_val: Vec<u8>,
}

#[wasm_bindgen]
impl SframeParticipantKey {
    /// 32-byte AEAD key.
    #[wasm_bindgen(getter, js_name = keyBytes)]
    pub fn key_bytes(&self) -> Vec<u8> {
        self.key_bytes_val.clone()
    }

    /// 12-byte nonce salt — XOR with `(counter as u32 BE in
    /// the low 4 bytes)` to compose each per-frame nonce.
    #[wasm_bindgen(getter, js_name = nonceSalt)]
    pub fn nonce_salt(&self) -> Vec<u8> {
        self.nonce_salt_val.clone()
    }
}

/// Derive the per-call SFrame seed from a conversation MLS
/// group's exporter (Phase 3a4).
#[wasm_bindgen(js_name = sframeDeriveCallKey)]
pub fn sframe_derive_call_key(
    group: &Group,
    identity: &Identity,
    call_id: &[u8],
) -> Result<Vec<u8>, JsError> {
    let g = group
        .inner
        .lock()
        .map_err(|_| JsError::new("internal lock poisoned"))?;
    let key = lakko_calls::CallKey::derive(&g, &identity.inner, call_id).map_err(calls_err)?;
    Ok(key.as_bytes().to_vec())
}

/// Derive a per-participant SFrame key + nonce salt
/// (Phase 3a4).
#[wasm_bindgen(js_name = sframeDeriveParticipantKey)]
pub fn sframe_derive_participant_key(
    call_key_seed: &[u8],
    sender_leaf: u32,
) -> Result<SframeParticipantKey, JsError> {
    if call_key_seed.len() != 32 {
        return Err(JsError::new("call_key_seed must be 32 bytes"));
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(call_key_seed);
    let ck = lakko_calls::CallKey::from_seed(seed);
    let pk = ck.participant_key(sender_leaf).map_err(calls_err)?;
    Ok(SframeParticipantKey {
        key_bytes_val: pk.key_bytes().to_vec(),
        nonce_salt_val: pk.nonce_salt().to_vec(),
    })
}

/// Open an SFrame frame with the Phase 3c5 overlap window.
/// Tries `currentCallKeySeed` first; on AEAD rejection
/// falls back to `previousCallKeySeed` if not `null`
/// (caller passes `null` outside the 1-second overlap
/// window). Returns the plaintext on success.
///
/// Seed-flat shape — platform code holds the 32-byte seeds
/// in its own non-extractable WebCrypto store and ferries
/// them in per-call.
#[wasm_bindgen(js_name = sframeOpenWithOverlap)]
pub fn sframe_open_with_overlap(
    current_call_key_seed: &[u8],
    previous_call_key_seed: Option<Vec<u8>>,
    sender_leaf: u32,
    aad: &[u8],
    ciphertext: &[u8],
    frame_counter: u32,
) -> Result<Vec<u8>, JsError> {
    let take_seed = |bytes: &[u8]| -> Result<[u8; 32], JsError> {
        if bytes.len() != 32 {
            return Err(JsError::new(&format!(
                "call_key_seed must be 32 bytes, got {}",
                bytes.len()
            )));
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(bytes);
        Ok(seed)
    };

    let current_seed = take_seed(current_call_key_seed)?;
    let current = lakko_calls::CallKey::from_seed(current_seed);
    let current_key = current.participant_key(sender_leaf).map_err(calls_err)?;
    match current_key.open(aad, ciphertext, frame_counter) {
        Ok(pt) => return Ok(pt),
        Err(lakko_calls::Error::Crypto(_)) => {
            // Fall through to the overlap-window path.
        },
        Err(other) => return Err(calls_err(other)),
    }

    if let Some(prev_bytes) = previous_call_key_seed {
        let prev_seed = take_seed(&prev_bytes)?;
        let previous = lakko_calls::CallKey::from_seed(prev_seed);
        let prev_key = previous.participant_key(sender_leaf).map_err(calls_err)?;
        return prev_key
            .open(aad, ciphertext, frame_counter)
            .map_err(calls_err);
    }
    Err(JsError::new(
        "frame failed authentication under current key (no overlap window active)",
    ))
}

// ============================================================================
// Phase 3b4 — Call-admission JWT FFI surface (web client)
// ============================================================================
//
// Same shape Swift / Kotlin get via UniFFI's
// `FfiCallJwtClaims` Record; here we expose a `ParsedCallJwt`
// class with getters because wasm-bindgen rejects enums with
// associated data (same constraint that drove the
// `ParsedCallSignaling` flat-class shape in 3a4 / 3c2).

/// Parsed call-JWT claims — flat class with getters.
#[wasm_bindgen]
#[allow(clippy::struct_field_names)]
pub struct ParsedCallJwt {
    iss_val: String,
    aud_val: String,
    sub_val: String,
    call_id_val: String,
    role_val: &'static str,
    iat_val: u64,
    exp_val: u64,
    kid_val: String,
}

fn role_str(role: lakko_call_jwt::CallRole) -> &'static str {
    match role {
        lakko_call_jwt::CallRole::Participant => "participant",
        lakko_call_jwt::CallRole::Publisher => "publisher",
        lakko_call_jwt::CallRole::Subscriber => "subscriber",
    }
}

fn claims_to_parsed(claims: lakko_call_jwt::CallJwtClaims, kid: String) -> ParsedCallJwt {
    ParsedCallJwt {
        iss_val: claims.iss,
        aud_val: claims.aud,
        sub_val: claims.sub,
        call_id_val: claims.call_id,
        role_val: role_str(claims.role),
        iat_val: claims.iat,
        exp_val: claims.exp,
        kid_val: kid,
    }
}

fn jwt_err(e: lakko_call_jwt::JwtError) -> JsError {
    JsError::new(&format!("{e}"))
}

#[wasm_bindgen]
impl ParsedCallJwt {
    /// `iss` claim — always `"lakko-auth"` on a well-formed JWT.
    #[wasm_bindgen(getter)]
    pub fn iss(&self) -> String {
        self.iss_val.clone()
    }
    /// `aud` claim — always `"lakko-sfu"`.
    #[wasm_bindgen(getter)]
    pub fn aud(&self) -> String {
        self.aud_val.clone()
    }
    /// account_id, 32-char lower-case hex.
    #[wasm_bindgen(getter)]
    pub fn sub(&self) -> String {
        self.sub_val.clone()
    }
    /// call_id, 32-char lower-case hex.
    #[wasm_bindgen(getter, js_name = callId)]
    pub fn call_id(&self) -> String {
        self.call_id_val.clone()
    }
    /// One of `"participant"` / `"publisher"` / `"subscriber"`.
    #[wasm_bindgen(getter)]
    pub fn role(&self) -> String {
        self.role_val.to_string()
    }
    /// `iat` — issued-at, unix seconds.
    #[wasm_bindgen(getter)]
    pub fn iat(&self) -> u64 {
        self.iat_val
    }
    /// `exp` — expires-at, unix seconds.
    #[wasm_bindgen(getter)]
    pub fn exp(&self) -> u64 {
        self.exp_val
    }
    /// JWS header `kid` — 16 hex chars.
    #[wasm_bindgen(getter)]
    pub fn kid(&self) -> String {
        self.kid_val.clone()
    }
}

/// Parse a JWT into typed claims WITHOUT verifying the
/// signature. Useful for inspecting role / expiry / kid
/// client-side before presenting the JWT to the SFU.
#[wasm_bindgen(js_name = parseCallJwtUnverified)]
pub fn parse_call_jwt_unverified(jwt: &str) -> Result<ParsedCallJwt, JsError> {
    let (header, claims) = lakko_call_jwt::parse_unverified(jwt).map_err(jwt_err)?;
    Ok(claims_to_parsed(claims, header.kid))
}

/// Verify a JWT against a JWKS document the client fetched
/// from `GET /.well-known/jwks.json`. Returns the typed
/// claims on success.
///
/// `nowUnixSecs` is injected so the platform controls the
/// clock source — pass `Math.floor(Date.now() / 1000)`.
#[wasm_bindgen(js_name = verifyCallJwtWithJwks)]
pub fn verify_call_jwt_with_jwks(
    jwks_doc_bytes: &[u8],
    jwt: &str,
    now_unix_secs: u64,
) -> Result<ParsedCallJwt, JsError> {
    let jwks = lakko_call_jwt::parse_jwks(jwks_doc_bytes).map_err(jwt_err)?;
    let claims = lakko_call_jwt::verify_with_jwks(&jwks, jwt, now_unix_secs).map_err(jwt_err)?;
    let (header, _) = lakko_call_jwt::parse_unverified(jwt).map_err(jwt_err)?;
    Ok(claims_to_parsed(claims, header.kid))
}

// ============================================================================
// Phase 3a11 — Call quality metrics FFI (web client)
// ============================================================================

/// MOS estimation result. `level` is one of
/// `"excellent" | "good" | "fair" | "poor" | "bad"`.
#[wasm_bindgen]
#[allow(clippy::struct_field_names)]
pub struct CallQualityScore {
    r_factor_val: f32,
    mos_val: f32,
    level_val: &'static str,
}

#[wasm_bindgen]
impl CallQualityScore {
    /// E-model R-factor in `[0.0, 100.0]`.
    #[wasm_bindgen(getter, js_name = rFactor)]
    pub fn r_factor(&self) -> f32 {
        self.r_factor_val
    }

    /// Mean Opinion Score in `[1.0, 4.5]`.
    #[wasm_bindgen(getter)]
    pub fn mos(&self) -> f32 {
        self.mos_val
    }

    /// One of `"excellent"` / `"good"` / `"fair"` /
    /// `"poor"` / `"bad"`.
    #[wasm_bindgen(getter)]
    pub fn level(&self) -> String {
        self.level_val.to_string()
    }
}

fn level_str(l: lakko_calls::QualityLevel) -> &'static str {
    match l {
        lakko_calls::QualityLevel::Excellent => "excellent",
        lakko_calls::QualityLevel::Good => "good",
        lakko_calls::QualityLevel::Fair => "fair",
        lakko_calls::QualityLevel::Poor => "poor",
        lakko_calls::QualityLevel::Bad => "bad",
    }
}

fn parse_codec(s: &str) -> lakko_calls::AudioCodec {
    match s {
        "opus" | "Opus" | "OPUS" => lakko_calls::AudioCodec::Opus,
        _ => lakko_calls::AudioCodec::Other,
    }
}

/// Estimate call quality from a single network sample
/// (Phase 3a11). `codec` accepts `"opus"` (default for
/// Lakko voice / video) or any other value (mapped to the
/// no-FEC baseline).
#[wasm_bindgen(js_name = estimateCallQuality)]
pub fn estimate_call_quality(
    rtt_ms: u32,
    jitter_ms: u32,
    loss_pct: f32,
    codec: &str,
) -> CallQualityScore {
    let sample = lakko_calls::QualitySample {
        rtt_ms,
        jitter_ms,
        loss_pct,
        codec: parse_codec(codec),
    };
    let score = lakko_calls::estimate_quality(sample);
    CallQualityScore {
        r_factor_val: score.r_factor,
        mos_val: score.mos,
        level_val: level_str(score.level),
    }
}

/// Recommend a new encoder bitrate given the current one +
/// a fresh quality sample. Returns the current value
/// unchanged when the sample falls in the "hold" band.
#[wasm_bindgen(js_name = recommendCallBitrate)]
pub fn recommend_call_bitrate(
    current_bps: u32,
    rtt_ms: u32,
    jitter_ms: u32,
    loss_pct: f32,
    codec: &str,
) -> u32 {
    let sample = lakko_calls::QualitySample {
        rtt_ms,
        jitter_ms,
        loss_pct,
        codec: parse_codec(codec),
    };
    lakko_calls::recommend_bitrate(current_bps, sample)
}

/// Default ring-buffer size for [`CallQualitySampler`].
#[wasm_bindgen(js_name = defaultQualitySampleWindow)]
#[must_use]
pub fn default_quality_sample_window() -> u32 {
    u32::try_from(lakko_calls::DEFAULT_SAMPLE_WINDOW).unwrap_or(5)
}

/// Maximum ring-buffer size accepted by
/// [`CallQualitySampler::new`].
#[wasm_bindgen(js_name = maxQualitySampleWindow)]
#[must_use]
pub fn max_quality_sample_window() -> u32 {
    u32::try_from(lakko_calls::MAX_SAMPLE_WINDOW).unwrap_or(60)
}

/// Stateful rolling-window smoothed quality sampler
/// (Phase 3a11). One instance per active call.
///
/// `record` pushes a fresh sample and returns the
/// smoothed [`CallQualityScore`]; `latest` returns the
/// current smoothed score without pushing; `clear` resets
/// the window (on call end / SFU handoff).
#[wasm_bindgen]
pub struct CallQualitySampler {
    inner: lakko_calls::QualitySampler,
}

#[wasm_bindgen]
impl CallQualitySampler {
    /// Construct a sampler with a custom window size.
    /// Sizes outside `[1, MAX_SAMPLE_WINDOW]` are clamped.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new(window_size: u32) -> Self {
        let bounded = usize::try_from(window_size).unwrap_or(lakko_calls::DEFAULT_SAMPLE_WINDOW);
        Self {
            inner: lakko_calls::QualitySampler::new(bounded),
        }
    }

    /// Effective window size after clamping.
    #[wasm_bindgen(getter, js_name = windowSize)]
    pub fn window_size(&self) -> u32 {
        u32::try_from(self.inner.window_size()).unwrap_or(u32::MAX)
    }

    /// Number of samples currently buffered.
    #[wasm_bindgen(getter)]
    pub fn count(&self) -> u32 {
        u32::try_from(self.inner.count()).unwrap_or(u32::MAX)
    }

    /// Drop every buffered sample.
    pub fn clear(&mut self) {
        self.inner.clear();
    }

    /// Push a sample and return the smoothed score.
    pub fn record(
        &mut self,
        rtt_ms: u32,
        jitter_ms: u32,
        loss_pct: f32,
        codec: &str,
    ) -> CallQualityScore {
        let sample = lakko_calls::QualitySample {
            rtt_ms,
            jitter_ms,
            loss_pct,
            codec: parse_codec(codec),
        };
        let score = self.inner.record(sample);
        CallQualityScore {
            r_factor_val: score.r_factor,
            mos_val: score.mos,
            level_val: level_str(score.level),
        }
    }

    /// Latest smoothed score without pushing a new sample.
    /// Returns `null` when the sampler is empty.
    pub fn latest(&self) -> Option<CallQualityScore> {
        self.inner.latest().map(|score| CallQualityScore {
            r_factor_val: score.r_factor,
            mos_val: score.mos,
            level_val: level_str(score.level),
        })
    }
}

// Tests run on the host target (`cargo test`) — no `wasm-bindgen-test`
// runner needed for what we cover here. The wire-level invariants
// from `lakko-mls` are unchanged regardless of FFI binding; this
// suite exists to catch shape regressions in the wasm-bindgen
// surface itself (constructor signatures, getter return types, the
// ProcessedMessage tagging convention).

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirror of the lakko-mls integration test driven through the
    /// wasm-bindgen surface. Same end-to-end check the UniFFI mirror
    /// runs — exercises identity creation, group creation, member
    /// addition (commit + welcome split), application-message round-
    /// trip, exporter consistency.
    #[test]
    fn wasm_three_party_lifecycle() {
        let alice = Identity::new("alice").expect("alice identity");
        let bob = Identity::new("bob").expect("bob identity");
        let charlie = Identity::new("charlie").expect("charlie identity");

        let alice_group = Group::new(&alice).expect("alice group");

        // Alice adds Bob.
        let bob_kp = bob.create_key_package().expect("bob key package");
        let add_bob = alice_group.add_member(&alice, &bob_kp).expect("add bob");
        let bob_group = Group::join_from_welcome(&add_bob.welcome(), &bob).expect("bob join");

        // Alice → Bob application message.
        let msg = alice_group
            .create_application_message(&alice, b"hello via wasm")
            .expect("alice send");
        let received = bob_group.process_incoming(&bob, &msg).expect("bob recv");
        assert_eq!(received.kind(), "Application");
        assert_eq!(
            received.application_data().as_deref(),
            Some(&b"hello via wasm"[..]),
        );

        // Alice adds Charlie; Bob processes the commit.
        let charlie_kp = charlie.create_key_package().expect("charlie kp");
        let add_charlie = alice_group
            .add_member(&alice, &charlie_kp)
            .expect("add charlie");
        let bob_post = bob_group
            .process_incoming(&bob, &add_charlie.commit())
            .expect("bob process commit");
        assert_eq!(bob_post.kind(), "Commit");
        assert!(bob_post.application_data().is_none());

        let charlie_group =
            Group::join_from_welcome(&add_charlie.welcome(), &charlie).expect("charlie join");

        // Member counts converge.
        assert_eq!(alice_group.member_count().unwrap(), 3);
        assert_eq!(bob_group.member_count().unwrap(), 3);
        assert_eq!(charlie_group.member_count().unwrap(), 3);

        // Exporter consistency — identical secrets across the group.
        let alice_export = alice_group
            .export_secret(&alice, "lakko/test", b"ctx", 32)
            .expect("alice export");
        let charlie_export = charlie_group
            .export_secret(&charlie, "lakko/test", b"ctx", 32)
            .expect("charlie export");
        assert_eq!(alice_export, charlie_export);
    }

    #[test]
    fn identity_signature_public_key_is_32_bytes() {
        let id = Identity::new("alice").unwrap();
        let pk = id.signature_public_key();
        assert_eq!(pk.len(), 32, "Ed25519 public key length");
    }

    /// Phase 1.16e6b — round-trip a live group through
    /// `Identity.snapshotStorage` / `Identity.restoreStorage` +
    /// `Group.load` and confirm the post-restore group reports
    /// the same `groupId`, `memberCount`, and own leaf index.
    /// Mirrors the host-only Rust integration test in
    /// `crates/lakko-mls/tests/end_to_end.rs` — JS-side error
    /// paths (panics in `JsError::new`) stay in
    /// `wasm-bindgen-test` per the rest of the FFI suite.
    #[test]
    fn snapshot_restore_load_round_trips_a_group() {
        let seed = [0xE6u8; 64];
        let alice = Identity::from_seed(&seed).expect("alice");
        let group = Group::new(&alice).expect("group");
        let group_id = group.group_id().expect("group_id");
        let initial_members = group.member_count().expect("count");
        let initial_leaf = group.own_leaf_index().expect("leaf");

        let snapshot = alice.snapshot_storage().expect("snapshot");
        assert!(!snapshot.is_empty());

        // Drop the original alice + group entirely; re-derive
        // alice from the same seed and restore.
        drop(group);
        drop(alice);

        let restored_alice = Identity::from_seed(&seed).expect("restored alice");
        restored_alice.restore_storage(&snapshot).expect("restore");

        let restored_group = Group::load(&restored_alice, &group_id)
            .expect("load")
            .expect("group present");
        assert_eq!(restored_group.group_id().expect("gid"), group_id);
        assert_eq!(restored_group.member_count().expect("mc"), initial_members);
        assert_eq!(restored_group.own_leaf_index().expect("li"), initial_leaf);
    }

    /// `Group.load` returns `None` when no group is persisted
    /// under that id — the JS side reads `undefined`.
    #[test]
    fn group_load_returns_none_for_unknown_id() {
        let alice = Identity::from_seed(&[0xE7u8; 64]).expect("alice");
        let arbitrary = [0x99u8; 32];
        let loaded = Group::load(&alice, &arbitrary).expect("load");
        assert!(loaded.is_none());
    }

    /// Phase 1.16d3b2 — `Identity.fromSeed` is deterministic.
    /// Same seed bytes ⇒ same Ed25519 + X25519 public keys.
    #[test]
    fn identity_from_seed_is_deterministic() {
        let seed = [0x77u8; 64];
        let a = Identity::from_seed(&seed).unwrap();
        let b = Identity::from_seed(&seed).unwrap();
        assert_eq!(a.signature_public_key(), b.signature_public_key());
        assert_eq!(a.x25519_public_key(), b.x25519_public_key());
    }

    /// Phase 1.16d3b — the X25519 envelope keypair surfaces as a
    /// pair of 32-byte `Vec<u8>` getters. Pubkey copies are fresh
    /// each call (idempotent, no shared mutable state).
    #[test]
    fn identity_x25519_keys_are_32_bytes_each() {
        let id = Identity::new("alice").unwrap();
        let pk = id.x25519_public_key();
        let sk = id.x25519_secret_key();
        assert_eq!(pk.len(), 32, "X25519 public key length");
        assert_eq!(sk.len(), 32, "X25519 secret key length");
        // Sanity: at least one byte non-zero on each — would
        // catch a default-initialised secret slipping through.
        assert!(pk.iter().any(|&b| b != 0));
        assert!(sk.iter().any(|&b| b != 0));
        // Idempotent reads — getters return copies of the same
        // underlying secret.
        assert_eq!(id.x25519_public_key(), pk);
        assert_eq!(id.x25519_secret_key(), sk);
    }

    #[test]
    fn processed_message_kind_strings_match_uniffi_enum() {
        // Every variant has a string literal — guards against
        // accidental rename on the Rust side breaking the JS API.
        let app = ProcessedMessage {
            kind: ProcessedMessageKind::Application,
            application_data: Some(vec![1, 2, 3]),
        };
        assert_eq!(app.kind(), "Application");
        assert_eq!(app.application_data().as_deref(), Some(&[1, 2, 3][..]));

        for k in [
            ProcessedMessageKind::Commit,
            ProcessedMessageKind::Proposal,
            ProcessedMessageKind::ExternalJoinProposal,
        ] {
            let m = ProcessedMessage {
                kind: k,
                application_data: None,
            };
            // Each non-Application case has no data.
            assert!(m.application_data().is_none(), "kind={k:?}");
            // And a non-empty discriminator string.
            assert!(!m.kind().is_empty());
        }
    }

    #[test]
    fn add_member_record_clones_on_each_getter_call() {
        // Confirm the getters don't move bytes out — JS callers can
        // read .commit / .welcome multiple times.
        let r = AddMember {
            commit_bytes: vec![0xAA; 4],
            welcome_bytes: vec![0xBB; 4],
        };
        assert_eq!(r.commit(), vec![0xAA; 4]);
        assert_eq!(r.commit(), vec![0xAA; 4]); // call twice → still same
        assert_eq!(r.welcome(), vec![0xBB; 4]);
    }
}
