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
//! - `composeCallScreenShare`, `verifyCallScreenShare` and
//!   `CallScreenShareState` - the signed screen-share state and the
//!   one-share-per-call fold of ADR-0083 (`lakko_calls::screen_share`).
//! - `composeCallName`, `verifyCallName`, `callNameIsNewer` - the name a
//!   participant gives themself for one call (`lakko_calls::call_name`).
//! - `composeCallChat`, `verifyCallChat`, `checkCallChatText`,
//!   `clampCallChatText`, `callChatMaxTextBytes` - a message in the call's
//!   own chat (`lakko_calls::call_chat`, `docs/ux/calls.md` section 4.1.4).
//!
//! A refusal throws an `Error` whose `callRelayError` own property is the
//! frozen code (`badLength`, `unknownVersion`, `notOpened`, `wrongDomain`,
//! `malformed`, `wrongCall`, `wrongPeer`, `notAMember`, `noAccount`,
//! `badSignature`, `identityKeyMismatch`, `seal`, `callIdLength`,
//! `seedLength`).
//!
//! [ADR-0078]: ../../../docs/adr/0078-servers-cannot-link-accounts.md

use lakko_calls::call_chat as chat;
use lakko_calls::call_name as names;
use lakko_calls::relay::{self as core, RelayError, RelayPayload, CALL_ID_LEN};
use lakko_calls::screen_share::{self as share, ShareId, ShareStatus};
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
    microphone_on: bool,
    announcement: Vec<u8>,
    state: Vec<u8>,
    call_name: Vec<u8>,
    message: Vec<u8>,
}

#[wasm_bindgen]
impl CallRelayPayload {
    /// `"peer-announcement"`, `"media-state"`, `"screen-share"`,
    /// `"call-name"`, `"call-chat"` or `"unknown"`.
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

    /// For `"media-state"`: whether the microphone is on (`false` = muted).
    #[wasm_bindgen(getter, js_name = microphoneOn)]
    #[must_use]
    pub fn microphone_on(&self) -> bool {
        self.microphone_on
    }

