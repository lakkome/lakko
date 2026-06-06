// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! `DeviceGroup` — per-user multi-device MLS substrate (Phase 2a3a).
//!
//! A `DeviceGroup` is a regular MLS group whose **leaves are the
//! user's own devices**, not contacts. The same MLS state machine
//! [`crate::group::Group`] uses powers it; we wrap it with a
//! conventional group_id (deterministic from `account_id`) and a
//! small surface that only exposes the operations a multi-device
//! flow needs:
//!
//!   * [`DeviceGroup::create_founder`] — bootstrap on signup, the
//!     local device becomes the sole leaf.
//!   * [`DeviceGroup::join_from_welcome`] — join from a Welcome a
//!     paired device sent over the rendezvous channel (e.g. QR ↔
//!     KeyPackage round-trip in 2a3b).
//!   * [`DeviceGroup::add_device`] / [`DeviceGroup::remove_device`]
//!     — Commit + Welcome / Commit. Same OpenMLS calls
//!     [`crate::group::Group`] exposes; the wrapper names them
//!     for clarity at the call site.
//!   * [`DeviceGroup::create_application_message`] /
//!     [`DeviceGroup::process_incoming`] — the application-message
//!     side. 2a3e ships a `schemas/mls-app/devicegroup.proto` with
//!     the concrete payload shapes (read receipts, drafts,
//!     conversation order); this layer is transport-only and
//!     deliberately ignores the bytes.
//!
//! ## Conventional group_id
//!
//! `group_id = BLAKE3("lakko-device-group/v1" || account_id)`.
//! Deterministic, 32 bytes, unique per account, predictable from
//! anything that already knows `account_id`. The server still
//! treats it as opaque — the DS journal indexes by `group_id` as
//! bytes; only the client knows it's a DeviceGroup.
//!
//! Why deterministic? Two reasons:
//!
//! 1. A paired device joining via a Welcome can verify that the
//!    `group_id` matches the expected hash for its own
//!    `account_id`. A misrouted Welcome from a different user's
//!    DeviceGroup is rejected at join time, before any state
//!    advance.
//! 2. The post-recovery flow on a fresh device knows the
//!    `group_id` to ask the server for catch-up MLS journal
//!    entries before any other device has had a chance to send
//!    the new device a Welcome.
//!
//! ## What lives **outside** this module
//!
//!   * **Wire transport** — the DS journal +/- mailboxes that
//!     ferry Commits/Welcomes between devices is a normal
//!     `services/ds` consumer. No protocol delta.
//!   * **Backup envelope `mls_snapshots` field** — the v1 payload
//!     reserves the slot empty (2a1a); populating it with the
//!     DeviceGroup state snapshot is the 2a3d work item.
//!   * **Application payloads** — see 2a3e + the protobuf in
//!     `schemas/mls-app/devicegroup.proto`.

use openmls::extensions::{
    Extension, ExtensionType, Extensions, RequiredCapabilitiesExtension, UnknownExtension,
};
use openmls::framing::MlsMessageOut;
use openmls::group::{MlsGroup, MlsGroupCreateConfig};
use openmls::prelude::{Capabilities, Ciphersuite, GroupId, KeyPackage, LeafNodeIndex};

use lakko_crypto::signing::VerifyingKey;

use crate::error::{Error, Result};
use crate::group::{Group, Incoming, MemberAddition, MemberRecord};
use crate::identity::Identity;

/// Custom GroupContext extension type carrying the DeviceGroup removal
/// policy (G-13, [ADR-0015]). `0xFF01` is in the private-use range, so
/// OpenMLS surfaces it as `Extension::Unknown`. Every Lakko device leaf
/// advertises support for this type (see [`device_leaf_capabilities`]) so a
/// DeviceGroup can carry the policy and any device can join one that does.
///
/// [ADR-0015]: ../../../docs/adr/0015-device-removal-authorization.md
pub(crate) const DEVICE_POLICY_EXTENSION_TYPE: u16 = 0xFF01;

/// Policy payload byte: device removals require a peer-verified attestation.
const POLICY_REQUIRE_ATTESTED_REMOVALS: u8 = 1;

/// Leaf capabilities every Lakko device advertises so it can participate in
/// a DeviceGroup that carries the removal-policy GroupContext extension
/// (G-13, ADR-0015). Shared by DeviceGroup creation and KeyPackage creation
/// (a device may need to JOIN such a group, and OpenMLS validates that every
/// member's leaf advertises a GroupContext extension type in use).
pub(crate) fn device_leaf_capabilities(cs: Ciphersuite) -> Capabilities {
    Capabilities::new(
        None,
        Some(&[cs]),
        Some(&[ExtensionType::Unknown(DEVICE_POLICY_EXTENSION_TYPE)]),
        None,
        None,
    )
}

/// The `required_capabilities` GroupContext extension a DeviceGroup carries
/// from creation. It lists the removal-policy extension type, which OpenMLS
/// requires before a `GroupContextExtensions` proposal may *add* that
/// extension later — i.e. it is what lets the policy be flipped on after the
/// fact (G-13, ADR-0015). It does NOT set the policy itself; the group stays
/// policy-off until [`DeviceGroup::set_require_attested_removals`].
fn device_required_capabilities() -> Extension {
    Extension::RequiredCapabilities(RequiredCapabilitiesExtension::new(
        &[ExtensionType::Unknown(DEVICE_POLICY_EXTENSION_TYPE)],
        &[],
        &[],
    ))
}

/// Domain separator for the remove-device action hash (G-13, ADR-0015).
/// MUST match the clients' `ACTION_DOMAIN` for remove-device so the peer
/// recomputes the same `action_hash` the auth service signed the
/// attestation over.
const REMOVE_DEVICE_ACTION_DOMAIN: &str = "lakko/action/remove-device/v1";

/// The action hash a remove-device attestation binds to:
/// `SHA-256(domain ‖ removed_leaf_signature_pubkey)`. The removing client
/// computes this from the leaf it is evicting, the auth service signs an
/// attestation over it, and a peer recomputes it here from the removed
/// leaf's signature key to verify (G-13, ADR-0015).
pub(crate) fn remove_device_action_hash(removed_signature_pubkey: &[u8]) -> [u8; 32] {
    let mut input =
        Vec::with_capacity(REMOVE_DEVICE_ACTION_DOMAIN.len() + removed_signature_pubkey.len());
    input.extend_from_slice(REMOVE_DEVICE_ACTION_DOMAIN.as_bytes());
    input.extend_from_slice(removed_signature_pubkey);
    lakko_crypto::hash::sha256(&input)
}

/// Domain-separation tag for the deterministic group_id hash.
/// Pinned ASCII; any change is a breaking protocol change to
/// every existing account's DeviceGroup.
const GROUP_ID_DOMAIN: &[u8] = b"lakko-device-group/v1";

/// Raw 16-byte account_id (UUIDv7).
pub type AccountId = [u8; 16];

/// MLS group whose leaves are the user's own devices.
///
/// Owns:
///
///   * the founding device's local view of the MLS group state
///     (via the wrapped [`Group`])
///   * the `account_id` the group_id was derived from — kept so
///     `group_id()` can be cheaply queried without re-hashing
pub struct DeviceGroup {
    inner: Group,
    account_id: AccountId,
}

