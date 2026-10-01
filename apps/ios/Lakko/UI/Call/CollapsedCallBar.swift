// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// The collapsed call (`docs/ux/calls.md` section 4.5): one bar across the top
/// of the app, above whatever screen is showing, surviving every navigation
/// because it is drawn by ``CallHostOverlay`` at the signed-in root and not by
/// any screen.
///
/// Left to right, and nothing else: who is on the call, the state **or** the
/// running time, a lock only when the call screen would show its chip, a camera
/// glyph while this device's camera is sending, and one round mute button.
///
/// **There is no End here, by design.** Ending is the one irreversible control
/// in a call; the call screen sets it apart by twice the gap and paints it the
/// only red on the screen, and a strip a thumb crosses on the way to a chat is
/// the wrong place for it. Coming back costs one tap.
///
/// What it says is not decided here: ``callBarStatus(phase:connectedForMs:)``
/// in `lakko-ui-rules` answers with a sum type, and
/// ``CallBarPresentation/line(_:)`` prints it.
struct CollapsedCallBar: View {
    /// Re-renders the bar's words on a language switch (ADR-0061).
    @ObservedObject var l10n = Localization.shared
    /// Who is on the call — ``CallPeerName/callLabel(profileName:groupCall:)``,
    /// the same label the call screen's status line carries.
    let label: String
    let phase: CallBarPhase
    /// When media started flowing, or `nil`. Non-`nil` exactly when the call is
    /// connected, so it doubles as "is there a clock to read" and the view
    /// never has to re-decide that.
    let connectedAt: Date?
    /// The call screen would show its `Encrypted` chip.
    let encrypted: Bool
    /// This device's camera is sending.
    let cameraSending: Bool
    let micOn: Bool
    let onExpand: () -> Void
    let onToggleMic: () -> Void

    var body: some View {
        HStack(spacing: 8) {
            // One button for the whole strip except the mute circle, which is
            // exactly what section 4.5 asks for: "Tapping the bar anywhere else
            // brings the call back", and spoken as one button.
            Button(action: onExpand) {
                content
            }
            .buttonStyle(.plain)

            muteButton
        }
        .padding(.leading, 16)
        .padding(.trailing, 10)
        .padding(.vertical, 6)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.lakkoSurface)
        .overlay(alignment: .bottom) {
            Rectangle().fill(Color.lakkoBorder).frame(height: 1)
        }
        // Section 4.5: state changes are heard and not only seen, so
        // `Reconnecting...` reaches a screen reader without the bar being
        // focused. On the phase and never on the clock — the running time would
        // otherwise interrupt whatever is being read once a second, which is
        // why a verdict that IS the clock is skipped.
        .onChange(of: phase) { _, newPhase in
            let status = callBarStatus(phase: newPhase, connectedForMs: 0)
            if case .elapsed = status { return }
            AccessibilityNotification.Announcement(CallBarPresentation.line(status)).post()
        }
    }

    // MARK: - The line

    @ViewBuilder
    private var content: some View {
        if let connectedAt {
            // One tick a second while the call runs. The bar is the only thing
            // that needs a clock, so it owns one — the same shape the call
            // screen's status line uses.
            TimelineView(.periodic(from: connectedAt, by: 1)) { context in
                strip(callBarStatus(
                    phase: phase,
                    connectedForMs: Self.elapsedMs(since: connectedAt, at: context.date)))
            }
        } else {
            // No clock to read. The rule ignores `connectedForMs` for every
            // phase but `Connected`, which is the phase that always has one.
            strip(callBarStatus(phase: phase, connectedForMs: 0))
        }
    }

    private func strip(_ status: CallBarStatus) -> some View {
        HStack(spacing: 8) {
            Text(label)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(Color.lakkoText)
                .lineLimit(1)

            Text(CallBarPresentation.line(status))
                .font(.subheadline)
                .foregroundStyle(Color.lakkoTextMuted)
                .monospacedDigit()
                .lineLimit(1)

            // Section 4.5: a lock and never the word on its own — a claim about
            // encryption belongs where the safety number can be read, which is
            // one tap away.
            if encrypted {
                Image(systemName: "lock.fill")
                    .font(.caption2)
                    .foregroundStyle(Color.lakkoTextMuted)
            }

            // "so a collapsed video call is never a camera you forgot about".
            if cameraSending {
                Image(systemName: "video.fill")
                    .font(.caption2)
                    .foregroundStyle(Color.lakkoAccent)
            }

            Spacer(minLength: 0)
        }
        .frame(minHeight: 44)
        .contentShape(Rectangle())
        .accessibilityElement(children: .ignore)
        .accessibilityAddTraits(.isButton)
        .accessibilityLabel(
            CallBarPresentation.spokenLabel(
                name: label, status: status, encrypted: encrypted, cameraSending: cameraSending))
        // The spoken sentence above is in the interface language; the UI
        // tests find the strip by this handle instead.
        .accessibilityIdentifier("collapsed-call-bar")
    }

    private static func elapsedMs(since start: Date, at now: Date) -> UInt64 {
        UInt64(max(0, now.timeIntervalSince(start)) * 1000)
    }

    // MARK: - Mute

    /// The one control on the bar, because it is the control a person needs in
    /// the second they need it and pressing it the wrong way round is undone by
    /// pressing it again.
    ///
    /// Which face it wears is ``CallScreenPresentation/face(_:on:)`` — the same
    /// decision the call screen's row makes, so off stays the loud state. The
    /// *palette* is the bar's own: the screen's circles are white on a dark
    /// call ground, and white-on-white would be invisible on ``Color/lakkoSurface``.
    private var muteButton: some View {
        let face = CallScreenPresentation.face(.microphone, on: micOn)
        return Button(action: onToggleMic) {
            ZStack {
                Circle().fill(face == .solid ? Color.lakkoText : Color.lakkoSurfaceSunken)
                if face == .outline {
                    Circle().strokeBorder(Color.lakkoBorder, lineWidth: 1)
                }
                Image(systemName: micOn ? "mic.fill" : "mic.slash.fill")
                    .font(.system(size: 14, weight: .regular))
                    .foregroundStyle(face == .solid ? Color.lakkoSurface : Color.lakkoText)
            }
            .frame(width: 32, height: 32)
            // Section 4.1.1's floor, kept on a bar that is shorter than the
            // circle it would otherwise be sized to.
            .frame(width: 44, height: 44)
            .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(CallStatusText.spokenControlLabel(.microphone, on: micOn))
    }
}
