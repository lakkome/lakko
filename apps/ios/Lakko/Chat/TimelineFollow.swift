// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CoreGraphics

/// Whether the conversation stays at its newest message when the space it is
/// shown in changes size.
///
/// The timeline is a `ScrollView` in a column with the composer below it and
/// the pinned bar (and other bars) above it. When the keyboard rises, or a bar
/// appears, the scroll view's frame gets shorter while its content offset is
/// kept, measured from the top: the newest rows end up below the visible
/// frame, under the composer. Every messenger keeps a conversation that was
/// at its newest message at its newest message; a conversation the person
/// scrolled up in keeps its place.
///
/// Both inputs are in the scroll view's own coordinate space: `contentMaxY` is
/// the bottom edge of the timeline's content measured from the top of the
/// visible frame, `viewportHeight` the height of that frame. A change of the
/// frame's height alone leaves `contentMaxY` as it was (the offset is kept),
/// so the decision can be taken against the height before the change.
public enum TimelineFollow {

    /// How far above the bottom edge the content may end and still count as
    /// at the newest message: about a line of slack, so a row still settling
    /// after a send or a fraction of a point of rounding does not count as
    /// having scrolled away. Chosen, not measured.
    public static let tolerance: CGFloat = 24

    /// Whether the newest message is in view: the content ends within the
    /// visible frame (a conversation shorter than the frame always is).
    public static func isAtBottom(contentMaxY: CGFloat, viewportHeight: CGFloat) -> Bool {
        guard contentMaxY.isFinite, viewportHeight.isFinite, viewportHeight > 0 else { return false }
        return contentMaxY <= viewportHeight + tolerance
    }

    /// Whether to scroll to the newest message after the visible frame
    /// changed height from `oldViewportHeight` to `newViewportHeight`: only
    /// when it was at the newest message before the change. A first
    /// measurement (old height 0) is not a change.
    public static func staysAtBottom(
        contentMaxY: CGFloat,
        oldViewportHeight: CGFloat,
        newViewportHeight: CGFloat
    ) -> Bool {
        guard oldViewportHeight > 0, newViewportHeight > 0 else { return false }
        guard abs(newViewportHeight - oldViewportHeight) >= 0.5 else { return false }
        return isAtBottom(contentMaxY: contentMaxY, viewportHeight: oldViewportHeight)
    }
}
