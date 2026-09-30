// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// A call arriving in a MUTED conversation, and where it is offered
/// (`docs/ux/calls.md` section 2.1.1).
///
/// ## What this exists for
///
/// Muting takes away the **interruption**, not the call. The first pass read
/// "a muted chat never rings" as "show nothing at all", so a muted call opened
/// no session and had no surface — which made a muted contact **unanswerable**,
/// turning muting a conversation into half-blocking the person. Section 2.1.1
/// rejects that reading.
///
/// So a muted call now rings exactly as an unmuted one does INSIDE the app —
/// the session is built, it holds the call slot, and it stays answerable for
/// the whole ring window — and what is withheld is the interruption: no
/// ringtone, no vibration, and neither of the two surfaces that take the screen
/// over (``IncomingCallScreen`` and the phone's own call screen). It is offered
/// on two quiet surfaces instead, both of which sit IN the layout:
///
///   * the top of that conversation's own chat screen, and
///   * that conversation's row in the list.
///
/// Whichever of the two the person happens to be looking at is the one that
/// offers the call; a person on neither simply finds a missed call, which is
/// what muting asked for.
///
/// ## Why the rule is here and not in the views
///
/// No view in this target is reachable from a test, so a rule written inside a
/// SwiftUI body is a rule nothing checks — the same argument
/// ``CallScreenPresentation`` and ``ConversationInfoDestination`` are here for.
/// Two surfaces reading one plain value is also what keeps them from
/// disagreeing about whether a call is on offer.
struct QuietIncomingCall: Equatable {
    /// The conversation the call arrived in. Both surfaces match on it: the
    /// chat shows the panel only in THAT chat, and only that chat's row carries
    /// the buttons.
    let groupIdHex: String
    /// The caller, already resolved through ``CallPeerName`` — a name or
    /// nothing, never a placeholder standing in for one.
    let callerName: String?
    /// The offer's own media flag, so the word the person reads matches what
    /// the caller chose.
    let video: Bool

    /// `Voice call` / `Video call` (`docs/ux/calls.md` section 3), derived from
    /// the offer rather than stated twice.
    var kindLabel: String { CallMediaChoice.kindLabel(video: video) }

    /// The name to show, falling back to the same placeholder the full-screen
    /// incoming call uses.
    var displayName: String {
        CallPeerName.resolve(profileName: callerName, chatTitle: nil)
            ?? CallPeerName.placeholder
    }
}

/// Whether a muted call is on offer, and on which of the two quiet surfaces.
enum QuietIncomingCallPresentation {

    /// The offer, or `nil` when there is nothing to show.
    ///
    /// Bounded by the ring window without restating it: the offer exists for
    /// exactly as long as the call is `.ringing`, and the host's ring timer —
    /// armed at `LakkoCore.callRingWindowMs()` through
    /// ``CallHostTiming/ringTimeoutMs`` — ends the session when the window runs
    /// out. So both surfaces disappear together, on the one timer the unmuted
    /// ring already uses, rather than on a second copy of the number.
    ///
    /// - Parameters:
    ///   - state: the 1x1 call's state. Only a ringing call is on offer; once
    ///     it is answered the ordinary full call screen owns it.
    ///   - quiet: this ring is the quiet one — the conversation is muted
    ///     (``CallHost/incomingQuiet``). An unmuted ring has the full-screen
    ///     surfaces and must not ALSO appear in a panel and a list row.
    ///   - groupIdHex: the conversation the ringing call lives in.
    static func offer(
        state: CallSession.WebCallState?,
        quiet: Bool,
        groupIdHex: String?,
        callerName: String?,
        video: Bool
    ) -> QuietIncomingCall? {
        guard quiet, state == .ringing, let groupIdHex else { return nil }
        return QuietIncomingCall(
            groupIdHex: groupIdHex, callerName: callerName, video: video)
    }

    /// Does the open chat carry the panel?
    ///
    /// Only the conversation the call arrived in. A panel over a different
    /// chat would be a notice about somebody else's conversation sitting in
    /// the middle of the one being read.
    static func showsInConversation(
        _ offer: QuietIncomingCall?, openGroupIdHex: String
    ) -> Bool {
        guard let offer else { return false }
        return sameConversation(offer.groupIdHex, openGroupIdHex)
    }

    /// Does this list row carry Accept and Decline?
    ///
    /// Only the row of the conversation the call arrived in. A channel row
    /// never does: its id is a channel id, which no group id can equal.
    static func showsOnListRow(
        _ offer: QuietIncomingCall?, rowId: String
    ) -> Bool {
        guard let offer else { return false }
        return sameConversation(offer.groupIdHex, rowId)
    }

    /// Group ids reach the two surfaces from different places — a DS row's
    /// group hex on one side, a thread's stored id on the other — and nothing
    /// guarantees they agree on case. The mute mirror's own key is folded for
    /// the same reason.
    private static func sameConversation(_ lhs: String, _ rhs: String) -> Bool {
        lhs.caseInsensitiveCompare(rhs) == .orderedSame
    }

    // MARK: - Copy

    /// The two words on both surfaces. Stated once so the panel and the row
    /// cannot drift apart, and so they read the same as the full-screen
    /// incoming call's own buttons (`docs/ux/calls.md` section 2.2).
    static var acceptLabel: String { Localization.text("call.incoming.accept") }
    static var declineLabel: String { Localization.text("call.incoming.decline") }
    /// Section 2.2's third choice, offered by the panel on a video call. The
    /// list row does not carry it: the row has space for the two the spec names
    /// and nothing more, and accepting from it takes the call as it was
    /// offered.
    static var acceptWithoutVideoLabel: String { Localization.text("call.incoming.accept_without_video") }

    /// What VoiceOver reads for the panel and the row as a whole.
    static func spokenSummary(_ offer: QuietIncomingCall) -> String {
        Localization.text(
            "chats.row.call.label", texts: ["kind": offer.kindLabel, "name": offer.displayName])
    }
}
