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

#![forbid(unsafe_code)]

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// Maximum size of an inbound signaling frame, bytes. The hard cap
/// protects the server against pathologically-large payloads on the
/// wire. SDP offers in production peak around 8 KiB; we leave 64×
/// headroom for unusual codec configurations.
pub const MAX_INBOUND_FRAME_BYTES: usize = 512 * 1024;

/// Most entries [`ClientFrame::SdpOffer`]'s `light_mids` may carry. A
/// frame with more is refused like any malformed frame. Chosen: a client
/// declares one light copy today (its camera's); the SFU honours one.
pub const MAX_LIGHT_MIDS: usize = 4;

/// Most bytes one entry of `light_mids` may have. An SDP `mid` a browser
/// writes is one or two characters, and str0m's own `Mid` holds 16 bytes.
pub const MAX_LIGHT_MID_BYTES: usize = 16;

/// Why a `light_mids` list is out of bounds, or `None` when it is within
/// them: at most [`MAX_LIGHT_MIDS`] entries, each non-empty and at most
/// [`MAX_LIGHT_MID_BYTES`] bytes.
#[must_use]
pub fn light_mids_problem(mids: &[String]) -> Option<&'static str> {
    if mids.len() > MAX_LIGHT_MIDS {
        return Some("light_mids has more entries than the protocol allows");
    }
    if mids.iter().any(String::is_empty) {
        return Some("light_mids has an empty entry");
    }
    if mids.iter().any(|mid| mid.len() > MAX_LIGHT_MID_BYTES) {
        return Some("light_mids has an entry longer than the protocol allows");
    }
    None
}

/// Why a `middle_mids` list is out of bounds, or `None` when it is within
/// them. The bounds are those of `light_mids` ([`MAX_LIGHT_MIDS`] entries,
/// each non-empty and at most [`MAX_LIGHT_MID_BYTES`] bytes); the SFU
/// honours one middle track per peer.
#[must_use]
pub fn middle_mids_problem(mids: &[String]) -> Option<&'static str> {
    if mids.len() > MAX_LIGHT_MIDS {
        return Some("middle_mids has more entries than the protocol allows");
    }
    if mids.iter().any(String::is_empty) {
        return Some("middle_mids has an empty entry");
    }
    if mids.iter().any(|mid| mid.len() > MAX_LIGHT_MID_BYTES) {
        return Some("middle_mids has an entry longer than the protocol allows");
    }
    None
}

/// Most entries one [`ClientFrame::VideoWants`] may carry. A frame with
/// more is refused like any malformed frame. Chosen: four pages of the
/// widest call grid (`lakko_ui_rules::call_group`, 16 tiles a page).
pub const MAX_VIDEO_WANTS: usize = 64;

/// Most bytes the `peer` of one [`VideoWant`] may have. A peer id the SFU
/// assigns is 33 bytes.
pub const MAX_VIDEO_WANT_PEER_BYTES: usize = 64;

/// Which of a publisher's video tracks a subscriber shows
/// ([`ClientFrame::VideoWants`]).
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum VideoQuality {
    /// The main track (the full picture). What a subscriber that says
    /// nothing gets.
    #[default]
    Main,
    /// The middle track (the camera at half size). Asked of a publisher
    /// that sends none, it means [`Self::Main`].
    Middle,
    /// The light track alone: neither the main nor the middle track is
    /// forwarded.
    Light,
}

impl VideoQuality {
    /// The wire name: `main`, `middle` or `light`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Middle => "middle",
            Self::Light => "light",
        }
    }

    /// The quality a wire name stands for; `None` for any other string.
    #[must_use]
    pub fn from_wire(name: &str) -> Option<Self> {
        match name {
            "main" => Some(Self::Main),
            "middle" => Some(Self::Middle),
            "light" => Some(Self::Light),
            _ => None,
        }
    }
}

/// One entry of [`ClientFrame::VideoWants`]: the picture of one publisher
/// the sending subscriber shows.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VideoWant {
    /// The publisher's peer id, as the roster names it. Non-empty and at
    /// most [`MAX_VIDEO_WANT_PEER_BYTES`] bytes.
    pub peer: String,
    /// Which of that publisher's video tracks is shown.
    pub quality: VideoQuality,
}

/// Why a `wants` list is out of bounds, or `None` when it is within them:
/// at most [`MAX_VIDEO_WANTS`] entries, each naming a non-empty peer of at
/// most [`MAX_VIDEO_WANT_PEER_BYTES`] bytes.
#[must_use]
pub fn video_wants_problem(wants: &[VideoWant]) -> Option<&'static str> {
    if wants.len() > MAX_VIDEO_WANTS {
        return Some("wants has more entries than the protocol allows");
    }
    if wants.iter().any(|want| want.peer.is_empty()) {
        return Some("wants has an entry with an empty peer");
    }
    if wants
        .iter()
        .any(|want| want.peer.len() > MAX_VIDEO_WANT_PEER_BYTES)
    {
        return Some("wants has an entry whose peer is longer than the protocol allows");
    }
    None
}

/// Most entries one [`ServerFrame::VideoGiven`] may carry: as many as a
/// subscriber may name in a [`ClientFrame::VideoWants`]. A subscriber of
/// more publishers than this is told about the most important ones.
pub const MAX_VIDEO_GIVEN: usize = MAX_VIDEO_WANTS;

