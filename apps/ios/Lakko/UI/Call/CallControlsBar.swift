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
    let control: CallControl
    let systemImage: String
    let on: Bool
    var enabled: Bool = true
    var busy: Bool = false
    let action: () -> Void

    private var diameter: CGFloat {
        control == .end
            ? CallScreenPresentation.endDiameter
            : CallScreenPresentation.controlDiameter
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
                Text(control.label)
                    .font(.caption2)
                    .foregroundStyle(Color.white.opacity(0.75))
                    .fixedSize()
            }
            // A control is dimmed when it cannot be used, never hidden: a
            // disappearing button moves every other one under the thumb.
            .opacity(enabled ? 1 : CallScreenPresentation.disabledOpacity)
            // Touch targets stay at least 44 pt however narrow the screen gets.
            .contentShape(Rectangle())
            .frame(minWidth: CallScreenPresentation.minimumTouchTarget)
        }
        .buttonStyle(.plain)
        .disabled(!enabled || busy)
        .accessibilityLabel(CallStatusText.spokenControlLabel(control, on: on))
    }
}

// MARK: - The bar

/// The row itself: equal circles, evenly spaced, the end button set apart by
/// twice the gap, all inside one rounded translucent container.
struct CallControlsBar<Content: View>: View {
    @ViewBuilder let content: () -> Content

    var body: some View {
        HStack(spacing: CallScreenPresentation.controlGap) {
            content()
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 14)
        .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 32, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 32, style: .continuous)
                .strokeBorder(Color.white.opacity(0.10), lineWidth: 1))
        .environment(\.colorScheme, .dark)
    }
}

/// The gap that sets the end button apart from the rest of the row. Sized here
/// rather than at each call site so the two screens that draw a row (the call
/// and the camera check) cannot drift apart.
struct CallControlsEndSpacer: View {
    var body: some View {
        Spacer().frame(width: CallScreenPresentation.endGap - CallScreenPresentation.controlGap)
    }
}
