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
    decode_payload_b64, decode_server_frame, encode_client_frame, encode_payload_b64, ClientFrame,
    ErrorCode, RelayTarget, ServerFrame, VideoQuality, VideoWant,
};

use crate::ui_rules::TileVideoQuality;
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
    /// the full `sub`). Legacy display tag — superseded by `peer_tag`
    /// (ADR-0021); kept during the rollout.
    pub account_pseudonym: String,
    /// Per-call peer tag (ADR-0021). Re-derive the same over a
    /// contact's account id with [`sfu_peer_tag`] to resolve this entry
    /// to a display name. Empty if the SFU predates ADR-0021.
    pub peer_tag: String,
}

impl From<lakko_sfu_proto::PeerSnapshot> for SfuPeerSnapshot {
    fn from(p: lakko_sfu_proto::PeerSnapshot) -> Self {
        Self {
            peer_id: p.peer_id,
            account_pseudonym: p.account_pseudonym,
            peer_tag: p.peer_tag,
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
    /// The call ended before this socket arrived; the SFU sent this in
    /// place of the Welcome and closed the socket.
    CallEnded,
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
            ErrorCode::CallEnded => Self::CallEnded,
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
        /// Pseudonymised account id (first 8 hex chars). Legacy —
        /// superseded by `peer_tag` (ADR-0021).
        account_pseudonym: String,
        /// Per-call peer tag (ADR-0021). Re-derive with
        /// [`sfu_peer_tag`] over a contact's account id to resolve.
        peer_tag: String,
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
    /// What this subscriber is being sent of each publisher's video
    /// (ADR-0070 step 6): the whole set, in the subscriber's order of
    /// importance. A publisher's quality is the one asked for or a smaller
    /// one, never a larger one.
    VideoGiven {
        /// One entry per publisher.
        given: Vec<SfuVideoGiven>,
    },
    /// Which of this publisher's own camera senders somebody is being
    /// sent, or is about to be sent. `false`: nobody is, the client may
    /// stop encoding and uploading that sender; `true`: it must be
    /// sending it. The light sender is always needed and is not here.
    VideoNeeded {
        /// The main (full size) camera sender.
        main: bool,
        /// The middle (half size) camera sender.
        middle: bool,
    },
}

/// One entry of [`SfuServerFrame::VideoGiven`]: the picture of one
/// publisher this subscriber is being sent.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SfuVideoGiven {
    /// The publisher's peer id, as the roster names it.
    pub peer: String,
    /// Which of that publisher's video tracks carries packets (the light
    /// track is sent at every quality).
    pub quality: TileVideoQuality,
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
                peer_tag,
            } => Self::PeerJoined {
                peer_id,
                account_pseudonym,
                peer_tag,
            },
            ServerFrame::PeerLeft { peer_id } => Self::PeerLeft { peer_id },
            ServerFrame::SignalRelay { from, payload_b64 } => {
                Self::SignalRelay { from, payload_b64 }
            },
            ServerFrame::Error { code, detail } => Self::Error {
                code: code.into(),
                detail,
            },
            ServerFrame::Pong { nonce } => Self::Pong { nonce },
            ServerFrame::SdpAnswer { sdp } => Self::SdpAnswer { sdp },
            ServerFrame::SdpOffer { sdp } => Self::SdpOffer { sdp },
            ServerFrame::VideoGiven { given } => Self::VideoGiven {
                given: given
                    .into_iter()
                    .map(|entry| SfuVideoGiven {
                        peer: entry.peer,
                        quality: match entry.quality {
                            VideoQuality::Main => TileVideoQuality::Main,
                            VideoQuality::Middle => TileVideoQuality::Middle,
                            VideoQuality::Light => TileVideoQuality::Light,
                        },
                    })
                    .collect(),
            },
            ServerFrame::VideoNeeded { main, middle } => Self::VideoNeeded { main, middle },
        }
    }
}

/// Decode an inbound server frame off the `lakko-sfu-v1` WebSocket.
#[uniffi::export]
pub fn parse_sfu_server_frame(json: String) -> Result<SfuServerFrame, FfiError> {
    Ok(decode_server_frame(&json)?.into())
}

