// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for the sealed call relay and the peer announcement
//! ([ADR-0078] section 2, `lakko_calls::relay`). Mirror of
//! `crates/lakko-ffi-wasm/src/call_relay.rs`.
//!
//! - [`sfu_room_for_call`] - the room a call lives in on the SFU, the value
//!   a client mints its admission token for and probes with.
//! - [`CallRelayKey`] - seals and opens every `signal-relay` payload of an
//!   SFU call under the key of the current call epoch.
//! - [`classify_call_relay_plaintext`] - what an opened payload is: a peer
//!   announcement or the camera on/off media state.
//! - [`compose_call_peer_announcement`] - this device's SFU peer id, signed
//!   with its MLS leaf key.
//! - [`verify_call_peer_announcement`] - what a receiver runs before it
//!   names a tile: the announcement must verify against a leaf of the
//!   call's group and claim the peer id the SFU stamped as the sender.
//!
//! [ADR-0078]: ../../../docs/adr/0078-servers-cannot-link-accounts.md

use std::sync::Arc;

use lakko_calls::relay::{self as core, RelayError, RelayPayload, CALL_ID_LEN};

use crate::{FfiError, Group, Identity};

fn relay_err(e: RelayError) -> FfiError {
    FfiError::CallRelay(format!("{}: {e}", e.code()))
}

fn call_id16(call_id: &[u8]) -> Result<[u8; CALL_ID_LEN], FfiError> {
    <[u8; CALL_ID_LEN]>::try_from(call_id).map_err(|_| {
        FfiError::CallRelay(format!(
            "callIdLength: a call id is 16 bytes, got {}",
            call_id.len()
        ))
    })
}

/// The SFU room of a call (ADR-0078 section 2): 64 lower-case hex
/// characters, `BLAKE3_derive_key("lakko/sfu/room/v1", call_id)`. The
/// admission token is minted for it and `GET /v1/rooms/{room}` probes it.
///
/// # Errors
///
/// [`FfiError::CallRelay`] `callIdLength` when `call_id` is not 16 bytes.
#[uniffi::export]
pub fn sfu_room_for_call(call_id: Vec<u8>) -> Result<String, FfiError> {
    Ok(lakko_sfu_proto::room_hex(&call_id16(&call_id)?))
}

/// The key the relay channel of one call epoch is sealed under.
#[derive(uniffi::Object)]
pub struct CallRelayKey {
    inner: core::RelayKey,
}

#[uniffi::export]
impl CallRelayKey {
    /// Derive the key from the 32-byte per-call seed of the current epoch
    /// (`group.exportSecret(identity, "lakko/call/v1", callId, 32)`).
    ///
    /// # Errors
    ///
    /// [`FfiError::CallRelay`] `seedLength` when the seed is not 32 bytes.
    #[uniffi::constructor]
    pub fn from_call_seed(seed: Vec<u8>) -> Result<Arc<Self>, FfiError> {
        let seed = <[u8; 32]>::try_from(seed.as_slice()).map_err(|_| {
            FfiError::CallRelay(format!(
                "seedLength: a call seed is 32 bytes, got {}",
                seed.len()
            ))
        })?;
        Ok(Arc::new(Self {
            inner: core::RelayKey::from_call_seed(&seed),
        }))
    }

    /// Seal a relay payload.
    ///
    /// # Errors
    ///
    /// [`FfiError::CallRelay`] `badLength` for a plaintext over the bound.
    pub fn seal(&self, plaintext: Vec<u8>) -> Result<Vec<u8>, FfiError> {
        self.inner.seal(&plaintext).map_err(relay_err)
    }

    /// Open a relay payload.
    ///
    /// # Errors
    ///
    /// [`FfiError::CallRelay`] `badLength`, `unknownVersion` or `notOpened`.
    pub fn open(&self, sealed: Vec<u8>) -> Result<Vec<u8>, FfiError> {
        self.inner.open(&sealed).map_err(relay_err)
    }
}

