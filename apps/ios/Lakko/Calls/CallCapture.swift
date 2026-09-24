// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Whether a call is HOLDING the microphone and camera, so the composer's
/// voice / video-note recorder must stand aside.
///
/// ## The defect this closes
///
/// `docs/ux/media-messages.md` blocks recording while a call owns capture, which
/// is right: two things cannot have the microphone. What ``ChatScreen`` asked was
/// a different question — is there a call in the app's one call slot — and the
/// two part company for exactly one state: an incoming call that is ringing and
/// has not been answered. Nothing is capturing then. ``CallSession/acceptCall``
/// is what builds the peer connection and adds the local audio track; a ringing
/// offer has neither.
///
/// The question is whether a call is live AT ALL, not whether it is live in the
/// chat being looked at. It used to be the same question, because a live call
/// owned the whole screen and there was no other chat to be standing in;
/// `docs/ux/calls.md` section 4.5 collapses a call to a bar, so a person can be
/// in chat B with a call running from chat A — and the microphone is just as
/// busy there. Every fact below is read from the app-level ``CallHost`` /
/// ``GroupCallHost`` slot, never compared against the open chat's group id.
///
/// So with a call ringing quietly in a MUTED conversation (`docs/ux/calls.md`
/// section 2.1.1) the composer refused the one reply that fitted the moment — a
/// voice message saying they cannot talk. It was invisible until those quiet
/// surfaces landed: before them a ringing call took the screen over, so a
/// recorder disabled behind it could not be noticed.
///
/// ## Why this is a type and not a line in the view
///
/// No SwiftUI body in this target is reachable from a test, so a rule written
/// inside one is a rule nothing checks — the same argument
/// ``QuietIncomingCallPresentation`` and ``ConversationInfoDestination`` are here
/// for, and how a call slot came to be answering a question about the microphone
/// in the first place. The rule itself lives in
/// `crates/lakko-ui-rules/src/capture.rs` and is reached over UniFFI; web reaches
/// the same code over wasm. Nothing here decides it.
enum CallCapture {

    /// The call in the slot is an incoming offer still being rung, which the
    /// person has not accepted — the one state that owns no capture.
    ///
    /// `.ringing` is reached only for a fresh INBOUND offer
    /// (``CallSession/applyIncomingSignaling``), never for an outgoing dial,
    /// which goes `.outgoing`; and accepting leaves it synchronously, before
    /// ``CallSession/acceptCall`` returns. So this one state is exactly the
    /// rule's `unansweredIncomingRing`, and reading it does not need a second
    /// "was it answered" flag.
    ///
    /// An OUTGOING call is deliberately never reported here, even while it is
    /// only dialing: the caller's microphone opens as the call is placed. The
    /// asymmetry is the rule's.
    ///
    /// - Parameters:
    ///   - oneToOneState: the 1x1 call's state, or `nil` when there is none.
    ///   - otherKindInSlot: a group or call-link call holds the slot. It owns
    ///     the microphone outright, so it must not be freed by whatever a stale
    ///     1x1 slot happens to read. The app admits one call at a time, so this
    ///     never fires in a correct host — it keeps the one failure that would
    ///     matter on the blocking side.
    static func unansweredIncomingRing(
        oneToOneState: CallSession.WebCallState?,
        otherKindInSlot: Bool
    ) -> Bool {
        !otherKindInSlot && oneToOneState == .ringing
    }

    /// Whether the composer's recorder must stand aside, by the shared rule.
    ///
    /// `callIsLive` is this client's own read of its one app-wide call slot,
    /// handed over unchanged rather than short-circuited on: the rule is what
    /// decides, and one fewer branch here is one fewer place for this client to
    /// answer on its own. It is app-wide and not per-chat on purpose — see the
    /// type's note on `docs/ux/calls.md` section 4.5.
    static func ownsCapture(
        callIsLive: Bool,
        oneToOneState: CallSession.WebCallState?,
        otherKindInSlot: Bool
    ) -> Bool {
        LakkoCore.callHoldsCapture(
            callIsLive: callIsLive,
            unansweredIncomingRing: unansweredIncomingRing(
                oneToOneState: oneToOneState, otherKindInSlot: otherKindInSlot))
    }
}