/// Re-derive the per-call peer tag (ADR-0021) for a `(call_id,
/// account_id)` pair — the same canonical derivation the SFU applies.
///
/// The client computes this over each of its contacts' account ids and
/// matches the result against a roster entry's `peer_tag` to resolve it
/// to a display name. The tag is one-way (reveals nothing about the
/// account id) and per-call (keyed on `call_id`).
#[uniffi::export]
#[must_use]
pub fn sfu_peer_tag(call_id: String, account_id: String) -> String {
    lakko_sfu_proto::peer_tag(&call_id, &account_id)
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
///
/// `light_mids` names the `mid`s of the offer's video m-lines that carry
/// a light copy of the sender's camera (the same picture scaled down, on
/// its own m-line beside the main one); the SFU forwards such a track as
/// the sender's light companion. Empty for an offer without one. More
/// than `lakko_sfu_proto::MAX_LIGHT_MIDS` entries, an empty entry or one
/// longer than `lakko_sfu_proto::MAX_LIGHT_MID_BYTES` bytes is an error.
#[uniffi::export]
pub fn compose_sfu_sdp_offer(sdp: String, light_mids: Vec<String>) -> Result<String, FfiError> {
    compose_sfu_sdp_offer_with_middle(sdp, light_mids, Vec::new())
}

/// Compose a client → SFU SDP offer that also names its middle track
/// (ADR-0070 step 5).
///
/// `middle_mids` names the `mid`s of the offer's video m-lines that carry
/// the middle copy of the sender's camera (the same picture at half size,
/// on its own m-line); the SFU forwards such a track only to a subscriber
/// whose `video-wants` asks for it. The bounds are those of `light_mids`;
/// outside them this is an error. With an empty `middle_mids` the frame is
/// exactly [`compose_sfu_sdp_offer`]'s.
#[uniffi::export]
pub fn compose_sfu_sdp_offer_with_middle(
    sdp: String,
    light_mids: Vec<String>,
    middle_mids: Vec<String>,
) -> Result<String, FfiError> {
    Ok(encode_client_frame(&ClientFrame::SdpOffer {
        sdp,
        light_mids,
        middle_mids,
    })?)
}

/// One entry of a `video-wants` frame: the picture of one publisher this
/// subscriber shows.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SfuVideoWant {
    /// The publisher's peer id, as the roster names it.
    pub peer: String,
    /// Which of that publisher's video tracks is shown
    /// ([`crate::ui_rules::tile_video_quality`]).
    pub quality: TileVideoQuality,
}

