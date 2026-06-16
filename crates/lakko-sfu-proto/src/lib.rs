// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Lakko-native SFU signaling wire format — the shared source of truth.
//!
//! Not LiveKit's JSON-RPC, not Janus's plugin protocol — this is the
//! minimal set of frames Lakko clients need to coordinate ICE / SDP /
//! SFrame keying through the SFU. The SFU only ever forwards or
//! generates these frames; it never inspects media payload.
//!
//! ## Why a separate crate
//!
//! These types were originally defined inside `services/sfu`. The
//! SFU multi-party arc (subsprint 1) lifts them into this shared
//! workspace crate so the **server** (`services/sfu`) and the
//! **clients** (via `lakko-ffi-uniffi` + `lakko-ffi-wasm`) parse and
//! emit the exact same wire shape from one definition — no hand-coded
//! per-platform JSON parsers, no drift between server and client.
//!
//! ## Wire shape
//!
//! Each WebSocket text frame is one JSON object with a `kind`
//! discriminator. The discriminator is the **first** field in
//! `serde`'s tagged-enum representation, so a decoder that rejects
//! unknown variants fails fast on version drift.
//!
//! ## Opacity invariant
//!
//! The `payload_b64` field of `SignalRelay` is **opaque bytes
//! base64url-encoded**. The SFU does not parse it. It could be a
//! WebRTC SDP offer / answer, an ICE candidate, an SFrame key
//! envelope (call-welcome / KP-forward, per `docs/security/calls.md`),
//! or any future Lakko-side payload the clients agree on. Keeping the
//! SFU oblivious to payload meaning lets us evolve client-to-client
//! protocols without touching the SFU. The audit-pack invariant the
//! SFU enforces is "I do not read this", not "I understand it".
//!
//! [`encode_payload_b64`] / [`decode_payload_b64`] are the helpers
//! clients use to wrap / unwrap that opaque body; the SFU never calls
//! them.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

/// Maximum size of an inbound signaling frame, bytes. The hard cap
/// protects the server against pathologically-large payloads on the
/// wire. SDP offers in production peak around 8 KiB; we leave 64×
/// headroom for unusual codec configurations.
pub const MAX_INBOUND_FRAME_BYTES: usize = 512 * 1024;