    /// For `"peer-announcement"`: the whole announcement.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn announcement(&self) -> Vec<u8> {
        self.announcement.clone()
    }

    /// For `"screen-share"`: the whole share state (ADR-0083).
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn state(&self) -> Vec<u8> {
        self.state.clone()
    }

    /// For `"call-name"`: the whole call name; verify it with
    /// `verifyCallName`.
    #[wasm_bindgen(getter, js_name = callName)]
    #[must_use]
    pub fn call_name(&self) -> Vec<u8> {
        self.call_name.clone()
    }

    /// For `"call-chat"`: the whole message; verify it with
    /// `verifyCallChat`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn message(&self) -> Vec<u8> {
        self.message.clone()
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
            microphone_on: false,
            announcement,
            state: Vec::new(),
            call_name: Vec::new(),
            message: Vec::new(),
        },
        RelayPayload::MediaState {
            camera_on,
            microphone_on,
        } => CallRelayPayload {
            kind: "media-state",
            camera_on,
            microphone_on,
            announcement: Vec::new(),
            state: Vec::new(),
            call_name: Vec::new(),
            message: Vec::new(),
        },
        RelayPayload::ScreenShare(state) => CallRelayPayload {
            kind: "screen-share",
            camera_on: false,
            microphone_on: false,
            announcement: Vec::new(),
            state,
            call_name: Vec::new(),
            message: Vec::new(),
        },
        RelayPayload::CallName(call_name) => CallRelayPayload {
            kind: "call-name",
            camera_on: false,
            microphone_on: false,
            announcement: Vec::new(),
            state: Vec::new(),
            call_name,
            message: Vec::new(),
        },
        RelayPayload::CallChat(message) => CallRelayPayload {
            kind: "call-chat",
            camera_on: false,
            microphone_on: false,
            announcement: Vec::new(),
            state: Vec::new(),
            call_name: Vec::new(),
            message,
        },
        RelayPayload::Unknown => CallRelayPayload {
            kind: "unknown",
            camera_on: false,
            microphone_on: false,
            announcement: Vec::new(),
            state: Vec::new(),
            call_name: Vec::new(),
            message: Vec::new(),
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

fn key32(bytes: &[u8]) -> Result<[u8; 32], JsValue> {
    <[u8; 32]>::try_from(bytes).map_err(|_| {
        coded_error(
            &format!("a leaf key is 32 bytes, got {}", bytes.len()),
            PROPERTY,
            "keyLength",
        )
    })
}

const fn status_of(sharing: bool) -> ShareStatus {
    if sharing {
        ShareStatus::Sharing
    } else {
        ShareStatus::Stopped
    }
}

/// The peer id a one-to-one call's screen-share state carries (ADR-0083).
#[wasm_bindgen(js_name = callScreenShareOneToOnePeerId)]
#[must_use]
pub fn call_screen_share_one_to_one_peer_id() -> String {
    share::ONE_TO_ONE_PEER_ID.to_owned()
}

/// SFrame stream index of a screen share's picture (ADR-0083).
#[wasm_bindgen(js_name = sframeStreamScreen)]
#[must_use]
pub fn sframe_stream_screen() -> u8 {
    share::SFRAME_STREAM_SCREEN
}

/// SFrame stream index of a screen share's sound (ADR-0083).
#[wasm_bindgen(js_name = sframeStreamScreenAudio)]
#[must_use]
pub fn sframe_stream_screen_audio() -> u8 {
    share::SFRAME_STREAM_SCREEN_AUDIO
}

/// The stream-id suffix a share's tracks travel under (`~screen`).
#[wasm_bindgen(js_name = screenStreamSuffix)]
#[must_use]
pub fn screen_stream_suffix() -> String {
    share::SCREEN_STREAM_SUFFIX.to_owned()
}

/// Compose this device's screen-share state (ADR-0083): share `seq` in
/// call `callId` under `peerId` (the SFU peer id, or
/// `callScreenShareOneToOnePeerId()`), sharing or stopped, signed with
/// `identity`'s leaf key.
///
/// # Errors
///
/// `callIdLength`, `malformed` (a bad peer id or a seq of 0) or
/// `identityKeyMismatch`.
#[wasm_bindgen(js_name = composeCallScreenShare)]
pub fn compose_call_screen_share(
    identity: &Identity,
    call_id: &[u8],
    peer_id: &str,
    seq: u64,
    sharing: bool,
    with_sound: bool,
) -> Result<Vec<u8>, JsValue> {
    share::compose_screen_share(
        identity.mls_identity(),
        &call_id16(call_id)?,
        peer_id,
        seq,
        status_of(sharing),
        with_sound,
    )
    .map_err(relay_err)
}

/// A screen-share state that verified against the call's roster.
#[wasm_bindgen]
#[derive(Clone)]
pub struct VerifiedCallScreenShare {
    seq: u64,
    signer_key: Vec<u8>,
    peer_id: String,
    sharing: bool,
    with_sound: bool,
    account_key: Vec<u8>,
}

#[wasm_bindgen]
impl VerifiedCallScreenShare {
    /// The share's sequence number.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// The sharer's 32-byte leaf signature key.
    #[wasm_bindgen(getter, js_name = signerKey)]
    #[must_use]
    pub fn signer_key(&self) -> Vec<u8> {
        self.signer_key.clone()
    }

    /// The sharer's peer id.
    #[wasm_bindgen(getter, js_name = peerId)]
    #[must_use]
    pub fn peer_id(&self) -> String {
        self.peer_id.clone()
    }

    /// `true` while sharing, `false` for a stop.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn sharing(&self) -> bool {
        self.sharing
    }

    /// Whether the share carries sound.
    #[wasm_bindgen(getter, js_name = withSound)]
    #[must_use]
    pub fn with_sound(&self) -> bool {
        self.with_sound
    }

    /// The account key the signer's leaf resolves to: the person.
    #[wasm_bindgen(getter, js_name = accountKey)]
    #[must_use]
    pub fn account_key(&self) -> Vec<u8> {
        self.account_key.clone()
    }
}

