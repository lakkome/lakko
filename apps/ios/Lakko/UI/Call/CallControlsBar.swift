// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The call's controls (`docs/ux/calls.md` sections 4.1 and 4.1.1): round icon
/// buttons — never a switch, a checkbox or a list row — in one rounded
/// translucent bar that floats over the content instead of being welded to the
/// bottom edge.
///
/// Which controls exist and whether each can be pressed is decided in
/// ``CallScreenPresentation``, so this file only draws.

// MARK: - The fit

/// The sizes the row is drawn at, set by ``CallControlsBar`` from the width it
/// is given and read by every ``CallControlButton`` inside it.
private struct CallControlFitKey: EnvironmentKey {
    static let defaultValue = CallControlBarFit.spec
}

extension EnvironmentValues {
    var callControlFit: CallControlBarFit {
        get { self[CallControlFitKey.self] }
        set { self[CallControlFitKey.self] = newValue }
    }
}

// MARK: - The circle

/// One control's face. Off is the loud state: solid white with a dark slashed
/// glyph, so a muted microphone is unmissable. On is an outline — a translucent
/// white fill with a white glyph.
struct CallControlFaceView: View {
    let systemImage: String
    let face: CallControlFace
    var diameter: CGFloat = CallScreenPresentation.controlDiameter
    /// The end button: the only red thing on a call screen.
    var destructive: Bool = false

    var body: some View {
        ZStack {
            Circle().fill(fill)
            if face == .outline && !destructive {
                Circle().strokeBorder(Color.white.opacity(0.28), lineWidth: 1)
            }
            Image(systemName: systemImage)
                .font(.system(size: diameter * 0.38, weight: .regular))
                .foregroundStyle(glyph)
        }
        .frame(width: diameter, height: diameter)
    }

    private var fill: Color {
        if destructive { return .lakkoDanger }
        return face == .solid ? .white : Color.white.opacity(0.12)
    }

    private var glyph: Color {
        if destructive { return .white }
        // The dark glyph on the solid face is the call background rather than
        // pure black, so the two read as one surface.
        return face == .solid ? Color(red: 0.06, green: 0.07, blue: 0.13) : .white
    }
}

// MARK: - The button

/// A circle with one short muted word under it. The label names the action and
/// never the state, so it does not resize when the state flips; the spoken
/// label does carry the state, because that is the one a screen reader user
/// needs (`docs/ux/calls.md` sections 4.1.1 and 8).
struct CallControlButton: View {
    /// Re-renders the word and the spoken label on a language switch.
    @ObservedObject var l10n = Localization.shared
    let control: CallControl
    let systemImage: String
    let on: Bool
    var enabled: Bool = true
    var busy: Bool = false
    /// The spoken label, for a control whose name depends on more than its
    /// own state. `nil` is ``CallStatusText/spokenControlLabel(_:on:)``.
    var spokenLabel: String? = nil
    let action: () -> Void
    @Environment(\.callControlFit) private var fit

    private var diameter: CGFloat {
        control == .end ? fit.endDiameter : fit.diameter
    }

    var body: some View {
        Button(action: action) {
            VStack(spacing: 6) {
                ZStack {
                    CallControlFaceView(
                        systemImage: systemImage,
                        face: CallScreenPresentation.face(control, on: on),
                        diameter: diameter,
                        destructive: control == .end)
                    if busy {
                        ProgressView()
                            .progressViewStyle(.circular)
                            .tint(.white)
                    }
                }
                // A group call's row on a phone carries no words
                // (`docs/ux/calls.md` section 4.1); the spoken label below
                // stays either way.
                if fit.showsLabels {
                    Text(control.label)
                        .font(.caption2)
                        .foregroundStyle(Color.white.opacity(0.75))
                        .fixedSize()
                }
            }
            // A control is dimmed when it cannot be used, never hidden: a
            // disappearing button moves every other one under the thumb. A
            // busy one keeps its full opacity: the spinner already says it is
            // working, and dimming it too would read as unavailable.
            .opacity(enabled || busy ? 1 : CallScreenPresentation.disabledOpacity)
            // Every column is the same width, sized to the screen by the bar;
            // the end button's column is at least its own circle.
            .frame(width: control == .end ? max(fit.column, fit.endDiameter) : fit.column)
            // The touch target is never smaller than 44 pt, however small the
            // circle is drawn.
            .frame(minHeight: CallScreenPresentation.minimumTouchTarget)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!enabled || busy)
        .accessibilityLabel(spokenLabel ?? CallStatusText.spokenControlLabel(control, on: on))
    }
}

// MARK: - The bar

/// The row itself: equal circles, evenly spaced, the end button set apart by
/// twice the gap, all inside one rounded translucent container.
struct CallControlsBar<Content: View>: View {
    /// The width the bar may take, margins already removed. `nil` draws the
    /// spec sizes (a preview, or a screen that has not measured itself).
    var availableWidth: CGFloat? = nil
    /// How many controls the row holds; with `availableWidth`, what the fit is
    /// computed from (`docs/ux/calls.md` section 4.1.1, and see
    /// `CallScreenPresentation.fitControlBar`).
    var count: Int = 0
    /// A fit decided by the caller, which wins over ``availableWidth``: a
    /// group call's row is sized by its own rule
    /// (``CallScreenPresentation/fitGroupControlBar(availableWidth:screenSize:count:)``).
    var explicitFit: CallControlBarFit? = nil
    @ViewBuilder let content: () -> Content

    private var fit: CallControlBarFit {
        if let explicitFit { return explicitFit }
        guard let width = availableWidth, count > 0 else { return .spec }
        return CallScreenPresentation.fitControlBar(availableWidth: width, count: count)
    }

    var body: some View {
        HStack(alignment: fit.showsLabels ? .top : .center, spacing: fit.gap) {
            content()
        }
        .environment(\.callControlFit, fit)
        .padding(.horizontal, fit.horizontalPadding)
        .padding(.vertical, fit.verticalPadding)
        .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 32, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 32, style: .continuous)
                .strokeBorder(Color.white.opacity(0.10), lineWidth: 1))
        .environment(\.colorScheme, .dark)
    }
}

/// What sets the end button apart from the rest of the row: twice the gap
/// (`docs/ux/calls.md` section 4.1.1).
///
/// A zero-width item, because the row's own spacing lands on both sides of
/// it: that IS the second gap. It used to be a spacer one gap wide, which set
/// the end button apart by three gaps and made the row one gap wider than
/// ``CallScreenPresentation/fitControlBar(availableWidth:count:diameter:endDiameter:)``
/// had sized it for.
struct CallControlsEndSpacer: View {
    var body: some View {
        Color.clear.frame(width: 0, height: 0)
    }
}
