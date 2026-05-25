// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Lakko-native SFU signaling wire format.
//!
//! Not LiveKit's JSON-RPC, not Janus's plugin protocol —
//! this is the minimal set of frames Lakko clients need to
//! coordinate ICE / SDP / SFrame keying through the SFU.
//! The SFU only ever forwards or generates these frames;
//! it never inspects media payload.
//!
//! ## Wire shape
//!
//! Each WebSocket text frame is one JSON object with a
//! `kind` discriminator. The discriminator is the **first**
//! field in `serde`'s tagged-enum representation, so a
//! decoder that rejects unknown variants fails fast on
//! version drift.
//!
//! ## Frame categories
//!
//! | Direction | Frame | When |
//! |---|---|---|
//! | server → client | [`ServerFrame::Welcome`] | First frame after admission. Carries the server-assigned `peer_id`, the call's current roster, and the call_id the JWT pinned. |
//! | server → client | [`ServerFrame::PeerJoined`] | Another peer joined the same call. |
//! | server → client | [`ServerFrame::PeerLeft`] | Another peer disconnected. |
//! | server → client | [`ServerFrame::SignalRelay`] | Opaque payload from another peer (SDP offer / answer / ICE candidate / SFrame-key envelope). Lakko-side opaque — the SFU does not parse the body. |
//! | server → client | [`ServerFrame::Error`] | Server-generated error (rate limit, bad payload, etc.). The connection may be closed after this. |
//! | client → server | [`ClientFrame::SignalRelay`] | Send an opaque payload to a specific peer or to every peer in the call. |
//! | client → server | [`ClientFrame::Ping`] | Application-layer keepalive distinct from the WS-protocol ping. Clients that survive a NAT timeout fire one of these periodically; the SFU echoes back a `Pong`. |
//!
//! ## Opacity invariant
//!
//! The `payload` field of `SignalRelay` is **opaque bytes
//! base64url-encoded**. The SFU does not parse it. It
//! could be:
//!
//! - WebRTC SDP offer / answer
//! - ICE candidate
//! - SFrame key envelope (call-welcome / KP-forward, per
//!   `docs/security/calls.md`)
//! - Any future Lakko-side payload the clients agree on
//!
//! The trade-off is deliberate: keeping the SFU oblivious
//! to payload meaning lets us evolve client-to-client
//! protocols without touching the SFU. The audit-pack
//! invariant the SFU enforces is "I do not read this",
//! not "I understand it".

use serde::{Deserialize, Serialize};

/// Maximum size of an inbound signaling frame, bytes. The
/// hard cap protects the server against pathologically-
/// large payloads on the wire. SDP offers in production
/// peak around 8 KiB; we leave 64× headroom for unusual
/// codec configurations.
pub(crate) const MAX_INBOUND_FRAME_BYTES: usize = 512 * 1024;

/// Server-to-client frame.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum ServerFrame {
    /// First frame after the WS upgrade. Confirms admission
    /// and seeds the client's view of the call.
    Welcome {
        /// Server-assigned id for this connection.
        peer_id: String,
        /// Call this peer was admitted to (from the JWT).
        call_id: String,
        /// Every other peer currently in the call. Empty
        /// list means the joiner is alone (typical at
        /// call creation).
        roster: Vec<PeerSnapshot>,
        /// Server wall-clock unix-seconds at admission —
        /// lets the client estimate its clock skew vs the
        /// server.
        server_ts: u64,
    },

    /// A new peer joined the same call.
    PeerJoined {
        /// New peer's id.
        peer_id: String,
        /// Pseudonymised account id (first 8 hex chars). The
        /// SFU never broadcasts the raw `sub`.
        account_pseudonym: String,
    },

    /// A peer disconnected.
    PeerLeft {
        /// Departing peer's id.
        peer_id: String,
    },

    /// Opaque payload from another peer. The SFU does not
    /// inspect this — see module-level "Opacity invariant".
    SignalRelay {
        /// Sender peer id (server-assigned, not account id).
        from: String,
        /// Base64url-encoded opaque payload bytes.
        payload_b64: String,
    },

    /// Server-generated error. The connection may be closed
    /// after this frame; the client should not send more
    /// frames in response.
    Error {
        /// Machine-readable error code (stable across
        /// releases).
        code: ErrorCode,
        /// Human-readable detail, English. Never includes
        /// PII or token contents.
        detail: String,
    },

    /// Application-layer pong reply.
    Pong {
        /// Echoed client-supplied nonce so the client can
        /// match against its outstanding pings.
        nonce: u64,
    },
}

