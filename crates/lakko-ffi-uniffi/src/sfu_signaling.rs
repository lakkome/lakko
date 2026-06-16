// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for the SFU signaling wire frames
//! ([`lakko_sfu_proto`]) — SFU multi-party arc, subsprint 1c.
//!
//! Mirror of the wasm-bindgen surface in
//! [`crates/lakko-ffi-wasm/src/sfu_signaling.rs`](../../../lakko-ffi-wasm/src/sfu_signaling.rs).
//! Clients use this to parse the server frames the SFU pushes over
//! the `lakko-sfu-v1` WebSocket and to compose the frames they send
//! back — from the **same** definition `services/sfu` links, so the
//! wire shape cannot drift between server and client.
//!
//! ## What's exposed
//!
//! - [`SfuServerFrame`] enum + [`SfuPeerSnapshot`] record +
//!   [`SfuErrorCode`] enum — the typed projection of an inbound
//!   server frame.
//! - [`parse_sfu_server_frame`] — decode a WS text frame.
//! - `compose_sfu_*` free functions — build the JSON wire string for
//!   each client → server frame.
//! - [`sfu_encode_payload`] / [`sfu_decode_payload`] — base64url
//!   wrap / unwrap for the opaque `SignalRelay` payload (SDP / ICE /
//!   SFrame-key envelope; opaque to the SFU).

use lakko_sfu_proto::{
    ClientFrame, ErrorCode, RelayTarget, ServerFrame, decode_payload_b64, decode_server_frame,
    encode_client_frame, encode_payload_b64,
};

use crate::FfiError;

impl From<lakko_sfu_proto::SfuProtoError> for FfiError {
    fn from(e: lakko_sfu_proto::SfuProtoError) -> Self {
        Self::Sfu(e.to_string())
    }
}

/// One peer in a [`SfuServerFrame::Welcome`] roster.
#[derive(uniffi::Record)]
pub struct SfuPeerSnapshot {
    /// Server-assigned peer id.
    pub peer_id: String,
    /// First 8 hex chars of the account id (the SFU never publishes
    /// the full `sub`).
    pub account_pseudonym: String,
}

impl From<lakko_sfu_proto::PeerSnapshot> for SfuPeerSnapshot {
    fn from(p: lakko_sfu_proto::PeerSnapshot) -> Self {
        Self {
            peer_id: p.peer_id,
            account_pseudonym: p.account_pseudonym,
        }
    }
}

/// Machine-readable SFU error code (projection of
/// [`lakko_sfu_proto::ErrorCode`]).
#[derive(uniffi::Enum)]
pub enum SfuErrorCode {
    /// Inbound JSON failed to parse against `ClientFrame`.
    BadFrame,
    /// Frame exceeded the inbound size cap.
    FrameTooLarge,
    /// Relay payload exceeded the per-peer budget.
    PayloadTooLarge,
    /// Destination peer id not found in the call.
    UnknownPeer,
    /// Peer's outbound queue was full; the SFU dropped the message.
    BackpressureDrop,
    /// Per-peer signaling rate exceeded.
    RateLimited,
    /// SDP offer failed to parse / was refused.
    BadSdp,
    /// ICE candidate failed to parse.
    BadIceCandidate,
}

impl From<ErrorCode> for SfuErrorCode {
    fn from(c: ErrorCode) -> Self {
        match c {
            ErrorCode::BadFrame => Self::BadFrame,
            ErrorCode::FrameTooLarge => Self::FrameTooLarge,
            ErrorCode::PayloadTooLarge => Self::PayloadTooLarge,
            ErrorCode::UnknownPeer => Self::UnknownPeer,
            ErrorCode::BackpressureDrop => Self::BackpressureDrop,
            ErrorCode::RateLimited => Self::RateLimited,
            ErrorCode::BadSdp => Self::BadSdp,
            ErrorCode::BadIceCandidate => Self::BadIceCandidate,
        }
    }
}

