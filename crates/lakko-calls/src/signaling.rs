// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Call signaling codec (Phase 3a3).
//!
//! Compose / parse [`SignalingBody`] payloads riding the
//! existing conversation MLS group as application messages.
//! Per the plan §5 + `docs/adr/0001-mls-only.md` §"Звонки",
//! in-chat-group calls reuse the chat group's epoch and DS
//! journal — no new transport.
//!
//! The codec is **plaintext-only** — the MLS envelope is the
//! privacy layer. Callers feed the bytes returned here into
//! [`lakko_mls::Group::create_application_message`] and unwrap
//! incoming `Incoming::Application(bytes)` via
//! [`parse_signaling_payload`].
//!
//! ## Multi-call disambiguation
//!
//! Every body variant carries a 16-byte `call_id` minted by
//! the offer side. Receivers match incoming candidates to
//! the active call by id; stale candidates from a prior
//! call land in the same group but don't match any live
//! call_id and get dropped.

use prost::Message as _;

use crate::error::{Error, Result};

/// 16-byte random per-call id.
pub type CallId = [u8; 16];

/// Reason discriminator carried in [`SignalingBody::End`]
/// and [`SignalingBody::GroupLeave`]. Pinned numeric values
/// so the wire ↔ enum mapping is stable across schema
/// versions. For [`SignalingBody::GroupLeave`] only `HungUp`,
/// `TimedOut`, and `Error` are meaningful — `Busy` /
/// `Declined` are 1×1-only and reserved.
///
/// `EndedForAll` is carried only by [`SignalingBody::End`] and only for a
/// group or link call: the person who started the call ended it for
/// everyone in it. The body states an intent and nothing about authority;
/// a receiver acts on it only when the MLS-authenticated sender is the
/// call's starter (`docs/ux/calls.md`), and treats it as a plain end
/// otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    /// Either side tore down (regular hangup).
    HungUp = 0,
    /// Callee was already on another call.
    Busy = 1,
    /// Callee explicitly declined.
    Declined = 2,
    /// No answer within the offer window.
    TimedOut = 3,
    /// Unrecoverable error mid-call.
    Error = 4,
    /// The call's starter ended it for everyone in it.
    EndedForAll = 5,
}

impl EndReason {
    fn from_proto(v: u32) -> Self {
        match v {
            1 => Self::Busy,
            2 => Self::Declined,
            3 => Self::TimedOut,
            4 => Self::Error,
            5 => Self::EndedForAll,
            _ => Self::HungUp,
        }
    }

    fn into_proto(self) -> u32 {
        self as u32
    }
}

/// Bitmap of media kinds the offer requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaFlags {
    /// `true` when the offer requests audio.
    pub audio: bool,
    /// `true` when the offer requests video.
    pub video: bool,
}

impl MediaFlags {
    /// `audio` + `video`.
    #[must_use]
    pub const fn audio_video() -> Self {
        Self {
            audio: true,
            video: true,
        }
    }

    /// `audio` only.
    #[must_use]
    pub const fn audio_only() -> Self {
        Self {
            audio: true,
            video: false,
        }
    }

    /// Wire-format bitmap. Bit 0 = audio, bit 1 = video.
    #[must_use]
    pub fn into_bits(self) -> u32 {
        let mut bits = 0u32;
        if self.audio {
            bits |= 0b01;
        }
        if self.video {
            bits |= 0b10;
        }
        bits
    }

    /// Inverse of [`Self::into_bits`].
    #[must_use]
    pub fn from_bits(bits: u32) -> Self {
        Self {
            audio: bits & 0b01 != 0,
            video: bits & 0b10 != 0,
        }
    }
}