/// Verify a screen-share state. `peerId` and `peerSignerKey` are what the
/// caller has bound to the sender: in a group call the relay `from` and the
/// signature key of that peer's verified announcement; in a one-to-one call
/// `callScreenShareOneToOnePeerId()` and the MLS sender's leaf key.
///
/// # Errors
///
/// Any refusal (`wrongCall`, `wrongPeer`, `notAMember`, `noAccount`,
/// `badSignature`, a decode code, `callIdLength`): drop the state.
#[wasm_bindgen(js_name = verifyCallScreenShare)]
pub fn verify_call_screen_share(
    group: &Group,
    call_id: &[u8],
    peer_id: &str,
    peer_signer_key: &[u8],
    state: &[u8],
) -> Result<VerifiedCallScreenShare, JsValue> {
    let call_id = call_id16(call_id)?;
    let g = group
        .mls_group()
        .lock()
        .map_err(|_| coded_error("group lock poisoned", PROPERTY, "lockPoisoned"))?;
    let v = share::verify_screen_share(&g, &call_id, peer_id, peer_signer_key, state)
        .map_err(relay_err)?;
    Ok(VerifiedCallScreenShare {
        seq: v.id.seq,
        signer_key: v.id.signer_key.to_vec(),
        peer_id: v.peer_id,
        sharing: v.status == ShareStatus::Sharing,
        with_sound: v.with_sound,
        account_key: v.account_key,
    })
}

/// Compose this device's name for the call: `name` (empty to go back to the
/// default naming) numbered `seq` (1, 2, 3, ... for the life of `peerId`)
/// for call `callId` under SFU `peerId`, signed with `identity`'s leaf key.
/// `name` must already be normalised (`normalizeDisplayName`). Seal it with
/// `CallRelayKey.seal`.
///
/// # Errors
///
/// `callIdLength`, `malformed` (a bad peer id, a seq of 0 or a bad name) or
/// `identityKeyMismatch`.
#[wasm_bindgen(js_name = composeCallName)]
pub fn compose_call_name(
    identity: &Identity,
    call_id: &[u8],
    peer_id: &str,
    seq: u64,
    name: &str,
) -> Result<Vec<u8>, JsValue> {
    names::compose_call_name(
        identity.mls_identity(),
        &call_id16(call_id)?,
        peer_id,
        seq,
        name,
    )
    .map_err(relay_err)
}

/// A call name that verified against the call's roster.
#[wasm_bindgen]
#[derive(Clone)]
pub struct VerifiedCallName {
    peer_id: String,
    seq: u64,
    name: Option<String>,
    account_key: Vec<u8>,
}

#[wasm_bindgen]
impl VerifiedCallName {
    /// The sender's peer id.
    #[wasm_bindgen(getter, js_name = peerId)]
    #[must_use]
    pub fn peer_id(&self) -> String {
        self.peer_id.clone()
    }

    /// The name's sequence number; keep the greatest per peer.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// The name, `undefined` when the sender went back to its default
    /// naming.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn name(&self) -> Option<String> {
        self.name.clone()
    }

    /// The account key the signer's leaf resolves to: the person.
    #[wasm_bindgen(getter, js_name = accountKey)]
    #[must_use]
    pub fn account_key(&self) -> Vec<u8> {
        self.account_key.clone()
    }
}