/// Typed projection of an inbound [`lakko_sfu_proto::ServerFrame`].
#[derive(uniffi::Enum)]
pub enum SfuServerFrame {
    /// First frame after admission — seeds the client's view.
    Welcome {
        /// Server-assigned id for this connection.
        peer_id: String,
        /// Call this peer was admitted to (from the JWT).
        call_id: String,
        /// Every other peer currently in the call.
        roster: Vec<SfuPeerSnapshot>,
        /// Server wall-clock unix-seconds at admission.
        server_ts: u64,
    },
    /// A new peer joined the same call.
    PeerJoined {
        /// New peer's id.
        peer_id: String,
        /// Pseudonymised account id (first 8 hex chars).
        account_pseudonym: String,
    },
    /// A peer disconnected.
    PeerLeft {
        /// Departing peer's id.
        peer_id: String,
    },
    /// Opaque payload relayed from another peer.
    SignalRelay {
        /// Sender peer id.
        from: String,
        /// Base64url-encoded opaque payload — decode with
        /// [`sfu_decode_payload`].
        payload_b64: String,
    },
    /// Server-generated error.
    Error {
        /// Machine-readable code.
        code: SfuErrorCode,
        /// Human-readable detail (English, no PII).
        detail: String,
    },
    /// Application-layer pong reply.
    Pong {
        /// Echoed nonce.
        nonce: u64,
    },
    /// SDP answer to the client's offer.
    SdpAnswer {
        /// SDP answer string.
        sdp: String,
    },
    /// SFU-initiated SDP offer (opens a downstream track).
    SdpOffer {
        /// SDP offer string.
        sdp: String,
    },
}

impl From<ServerFrame> for SfuServerFrame {
    fn from(f: ServerFrame) -> Self {
        match f {
            ServerFrame::Welcome {
                peer_id,
                call_id,
                roster,
                server_ts,
            } => Self::Welcome {
                peer_id,
                call_id,
                roster: roster.into_iter().map(Into::into).collect(),
                server_ts,
            },
            ServerFrame::PeerJoined {
                peer_id,
                account_pseudonym,
                // `peer_tag` (ADR-0021) is surfaced to clients in a
                // follow-up FFI sprint; ignored here for now.
                ..
            } => Self::PeerJoined {
                peer_id,
                account_pseudonym,
            },
            ServerFrame::PeerLeft { peer_id } => Self::PeerLeft { peer_id },
            ServerFrame::SignalRelay { from, payload_b64 } => Self::SignalRelay { from, payload_b64 },
            ServerFrame::Error { code, detail } => Self::Error {
                code: code.into(),
                detail,
            },
            ServerFrame::Pong { nonce } => Self::Pong { nonce },
            ServerFrame::SdpAnswer { sdp } => Self::SdpAnswer { sdp },
            ServerFrame::SdpOffer { sdp } => Self::SdpOffer { sdp },
        }
    }
}

/// Decode an inbound server frame off the `lakko-sfu-v1` WebSocket.
#[uniffi::export]
pub fn parse_sfu_server_frame(json: String) -> Result<SfuServerFrame, FfiError> {
    Ok(decode_server_frame(&json)?.into())
}

/// Compose a `SignalRelay` to a single peer. `payload` is the raw
/// opaque bytes (SDP / ICE / SFrame-key envelope); this helper
/// base64url-encodes them.
#[uniffi::export]
pub fn compose_sfu_signal_relay_to_peer(
    peer_id: String,
    payload: Vec<u8>,
) -> Result<String, FfiError> {
    let frame = ClientFrame::SignalRelay {
        to: RelayTarget::Peer { peer_id },
        payload_b64: encode_payload_b64(&payload),
    };
    Ok(encode_client_frame(&frame)?)
}

/// Compose a `SignalRelay` broadcast to every other peer in the call.
#[uniffi::export]
pub fn compose_sfu_signal_relay_to_call(payload: Vec<u8>) -> Result<String, FfiError> {
    let frame = ClientFrame::SignalRelay {
        to: RelayTarget::Call,
        payload_b64: encode_payload_b64(&payload),
    };
    Ok(encode_client_frame(&frame)?)
}

