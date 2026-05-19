// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Call session state machine (Phase 3a1 scaffolding).
//!
//! The shared Rust core does **not** own the live WebRTC
//! [`PeerConnection`]; every platform uses its native stack:
//!
//!   * iOS / macOS — Apple `WebRTC.framework` (or `GoogleWebRTC` pod).
//!   * Android — `org.webrtc:google-webrtc`.
//!   * Web — browser-native `RTCPeerConnection`.
//!
//! What the core DOES own:
//!
//!   * The **state machine** below — `Idle → Outgoing →
//!     Ringing → Connecting → Connected → Ending → Ended`
//!     with the transitions a 1×1 call goes through.
//!   * The **wire-typed primitives** — [`SdpDescription`],
//!     [`IceCandidate`], [`MediaKind`] — that travel through
//!     the `crate::signaling` codec onto the MLS group's
//!     application-message channel.
//!
//! Platform code drives `CallSession` via the [`on_event`]
//! method, receives a [`Transition`] back, and applies it to
//! its native PeerConnection (apply SDP, send candidate,
//! show / hide ringer UI, etc.).
//!
//! ## Design intent
//!
//! Keeping the WebRTC layer out of the Rust core is a
//! deliberate platform-IL split. Cross-platform Rust-WebRTC
//! crates (`webrtc-rs`, `str0m`) don't compile to wasm32
//! beyond pure-state primitives; even on native they double
//! the binary size + lock us into one ABI for media. Each
//! platform's native stack is the right tool for its
//! media-codec pipeline.
//!
//! ## SFrame interaction
//!
//! Once the state hits [`CallState::Connected`], the
//! platform code reads the matching `mls/call_id` exporter
//! via [`crate::sframe::CallKey::derive`], derives a
//! [`crate::sframe::ParticipantKey`] for each remote leaf,
//! and pipes the per-participant key into the
//! WebRTC-insertable-streams hook on the platform side. The
//! SFU never sees a plaintext frame.

use crate::signaling::CallId;

// =============================================================================
// Wire-typed primitives
// =============================================================================

/// Media kinds a call can request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    /// Audio only.
    Audio,
    /// Audio + video.
    AudioVideo,
}

impl MediaKind {
    /// Convert to / from the bitmap the wire schema carries.
    #[must_use]
    pub fn into_media_flags(self) -> crate::signaling::MediaFlags {
        match self {
            Self::Audio => crate::signaling::MediaFlags::audio_only(),
            Self::AudioVideo => crate::signaling::MediaFlags::audio_video(),
        }
    }

    /// Coerce a wire-format media-flags bitmap back into the
    /// typed enum. Audio-only is the lower-bound fallback;
    /// any future bit not represented here demotes to
    /// `AudioVideo` if `video=true`, otherwise to `Audio`.
    #[must_use]
    pub fn from_media_flags(flags: crate::signaling::MediaFlags) -> Self {
        if flags.video {
            Self::AudioVideo
        } else {
            Self::Audio
        }
    }
}

/// Role this device plays in the current call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// "I made the call." Sends `Offer`, awaits `Answer` +
    /// candidates.
    Offerer,
    /// "I picked up the call." Receives `Offer`, sends
    /// `Answer` + candidates.
    Answerer,
}

/// Type-erased SDP body — the Rust core treats SDP as opaque
/// text. The platform's PeerConnection generates it on offer
/// / answer; the wire layer ferries it; the receiver's
/// PeerConnection applies it. No content inspection in core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdpDescription {
    /// `offer` or `answer` — set by the platform code from
    /// its PeerConnection's `localDescription.type`.
    pub kind: SdpKind,
    /// The SDP body.
    pub body: String,
}

/// Discriminator for [`SdpDescription`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdpKind {
    /// SDP offer body.
    Offer,
    /// SDP answer body.
    Answer,
}

/// Trickled ICE candidate. Mirror of the same-named field
/// on `crate::signaling::SignalingBody::IceCandidate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IceCandidate {
    /// ICE candidate string in SDP `a=candidate:` form.
    pub candidate: String,
    /// SDP m-section identifier.
    pub sdp_mid: String,
    /// SDP m-section index (zero-based).
    pub sdp_m_line_index: u32,
}

