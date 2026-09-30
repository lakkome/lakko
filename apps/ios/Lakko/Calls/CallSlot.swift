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
        // One whole sentence per (running call, start or join): `call.busy.*`,
        // never a phrase slotted into a frame (batch 5 convention).
        let start = verb == .start
        let key: String
        switch occupant {
        case .oneToOne: key = start ? "call.busy.call_start" : "call.busy.call_join"
        case .group: key = start ? "call.busy.group_start" : "call.busy.group_join"
        case .link: key = start ? "call.busy.link_start" : "call.busy.link_join"
        }
        return Localization.text(key)
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

/// The words for a call flow's failure, from the KIND of failure and never from
/// its prose (ADR-0058, batch 5; the web `calls/call-errors.ts`). Core, SFU and
/// server prose goes to the log only.
enum CallErrorText {

    /// The reason a call flow failed, or `nil` when the kind has no sentence of
    /// its own and the flow's fallback is the whole message.
    static func reason(for error: Error) -> String? {
        switch error {
        case CallJwtClientError.http(let status, _, _):
            switch status {
            case 429: return Localization.text("call.error.not_accepting")
            case 401: return Localization.text("common.error.session_expired")
            case 503: return Localization.text("call.error.unavailable")
            default: return nil
            }
        case CallJwtClientError.network:
            return Localization.text("common.error.offline")
        case LinkCallHostError.signedOut:
            return Localization.text("call.error.signed_out_start")
        case let urlError as URLError where Self.offlineCodes.contains(urlError.code):
            return Localization.text("common.error.offline")
        default:
            return nil
        }
    }

    /// The whole message: the reason when the kind has one, else the flow's own
    /// fallback (`call.error.start_group`, `call.error.join_group`, ...).
    static func message(for error: Error, fallbackKey: String) -> String {
        reason(for: error) ?? Localization.text(fallbackKey)
    }

    /// A mid-call camera start that failed: the call continues without it.
    static func cameraFailure(_ error: Error) -> String {
        switch error {
        case SfuCallSession.SfuCallError.noCamera, CallSessionError.noCamera:
            return Localization.text("call.camera_failed.not_found")
        default:
            return Localization.text("call.camera_failed.other")
        }
    }

    private static let offlineCodes: Set<URLError.Code> = [
        .notConnectedToInternet, .networkConnectionLost, .cannotConnectToHost,
        .cannotFindHost, .dnsLookupFailed, .timedOut,
    ]
}
