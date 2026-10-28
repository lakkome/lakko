// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Slim call-signalling CLASSIFIER — the one thing the NSE needs in order to
//! ring (ADR-0041 Phase 2, for the call wake of `docs/ux/calls.md` §2 + §7).
//!
//! ## Why the NSE needs this at all
//!
//! Call signalling rides the conversation MLS group as an ordinary application
//! message (`lakko_calls::signaling`), exactly like a chat message and like the
//! three service envelopes this crate already recognises. Until now the NSE
//! could not tell a call offer from any other non-chat payload, so every call
//! frame fell into the same bucket and was suppressed — which is precisely why
//! `docs/ux/calls.md` §7 records "all three background paths drop call
//! signalling today, so a closed app never learns about a call" as the single
//! change the whole of §2 depends on. An iPhone does not ring because this
//! parse does not exist.
//!
//! Worse than silence is the alternative: the payload is prost, not JSON, and
//! [`crate::attachments::parse_body_envelope`] has a deliberate legacy fallback
//! that degrades unrecognised bytes to `Chat` via `from_utf8_lossy` — so an
//! unfiltered offer reaches the notification body as a smear of SDP fragments
//! and control bytes. `an_unfiltered_offer_would_otherwise_render_as_chat_text`
//! pins that, the same way the sibling filter modules pin theirs.
//!
//! ## Scope: CLASSIFY, nothing else
//!
//! [`parse_call_signal`] answers three questions and no more — *is this a call
//! being offered to me, is it a call ending, or is it some other call frame* —
//! plus, for an offer, the `call_id` the ring is keyed on and whether the
//! caller asked for video (`docs/ux/calls.md` §2.2: "the word on the callee's
//! screen matches what the caller chose, always").
//!
//! Deliberately ABSENT, and each for a reason:
//!
//! - **No compose.** The NSE cannot answer, decline or end a call: every one of
//!   those is an MLS application message, which advances the sender ratchet —
//!   the write this process structurally must not perform (C-59). The app
//!   answers; the extension only rings.
//! - **No SDP, no ICE candidate, no timestamps.** A ring needs none of them,
//!   and the SDP body is the largest field on the wire. The app re-reads the
//!   same journal row for itself and parses it in full; the NSE's copy is
//!   discarded with the rest of the throwaway state.
//! - **No key material and no SFrame.** This module touches exactly one
//!   function of `lakko-calls`, [`lakko_calls::signaling::parse_signaling_payload`].
//!   The call key, the key ring and the SFrame codec are not referenced here
//!   and the linker dead-strips them.
//!
//! ## What the caller must do with the result
//!
//! Run this arm of the filter chain BEFORE the chat parse and after (or
//! alongside) the service-envelope arms — the isolation tests below pin that no
//! kind accepts another's bytes, so the order among the non-chat arms does not
//! matter, but the chat parse must come last because it never fails.

use lakko_calls::signaling::{parse_signaling_payload, SignalingBody};

use crate::FfiError;

/// What a decrypted call-signalling payload is, reduced to what a ring needs.
///
/// Three classes, mirroring the three decisions the notification path takes:
/// start ringing, stop ringing, or do neither but remember the call this frame
/// belongs to. Every variant carries the 16-byte `call_id` because every
/// signalling body does, and because the stop-ringing rules of
/// `docs/ux/calls.md` §2.1 are all keyed on it — "a call answered on one of the
/// callee's devices stops ringing on the others" is an [`Self::Other`] frame
/// (an answer) whose `call_id` names the ring to cancel.
#[derive(Debug, uniffi::Enum)]
pub enum CallSignal {
    /// A call is being offered to this device — **ring**.
    ///
    /// Covers both the 1x1 SDP offer and the group-call announce; `group`
    /// separates them, because the two have different accept paths and the
    /// distinction is recoverable from nothing else on this surface. Folding
    /// the announce into [`Self::Other`] instead would have meant a group call
    /// never rings, silently, which is the failure this module exists to end.
    Offer {
        /// The 16-byte call id the ring is keyed on.
        call_id: Vec<u8>,
        /// `true` when the caller asked for video. This is the caller's own
        /// choice off the wire, which is what `docs/ux/calls.md` §2.2 requires
        /// the callee's screen to say — not a guess from the media that later
        /// arrives.
        video: bool,
        /// `true` for a group-call announce, `false` for a 1x1 offer.
        group: bool,
    },
    /// The call is over — **stop ringing** and record the outcome.
    ///
    /// The end REASON is deliberately not surfaced: the transcript row it
    /// drives (`docs/ux/calls.md` §6) is written by the app from the same
    /// journal row, and the extension's only decision is to stop.
    End {
        /// The 16-byte id of the call that ended.
        call_id: Vec<u8>,
    },
    /// Any other call frame — an answer, an ICE candidate, a group join or a
    /// group leave. Not a ring and not an end; the `call_id` is carried so the
    /// caller can tell whether the frame belongs to a call it is currently
    /// ringing for.
    Other {
        /// The 16-byte id of the call this frame belongs to.
        call_id: Vec<u8>,
    },
}

