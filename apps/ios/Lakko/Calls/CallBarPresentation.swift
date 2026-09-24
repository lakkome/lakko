// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreGraphics
import Foundation
import LakkoCore

/// The collapsed call bar (`docs/ux/calls.md` section 4.5) as values: which
/// phase the bar sees, when the running time started, what its one line says,
/// and whether a finished drag was the swipe that puts the call away.
///
/// ## What is deliberately NOT here
///
/// The two decisions with no platform in them live in
/// `crates/lakko-ui-rules/src/call_bar.rs` and are reached over UniFFI:
/// ``callBarVisible(phase:callScreenUp:)`` and
/// ``callBarStatus(phase:connectedForMs:)``. Nothing in this file re-decides
/// either.
///
/// ``line(_:)`` prints the Rust verdict in the same four strings
/// ``CallStatusText/line(phase:durationMs:)`` prints, so the bar and the call
/// screen cannot drift apart — and because the verdict arrives as a sum type
/// (`CallBarStatus`) rather than three booleans, "the state or the running
/// time, never both" is not something a view here can break.
enum CallBarPresentation {

    // MARK: - Which phase the bar sees

    /// The bar's view of a 1x1 ``CallSession``.
    ///
    /// This is **not** `CallUiState.mapSessionPhase`, and the single case they
    /// differ on is the reason it exists: that mapping folds `.ringing` into
    /// `Calling`, which is harmless for the call screen (a screen only exists
    /// once the ring is over) and wrong here. A call ringing in a **muted**
    /// conversation is offered with no screen at all (section 2.1.1), so
    /// `callScreenUp` is `false` and the phase is the only thing left to keep
    /// the bar away — and section 4.5 says a call that is only ringing does not
    /// collapse.
    ///
    /// `.ending` reports `Connected`: a call being hung up is still a call, and
    /// the screen's own status line says the same.
    static func phase(oneToOne state: CallSession.WebCallState?) -> CallBarPhase {
        switch state {
        case nil: return .idle
        case .idle?: return .preparing
        case .outgoing?: return .calling
        case .ringing?: return .ringing
        case .connecting?: return .connecting
        case .connected?, .ending?: return .connected
        case .done?: return .ended
        }
    }

    /// The bar's view of an externally driven call (group, or a call link),
    /// whose phase the host already holds in a ``CallUiState``.
    static func phase(external phase: CallUiState.CallPhase) -> CallBarPhase {
        switch phase {
        case .preparing: return .preparing
        case .calling: return .calling
        case .connecting: return .connecting
        case .connected: return .connected
        case .reconnecting: return .reconnecting
        case .ended: return .ended
        }
    }

    // MARK: - How long a collapse lasts

    /// Whether a collapse outlives this phase.
    ///
    /// Derived from the Rust rule rather than restated: the phases a collapse
    /// outlives are exactly the phases the bar is shown in when no screen is
    /// up, and keeping one copy of that set is the whole reason the rule is in
    /// `lakko-ui-rules`. A second list here would be the drift it exists to
    /// prevent.
    static func collapseSurvives(_ phase: CallBarPhase) -> Bool {
        callBarVisible(phase: phase, callScreenUp: false)
    }

    /// When the running time started, carried across a collapse.
    ///
    /// The call screen owned this as view state, so a call put away for two
    /// minutes and brought back read `0:00` while the bar a second earlier had
    /// read `2:07`. Section 4.5 says collapsing leaves the media, the keys and
    /// the timer untouched, so the moment belongs to the call rather than to a
    /// screen that comes and goes.
    ///
    /// A phase that is not `Connected` clears it, so a reconnect restarts the
    /// clock. That is the call screen's existing behaviour, kept rather than
    /// quietly changed here: the bar reads `Reconnecting...` while it is gone,
    /// so nothing shows a time that would disagree with the screen's.
    static func connectedAt(previous: Date?, phase: CallBarPhase, now: Date) -> Date? {
        guard phase == .connected else { return nil }
        return previous ?? now
    }

    // MARK: - The one line