/// Compose a client → SFU SDP offer.
#[uniffi::export]
pub fn compose_sfu_sdp_offer(sdp: String) -> Result<String, FfiError> {
    Ok(encode_client_frame(&ClientFrame::SdpOffer { sdp })?)
}

/// Compose a client → SFU SDP answer (responds to a SFU-initiated
/// offer).
#[uniffi::export]
pub fn compose_sfu_sdp_answer(sdp: String) -> Result<String, FfiError> {
    Ok(encode_client_frame(&ClientFrame::SdpAnswer { sdp })?)
}

/// Compose a client → SFU trickle-ICE candidate.
#[uniffi::export]
pub fn compose_sfu_ice_candidate(candidate_sdp: String) -> Result<String, FfiError> {
    Ok(encode_client_frame(&ClientFrame::IceCandidate { candidate_sdp })?)
}

/// Compose an application-layer ping (the SFU echoes a `Pong`).
#[uniffi::export]
pub fn compose_sfu_ping(nonce: u64) -> Result<String, FfiError> {
    Ok(encode_client_frame(&ClientFrame::Ping { nonce })?)
}

/// Base64url-encode an opaque relay payload for a `SignalRelay`.
#[uniffi::export]
pub fn sfu_encode_payload(payload: Vec<u8>) -> String {
    encode_payload_b64(&payload)
}

/// Base64url-decode a relay payload from a `SignalRelay`.
#[uniffi::export]
pub fn sfu_decode_payload(payload_b64: String) -> Result<Vec<u8>, FfiError> {
    Ok(decode_payload_b64(&payload_b64)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welcome_parses_into_typed_frame() {
        let json = r#"{"kind":"welcome","peer_id":"p1","call_id":"c1","roster":[{"peer_id":"p0","account_pseudonym":"deadbeef"}],"server_ts":1700000000}"#;
        match parse_sfu_server_frame(json.into()).expect("parse") {
            SfuServerFrame::Welcome {
                peer_id,
                roster,
                server_ts,
                ..
            } => {
                assert_eq!(peer_id, "p1");
                assert_eq!(roster.len(), 1);
                assert_eq!(roster[0].account_pseudonym, "deadbeef");
                assert_eq!(server_ts, 1_700_000_000);
            }
            _ => panic!("expected Welcome"),
        }
    }

    #[test]
    fn error_frame_maps_code() {
        let json = r#"{"kind":"error","code":"bad-sdp","detail":"nope"}"#;
        match parse_sfu_server_frame(json.into()).expect("parse") {
            SfuServerFrame::Error { code, .. } => {
                assert!(matches!(code, SfuErrorCode::BadSdp));
            }
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn compose_signal_relay_round_trips_payload() {
        let payload = b"opaque-sdp-bytes".to_vec();
        let json = compose_sfu_signal_relay_to_peer("p2".into(), payload.clone()).expect("compose");
        assert!(json.contains("\"kind\":\"signal-relay\""));
        assert!(json.contains("\"kind\":\"peer\""));
        // Decode back through the proto crate and confirm the payload
        // round-trips.
        match lakko_sfu_proto::decode_client_frame(&json).expect("decode") {
            ClientFrame::SignalRelay {
                to: RelayTarget::Peer { peer_id },
                payload_b64,
            } => {
                assert_eq!(peer_id, "p2");
                assert_eq!(sfu_decode_payload(payload_b64).expect("payload"), payload);
            }
            _ => panic!("expected SignalRelay to peer"),
        }
    }

    #[test]
    fn compose_sdp_offer_kebab() {
        let json = compose_sfu_sdp_offer("v=0\r\n".into()).expect("compose");
        assert!(json.contains("\"kind\":\"sdp-offer\""), "got {json}");
    }

    #[test]
    fn parse_rejects_unknown_kind() {
        assert!(parse_sfu_server_frame(r#"{"kind":"nope"}"#.into()).is_err());
    }
}