/// Compose a client → SFU `video-wants` frame (ADR-0070 step 5): which
/// picture of each publisher this subscriber shows. Each frame replaces
/// the whole set; a publisher left out is `Main`. An error when the list
/// is outside the protocol's bounds (more than
/// `lakko_sfu_proto::MAX_VIDEO_WANTS` entries, an empty peer id or one
/// longer than `lakko_sfu_proto::MAX_VIDEO_WANT_PEER_BYTES` bytes).
#[uniffi::export]
pub fn compose_sfu_video_wants(wants: Vec<SfuVideoWant>) -> Result<String, FfiError> {
    let wants = wants
        .into_iter()
        .map(|want| VideoWant {
            peer: want.peer,
            quality: match want.quality {
                TileVideoQuality::Main => VideoQuality::Main,
                TileVideoQuality::Middle => VideoQuality::Middle,
                TileVideoQuality::Light => VideoQuality::Light,
            },
        })
        .collect();
    Ok(encode_client_frame(&ClientFrame::VideoWants { wants })?)
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
    Ok(encode_client_frame(&ClientFrame::IceCandidate {
        candidate_sdp,
    })?)
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
            },
            _ => panic!("expected Welcome"),
        }
    }

    #[test]
    fn error_frame_maps_code() {
        let json = r#"{"kind":"error","code":"bad-sdp","detail":"nope"}"#;
        match parse_sfu_server_frame(json.into()).expect("parse") {
            SfuServerFrame::Error { code, .. } => {
                assert!(matches!(code, SfuErrorCode::BadSdp));
            },
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
            },
            _ => panic!("expected SignalRelay to peer"),
        }
    }

    #[test]
    fn compose_sdp_offer_kebab() {
        let json = compose_sfu_sdp_offer("v=0\r\n".into(), Vec::new()).expect("compose");
        assert!(json.contains("\"kind\":\"sdp-offer\""), "got {json}");
        assert!(!json.contains("light_mids"), "got {json}");
    }

    #[test]
    fn compose_sdp_offer_carries_the_light_mids() {
        let json = compose_sfu_sdp_offer("v=0\r\n".into(), vec!["2".into()]).expect("compose");
        match lakko_sfu_proto::decode_client_frame(&json).expect("decode") {
            ClientFrame::SdpOffer {
                sdp,
                light_mids,
                middle_mids,
            } => {
                assert_eq!(sdp, "v=0\r\n");
                assert_eq!(light_mids, vec!["2".to_owned()]);
                assert!(middle_mids.is_empty());
            },
            _ => panic!("expected SdpOffer"),
        }
        assert!(!json.contains("middle_mids"), "got {json}");
        // Outside the bound: not composed.
        let many = vec!["1".to_owned(); lakko_sfu_proto::MAX_LIGHT_MIDS + 1];
        assert!(compose_sfu_sdp_offer("v=0\r\n".into(), many).is_err());
    }

    #[test]
    fn compose_sdp_offer_carries_the_middle_mids() {
        let json =
            compose_sfu_sdp_offer_with_middle("v=0\r\n".into(), vec!["2".into()], vec!["3".into()])
                .expect("compose");
        match lakko_sfu_proto::decode_client_frame(&json).expect("decode") {
            ClientFrame::SdpOffer {
                light_mids,
                middle_mids,
                ..
            } => {
                assert_eq!(light_mids, vec!["2".to_owned()]);
                assert_eq!(middle_mids, vec!["3".to_owned()]);
            },
            _ => panic!("expected SdpOffer"),
        }
        let many = vec!["1".to_owned(); lakko_sfu_proto::MAX_LIGHT_MIDS + 1];
        assert!(compose_sfu_sdp_offer_with_middle("v=0\r\n".into(), Vec::new(), many).is_err());
    }

    #[test]
    fn compose_video_wants_round_trips_and_refuses_what_the_server_would() {
        let want = |peer: &str, quality| SfuVideoWant {
            peer: peer.to_owned(),
            quality,
        };
        let json = compose_sfu_video_wants(vec![
            want("p1", TileVideoQuality::Main),
            want("p2", TileVideoQuality::Middle),
            want("p3", TileVideoQuality::Light),
        ])
        .expect("compose");
        assert_eq!(
            json,
            r#"{"kind":"video-wants","wants":[{"peer":"p1","quality":"main"},{"peer":"p2","quality":"middle"},{"peer":"p3","quality":"light"}]}"#
        );
        assert!(lakko_sfu_proto::decode_client_frame(&json).is_ok());
        assert_eq!(
            compose_sfu_video_wants(Vec::new()).expect("empty set"),
            r#"{"kind":"video-wants","wants":[]}"#
        );
        assert!(compose_sfu_video_wants(vec![want("", TileVideoQuality::Main)]).is_err());
        let many = vec![want("p", TileVideoQuality::Light); lakko_sfu_proto::MAX_VIDEO_WANTS + 1];
        assert!(compose_sfu_video_wants(many).is_err());
    }

    #[test]
    fn video_given_parses_into_typed_frame() {
        let json = r#"{"kind":"video-given","given":[{"peer":"p1","quality":"main"},{"peer":"p2","quality":"middle"},{"peer":"p3","quality":"light"}]}"#;
        match parse_sfu_server_frame(json.to_owned()).expect("parses") {
            SfuServerFrame::VideoGiven { given } => assert_eq!(
                given,
                vec![
                    SfuVideoGiven {
                        peer: "p1".into(),
                        quality: TileVideoQuality::Main,
                    },
                    SfuVideoGiven {
                        peer: "p2".into(),
                        quality: TileVideoQuality::Middle,
                    },
                    SfuVideoGiven {
                        peer: "p3".into(),
                        quality: TileVideoQuality::Light,
                    },
                ]
            ),
            _ => panic!("expected VideoGiven"),
        }
        // Out of bounds, as the wire crate refuses it.
        assert!(parse_sfu_server_frame(
            r#"{"kind":"video-given","given":[{"peer":"","quality":"main"}]}"#.to_owned()
        )
        .is_err());
    }

    #[test]
    fn video_needed_parses_into_typed_frame() {
        for (main, middle) in [(true, true), (false, true), (true, false), (false, false)] {
            let json = format!(r#"{{"kind":"video-needed","main":{main},"middle":{middle}}}"#);
            match parse_sfu_server_frame(json).expect("parses") {
                SfuServerFrame::VideoNeeded { main: m, middle: d } => {
                    assert_eq!((m, d), (main, middle));
                },
                _ => panic!("expected VideoNeeded"),
            }
        }
        // Both flags are required, and they are booleans.
        assert!(
            parse_sfu_server_frame(r#"{"kind":"video-needed","main":true}"#.to_owned()).is_err()
        );
        assert!(parse_sfu_server_frame(
            r#"{"kind":"video-needed","main":1,"middle":0}"#.to_owned()
        )
        .is_err());
    }

    #[test]
    fn parse_rejects_unknown_kind() {
        assert!(parse_sfu_server_frame(r#"{"kind":"nope"}"#.into()).is_err());
    }
}
