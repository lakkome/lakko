// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/**
 * Keeping the newest message in view when the space for the conversation
 * changes under it: the keyboard opening, the composer growing (a reply or
 * edit strip, a longer draft), the pinned bar appearing, or the last bubble
 * growing after an edit.
 *
 * The timeline is an ordinary top-anchored list, so when its viewport gets
 * shorter it keeps its FIRST visible row where it was and the newest rows
 * slide under the composer. This rule says when to scroll them back: only
 * while the person was at the bottom before the change and is not scrolling
 * themselves. A person reading older messages is left where they are.
 */
public object StickToBottom {

    /** Within this many pixels of the end counts as "at the bottom". */
    public const val TOLERANCE_PX: Int = 1

    /**
     * The geometry of one layout of the list. [lastVisibleEnd] is where the
     * last visible row ends, [viewportEnd] the end of the space rows may use
     * (the viewport minus the bottom content padding), both in the same
     * coordinates.
     */
    public data class Geometry(
        val totalItems: Int,
        val lastVisibleIndex: Int,
        val lastVisibleEnd: Int,
        val viewportEnd: Int,
    ) {
        /** The list's last row is fully in view (or there is nothing). */
        val atBottom: Boolean
            get() = totalItems == 0 ||
                (lastVisibleIndex == totalItems - 1 && lastVisibleEnd <= viewportEnd + TOLERANCE_PX)
    }

    /** What to do after a layout. */
    public sealed interface Action {
        /** Leave the list where it is. */
        public data object None : Action

        /** Scroll forward by [px] pixels: the last row is in view but cut off. */
        public data class ScrollBy(val px: Int) : Action

        /** Jump to the last row: it is not in view at all. */
        public data object JumpToEnd : Action
    }

    /** The action for a layout and whether the list is held at the bottom after it. */
    public data class Decision(val action: Action, val stuck: Boolean)

    /**
     * [stuck]: the list was held at the bottom before this layout.
     * [scrolling]: a scroll is in progress (the person's, or one this screen
     * started, such as a jump to a pinned message); the list then follows it
     * and is held at the bottom only if the scroll ends there.
     */
    public fun decide(stuck: Boolean, scrolling: Boolean, geometry: Geometry): Decision {
        if (scrolling || !stuck || geometry.atBottom) {
            return Decision(Action.None, stuck = geometry.atBottom)
        }
        val action = if (geometry.lastVisibleIndex == geometry.totalItems - 1) {
            Action.ScrollBy(geometry.lastVisibleEnd - geometry.viewportEnd)
        } else {
            Action.JumpToEnd
        }
        return Decision(action, stuck = true)
    }
}