/// Typed body of a call-signaling event. Mirror of the
/// `CallSignalingPayload.body` oneof in
/// `schemas/mls-app/call_signaling_v1.proto`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalingBody {
    /// SDP offer from the initiator.
    Offer {
        /// 16-byte random id minted by the offer side.
        call_id: CallId,
        /// Full SDP-offer body.
        sdp: String,
        /// Wall-clock UNIX millis when the offer was minted.
        created_at_ms: u64,
        /// Audio / video request flags.
        media: MediaFlags,
    },
    /// SDP answer from the callee.
    Answer {
        call_id: CallId,
        /// Full SDP-answer body.
        sdp: String,
    },
    /// Trickled ICE candidate.
    IceCandidate {
        call_id: CallId,
        /// ICE candidate string in SDP `a=candidate:` form.
        candidate: String,
        /// SDP m-section identifier (`audio` / `video`).
        sdp_mid: String,
        /// SDP m-section index (zero-based).
        sdp_m_line_index: u32,
    },
    /// Terminal event.
    End { call_id: CallId, reason: EndReason },
    /// Group call announce — first event in the group-call
    /// lifecycle. Posted to the chat MLS group by the
    /// initiator; observers render a joinable-call card.
    /// Per-peer SDP / ICE still goes participant ↔ SFU
    /// directly, not through MLS.
    GroupStart {
        /// 16-byte random id minted by the initiator.
        call_id: CallId,
        /// MLS leaf index of the initiator at announce time.
        initiator_leaf: u32,
        /// Wall-clock UNIX millis when the announce was minted.
        created_at_ms: u64,
        /// Audio / video support flags for the call.
        media: MediaFlags,
    },
    /// Group call join — participant connected to the SFU.
    /// Receivers fold this into their local
    /// [`crate::GroupCallSession`] roster via
    /// [`crate::GroupCallSession::apply_remote_join`].
    GroupJoin {
        call_id: CallId,
        /// MLS leaf index of the joining participant.
        leaf_index: u32,
        /// Wall-clock UNIX millis when the join landed.
        joined_at_ms: u64,
    },
    /// Group call leave — participant disconnected from
    /// the SFU. Receivers fold this via
    /// [`crate::GroupCallSession::apply_remote_leave`];
    /// the local rekey flag flips so the next MLS commit
    /// triggers a key rotation per 3c5.
    GroupLeave {
        call_id: CallId,
        leaf_index: u32,
        /// Reason discriminator; meaningful values for
        /// group-call leave are `HungUp` (voluntary),
        /// `TimedOut` (connection lost / SFU eviction),
        /// `Error` (unrecoverable per-participant fault).
        reason: EndReason,
    },
}

impl SignalingBody {
    /// Extract the `call_id`. Every variant carries one.
    #[must_use]
    pub fn call_id(&self) -> &CallId {
        match self {
            Self::Offer { call_id, .. }
            | Self::Answer { call_id, .. }
            | Self::IceCandidate { call_id, .. }
            | Self::End { call_id, .. }
            | Self::GroupStart { call_id, .. }
            | Self::GroupJoin { call_id, .. }
            | Self::GroupLeave { call_id, .. } => call_id,
        }
    }

    /// `true` for `GroupStart` / `GroupJoin` / `GroupLeave` —
    /// the call-control-only bodies that ride MLS in
    /// SFU-based group calls. Useful for client dispatchers
    /// that route 1×1 vs group-call events to different
    /// state machines.
    #[must_use]
    pub fn is_group_control(&self) -> bool {
        matches!(
            self,
            Self::GroupStart { .. } | Self::GroupJoin { .. } | Self::GroupLeave { .. }
        )
    }
}