/// Verify a call name. `peerId` is the relay `from`, `peerSignerKey` the
/// signature key of that peer's verified announcement.
///
/// # Errors
///
/// Any refusal (`wrongCall`, `wrongPeer`, `notAMember`, `noAccount`,
/// `badSignature`, a decode code, `callIdLength`): drop it.
#[wasm_bindgen(js_name = verifyCallName)]
pub fn verify_call_name(
    group: &Group,
    call_id: &[u8],
    peer_id: &str,
    peer_signer_key: &[u8],
    name: &[u8],
) -> Result<VerifiedCallName, JsValue> {
    let call_id = call_id16(call_id)?;
    let g = group
        .mls_group()
        .lock()
        .map_err(|_| coded_error("group lock poisoned", PROPERTY, "lockPoisoned"))?;
    let v =
        names::verify_call_name(&g, &call_id, peer_id, peer_signer_key, name).map_err(relay_err)?;
    Ok(VerifiedCallName {
        peer_id: v.peer_id,
        seq: v.seq,
        name: v.name,
        account_key: v.account_key,
    })
}

/// Whether a call name numbered `incomingSeq` replaces the one held for the
/// same peer (`storedSeq`, `undefined` when none is held): strictly greater.
#[wasm_bindgen(js_name = callNameIsNewer)]
#[must_use]
pub fn call_name_is_newer(incoming_seq: u64, stored_seq: Option<u64>) -> bool {
    names::call_name_is_newer(incoming_seq, stored_seq)
}

/// The longest text of a call-chat message, in bytes.
#[wasm_bindgen(js_name = callChatMaxTextBytes)]
#[must_use]
pub fn call_chat_max_text_bytes() -> u32 {
    u32::try_from(chat::MAX_CALL_CHAT_TEXT_BYTES).unwrap_or(u32::MAX)
}

/// Why a text cannot be sent in the call chat: `undefined` when it can, else
/// `"empty"`, `"blank"`, `"tooLong"` or `"controlCharacter"`.
#[wasm_bindgen(js_name = checkCallChatText)]
#[must_use]
pub fn check_call_chat_text(text: &str) -> Option<String> {
    chat::check_call_chat_text(text)
        .err()
        .map(|e| e.code().to_owned())
}

/// The longest prefix of `text` a call-chat message can carry, cut on a
/// character boundary.
#[wasm_bindgen(js_name = clampCallChatText)]
#[must_use]
pub fn clamp_call_chat_text(text: &str) -> String {
    chat::clamp_call_chat_text(text).to_owned()
}

/// Compose a call-chat message: `text` numbered `seq` (1, 2, 3, ... for the
/// life of `peerId`) for call `callId` under SFU `peerId`, signed with
/// `identity`'s leaf key. Seal it with `CallRelayKey.seal`.
///
/// # Errors
///
/// `callIdLength`, `malformed` (a bad peer id, a seq of 0 or a text
/// `checkCallChatText` refuses) or `identityKeyMismatch`.
#[wasm_bindgen(js_name = composeCallChat)]
pub fn compose_call_chat(
    identity: &Identity,
    call_id: &[u8],
    peer_id: &str,
    seq: u64,
    text: &str,
) -> Result<Vec<u8>, JsValue> {
    chat::compose_call_chat(
        identity.mls_identity(),
        &call_id16(call_id)?,
        peer_id,
        seq,
        text,
    )
    .map_err(relay_err)
}

/// A call-chat message that verified against the call's roster.
#[wasm_bindgen]
#[derive(Clone)]
pub struct VerifiedCallChat {
    peer_id: String,
    seq: u64,
    text: String,
    account_key: Vec<u8>,
}

#[wasm_bindgen]
impl VerifiedCallChat {
    /// The sender's peer id.
    #[wasm_bindgen(getter, js_name = peerId)]
    #[must_use]
    pub fn peer_id(&self) -> String {
        self.peer_id.clone()
    }

    /// The message's sequence number; show a message once per
    /// `(peerId, seq)`.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// The text.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn text(&self) -> String {
        self.text.clone()
    }

    /// The account key the signer's leaf resolves to: the person.
    #[wasm_bindgen(getter, js_name = accountKey)]
    #[must_use]
    pub fn account_key(&self) -> Vec<u8> {
        self.account_key.clone()
    }
}