// =============================================================================
// State + events + transitions
// =============================================================================

/// Phase the call is in. Phase ordering is monotonically
/// forward; the only "backwards" transitions are the explicit
/// teardown paths (`Ending → Ended`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallState {
    /// No call active.
    Idle,
    /// Local user started a call; offer has been (or is
    /// being) sent. Awaiting [`Event::RemoteAnswered`].
    Outgoing,
    /// Local user is being called; remote sent the offer.
    /// Awaiting [`Event::LocalAccepts`] or
    /// [`Event::LocalDeclines`].
    Ringing,
    /// Either side: offer + answer exchanged, ICE pair still
    /// being negotiated.
    Connecting,
    /// Media path is open. Both sides can talk + see.
    Connected,
    /// One side initiated teardown; awaiting the other side's
    /// acknowledgement or the timeout.
    Ending,
    /// Terminal. The session is fully torn down.
    Ended,
}

/// Inbound event the platform layer feeds the state machine.
/// One per signaling message + a few local-action triggers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Local user dials a contact.
    LocalDials {
        /// Media kinds the user requested.
        media: MediaKind,
    },
    /// Platform's PeerConnection produced its local SDP
    /// offer.
    LocalOfferReady(SdpDescription),
    /// Remote sent an `Offer` signaling event.
    RemoteOffered {
        sdp: SdpDescription,
        media: MediaKind,
    },
    /// Local user picked up.
    LocalAccepts,
    /// Local user declined.
    LocalDeclines,
    /// Platform's PeerConnection produced its local SDP
    /// answer (after `LocalAccepts`).
    LocalAnswerReady(SdpDescription),
    /// Remote sent an `Answer` signaling event.
    RemoteAnswered(SdpDescription),
    /// Trickled ICE candidate from either side. Source is
    /// determined by the active role + the call state.
    IceCandidate(IceCandidate),
    /// Local user hangs up (or the local PeerConnection
    /// reports failure).
    LocalEnds {
        /// Reason carried through to the wire `End` event.
        reason: crate::signaling::EndReason,
    },
    /// Remote sent an `End` signaling event.
    RemoteEnded { reason: crate::signaling::EndReason },
    /// Platform's PeerConnection signalled ICE-connected.
    IceConnected,
    /// Platform's PeerConnection signalled ICE-failed.
    IceFailed,
}

/// Outbound transition the state machine emits for the
/// platform layer to apply. Each event call returns zero or
/// more transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transition {
    /// Send the wire `Offer` signaling event with this SDP +
    /// media flags. Platform wraps + ships via the
    /// `lakko_mls::Group::create_application_message` ↔
    /// DS-journal path.
    SendOffer {
        sdp: SdpDescription,
        media: MediaKind,
    },
    /// Send the wire `Answer` signaling event.
    SendAnswer { sdp: SdpDescription },
    /// Send a trickled ICE candidate.
    SendIceCandidate(IceCandidate),
    /// Send the terminal `End` event.
    SendEnd { reason: crate::signaling::EndReason },
    /// Tell the local PeerConnection to apply the remote
    /// description.
    ApplyRemoteDescription(SdpDescription),
    /// Tell the local PeerConnection to apply a remote ICE
    /// candidate.
    ApplyRemoteIceCandidate(IceCandidate),
    /// Show the ringer UI (local user is being called).
    ShowRinger {
        /// What media the offer asks for.
        media: MediaKind,
    },
    /// Hide the ringer UI (call was accepted, declined, or
    /// timed out before pickup).
    HideRinger,
    /// Notify the UI that the call is now Connected — start
    /// the "active call" surface.
    NotifyConnected,
    /// Notify the UI that the call has fully ended.
    NotifyEnded { reason: crate::signaling::EndReason },
}