/// Encode a [`SignalingBody`] to plaintext proto bytes the
/// caller wraps in an MLS application message via
/// [`lakko_mls::Group::create_application_message`].
///
/// # Errors
///
/// Returns [`Error::Crypto`] on the (practically unreachable)
/// prost encode failure — every field is fixed-shape.
pub fn compose_signaling_payload(body: &SignalingBody) -> Result<Vec<u8>> {
    use lakko_proto::mls_app::call_signaling::v1::{
        call_signaling_payload::Body as ProtoBody, CallAnswer, CallEnd, CallOffer,
        CallSignalingPayload, GroupCallJoin, GroupCallLeave, GroupCallStart, IceCandidate,
    };

    let proto_body = match body {
        SignalingBody::Offer {
            call_id,
            sdp,
            created_at_ms,
            media,
        } => ProtoBody::Offer(CallOffer {
            call_id: call_id.to_vec(),
            sdp: sdp.clone(),
            created_at_ms: *created_at_ms,
            media_flags: media.into_bits(),
        }),
        SignalingBody::Answer { call_id, sdp } => ProtoBody::Answer(CallAnswer {
            call_id: call_id.to_vec(),
            sdp: sdp.clone(),
        }),
        SignalingBody::IceCandidate {
            call_id,
            candidate,
            sdp_mid,
            sdp_m_line_index,
        } => ProtoBody::Candidate(IceCandidate {
            call_id: call_id.to_vec(),
            candidate: candidate.clone(),
            sdp_mid: sdp_mid.clone(),
            sdp_m_line_index: *sdp_m_line_index,
        }),
        SignalingBody::End { call_id, reason } => ProtoBody::End(CallEnd {
            call_id: call_id.to_vec(),
            reason: reason.into_proto(),
        }),
        SignalingBody::GroupStart {
            call_id,
            initiator_leaf,
            created_at_ms,
            media,
        } => ProtoBody::GroupStart(GroupCallStart {
            call_id: call_id.to_vec(),
            initiator_leaf: *initiator_leaf,
            created_at_ms: *created_at_ms,
            media_flags: media.into_bits(),
        }),
        SignalingBody::GroupJoin {
            call_id,
            leaf_index,
            joined_at_ms,
        } => ProtoBody::GroupJoin(GroupCallJoin {
            call_id: call_id.to_vec(),
            leaf_index: *leaf_index,
            joined_at_ms: *joined_at_ms,
        }),
        SignalingBody::GroupLeave {
            call_id,
            leaf_index,
            reason,
        } => ProtoBody::GroupLeave(GroupCallLeave {
            call_id: call_id.to_vec(),
            leaf_index: *leaf_index,
            reason: reason.into_proto(),
        }),
    };

    let payload = CallSignalingPayload {
        body: Some(proto_body),
    };
    let mut out = Vec::with_capacity(payload.encoded_len());
    payload
        .encode(&mut out)
        .map_err(|e| Error::Crypto(format!("encode CallSignalingPayload: {e}")))?;
    Ok(out)
}

