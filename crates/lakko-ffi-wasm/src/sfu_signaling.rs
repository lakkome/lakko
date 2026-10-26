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
    decode_payload_b64, decode_server_frame, encode_client_frame, encode_payload_b64, ClientFrame,
    RelayTarget, ServerFrame, VideoQuality, VideoWant,
};
use wasm_bindgen::prelude::*;

fn to_err(e: lakko_sfu_proto::SfuProtoError) -> JsError {
    JsError::new(&e.to_string())
}

/// Validate an inbound server frame off the `lakko-sfu-v1`
/// WebSocket and return its `kind` discriminator (kebab-case:
/// `"welcome"`, `"peer-joined"`, `"peer-left"`, `"signal-relay"`,
/// `"error"`, `"pong"`, `"sdp-answer"`, `"sdp-offer"`, `"video-given"`,
/// `"video-needed"`, `"publish-budget"`).
/// A `publish-budget` frame carries two required integers, `main_kbps`
/// and `middle_kbps`: the ceiling the SFU sets for this publisher's own
/// main and middle camera senders (`parseSfuPublishBudget`).
/// A `video-needed` frame carries two required booleans, `main` and
/// `middle`: which of this publisher's own camera senders somebody is
/// being sent (`parseSfuVideoNeeded`).
/// A `video-given` frame (ADR-0070 step 6) carries `given`, a list of
/// `{ peer, quality }` with `quality` one of `"main"`, `"middle"`,
/// `"light"`: what this subscriber is being sent of each publisher's
/// video, the whole set, validated here against the protocol's bounds.
/// Throws on a
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
        ServerFrame::VideoGiven { .. } => "video-given",
        ServerFrame::VideoNeeded { .. } => "video-needed",
        ServerFrame::PublishBudget { .. } => "publish-budget",
    };
    Ok(kind.to_string())
}

/// The two flags of a `video-needed` server frame as a two-byte array
/// `[main, middle]`, each `1` (somebody is being sent, or is about to be
/// sent, that camera sender of this publisher: keep it encoding) or `0`
/// (nobody is: the client may stop encoding and uploading it). The light
/// sender is always needed and is not in the frame. Throws when the JSON
/// is not a valid `video-needed` frame.
#[wasm_bindgen(js_name = "parseSfuVideoNeeded")]
pub fn parse_sfu_video_needed(json: &str) -> Result<Vec<u8>, JsError> {
    video_needed_flags(json).map_err(|e| JsError::new(&e))
}

/// [`parse_sfu_video_needed`] without the `JsError`, so a host test can
/// run it.
fn video_needed_flags(json: &str) -> Result<Vec<u8>, String> {
    match decode_server_frame(json).map_err(|e| e.to_string())? {
        ServerFrame::VideoNeeded { main, middle } => Ok(vec![u8::from(main), u8::from(middle)]),
        other => Err(format!(
            "expected a video-needed frame, got {}",
            other.kind_str()
        )),
    }
}

/// The two ceilings of a `publish-budget` server frame (ADR-0068,
/// amendment of 2026-10-11) as a two-element `Uint32Array`
/// `[main_kbps, middle_kbps]`: the most this publisher's own main and
/// middle camera sender may send, in kilobits per second, each at least 1
/// and at most `MAX_PUBLISH_BUDGET_KBPS` (20000). The client sets each as
/// that sender's maximum bitrate (`maxBitrate` is in bits per second:
/// multiply by 1000). The light sender and audio are not in the frame and
/// are not limited by it. While the publisher sends a screen (ADR-0083)
/// the array has a third element, the screen sender's ceiling. Throws when
/// the JSON is not a valid `publish-budget` frame.
#[wasm_bindgen(js_name = "parseSfuPublishBudget")]
pub fn parse_sfu_publish_budget(json: &str) -> Result<Vec<u32>, JsError> {
    publish_budget_ceilings(json).map_err(|e| JsError::new(&e))
}

