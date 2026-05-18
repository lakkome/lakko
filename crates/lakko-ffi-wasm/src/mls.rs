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
    pub fn create_founder(
        identity: &Identity,
        account_id: &[u8],
    ) -> Result<DeviceGroup, JsError> {
        if account_id.len() != 16 {
            return Err(JsError::new("account_id must be 16 bytes"));
        }
        let mut acct = [0u8; 16];
        acct.copy_from_slice(account_id);
        let inner =
            MlsDeviceGroup::create_founder(&identity.inner, &acct).map_err(mls_err)?;
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
        let secret = lakko_mls::history_bundle::exporter_secret(&g, &identity.inner)
            .map_err(mls_err)?;
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
    let parsed =
        lakko_mls::DeviceGroup::parse_devicegroup_payload(plaintext).map_err(mls_err)?;
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

    let bundle =
        lakko_proto::mls_app::history_bundle::v1::HistoryBundle::decode(bundle_bytes)
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
