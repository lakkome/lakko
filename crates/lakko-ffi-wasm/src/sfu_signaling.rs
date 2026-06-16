// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for the SFU signaling wire frames
//! ([`lakko_sfu_proto`]) — SFU multi-party arc, subsprint 1c.
//!
//! Mirror of the UniFFI surface in
//! [`crates/lakko-ffi-uniffi/src/sfu_signaling.rs`](../../../lakko-ffi-uniffi/src/sfu_signaling.rs),
//! adapted to the web client's idioms.
//!
//! ## Why the surface differs slightly from UniFFI
//!
//! The mobile (UniFFI) side gets a fully-typed `SfuServerFrame`
//! enum because UniFFI maps Rust enums-with-data + nested records
//! cleanly. On the web, JavaScript already has first-class
//! `JSON.parse` + TypeScript types, and `wasm-bindgen` does not
//! return Rust enum-with-data / `Vec<struct>` ergonomically. So the
//! web surface is the part where sharing actually buys safety:
//!
//! - **Compose** (`composeSfu*`) — build the exact wire JSON for
//!   each client → server frame from the shared definition, so the
//!   web cannot emit a malformed frame.
//! - **Validate** ([`validate_sfu_server_frame`]) — decode an
//!   inbound frame through the shared crate (enforces the size cap +
//!   fails fast on an unknown `kind`) and return its `kind`; the web
//!   then reads fields off the original JSON with a TS type.
//! - **Payload** base64url helpers for the opaque `SignalRelay`
//!   body.

use lakko_sfu_proto::{
    ClientFrame, RelayTarget, ServerFrame, decode_payload_b64, decode_server_frame,
    encode_client_frame, encode_payload_b64,
};
use wasm_bindgen::prelude::*;

fn to_err(e: lakko_sfu_proto::SfuProtoError) -> JsError {
    JsError::new(&e.to_string())
}

/// Validate an inbound server frame off the `lakko-sfu-v1`
/// WebSocket and return its `kind` discriminator (kebab-case:
/// `"welcome"`, `"peer-joined"`, `"peer-left"`, `"signal-relay"`,
/// `"error"`, `"pong"`, `"sdp-answer"`, `"sdp-offer"`). Throws on a
/// malformed / unknown-kind / oversize frame. The web reads the
/// frame's fields off the original JSON with a TypeScript type.
#[wasm_bindgen(js_name = "validateSfuServerFrame")]
pub fn validate_sfu_server_frame(json: &str) -> Result<String, JsError> {
    let kind = match decode_server_frame(json).map_err(to_err)? {
        ServerFrame::Welcome { .. } => "welcome",
        ServerFrame::PeerJoined { .. } => "peer-joined",
        ServerFrame::PeerLeft { .. } => "peer-left",
        ServerFrame::SignalRelay { .. } => "signal-relay",
        ServerFrame::Error { .. } => "error",
        ServerFrame::Pong { .. } => "pong",
        ServerFrame::SdpAnswer { .. } => "sdp-answer",
        ServerFrame::SdpOffer { .. } => "sdp-offer",
    };
    Ok(kind.to_string())
}

/// Re-derive the per-call peer tag (ADR-0021) for a `(call_id,
/// account_id)` pair — the same canonical derivation the SFU applies.
///
/// The web computes this over each of its contacts' account ids and
/// matches it against a roster entry's `peer_tag` (read off the frame
/// JSON) to resolve the entry to a display name. One-way (reveals
/// nothing about the account id) and per-call (keyed on `call_id`).
#[wasm_bindgen(js_name = "sfuPeerTag")]
#[must_use]
pub fn sfu_peer_tag(call_id: &str, account_id: &str) -> String {
    lakko_sfu_proto::peer_tag(call_id, account_id)
}

/// Compose a `SignalRelay` to a single peer. `payload` is the raw
/// opaque bytes (SDP / ICE / SFrame-key envelope); base64url-encoded
/// into the frame.
#[wasm_bindgen(js_name = "composeSfuSignalRelayToPeer")]
pub fn compose_sfu_signal_relay_to_peer(peer_id: &str, payload: &[u8]) -> Result<String, JsError> {
    let frame = ClientFrame::SignalRelay {
        to: RelayTarget::Peer {
            peer_id: peer_id.to_string(),
        },
        payload_b64: encode_payload_b64(payload),
    };
    encode_client_frame(&frame).map_err(to_err)
}