impl DeviceGroup {
    /// Compute the deterministic group id for an account.
    ///
    /// `group_id = BLAKE3("lakko-device-group/v1" || account_id)`.
    /// Output is always 32 bytes; clients can compare directly
    /// against a wire-format group_id without parsing or
    /// normalising.
    #[must_use]
    pub fn group_id_for(account_id: &AccountId) -> Vec<u8> {
        // Concatenate domain tag + account_id, then BLAKE3.
        // 21 + 16 = 37 bytes, single stack allocation.
        let mut buf = Vec::with_capacity(GROUP_ID_DOMAIN.len() + account_id.len());
        buf.extend_from_slice(GROUP_ID_DOMAIN);
        buf.extend_from_slice(account_id);
        lakko_crypto::hash::blake3(&buf).to_vec()
    }

    /// Bootstrap a DeviceGroup with `identity` as the founding
    /// leaf. Called on signup / recover when there is no prior
    /// DeviceGroup state on this device.
    ///
    /// The group's MLS `group_id` is set to
    /// [`DeviceGroup::group_id_for(account_id)`], not a random
    /// UUID. See the module docstring for the rationale.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Group`] for any OpenMLS-side failure
    /// (signature, storage, etc.).
    pub fn create_founder(identity: &Identity, account_id: &AccountId) -> Result<Self> {
        let group_id_bytes = Self::group_id_for(account_id);
        // G-13 (ADR-0015): declare the removal-policy extension type in
        // `required_capabilities` at creation, so a later
        // `GroupContextExtensions` proposal may add the policy flag (OpenMLS
        // refuses to add an extension that isn't in required_capabilities).
        let required = Extensions::single(device_required_capabilities())
            .map_err(|e| Error::Group(format!("required_capabilities: {e:?}")))?;
        let cfg = MlsGroupCreateConfig::builder()
            .ciphersuite(identity.ciphersuite())
            // Same posture as `Group::create` — the ratchet tree
            // travels inside Welcomes so a freshly-paired device
            // doesn't need an out-of-band tree fetch.
            .use_ratchet_tree_extension(true)
            // Advertise support for the removal-policy GroupContext extension.
            .capabilities(device_leaf_capabilities(identity.ciphersuite()))
            .with_group_context_extensions(required)
            .build();

        let mls = MlsGroup::new_with_group_id(
            &identity.provider,
            &identity.signature_keys,
            &cfg,
            GroupId::from_slice(&group_id_bytes),
            identity.credential.clone(),
        )
        .map_err(|e| Error::Group(format!("DeviceGroup create: {e:?}")))?;

        Ok(Self {
            inner: Group::from_mls_group(mls),
            account_id: *account_id,
        })
    }

    /// Join a DeviceGroup from a Welcome that an existing device
    /// produced for our [`KeyPackage`].
    ///
    /// Verifies that the embedded `group_id` matches
    /// [`DeviceGroup::group_id_for(account_id)`] — a misrouted
    /// Welcome from a different user's DeviceGroup is rejected
    /// before any state advance.
    ///
    /// # Errors
    ///
    /// - [`Error::Codec`] when the wire bytes don't decode.
    /// - [`Error::Unsupported`] when the message is not a
    ///   Welcome.
    /// - [`Error::Group`] for OpenMLS failures
    ///   (signature, ciphersuite, storage).
    /// - [`Error::Group`] with a `"group_id mismatch"` body
    ///   when the deterministic check fails.
    pub fn join_from_welcome(
        welcome_bytes: &[u8],
        identity: &Identity,
        account_id: &AccountId,
    ) -> Result<Self> {
        let inner = Group::join_from_welcome(welcome_bytes, identity)?;
        let expected = Self::group_id_for(account_id);
        if inner.group_id().as_slice() != expected.as_slice() {
            return Err(Error::Group(
                "DeviceGroup join: group_id mismatch (Welcome is for a different account)".into(),
            ));
        }
        Ok(Self {
            inner,
            account_id: *account_id,
        })
    }

    /// Load the DeviceGroup for `account_id` from `identity`'s
    /// MLS provider storage. Returns `None` when the provider
    /// has no row for the deterministic
    /// [`DeviceGroup::group_id_for(account_id)`] (typical
    /// case: a freshly-bootstrapped session that hasn't yet
    /// rehydrated, or this device hasn't joined the
    /// DeviceGroup yet).
    ///
    /// Pair with [`crate::identity::Identity::restore_storage`]
    /// on app startup: restore the snapshot first, then call
    /// `DeviceGroup::load` to get a live handle.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Group`] when storage carries a row
    /// under the expected `group_id` but OpenMLS rejects it
    /// (corrupt snapshot, ciphersuite mismatch, …).
    pub fn load(identity: &Identity, account_id: &AccountId) -> Result<Option<Self>> {
        let group_id = Self::group_id_for(account_id);
        let loaded = Group::load(identity, &group_id)?;
        Ok(loaded.map(|inner| Self {
            inner,
            account_id: *account_id,
        }))
    }

    /// Reconstruct a [`DeviceGroup`] handle from an underlying
    /// [`Group`] already loaded from disk. The caller asserts
    /// (via `account_id`) what the group_id should look like;
    /// the mismatch is rejected as a sanity check.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Group`] with a `"group_id mismatch"`
    /// body when the inner group_id doesn't match
    /// [`DeviceGroup::group_id_for(account_id)`].
    pub fn from_group(group: Group, account_id: &AccountId) -> Result<Self> {
        let expected = Self::group_id_for(account_id);
        if group.group_id().as_slice() != expected.as_slice() {
            return Err(Error::Group(
                "DeviceGroup from_group: group_id mismatch".into(),
            ));
        }
        Ok(Self {
            inner: group,
            account_id: *account_id,
        })
    }

    /// The deterministic group_id this DeviceGroup is bound to.
    /// Cheap accessor (no re-hash) — the bytes live next to the
    /// struct since construction.
    #[must_use]
    pub fn group_id(&self) -> Vec<u8> {
        Self::group_id_for(&self.account_id)
    }

    /// Raw 16-byte UUIDv7 account this DeviceGroup belongs to.
    #[must_use]
    pub fn account_id(&self) -> &AccountId {
        &self.account_id
    }

    /// Add a new device by its published [`KeyPackage`]. Caller
    /// distributes:
    ///
    ///   * the [`MemberAddition::commit`] to every existing
    ///     DeviceGroup member (incl. the local device — OpenMLS
    ///     merges the pending commit automatically as part of
    ///     this call),
    ///   * the [`MemberAddition::welcome`] to the new device via
    ///     the QR rendezvous channel (2a3b).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Group`] for OpenMLS validation /
    /// signature / storage failures.
    pub fn add_device(
        &mut self,
        identity: &Identity,
        key_package: KeyPackage,
    ) -> Result<MemberAddition> {
        self.inner.add_member(identity, key_package)
    }

    /// Convenience wrapper that deserialises a wire-format
    /// `KeyPackage` blob (the bytes a paired device sends via
    /// the QR rendezvous) and adds the device.
    ///
    /// # Errors
    ///
    /// See [`crate::group::Group::add_member_from_wire`] for the
    /// validation-failure cases.
    pub fn add_device_from_wire(
        &mut self,
        identity: &Identity,
        key_package_bytes: &[u8],
    ) -> Result<MemberAddition> {
        self.inner.add_member_from_wire(identity, key_package_bytes)
    }