/// Errors raised by the encode / decode helpers in this crate.
#[derive(Debug, thiserror::Error)]
pub enum SfuProtoError {
    /// JSON (de)serialisation failed — malformed frame, unknown
    /// `kind`, or a missing required field.
    #[error("sfu frame json error: {0}")]
    Json(#[from] serde_json::Error),
    /// `payload_b64` was not valid base64url.
    #[error("sfu payload base64url decode error: {0}")]
    Base64(#[from] base64::DecodeError),
    /// An inbound frame exceeded [`MAX_INBOUND_FRAME_BYTES`].
    #[error("sfu frame exceeds {MAX_INBOUND_FRAME_BYTES} bytes (was {0})")]
    FrameTooLarge(usize),
}

/// Domain-separation context for the per-call peer tag (ADR-0021).
const PEER_TAG_CONTEXT: &str = "lakko/sfu/peer-tag/v1";

/// Length of the per-call peer tag in bytes (rendered as `2×` lower-case
/// hex chars). 8 bytes = 64 bits — birthday-safe for the few-hundred
/// participants a call holds, and short on the wire / as a fallback
/// display label.
pub const PEER_TAG_LEN: usize = 8;

/// Derive the per-call **peer tag** (ADR-0021).
///
/// A privacy-preserving, collision-resistant, per-call identifier the SFU
/// emits for each peer (over the JWT-verified `account_id`) and the
/// clients re-derive over their contacts' account ids to resolve a roster
/// entry to a display name.
///
/// `peer_tag = BLAKE3_derive_key(PEER_TAG_CONTEXT, len(call_id) ‖ call_id
/// ‖ len(account_id) ‖ account_id)` truncated to [`PEER_TAG_LEN`] bytes,
/// lower-case hex. The big-endian length framing prevents a
/// `(call_id, account_id)` concatenation ambiguity; keying on `call_id`
/// makes the tag fresh per call (unlinkable across calls); the one-way
/// hash never reveals the `account_id`. Deterministic + canonical, so the
/// SFU and every client compute byte-identical strings.
#[must_use]
pub fn peer_tag(call_id: &str, account_id: &str) -> String {
    use core::fmt::Write as _;
    let mut hasher = blake3::Hasher::new_derive_key(PEER_TAG_CONTEXT);
    hasher.update(&(call_id.len() as u64).to_be_bytes());
    hasher.update(call_id.as_bytes());
    hasher.update(&(account_id.len() as u64).to_be_bytes());
    hasher.update(account_id.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(PEER_TAG_LEN * 2);
    for b in &digest.as_bytes()[..PEER_TAG_LEN] {
        // Writing a `u8` as hex into a `String` is infallible.
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Server-to-client frame.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ServerFrame {
    /// First frame after the WS upgrade. Confirms admission and seeds
    /// the client's view of the call.
    Welcome {
        /// Server-assigned id for this connection.
        peer_id: String,
        /// Call this peer was admitted to (from the JWT).
        call_id: String,
        /// Every other peer currently in the call. Empty list means
        /// the joiner is alone (typical at call creation).
        roster: Vec<PeerSnapshot>,
        /// Server wall-clock unix-seconds at admission — lets the
        /// client estimate its clock skew vs the server.
        server_ts: u64,
    },

    /// A new peer joined the same call.
    PeerJoined {
        /// New peer's id.
        peer_id: String,
        /// Pseudonymised account id (first 8 hex chars). The SFU
        /// never broadcasts the raw `sub`. Legacy display tag — kept
        /// during the ADR-0021 rollout; superseded by `peer_tag`.
        account_pseudonym: String,
        /// Per-call peer tag (ADR-0021) — [`peer_tag`] over the
        /// JWT-verified `account_id` + the `call_id`. `#[serde(default)]`
        /// for forward/backward compatibility during rollout.
        #[serde(default)]
        peer_tag: String,
    },

    /// A peer disconnected.
    PeerLeft {
        /// Departing peer's id.
        peer_id: String,
    },

    /// Opaque payload from another peer. The SFU does not inspect
    /// this — see module-level "Opacity invariant".
    SignalRelay {
        /// Sender peer id (server-assigned, not account id).
        from: String,
        /// Base64url-encoded opaque payload bytes.
        payload_b64: String,
    },

    /// Server-generated error. The connection may be closed after
    /// this frame; the client should not send more frames in
    /// response.
    Error {
        /// Machine-readable error code (stable across releases).
        code: ErrorCode,
        /// Human-readable detail, English. Never includes PII or
        /// token contents.
        detail: String,
    },

    /// Application-layer pong reply.
    Pong {
        /// Echoed client-supplied nonce so the client can match
        /// against its outstanding pings.
        nonce: u64,
    },

    /// SDP answer the SFU produced in response to the client's
    /// [`ClientFrame::SdpOffer`]. Carries the SFU's local ICE
    /// candidates inline (`a=candidate:` lines) — str0m bakes them
    /// into the answer rather than emitting them as separate events.
    SdpAnswer {
        /// SDP answer string. Plain text, no JSON wrapping — str0m's
        /// `SdpAnswer::to_sdp_string` output verbatim.
        sdp: String,
    },

    /// SDP offer the **SFU** sends to a peer when an upstream
    /// `MediaAdded` event from another peer in the same call needs a
    /// renegotiation cycle to open a downstream track. The peer's
    /// `RTCPeerConnection` accepts the offer, generates an answer,
    /// and ships it back as [`ClientFrame::SdpAnswer`].
    SdpOffer {
        /// SDP offer the SFU produced via `Rtc::sdp_api()` +
        /// `apply()`. Same shape as `ClientFrame::SdpOffer.sdp` from
        /// the inverse flow.
        sdp: String,
    },
}

/// Client-to-server frame.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ClientFrame {
    /// Send an opaque payload. `to` selects the destination peer (or
    /// broadcast to the whole call).
    SignalRelay {
        /// Destination — a specific peer or every peer in the call.
        to: RelayTarget,
        /// Base64url-encoded opaque payload bytes.
        payload_b64: String,
    },

    /// Application-layer keepalive. The server echoes back `Pong`
    /// with the same nonce.
    Ping {
        /// Caller-chosen nonce.
        nonce: u64,
    },

    /// Client → SFU: SDP offer that the SFU consumes via its own
    /// `str0m::Rtc::sdp_api()`. The SFU replies with
    /// [`ServerFrame::SdpAnswer`] on success or [`ServerFrame::Error`]
    /// with [`ErrorCode::BadSdp`] on a parse / state failure.
    ///
    /// Unlike [`Self::SignalRelay`] this is NOT an opaque
    /// peer-to-peer payload — the SFU MUST parse the SDP to drive its
    /// own media state machine. The audit invariant still holds: the
    /// SFU only inspects transport-layer SDP (codecs, ICE creds, DTLS
    /// fingerprint) — the inner RTP payload it later forwards is
    /// SFrame ciphertext and stays opaque.
    SdpOffer {
        /// SDP offer string. Plain text, no JSON wrapping — same
        /// shape `RTCPeerConnection.localDescription.sdp` returns on
        /// the browser side.
        sdp: String,
    },

    /// Client → SFU: a trickle-ICE candidate the client gathered
    /// after sending its initial [`Self::SdpOffer`]. The SFU hands it
    /// to `str0m::Rtc::add_remote_candidate`. Failure to parse
    /// surfaces as [`ServerFrame::Error`] with
    /// [`ErrorCode::BadIceCandidate`].
    ///
    /// `candidate_sdp` is the SDP `a=candidate:…` attribute *value*
    /// (everything after `candidate:`), matching what
    /// `RTCIceCandidate.candidate` returns on the browser side.
    IceCandidate {
        /// SDP candidate string — for example
        /// `candidate:842163049 1 udp 1677729535 192.0.2.1 60769 typ
        /// srflx raddr 0.0.0.0 rport 0 generation 0`.
        candidate_sdp: String,
    },

    /// Client → SFU: SDP answer that responds to a **SFU-initiated**
    /// offer (the inverse of the usual flow). The SFU sent
    /// [`ServerFrame::SdpOffer`] to open a downstream track for media
    /// coming from another peer; the client's
    /// `RTCPeerConnection.createAnswer` runs against that offer and
    /// the resulting SDP rides back on this frame.
    SdpAnswer {
        /// SDP answer string. Same shape as
        /// `RTCPeerConnection.localDescription.sdp` returns on the
        /// browser side after a `setRemoteDescription(offer)` +
        /// `createAnswer()`.
        sdp: String,
    },
}

/// Destination selector for [`ClientFrame::SignalRelay`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RelayTarget {
    /// Single recipient — most ICE / SDP exchanges fall in this
    /// bucket.
    Peer {
        /// Destination peer id.
        peer_id: String,
    },
    /// Every other peer in the call (excluding the sender). Used for
    /// "I'm leaving" announcements or call-wide SFrame key rotations.
    Call,
}

/// Snapshot of a peer in the welcome roster.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerSnapshot {
    /// Server-assigned peer id.
    pub peer_id: String,
    /// First 8 hex chars of the account id. The SFU never publishes
    /// the full `sub`. Legacy display tag — kept during the ADR-0021
    /// rollout; superseded by [`PeerSnapshot::peer_tag`].
    pub account_pseudonym: String,
    /// Per-call peer tag (ADR-0021) — [`peer_tag`] over the JWT-verified
    /// `account_id` + the `call_id`. The client re-derives it over its
    /// contacts' account ids to resolve this entry to a display name.
    /// `#[serde(default)]` so a frame from a pre-ADR-0021 SFU (or a
    /// fixture) still decodes (empty tag → no resolution, falls back to
    /// the pseudonym).
    #[serde(default)]
    pub peer_tag: String,
}

/// Machine-readable error codes the SFU may emit.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
    /// Inbound JSON failed to parse against `ClientFrame`.
    BadFrame,
    /// Frame exceeded [`MAX_INBOUND_FRAME_BYTES`].
    FrameTooLarge,
    /// `payload_b64` decoded to a payload larger than the per-peer
    /// relay budget.
    PayloadTooLarge,
    /// Destination peer id was not found in the same call.
    UnknownPeer,
    /// Peer's outbound queue is full; the SFU dropped the inbound
    /// message to protect the slow recipient.
    BackpressureDrop,
    /// Per-peer signaling rate exceeded
    /// `LAKKO_SFU_PEER_MSG_RATE_PER_SEC`.
    RateLimited,
    /// `ClientFrame::SdpOffer.sdp` failed to parse or the SFU's state
    /// machine refused the offer (see the `detail` field for the
    /// str0m diagnostic).
    BadSdp,
    /// `ClientFrame::IceCandidate.candidate_sdp` failed to parse.
    BadIceCandidate,
}

// ---------------------------------------------------------------------------
// Encode / decode helpers — the surface the FFI layer (SFU-1c) wraps.
// ---------------------------------------------------------------------------

/// Decode an inbound client frame from a WS text payload, enforcing
/// the [`MAX_INBOUND_FRAME_BYTES`] cap first. This is the server-side
/// entry point, but it's pure and lives here so the wire rules stay
/// in one place.
pub fn decode_client_frame(json: &str) -> Result<ClientFrame, SfuProtoError> {
    if json.len() > MAX_INBOUND_FRAME_BYTES {
        return Err(SfuProtoError::FrameTooLarge(json.len()));
    }
    Ok(serde_json::from_str(json)?)
}

/// Serialise a client frame to its JSON wire form (client side).
pub fn encode_client_frame(frame: &ClientFrame) -> Result<String, SfuProtoError> {
    Ok(serde_json::to_string(frame)?)
}

/// Decode an inbound server frame from a WS text payload (client
/// side). Enforces the same size cap as the client-frame path.
pub fn decode_server_frame(json: &str) -> Result<ServerFrame, SfuProtoError> {
    if json.len() > MAX_INBOUND_FRAME_BYTES {
        return Err(SfuProtoError::FrameTooLarge(json.len()));
    }
    Ok(serde_json::from_str(json)?)
}

/// Serialise a server frame to its JSON wire form (server side).
pub fn encode_server_frame(frame: &ServerFrame) -> Result<String, SfuProtoError> {
    Ok(serde_json::to_string(frame)?)
}

/// Base64url-encode an opaque relay payload (no padding) for the
/// `payload_b64` field. Client-side helper — the SFU never calls it.
#[must_use]
pub fn encode_payload_b64(payload: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(payload)
}

/// Base64url-decode a relay payload from the `payload_b64` field.
/// Client-side helper — the SFU never calls it.
pub fn decode_payload_b64(payload_b64: &str) -> Result<Vec<u8>, SfuProtoError> {
    Ok(URL_SAFE_NO_PAD.decode(payload_b64.as_bytes())?)
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
                peer_tag: peer_tag("0123456789abcdef0123456789abcdef", "p100acct"),
            }],
            server_ts: 1_700_000_000,
        };
        let json = encode_server_frame(&frame).expect("encode");
        assert!(json.contains("\"kind\":\"welcome\""));
        let back = decode_server_frame(&json).expect("decode");
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
        let json = encode_client_frame(&frame).expect("encode");
        assert!(json.contains("\"kind\":\"signal-relay\""));
        assert!(json.contains("\"to\":{\"kind\":\"peer\""));
    }

    #[test]
    fn relay_target_call_round_trip() {
        let t = RelayTarget::Call;
        let json = serde_json::to_string(&t).expect("encode");
        assert_eq!(json, r#"{"kind":"call"}"#);
        let back: RelayTarget = serde_json::from_str(&json).expect("decode");
        assert_eq!(back, t);
    }

    #[test]
    fn ping_pong_round_trip() {
        let request = ClientFrame::Ping { nonce: 42 };
        let reply = ServerFrame::Pong { nonce: 42 };
        let request_json = encode_client_frame(&request).expect("encode");
        let reply_json = encode_server_frame(&reply).expect("encode");
        assert!(request_json.contains("\"nonce\":42"));
        assert!(reply_json.contains("\"nonce\":42"));
        let _ = decode_client_frame(&request_json).expect("decode req");
        let _ = decode_server_frame(&reply_json).expect("decode reply");
    }

    #[test]
    fn error_codes_serialise_kebab() {
        let frame = ServerFrame::Error {
            code: ErrorCode::FrameTooLarge,
            detail: "exceeds 512 KiB".into(),
        };
        let json = encode_server_frame(&frame).expect("encode");
        assert!(json.contains("\"code\":\"frame-too-large\""));
    }

    #[test]
    fn sdp_offer_round_trip_kebab() {
        let frame = ClientFrame::SdpOffer {
            sdp: "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\n".into(),
        };
        let json = encode_client_frame(&frame).expect("encode");
        assert!(json.contains("\"kind\":\"sdp-offer\""), "got {json}");
        let back = decode_client_frame(&json).expect("decode");
        assert_eq!(back, frame);
    }

    #[test]
    fn sdp_answer_round_trip_kebab() {
        let frame = ServerFrame::SdpAnswer {
            sdp: "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\n".into(),
        };
        let json = encode_server_frame(&frame).expect("encode");
        assert!(json.contains("\"kind\":\"sdp-answer\""), "got {json}");
        let back = decode_server_frame(&json).expect("decode");
        assert_eq!(back, frame);
    }

    #[test]
    fn ice_candidate_round_trip_kebab() {
        let frame = ClientFrame::IceCandidate {
            candidate_sdp: "candidate:1 1 udp 1677729535 192.0.2.1 60769 typ srflx".into(),
        };
        let json = encode_client_frame(&frame).expect("encode");
        assert!(json.contains("\"kind\":\"ice-candidate\""), "got {json}");
        let back = decode_client_frame(&json).expect("decode");
        assert_eq!(back, frame);
    }

    #[test]
    fn unknown_kind_is_rejected() {
        let raw = r#"{"kind":"future-frame","x":1}"#;
        assert!(decode_client_frame(raw).is_err());
    }

    #[test]
    fn frame_too_large_is_rejected() {
        let big = format!(
            r#"{{"kind":"ping","nonce":1,"pad":"{}"}}"#,
            "a".repeat(MAX_INBOUND_FRAME_BYTES)
        );
        match decode_client_frame(&big) {
            Err(SfuProtoError::FrameTooLarge(n)) => assert!(n > MAX_INBOUND_FRAME_BYTES),
            other => panic!("expected FrameTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn payload_b64_round_trip() {
        let payload = b"\x00\x01\x02\xfe\xff opaque SFrame envelope";
        let encoded = encode_payload_b64(payload);
        // base64url alphabet: no '+' / '/' / '=' padding.
        assert!(!encoded.contains('+') && !encoded.contains('/') && !encoded.contains('='));
        let decoded = decode_payload_b64(&encoded).expect("decode");
        assert_eq!(decoded, payload);
    }

    #[test]
    fn payload_b64_rejects_non_base64url() {
        assert!(decode_payload_b64("not valid base64!!").is_err());
    }

    // -- ADR-0021 per-call peer tag ----------------------------------

    const ACCT_A: &str = "deadbeefdeadbeefdeadbeefdeadbeef";

    #[test]
    fn peer_tag_is_deterministic() {
        assert_eq!(peer_tag("call-1", ACCT_A), peer_tag("call-1", ACCT_A));
    }

    #[test]
    fn peer_tag_shape_is_lowercase_hex() {
        let t = peer_tag("call-1", ACCT_A);
        assert_eq!(t.len(), PEER_TAG_LEN * 2);
        assert!(t.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)));
    }

    #[test]
    fn peer_tag_is_per_call_unlinkable() {
        // Same account, different call → different tag.
        assert_ne!(peer_tag("call-1", ACCT_A), peer_tag("call-2", ACCT_A));
    }

    #[test]
    fn peer_tag_distinguishes_accounts() {
        assert_ne!(
            peer_tag("call-1", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            peer_tag("call-1", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        );
    }

    #[test]
    fn peer_tag_length_framing_avoids_concatenation_ambiguity() {
        // ("ab","c") vs ("a","bc") must not collide.
        assert_ne!(peer_tag("ab", "c"), peer_tag("a", "bc"));
    }

    #[test]
    fn peer_tag_golden_vector() {
        // Pins the canonical derivation so the SFU + every client agree.
        assert_eq!(
            peer_tag("call-golden", "00000000000000000000000000000000"),
            PEER_TAG_GOLDEN,
        );
    }

    const PEER_TAG_GOLDEN: &str = "2151ad585010e90b";

    #[test]
    fn peer_snapshot_decodes_without_peer_tag() {
        // serde(default) — a pre-ADR-0021 frame (no peer_tag) still parses.
        let json = r#"{"peer_id":"p1","account_pseudonym":"deadbeef…"}"#;
        let snap: PeerSnapshot = serde_json::from_str(json).expect("decode");
        assert_eq!(snap.peer_tag, "");
        assert_eq!(snap.peer_id, "p1");
    }
}