/// In-memory call session — one per active call.
///
/// Construction is via [`CallSession::idle`]; the platform
/// drives state through [`on_event`]. The session terminates
/// when the state reaches [`CallState::Ended`]; further
/// `on_event` calls in that state are no-ops.
#[derive(Debug, Clone)]
pub struct CallSession {
    /// Unique per-call id. Minted by the offer side
    /// (typically `lakko_crypto::random::fill` on 16 bytes)
    /// and copied into every signaling event.
    pub call_id: CallId,
    /// Current state.
    pub state: CallState,
    /// Role this device plays. `None` until the first
    /// dial / offer-receive event lands.
    pub role: Option<Role>,
    /// Media kinds negotiated for the call.
    pub media: Option<MediaKind>,
}

impl CallSession {
    /// Construct a fresh idle session bound to `call_id`.
    /// `call_id` is normally the random 16-byte value the
    /// offer side mints; on the answer side the platform
    /// extracts it from the inbound `CallOffer` and feeds it
    /// here before calling [`on_event`] with
    /// [`Event::RemoteOffered`].
    #[must_use]
    pub fn idle(call_id: CallId) -> Self {
        Self {
            call_id,
            state: CallState::Idle,
            role: None,
            media: None,
        }
    }

    /// Drive the state machine forward. Returns the list of
    /// [`Transition`]s the platform should apply, in order.
    ///
    /// Unexpected events for the current state are dropped
    /// silently (returns empty vec) — that's the
    /// "drop late-arriving stale candidate" path. Loud
    /// errors would force every call site into noisy match
    /// arms; silent drop matches the canonical signaling-
    /// state-machine UX (cf. WebRTC PeerConnection JSEP).
    #[allow(clippy::too_many_lines)]
    pub fn on_event(&mut self, event: Event) -> Vec<Transition> {
        match (self.state, &event) {
            // ---------- Idle ----------
            (CallState::Idle, Event::LocalDials { media }) => {
                self.role = Some(Role::Offerer);
                self.media = Some(*media);
                self.state = CallState::Outgoing;
                // No wire emission yet — the platform now
                // builds the SDP offer; the next event
                // (LocalOfferReady) emits SendOffer.
                vec![]
            },
            (CallState::Idle, Event::RemoteOffered { sdp, media }) => {
                self.role = Some(Role::Answerer);
                self.media = Some(*media);
                self.state = CallState::Ringing;
                vec![
                    Transition::ApplyRemoteDescription(sdp.clone()),
                    Transition::ShowRinger { media: *media },
                ]
            },

            // ---------- Outgoing (offerer waiting for answer) ----------
            (CallState::Outgoing, Event::LocalOfferReady(sdp)) => {
                let media = self.media.unwrap_or(MediaKind::Audio);
                vec![Transition::SendOffer {
                    sdp: sdp.clone(),
                    media,
                }]
            },
            (CallState::Outgoing, Event::RemoteAnswered(sdp)) => {
                self.state = CallState::Connecting;
                vec![Transition::ApplyRemoteDescription(sdp.clone())]
            },
            (CallState::Outgoing, Event::IceCandidate(c)) => {
                // Local candidates trickle out; the platform's
                // PeerConnection produces them via its
                // `onicecandidate` callback.
                vec![Transition::SendIceCandidate(c.clone())]
            },
            (CallState::Outgoing, Event::LocalEnds { reason } | Event::RemoteEnded { reason }) => {
                self.state = CallState::Ended;
                vec![
                    if matches!(event, Event::LocalEnds { .. }) {
                        Transition::SendEnd { reason: *reason }
                    } else {
                        Transition::NotifyEnded { reason: *reason }
                    },
                    Transition::NotifyEnded { reason: *reason },
                ]
            },

            // ---------- Ringing (answerer waiting for local pickup) ----------
            (CallState::Ringing, Event::LocalAccepts) => {
                // Stay in Ringing until LocalAnswerReady
                // produces the SDP body the platform's
                // PeerConnection minted.
                vec![Transition::HideRinger]
            },
            (CallState::Ringing, Event::LocalAnswerReady(sdp)) => {
                self.state = CallState::Connecting;
                vec![Transition::SendAnswer { sdp: sdp.clone() }]
            },
            (CallState::Ringing, Event::LocalDeclines) => {
                self.state = CallState::Ended;
                vec![
                    Transition::HideRinger,
                    Transition::SendEnd {
                        reason: crate::signaling::EndReason::Declined,
                    },
                    Transition::NotifyEnded {
                        reason: crate::signaling::EndReason::Declined,
                    },
                ]
            },
            (CallState::Ringing, Event::RemoteEnded { reason }) => {
                self.state = CallState::Ended;
                vec![
                    Transition::HideRinger,
                    Transition::NotifyEnded { reason: *reason },
                ]
            },

            // ---------- Connecting (SDP done, ICE pending) ----------
            (CallState::Connecting, Event::IceCandidate(c)) => {
                vec![Transition::SendIceCandidate(c.clone())]
            },
            (CallState::Connecting, Event::IceConnected) => {
                self.state = CallState::Connected;
                vec![Transition::NotifyConnected]
            },
            (CallState::Connecting, Event::IceFailed) => {
                self.state = CallState::Ended;
                vec![
                    Transition::SendEnd {
                        reason: crate::signaling::EndReason::Error,
                    },
                    Transition::NotifyEnded {
                        reason: crate::signaling::EndReason::Error,
                    },
                ]
            },

            // ---------- Connected (media flowing) ----------
            (CallState::Connected, Event::IceCandidate(c)) => {
                // Late candidates can arrive even after the
                // primary ICE pair is up — ICE-restart path.
                vec![Transition::SendIceCandidate(c.clone())]
            },
            (CallState::Connected, Event::IceFailed) => {
                // The platform's PeerConnection may try to
                // recover with an ICE restart; the state
                // stays Connected until the
                // platform raises a follow-up event. No
                // signaling change.
                vec![]
            },

            // ---------- Either side: terminal (Outgoing
            //  is handled by the specific arm above) ----------
            (
                CallState::Ringing | CallState::Connecting | CallState::Connected,
                Event::LocalEnds { reason },
            ) => {
                self.state = CallState::Ending;
                vec![
                    Transition::SendEnd { reason: *reason },
                    Transition::NotifyEnded { reason: *reason },
                ]
            },
            (CallState::Connecting | CallState::Connected, Event::RemoteEnded { reason }) => {
                self.state = CallState::Ended;
                vec![Transition::NotifyEnded { reason: *reason }]
            },

            // ---------- Ending / Ended / unmatched ----------
            (
                CallState::Ending,
                Event::RemoteEnded { reason: _ } | Event::IceFailed | Event::IceConnected,
            ) => {
                self.state = CallState::Ended;
                vec![]
            },
            // Unmatched event for the current state —
            // dropped silently (stale candidate, races,
            // post-end follow-up). The "drop" path is the
            // canonical signaling-state-machine UX.
            _ => vec![],
        }
    }