    /// Revoke a device by its leaf index. The Commit must be
    /// broadcast to every remaining DeviceGroup member so they
    /// advance past the revoked leaf and ratchet the group
    /// secrets out of the compromised device's reach.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Group`] for OpenMLS-side failures.
    pub fn remove_device(
        &mut self,
        identity: &Identity,
        leaf: LeafNodeIndex,
    ) -> Result<MlsMessageOut> {
        self.inner.remove_member(identity, leaf)
    }

    /// FFI-friendly wrapper around [`Self::remove_device`].
    ///
    /// Takes a raw `u32` leaf index (UniFFI / wasm-bindgen
    /// don't speak `LeafNodeIndex`) and returns the wire-format
    /// bytes of the resulting Commit. The local view advances
    /// atomically inside [`Self::remove_device`] (which calls
    /// `merge_pending_commit` after building the commit), so
    /// after this returns the caller's job is simply to
    /// broadcast `commit_bytes` to every remaining device on
    /// the DS group journal.
    ///
    /// Caller-visible safety:
    ///
    ///   * **Don't remove yourself.** `leaf_index ==
    ///     own_leaf_index()` is rejected up front — every
    ///     remaining peer would still ratchet past us, but our
    ///     local Commit `merge_pending_commit` would fail mid-
    ///     way (the leaf the commit's signer points at is no
    ///     longer in the tree the merge resolves to). The user
    ///     should use `forgetDevice` on this device instead.
    ///   * **Leaf indices renumber on epoch boundaries.** Pass
    ///     a `leaf_index` taken from a `devices()` snapshot in
    ///     the SAME epoch; if a concurrent Commit landed since
    ///     the snapshot, refetch via `devices()` and retry.
    ///
    /// # Errors
    ///
    /// - [`Error::Group`] with a "cannot remove own leaf" body
    ///   when `leaf_index == own_leaf_index()`.
    /// - [`Error::Group`] with a "no such device leaf" body when
    ///   the supplied index doesn't correspond to any current
    ///   member.
    /// - [`Error::Group`] for OpenMLS-side failures
    ///   (signature, storage, …).
    /// - [`Error::Codec`] / [`Error::Message`] surfacing the
    ///   inner wire-encode failure (practically unreachable —
    ///   OpenMLS produced the message in the previous step).
    pub fn remove_device_at(&mut self, identity: &Identity, leaf_index: u32) -> Result<Vec<u8>> {
        let own = self.inner.own_leaf_index().u32();
        if leaf_index == own {
            return Err(Error::Group(
                "DeviceGroup remove_device_at: cannot remove own leaf — use forgetDevice instead"
                    .into(),
            ));
        }
        // Bail before we hand OpenMLS a leaf it doesn't know
        // about, so the caller gets a clean "no such device"
        // signal instead of an opaque `remove_members` error.
        if !self
            .inner
            .members()
            .iter()
            .any(|m| m.leaf_index == leaf_index)
        {
            return Err(Error::Group(format!(
                "DeviceGroup remove_device_at: no such device leaf {leaf_index}"
            )));
        }
        let commit_out = self
            .inner
            .remove_member(identity, LeafNodeIndex::new(leaf_index))?;
        crate::group::encode_message(&commit_out)
    }

    /// Flip this DeviceGroup's policy to **require peer-verified attestation
    /// for device removals** (G-13, ADR-0015). Returns the wire-encoded
    /// commit to broadcast over the DeviceGroup journal; every device that
    /// processes it then reads the policy from its authenticated
    /// GroupContext via [`Self::require_attested_removals`].
    ///
    /// Called when the account first enables a passkey. OPAQUE-only accounts
    /// never call it, so their DeviceGroups stay policy-off and removals are
    /// not attestation-gated (peers cannot demand an attestation the account
    /// has no passkey to produce).
    ///
    /// # Errors
    ///
    /// [`Error::Group`] on the OpenMLS update / merge, or [`Error::Codec`] on
    /// commit encoding.
    pub fn set_require_attested_removals(&mut self, identity: &Identity) -> Result<Vec<u8>> {
        // `update_group_context_extensions` REPLACES the whole GroupContext
        // extension set, so we must re-include `required_capabilities`
        // alongside the policy flag (and the policy type must be in
        // required_capabilities for the proposal to validate).
        let exts = Extensions::from_vec(vec![
            device_required_capabilities(),
            Extension::Unknown(
                DEVICE_POLICY_EXTENSION_TYPE,
                UnknownExtension(vec![POLICY_REQUIRE_ATTESTED_REMOVALS]),
            ),
        ])
        .map_err(|e| Error::Group(format!("build policy extension: {e:?}")))?;
        let commit = self.inner.set_group_context_extensions(identity, exts)?;
        crate::group::encode_message(&commit)
    }

    /// Whether this DeviceGroup currently requires a peer-verified
    /// attestation for removals (G-13, ADR-0015). Read from the
    /// authenticated GroupContext, so every device that has applied the
    /// policy commit agrees on the answer.
    #[must_use]
    pub fn require_attested_removals(&self) -> bool {
        self.inner
            .group_context_extension(DEVICE_POLICY_EXTENSION_TYPE)
            .is_some_and(|v| v.first() == Some(&POLICY_REQUIRE_ATTESTED_REMOVALS))
    }

    /// Apply an incoming DeviceGroup commit, **enforcing the attested-removal
    /// policy** (G-13, ADR-0015). This is the peer-verification side: when
    /// this group [`requires attested removals`](Self::require_attested_removals)
    /// and the commit removes one or more leaves, EVERY removed leaf must be
    /// covered by an attestation in `attestations` that verifies against
    /// `attest_vk`, is bound to this account and to the removed leaf's
    /// [`remove_device_action_hash`], and is fresh at `now_unix_secs`. If any
    /// removed leaf lacks a valid attestation the commit is **refused** — not
    /// merged — so on-device malware that evicts a device without a real
    /// passkey gesture cannot make peers drop that device.
    ///
    /// When the policy is off, or the commit removes nothing (an add, an
    /// update, the policy-set commit itself), no attestation is required and
    /// this behaves like [`process_incoming`](Self::process_incoming).
    ///
    /// The attestation(s) travel alongside the commit at the application /
    /// transport layer (a client concern); the core only needs the commit
    /// bytes plus whatever attestations accompanied them.
    ///
    /// # Errors
    ///
    /// [`Error::Group`] when a required attestation is missing or invalid (the
    /// commit is left un-merged), plus the usual decode / process errors.
    pub fn process_incoming_verifying_removals(
        &mut self,
        identity: &Identity,
        commit_bytes: &[u8],
        attestations: &[Vec<u8>],
        attest_vk: &VerifyingKey,
        now_unix_secs: i64,
    ) -> Result<Incoming> {
        let require = self.require_attested_removals();
        let account_id = self.account_id;
        self.inner
            .process_incoming_gated(identity, commit_bytes, |removed| {
                if !require || removed.is_empty() {
                    return Ok(());
                }
                for (leaf, pubkey) in removed {
                    let action_hash = remove_device_action_hash(pubkey);
                    let covered = attestations.iter().any(|att| {
                        lakko_attest::verify_bound(
                            att,
                            attest_vk,
                            &account_id,
                            &action_hash,
                            now_unix_secs,
                        )
                        .is_ok()
                    });
                    if !covered {
                        return Err(Error::Group(format!(
                            "DeviceGroup: removal of leaf {} lacks a valid attestation \
                             (policy requires attested removals)",
                            leaf.u32()
                        )));
                    }
                }
                Ok(())
            })
    }