/// One entry of [`ServerFrame::VideoGiven`]: the picture of one publisher
/// the receiving subscriber is being sent.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VideoGiven {
    /// The publisher's peer id, as the roster names it. Non-empty and at
    /// most [`MAX_VIDEO_WANT_PEER_BYTES`] bytes.
    pub peer: String,
    /// Which of that publisher's video tracks carries packets: the one the
    /// subscriber asked for, or a smaller one. Never a larger one. The light
    /// track is sent at every quality.
    pub quality: VideoQuality,
}

/// Why a `given` list is out of bounds, or `None` when it is within them:
/// at most [`MAX_VIDEO_GIVEN`] entries, each naming a non-empty peer of at
/// most [`MAX_VIDEO_WANT_PEER_BYTES`] bytes.
#[must_use]
pub fn video_given_problem(given: &[VideoGiven]) -> Option<&'static str> {
    if given.len() > MAX_VIDEO_GIVEN {
        return Some("given has more entries than the protocol allows");
    }
    if given.iter().any(|entry| entry.peer.is_empty()) {
        return Some("given has an entry with an empty peer");
    }
    if given
        .iter()
        .any(|entry| entry.peer.len() > MAX_VIDEO_WANT_PEER_BYTES)
    {
        return Some("given has an entry whose peer is longer than the protocol allows");
    }
    None
}

/// The highest ceiling one field of a [`ServerFrame::PublishBudget`] may
/// carry, kilobits per second. A bound of the wire format, far above any
/// rate a Lakko camera sender is planned at; chosen, not measured.
pub const MAX_PUBLISH_BUDGET_KBPS: u32 = 20_000;

/// Why the two ceilings of a [`ServerFrame::PublishBudget`] are out of
/// bounds, or `None` when they are within them: each is at least 1 and at
/// most [`MAX_PUBLISH_BUDGET_KBPS`]. Zero is refused because a ceiling of
/// zero would say "send nothing", and switching a sender off is
/// [`ServerFrame::VideoNeeded`]'s word, not this frame's.
#[must_use]
pub const fn publish_budget_problem(main_kbps: u32, middle_kbps: u32) -> Option<&'static str> {
    if main_kbps == 0 || middle_kbps == 0 {
        return Some("publish-budget has a ceiling of zero");
    }
    if main_kbps > MAX_PUBLISH_BUDGET_KBPS || middle_kbps > MAX_PUBLISH_BUDGET_KBPS {
        return Some("publish-budget has a ceiling above what the protocol allows");
    }
    None
}

/// Deserialize one ceiling of a [`ServerFrame::PublishBudget`], refusing
/// a value outside its bounds ([`publish_budget_problem`]; the bounds are
/// the same for both fields, so each is checked on its own).
fn deserialize_publish_budget_kbps<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let kbps = u32::deserialize(deserializer)?;
    match publish_budget_problem(kbps, kbps) {
        Some(problem) => Err(serde::de::Error::custom(problem)),
        None => Ok(kbps),
    }
}

/// A sequence read with its entry count checked as it is read, so an
/// over-long list is refused without being collected, and then checked as
/// a whole by `problem`.
struct Bounded<T> {
    max: usize,
    too_many: &'static str,
    problem: fn(&[T]) -> Option<&'static str>,
}

impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Bounded<T> {
    type Value = Vec<T>;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "a list of at most {} entries", self.max)
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        let mut entries: Vec<T> = Vec::new();
        while let Some(entry) = seq.next_element::<T>()? {
            if entries.len() >= self.max {
                return Err(serde::de::Error::custom(self.too_many));
            }
            entries.push(entry);
        }
        match (self.problem)(&entries) {
            Some(problem) => Err(serde::de::Error::custom(problem)),
            None => Ok(entries),
        }
    }
}

/// Deserialize `light_mids`, refusing a list outside its bounds
/// ([`light_mids_problem`]).
fn deserialize_light_mids<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserializer.deserialize_seq(Bounded {
        max: MAX_LIGHT_MIDS,
        too_many: "light_mids has more entries than the protocol allows",
        problem: |mids: &[String]| light_mids_problem(mids),
    })
}

/// Deserialize `middle_mids`, refusing a list outside its bounds
/// ([`middle_mids_problem`]).
fn deserialize_middle_mids<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserializer.deserialize_seq(Bounded {
        max: MAX_LIGHT_MIDS,
        too_many: "middle_mids has more entries than the protocol allows",
        problem: |mids: &[String]| middle_mids_problem(mids),
    })
}

/// Deserialize `wants`, refusing a list outside its bounds
/// ([`video_wants_problem`]).
fn deserialize_video_wants<'de, D>(deserializer: D) -> Result<Vec<VideoWant>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserializer.deserialize_seq(Bounded {
        max: MAX_VIDEO_WANTS,
        too_many: "wants has more entries than the protocol allows",
        problem: |wants: &[VideoWant]| video_wants_problem(wants),
    })
}