    /// Apply a remote signaling event's payload — convenience
    /// helper for the platform's signaling dispatcher.
    /// Receiving `CallOffer` → `RemoteOffered`, `CallAnswer`
    /// → `RemoteAnswered`, etc. The mapping is one-to-one
    /// with [`Event`] and lets the dispatcher avoid building
    /// the enum by hand.
    pub fn apply_remote_signaling(
        &mut self,
        body: &crate::signaling::SignalingBody,
    ) -> Vec<Transition> {
        use crate::signaling::SignalingBody;
        let event = match body {
            SignalingBody::Offer { sdp, media, .. } => Event::RemoteOffered {
                sdp: SdpDescription {
                    kind: SdpKind::Offer,
                    body: sdp.clone(),
                },
                media: MediaKind::from_media_flags(*media),
            },
            SignalingBody::Answer { sdp, .. } => Event::RemoteAnswered(SdpDescription {
                kind: SdpKind::Answer,
                body: sdp.clone(),
            }),
            SignalingBody::IceCandidate {
                candidate,
                sdp_mid,
                sdp_m_line_index,
                ..
            } => {
                // For incoming candidates the platform should
                // call ApplyRemoteIceCandidate, NOT
                // SendIceCandidate. Apply directly here.
                return vec![Transition::ApplyRemoteIceCandidate(IceCandidate {
                    candidate: candidate.clone(),
                    sdp_mid: sdp_mid.clone(),
                    sdp_m_line_index: *sdp_m_line_index,
                })];
            },
            SignalingBody::End { reason, .. } => Event::RemoteEnded { reason: *reason },
        };
        self.on_event(event)
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signaling::{EndReason, MediaFlags, SignalingBody};

    fn fixture_call_id() -> CallId {
        let mut id = [0u8; 16];
        id[0] = 0xAB;
        id[15] = 0xCD;
        id
    }

    fn offer_sdp() -> SdpDescription {
        SdpDescription {
            kind: SdpKind::Offer,
            body: "v=0\no=- 1 2 IN IP4 0.0.0.0".into(),
        }
    }

    fn answer_sdp() -> SdpDescription {
        SdpDescription {
            kind: SdpKind::Answer,
            body: "v=0\no=- 3 4 IN IP4 0.0.0.0".into(),
        }
    }

    fn fixture_candidate() -> IceCandidate {
        IceCandidate {
            candidate: "candidate:1 1 udp 1677729535 192.168.0.1 56789 typ srflx".into(),
            sdp_mid: "audio".into(),
            sdp_m_line_index: 0,
        }
    }

    #[test]
    fn outgoing_happy_path() {
        let mut s = CallSession::idle(fixture_call_id());
        assert_eq!(s.state, CallState::Idle);
        // Local user dials → Outgoing, no immediate wire emit.
        let t = s.on_event(Event::LocalDials {
            media: MediaKind::AudioVideo,
        });
        assert!(t.is_empty());
        assert_eq!(s.state, CallState::Outgoing);
        assert_eq!(s.role, Some(Role::Offerer));
        // Platform's PeerConnection produces the SDP offer.
        let t = s.on_event(Event::LocalOfferReady(offer_sdp()));
        assert_eq!(t.len(), 1);
        match &t[0] {
            Transition::SendOffer { media, .. } => assert_eq!(*media, MediaKind::AudioVideo),
            other => panic!("expected SendOffer, got {other:?}"),
        }
        // Remote answers → Connecting.
        let t = s.on_event(Event::RemoteAnswered(answer_sdp()));
        assert!(matches!(
            t.as_slice(),
            [Transition::ApplyRemoteDescription(_)]
        ));
        assert_eq!(s.state, CallState::Connecting);
        // ICE pair completes → Connected.
        let t = s.on_event(Event::IceConnected);
        assert!(matches!(t.as_slice(), [Transition::NotifyConnected]));
        assert_eq!(s.state, CallState::Connected);
    }

    #[test]
    fn incoming_happy_path() {
        let mut s = CallSession::idle(fixture_call_id());
        // Remote offers → Ringing + ShowRinger.
        let t = s.on_event(Event::RemoteOffered {
            sdp: offer_sdp(),
            media: MediaKind::Audio,
        });
        assert_eq!(t.len(), 2);
        assert!(matches!(t[0], Transition::ApplyRemoteDescription(_)));
        match &t[1] {
            Transition::ShowRinger { media } => assert_eq!(*media, MediaKind::Audio),
            other => panic!("expected ShowRinger, got {other:?}"),
        }
        assert_eq!(s.state, CallState::Ringing);
        assert_eq!(s.role, Some(Role::Answerer));
        // Local accepts → hide ringer, stay in Ringing
        // until LocalAnswerReady arrives.
        let t = s.on_event(Event::LocalAccepts);
        assert!(matches!(t.as_slice(), [Transition::HideRinger]));
        assert_eq!(s.state, CallState::Ringing);
        // Platform produces the SDP answer.
        let t = s.on_event(Event::LocalAnswerReady(answer_sdp()));
        assert!(matches!(t.as_slice(), [Transition::SendAnswer { .. }]));
        assert_eq!(s.state, CallState::Connecting);
        // ICE pair completes.
        let t = s.on_event(Event::IceConnected);
        assert!(matches!(t.as_slice(), [Transition::NotifyConnected]));
        assert_eq!(s.state, CallState::Connected);
    }

    #[test]
    fn incoming_decline_path() {
        let mut s = CallSession::idle(fixture_call_id());
        s.on_event(Event::RemoteOffered {
            sdp: offer_sdp(),
            media: MediaKind::Audio,
        });
        let t = s.on_event(Event::LocalDeclines);
        assert_eq!(t.len(), 3);
        assert!(matches!(t[0], Transition::HideRinger));
        match &t[1] {
            Transition::SendEnd { reason } => assert_eq!(*reason, EndReason::Declined),
            other => panic!("expected SendEnd Declined, got {other:?}"),
        }
        assert_eq!(s.state, CallState::Ended);
    }

    #[test]
    fn local_hangup_during_connected() {
        let mut s = CallSession::idle(fixture_call_id());
        s.on_event(Event::LocalDials {
            media: MediaKind::Audio,
        });
        s.on_event(Event::LocalOfferReady(offer_sdp()));
        s.on_event(Event::RemoteAnswered(answer_sdp()));
        s.on_event(Event::IceConnected);
        assert_eq!(s.state, CallState::Connected);
        let t = s.on_event(Event::LocalEnds {
            reason: EndReason::HungUp,
        });
        assert_eq!(t.len(), 2);
        assert!(matches!(
            t[0],
            Transition::SendEnd {
                reason: EndReason::HungUp
            }
        ));
        assert_eq!(s.state, CallState::Ending);
    }

    #[test]
    fn ice_failure_during_connecting() {
        let mut s = CallSession::idle(fixture_call_id());
        s.on_event(Event::LocalDials {
            media: MediaKind::Audio,
        });
        s.on_event(Event::LocalOfferReady(offer_sdp()));
        s.on_event(Event::RemoteAnswered(answer_sdp()));
        assert_eq!(s.state, CallState::Connecting);
        let t = s.on_event(Event::IceFailed);
        assert_eq!(t.len(), 2);
        match &t[0] {
            Transition::SendEnd { reason } => assert_eq!(*reason, EndReason::Error),
            other => panic!("expected SendEnd Error, got {other:?}"),
        }
        assert_eq!(s.state, CallState::Ended);
    }

    #[test]
    fn stale_event_in_ended_is_dropped() {
        let mut s = CallSession::idle(fixture_call_id());
        s.on_event(Event::LocalDials {
            media: MediaKind::Audio,
        });
        s.on_event(Event::RemoteEnded {
            reason: EndReason::Declined,
        });
        assert_eq!(s.state, CallState::Ended);
        // Any further events drop silently.
        let t = s.on_event(Event::IceCandidate(fixture_candidate()));
        assert!(t.is_empty());
        let t = s.on_event(Event::RemoteAnswered(answer_sdp()));
        assert!(t.is_empty());
    }

    #[test]
    fn apply_remote_signaling_offer() {
        let mut s = CallSession::idle(fixture_call_id());
        let body = SignalingBody::Offer {
            call_id: fixture_call_id(),
            sdp: "v=0\no=- 1 2 IN IP4 0.0.0.0".into(),
            created_at_ms: 0,
            media: MediaFlags::audio_video(),
        };
        let t = s.apply_remote_signaling(&body);
        assert_eq!(t.len(), 2);
        assert!(matches!(t[0], Transition::ApplyRemoteDescription(_)));
        assert!(matches!(
            t[1],
            Transition::ShowRinger {
                media: MediaKind::AudioVideo
            }
        ));
        assert_eq!(s.state, CallState::Ringing);
    }

    #[test]
    fn apply_remote_signaling_candidate_in_any_state() {
        // Candidates from the wire always map to
        // ApplyRemoteIceCandidate (not SendIceCandidate),
        // regardless of state.
        let mut s = CallSession::idle(fixture_call_id());
        s.on_event(Event::RemoteOffered {
            sdp: offer_sdp(),
            media: MediaKind::Audio,
        });
        let body = SignalingBody::IceCandidate {
            call_id: fixture_call_id(),
            candidate: "candidate:2 1 udp 100 1.2.3.4 1234 typ host".into(),
            sdp_mid: "audio".into(),
            sdp_m_line_index: 0,
        };
        let t = s.apply_remote_signaling(&body);
        assert!(matches!(
            t.as_slice(),
            [Transition::ApplyRemoteIceCandidate(_)]
        ));
    }
}