    /// List every device leaf currently in the group. Useful
    /// for rendering a "Your devices" management screen.
    #[must_use]
    pub fn devices(&self) -> Vec<MemberRecord> {
        self.inner.members()
    }

    /// Leaf index of THIS device inside the DeviceGroup.
    ///
    /// UI uses this to render the "(this device)" badge next
    /// to one row of the device list and to disable that row's
    /// Remove button. Cheap to call — no hash, no copy.
    ///
    /// Stable inside an epoch; can renumber after a
    /// remove/add cycle, so callers MUST NOT cache the value
    /// across DeviceGroup state changes.
    #[must_use]
    pub fn own_leaf_index(&self) -> u32 {
        self.inner.own_leaf_index().u32()
    }

    /// Encrypt a cross-device application payload (read receipt,
    /// draft change, conversation-order update — see
    /// `schemas/mls-app/devicegroup.proto`). The returned wire
    /// bytes go through the same DS journal every other group
    /// uses.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Group`] for OpenMLS encryption failures.
    pub fn create_application_message(
        &mut self,
        identity: &Identity,
        payload: &[u8],
    ) -> Result<MlsMessageOut> {
        self.inner.create_application_message(identity, payload)
    }

    /// Decrypt / advance the state machine for an inbound
    /// message from the DS journal (Commit / Welcome /
    /// application). Mirrors [`Group::process_incoming`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Codec`] / [`Error::Group`] etc. — see
    /// [`Group::process_incoming`].
    pub fn process_incoming(
        &mut self,
        identity: &Identity,
        message_bytes: &[u8],
    ) -> Result<Incoming> {
        self.inner.process_incoming(identity, message_bytes)
    }

    /// Borrow the underlying [`Group`] for callers that need the
    /// raw MLS surface (e.g. exporter secrets for backup
    /// snapshot encryption — 2a3d).
    #[must_use]
    pub fn as_group(&self) -> &Group {
        &self.inner
    }

    /// Mutable borrow — same caveat as [`Self::as_group`].
    pub fn as_group_mut(&mut self) -> &mut Group {
        &mut self.inner
    }

    // -----------------------------------------------------------------
    // Phase 2a3e1 — typed cross-device application payloads
    // -----------------------------------------------------------------

    /// Compose + encrypt a cross-device payload (`DeviceGroupPayload`
    /// from `schemas/mls-app/devicegroup_v1.proto`) and return the
    /// wire-format bytes ready to PUT to the DS group journal.
    ///
    /// Caller supplies the typed [`DeviceGroupBody`] + the HLC
    /// timestamp (callers are expected to wire a single HLC
    /// generator at the application layer so total ordering
    /// survives across devices). `sender_leaf` is taken from the
    /// local view of the group, not threaded by the caller.
    ///
    /// # Errors
    ///
    /// - [`Error::Codec`] when the proto encode fails (practically
    ///   unreachable — every field is fixed-shape).
    /// - [`Error::Message`] / [`Error::Group`] for OpenMLS-side
    ///   encryption or wire-encode failures.
    pub fn create_devicegroup_message(
        &mut self,
        identity: &Identity,
        body: DeviceGroupBody,
        hlc: u64,
    ) -> Result<Vec<u8>> {
        use prost::Message as _;

        let proto_body = body.into_proto();
        let payload = lakko_proto::mls_app::devicegroup::v1::DeviceGroupPayload {
            hlc,
            sender_leaf: self.inner.own_leaf_index().u32(),
            body: Some(proto_body),
        };
        let mut buf = Vec::with_capacity(payload.encoded_len());
        payload
            .encode(&mut buf)
            .map_err(|e| Error::Codec(format!("DeviceGroupPayload encode: {e}")))?;
        let mls_out = self.inner.create_application_message(identity, &buf)?;
        crate::group::encode_message(&mls_out)
    }

    /// Decode a `DeviceGroupPayload` from the bytes
    /// [`Group::process_incoming`] surfaced as
    /// [`Incoming::Application`]. Static rather than method on
    /// `&self` because the receiver-side flow goes
    /// `process_incoming → Incoming::Application(bytes) →
    /// parse_devicegroup_payload(&bytes)`, with no `DeviceGroup`
    /// state to consult.
    ///
    /// # Errors
    ///
    /// - [`Error::Codec`] when the proto decode fails.
    /// - [`Error::Unsupported`] when the payload's `body` is
    ///   absent or a variant this build doesn't recognise (an
    ///   older client reading a future-version payload).
    pub fn parse_devicegroup_payload(bytes: &[u8]) -> Result<ParsedDeviceGroupPayload> {
        use prost::Message as _;

        let payload = lakko_proto::mls_app::devicegroup::v1::DeviceGroupPayload::decode(bytes)
            .map_err(|e| Error::Codec(format!("DeviceGroupPayload decode: {e}")))?;
        let body = payload
            .body
            .ok_or(Error::Unsupported("DeviceGroupPayload missing body"))?;
        let parsed_body = DeviceGroupBody::from_proto(body);
        Ok(ParsedDeviceGroupPayload {
            hlc: payload.hlc,
            sender_leaf: payload.sender_leaf,
            body: parsed_body,
        })
    }
}

/// Parsed payload returned by
/// [`DeviceGroup::parse_devicegroup_payload`]. Wraps the typed
/// [`DeviceGroupBody`] with the envelope metadata callers
/// frequently want (HLC for ordering, sender leaf for
/// originator identification).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDeviceGroupPayload {
    /// Hybrid logical clock timestamp from the envelope.
    pub hlc: u64,
    /// Leaf index of the device that authored this payload.
    /// Stable inside an epoch; can renumber across a
    /// remove/add cycle, so callers MUST NOT compare across
    /// epochs.
    pub sender_leaf: u32,
    /// Concrete payload — one of `ReadCursor` / `DraftUpdate`
    /// / `ThreadFlags` / `ConversationOrder`.
    pub body: DeviceGroupBody,
}

