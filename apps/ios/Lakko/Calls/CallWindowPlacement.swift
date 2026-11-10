// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreGraphics
import Foundation

/// Where the collapsed call window sits and how big it is
/// (`docs/ux/calls.md` section 4.5.2), as values.
///
/// Placement is a fact about one device and one hand, so it is this client's
/// own and not a shared rule; the numbers are the spec's, which all three
/// clients follow. Pure: no SwiftUI. The overlay measures the area it may use
/// (the safe area with the keyboard taken out) and draws the answer.
enum CallWindowGeometry {

    /// The gap from the safe edges of the area, in points.
    static let margin: CGFloat = 16
    /// How far below the top of the safe area the top corners sit, so the
    /// window clears a screen's own header (navigation bar and title) and
    /// covers neither the composer nor the send button. Chosen, not measured:
    /// a standard navigation bar is 44 points, a large title more.
    static let headerClearance: CGFloat = 56
    /// The narrow side of the screen under this many points is a phone.
    static let compactLimit: CGFloat = 600
    /// A phone's window is this fraction of the screen width...
    static let phoneWidthFraction: CGFloat = 0.36
    /// ...and never narrower than this.
    static let phoneMinWidth: CGFloat = 120
    /// A tablet's window width.
    static let wideWidth: CGFloat = 320
    /// The width of the strip a tucked window leaves at the edge.
    static let stripWidth: CGFloat = 28
    /// The height of that strip.
    static let stripHeight: CGFloat = 72

    /// Whether `screen` (the whole screen, in points) is a phone's.
    static func isPhone(screen: CGSize) -> Bool {
        min(screen.width, screen.height) < compactLimit
    }

    /// The window's size on `screen`. A phone's is 3:4, upright, 36% of the
    /// screen width and at least 120 points; a tablet's 16:9 and 320 points
    /// wide; a screen share is 16:9 at the same width on either.
    static func size(screen: CGSize, screenShare: Bool) -> CGSize {
        let width: CGFloat
        if isPhone(screen: screen) {
            width = max(phoneMinWidth, (screen.width * phoneWidthFraction).rounded())
        } else {
            width = wideWidth
        }
        let lying = screenShare || !isPhone(screen: screen)
        let height = lying ? (width * 9 / 16).rounded() : (width * 4 / 3).rounded()
        return CGSize(width: width, height: height)
    }

    /// The window's frame in `corner` of `area` (the safe area with the
    /// keyboard taken out, in its own coordinates).
    ///
    /// A window that does not fit the area's height (a short landscape screen
    /// with the keyboard up) keeps to the top rather than leaving the area at
    /// the top edge.
    static func frame(corner: CallWindowCorner, size: CGSize, in area: CGRect) -> CGRect {
        let left = area.minX + margin
        let right = area.maxX - margin - size.width
        let top = area.minY + headerClearance
        let bottom = max(top, area.maxY - margin - size.height)
        let x = corner.isLeading ? left : max(left, right)
        let y = corner.isTop ? top : bottom
        return CGRect(origin: CGPoint(x: x, y: y), size: size)
    }

    /// The corner a window lands in when released with its centre heading for
    /// `projected` (where it would come to rest given how fast it was moving):
    /// the corner whose window centre is closest to that point.
    static func nearestCorner(to projected: CGPoint, size: CGSize, in area: CGRect) -> CallWindowCorner {
        var best = CallWindowCorner.topTrailing
        var bestDistance = CGFloat.greatestFiniteMagnitude
        for corner in CallWindowCorner.allCases {
            let window = frame(corner: corner, size: size, in: area)
            let center = CGPoint(x: window.midX, y: window.midY)
            let distance = hypot(center.x - projected.x, center.y - projected.y)
            if distance < bestDistance {
                best = corner
                bestDistance = distance
            }
        }
        return best
    }

    /// Whether a release heading for `projected` tucks the window away: on a
    /// phone, when the window's centre would come to rest past the left or
    /// right edge of the area. Nothing on a tablet, where the window is not
    /// thrown.
    static func tuck(projected: CGPoint, in area: CGRect, phone: Bool) -> CallWindowTuck? {
        guard phone else { return nil }
        if projected.x < area.minX { return .leading }
        if projected.x > area.maxX { return .trailing }
        return nil
    }

    /// The strip a tucked window leaves: at the edge it was tucked past, level
    /// with the middle of the corner it came from.
    static func stripFrame(
        tuck: CallWindowTuck, corner: CallWindowCorner, size: CGSize, in area: CGRect
    ) -> CGRect {
        let window = frame(corner: corner, size: size, in: area)
        let x = tuck == .leading ? area.minX : area.maxX - stripWidth
        let y = window.midY - stripHeight / 2
        return CGRect(x: x, y: y, width: stripWidth, height: stripHeight)
    }

    /// Whether a drag on the strip brings the window back: far enough away
    /// from the edge it is tucked at.
    static func untucks(_ translation: CGSize, from tuck: CallWindowTuck) -> Bool {
        tuck == .leading ? translation.width > 24 : translation.width < -24
    }

    // MARK: - One touch on the window

    /// How far a finger must move before a touch is a drag and not a tap.
    static let dragThreshold: CGFloat = 8
    /// The mute button's touch target, and its inset from the window's top
    /// right corner.
    static let muteTarget: CGFloat = 44
    static let muteInset: CGFloat = 2

    /// Whether `translation` has gone beyond the drag threshold.
    static func isDrag(_ translation: CGSize) -> Bool {
        hypot(translation.width, translation.height) >= dragThreshold
    }

    /// The mute button's target inside a window drawn at `window`.
    static func muteFrame(window: CGRect) -> CGRect {
        CGRect(
            x: window.maxX - muteInset - muteTarget, y: window.minY + muteInset,
            width: muteTarget, height: muteTarget)
    }

    /// What one touch sequence on the window does. A sequence that ever moved
    /// beyond the drag threshold only moves the window, wherever it started,
    /// so a drag that begins on the mute button never toggles the microphone;
    /// a tap toggles it on the mute button and brings the call back anywhere
    /// else. Decided once, at the end of the sequence, from the whole of it.
    static func touchOutcome(start: CGPoint, moved: Bool, window: CGRect) -> CallWindowTouch {
        if moved { return .move }
        return muteFrame(window: window).contains(start) ? .toggleMute : .expand
    }

    /// The corner of the window the own camera inset sits in: the bottom
    /// corner away from the screen edge the window is on.
    static func insetOnLeading(corner: CallWindowCorner) -> Bool {
        !corner.isLeading
    }
}

/// A corner of the area the window may use.
enum CallWindowCorner: String, CaseIterable, Equatable, Sendable {
    case topLeading
    case topTrailing
    case bottomLeading
    case bottomTrailing

    var isTop: Bool { self == .topLeading || self == .topTrailing }
    var isLeading: Bool { self == .topLeading || self == .bottomLeading }
}

/// What one touch sequence on the window did.
enum CallWindowTouch: Equatable, Sendable {
    /// A tap anywhere but the mute button: bring the call back.
    case expand
    /// A tap on the mute button.
    case toggleMute
    /// A drag: the window moves and lands.
    case move
}

/// The edge a phone's window was tucked past.
enum CallWindowTuck: Equatable, Sendable {
    case leading
    case trailing
}

/// The corner the window landed in last, kept while the app runs and nowhere
/// else (section 4.5.2): the next collapsed call opens there. Never written to
/// disk and never shared with another device: it is about this hand on this
/// screen today.
@MainActor
enum CallWindowPlacementMemory {
    /// The window starts in the top right corner.
    static var corner: CallWindowCorner = .topTrailing
}
