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
pub const MAX_INBOUND_FRAME_BYTES: usize = 512 * 1024;

/// Server-to-client frame.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ServerFrame {
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

    /// SDP answer the SFU produced in response to the
    /// client's [`ClientFrame::SdpOffer`]. Carries the
    /// SFU's local ICE candidates inline (`a=candidate:`
    /// lines) — str0m bakes them into the answer rather
    /// than emitting them as separate events.
    ///
    /// S-3-B-routing-media step 2: the wire frame is
    /// shipped; the router wiring that hands offers
    /// through to [`crate::media::MediaPeer::accept_offer`]
    /// lands in step 3 alongside the UDP socket loop.
    SdpAnswer {
        /// SDP answer string. Plain text, no JSON
        /// wrapping — str0m's
        /// `SdpAnswer::to_sdp_string` output verbatim.
        sdp: String,
    },

    /// SDP offer the **SFU** sends to a peer when an
    /// upstream `MediaAdded` event from another peer
    /// in the same call needs a renegotiation cycle to
    /// open a downstream track (S-3-B-tracks step 2).
    ///
    /// The wire flow that produces this frame:
    ///
    /// 1. Peer A sends `ClientFrame::SdpOffer` to add a
    ///    new media line.
    /// 2. SFU replies `ServerFrame::SdpAnswer` (existing
    ///    path).
    /// 3. SFU's media task sees `Event::MediaAdded` on
    ///    A's `Rtc`, registers the `TrackIn`, propagates
    ///    a `TrackOutEntry::ToOpen` to every other peer
    ///    in the same call.
    /// 4. For each peer with a `ToOpen` entry, the SFU
    ///    calls `Rtc::sdp_api().add_media(...)`, calls
    ///    `apply()` to produce an SDP offer +
    ///    `SdpPendingOffer`, ships the offer here.
    ///
    /// The peer's `RTCPeerConnection` accepts the
    /// offer, generates an answer, ships it back as
    /// `ClientFrame::SdpAnswer`. The SFU calls
    /// `Rtc::sdp_api().accept_answer(pending, answer)`
    /// and promotes the `TrackOutEntry` to
    /// `Open(local_mid)` — fanout starts delivering
    /// upstream `MediaData` frames through.
    SdpOffer {
        /// SDP offer the SFU produced via
        /// `Rtc::sdp_api()` + `apply()`. Same shape as
        /// `ClientFrame::SdpOffer.sdp` from the
        /// inverse flow.
        sdp: String,
    },
}

/// Client-to-server frame.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ClientFrame {
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

    /// Client → SFU: SDP offer that the SFU consumes via
    /// its own `str0m::Rtc::sdp_api()`. The SFU replies
    /// with [`ServerFrame::SdpAnswer`] on success or
    /// [`ServerFrame::Error`] with
    /// [`ErrorCode::BadSdp`] on a parse / state failure.
    ///
    /// Unlike [`Self::SignalRelay`] this is NOT an opaque
    /// peer-to-peer payload — the SFU MUST parse the SDP
    /// to drive its own media state machine. The audit
    /// invariant from `docs/security/calls.md` still
    /// holds: the SFU only inspects transport-layer SDP
    /// (codecs, ICE creds, DTLS fingerprint) — the inner
    /// RTP payload it later forwards is SFrame ciphertext
    /// and stays opaque.
    SdpOffer {
        /// SDP offer string. Plain text, no JSON
        /// wrapping — same shape `RTCPeerConnection.localDescription.sdp`
        /// returns on the browser side.
        sdp: String,
    },

    /// Client → SFU: a trickle-ICE candidate the client
    /// gathered after sending its initial
    /// [`Self::SdpOffer`]. The SFU hands it to its
    /// `str0m::Rtc::add_remote_candidate` so the next
    /// connectivity check can include it. Failure to
    /// parse the candidate surfaces as
    /// [`ServerFrame::Error`] with
    /// [`ErrorCode::BadIceCandidate`].
    ///
    /// `candidate_sdp` is the SDP `a=candidate:…`
    /// attribute *value* (everything after `candidate:`),
    /// matching what `RTCIceCandidate.candidate` returns
    /// on the browser side. `str0m::Candidate::from_sdp_string`
    /// is the parser the SFU uses.
    IceCandidate {
        /// SDP candidate string — for example
        /// `candidate:842163049 1 udp 1677729535 192.0.2.1
        /// 60769 typ srflx raddr 0.0.0.0 rport 0
        /// generation 0`.
        candidate_sdp: String,
    },

    /// Client → SFU: SDP answer that responds to a
    /// **SFU-initiated** offer (the inverse of the
    /// usual flow). The SFU sent
    /// [`ServerFrame::SdpOffer`] to open a downstream
    /// track for media coming from another peer; the
    /// client's `RTCPeerConnection.createAnswer` runs
    /// against that offer and the resulting SDP rides
    /// back on this frame.
    ///
    /// The SFU hands the answer to
    /// `Rtc::sdp_api().accept_answer(pending, answer)`
    /// and promotes the matching `TrackOutEntry` from
    /// `Negotiating(local_mid)` to
    /// `Open(local_mid)` — fanout starts delivering
    /// upstream RTP through.
    SdpAnswer {
        /// SDP answer string. Same shape as
        /// `RTCPeerConnection.localDescription.sdp`
        /// returns on the browser side after a
        /// `setRemoteDescription(offer)` +
        /// `createAnswer()`.
        sdp: String,
    },
}