/// Typed body of a DeviceGroup application payload. Wraps the
/// generated `DeviceGroupPayload.body` `oneof` with a Rust-side
/// enum so call sites get exhaustive `match` checking instead
/// of having to remember to handle a `None` proto variant.
///
/// Conversion methods [`DeviceGroupBody::into_proto`] and
/// [`DeviceGroupBody::from_proto`] are the only places that
/// touch the generated proto types — keeping the protobuf
/// shape contained inside the conversion functions means the
/// rest of `lakko-mls` (and downstream callers via FFI) sees
/// a plain Rust enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceGroupBody {
    /// "I read up to this point in this thread." Other
    /// devices use the last-write-wins (by HLC) cursor to
    /// compute their own per-thread unread count.
    ReadCursor {
        /// The conversation group's MLS group_id.
        group_id: Vec<u8>,
        /// The message id (mls-app HLC bytes) the sender
        /// claims to have read up to.
        up_to_message_id: Vec<u8>,
        /// Wall-clock UNIX millis when the read happened.
        /// Display only — never used for ordering.
        read_at_ms: u64,
    },
    /// In-flight composer state for a single thread. Empty
    /// `text` clears the draft (receivers wipe their local
    /// copy).
    Draft {
        /// The conversation group_id the draft belongs to.
        group_id: Vec<u8>,
        /// Composer text. UTF-8, may be empty.
        text: String,
        /// When non-empty, the draft was started as a reply
        /// to this message.
        reply_to_id: Vec<u8>,
    },
    /// Per-thread UI flags (last-write-wins snapshot of the
    /// whole struct).
    Flags {
        /// The conversation group_id the flags apply to.
        group_id: Vec<u8>,
        /// Pinned threads stay at the top of the list.
        pinned: bool,
        /// Muted threads suppress push notifications +
        /// unread indicators.
        muted: bool,
        /// Archived threads drop out of the main list but
        /// remain searchable.
        archived: bool,
        /// Manual "unread" override — user marked the thread
        /// unread after having read it.
        unread_override: bool,
    },
    /// Explicit display ordering for the conversation list.
    /// Threads listed here render in the given order; threads
    /// not mentioned fall back to recency-of-last-message.
    Order {
        /// Conversation group_ids in display order,
        /// top-to-bottom.
        group_ids: Vec<Vec<u8>>,
    },
}

impl DeviceGroupBody {
    fn into_proto(self) -> lakko_proto::mls_app::devicegroup::v1::device_group_payload::Body {
        use lakko_proto::mls_app::devicegroup::v1::{
            device_group_payload::Body, ConversationOrder, DraftUpdate, ReadCursor, ThreadFlags,
        };
        match self {
            Self::ReadCursor {
                group_id,
                up_to_message_id,
                read_at_ms,
            } => Body::ReadCursor(ReadCursor {
                group_id,
                up_to_message_id,
                read_at_ms,
            }),
            Self::Draft {
                group_id,
                text,
                reply_to_id,
            } => Body::Draft(DraftUpdate {
                group_id,
                text,
                reply_to_id,
            }),
            Self::Flags {
                group_id,
                pinned,
                muted,
                archived,
                unread_override,
            } => Body::Flags(ThreadFlags {
                group_id,
                pinned,
                muted,
                archived,
                unread_override,
            }),
            Self::Order { group_ids } => Body::Order(ConversationOrder { group_ids }),
        }
    }