/// Deserialize `given`, refusing a list outside its bounds
/// ([`video_given_problem`]).
fn deserialize_video_given<'de, D>(deserializer: D) -> Result<Vec<VideoGiven>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserializer.deserialize_seq(Bounded {
        max: MAX_VIDEO_GIVEN,
        too_many: "given has more entries than the protocol allows",
        problem: |given: &[VideoGiven]| video_given_problem(given),
    })
}

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

    /// What the receiving subscriber is being sent of each publisher's
    /// video (ADR-0070 step 6): the SFU estimates the subscriber's
    /// downlink and, when it is short, gives up the larger tracks first.
    /// The list is the **whole** set: every publisher the subscriber has a
    /// video track of, in the subscriber's order of importance (the order
    /// of its last [`ClientFrame::VideoWants`], then the publishers it did
    /// not name, in the order they joined). A publisher at the quality the
    /// subscriber asked for is listed too. Sent when the set changes and
    /// after every `video-wants` frame; never sent by an SFU with downlink
    /// control off. At most [`MAX_VIDEO_GIVEN`] entries
    /// ([`video_given_problem`]).
    VideoGiven {
        /// One entry per publisher.
        #[serde(deserialize_with = "deserialize_video_given")]
        given: Vec<VideoGiven>,
    },

    /// Which of the receiving publisher's **own** camera senders somebody
    /// is being sent, or is about to be sent (ADR-0070, "senders nobody
    /// watches"). `false` means no subscriber is sent that track, so the
    /// client may stop encoding and uploading it; `true` means the client
    /// must be sending it. The light sender is always needed and is not in
    /// the frame. Sent to a peer once it has declared its senders and then
    /// whenever a value changes: `true` at once, `false` only after the
    /// track has been unneeded for a while. Both fields are required; a
    /// client that ignores the frame keeps every sender on, as before.
    VideoNeeded {
        /// The main (full size) camera sender.
        main: bool,
        /// The middle (half size) camera sender. Always `false` for a
        /// publisher that declared no middle sender.
        middle: bool,
    },

    /// How much the receiving publisher's **own** camera senders may send
    /// (ADR-0068, amendment of 2026-10-11): the SFU estimates the
    /// publisher's uplink and tells it here, over Lakko's own signaling,
    /// the ceiling for its main and its middle sender, in kilobits per
    /// second. The client sets each as that sender's maximum bitrate. The
    /// light sender keeps its fixed ceiling and audio is never limited;
    /// neither is in the frame. A ceiling is given for a sender whether or
    /// not it is needed ([`Self::VideoNeeded`] switches senders on and
    /// off; this frame only bounds them). Sent once an estimate exists and
    /// then when the budget changes; never sent by an SFU that does not
    /// decide a publisher's bitrate. Both fields are required, each at
    /// least 1 and at most [`MAX_PUBLISH_BUDGET_KBPS`]
    /// ([`publish_budget_problem`]); a client that ignores the frame keeps
    /// its own ceilings, as before.
    PublishBudget {
        /// The ceiling of the main (full size) camera sender, kbit/s.
        #[serde(deserialize_with = "deserialize_publish_budget_kbps")]
        main_kbps: u32,
        /// The ceiling of the middle (half size) camera sender, kbit/s.
        /// Given also to a publisher that has no middle sender, which
        /// has nothing to apply it to.
        #[serde(deserialize_with = "deserialize_publish_budget_kbps")]
        middle_kbps: u32,
    },
}

impl ServerFrame {
    /// Stable kebab-case discriminant for this frame, matching the serde
    /// `kind` tag. Intended for low-cardinality metric labels and structured
    /// logs — never derive protocol behaviour from it. Kept in lock-step with
    /// the `#[serde(rename_all = "kebab-case")]` tags above (a unit test
    /// asserts they never drift).
    #[must_use]
    pub const fn kind_str(&self) -> &'static str {
        match self {
            Self::Welcome { .. } => "welcome",
            Self::PeerJoined { .. } => "peer-joined",
            Self::PeerLeft { .. } => "peer-left",
            Self::SignalRelay { .. } => "signal-relay",
            Self::Error { .. } => "error",
            Self::Pong { .. } => "pong",
            Self::SdpAnswer { .. } => "sdp-answer",
            Self::SdpOffer { .. } => "sdp-offer",
            Self::VideoGiven { .. } => "video-given",
            Self::VideoNeeded { .. } => "video-needed",
            Self::PublishBudget { .. } => "publish-budget",
        }
    }
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
        /// The `mid`s of this offer's video m-lines that carry a
        /// **light copy** of the sender's camera: the same picture
        /// scaled down and capped at a low bitrate, sent on its own
        /// m-line beside the main one. The SFU forwards such a track
        /// to every subscriber as the light companion of the sender
        /// (MSID stream id = the sender's peer id followed by
        /// `~light`) and never as a main track.
        ///
        /// Absent on the wire when empty, and an absent field
        /// decodes to empty, so a client that sends no light copy
        /// writes the frame it always wrote. At most
        /// [`MAX_LIGHT_MIDS`] entries of at most
        /// [`MAX_LIGHT_MID_BYTES`] bytes each; a frame outside the
        /// bound is refused like any malformed frame.
        #[serde(
            default,
            skip_serializing_if = "Vec::is_empty",
            deserialize_with = "deserialize_light_mids"
        )]
        light_mids: Vec<String>,
        /// The `mid`s of this offer's video m-lines that carry the
        /// **middle copy** of the sender's camera (ADR-0070 step 5):
        /// the same picture at half size, on its own m-line. The SFU
        /// forwards such a track as the sender's middle track (MSID
        /// stream id = the sender's peer id followed by `~mid`), and
        /// only to a subscriber that says it shows it
        /// ([`Self::VideoWants`]).
        ///
        /// Absent on the wire when empty, and an absent field decodes
        /// to empty. The bounds are those of `light_mids`
        /// ([`middle_mids_problem`]).
        #[serde(
            default,
            skip_serializing_if = "Vec::is_empty",
            deserialize_with = "deserialize_middle_mids"
        )]
        middle_mids: Vec<String>,
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

    /// Client → SFU: which picture of each publisher this subscriber
    /// shows (ADR-0070 step 5). For every publisher named the SFU
    /// forwards the main track only when the quality is `main`, the
    /// middle track only when it is `middle`, and the light track
    /// always. A publisher not named, and every publisher before the
    /// first frame, is `main`. `middle` asked of a publisher without a
    /// middle track is `main`.
    ///
    /// Each frame **replaces** the whole set: a publisher the new frame
    /// omits is back at `main`. When one frame names a peer twice the
    /// last entry counts. At most [`MAX_VIDEO_WANTS`] entries
    /// ([`video_wants_problem`]); an unknown quality or an unknown field
    /// is refused like any malformed frame. There is no reply.
    VideoWants {
        /// The whole set.
        #[serde(deserialize_with = "deserialize_video_wants")]
        wants: Vec<VideoWant>,
    },
}

