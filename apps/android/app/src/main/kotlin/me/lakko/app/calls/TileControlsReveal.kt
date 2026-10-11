// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * Which call tile shows its controls after a tap
 * (`docs/ux/group-call-grid.md` section 6.2): the twin of the web
 * `tile-controls-reveal.ts`.
 *
 * A touch screen has no hover, so the pin button of a tile is brought up by
 * a tap on the tile. That tap does nothing else: it never pins. The controls
 * of one tile at a time stay up for a few seconds, and go when the time runs
 * out or the person presses anywhere else.
 *
 * Pure: the timer is handed in, so the whole rule runs in a test.
 */

/** How long a tile's controls stay up after the tap. Chosen, not measured. */
internal const val TILE_CONTROLS_REVEAL_MS: Long = 3_000L

/** Holds the one tile whose controls are up. One caller, one thread. */
internal class TileControlsReveal(
    private val timers: CallTimers,
    private val onChange: (tileId: String?) -> Unit,
) {
    private var shown: String? = null
    private var timer: Any? = null

    /** The tile whose controls are up, or `null`. */
    val tileId: String? get() = shown

    /** A tap on a tile: its controls come up, and the time starts again. */
    fun tap(tileId: String) {
        stopTimer()
        timer = timers.schedule(TILE_CONTROLS_REVEAL_MS) {
            timer = null
            set(null)
        }
        set(tileId)
    }

    /**
     * A press began somewhere: on the tile with this id, or on no tile
     * (`null`). Anywhere but the tile whose controls are up hides them.
     */
    fun pressBegan(tileId: String?) {
        if (shown != null && tileId != shown) hide()
    }

    /** Hide the controls now. */
    fun hide() {
        stopTimer()
        set(null)
    }

    /** Stop the timer; nothing is reported any more. */
    fun dispose() {
        stopTimer()
        shown = null
    }

    private fun stopTimer() {
        timer?.let { timers.cancel(it) }
        timer = null
    }

    private fun set(tileId: String?) {
        if (shown == tileId) return
        shown = tileId
        onChange(tileId)
    }
}

/**
 * Decides, for each press on the call screen, whether it hides the controls
 * a tap brought up: every press does, except one that began on the tile whose
 * controls they are (its pin button included, which is inside the tile).
 *
 * A press is reported in three steps, in this order, within one pointer
 * event: [pressSeen] by whatever watches the whole screen, before anything
 * under the finger hears of it; [pressOnTile] by the tile under the finger,
 * if there is one; [pressSettled] by the watcher again, after everything
 * under the finger has heard. A press on the status line, the layout switch,
 * the control bar, a margin or the page dots names no tile, and hides.
 *
 * Pure. One caller, one thread.
 */
internal class TilePressWatch(private val reveal: TileControlsReveal) {
    private var tileId: String? = null

    /** A press began somewhere on the screen. */
    fun pressSeen() {
        tileId = null
    }

    /** The press that began is on this tile. */
    fun pressOnTile(tileId: String) {
        this.tileId = tileId
    }

    /** Everything under the finger has heard of the press: decide. */
    fun pressSettled() {
        reveal.pressBegan(tileId)
        tileId = null
    }
}

/**
 * What pressing a tile's pin button does (section 6.2), as the two things the
 * call state is told, in order.
 *
 * Unpinning leaves the layout as it is: the large tile goes back to following
 * the speaker. Pinning is asking to see the person large: it switches to
 * Focus, which is also where an unpin leaves the screen, and the layout is
 * set first so the tiles are rebuilt once and the pin then only moves the
 * large tile. One pin at a time: pinning someone else moves the pin.
 */
internal fun togglePin(
    pinnedId: String?,
    id: String,
    setLayoutChoice: (GroupLayoutKind) -> Unit,
    pin: (String?) -> Unit,
) {
    if (pinnedId == id) {
        pin(null)
        return
    }
    setLayoutChoice(GroupLayoutKind.FOCUS)
    pin(id)
}