/// Inverse of [`compose_signaling_payload`]. Decodes the
/// plaintext bytes that fell out of `Incoming::Application`.
///
/// # Errors
///
/// - [`Error::Crypto`] on prost decode failure.
/// - [`Error::Crypto`] with a `"call_id length"` body when
///   the embedded `call_id` isn't exactly 16 bytes.
/// - [`Error::Crypto`] with a `"missing body"` body when the
///   payload's oneof is absent (older client reading a
///   future variant). Caller may treat as drop-silently.
pub fn parse_signaling_payload(bytes: &[u8]) -> Result<SignalingBody> {
    use lakko_proto::mls_app::call_signaling::v1::{
        call_signaling_payload::Body as ProtoBody, CallSignalingPayload,
    };

    let payload = CallSignalingPayload::decode(bytes)
        .map_err(|e| Error::Crypto(format!("decode CallSignalingPayload: {e}")))?;
    let body = payload
        .body
        .ok_or_else(|| Error::Crypto("CallSignalingPayload missing body".to_string()))?;

    let take_call_id = |bytes: Vec<u8>| -> Result<CallId> {
        if bytes.len() != 16 {
            return Err(Error::Crypto(format!(
                "call_id length: expected 16, got {}",
                bytes.len()
            )));
        }
        let mut id = [0u8; 16];
        id.copy_from_slice(&bytes);
        Ok(id)
    };

    Ok(match body {
        ProtoBody::Offer(o) => SignalingBody::Offer {
            call_id: take_call_id(o.call_id)?,
            sdp: o.sdp,
            created_at_ms: o.created_at_ms,
            media: MediaFlags::from_bits(o.media_flags),
        },
        ProtoBody::Answer(a) => SignalingBody::Answer {
            call_id: take_call_id(a.call_id)?,
            sdp: a.sdp,
        },
        ProtoBody::Candidate(c) => SignalingBody::IceCandidate {
            call_id: take_call_id(c.call_id)?,
            candidate: c.candidate,
            sdp_mid: c.sdp_mid,
            sdp_m_line_index: c.sdp_m_line_index,
        },
        ProtoBody::End(e) => SignalingBody::End {
            call_id: take_call_id(e.call_id)?,
            reason: EndReason::from_proto(e.reason),
        },
        ProtoBody::GroupStart(s) => SignalingBody::GroupStart {
            call_id: take_call_id(s.call_id)?,
            initiator_leaf: s.initiator_leaf,
            created_at_ms: s.created_at_ms,
            media: MediaFlags::from_bits(s.media_flags),
        },
        ProtoBody::GroupJoin(j) => SignalingBody::GroupJoin {
            call_id: take_call_id(j.call_id)?,
            leaf_index: j.leaf_index,
            joined_at_ms: j.joined_at_ms,
        },
        ProtoBody::GroupLeave(l) => SignalingBody::GroupLeave {
            call_id: take_call_id(l.call_id)?,
            leaf_index: l.leaf_index,
            reason: EndReason::from_proto(l.reason),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_call_id() -> CallId {
        let mut id = [0u8; 16];
        id[0] = 0xAB;
        id[15] = 0xCD;
        id
    }

    #[test]
    fn offer_round_trips() {
        let body = SignalingBody::Offer {
            call_id: fixture_call_id(),
            sdp: "v=0\no=- 1 2 IN IP4 0.0.0.0".into(),
            created_at_ms: 1_700_000_000_000,
            media: MediaFlags::audio_video(),
        };
        let bytes = compose_signaling_payload(&body).unwrap();
        let parsed = parse_signaling_payload(&bytes).unwrap();
        assert_eq!(parsed, body);
    }

    #[test]
    fn answer_round_trips() {
        let body = SignalingBody::Answer {
            call_id: fixture_call_id(),
            sdp: "v=0\no=- 3 4 IN IP4 0.0.0.0".into(),
        };
        let bytes = compose_signaling_payload(&body).unwrap();
        let parsed = parse_signaling_payload(&bytes).unwrap();
        assert_eq!(parsed, body);
    }

    #[test]
    fn ice_candidate_round_trips() {
        let body = SignalingBody::IceCandidate {
            call_id: fixture_call_id(),
            candidate: "candidate:1 1 udp 1677729535 192.168.0.1 56789 typ srflx".into(),
            sdp_mid: "audio".into(),
            sdp_m_line_index: 0,
        };
        let bytes = compose_signaling_payload(&body).unwrap();
        let parsed = parse_signaling_payload(&bytes).unwrap();
        assert_eq!(parsed, body);
    }

    #[test]
    fn end_round_trips_each_reason() {
        for reason in [
            EndReason::HungUp,
            EndReason::Busy,
            EndReason::Declined,
            EndReason::TimedOut,
            EndReason::Error,
            EndReason::EndedForAll,
        ] {
            let body = SignalingBody::End {
                call_id: fixture_call_id(),
                reason,
            };
            let bytes = compose_signaling_payload(&body).unwrap();
            let parsed = parse_signaling_payload(&bytes).unwrap();
            assert_eq!(parsed, body);
        }
    }

    #[test]
    fn unknown_end_reason_folds_to_hung_up() {
        // Forward-compat: a future schema bump adding new
        // reason codes lands the unknown variants as
        // `HungUp` on the receiving side rather than
        // crashing.
        use lakko_proto::mls_app::call_signaling::v1::{
            call_signaling_payload::Body as ProtoBody, CallEnd, CallSignalingPayload,
        };
        let payload = CallSignalingPayload {
            body: Some(ProtoBody::End(CallEnd {
                call_id: fixture_call_id().to_vec(),
                reason: 9999,
            })),
        };
        let mut buf = Vec::new();
        payload.encode(&mut buf).unwrap();
        let parsed = parse_signaling_payload(&buf).unwrap();
        match parsed {
            SignalingBody::End { reason, .. } => {
                assert_eq!(reason, EndReason::HungUp);
            },
            other => panic!("expected End, got {other:?}"),
        }
    }

    #[test]
    fn wrong_call_id_length_rejected() {
        use lakko_proto::mls_app::call_signaling::v1::{
            call_signaling_payload::Body as ProtoBody, CallOffer, CallSignalingPayload,
        };
        let payload = CallSignalingPayload {
            body: Some(ProtoBody::Offer(CallOffer {
                call_id: vec![0x42; 8], // wrong — expected 16
                sdp: "v=0\n".into(),
                created_at_ms: 0,
                media_flags: 0b01,
            })),
        };
        let mut buf = Vec::new();
        payload.encode(&mut buf).unwrap();
        match parse_signaling_payload(&buf) {
            Err(Error::Crypto(msg)) => {
                assert!(msg.contains("call_id length"), "unexpected body: {msg}");
            },
            other => panic!("expected Crypto length error, got {other:?}"),
        }
    }

    #[test]
    fn group_start_round_trips() {
        let body = SignalingBody::GroupStart {
            call_id: fixture_call_id(),
            initiator_leaf: 7,
            created_at_ms: 1_700_000_000_000,
            media: MediaFlags::audio_video(),
        };
        let bytes = compose_signaling_payload(&body).unwrap();
        let parsed = parse_signaling_payload(&bytes).unwrap();
        assert_eq!(parsed, body);
        assert!(parsed.is_group_control());
    }

    #[test]
    fn group_join_round_trips() {
        let body = SignalingBody::GroupJoin {
            call_id: fixture_call_id(),
            leaf_index: 12,
            joined_at_ms: 1_700_000_000_500,
        };
        let bytes = compose_signaling_payload(&body).unwrap();
        let parsed = parse_signaling_payload(&bytes).unwrap();
        assert_eq!(parsed, body);
        assert!(parsed.is_group_control());
    }

    #[test]
    fn group_leave_round_trips_each_reason() {
        // Receivers fold unknown reason codes to HungUp via
        // EndReason::from_proto — verify the meaningful set
        // round-trips identity-cleanly and the forward-compat
        // fold still holds on this variant.
        for reason in [EndReason::HungUp, EndReason::TimedOut, EndReason::Error] {
            let body = SignalingBody::GroupLeave {
                call_id: fixture_call_id(),
                leaf_index: 5,
                reason,
            };
            let bytes = compose_signaling_payload(&body).unwrap();
            let parsed = parse_signaling_payload(&bytes).unwrap();
            assert_eq!(parsed, body);
            assert!(parsed.is_group_control());
        }
    }

    #[test]
    fn is_group_control_false_for_one_on_one_bodies() {
        let one_to_one = [
            SignalingBody::Offer {
                call_id: fixture_call_id(),
                sdp: "v=0\n".into(),
                created_at_ms: 0,
                media: MediaFlags::audio_only(),
            },
            SignalingBody::Answer {
                call_id: fixture_call_id(),
                sdp: "v=0\n".into(),
            },
            SignalingBody::IceCandidate {
                call_id: fixture_call_id(),
                candidate: String::new(),
                sdp_mid: "audio".into(),
                sdp_m_line_index: 0,
            },
            SignalingBody::End {
                call_id: fixture_call_id(),
                reason: EndReason::HungUp,
            },
        ];
        for body in one_to_one {
            assert!(!body.is_group_control(), "{body:?}");
        }
    }

    #[test]
    fn media_flags_round_trip_bits() {
        let cases = [
            MediaFlags {
                audio: false,
                video: false,
            },
            MediaFlags::audio_only(),
            MediaFlags::audio_video(),
            MediaFlags {
                audio: false,
                video: true,
            },
        ];
        for flags in cases {
            assert_eq!(MediaFlags::from_bits(flags.into_bits()), flags);
        }
    }
}