    /// Infallible — every proto `Body` variant maps onto a
    /// concrete `Self` case. A future proto extension that adds
    /// a variant we don't recognise would surface earlier as a
    /// prost-decode failure, not here.
    fn from_proto(body: lakko_proto::mls_app::devicegroup::v1::device_group_payload::Body) -> Self {
        use lakko_proto::mls_app::devicegroup::v1::device_group_payload::Body;
        match body {
            Body::ReadCursor(b) => Self::ReadCursor {
                group_id: b.group_id,
                up_to_message_id: b.up_to_message_id,
                read_at_ms: b.read_at_ms,
            },
            Body::Draft(b) => Self::Draft {
                group_id: b.group_id,
                text: b.text,
                reply_to_id: b.reply_to_id,
            },
            Body::Flags(b) => Self::Flags {
                group_id: b.group_id,
                pinned: b.pinned,
                muted: b.muted,
                archived: b.archived,
                unread_override: b.unread_override,
            },
            Body::Order(b) => Self::Order {
                group_ids: b.group_ids,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    fn fixture_account_id() -> AccountId {
        // Pinned to a known value so the deterministic group_id
        // can be re-derived independently in cross-module tests.
        let mut id = [0u8; 16];
        id[0] = 0x01;
        id[15] = 0xab;
        id
    }

    #[test]
    fn group_id_is_deterministic() {
        let account = fixture_account_id();
        let a = DeviceGroup::group_id_for(&account);
        let b = DeviceGroup::group_id_for(&account);
        assert_eq!(a, b);
        assert_eq!(a.len(), 32, "BLAKE3 output is 32 bytes");
    }

    #[test]
    fn group_id_differs_across_accounts() {
        let mut a = fixture_account_id();
        let mut b = fixture_account_id();
        b[0] = 0x02;
        assert_ne!(DeviceGroup::group_id_for(&a), DeviceGroup::group_id_for(&b),);
        // And the domain tag actually contributes — flipping
        // a byte in the account_id flips the hash, but flipping
        // back makes them equal again.
        a[0] = 0x02;
        assert_eq!(DeviceGroup::group_id_for(&a), DeviceGroup::group_id_for(&b),);
    }

    #[test]
    fn create_founder_uses_deterministic_group_id() {
        let account = fixture_account_id();
        let identity = Identity::generate("device-1").expect("generate identity");
        let group = DeviceGroup::create_founder(&identity, &account).expect("create founder");
        assert_eq!(group.group_id(), DeviceGroup::group_id_for(&account));
        assert_eq!(group.account_id(), &account);
    }

    #[test]
    fn two_devices_join_via_welcome() {
        let account = fixture_account_id();

        // Device A bootstraps the group.
        let id_a = Identity::generate("device-A").expect("generate A");
        let mut group_a = DeviceGroup::create_founder(&id_a, &account).expect("create founder");

        // Device B publishes a KeyPackage.
        let id_b = Identity::generate("device-B").expect("generate B");
        let kp_b = id_b.create_key_package().expect("create KP for B");

        // A adds B; produces (commit, welcome). A's view already
        // advanced via `merge_pending_commit` inside `add_device`.
        let addition = group_a.add_device(&id_a, kp_b).expect("add device B");

        // B joins from the welcome.
        let welcome_bytes =
            crate::group::encode_message(&addition.welcome).expect("serialise welcome");
        let group_b =
            DeviceGroup::join_from_welcome(&welcome_bytes, &id_b, &account).expect("B joins");

        // Both views agree on the group_id.
        assert_eq!(group_a.group_id(), group_b.group_id());
        assert_eq!(group_a.account_id(), group_b.account_id());

        // And both see 2 leaves.
        let devices_a = group_a.devices();
        let devices_b = group_b.devices();
        assert_eq!(devices_a.len(), 2);
        assert_eq!(devices_b.len(), 2);
    }

    #[test]
    fn join_rejects_welcome_for_a_different_account() {
        let account_a = fixture_account_id();
        let mut account_b = fixture_account_id();
        account_b[0] ^= 0xFF;

        let id_a = Identity::generate("A").unwrap();
        let mut group_a = DeviceGroup::create_founder(&id_a, &account_a).unwrap();

        let id_b = Identity::generate("B").unwrap();
        let kp_b = id_b.create_key_package().unwrap();
        let addition = group_a.add_device(&id_a, kp_b).unwrap();

        let welcome_bytes = crate::group::encode_message(&addition.welcome).unwrap();

        // B tries to join under the *wrong* account_id — must be
        // rejected before any state advance.
        match DeviceGroup::join_from_welcome(&welcome_bytes, &id_b, &account_b) {
            Ok(_) => panic!("join_from_welcome should have rejected the mismatched account"),
            Err(Error::Group(msg)) => {
                assert!(
                    msg.contains("group_id mismatch"),
                    "unexpected error body: {msg}"
                );
            },
            Err(other) => panic!("expected Group error, got {other:?}"),
        }
    }

    /// Phase 2a3e1 — end-to-end: device A composes a
    /// `ReadCursor` payload, the wire bytes round-trip through
    /// the MLS application-message path, and device B parses
    /// the inbound payload back into the same typed body.
    #[test]
    fn devicegroup_application_payload_round_trips_a_to_b() {
        let account = fixture_account_id();

        // A bootstraps; B joins via Welcome.
        let id_a = Identity::generate("device-A").unwrap();
        let mut group_a = DeviceGroup::create_founder(&id_a, &account).unwrap();
        let id_b = Identity::generate("device-B").unwrap();
        let kp_b = id_b.create_key_package().unwrap();
        let addition = group_a.add_device(&id_a, kp_b).unwrap();
        let welcome_bytes = crate::group::encode_message(&addition.welcome).unwrap();
        let mut group_b = DeviceGroup::join_from_welcome(&welcome_bytes, &id_b, &account).unwrap();

        // A sends a typed ReadCursor payload.
        let body = DeviceGroupBody::ReadCursor {
            group_id: vec![0xAB; 32],
            up_to_message_id: vec![0xCD; 8],
            read_at_ms: 1_700_000_000_001,
        };
        let wire = group_a
            .create_devicegroup_message(&id_a, body.clone(), 12345)
            .expect("compose");

        // B receives + decrypts.
        let incoming = group_b
            .process_incoming(&id_b, &wire)
            .expect("process_incoming");
        let bytes = match incoming {
            Incoming::Application(b) => b,
            other => panic!("expected Application, got {other:?}"),
        };

        // B parses the typed payload back out.
        let parsed = DeviceGroup::parse_devicegroup_payload(&bytes).expect("parse");
        assert_eq!(parsed.hlc, 12345);
        // sender_leaf is A's leaf — index 0 in this 2-leaf
        // group where A is the founder.
        assert_eq!(parsed.sender_leaf, 0);
        assert_eq!(parsed.body, body);
    }

    #[test]
    fn remove_device_at_rejects_own_leaf() {
        let account = fixture_account_id();
        let id_a = Identity::generate("device-A").unwrap();
        let mut group_a = DeviceGroup::create_founder(&id_a, &account).unwrap();
        let own = group_a.own_leaf_index();
        match group_a.remove_device_at(&id_a, own) {
            Ok(_) => panic!("remove_device_at must reject own leaf"),
            Err(Error::Group(msg)) => {
                assert!(msg.contains("own leaf"), "unexpected body: {msg}");
            },
            Err(other) => panic!("expected Group error, got {other:?}"),
        }
    }

    // ADR-0015 sprint-4 spike: can OpenMLS 0.8 carry a CUSTOM GroupContext
    // extension (the DeviceGroup "require attested removals" policy flag) and
    // does a joining member see it? GroupContext is shared authenticated
    // state, so if creation accepts it and a joiner reconstructs it, peers
    // can enforce the policy. This is a feasibility test kept as a regression
    // guard on the OpenMLS API the policy flag will rely on.
    #[test]
    fn spike_custom_group_context_extension_propagates_to_joiners() {
        use openmls::extensions::{Extension, Extensions, ExtensionType, UnknownExtension};
        use openmls::framing::{MlsMessageBodyIn, MlsMessageIn};
        use openmls::group::{GroupId, MlsGroupJoinConfig, StagedWelcome};
        use openmls::key_packages::KeyPackage;
        use openmls::prelude::Capabilities;
        use tls_codec::{Deserialize as _, Serialize as _};

        // 0xFF01 is outside OpenMLS's recognised extension types -> it is
        // surfaced as `Extension::Unknown`, a private-use custom extension.
        const POLICY_EXT: u16 = 0xFF01;

        let account = fixture_account_id();
        let id_a = Identity::generate("A").unwrap();
        let id_b = Identity::generate("B").unwrap();

        // Leaf capabilities must advertise the custom extension type so
        // OpenMLS accepts a group / leaf node that carries it.
        let cs = id_a.ciphersuite();
        let caps = Capabilities::new(
            None,
            Some(&[cs]),
            Some(&[ExtensionType::Unknown(POLICY_EXT)]),
            None,
            None,
        );

        let gc_exts = Extensions::single(Extension::Unknown(
            POLICY_EXT,
            UnknownExtension(vec![0x01]),
        ))
        .unwrap();

        let cfg = MlsGroupCreateConfig::builder()
            .ciphersuite(cs)
            .use_ratchet_tree_extension(true)
            .capabilities(caps.clone())
            .with_group_context_extensions(gc_exts)
            .build();

        let group_id = DeviceGroup::group_id_for(&account);
        let mut group_a = MlsGroup::new_with_group_id(
            &id_a.provider,
            &id_a.signature_keys,
            &cfg,
            GroupId::from_slice(&group_id),
            id_a.credential.clone(),
        )
        .expect("create group A with a custom GroupContext extension");

        // (1) The founder reads its own policy flag back.
        let on_a = group_a
            .extensions()
            .unknown(POLICY_EXT)
            .expect("policy ext present on A");
        assert_eq!(on_a.0, vec![0x01]);

        // (2) B's KeyPackage advertises the same capability.
        let kp_b = KeyPackage::builder()
            .leaf_node_capabilities(caps)
            .build(cs, &id_b.provider, &id_b.signature_keys, id_b.credential.clone())
            .expect("B key package")
            .key_package()
            .clone();

        // (3) Add B, then merge the commit on A.
        let (_commit, welcome, _gi) = group_a
            .add_members(&id_a.provider, &id_a.signature_keys, &[kp_b])
            .expect("add B");
        group_a
            .merge_pending_commit(&id_a.provider)
            .expect("merge pending commit");

        // (4) B joins from the Welcome.
        let welcome_bytes = welcome.tls_serialize_detached().expect("serialize welcome");
        let mls_in = MlsMessageIn::tls_deserialize(&mut welcome_bytes.as_slice()).unwrap();
        let MlsMessageBodyIn::Welcome(welcome_in) = mls_in.extract() else {
            panic!("expected a Welcome message");
        };
        let join_cfg = MlsGroupJoinConfig::builder()
            .use_ratchet_tree_extension(true)
            .build();
        let group_b = StagedWelcome::new_from_welcome(&id_b.provider, &join_cfg, welcome_in, None)
            .expect("staged welcome")
            .into_group(&id_b.provider)
            .expect("B joins");

        // (5) B sees the SAME policy flag -> the GroupContext extension
        // propagates to joiners, so peers can enforce the policy.
        let on_b = group_b
            .extensions()
            .unknown(POLICY_EXT)
            .expect("policy ext present on B");
        assert_eq!(on_b.0, vec![0x01], "joiner must see the GroupContext policy flag");
    }

    #[test]
    fn device_group_default_has_no_removal_policy() {
        // A fresh DeviceGroup (OPAQUE-only account, no passkey) must NOT
        // require attested removals — peers cannot demand an attestation the
        // account has no passkey to produce.
        let account = fixture_account_id();
        let id_a = Identity::generate("A").unwrap();
        let group_a = DeviceGroup::create_founder(&id_a, &account).unwrap();
        assert!(!group_a.require_attested_removals());
    }

    #[test]
    fn set_require_attested_removals_propagates_to_a_joined_device() {
        let account = fixture_account_id();

        let id_a = Identity::generate("device-A").unwrap();
        let mut group_a = DeviceGroup::create_founder(&id_a, &account).unwrap();

        // B joins via Welcome.
        let id_b = Identity::generate("device-B").unwrap();
        let kp_b = id_b.create_key_package().unwrap();
        let addition = group_a.add_device(&id_a, kp_b).unwrap();
        let welcome_bytes = crate::group::encode_message(&addition.welcome).unwrap();
        let mut group_b =
            DeviceGroup::join_from_welcome(&welcome_bytes, &id_b, &account).unwrap();

        // Both start policy-off.
        assert!(!group_a.require_attested_removals());
        assert!(!group_b.require_attested_removals());

        // A flips the policy on and broadcasts the commit.
        let commit = group_a.set_require_attested_removals(&id_a).unwrap();
        assert!(
            group_a.require_attested_removals(),
            "setter must update the founder's own view"
        );

        // B applies the policy commit and now agrees.
        let incoming = group_b.process_incoming(&id_b, &commit).unwrap();
        assert!(matches!(incoming, Incoming::Commit));
        assert!(
            group_b.require_attested_removals(),
            "the policy flag must propagate through the GroupContext to B"
        );
    }

    // ---- ADR-0015 sprint 3: apply-path attestation verification ----

    use lakko_attest::ActionAttestation;
    use lakko_crypto::signing::SigningKey;

    /// Fixed seed for the test attestation signing key.
    const REMOVAL_ATTEST_SEED: [u8; 32] = [0x55; 32];

    fn make_attestation(account: &AccountId, action_hash: [u8; 32], sk: &SigningKey) -> Vec<u8> {
        ActionAttestation::new(*account, action_hash, 10_000)
            .sign(sk)
            .to_vec()
    }

    /// Build a 3-device DeviceGroup (A founder + B + C), flip the
    /// attested-removal policy ON, sync the verifying peer B, then have A
    /// remove C. Returns `(id_b, group_b, encoded_remove_commit, c_pubkey,
    /// account)`; the caller drives `process_incoming_verifying_removals` on
    /// B with chosen attestations. B sits at the pre-remove epoch, policy-on.
    fn attested_removal_fixture() -> (Identity, DeviceGroup, Vec<u8>, Vec<u8>, AccountId) {
        let account = fixture_account_id();
        let id_a = Identity::generate("A").unwrap();
        let mut group_a = DeviceGroup::create_founder(&id_a, &account).unwrap();

        let id_b = Identity::generate("B").unwrap();
        let kp_b = id_b.create_key_package().unwrap();
        let add_b = group_a.add_device(&id_a, kp_b).unwrap();
        let mut group_b = DeviceGroup::join_from_welcome(
            &crate::group::encode_message(&add_b.welcome).unwrap(),
            &id_b,
            &account,
        )
        .unwrap();

        let id_c = Identity::generate("C").unwrap();
        let kp_c = id_c.create_key_package().unwrap();
        let add_c = group_a.add_device(&id_a, kp_c).unwrap();
        // B advances past the add-C commit so it stays in lockstep with A.
        group_b
            .process_incoming(&id_b, &crate::group::encode_message(&add_c.commit).unwrap())
            .unwrap();

        // Flip the policy on; B applies it.
        let commit_policy = group_a.set_require_attested_removals(&id_a).unwrap();
        group_b.process_incoming(&id_b, &commit_policy).unwrap();
        assert!(group_b.require_attested_removals());

        // C's leaf + signature pubkey in A's view.
        let c_pubkey = id_c.signature_keys.public().to_vec();
        let c_leaf = group_a
            .devices()
            .into_iter()
            .find(|m| m.signature_public_key == c_pubkey)
            .expect("A sees C's leaf")
            .leaf_index;

        // A removes C.
        let commit_remove = group_a.remove_device_at(&id_a, c_leaf).unwrap();

        (id_b, group_b, commit_remove, c_pubkey, account)
    }

    fn b_still_has(group_b: &DeviceGroup, pubkey: &[u8]) -> bool {
        group_b
            .devices()
            .iter()
            .any(|m| m.signature_public_key == pubkey)
    }

    #[test]
    fn attested_removal_applies_with_a_valid_attestation() {
        let (id_b, mut group_b, commit_remove, c_pubkey, account) = attested_removal_fixture();
        let sk = SigningKey::from_seed(&REMOVAL_ATTEST_SEED);
        let att = make_attestation(&account, remove_device_action_hash(&c_pubkey), &sk);

        let incoming = group_b
            .process_incoming_verifying_removals(
                &id_b,
                &commit_remove,
                &[att],
                &sk.verifying_key(),
                100,
            )
            .expect("a valid attestation authorises the removal");
        assert!(matches!(incoming, Incoming::Commit));
        assert!(!b_still_has(&group_b, &c_pubkey), "C must be removed from B's view");
    }

    #[test]
    fn attested_removal_refused_without_attestation() {
        let (id_b, mut group_b, commit_remove, c_pubkey, _account) = attested_removal_fixture();
        let sk = SigningKey::from_seed(&REMOVAL_ATTEST_SEED);

        let res = group_b.process_incoming_verifying_removals(
            &id_b,
            &commit_remove,
            &[],
            &sk.verifying_key(),
            100,
        );
        assert!(res.is_err(), "a removal with no attestation must be refused under the policy");
        assert!(
            b_still_has(&group_b, &c_pubkey),
            "the refused commit must NOT be merged — C stays present"
        );
    }

    #[test]
    fn attested_removal_refused_for_a_different_leaf() {
        let (id_b, mut group_b, commit_remove, c_pubkey, account) = attested_removal_fixture();
        let sk = SigningKey::from_seed(&REMOVAL_ATTEST_SEED);
        // Attestation bound to a DIFFERENT pubkey than the one being removed.
        let att = make_attestation(
            &account,
            remove_device_action_hash(b"a-different-leaf-signature-key"),
            &sk,
        );

        let res = group_b.process_incoming_verifying_removals(
            &id_b,
            &commit_remove,
            &[att],
            &sk.verifying_key(),
            100,
        );
        assert!(res.is_err(), "an attestation for another leaf must not authorise this removal");
        assert!(b_still_has(&group_b, &c_pubkey));
    }

    #[test]
    fn attested_removal_refused_with_a_forged_signing_key() {
        let (id_b, mut group_b, commit_remove, c_pubkey, account) = attested_removal_fixture();
        let real = SigningKey::from_seed(&REMOVAL_ATTEST_SEED);
        let forged = SigningKey::from_seed(&[0x66; 32]);
        // Correct binding, but signed by the wrong key.
        let att = make_attestation(&account, remove_device_action_hash(&c_pubkey), &forged);

        let res = group_b.process_incoming_verifying_removals(
            &id_b,
            &commit_remove,
            &[att],
            &real.verifying_key(),
            100,
        );
        assert!(res.is_err(), "an attestation signed by the wrong key must be refused");
        assert!(b_still_has(&group_b, &c_pubkey));
    }

    #[test]
    fn unattested_removal_applies_when_policy_is_off() {
        // Same shape as the fixture but the policy is never flipped on.
        let account = fixture_account_id();
        let id_a = Identity::generate("A").unwrap();
        let mut group_a = DeviceGroup::create_founder(&id_a, &account).unwrap();
        let id_b = Identity::generate("B").unwrap();
        let kp_b = id_b.create_key_package().unwrap();
        let add_b = group_a.add_device(&id_a, kp_b).unwrap();
        let mut group_b = DeviceGroup::join_from_welcome(
            &crate::group::encode_message(&add_b.welcome).unwrap(),
            &id_b,
            &account,
        )
        .unwrap();
        let id_c = Identity::generate("C").unwrap();
        let kp_c = id_c.create_key_package().unwrap();
        let add_c = group_a.add_device(&id_a, kp_c).unwrap();
        group_b
            .process_incoming(&id_b, &crate::group::encode_message(&add_c.commit).unwrap())
            .unwrap();
        assert!(!group_b.require_attested_removals());

        let c_pubkey = id_c.signature_keys.public().to_vec();
        let c_leaf = group_a
            .devices()
            .into_iter()
            .find(|m| m.signature_public_key == c_pubkey)
            .unwrap()
            .leaf_index;
        let commit_remove = group_a.remove_device_at(&id_a, c_leaf).unwrap();

        // No attestation supplied, yet the removal applies (policy off).
        let sk = SigningKey::from_seed(&REMOVAL_ATTEST_SEED);
        let incoming = group_b
            .process_incoming_verifying_removals(
                &id_b,
                &commit_remove,
                &[],
                &sk.verifying_key(),
                100,
            )
            .expect("policy off -> no attestation required");
        assert!(matches!(incoming, Incoming::Commit));
        assert!(!b_still_has(&group_b, &c_pubkey));
    }

    #[test]
    fn non_remove_commit_needs_no_attestation_under_policy() {
        // Policy ON, but the incoming commit is an ADD (no removals), so it
        // must apply without any attestation.
        let account = fixture_account_id();
        let id_a = Identity::generate("A").unwrap();
        let mut group_a = DeviceGroup::create_founder(&id_a, &account).unwrap();
        let id_b = Identity::generate("B").unwrap();
        let kp_b = id_b.create_key_package().unwrap();
        let add_b = group_a.add_device(&id_a, kp_b).unwrap();
        let mut group_b = DeviceGroup::join_from_welcome(
            &crate::group::encode_message(&add_b.welcome).unwrap(),
            &id_b,
            &account,
        )
        .unwrap();
        let commit_policy = group_a.set_require_attested_removals(&id_a).unwrap();
        group_b.process_incoming(&id_b, &commit_policy).unwrap();
        assert!(group_b.require_attested_removals());

        // A adds a third device D — an Add commit, no removals.
        let id_d = Identity::generate("D").unwrap();
        let kp_d = id_d.create_key_package().unwrap();
        let add_d = group_a.add_device(&id_a, kp_d).unwrap();

        let sk = SigningKey::from_seed(&REMOVAL_ATTEST_SEED);
        let incoming = group_b
            .process_incoming_verifying_removals(
                &id_b,
                &crate::group::encode_message(&add_d.commit).unwrap(),
                &[],
                &sk.verifying_key(),
                100,
            )
            .expect("a non-remove commit needs no attestation");
        assert!(matches!(incoming, Incoming::Commit));
        assert_eq!(group_b.devices().len(), 3, "D must have been added on B's view");
    }

    #[test]
    fn remove_device_at_rejects_unknown_leaf() {
        let account = fixture_account_id();
        let id_a = Identity::generate("device-A").unwrap();
        let mut group_a = DeviceGroup::create_founder(&id_a, &account).unwrap();
        match group_a.remove_device_at(&id_a, 999_999) {
            Ok(_) => panic!("remove_device_at must reject an unknown leaf"),
            Err(Error::Group(msg)) => {
                assert!(
                    msg.contains("no such device leaf"),
                    "unexpected body: {msg}"
                );
            },
            Err(other) => panic!("expected Group error, got {other:?}"),
        }
    }

    /// A→B pairing, then A revokes B. The returned commit bytes
    /// decode cleanly and B's `process_incoming` advances B's
    /// view past its own removal.
    #[test]
    fn remove_device_at_revokes_paired_device() {
        let account = fixture_account_id();
        let id_a = Identity::generate("device-A").unwrap();
        let mut group_a = DeviceGroup::create_founder(&id_a, &account).unwrap();
        let id_b = Identity::generate("device-B").unwrap();
        let kp_b = id_b.create_key_package().unwrap();
        let addition = group_a.add_device(&id_a, kp_b).unwrap();
        let welcome_bytes = crate::group::encode_message(&addition.welcome).unwrap();
        let mut group_b = DeviceGroup::join_from_welcome(&welcome_bytes, &id_b, &account).unwrap();

        // Find B's leaf index in A's view (NOT B's own_leaf_index
        // — both views happen to agree on leaf 1 for B, but the
        // production flow always queries A-side first).
        let own_a = group_a.own_leaf_index();
        let b_leaf_in_a = group_a
            .devices()
            .into_iter()
            .find(|m| m.leaf_index != own_a)
            .expect("a sees b's leaf")
            .leaf_index;

        // A removes B.
        let commit_bytes = group_a.remove_device_at(&id_a, b_leaf_in_a).unwrap();

        // A's view is already advanced (merge_pending_commit
        // ran inside remove_device_at); only A's own leaf
        // remains.
        let after_a = group_a.devices();
        assert_eq!(after_a.len(), 1);
        assert_eq!(after_a[0].leaf_index, own_a);

        // B receives the Commit and advances past its own
        // removal. OpenMLS surfaces a "removed self" outcome
        // as a normal Commit — the next state query reports
        // the leaf gone.
        let incoming = group_b.process_incoming(&id_b, &commit_bytes).unwrap();
        assert!(matches!(incoming, Incoming::Commit));
    }

    #[test]
    fn devicegroup_payload_each_variant_roundtrips() {
        // Smoke-test all four variants survive the
        // proto-encode + proto-decode trip via the typed
        // `DeviceGroupBody` wrappers. Doesn't need a real MLS
        // group — encode + decode round-trips the bytes
        // directly via the same conversion methods the
        // application-message helpers use.
        use prost::Message as _;

        let cases = vec![
            DeviceGroupBody::ReadCursor {
                group_id: vec![0x01; 32],
                up_to_message_id: vec![0xAB; 8],
                read_at_ms: 42,
            },
            DeviceGroupBody::Draft {
                group_id: vec![0x02; 32],
                text: "hello".into(),
                reply_to_id: vec![0x99; 8],
            },
            DeviceGroupBody::Flags {
                group_id: vec![0x03; 32],
                pinned: true,
                muted: false,
                archived: true,
                unread_override: false,
            },
            DeviceGroupBody::Order {
                group_ids: vec![vec![0xAA; 32], vec![0xBB; 32]],
            },
        ];
        for body in cases {
            let payload = lakko_proto::mls_app::devicegroup::v1::DeviceGroupPayload {
                hlc: 7,
                sender_leaf: 0,
                body: Some(body.clone().into_proto()),
            };
            let mut buf = Vec::new();
            payload.encode(&mut buf).unwrap();
            let parsed = DeviceGroup::parse_devicegroup_payload(&buf).unwrap();
            assert_eq!(parsed.hlc, 7);
            assert_eq!(parsed.sender_leaf, 0);
            assert_eq!(parsed.body, body);
        }
    }
}