/// Verify a call-chat message. `peerId` is the relay `from`,
/// `peerSignerKey` the signature key of that peer's verified announcement.
///
/// # Errors
///
/// Any refusal (`wrongCall`, `wrongPeer`, `notAMember`, `noAccount`,
/// `badSignature`, a decode code, `callIdLength`): drop it.
#[wasm_bindgen(js_name = verifyCallChat)]
pub fn verify_call_chat(
    group: &Group,
    call_id: &[u8],
    peer_id: &str,
    peer_signer_key: &[u8],
    message: &[u8],
) -> Result<VerifiedCallChat, JsValue> {
    let call_id = call_id16(call_id)?;
    let g = group
        .mls_group()
        .lock()
        .map_err(|_| coded_error("group lock poisoned", PROPERTY, "lockPoisoned"))?;
    let v = chat::verify_call_chat(&g, &call_id, peer_id, peer_signer_key, message)
        .map_err(relay_err)?;
    Ok(VerifiedCallChat {
        peer_id: v.peer_id,
        seq: v.seq,
        text: v.text,
        account_key: v.account_key,
    })
}

/// The share a call is showing now.
#[wasm_bindgen]
pub struct CallCurrentScreenShare {
    seq: u64,
    signer_key: Vec<u8>,
    peer_id: String,
    with_sound: bool,
    account_key: Vec<u8>,
    is_own: bool,
}

#[wasm_bindgen]
impl CallCurrentScreenShare {
    /// The share's sequence number.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// The sharer's leaf signature key.
    #[wasm_bindgen(getter, js_name = signerKey)]
    #[must_use]
    pub fn signer_key(&self) -> Vec<u8> {
        self.signer_key.clone()
    }

    /// The sharer's peer id.
    #[wasm_bindgen(getter, js_name = peerId)]
    #[must_use]
    pub fn peer_id(&self) -> String {
        self.peer_id.clone()
    }

    /// Whether it carries sound.
    #[wasm_bindgen(getter, js_name = withSound)]
    #[must_use]
    pub fn with_sound(&self) -> bool {
        self.with_sound
    }

    /// The sharer's account key (empty for this device's own share).
    #[wasm_bindgen(getter, js_name = accountKey)]
    #[must_use]
    pub fn account_key(&self) -> Vec<u8> {
        self.account_key.clone()
    }

    /// Whether it is this device's own share.
    #[wasm_bindgen(getter, js_name = isOwn)]
    #[must_use]
    pub fn is_own(&self) -> bool {
        self.is_own
    }
}

/// The one-share-per-call fold of ADR-0083 (C-87), one per call.
#[wasm_bindgen]
pub struct CallScreenShareState {
    inner: share::ScreenShareState,
}

#[wasm_bindgen]
impl CallScreenShareState {
    /// A fold for a device whose leaf signature key is `ownKey`.
    ///
    /// # Errors
    ///
    /// `keyLength` when the key is not 32 bytes.
    #[wasm_bindgen(constructor)]
    pub fn new(own_key: &[u8]) -> Result<Self, JsValue> {
        Ok(Self {
            inner: share::ScreenShareState::new(key32(own_key)?),
        })
    }

    /// The seq a share this device starts now must carry.
    #[wasm_bindgen(js_name = nextStartSeq)]
    #[must_use]
    pub fn next_start_seq(&self) -> u64 {
        self.inner.next_start_seq()
    }

    /// Fold in a verified state from another participant. Returns whether
    /// the current share changed.
    ///
    /// # Errors
    ///
    /// `keyLength` for a signer key not 32 bytes.
    pub fn observe(&mut self, state: &VerifiedCallScreenShare) -> Result<bool, JsValue> {
        let verified = share::VerifiedShare {
            id: ShareId {
                seq: state.seq,
                signer_key: key32(&state.signer_key)?,
            },
            peer_id: state.peer_id.clone(),
            status: status_of(state.sharing),
            with_sound: state.with_sound,
            account_key: state.account_key.clone(),
        };
        Ok(self.inner.observe(&verified) == share::ShareChange::Changed)
    }