/// [`parse_sfu_publish_budget`] without the `JsError`, so a host test can
/// run it.
fn publish_budget_ceilings(json: &str) -> Result<Vec<u32>, String> {
    match decode_server_frame(json).map_err(|e| e.to_string())? {
        ServerFrame::PublishBudget {
            main_kbps,
            middle_kbps,
            screen_kbps,
        } => Ok(screen_kbps.map_or_else(
            || vec![main_kbps, middle_kbps],
            |screen| vec![main_kbps, middle_kbps, screen],
        )),
        other => Err(format!(
            "expected a publish-budget frame, got {}",
            other.kind_str()
        )),
    }
}

/// The `given` list of a `video-given` server frame (ADR-0070 step 6) as
/// two parallel lists flattened into one: `[peer0, quality0, peer1,
/// quality1, ...]`, each quality `"main"`, `"middle"` or `"light"`
/// (`tileVideoQualityName`), in the frame's order (the subscriber's order
/// of importance). Throws when the JSON is not a valid `video-given`
/// frame.
#[wasm_bindgen(js_name = "parseSfuVideoGiven")]
pub fn parse_sfu_video_given(json: &str) -> Result<Vec<String>, JsError> {
    video_given_pairs(json).map_err(|e| JsError::new(&e))
}

/// [`parse_sfu_video_given`] without the `JsError`, so a host test can run
/// it.
fn video_given_pairs(json: &str) -> Result<Vec<String>, String> {
    match decode_server_frame(json).map_err(|e| e.to_string())? {
        ServerFrame::VideoGiven { given } => Ok(given
            .into_iter()
            .flat_map(|entry| [entry.peer, entry.quality.as_str().to_owned()])
            .collect()),
        other => Err(format!(
            "expected a video-given frame, got {}",
            other.kind_str()
        )),
    }
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
///
/// `light_mids` (`string[]`) names the `mid`s of the offer's video
/// m-lines that carry a light copy of the sender's camera (the same
/// picture scaled down, on its own m-line beside the main one); the SFU
/// forwards such a track as the sender's light companion. Pass `[]` for
/// an offer without one. More than `lakko_sfu_proto::MAX_LIGHT_MIDS`
/// entries, an empty entry or one longer than
/// `lakko_sfu_proto::MAX_LIGHT_MID_BYTES` bytes throws.
#[wasm_bindgen(js_name = "composeSfuSdpOffer")]
pub fn compose_sfu_sdp_offer(sdp: &str, light_mids: Vec<String>) -> Result<String, JsError> {
    compose_sfu_sdp_offer_with_middle(sdp, light_mids, Vec::new())
}

/// Compose a client → SFU SDP offer that also names its middle track
/// (ADR-0070 step 5).
///
/// `middle_mids` (`string[]`) names the `mid`s of the offer's video
/// m-lines that carry the middle copy of the sender's camera (the same
/// picture at half size, on its own m-line); the SFU forwards such a
/// track only to a subscriber whose `video-wants` asks for it. The
/// bounds are those of `light_mids`; outside them this throws. With an
/// empty `middle_mids` the frame is exactly `composeSfuSdpOffer`'s.
#[wasm_bindgen(js_name = "composeSfuSdpOfferWithMiddle")]
pub fn compose_sfu_sdp_offer_with_middle(
    sdp: &str,
    light_mids: Vec<String>,
    middle_mids: Vec<String>,
) -> Result<String, JsError> {
    encode_client_frame(&ClientFrame::SdpOffer {
        sdp: sdp.to_string(),
        light_mids,
        middle_mids,
        screen_mids: Vec::new(),
    })
    .map_err(to_err)
}

/// Compose a client -> SFU SDP offer that also names its screen-share
/// lines (ADR-0083). `screen_mids` (`string[]`) are the `mid`s of the
/// share's picture and, when it has one, its sound; the SFU forwards them
/// under the stream id `<peer>~screen`. The bounds are those of
/// `light_mids`; outside them this throws. With an empty `screen_mids` the
/// frame is exactly `composeSfuSdpOfferWithMiddle`'s.
#[wasm_bindgen(js_name = "composeSfuSdpOfferWithScreen")]
pub fn compose_sfu_sdp_offer_with_screen(
    sdp: &str,
    light_mids: Vec<String>,
    middle_mids: Vec<String>,
    screen_mids: Vec<String>,
) -> Result<String, JsError> {
    encode_client_frame(&ClientFrame::SdpOffer {
        sdp: sdp.to_string(),
        light_mids,
        middle_mids,
        screen_mids,
    })
    .map_err(to_err)
}

/// Compose a client → SFU `video-wants` frame (ADR-0070 step 5): which
/// picture of each publisher this subscriber shows.
///
/// `peers` and `qualities` are parallel `string[]`s: `qualities[i]` is
/// `"main"`, `"middle"` or `"light"` (`tileVideoQualityName`) for the
/// publisher `peers[i]`. Each frame replaces the whole set; a publisher
/// left out is `main`. Throws when the two lengths differ, a quality is
/// none of the three, or the list is outside the protocol's bounds (more
/// than `lakko_sfu_proto::MAX_VIDEO_WANTS` entries, an empty peer id or
/// one longer than `lakko_sfu_proto::MAX_VIDEO_WANT_PEER_BYTES` bytes).
#[wasm_bindgen(js_name = "composeSfuVideoWants")]
pub fn compose_sfu_video_wants(
    peers: Vec<String>,
    qualities: Vec<String>,
) -> Result<String, JsError> {
    video_wants_json(peers, &qualities).map_err(|reason| JsError::new(&reason))
}

/// The body of [`compose_sfu_video_wants`], with its refusals as plain
/// strings so they can be tested on a host target.
fn video_wants_json(peers: Vec<String>, qualities: &[String]) -> Result<String, String> {
    if peers.len() != qualities.len() {
        return Err("peers and qualities differ in length".to_owned());
    }
    let wants = peers
        .into_iter()
        .zip(qualities)
        .map(|(peer, quality)| {
            VideoQuality::from_wire(quality)
                .map(|quality| VideoWant { peer, quality })
                .ok_or_else(|| "quality is none of main, middle, light".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    encode_client_frame(&ClientFrame::VideoWants { wants }).map_err(|e| e.to_string())
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
        let json = r#"{"kind":"welcome","peer_id":"p1","room":"r1","roster":[],"server_ts":1}"#;
        assert_eq!(validate_sfu_server_frame(json).expect("ok"), "welcome");
    }

    #[test]
    fn video_given_validates_and_flattens() {
        let json = r#"{"kind":"video-given","given":[{"peer":"p1","quality":"main"},{"peer":"p2","quality":"light"}]}"#;
        assert_eq!(validate_sfu_server_frame(json).expect("ok"), "video-given");
        assert_eq!(
            video_given_pairs(json).expect("ok"),
            vec!["p1", "main", "p2", "light"]
        );
        assert!(video_given_pairs(r#"{"kind":"pong","nonce":1}"#).is_err());
        assert!(video_given_pairs(
            r#"{"kind":"video-given","given":[{"peer":"","quality":"main"}]}"#
        )
        .is_err());
    }

    #[test]
    fn publish_budget_validates_and_flattens() {
        let frame = |main: &str, middle: &str| {
            format!(r#"{{"kind":"publish-budget","main_kbps":{main},"middle_kbps":{middle}}}"#)
        };
        assert_eq!(
            validate_sfu_server_frame(&frame("2500", "500")).expect("valid"),
            "publish-budget"
        );
        assert_eq!(
            publish_budget_ceilings(&frame("2500", "500")).expect("ok"),
            [2500, 500]
        );
        assert_eq!(
            publish_budget_ceilings(&frame("1", "20000")).expect("ok"),
            [1, 20000]
        );
        // Another frame, a missing field, zero, above the bound, not an
        // integer.
        assert!(publish_budget_ceilings(r#"{"kind":"pong","nonce":1}"#).is_err());
        assert!(publish_budget_ceilings(r#"{"kind":"publish-budget","main_kbps":900}"#).is_err());
        assert!(publish_budget_ceilings(&frame("0", "500")).is_err());
        assert!(publish_budget_ceilings(&frame("900", "0")).is_err());
        assert!(publish_budget_ceilings(&frame("20001", "500")).is_err());
        assert!(publish_budget_ceilings(&frame("900.5", "500")).is_err());
        assert!(publish_budget_ceilings(&frame("\"900\"", "500")).is_err());
    }

    #[test]
    fn video_needed_validates_and_flattens() {
        let frame = |main: bool, middle: bool| {
            format!(r#"{{"kind":"video-needed","main":{main},"middle":{middle}}}"#)
        };
        assert_eq!(
            validate_sfu_server_frame(&frame(true, false)).expect("ok"),
            "video-needed"
        );
        assert_eq!(video_needed_flags(&frame(true, true)).expect("ok"), [1, 1]);
        assert_eq!(video_needed_flags(&frame(false, true)).expect("ok"), [0, 1]);
        assert_eq!(video_needed_flags(&frame(true, false)).expect("ok"), [1, 0]);
        assert_eq!(
            video_needed_flags(&frame(false, false)).expect("ok"),
            [0, 0]
        );
        assert!(video_needed_flags(r#"{"kind":"pong","nonce":1}"#).is_err());
        assert!(video_needed_flags(r#"{"kind":"video-needed","main":true}"#).is_err());
        assert!(video_needed_flags(r#"{"kind":"video-needed","main":1,"middle":0}"#).is_err());
    }

    // The unknown-kind / malformed rejection path constructs a
    // `JsError`, which cannot be exercised on a non-wasm host target
    // (`cannot call wasm-bindgen imported functions on non-wasm
    // targets`). The rejection itself is covered by
    // `lakko_sfu_proto`'s own `unknown_kind_is_rejected` test.

    #[test]
    fn compose_sdp_offer_kebab() {
        let json = compose_sfu_sdp_offer("v=0\r\n", Vec::new()).expect("compose");
        assert!(json.contains("\"kind\":\"sdp-offer\""), "got {json}");
        assert!(!json.contains("light_mids"), "got {json}");
    }

    // The out-of-bounds refusal constructs a `JsError` and so cannot run
    // on a host target; `lakko_sfu_proto` covers it.
    #[test]
    fn compose_sdp_offer_carries_the_light_mids() {
        let json = compose_sfu_sdp_offer("v=0\r\n", vec!["2".into()]).expect("compose");
        match lakko_sfu_proto::decode_client_frame(&json).expect("decode") {
            ClientFrame::SdpOffer {
                sdp,
                light_mids,
                middle_mids,
                screen_mids,
            } => {
                assert_eq!(sdp, "v=0\r\n");
                assert_eq!(light_mids, vec!["2".to_owned()]);
                assert!(middle_mids.is_empty());
                assert!(screen_mids.is_empty());
            },
            _ => panic!("expected SdpOffer"),
        }
        assert!(!json.contains("middle_mids"), "got {json}");
    }

    #[test]
    fn compose_sdp_offer_carries_the_middle_mids() {
        let json = compose_sfu_sdp_offer_with_middle("v=0\r\n", vec!["2".into()], vec!["3".into()])
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
    }

    #[test]
    fn compose_video_wants_round_trips_and_refuses_what_the_server_would() {
        let json = video_wants_json(
            vec!["p1".into(), "p2".into(), "p3".into()],
            &["main".into(), "middle".into(), "light".into()],
        )
        .expect("compose");
        assert_eq!(
            json,
            r#"{"kind":"video-wants","wants":[{"peer":"p1","quality":"main"},{"peer":"p2","quality":"middle"},{"peer":"p3","quality":"light"}]}"#
        );
        assert!(lakko_sfu_proto::decode_client_frame(&json).is_ok());
        assert_eq!(
            video_wants_json(Vec::new(), &[]).expect("empty set"),
            r#"{"kind":"video-wants","wants":[]}"#
        );
        // Lengths differ, an unknown quality, an empty peer, too many.
        assert!(video_wants_json(vec!["p1".into()], &[]).is_err());
        assert!(video_wants_json(vec!["p1".into()], &["full".into()]).is_err());
        assert!(video_wants_json(vec![String::new()], &["main".into()]).is_err());
        let many = lakko_sfu_proto::MAX_VIDEO_WANTS + 1;
        assert!(video_wants_json(vec!["p".into(); many], &vec!["main".to_owned(); many]).is_err());
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