/// Classify a decrypted application payload as call signalling.
///
/// Feed it the `data` of a [`crate::ProcessedMessage::Application`]. An `Err`
/// means "not call signalling" and the caller falls through to the next filter
/// arm and finally to the chat parse — on this path that is the EXPECTED,
/// common outcome, not a fault, exactly as it is for the service-envelope arms.
///
/// Fail-closed: a payload that does not prost-decode, that carries no body
/// oneof (an older build reading a future variant), or whose embedded `call_id`
/// is not exactly 16 bytes is refused rather than guessed at.
///
/// # Errors
///
/// * [`FfiError::Call`] when `plaintext` is not a well-formed call-signalling
///   payload.
#[uniffi::export]
pub fn parse_call_signal(plaintext: Vec<u8>) -> Result<CallSignal, FfiError> {
    let body = parse_signaling_payload(&plaintext).map_err(|e| FfiError::Call(e.to_string()))?;
    let call_id = body.call_id().to_vec();
    Ok(match body {
        SignalingBody::Offer { media, .. } => CallSignal::Offer {
            call_id,
            video: media.video,
            group: false,
        },
        SignalingBody::GroupStart { media, .. } => CallSignal::Offer {
            call_id,
            video: media.video,
            group: true,
        },
        SignalingBody::End { .. } => CallSignal::End { call_id },
        SignalingBody::Answer { .. }
        | SignalingBody::IceCandidate { .. }
        | SignalingBody::GroupLeave { .. }
        | SignalingBody::GroupJoin { .. }
        // A one-to-one screen-share state (ADR-0083) never rings.
        | SignalingBody::ScreenShare { .. } => CallSignal::Other { call_id },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lakko_calls::signaling::{compose_signaling_payload, EndReason, MediaFlags};

    fn call_id() -> [u8; 16] {
        let mut id = [0u8; 16];
        id[0] = 0xAB;
        id[15] = 0xCD;
        id
    }

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut out, b| {
            // Writing to a `String` is infallible.
            let _ = write!(out, "{b:02x}");
            out
        })
    }

    fn offer_wire(media: MediaFlags) -> Vec<u8> {
        compose_signaling_payload(&SignalingBody::Offer {
            call_id: call_id(),
            sdp: "v=0".into(),
            created_at_ms: 1_700_000_000_000,
            media,
        })
        .expect("compose an offer")
    }

    /// A video offer classifies as a ring, names its call, and reports the
    /// caller's OWN choice of video — the fact `docs/ux/calls.md` §2.2 requires
    /// the callee's screen to be told.
    #[test]
    fn a_video_offer_rings_and_reports_video() {
        match parse_call_signal(offer_wire(MediaFlags::audio_video())).expect("an offer classifies")
        {
            CallSignal::Offer {
                call_id: id,
                video,
                group,
            } => {
                assert_eq!(id, call_id().to_vec());
                assert!(video, "the caller asked for video");
                assert!(!group, "a 1x1 offer is not a group announce");
            },
            other => panic!("expected an Offer, got {other:?}"),
        }
    }

    /// A voice offer must NOT report video. The whole point of carrying the
    /// flag is that the callee is told what the caller chose; a voice call that
    /// announces itself as video is the defect §2.2 names.
    #[test]
    fn a_voice_offer_does_not_report_video() {
        match parse_call_signal(offer_wire(MediaFlags::audio_only())).expect("an offer classifies")
        {
            CallSignal::Offer { video, .. } => assert!(!video, "a voice call is not a video call"),
            other => panic!("expected an Offer, got {other:?}"),
        }
    }

    /// A group-call announce rings too, and says it is a group. Folding it into
    /// `Other` would mean a group call never rings — pinned so that choice
    /// cannot be reversed silently.
    #[test]
    fn a_group_announce_rings_and_says_it_is_a_group() {
        let wire = compose_signaling_payload(&SignalingBody::GroupStart {
            call_id: call_id(),
            initiator_leaf: 3,
            created_at_ms: 1_700_000_000_000,
            media: MediaFlags::audio_video(),
        })
        .expect("compose a group start");
        match parse_call_signal(wire).expect("an announce classifies") {
            CallSignal::Offer { group, video, .. } => {
                assert!(group, "a group announce reports as a group");
                assert!(video, "and still carries the caller's media choice");
            },
            other => panic!("expected an Offer, got {other:?}"),
        }
    }

    /// An end frame stops the ring and names the call it ends.
    #[test]
    fn an_end_frame_classifies_as_end() {
        let wire = compose_signaling_payload(&SignalingBody::End {
            call_id: call_id(),
            reason: EndReason::Declined,
        })
        .expect("compose an end");
        match parse_call_signal(wire).expect("an end classifies") {
            CallSignal::End { call_id: id } => assert_eq!(id, call_id().to_vec()),
            other => panic!("expected an End, got {other:?}"),
        }
    }

    /// Every remaining body is `Other` and still carries its `call_id` — which
    /// is what lets "answered on another device stops this one ringing"
    /// (`docs/ux/calls.md` §2.1) work at all.
    #[test]
    fn every_other_frame_is_other_and_keeps_its_call_id() {
        let bodies = [
            SignalingBody::Answer {
                call_id: call_id(),
                sdp: "v=0".into(),
            },
            SignalingBody::IceCandidate {
                call_id: call_id(),
                candidate: "candidate:1 1 udp 1 1.2.3.4 1 typ host".into(),
                sdp_mid: "audio".into(),
                sdp_m_line_index: 0,
            },
            SignalingBody::GroupJoin {
                call_id: call_id(),
                leaf_index: 1,
                joined_at_ms: 1,
            },
            SignalingBody::GroupLeave {
                call_id: call_id(),
                leaf_index: 1,
                reason: EndReason::HungUp,
            },
        ];
        for body in bodies {
            let wire = compose_signaling_payload(&body).expect("compose");
            match parse_call_signal(wire).expect("classifies") {
                CallSignal::Other { call_id: id } => assert_eq!(id, call_id().to_vec()),
                other => panic!("expected Other for {body:?}, got {other:?}"),
            }
        }
    }

    /// The classifier must fail on a real chat envelope, or the NSE would treat
    /// every message as a call and ring for text.
    #[test]
    fn refuses_a_chat_envelope() {
        let chat = lakko_attachments::compose("hello", Vec::new()).expect("compose chat");
        assert!(matches!(parse_call_signal(chat), Err(FfiError::Call(_))));
    }

    /// Cross-kind isolation at the slim boundary: the call arm and the two
    /// service-envelope arms must not accept each other's bytes, or the filter
    /// chain would classify one kind as another.
    #[test]
    fn does_not_cross_accept_the_service_envelopes() {
        let beacon = lakko_contacts::committer_beacon::compose_committer_beacon();
        let routing = lakko_contacts::push_routing::compose_push_routing_secret(7, &[0x42u8; 32]);

        assert!(matches!(
            parse_call_signal(beacon.clone()),
            Err(FfiError::Call(_))
        ));
        assert!(matches!(
            parse_call_signal(routing.clone()),
            Err(FfiError::Call(_))
        ));

        let offer = offer_wire(MediaFlags::audio_only());
        assert!(matches!(
            crate::committer_beacon::decode_committer_beacon(offer.clone()),
            Err(FfiError::Contacts(_)),
        ));
        assert!(matches!(
            crate::push_routing::decode_push_routing_secret(offer),
            Err(FfiError::Contacts(_)),
        ));
        // ...and the service envelopes still decode on their own arms, so the
        // isolation above is not passing merely because everything fails.
        crate::committer_beacon::decode_committer_beacon(beacon).expect("beacon still decodes");
        crate::push_routing::decode_push_routing_secret(routing).expect("routing still decodes");
    }

    /// Malformed input is refused rather than guessed at: truncated bytes, a
    /// payload with no body oneof, and — the one a corrupt or hostile journal
    /// row can actually produce — a body whose `call_id` is the wrong length.
    #[test]
    fn refuses_malformed_payloads() {
        // Empty bytes are a valid `CallSignalingPayload` with every field
        // defaulted — it decodes, but the body oneof is absent, which is the
        // "older build reading a future variant" case and must refuse.
        assert!(matches!(parse_call_signal(vec![]), Err(FfiError::Call(_))));

        let mut truncated = offer_wire(MediaFlags::audio_only());
        truncated.truncate(truncated.len() / 2);
        assert!(matches!(
            parse_call_signal(truncated),
            Err(FfiError::Call(_))
        ));

        // Field 1 (`offer`), length-delimited, containing field 1 (`call_id`)
        // with a 4-byte value — a well-formed proto carrying a wrong-length id.
        let short_id = vec![0x0A, 0x06, 0x0A, 0x04, 0x01, 0x02, 0x03, 0x04];
        assert!(matches!(
            parse_call_signal(short_id),
            Err(FfiError::Call(_))
        ));
    }

    /// Without this arm the NSE renders a call offer as chat text. Pinned as
    /// the reason the module exists, so deleting it fails loudly rather than
    /// quietly reinstating the bug — the same guard the beacon filter carries.
    #[test]
    fn an_unfiltered_offer_would_otherwise_render_as_chat_text() {
        let wire = offer_wire(MediaFlags::audio_video());
        match lakko_attachments::parse_body(&wire) {
            lakko_attachments::MessageBody::Chat { text, .. } => assert!(
                text.contains("v=0"),
                "the lossy-UTF-8 fallback smears SDP into the notification body",
            ),
            other => panic!("expected the lossy Chat fallback, got {other:?}"),
        }
        // ...and the filter arm catches it first.
        assert!(matches!(
            parse_call_signal(wire),
            Ok(CallSignal::Offer { .. })
        ));
    }

    /// Golden: the exact wire bytes the Swift branch is taken on.
    ///
    /// Swift never sees the proto — it switches on [`CallSignal`] — so what has
    /// to be pinned is the bytes that produce each branch, plus the branch they
    /// produce. A field-number change, a `media_flags` bit swap or a schema
    /// bump then fails HERE, in Rust CI, rather than as a phone that stops
    /// ringing or announces a voice call as video.
    ///
    /// The hex is a LITERAL on purpose: re-deriving it from `compose` would be
    /// a tautology no wire revision could fail.
    #[test]
    fn golden_pins_the_wire_shape_swift_branches_on() {
        let video_offer = offer_wire(MediaFlags::audio_video());
        assert_eq!(
            hex(&video_offer),
            "52200a10ab0000000000000000000000000000cd\
             1203763d301880d095ffbc312003",
            "video-offer wire",
        );

        let voice_offer = offer_wire(MediaFlags::audio_only());
        assert_eq!(
            hex(&voice_offer),
            "52200a10ab0000000000000000000000000000cd\
             1203763d301880d095ffbc312001",
            "voice-offer wire",
        );

        // `0x52` is the `body` oneof's `offer` tag (field 10, wire type 2).
        // The iOS filter's cheap byte peek keys on exactly this, so pin it
        // separately from the whole-wire hex — a field-number change must fail
        // here rather than as a call that no longer wakes the phone.
        assert_eq!(video_offer[0], 0x52, "the offer oneof tag Swift peeks at");

        // The two differ in exactly one byte — the `media_flags` varint, bit 1
        // being video. That single byte is the whole of "say what kind of call
        // it is" (`docs/ux/calls.md` §2.2), so it is pinned explicitly.
        assert_eq!(video_offer.len(), voice_offer.len());
        let differing: Vec<usize> = (0..video_offer.len())
            .filter(|&i| video_offer[i] != voice_offer[i])
            .collect();
        assert_eq!(differing.len(), 1, "only the media flag differs");
        assert_eq!(video_offer[differing[0]], 0x03);
        assert_eq!(voice_offer[differing[0]], 0x01);

        // And the branches those bytes take.
        assert!(matches!(
            parse_call_signal(video_offer),
            Ok(CallSignal::Offer { video: true, .. }),
        ));
        assert!(matches!(
            parse_call_signal(voice_offer),
            Ok(CallSignal::Offer { video: false, .. }),
        ));

        let end = compose_signaling_payload(&SignalingBody::End {
            call_id: call_id(),
            reason: EndReason::HungUp,
        })
        .expect("compose an end");
        assert_eq!(
            hex(&end),
            "6a120a10ab0000000000000000000000000000cd",
            "end wire",
        );
        assert_eq!(end[0], 0x6a, "the end oneof tag Swift peeks at");
        assert!(matches!(parse_call_signal(end), Ok(CallSignal::End { .. })));
    }

    /// Golden: the first wire tag of EVERY signalling body.
    ///
    /// The iOS filter runs a cheap byte peek before the authoritative parse,
    /// and that peek carries a hand-copied table of these tags
    /// (`nseLooksLikeCallSignaling`). Hand-copied wire constants on a fourth
    /// platform are the drift class ADR-0044 documents, and this one fails in
    /// the dangerous direction: a tag that moves without the table moving stops
    /// the peek matching, and the frame falls through to the chat parser's
    /// lossy fallback — a garbage notification instead of a ring.
    ///
    /// Pinned as LITERALS so a `schemas/mls-app/call_signaling_v1.proto` field
    /// renumber fails in Rust CI. `group_leave` is field 16, whose tag needs two
    /// varint bytes (`0x82 0x01`) — which is why the peek cannot be a
    /// first-byte rule for every kind.
    #[test]
    fn golden_pins_every_body_tag_the_ios_peek_tabulates() {
        let cases: [(&str, &[u8], SignalingBody); 8] = [
            (
                "offer",
                &[0x52],
                SignalingBody::Offer {
                    call_id: call_id(),
                    sdp: "v=0".into(),
                    created_at_ms: 1,
                    media: MediaFlags::audio_only(),
                },
            ),
            (
                "answer",
                &[0x5a],
                SignalingBody::Answer {
                    call_id: call_id(),
                    sdp: "v=0".into(),
                },
            ),
            (
                "candidate",
                &[0x62],
                SignalingBody::IceCandidate {
                    call_id: call_id(),
                    candidate: "candidate:1".into(),
                    sdp_mid: "audio".into(),
                    sdp_m_line_index: 0,
                },
            ),
            (
                "end",
                &[0x6a],
                SignalingBody::End {
                    call_id: call_id(),
                    reason: EndReason::HungUp,
                },
            ),
            (
                "group_start",
                &[0x72],
                SignalingBody::GroupStart {
                    call_id: call_id(),
                    initiator_leaf: 0,
                    created_at_ms: 1,
                    media: MediaFlags::audio_only(),
                },
            ),
            (
                "group_join",
                &[0x7a],
                SignalingBody::GroupJoin {
                    call_id: call_id(),
                    leaf_index: 0,
                    joined_at_ms: 1,
                },
            ),
            (
                "group_leave",
                &[0x82, 0x01],
                SignalingBody::GroupLeave {
                    call_id: call_id(),
                    leaf_index: 0,
                    reason: EndReason::HungUp,
                },
            ),
            (
                "screen_share",
                &[0x8a, 0x01],
                SignalingBody::ScreenShare {
                    call_id: call_id(),
                    state: vec![1],
                },
            ),
        ];
        for (name, tag, body) in cases {
            let wire = compose_signaling_payload(&body).expect("compose");
            assert_eq!(&wire[..tag.len()], tag, "{name} tag");
            // ...and no signalling tag collides with a chat envelope's `{`,
            // which is what makes an ordered filter chain safe at all.
            assert_ne!(wire[0], b'{', "{name} must not look like chat JSON");
            parse_call_signal(wire).expect("every body classifies");
        }
    }
}
