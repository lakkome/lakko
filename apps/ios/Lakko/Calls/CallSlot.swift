// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The kind of call occupying the one call slot.
enum CallKind: Equatable {
    /// A 1x1 P2P call (``CallHost``).
    case oneToOne
    /// A group (SFU) call in a chat (``GroupCallHost``).
    case group
    /// An ADR-0032 call-link call (``LinkCallHost``).
    case link
}

/// One call at a time, across the three kinds of call this client can be in.
///
/// The three app-level hosts share one full-screen call presentation, one audio
/// session and one camera, so two of them live at once would not be two calls —
/// it would be one screen driven by two sessions. Before the group and link
/// calls moved to the app level, nothing stopped that: a group call was known to
/// the 1x1 host only through the chat view model that owned it, and a link call
/// not at all, so a 1x1 offer rang straight over a live link call.
///
/// Every entry point into a call asks ``occupant(oneToOneInCall:groupCallActive:linkCallActive:)``
/// first, and a refusal is explained with ``busyMessage(_:verb:)`` rather than
/// silently ignored. Same contract and copy as `apps/web/src/calls/call-slot.ts`.
enum CallSlot {

    /// What the user tried to do when a call was refused.
    enum Verb: Equatable {
        case start
        case join
    }

    /// The call occupying the slot, or `nil` when it is free. At most one input
    /// is true in a correct app; the order only makes a bug deterministic.
    ///
    /// - Parameters:
    ///   - oneToOneInCall: a 1x1 call is being prepared, ringing, dialing,
    ///     connecting, live or ending.
    ///   - groupCallActive: a group call is starting or live.
    ///   - linkCallActive: a call-link call is starting, waiting for admission
    ///     or live.
    static func occupant(
        oneToOneInCall: Bool,
        groupCallActive: Bool,
        linkCallActive: Bool
    ) -> CallKind? {
        if oneToOneInCall { return .oneToOne }
        if groupCallActive { return .group }
        if linkCallActive { return .link }
        return nil
    }

    /// Why a new call was refused: `occupant` is the call already running,
    /// `verb` what the user tried to do. Says what to do about it, because the
    /// refusal is the whole outcome — nothing else happens.
    static func busyMessage(_ occupant: CallKind, verb: Verb) -> String {
        let what: String
        switch occupant {
        case .oneToOne: what = "a call"
        case .group: what = "a group call"
        case .link: what = "a call from a link"
        }
        let action = verb == .start ? "start" : "join"
        return "You are already in \(what). End it before you \(action) another one."
    }
}

/// Connects the three app-level call hosts so each can see the one call slot.
///
/// Every host keeps its own call; none holds another strongly. Done once by the
/// composition root, and by tests that exercise the slot across kinds.
@MainActor
enum CallSlotWiring {

    static func wire(oneToOne: CallHost, group: GroupCallHost, link: LinkCallHost) {
        let occupant: @MainActor () -> CallKind? = { [weak oneToOne, weak group, weak link] in
            CallSlot.occupant(
                oneToOneInCall: oneToOne?.occupiesSlot ?? false,
                groupCallActive: group?.occupiesSlot ?? false,
                linkCallActive: link?.occupiesSlot ?? false
            )
        }
        oneToOne.otherCallOccupant = { [weak group, weak link] in
            CallSlot.occupant(
                oneToOneInCall: false,
                groupCallActive: group?.occupiesSlot ?? false,
                linkCallActive: link?.occupiesSlot ?? false
            )
        }
        group.slotOccupant = occupant
        link.slotOccupant = occupant
        link.endCurrentCall = { [weak oneToOne, weak group, weak link] in
            switch occupant() {
            case .oneToOne?: oneToOne?.endCurrentCall()
            case .group?: group?.leave(hungUp: true)
            case .link?: link?.leave()
            case nil: break
            }
        }
    }
}