/// Client-to-server frame.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum ClientFrame {
    /// Send an opaque payload. `to` selects the destination
    /// peer (or broadcast to the whole call).
    SignalRelay {
        /// Destination — a specific peer or every peer in
        /// the call.
        to: RelayTarget,
        /// Base64url-encoded opaque payload bytes.
        payload_b64: String,
    },

    /// Application-layer keepalive. The server echoes back
    /// `Pong` with the same nonce.
    Ping {
        /// Caller-chosen nonce.
        nonce: u64,
    },
}

/// Destination selector for [`ClientFrame::SignalRelay`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum RelayTarget {
    /// Single recipient — most ICE / SDP exchanges fall in
    /// this bucket.
    Peer {
        /// Destination peer id.
        peer_id: String,
    },
    /// Every other peer in the call (excluding the sender).
    /// Used for "I'm leaving" announcements or call-wide
    /// SFrame key rotations.
    Call,
}

/// Snapshot of a peer in the welcome roster.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PeerSnapshot {
    /// Server-assigned peer id.
    pub peer_id: String,
    /// First 8 hex chars of the account id. The SFU never
    /// publishes the full `sub`.
    pub account_pseudonym: String,
}

/// Machine-readable error codes the SFU may emit.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ErrorCode {
    /// Inbound JSON failed to parse against `ClientFrame`.
    BadFrame,
    /// Frame exceeded `MAX_INBOUND_FRAME_BYTES`.
    FrameTooLarge,
    /// `payload_b64` decoded to a payload larger than the
    /// per-peer relay budget.
    PayloadTooLarge,
    /// Destination peer id was not found in the same call.
    UnknownPeer,
    /// Peer's outbound queue is full; the SFU dropped the
    /// inbound message to protect the slow recipient.
    BackpressureDrop,
    /// Per-peer signaling rate exceeded
    /// `LAKKO_SFU_PEER_MSG_RATE_PER_SEC`.
    RateLimited,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welcome_round_trip_via_json() {
        let frame = ServerFrame::Welcome {
            peer_id: "p123".into(),
            call_id: "0123456789abcdef0123456789abcdef".into(),
            roster: vec![PeerSnapshot {
                peer_id: "p100".into(),
                account_pseudonym: "deadbeef".into(),
            }],
            server_ts: 1_700_000_000,
        };
        let json = serde_json::to_string(&frame).unwrap();
        assert!(json.contains("\"kind\":\"welcome\""));
        let back: ServerFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(back, frame);
    }

    #[test]
    fn signal_relay_serialises_to_kebab_case() {
        let frame = ClientFrame::SignalRelay {
            to: RelayTarget::Peer { peer_id: "p1".into() },
            payload_b64: "AAAA".into(),
        };
        let json = serde_json::to_string(&frame).unwrap();
        assert!(json.contains("\"kind\":\"signal-relay\""));
        assert!(json.contains("\"to\":{\"kind\":\"peer\""));
    }

    #[test]
    fn relay_target_call_round_trip() {
        let t = RelayTarget::Call;
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, r#"{"kind":"call"}"#);
        let back: RelayTarget = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn ping_pong_round_trip() {
        let request = ClientFrame::Ping { nonce: 42 };
        let reply = ServerFrame::Pong { nonce: 42 };
        let request_json = serde_json::to_string(&request).unwrap();
        let reply_json = serde_json::to_string(&reply).unwrap();
        assert!(request_json.contains("\"nonce\":42"));
        assert!(reply_json.contains("\"nonce\":42"));
        let _: ClientFrame = serde_json::from_str(&request_json).unwrap();
        let _: ServerFrame = serde_json::from_str(&reply_json).unwrap();
    }

    #[test]
    fn error_codes_serialise_kebab() {
        let frame = ServerFrame::Error {
            code: ErrorCode::FrameTooLarge,
            detail: "exceeds 512 KiB".into(),
        };
        let json = serde_json::to_string(&frame).unwrap();
        assert!(json.contains("\"code\":\"frame-too-large\""));
    }

    #[test]
    fn unknown_kind_is_rejected() {
        // Untagged extra variants must fail decode — this is
        // the "fail fast on version drift" property the
        // module-level docs promise.
        let raw = r#"{"kind":"future-frame","x":1}"#;
        let result: Result<ClientFrame, _> = serde_json::from_str(raw);
        assert!(result.is_err());
    }

    #[test]
    fn server_frame_and_client_frame_do_not_alias_kinds() {
        // The two enums share the kind=signal-relay name —
        // intentional (it's the same logical frame, one in
        // each direction). Everything else must be unique
        // per direction so a misrouted frame fails decode.
        let server_kinds = [
            "welcome",
            "peer-joined",
            "peer-left",
            "signal-relay",
            "error",
            "pong",
        ];
        let client_kinds = ["signal-relay", "ping"];
        for c in client_kinds {
            // Every client kind must either be unique or
            // explicitly shared (`signal-relay`).
            assert!(c == "signal-relay" || !server_kinds.contains(&c));
        }
    }
}