impl ClientFrame {
    /// Stable kebab-case discriminant for this frame, matching the serde
    /// `kind` tag. Intended for low-cardinality metric labels and structured
    /// logs — never derive protocol behaviour from it. Kept in lock-step with
    /// the `#[serde(rename_all = "kebab-case")]` tags above (a unit test
    /// asserts they never drift).
    #[must_use]
    pub const fn kind_str(&self) -> &'static str {
        match self {
            Self::SignalRelay { .. } => "signal-relay",
            Self::Ping { .. } => "ping",
            Self::SdpOffer { .. } => "sdp-offer",
            Self::IceCandidate { .. } => "ice-candidate",
            Self::SdpAnswer { .. } => "sdp-answer",
            Self::VideoWants { .. } => "video-wants",
        }
    }
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
    /// The call this socket's token names ended before it arrived: the last
    /// participant left more than the SFU's empty grace ago. Sent in place of
    /// the Welcome, after which the SFU closes the socket. The member that
    /// receives it closes the call's record for its chat.
    CallEnded,
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
///
/// # Errors
///
/// [`SfuProtoError::Json`] when the frame cannot be serialised, or when
/// an SDP offer's `light_mids` or `middle_mids`, or the `wants` of a
/// [`ClientFrame::VideoWants`], is outside its bounds
/// ([`light_mids_problem`], [`middle_mids_problem`],
/// [`video_wants_problem`]): the server would refuse that frame, so it is
/// not composed.
pub fn encode_client_frame(frame: &ClientFrame) -> Result<String, SfuProtoError> {
    let problem = match frame {
        ClientFrame::SdpOffer {
            light_mids,
            middle_mids,
            ..
        } => light_mids_problem(light_mids).or_else(|| middle_mids_problem(middle_mids)),
        ClientFrame::VideoWants { wants } => video_wants_problem(wants),
        _ => None,
    };
    if let Some(problem) = problem {
        return Err(SfuProtoError::Json(serde::ser::Error::custom(problem)));
    }
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
///
/// # Errors
///
/// [`SfuProtoError::Json`] when the frame cannot be serialised, or when
/// the `given` of a [`ServerFrame::VideoGiven`] is outside its bounds
/// ([`video_given_problem`]) or the ceilings of a
/// [`ServerFrame::PublishBudget`] are outside theirs
/// ([`publish_budget_problem`]): a client would refuse that frame, so it is
/// not composed.
pub fn encode_server_frame(frame: &ServerFrame) -> Result<String, SfuProtoError> {
    let problem = match frame {
        ServerFrame::VideoGiven { given } => video_given_problem(given),
        ServerFrame::PublishBudget {
            main_kbps,
            middle_kbps,
        } => publish_budget_problem(*main_kbps, *middle_kbps),
        _ => None,
    };
    if let Some(problem) = problem {
        return Err(SfuProtoError::Json(serde::ser::Error::custom(problem)));
    }
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
            light_mids: Vec::new(),
            middle_mids: Vec::new(),
        };
        let json = encode_client_frame(&frame).expect("encode");
        assert!(json.contains("\"kind\":\"sdp-offer\""), "got {json}");
        assert!(
            !json.contains("middle_mids"),
            "an empty list is not written: {json}"
        );
        assert!(
            !json.contains("light_mids"),
            "an empty list is not written: {json}"
        );
        let back = decode_client_frame(&json).expect("decode");
        assert_eq!(back, frame);
    }

    #[test]
    fn sdp_offer_light_mids_round_trip() {
        let frame = ClientFrame::SdpOffer {
            sdp: "v=0\r\n".into(),
            light_mids: vec!["2".into()],
            middle_mids: Vec::new(),
        };
        let json = encode_client_frame(&frame).expect("encode");
        assert!(json.contains("\"light_mids\":[\"2\"]"), "got {json}");
        assert_eq!(decode_client_frame(&json).expect("decode"), frame);
    }

    #[test]
    fn sdp_offer_without_light_mids_decodes_to_empty() {
        let raw = r#"{"kind":"sdp-offer","sdp":"v=0"}"#;
        match decode_client_frame(raw).expect("decode") {
            ClientFrame::SdpOffer {
                sdp,
                light_mids,
                middle_mids,
            } => {
                assert_eq!(sdp, "v=0");
                assert!(light_mids.is_empty());
                assert!(middle_mids.is_empty());
            },
            other => panic!("expected SdpOffer, got {other:?}"),
        }
    }

    #[test]
    fn sdp_offer_light_mids_bounds_are_enforced_on_decode() {
        let frame =
            |mids: &str| format!(r#"{{"kind":"sdp-offer","sdp":"v=0","light_mids":{mids}}}"#);
        // At the bounds: accepted.
        let longest = "m".repeat(MAX_LIGHT_MID_BYTES);
        let full = format!(r#"["a","b","c","{longest}"]"#);
        match decode_client_frame(&frame(&full)).expect("at the bounds") {
            ClientFrame::SdpOffer { light_mids, .. } => {
                assert_eq!(light_mids.len(), MAX_LIGHT_MIDS);
                assert_eq!(light_mids[3], longest);
            },
            other => panic!("expected SdpOffer, got {other:?}"),
        }
        // One entry too many, an entry one byte too long, an empty entry,
        // and a wrong type: each refused as a malformed frame.
        let too_long = format!(r#"["{}"]"#, "m".repeat(MAX_LIGHT_MID_BYTES + 1));
        for bad in [
            r#"["a","b","c","d","e"]"#,
            too_long.as_str(),
            r#"[""]"#,
            "[1]",
            r#""2""#,
        ] {
            assert!(
                matches!(
                    decode_client_frame(&frame(bad)),
                    Err(SfuProtoError::Json(_))
                ),
                "refused: {bad}"
            );
            // The service decodes with serde directly; the bound holds there.
            assert!(serde_json::from_str::<ClientFrame>(&frame(bad)).is_err());
        }
    }

    #[test]
    fn sdp_offer_light_mids_bounds_are_enforced_on_encode() {
        let offer = |light_mids: Vec<String>| ClientFrame::SdpOffer {
            sdp: "v=0".into(),
            light_mids,
            middle_mids: Vec::new(),
        };
        assert!(encode_client_frame(&offer(vec!["1".into(); MAX_LIGHT_MIDS])).is_ok());
        assert!(encode_client_frame(&offer(vec!["1".into(); MAX_LIGHT_MIDS + 1])).is_err());
        assert!(encode_client_frame(&offer(vec![String::new()])).is_err());
        assert!(encode_client_frame(&offer(vec!["m".repeat(MAX_LIGHT_MID_BYTES + 1)])).is_err());
    }

    #[test]
    fn sdp_offer_middle_mids_round_trip() {
        let frame = ClientFrame::SdpOffer {
            sdp: "v=0\r\n".into(),
            light_mids: vec!["2".into()],
            middle_mids: vec!["3".into()],
        };
        let json = encode_client_frame(&frame).expect("encode");
        assert!(json.contains("\"middle_mids\":[\"3\"]"), "got {json}");
        assert_eq!(decode_client_frame(&json).expect("decode"), frame);
    }

    #[test]
    fn sdp_offer_middle_mids_bounds_are_those_of_light_mids() {
        let frame =
            |mids: &str| format!(r#"{{"kind":"sdp-offer","sdp":"v=0","middle_mids":{mids}}}"#);
        let longest = "m".repeat(MAX_LIGHT_MID_BYTES);
        let full = format!(r#"["a","b","c","{longest}"]"#);
        match decode_client_frame(&frame(&full)).expect("at the bounds") {
            ClientFrame::SdpOffer { middle_mids, .. } => {
                assert_eq!(middle_mids.len(), MAX_LIGHT_MIDS);
            },
            other => panic!("expected SdpOffer, got {other:?}"),
        }
        let too_long = format!(r#"["{}"]"#, "m".repeat(MAX_LIGHT_MID_BYTES + 1));
        for bad in [
            r#"["a","b","c","d","e"]"#,
            too_long.as_str(),
            r#"[""]"#,
            "[1]",
            r#""2""#,
        ] {
            assert!(
                matches!(
                    decode_client_frame(&frame(bad)),
                    Err(SfuProtoError::Json(_))
                ),
                "refused: {bad}"
            );
            assert!(serde_json::from_str::<ClientFrame>(&frame(bad)).is_err());
        }
        let offer = |middle_mids: Vec<String>| ClientFrame::SdpOffer {
            sdp: "v=0".into(),
            light_mids: Vec::new(),
            middle_mids,
        };
        assert!(encode_client_frame(&offer(vec!["1".into(); MAX_LIGHT_MIDS])).is_ok());
        assert!(encode_client_frame(&offer(vec!["1".into(); MAX_LIGHT_MIDS + 1])).is_err());
        assert!(encode_client_frame(&offer(vec![String::new()])).is_err());
        assert!(encode_client_frame(&offer(vec!["m".repeat(MAX_LIGHT_MID_BYTES + 1)])).is_err());
    }

    #[test]
    fn video_wants_round_trip_and_wire_shape() {
        let frame = ClientFrame::VideoWants {
            wants: vec![
                VideoWant {
                    peer: "p1".into(),
                    quality: VideoQuality::Main,
                },
                VideoWant {
                    peer: "p2".into(),
                    quality: VideoQuality::Middle,
                },
                VideoWant {
                    peer: "p3".into(),
                    quality: VideoQuality::Light,
                },
            ],
        };
        let json = encode_client_frame(&frame).expect("encode");
        assert_eq!(
            json,
            r#"{"kind":"video-wants","wants":[{"peer":"p1","quality":"main"},{"peer":"p2","quality":"middle"},{"peer":"p3","quality":"light"}]}"#
        );
        assert_eq!(decode_client_frame(&json).expect("decode"), frame);
        // An empty set is a frame too: everyone back to `main`.
        let empty = ClientFrame::VideoWants { wants: Vec::new() };
        let json = encode_client_frame(&empty).expect("encode");
        assert_eq!(json, r#"{"kind":"video-wants","wants":[]}"#);
        assert_eq!(decode_client_frame(&json).expect("decode"), empty);
    }

    #[test]
    fn video_quality_wire_names_match_serde() {
        for quality in [
            VideoQuality::Main,
            VideoQuality::Middle,
            VideoQuality::Light,
        ] {
            let json = serde_json::to_string(&quality).expect("encode");
            assert_eq!(json, format!("\"{}\"", quality.as_str()));
            assert_eq!(VideoQuality::from_wire(quality.as_str()), Some(quality));
        }
        assert_eq!(VideoQuality::from_wire("full"), None);
        assert_eq!(VideoQuality::default(), VideoQuality::Main);
    }

    #[test]
    fn video_wants_bounds_and_unknown_values_are_refused() {
        let frame = |wants: &str| format!(r#"{{"kind":"video-wants","wants":{wants}}}"#);
        let entry = |peer: &str| format!(r#"{{"peer":"{peer}","quality":"light"}}"#);
        let list = |n: usize| {
            let entries: Vec<String> = (0..n).map(|i| entry(&format!("p{i}"))).collect();
            format!("[{}]", entries.join(","))
        };
        // At the bounds: accepted.
        match decode_client_frame(&frame(&list(MAX_VIDEO_WANTS))).expect("at the bound") {
            ClientFrame::VideoWants { wants } => assert_eq!(wants.len(), MAX_VIDEO_WANTS),
            other => panic!("expected VideoWants, got {other:?}"),
        }
        let longest = format!("[{}]", entry(&"p".repeat(MAX_VIDEO_WANT_PEER_BYTES)));
        assert!(decode_client_frame(&frame(&longest)).is_ok());

        let too_long = format!("[{}]", entry(&"p".repeat(MAX_VIDEO_WANT_PEER_BYTES + 1)));
        let too_many = list(MAX_VIDEO_WANTS + 1);
        for bad in [
            too_many.as_str(),
            too_long.as_str(),
            // An empty peer.
            r#"[{"peer":"","quality":"main"}]"#,
            // An unknown quality, and a quality of the wrong case or type.
            r#"[{"peer":"p1","quality":"full"}]"#,
            r#"[{"peer":"p1","quality":"Main"}]"#,
            r#"[{"peer":"p1","quality":1}]"#,
            // A missing field, an unknown field, a wrong type.
            r#"[{"peer":"p1"}]"#,
            r#"[{"quality":"main"}]"#,
            r#"[{"peer":"p1","quality":"main","extra":1}]"#,
            r#"["p1"]"#,
            r#"{"p1":"main"}"#,
        ] {
            assert!(
                matches!(
                    decode_client_frame(&frame(bad)),
                    Err(SfuProtoError::Json(_))
                ),
                "refused: {bad}"
            );
            assert!(serde_json::from_str::<ClientFrame>(&frame(bad)).is_err());
        }
        // `wants` itself is required.
        assert!(decode_client_frame(r#"{"kind":"video-wants"}"#).is_err());

        // The same bounds on the composing side.
        let want = |peer: String| VideoWant {
            peer,
            quality: VideoQuality::Main,
        };
        let wants = |n: usize| ClientFrame::VideoWants {
            wants: (0..n).map(|i| want(format!("p{i}"))).collect(),
        };
        assert!(encode_client_frame(&wants(MAX_VIDEO_WANTS)).is_ok());
        assert!(encode_client_frame(&wants(MAX_VIDEO_WANTS + 1)).is_err());
        let one = |peer: String| ClientFrame::VideoWants {
            wants: vec![want(peer)],
        };
        assert!(encode_client_frame(&one(String::new())).is_err());
        assert!(encode_client_frame(&one("p".repeat(MAX_VIDEO_WANT_PEER_BYTES + 1))).is_err());
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
        assert!(t
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)));
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

    #[test]
    fn video_given_round_trips_in_the_wire_shape() {
        let frame = ServerFrame::VideoGiven {
            given: vec![
                VideoGiven {
                    peer: "p1".into(),
                    quality: VideoQuality::Main,
                },
                VideoGiven {
                    peer: "p2".into(),
                    quality: VideoQuality::Middle,
                },
                VideoGiven {
                    peer: "p3".into(),
                    quality: VideoQuality::Light,
                },
            ],
        };
        let json = encode_server_frame(&frame).expect("encode");
        assert_eq!(
            json,
            r#"{"kind":"video-given","given":[{"peer":"p1","quality":"main"},{"peer":"p2","quality":"middle"},{"peer":"p3","quality":"light"}]}"#
        );
        assert_eq!(decode_server_frame(&json).expect("decode"), frame);
        let empty = ServerFrame::VideoGiven { given: Vec::new() };
        let json = encode_server_frame(&empty).expect("encode");
        assert_eq!(json, r#"{"kind":"video-given","given":[]}"#);
        assert_eq!(decode_server_frame(&json).expect("decode"), empty);
    }

    #[test]
    fn video_given_is_bounded_like_video_wants() {
        let frame = |given: &str| format!(r#"{{"kind":"video-given","given":{given}}}"#);
        let list = |n: usize| {
            let entries: Vec<String> = (0..n)
                .map(|i| format!(r#"{{"peer":"p{i}","quality":"light"}}"#))
                .collect();
            format!("[{}]", entries.join(","))
        };
        match decode_server_frame(&frame(&list(MAX_VIDEO_GIVEN))).expect("at the bound") {
            ServerFrame::VideoGiven { given } => assert_eq!(given.len(), MAX_VIDEO_GIVEN),
            other => panic!("expected VideoGiven, got {other:?}"),
        }
        assert!(decode_server_frame(&frame(&list(MAX_VIDEO_GIVEN + 1))).is_err());
        // An empty peer, an over-long peer, an unknown quality, an unknown
        // field and a missing list are all refused.
        assert!(decode_server_frame(&frame(r#"[{"peer":"","quality":"main"}]"#)).is_err());
        let long = "p".repeat(MAX_VIDEO_WANT_PEER_BYTES + 1);
        assert!(decode_server_frame(&frame(&format!(
            r#"[{{"peer":"{long}","quality":"main"}}]"#
        )))
        .is_err());
        assert!(decode_server_frame(&frame(r#"[{"peer":"p","quality":"full"}]"#)).is_err());
        assert!(decode_server_frame(&frame(r#"[{"peer":"p","quality":"main","x":1}]"#)).is_err());
        assert!(decode_server_frame(r#"{"kind":"video-given"}"#).is_err());

        // The encoder refuses what a client would.
        let entry = |peer: String| VideoGiven {
            peer,
            quality: VideoQuality::Main,
        };
        let many = |n: usize| ServerFrame::VideoGiven {
            given: (0..n).map(|i| entry(format!("p{i}"))).collect(),
        };
        assert!(encode_server_frame(&many(MAX_VIDEO_GIVEN)).is_ok());
        assert!(encode_server_frame(&many(MAX_VIDEO_GIVEN + 1)).is_err());
        let one = |peer: String| ServerFrame::VideoGiven {
            given: vec![entry(peer)],
        };
        assert!(encode_server_frame(&one(String::new())).is_err());
        assert!(encode_server_frame(&one(long)).is_err());
    }

    #[test]
    fn video_needed_round_trips_in_the_wire_shape() {
        for (main, middle, wire) in [
            (
                true,
                true,
                r#"{"kind":"video-needed","main":true,"middle":true}"#,
            ),
            (
                false,
                true,
                r#"{"kind":"video-needed","main":false,"middle":true}"#,
            ),
            (
                true,
                false,
                r#"{"kind":"video-needed","main":true,"middle":false}"#,
            ),
            (
                false,
                false,
                r#"{"kind":"video-needed","main":false,"middle":false}"#,
            ),
        ] {
            let frame = ServerFrame::VideoNeeded { main, middle };
            assert_eq!(encode_server_frame(&frame).expect("encode"), wire);
            assert_eq!(decode_server_frame(wire).expect("decode"), frame);
        }
    }

    #[test]
    fn video_needed_requires_two_booleans() {
        // A missing field, a field of another type and a null are refused:
        // a client must never have to guess a default.
        for bad in [
            r#"{"kind":"video-needed"}"#,
            r#"{"kind":"video-needed","main":true}"#,
            r#"{"kind":"video-needed","middle":true}"#,
            r#"{"kind":"video-needed","main":1,"middle":true}"#,
            r#"{"kind":"video-needed","main":"true","middle":true}"#,
            r#"{"kind":"video-needed","main":true,"middle":null}"#,
            r#"{"kind":"video-needed","main":[true],"middle":false}"#,
        ] {
            assert!(decode_server_frame(bad).is_err(), "accepted {bad}");
        }
        // The frame is a server frame only.
        assert!(
            decode_client_frame(r#"{"kind":"video-needed","main":true,"middle":true}"#).is_err()
        );
    }

    #[test]
    fn publish_budget_round_trips_in_the_wire_shape() {
        for (main_kbps, middle_kbps, wire) in [
            (
                2500,
                500,
                r#"{"kind":"publish-budget","main_kbps":2500,"middle_kbps":500}"#,
            ),
            (
                1,
                1,
                r#"{"kind":"publish-budget","main_kbps":1,"middle_kbps":1}"#,
            ),
            (
                MAX_PUBLISH_BUDGET_KBPS,
                150,
                r#"{"kind":"publish-budget","main_kbps":20000,"middle_kbps":150}"#,
            ),
        ] {
            let frame = ServerFrame::PublishBudget {
                main_kbps,
                middle_kbps,
            };
            assert_eq!(encode_server_frame(&frame).expect("encode"), wire);
            assert_eq!(decode_server_frame(wire).expect("decode"), frame);
        }
    }

    #[test]
    fn publish_budget_is_bounded_and_requires_both_ceilings() {
        // A missing field, a field of another type, a null, a fraction, a
        // negative number, zero and a value above the bound are refused: a
        // client must never have to guess a ceiling.
        for bad in [
            r#"{"kind":"publish-budget"}"#,
            r#"{"kind":"publish-budget","main_kbps":2500}"#,
            r#"{"kind":"publish-budget","middle_kbps":500}"#,
            r#"{"kind":"publish-budget","main_kbps":"2500","middle_kbps":500}"#,
            r#"{"kind":"publish-budget","main_kbps":2500,"middle_kbps":null}"#,
            r#"{"kind":"publish-budget","main_kbps":2500.5,"middle_kbps":500}"#,
            r#"{"kind":"publish-budget","main_kbps":-1,"middle_kbps":500}"#,
            r#"{"kind":"publish-budget","main_kbps":0,"middle_kbps":500}"#,
            r#"{"kind":"publish-budget","main_kbps":2500,"middle_kbps":0}"#,
            r#"{"kind":"publish-budget","main_kbps":20001,"middle_kbps":500}"#,
            r#"{"kind":"publish-budget","main_kbps":2500,"middle_kbps":20001}"#,
            r#"{"kind":"publish-budget","main_kbps":4294967296,"middle_kbps":500}"#,
            r#"{"kind":"publish-budget","main_kbps":true,"middle_kbps":500}"#,
        ] {
            assert!(decode_server_frame(bad).is_err(), "accepted {bad}");
        }
        // The frame is a server frame only.
        assert!(decode_client_frame(
            r#"{"kind":"publish-budget","main_kbps":2500,"middle_kbps":500}"#
        )
        .is_err());
        // The encoder refuses what a client would.
        let frame = |main_kbps, middle_kbps| ServerFrame::PublishBudget {
            main_kbps,
            middle_kbps,
        };
        assert!(encode_server_frame(&frame(0, 500)).is_err());
        assert!(encode_server_frame(&frame(2500, 0)).is_err());
        assert!(encode_server_frame(&frame(MAX_PUBLISH_BUDGET_KBPS + 1, 500)).is_err());
        assert!(encode_server_frame(&frame(2500, MAX_PUBLISH_BUDGET_KBPS + 1)).is_err());
        assert_eq!(publish_budget_problem(1, MAX_PUBLISH_BUDGET_KBPS), None);
    }

    // -- kind_str() drift guards (metric labels must match serde tags) -----

    /// `ServerFrame::kind_str()` must equal the serialized `kind` tag for
    /// every variant — a metric label that drifts from the wire tag is a
    /// silent dashboard lie.
    #[test]
    fn server_frame_kind_str_matches_serde_tag() {
        let frames = [
            ServerFrame::Welcome {
                peer_id: "p".into(),
                call_id: "c".into(),
                roster: vec![],
                server_ts: 0,
            },
            ServerFrame::PeerJoined {
                peer_id: "p".into(),
                account_pseudonym: "a".into(),
                peer_tag: "t".into(),
            },
            ServerFrame::PeerLeft {
                peer_id: "p".into(),
            },
            ServerFrame::SignalRelay {
                from: "p".into(),
                payload_b64: "AAAA".into(),
            },
            ServerFrame::Error {
                code: ErrorCode::BadFrame,
                detail: "d".into(),
            },
            ServerFrame::Pong { nonce: 1 },
            ServerFrame::SdpAnswer { sdp: "v=0".into() },
            ServerFrame::SdpOffer { sdp: "v=0".into() },
            ServerFrame::VideoGiven {
                given: vec![VideoGiven {
                    peer: "p".into(),
                    quality: VideoQuality::Middle,
                }],
            },
            ServerFrame::VideoNeeded {
                main: false,
                middle: true,
            },
            ServerFrame::PublishBudget {
                main_kbps: 1200,
                middle_kbps: 500,
            },
        ];
        for frame in &frames {
            let json = encode_server_frame(frame).expect("encode");
            let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
            let tag = value
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .expect("kind tag present");
            assert_eq!(
                frame.kind_str(),
                tag,
                "ServerFrame::kind_str drifted from serde tag: {json}"
            );
        }
    }

    /// Same drift guard for every `ClientFrame` variant.
    #[test]
    fn client_frame_kind_str_matches_serde_tag() {
        let frames = [
            ClientFrame::SignalRelay {
                to: RelayTarget::Call,
                payload_b64: "AAAA".into(),
            },
            ClientFrame::Ping { nonce: 1 },
            ClientFrame::SdpOffer {
                sdp: "v=0".into(),
                light_mids: vec!["1".into()],
                middle_mids: vec!["2".into()],
            },
            ClientFrame::IceCandidate {
                candidate_sdp: "candidate:1".into(),
            },
            ClientFrame::SdpAnswer { sdp: "v=0".into() },
            ClientFrame::VideoWants {
                wants: vec![VideoWant {
                    peer: "p1".into(),
                    quality: VideoQuality::Middle,
                }],
            },
        ];
        for frame in &frames {
            let json = encode_client_frame(frame).expect("encode");
            let value: serde_json::Value = serde_json::from_str(&json).expect("parse");
            let tag = value
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .expect("kind tag present");
            assert_eq!(
                frame.kind_str(),
                tag,
                "ClientFrame::kind_str drifted from serde tag: {json}"
            );
        }
    }
}
