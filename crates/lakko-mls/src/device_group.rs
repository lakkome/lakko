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

use openmls::framing::MlsMessageOut;
use openmls::group::{MlsGroup, MlsGroupCreateConfig};
use openmls::prelude::{GroupId, KeyPackage, LeafNodeIndex};

use crate::error::{Error, Result};
use crate::group::{Group, Incoming, MemberAddition, MemberRecord};
use crate::identity::Identity;

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
        let cfg = MlsGroupCreateConfig::builder()
            .ciphersuite(identity.ciphersuite())
            // Same posture as `Group::create` — the ratchet tree
            // travels inside Welcomes so a freshly-paired device
            // doesn't need an out-of-band tree fetch.
            .use_ratchet_tree_extension(true)
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

    /// List every device leaf currently in the group. Useful
    /// for rendering a "Your devices" management screen.
    #[must_use]
    pub fn devices(&self) -> Vec<MemberRecord> {
        self.inner.members()
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
}
