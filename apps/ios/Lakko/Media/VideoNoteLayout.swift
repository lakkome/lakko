// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreGraphics

/// `docs/ux/media-messages.md` section 3, "Timeline bubble" (amended
/// 2026-11-22) — what sits on and beside a video-note circle.
///
/// Seen on a real iPhone and a real Pixel: the duration, the time with its
/// ticks and the `Aa` control stood under the circle on loose lines and read
/// as text floating under a video. The note is now drawn like a photo: the
/// duration on a pill inside the circle at the bottom leading corner, the time
/// and ticks on a pill at the bottom trailing corner, and `Aa` beside the
/// circle on the side away from the screen edge. The same numbers are in the
/// web (`media/video-note-layout.ts`) and Android bubbles.
enum VideoNoteLayout {
    /// Height of the pills' bottom edge above the circle's bottom, as a
    /// fraction of the diameter.
    static let pillBottom: CGFloat = 0.14
    /// Distance of the pills' outer edge from the circle's side, as a fraction
    /// of the diameter.
    static let pillInset: CGFloat = 0.16
    /// Gap between the circle and the `Aa` control beside it.
    static let asideGap: CGFloat = 6

    /// Whether a pill's outer bottom corner, placed by the two fractions,
    /// lies inside a circle of `diameter`.
    static func pillCornerInside(diameter: CGFloat) -> Bool {
        let r = diameter / 2
        let dx = r - pillInset * diameter
        let dy = r - pillBottom * diameter
        return dx * dx + dy * dy < r * r
    }

    /// `Aa` sits on the side away from the screen edge: after the circle on
    /// an incoming note, before it on an own note.
    static func asideFirst(incoming: Bool) -> Bool { !incoming }

    /// What the duration pill says.
    enum DurationLabel: Equatable {
        case elapsed(ms: Int64)
        case total(ms: Int64)
    }

    /// The elapsed time while the note is the one playing with sound (paused
    /// part-way included), the total otherwise, or nothing without a duration.
    static func durationLabel(playingWithSound: Bool, progress: Double, durationMs: Int64?) -> DurationLabel? {
        guard let durationMs else { return nil }
        if playingWithSound {
            let fraction = progress.isFinite ? min(max(progress, 0), 1) : 0
            return .elapsed(ms: Int64(Double(durationMs) * fraction))
        }
        return .total(ms: durationMs)
    }
}