/// Compose a `SignalRelay` broadcast to every other peer in the call.
#[wasm_bindgen(js_name = "composeSfuSignalRelayToCall")]
pub fn compose_sfu_signal_relay_to_call(payload: &[u8]) -> Result<String, JsError> {
    let frame = ClientFrame::SignalRelay {
        to: RelayTarget::Call,
        payload_b64: encode_payload_b64(payload),
    };
    encode_client_frame(&frame).map_err(to_err)
}

/// Compose a client → SFU SDP offer.
#[wasm_bindgen(js_name = "composeSfuSdpOffer")]
pub fn compose_sfu_sdp_offer(sdp: &str) -> Result<String, JsError> {
    encode_client_frame(&ClientFrame::SdpOffer {
        sdp: sdp.to_string(),
    })
    .map_err(to_err)
}

/// Compose a client → SFU SDP answer (responds to a SFU-initiated
/// offer).
#[wasm_bindgen(js_name = "composeSfuSdpAnswer")]
pub fn compose_sfu_sdp_answer(sdp: &str) -> Result<String, JsError> {
    encode_client_frame(&ClientFrame::SdpAnswer {
        sdp: sdp.to_string(),
    })
    .map_err(to_err)
}

/// Compose a client → SFU trickle-ICE candidate.
#[wasm_bindgen(js_name = "composeSfuIceCandidate")]
pub fn compose_sfu_ice_candidate(candidate_sdp: &str) -> Result<String, JsError> {
    encode_client_frame(&ClientFrame::IceCandidate {
        candidate_sdp: candidate_sdp.to_string(),
    })
    .map_err(to_err)
}

/// Compose an application-layer ping (the SFU echoes a `Pong`).
#[wasm_bindgen(js_name = "composeSfuPing")]
pub fn compose_sfu_ping(nonce: u64) -> Result<String, JsError> {
    encode_client_frame(&ClientFrame::Ping { nonce }).map_err(to_err)
}

/// Base64url-encode an opaque relay payload for a `SignalRelay`.
#[wasm_bindgen(js_name = "sfuEncodePayload")]
#[must_use]
pub fn sfu_encode_payload(payload: &[u8]) -> String {
    encode_payload_b64(payload)
}

/// Base64url-decode a relay payload from a `SignalRelay`.
#[wasm_bindgen(js_name = "sfuDecodePayload")]
pub fn sfu_decode_payload(payload_b64: &str) -> Result<Vec<u8>, JsError> {
    decode_payload_b64(payload_b64).map_err(to_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_recognises_welcome() {
        let json = r#"{"kind":"welcome","peer_id":"p1","call_id":"c1","roster":[],"server_ts":1}"#;
        assert_eq!(validate_sfu_server_frame(json).expect("ok"), "welcome");
    }

    // The unknown-kind / malformed rejection path constructs a
    // `JsError`, which cannot be exercised on a non-wasm host target
    // (`cannot call wasm-bindgen imported functions on non-wasm
    // targets`). The rejection itself is covered by
    // `lakko_sfu_proto`'s own `unknown_kind_is_rejected` test.

    #[test]
    fn compose_sdp_offer_kebab() {
        let json = compose_sfu_sdp_offer("v=0\r\n").expect("compose");
        assert!(json.contains("\"kind\":\"sdp-offer\""), "got {json}");
    }

    #[test]
    fn payload_round_trips() {
        let payload = b"opaque".to_vec();
        let encoded = sfu_encode_payload(&payload);
        assert_eq!(sfu_decode_payload(&encoded).expect("decode"), payload);
    }

    #[test]
    fn signal_relay_to_peer_decodes_back() {
        let json = compose_sfu_signal_relay_to_peer("p2", b"x").expect("compose");
        match lakko_sfu_proto::decode_client_frame(&json).expect("decode") {
            ClientFrame::SignalRelay {
                to: RelayTarget::Peer { peer_id },
                ..
            } => assert_eq!(peer_id, "p2"),
            _ => panic!("expected SignalRelay to peer"),
        }
    }
}