    /// The words for a verdict. The *decision* of which of the four to say is
    /// the Rust rule's; this only prints it, in the strings the call screen
    /// already prints.
    static func line(_ status: CallBarStatus) -> String {
        switch status {
        case .calling: return "Calling..."
        case .connecting: return "Connecting..."
        case .reconnecting: return "Reconnecting..."
        case .elapsed(let ms): return CallTranscriptRow.durationLabel(ms: Int64(clamping: ms))
        }
    }

    /// What VoiceOver reads instead of the line: a duration as a duration,
    /// never digit by digit (`docs/ux/calls.md` section 8).
    static func spokenLine(_ status: CallBarStatus) -> String {
        if case .elapsed(let ms) = status {
            return CallTranscriptRow.spokenDuration(ms: Int64(clamping: ms))
        }
        return line(status)
    }

    /// The whole bar spoken as one button: who is on the call, how long it has
    /// been, and what tapping it does (section 4.5).
    ///
    /// `isPerson` is what keeps a group call from being read "Call with Group
    /// call": the spec's example sentence (`Call with Anna, …`) is the 1x1
    /// shape, and a call whose label is already the name of a call carries it
    /// on its own.
    static func spokenLabel(name: String, isPerson: Bool, status: CallBarStatus) -> String {
        let subject = isPerson ? "Call with \(name)" : name
        return "\(subject), \(spokenLine(status)), return to call"
    }

    // MARK: - The swipe

    /// How far down a drag must travel to put the call away.
    static let collapseSwipeDistance: CGFloat = 60

    /// Whether a finished drag on the call screen was the section-4.5 downward
    /// swipe: far enough down, and more down than sideways.
    ///
    /// The sideways test is what stops a horizontal flick, or a drag that
    /// wandered while aiming at something else, from taking the call screen
    /// away from under a thumb.
    static func isCollapseSwipe(_ translation: CGSize) -> Bool {
        translation.height >= collapseSwipeDistance
            && abs(translation.width) < translation.height
    }
}

// MARK: - The collapsed state itself

/// Whether the one live call is put away, and when it connected.
///
/// Section 4.5: *"the presentation mode is a property of the call, not of a
/// screen"* — so this is one flag beside the three call hosts rather than
/// anything a route can touch, and a navigation cannot change it because no
/// navigation can reach it. It is owned by ``CallHostOverlay``, which is
/// mounted once at the signed-in root, for the same reason the calls are.
///
/// One flag and not three: the three hosts share one call slot (``CallSlot``),
/// so a second copy of "is it collapsed" would only be a second way for them
/// to disagree.
@MainActor
final class CallBarHost: ObservableObject {

    /// The live call is put away and the bar is standing in for it.
    @Published private(set) var collapsed = false

    /// When media started flowing on the live call, or `nil`. Survives the call
    /// screen coming and going, which is the point — see
    /// ``CallBarPresentation/connectedAt(previous:phase:now:)``.
    @Published private(set) var connectedAt: Date?

    /// Put the call away: the chevron at the call screen's top left, or the
    /// downward swipe on it.
    func collapse() {
        if !collapsed { collapsed = true }
    }

    /// Bring it back: a tap on the bar.
    func expand() {
        if collapsed { collapsed = false }
    }

    /// Fold the live call's phase.
    ///
    /// Returns `true` when a **collapsed** call has just ended, which is the
    /// one outcome the caller has to act on. Section 4.5: the `Call ended`
    /// overlay belongs to the call screen, and *"a person who put the call away
    /// asked not to be interrupted by it"* — so its presentation is dropped
    /// rather than allowed to spring back over whatever they were reading. It
    /// cannot merely be left alone either: nothing would then dismiss the
    /// overlay that was never drawn, and the host would hold a finished call
    /// for the rest of the session.
    @discardableResult
    func observe(phase: CallBarPhase, now: Date = Date()) -> Bool {
        connectedAt = CallBarPresentation.connectedAt(
            previous: connectedAt, phase: phase, now: now)
        guard !CallBarPresentation.collapseSurvives(phase) else { return false }
        let wasCollapsed = collapsed
        // A collapse never outlives its call: the next one starts on the
        // screen, whatever the last one was put away as.
        collapsed = false
        return wasCollapsed && phase == .ended
    }
}