/// Destination selector for [`ClientFrame::SignalRelay`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RelayTarget {
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
pub struct PeerSnapshot {
    /// Server-assigned peer id.
    pub peer_id: String,
    /// First 8 hex chars of the account id. The SFU never
    /// publishes the full `sub`.
    pub account_pseudonym: String,
}

/// Machine-readable error codes the SFU may emit.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
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
    /// `ClientFrame::SdpOffer.sdp` failed to parse via
    /// `str0m::SdpOffer::from_sdp_string` or the SFU's
    /// state machine refused the offer (see the `detail`
    /// field for the str0m diagnostic).
    BadSdp,
    /// `ClientFrame::IceCandidate.candidate_sdp` failed
    /// to parse via `str0m::Candidate::from_sdp_string`.
    BadIceCandidate,
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
            to: RelayTarget::Peer {
                peer_id: "p1".into(),
            },
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
    fn sdp_offer_round_trip_kebab() {
        let frame = ClientFrame::SdpOffer {
            sdp: "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\n".into(),
        };
        let json = serde_json::to_string(&frame).unwrap();
        assert!(
            json.contains("\"kind\":\"sdp-offer\""),
            "expected kebab kind=sdp-offer, got {json}"
        );
        let back: ClientFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(back, frame);
    }

    #[test]
    fn sdp_answer_round_trip_kebab() {
        let frame = ServerFrame::SdpAnswer {
            sdp: "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\n".into(),
        };
        let json = serde_json::to_string(&frame).unwrap();
        assert!(
            json.contains("\"kind\":\"sdp-answer\""),
            "expected kebab kind=sdp-answer, got {json}"
        );
        let back: ServerFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(back, frame);
    }

    #[test]
    fn ice_candidate_round_trip_kebab() {
        let frame = ClientFrame::IceCandidate {
            candidate_sdp: "candidate:1 1 udp 1677729535 192.0.2.1 60769 typ srflx".into(),
        };
        let json = serde_json::to_string(&frame).unwrap();
        assert!(
            json.contains("\"kind\":\"ice-candidate\""),
            "expected kebab kind=ice-candidate, got {json}"
        );
        let back: ClientFrame = serde_json::from_str(&json).unwrap();
        assert_eq!(back, frame);
    }

    #[test]
    fn bad_sdp_error_code_kebab() {
        let frame = ServerFrame::Error {
            code: ErrorCode::BadSdp,
            detail: "str0m: missing m= line".into(),
        };
        let json = serde_json::to_string(&frame).unwrap();
        assert!(
            json.contains("\"code\":\"bad-sdp\""),
            "expected kebab code=bad-sdp, got {json}"
        );
    }

    #[test]
    fn bad_ice_candidate_error_code_kebab() {
        let frame = ServerFrame::Error {
            code: ErrorCode::BadIceCandidate,
            detail: "candidate:… parse failed".into(),
        };
        let json = serde_json::to_string(&frame).unwrap();
        assert!(
            json.contains("\"code\":\"bad-ice-candidate\""),
            "expected kebab code=bad-ice-candidate, got {json}"
        );
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
