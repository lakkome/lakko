// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for the sealed call relay and the peer announcement
//! ([ADR-0078] section 2, `lakko_calls::relay`). Mirror of
//! `crates/lakko-ffi-uniffi/src/call_relay.rs`.
//!
//! - `sfuRoomForCall(callId)` - the room a call lives in on the SFU.
//! - `CallRelayKey.fromCallSeed(seed)` - seals and opens every
//!   `signal-relay` payload of an SFU call under the current epoch's key.
//! - `classifyCallRelayPlaintext(plaintext)` - a peer announcement, the
//!   camera on/off media state, or something to drop.
//! - `composeCallPeerAnnouncement(identity, callId, peerId)` - this device's
//!   SFU peer id, signed with its MLS leaf key.
//! - `verifyCallPeerAnnouncement(group, callId, relayFrom, announcement)` -
//!   the check a receiver runs before it names a tile.
//!
//! A refusal throws an `Error` whose `callRelayError` own property is the
//! frozen code (`badLength`, `unknownVersion`, `notOpened`, `wrongDomain`,
//! `malformed`, `wrongCall`, `wrongPeer`, `notAMember`, `noAccount`,
//! `badSignature`, `identityKeyMismatch`, `seal`, `callIdLength`,
//! `seedLength`).
//!
//! [ADR-0078]: ../../../docs/adr/0078-servers-cannot-link-accounts.md

use lakko_calls::relay::{self as core, RelayError, RelayPayload, CALL_ID_LEN};
use wasm_bindgen::prelude::*;

use crate::contacts::coded_error;
use crate::mls::{Group, Identity};

const PROPERTY: &str = "callRelayError";

fn relay_err(e: RelayError) -> JsValue {
    coded_error(&e.to_string(), PROPERTY, e.code())
}

fn call_id16(call_id: &[u8]) -> Result<[u8; CALL_ID_LEN], JsValue> {
    <[u8; CALL_ID_LEN]>::try_from(call_id).map_err(|_| {
        coded_error(
            &format!("a call id is 16 bytes, got {}", call_id.len()),
            PROPERTY,
            "callIdLength",
        )
    })
}

/// The SFU room of a call: 64 lower-case hex characters,
/// `BLAKE3_derive_key("lakko/sfu/room/v1", call_id)`.
///
/// # Errors
///
/// `callIdLength` when `callId` is not 16 bytes.
#[wasm_bindgen(js_name = sfuRoomForCall)]
pub fn sfu_room_for_call(call_id: &[u8]) -> Result<String, JsValue> {
    Ok(lakko_sfu_proto::room_hex(&call_id16(call_id)?))
}

/// The key the relay channel of one call epoch is sealed under.
#[wasm_bindgen]
pub struct CallRelayKey {
    inner: core::RelayKey,
}

#[wasm_bindgen]
impl CallRelayKey {
    /// Derive the key from the 32-byte per-call seed of the current epoch.
    ///
    /// # Errors
    ///
    /// `seedLength` when the seed is not 32 bytes.
    #[wasm_bindgen(js_name = fromCallSeed)]
    pub fn from_call_seed(seed: &[u8]) -> Result<CallRelayKey, JsValue> {
        let seed = <[u8; 32]>::try_from(seed).map_err(|_| {
            coded_error(
                &format!("a call seed is 32 bytes, got {}", seed.len()),
                PROPERTY,
                "seedLength",
            )
        })?;
        Ok(Self {
            inner: core::RelayKey::from_call_seed(&seed),
        })
    }

    /// Seal a relay payload.
    ///
    /// # Errors
    ///
    /// `badLength` for a plaintext over the bound.
    pub fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, JsValue> {
        self.inner.seal(plaintext).map_err(relay_err)
    }

    /// Open a relay payload.
    ///
    /// # Errors
    ///
    /// `badLength`, `unknownVersion` or `notOpened`.
    pub fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, JsValue> {
        self.inner.open(sealed).map_err(relay_err)
    }
}

/// What an opened relay plaintext is (flat: wasm-bindgen has no enums with
/// data).
#[wasm_bindgen]
pub struct CallRelayPayload {
    kind: &'static str,
    camera_on: bool,
    announcement: Vec<u8>,
}

