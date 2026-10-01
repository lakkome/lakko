// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The call-link (`/c/<slug>`, ADR-0032) call as the app-level ``LinkCallHost``
/// holds it — the decisions, kept pure so they can be tested.
///
/// ## Why it moved
///
/// `JoinCallLandingViewModel` owned the ``MultiJoinerCall``: the landing is
/// shown only while `InviteIntake` has a parked `/c/` slug, and is replaced by
/// any contact-invite or group-join link that arrives meanwhile, which released
/// the view model and the call with it without hanging it up. Nothing stopped a
/// link call from starting while a 1x1 or group call was live, and a 1x1 offer
/// rang straight over a live link call because the 1x1 host could not see it.
/// The host now owns the orchestrator and its call state; the landing decodes
/// the link, runs the guest sign-up, asks the host for the call and shows the
/// host's state for it.
///
/// Nothing about the protocol changed: `MultiJoinerCall` still founds (owner) or
/// joins (joiner), elects the committer, dedups admissions, rekeys per
/// admission, checks the owner is in the joined group and times out a call with
/// no live committer. What is here is only where its outcome goes.
///
/// Same contract and copy as `apps/web/src/calls/link-call-host.ts`.
enum LinkCallHostRules {

    /// Where a link call is, from this device's point of view.
    enum Phase: Equatable {
        /// Settling permissions, minting the call-admission token and connecting
        /// to the SFU.
        case starting
        /// Joiner: signaling is open and the KeyPackage is out; no Welcome yet.
        case waiting
        /// In the call's MLS group with the seed derived (owner: at founding).
        case live
        /// The session closed or failed.
        case ended
        /// Joiner: nobody admitted this device within the admission window —
        /// the call has no live committer.
        case dead
        /// A setup error, or a Welcome into a group without the owner (refused).
        case error
    }

    /// What the orchestrator reported.
    enum Event: Equatable {
        case signalingOpen(owner: Bool)
        case joined
        case ended
        case noCommitter
        case error
    }

    /// The phases in which the call holds the one call slot.
    static func occupiesSlot(_ phase: Phase) -> Bool {
        phase == .starting || phase == .waiting || phase == .live
    }

    /// Fold one orchestrator report.
    ///
    /// `ended` is a weak terminal: `MultiJoinerCall` tears its session down
    /// BEFORE it reports why — an admission timeout closes the session
    /// (reported `ended`) and then reports `noCommitter`; a refused redirect
    /// closes it and then reports `error`. The later, specific report replaces
    /// `ended`. `dead` and `error` are final.
    static func reduce(_ phase: Phase, _ event: Event) -> Phase {
        if phase == .dead || phase == .error { return phase }
        switch event {
        case .signalingOpen(let owner):
            // The owner is live from founding; a joiner still starting now waits
            // for its Welcome (which may already have arrived).
            if phase == .starting && !owner { return .waiting }
            return phase
        case .joined:
            return phase == .starting || phase == .waiting ? .live : phase
        case .ended:
            return occupiesSlot(phase) ? .ended : phase
        case .noCommitter:
            return phase == .live ? phase : .dead
        case .error:
            return .error
        }
    }

    /// Whether a link call may start now.
    enum StartDecision: Equatable {
        case start
        /// This very call is already starting / waiting / live here: show it,
        /// never a second session for it.
        case alreadyInThisCall
        /// Kept as a message so a refusal on screen follows a language switch.
        case busy(message: UiText)

        /// Two decisions are equal when they would show the same words now:
        /// a message is compared by its text, not by its identity.
        static func == (lhs: StartDecision, rhs: StartDecision) -> Bool {
            switch (lhs, rhs) {
            case (.start, .start), (.alreadyInThisCall, .alreadyInThisCall): return true
            case let (.busy(a), .busy(b)): return a.text == b.text
            default: return false
            }
        }
    }

    /// One call at a time, across all kinds.
    ///
    /// - Parameters:
    ///   - occupant: the call occupying the slot, or `nil`.
    ///   - activeLinkCallIdHex: the link call in the host's slot, if it holds
    ///     the slot.
    ///   - requestedCallIdHex: the call the link names.
    static func decideStart(
        occupant: CallKind?,
        activeLinkCallIdHex: String?,
        requestedCallIdHex: String,
        verb: CallSlot.Verb
    ) -> StartDecision {
        guard let occupant else { return .start }
        if occupant == .link, activeLinkCallIdHex == requestedCallIdHex {
            return .alreadyInThisCall
        }
        return .busy(message: CallSlot.busyText(occupant, verb: verb))
    }

    /// A status line for a link call on a screen other than its landing.
    struct Notice: Equatable {
        enum Action: Equatable {
            /// Return to the link's landing.
            case open
            /// Leave the call.
            case cancel
            /// Forget an outcome.
            case dismiss
        }
        let text: String
        let actions: [Action]
    }

    /// What to show about a link call away from its landing, or `nil`.
    ///
    /// Nothing while the landing is showing the call itself, and nothing while
    /// the call is live — the full-screen call covers every screen then. Before
    /// it is live the user can move around (the admission wait can take a
    /// while), so they are told it is still going and can cancel it; after it
    /// ends they are told why once, since the landing that would have said so is
    /// gone.
    static func notice(phase: Phase?, landingShowing: Bool, errorMessage: UiText?) -> Notice? {
        guard let phase, !landingShowing else { return nil }
        switch phase {
        case .live:
            return nil
        case .starting:
            return Notice(text: Localization.text("call.link.notice.connecting"), actions: [.open, .cancel])
        case .waiting:
            return Notice(text: Localization.text("call.link.waiting"), actions: [.open, .cancel])
        case .ended:
            return Notice(text: Localization.text("call.link.ended"), actions: [.dismiss])
        case .dead:
            return Notice(text: Localization.text("call.link.dead"), actions: [.dismiss])
        case .error:
            // Two sentences through `common.sentence_pair`: the failure, then
            // the reason when its kind has one (batch 5 convention).
            let failed = Localization.text("call.link.error.generic")
            return Notice(
                text: errorMessage.map {
                    Localization.text("common.sentence_pair", texts: ["first": failed, "second": $0.text])
                } ?? failed,
                actions: [.dismiss]
            )
        }
    }
}
