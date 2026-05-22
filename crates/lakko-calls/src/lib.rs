// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Audio/video calls for Lakko.
//!
//! Implements client-side call mechanics: SFrame (RFC 9605) media encryption,
//! adaptive jitter buffer, codec negotiation (Opus / AV1-SVC / VP9-SVC / H.264
//! fallback), and ICE/STUN/TURN with QUIC and TURN-over-TLS fallbacks.
//!
//! Media keys are derived from MLS exporters (`lakko/call/v1`) — see
//! `docs/adr/0001-mls-only.md` and `docs/adr/0003-livekit-fork.md`.
//!
//! ## Phase 3 scaffolding scope
//!
//! Today's surface covers the **state-machine + crypto + wire** layers; the
//! live media path stays on each platform's native WebRTC stack:
//!
//!   * [`session`] — Phase 3a1. `CallSession` state machine
//!     (`Idle → Outgoing/Ringing → Connecting → Connected →
//!     Ending → Ended`), wire-typed `SdpDescription` +
//!     `IceCandidate` + `MediaKind`. Platform code drives via
//!     `on_event` and applies the returned `Transition`s.
//!   * [`signaling`] — Phase 3a3. `SignalingBody` enum
//!     (Offer / Answer / IceCandidate / End) + the
//!     `CallSignalingPayload` proto codec. Bytes ride the
//!     existing chat MLS group as application messages —
//!     no new transport.
//!   * [`sframe`] — Phase 3a4. Per-call media-key seed from a
//!     `lakko_mls::Group` exporter; per-participant key
//!     derivation via HKDF over the seed + sender leaf index.
//!     Counter-based per-frame nonce composition matches
//!     RFC 9605 §4.4.
//!   * [`group_call`] — Phase 3c1. Multi-participant ledger
//!     that owns the [`CallKey`] seed + tracks which leaves
//!     are currently on the call (a subset of the MLS
//!     group's full membership), surfaces a
//!     `needs_rekey()` flag on every membership change.
//!
//! What's NOT in core: live `RTCPeerConnection` instances,
//! `MediaStream`s, codec pipelines, ICE-pair establishment.
//! Every platform owns its own native WebRTC stack
//! (Apple `WebRTC.framework` / `org.webrtc:google-webrtc` /
//! browser-native `RTCPeerConnection`); the core sits on top
//! with a shared state machine + wire format + key
//! derivation. See [`session`] for the rationale.

#![forbid(unsafe_code)]

pub mod bandwidth;
pub mod codec_audio;
pub mod codec_video;
pub mod error;
pub mod group_call;
pub mod link_invite;
pub mod quality;
pub mod session;
pub mod sframe;
pub mod signaling;

pub use bandwidth::{BandwidthController, LayerSwitch, SimulcastLayer, CONFIRMATION_STREAK};
pub use codec_audio::{OpusConfig, OpusProfile, OPUS_MAX_BITRATE_KBPS, OPUS_MIN_BITRATE_KBPS};
pub use codec_video::{VideoCapability, VideoCodec};
pub use error::{Error, Result};
pub use group_call::{GroupCallScope, GroupCallSession, Participant, SignalingOutcome};
pub use link_invite::{accept_call_invite, AcceptedCallInvite};
pub use quality::{
    estimate_quality, recommend_bitrate, AudioCodec, QualityLevel, QualitySample, QualitySampler,
    QualityScore, DEFAULT_SAMPLE_WINDOW, MAX_SAMPLE_WINDOW, OPUS_MAX_BITRATE_BPS,
    OPUS_MIN_BITRATE_BPS,
};
pub use session::{
    CallSession, CallState, Event, IceCandidate, MediaKind, Role, SdpDescription, SdpKind,
    Transition,
};
pub use sframe::{CallKey, ParticipantKey, SFRAME_EXPORT_LABEL};
pub use signaling::{
    compose_signaling_payload, parse_signaling_payload, CallId, EndReason, MediaFlags,
    SignalingBody,
};
