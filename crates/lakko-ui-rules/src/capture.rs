// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Whether a call is holding the microphone and camera, so the composer's
//! recorder must stand aside.
//!
//! `docs/ux/media-messages.md` blocks recording a voice message or a video
//! note while a call owns capture, which is right: two things cannot have the
//! microphone. What each client actually asked was a different question - "is
//! there a call in this chat's slot" - and the two differ for exactly one
//! state, an incoming call that is ringing and has not been answered. Nothing
//! is capturing then. The callee's microphone is not opened until they accept.
//!
//! That gap was invisible until a muted conversation stopped taking the
//! screen over: before it, a ringing call covered the chat, so a recorder
//! disabled behind it could not be noticed. With the quiet surfaces of
//! `docs/ux/calls.md` section 2.1.1 the person is looking straight at the
//! chat while it rings, and the one reply that fits the moment - a voice
//! message saying they cannot talk - is the one the composer refuses. Muting
//! asked for no interruption, and silently taking the recorder away is one.
//!
//! So the rule is stated as what it always meant: capture is busy when a call
//! HOLDS it, and a call that is only being offered holds nothing.

/// Whether the composer's voice and video-note recorder must stand aside.
///
/// `call_in_this_chat` is the client's own read of its call slot - the same
/// value each one already had. `unanswered_incoming_ring` narrows it to the
/// one state that owns no capture: an incoming call still being offered,
/// which the callee has not accepted.
///
/// An OUTGOING call is deliberately NOT carved out, even while it is only
/// dialing: the caller's microphone is opened as the call is placed, so it is
/// held from the first moment. The asymmetry is real rather than an
/// oversight, and it is the reason this takes the narrower flag rather than
/// "is the call ringing".
#[must_use]
pub const fn call_holds_capture(call_in_this_chat: bool, unanswered_incoming_ring: bool) -> bool {
    call_in_this_chat && !unanswered_incoming_ring
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_call_means_the_recorder_is_free() {
        assert!(!call_holds_capture(false, false));
    }

    /// The defect this rule exists to close: an offer being rung is not a
    /// call holding a microphone, and until the quiet surfaces landed nobody
    /// could see that the recorder was refusing.
    #[test]
    fn an_offer_still_being_rung_holds_nothing() {
        assert!(!call_holds_capture(true, true));
    }

    #[test]
    fn an_answered_call_holds_capture() {
        assert!(call_holds_capture(true, false));
    }

    /// A caller's microphone opens as the call is placed, so an outgoing call
    /// holds capture from its first moment - which is why the carve-out is
    /// keyed on an INCOMING offer rather than on ringing.
    #[test]
    fn an_outgoing_call_holds_capture_while_it_is_still_dialing() {
        // A client reports a dialing outgoing call as present and NOT an
        // unanswered incoming ring, which is the whole distinction.
        assert!(call_holds_capture(true, false));
    }

    /// The narrowing flag can never on its own free a recorder no call was
    /// holding, so a client that mis-reads it cannot turn the rule inside
    /// out.
    #[test]
    fn the_narrowing_flag_alone_never_blocks_anything() {
        assert_eq!(
            call_holds_capture(false, true),
            call_holds_capture(false, false)
        );
    }
}