    /// Record that this device started share `seq`. Returns whether the
    /// current share changed.
    #[wasm_bindgen(js_name = startOwn)]
    pub fn start_own(&mut self, seq: u64, peer_id: &str, with_sound: bool) -> bool {
        self.inner.start_own(seq, peer_id, with_sound) == share::ShareChange::Changed
    }

    /// Record that this device stopped its share: the seq its stopped state
    /// must carry, or `undefined` when it had none.
    #[wasm_bindgen(js_name = stopOwn)]
    pub fn stop_own(&mut self) -> Option<u64> {
        self.inner.stop_own()
    }

    /// The seq of this device's share when another share superseded it:
    /// stop the capture, then `stopOwn()`.
    #[wasm_bindgen(js_name = ownSuperseded)]
    #[must_use]
    pub fn own_superseded(&self) -> Option<u64> {
        self.inner.own_superseded()
    }

    /// The participant whose leaf key is `signerKey` left the call.
    /// Returns whether the current share changed.
    #[wasm_bindgen(js_name = participantLeft)]
    pub fn participant_left(&mut self, signer_key: &[u8]) -> bool {
        self.inner.participant_left(signer_key) == share::ShareChange::Changed
    }

    /// The share to show, or `undefined`.
    #[must_use]
    pub fn current(&self) -> Option<CallCurrentScreenShare> {
        let is_own = self.inner.current_is_own();
        self.inner.current().map(|c| CallCurrentScreenShare {
            seq: c.id.seq,
            signer_key: c.id.signer_key.to_vec(),
            peer_id: c.peer_id,
            with_sound: c.with_sound,
            account_key: c.account_key,
            is_own,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_and_seal_round_trip() {
        let key = CallRelayKey {
            inner: core::RelayKey::from_call_seed(&[3; 32]),
        };
        let state = lakko_contacts::media_state::compose_media_state(true, false);
        let sealed = key.inner.seal(&state).expect("seal");
        let opened = key.inner.open(&sealed).expect("open");
        let payload = classify_call_relay_plaintext(&opened);
        assert_eq!(payload.kind, "media-state");
        assert!(payload.camera_on);
        assert!(!payload.microphone_on);
        assert_eq!(classify_call_relay_plaintext(b"x").kind, "unknown");
    }

    #[test]
    fn a_call_name_classifies_as_call_name() {
        let alice = lakko_mls::Identity::from_seed(&[1; 32]).expect("alice");
        let wire = names::compose_call_name(&alice, &[9; 16], "a", 1, "Anna").expect("compose");
        let payload = classify_call_relay_plaintext(&wire);
        assert_eq!(payload.kind, "call-name");
        assert_eq!(payload.call_name, wire);
        assert!(call_name_is_newer(1, None) && !call_name_is_newer(1, Some(1)));
    }

    #[test]
    fn a_call_chat_message_classifies_as_call_chat() {
        let alice = lakko_mls::Identity::from_seed(&[1; 32]).expect("alice");
        let wire = chat::compose_call_chat(&alice, &[9; 16], "a", 1, "hi").expect("compose");
        let payload = classify_call_relay_plaintext(&wire);
        assert_eq!(payload.kind, "call-chat");
        assert_eq!(payload.message, wire);
        assert!(payload.call_name.is_empty());
        assert_eq!(check_call_chat_text("hi"), None);
        assert_eq!(check_call_chat_text(" ").as_deref(), Some("blank"));
        assert_eq!(clamp_call_chat_text(&"я".repeat(500)).len(), 800);
        assert_eq!(call_chat_max_text_bytes(), 800);
    }
}