/// What an opened relay plaintext is.
#[derive(uniffi::Enum, Debug, Clone, PartialEq, Eq)]
pub enum CallRelayPayload {
    /// A peer announcement; verify it with
    /// [`verify_call_peer_announcement`].
    PeerAnnouncement {
        /// The whole announcement.
        announcement: Vec<u8>,
    },
    /// The sender's camera on/off state.
    MediaState {
        /// Whether the camera is on.
        camera_on: bool,
    },
    /// Anything else: drop it.
    Unknown,
}

/// Classify an opened relay plaintext.
#[uniffi::export]
#[must_use]
pub fn classify_call_relay_plaintext(plaintext: Vec<u8>) -> CallRelayPayload {
    match core::classify_relay_plaintext(&plaintext) {
        RelayPayload::PeerAnnouncement(announcement) => {
            CallRelayPayload::PeerAnnouncement { announcement }
        },
        RelayPayload::MediaState { camera_on } => CallRelayPayload::MediaState { camera_on },
        RelayPayload::Unknown => CallRelayPayload::Unknown,
    }
}

/// Compose this device's peer announcement: `peer_id` (the SFU's Welcome
/// `peer_id`) in call `call_id`, signed with `identity`'s leaf key. Seal it
/// with [`CallRelayKey::seal`] before relaying it.
///
/// # Errors
///
/// [`FfiError::CallRelay`] `callIdLength`, `malformed` (a bad peer id) or
/// `identityKeyMismatch`.
#[uniffi::export]
pub fn compose_call_peer_announcement(
    identity: Arc<Identity>,
    call_id: Vec<u8>,
    peer_id: String,
) -> Result<Vec<u8>, FfiError> {
    core::compose_peer_announcement(&identity.inner, &call_id16(&call_id)?, &peer_id)
        .map_err(relay_err)
}

/// A peer announcement that verified against the call's roster.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCallPeer {
    /// The SFU peer id.
    pub peer_id: String,
    /// The signer's leaf index at this epoch.
    pub leaf_index: u32,
    /// The signer's 32-byte leaf signature key.
    pub signature_key: Vec<u8>,
    /// The account key the leaf resolves to (ADR-0050): the person.
    pub account_key: Vec<u8>,
}

/// Verify a peer announcement against `group` (the call's MLS group: the
/// chat group of a group call, the call group of a link call).
/// `relay_from` is the `from` the SFU stamped on the relay frame.
///
/// # Errors
///
/// [`FfiError::CallRelay`] with `callIdLength`, a decode code, `wrongCall`,
/// `wrongPeer`, `notAMember`, `noAccount` or `badSignature`: drop the
/// announcement.
#[uniffi::export]
pub fn verify_call_peer_announcement(
    group: Arc<Group>,
    call_id: Vec<u8>,
    relay_from: String,
    announcement: Vec<u8>,
) -> Result<VerifiedCallPeer, FfiError> {
    let call_id = call_id16(&call_id)?;
    let g = group.inner.lock().map_err(|_| FfiError::LockPoisoned)?;
    let verified = core::verify_peer_announcement(&g, &call_id, &relay_from, &announcement)
        .map_err(relay_err)?;
    Ok(VerifiedCallPeer {
        peer_id: verified.peer_id,
        leaf_index: verified.leaf_index,
        signature_key: verified.signature_key.to_vec(),
        account_key: verified.account_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_is_the_core_derivation_and_refuses_a_short_id() {
        let room = sfu_room_for_call(vec![0x01; 16]).expect("room");
        assert_eq!(room, lakko_sfu_proto::room_hex(&[0x01; 16]));
        assert!(sfu_room_for_call(vec![0x01; 15]).is_err());
    }

    #[test]
    fn a_relay_payload_round_trips_and_classifies() {
        let key = CallRelayKey::from_call_seed(vec![7; 32]).expect("key");
        let state = lakko_contacts::media_state::compose_media_state(false);
        let sealed = key.seal(state).expect("seal");
        let opened = key.open(sealed).expect("open");
        assert_eq!(
            classify_call_relay_plaintext(opened),
            CallRelayPayload::MediaState { camera_on: false }
        );
        assert!(CallRelayKey::from_call_seed(vec![7; 31]).is_err());
    }
}