#[wasm_bindgen]
impl CallRelayPayload {
    /// `"peer-announcement"`, `"media-state"` or `"unknown"`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn kind(&self) -> String {
        self.kind.to_owned()
    }

    /// For `"media-state"`: whether the camera is on.
    #[wasm_bindgen(getter, js_name = cameraOn)]
    #[must_use]
    pub fn camera_on(&self) -> bool {
        self.camera_on
    }

    /// For `"peer-announcement"`: the whole announcement.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn announcement(&self) -> Vec<u8> {
        self.announcement.clone()
    }
}

/// Classify an opened relay plaintext.
#[wasm_bindgen(js_name = classifyCallRelayPlaintext)]
#[must_use]
pub fn classify_call_relay_plaintext(plaintext: &[u8]) -> CallRelayPayload {
    match core::classify_relay_plaintext(plaintext) {
        RelayPayload::PeerAnnouncement(announcement) => CallRelayPayload {
            kind: "peer-announcement",
            camera_on: false,
            announcement,
        },
        RelayPayload::MediaState { camera_on } => CallRelayPayload {
            kind: "media-state",
            camera_on,
            announcement: Vec::new(),
        },
        RelayPayload::Unknown => CallRelayPayload {
            kind: "unknown",
            camera_on: false,
            announcement: Vec::new(),
        },
    }
}

/// Compose this device's peer announcement: `peerId` (the SFU Welcome's
/// `peer_id`) in call `callId`, signed with `identity`'s leaf key. Seal it
/// with `CallRelayKey.seal` before relaying it.
///
/// # Errors
///
/// `callIdLength`, `malformed` or `identityKeyMismatch`.
#[wasm_bindgen(js_name = composeCallPeerAnnouncement)]
pub fn compose_call_peer_announcement(
    identity: &Identity,
    call_id: &[u8],
    peer_id: &str,
) -> Result<Vec<u8>, JsValue> {
    core::compose_peer_announcement(identity.mls_identity(), &call_id16(call_id)?, peer_id)
        .map_err(relay_err)
}

/// A peer announcement that verified against the call's roster.
#[wasm_bindgen]
pub struct VerifiedCallPeer {
    peer_id: String,
    leaf_index: u32,
    signature_key: Vec<u8>,
    account_key: Vec<u8>,
}

#[wasm_bindgen]
impl VerifiedCallPeer {
    /// The SFU peer id.
    #[wasm_bindgen(getter, js_name = peerId)]
    #[must_use]
    pub fn peer_id(&self) -> String {
        self.peer_id.clone()
    }

    /// The signer's leaf index at this epoch.
    #[wasm_bindgen(getter, js_name = leafIndex)]
    #[must_use]
    pub fn leaf_index(&self) -> u32 {
        self.leaf_index
    }

    /// The signer's 32-byte leaf signature key.
    #[wasm_bindgen(getter, js_name = signatureKey)]
    #[must_use]
    pub fn signature_key(&self) -> Vec<u8> {
        self.signature_key.clone()
    }

    /// The account key the leaf resolves to: the person.
    #[wasm_bindgen(getter, js_name = accountKey)]
    #[must_use]
    pub fn account_key(&self) -> Vec<u8> {
        self.account_key.clone()
    }
}

/// Verify a peer announcement against `group` (the call's MLS group).
/// `relayFrom` is the `from` the SFU stamped on the relay frame.
///
/// # Errors
///
/// Any refusal (`wrongCall`, `wrongPeer`, `notAMember`, `noAccount`,
/// `badSignature`, a decode code, `callIdLength`): drop the announcement.
#[wasm_bindgen(js_name = verifyCallPeerAnnouncement)]
pub fn verify_call_peer_announcement(
    group: &Group,
    call_id: &[u8],
    relay_from: &str,
    announcement: &[u8],
) -> Result<VerifiedCallPeer, JsValue> {
    let call_id = call_id16(call_id)?;
    let g = group
        .mls_group()
        .lock()
        .map_err(|_| coded_error("group lock poisoned", PROPERTY, "lockPoisoned"))?;
    let verified = core::verify_peer_announcement(&g, &call_id, relay_from, announcement)
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
    fn classify_and_seal_round_trip() {
        let key = CallRelayKey {
            inner: core::RelayKey::from_call_seed(&[3; 32]),
        };
        let state = lakko_contacts::media_state::compose_media_state(true);
        let sealed = key.inner.seal(&state).expect("seal");
        let opened = key.inner.open(&sealed).expect("open");
        let payload = classify_call_relay_plaintext(&opened);
        assert_eq!(payload.kind, "media-state");
        assert!(payload.camera_on);
        assert_eq!(classify_call_relay_plaintext(b"x").kind, "unknown");
    }
}
