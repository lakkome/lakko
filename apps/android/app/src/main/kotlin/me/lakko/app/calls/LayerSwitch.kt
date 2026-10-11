// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import uniffi.lakko_ffi_uniffi.LayerDisplay
import uniffi.lakko_ffi_uniffi.LayerDisplayCounters
import uniffi.lakko_ffi_uniffi.LayerDisplayInterface
import uniffi.lakko_ffi_uniffi.ShownLayer
import uniffi.lakko_ffi_uniffi.TileVideoQuality
import uniffi.lakko_ffi_uniffi.addLayerDisplayCounters

/**
 * Whether a tile shows the heavy picture it asked for of a remote camera or
 * that camera's light copy (`docs/ux/group-call-grid.md` section 7.5,
 * ADR-0067 step 5, ADR-0070).
 *
 * The rule and its counters are written once, in Rust (`crates/lakko-ui-rules`,
 * `layer_display`), and reached here over UniFFI as [LayerDisplay]; web and
 * iOS reach the same rule, so the three clients switch at the same moments.
 * This file is the Android wiring around it: [LayeredTileDriver] is what one
 * remote tile asks, [ShownLayerLedger] adds up what the tiles of a call
 * counted, [describeShown] is the log line. Nothing here decides when to
 * switch.
 *
 * No Android or libwebrtc type: the host JVM tests drive all of it with a
 * fake [LayerDisplayInterface], since they cannot load the native library.
 */

/**
 * One remote tile's use of the shared rule.
 *
 * The tile has one renderer and a sink on each of the two tracks it shows of
 * the person: a heavy one (the main or the middle track) and the light one.
 * Each sink asks [heavyFrame] / [lightFrame] for every frame its track
 * delivers and hands the frame to the renderer only when told to; [tick] runs
 * every `layerTickMs()` so a picture that stopped is noticed without a frame
 * arriving.
 *
 * Thread-safe where it has to be: the two frame callbacks run on libwebrtc's
 * decoder threads while everything else runs on the main thread. The shared
 * rule is itself thread-safe; [observing] and [closed] are volatile.
 *
 * @param rule the shared rule, one per tile.
 * @param clock monotonic milliseconds, one clock for every call into [rule].
 * @param release frees [rule]; called once, by [close].
 * @param observing whether the tile can present frames now (the app is in the
 *   foreground).
 */
internal class LayeredTileDriver(
    private val rule: LayerDisplayInterface,
    private val clock: () -> Long,
    private val release: () -> Unit = {},
    observing: Boolean = true,
) {
    @Volatile
    private var observing: Boolean = observing

    @Volatile
    private var closed: Boolean = false

    /** The tile has no heavy picture: the light track is its one picture. */
    @Volatile
    private var lightOnly: Boolean = false

    /** The person's light track is on the tile. */
    @Volatile
    private var hasLight: Boolean = false

    /** When the heavy picture was last replaced and has not delivered a frame
     *  since; `null` otherwise. */
    @Volatile
    private var heavySwappedAt: Long? = null

    private fun now(): ULong = clock().coerceAtLeast(0L).toULong()

    /** Run [ask] against the rule; `null` when the tile is not observing, is
     *  closed, or the rule could not be asked. */
    private inline fun <T> asked(ask: (ULong) -> T): T? {
        if (closed || !observing) return null
        return try {
            ask(now())
        } catch (cause: RuntimeException) {
            // The rule was released under a late frame callback.
            null
        }
    }

    /**
     * Which of the person's tracks the tile's sinks are on now
     * (`VideoWants.kt`, `VideoGiven.kt`): a heavy one (the main or the middle
     * track) or none, and the light one or none.
     *
     * With [heavyReplaced] the heavy picture is another one than before (the
     * tile asked for another, the SFU gives another, the person's middle
     * track arrived): the rule starts again, as for a new tile. `resync`
     * forgets the last frame of the old picture, and the rule counts a
     * displayed stall only from a frame it has seen, so neither the picture
     * the SFU paused nor the wait for the new one's first key frame is
     * counted as a stall. Until that first frame the light copy covers
     * ([coversHeavySwap]).
     *
     * The rule is told of a light track only while it is the companion of a
     * heavy one. Shown alone, the light track is the tile's one picture and
     * its frames are reported as that picture's.
     */
    fun setSources(hasHeavy: Boolean, hasLight: Boolean, heavyReplaced: Boolean) {
        if (closed) return
        lightOnly = !hasHeavy
        this.hasLight = hasLight
        try {
            if (heavyReplaced) {
                rule.resync(now())
                heavySwappedAt = if (hasHeavy) clock() else null
            }
            rule.setLightAvailable(hasLight && hasHeavy, now())
        } catch (cause: RuntimeException) {
            // Released; nothing to tell.
        }
    }

    /**
     * The heavy track (the main or the middle one) delivered a frame. `true`
     * when the tile draws it. While the rule cannot be asked the heavy track
     * is the one drawn.
     */
    fun heavyFrame(): Boolean {
        if (heavySwappedAt != null) heavySwappedAt = null
        return (asked { rule.fullFrame(it) } ?: ShownLayer.FULL) == ShownLayer.FULL
    }

    /**
     * The light track delivered a frame. `true` when the tile draws it:
     * always when it is the tile's one picture, while it covers a heavy
     * picture that was just replaced, and when the rule says the light copy
     * is the one shown. While the rule cannot be asked a companion is not
     * drawn.
     */
    fun lightFrame(): Boolean {
        if (lightOnly) {
            asked { rule.fullFrame(it) }
            return true
        }
        val shown = asked { rule.lightFrame(it) } ?: ShownLayer.FULL
        return shown == ShownLayer.LIGHT || coversHeavySwap(heavySwappedAt != null, hasLight)
    }

    /** Time has passed; notices a stall without a frame arriving. */
    fun tick() {
        val swappedAt = heavySwappedAt
        if (swappedAt != null && clock() - swappedAt >= FIRST_FRAME_GRACE_MS) {
            // No first frame came in the time the rule itself waits for one:
            // the rule decides from here.
            heavySwappedAt = null
        }
        asked { rule.tick(it) }
    }

    /** The app left the foreground: no frame is presented, so nothing is
     *  observed or counted until [resume]. */
    fun pause() {
        observing = false
    }

    /** The app is in the foreground again: the tile starts again on the full
     *  picture and counts nothing for the time it could not see. */
    fun resume() {
        if (closed) return
        try {
            rule.resync(now())
        } catch (cause: RuntimeException) {
            return
        }
        observing = true
    }

    /** Which picture the tile shows now. */
    fun shown(): ShownLayer =
        if (closed) ShownLayer.FULL else try {
            rule.shown()
        } catch (cause: RuntimeException) {
            ShownLayer.FULL
        }

    /** What the tile counted since the last drain, or `null` when it cannot
     *  be read. */
    fun drain(): LayerDisplayCounters? {
        if (closed) return null
        return try {
            rule.drain()
        } catch (cause: RuntimeException) {
            null
        }
    }

    /**
     * The tile is going away: a last look at the clock, the last counts, and
     * the rule is released. Every later call is a no-op that leaves the heavy
     * track drawn. Returns what was counted since the last drain.
     */
    fun close(): LayerDisplayCounters? {
        if (closed) return null
        tick()
        val last = drain()
        closed = true
        try {
            release()
        } catch (cause: RuntimeException) {
            // Already released.
        }
        return last
    }

    companion object {
        /** A driver over the shared rule, for a tile that starts now. */
        fun forTile(clock: () -> Long, lightAvailable: Boolean, observing: Boolean): LayeredTileDriver {
            val display = LayerDisplay(clock().coerceAtLeast(0L).toULong(), lightAvailable)
            return LayeredTileDriver(
                rule = display,
                clock = clock,
                release = { display.destroy() },
                observing = observing,
            )
        }
    }
}

/**
 * How long the light copy covers a heavy picture that was just replaced and
 * has delivered no frame yet, milliseconds: the time the shared rule itself
 * waits for a tile's first frame (the web client's `FIRST_FRAME_GRACE_MS`).
 */
internal const val FIRST_FRAME_GRACE_MS: Long = 1_500L

/**
 * What every remote tile of a call counted, by SFU peer id.
 *
 * A peer can be drawn by several tiles over a call (the grid, the stage, a
 * tile rebuilt when the layout changes), so each tile hands over what it
 * drained and the ledger adds it up. The media counters read an interval at a
 * time and the total at the end of the call. The twin of the web
 * `LayerDisplayLedger`.
 *
 * @param plus two sets of counters as one. The default is the shared rule's
 *   own sum (`addLayerDisplayCounters`); injectable because the host JVM
 *   tests cannot load the native library.
 */
internal class ShownLayerLedger(
    private val plus: (LayerDisplayCounters, LayerDisplayCounters) -> LayerDisplayCounters =
        { a, b -> addLayerDisplayCounters(a, b) },
) {
    private val totalByPeer = LinkedHashMap<String, LayerDisplayCounters>()
    private val intervalByPeer = LinkedHashMap<String, LayerDisplayCounters>()

    /** A tile of [peerId] reports what it counted since its last report. */
    @Synchronized
    fun add(peerId: String, counters: LayerDisplayCounters) {
        totalByPeer[peerId] = totalByPeer[peerId]?.let { plus(it, counters) } ?: counters
        intervalByPeer[peerId] = intervalByPeer[peerId]?.let { plus(it, counters) } ?: counters
    }

    /** Every peer a tile reported on since the last call, and start a new
     *  interval. A peer no tile reported on is absent. */
    @Synchronized
    fun takeInterval(): Map<String, LayerDisplayCounters> {
        val out = LinkedHashMap(intervalByPeer)
        intervalByPeer.clear()
        return out
    }

    /** Every peer a tile ever reported on, counted from the start. */
    @Synchronized
    fun totals(): Map<String, LayerDisplayCounters> = LinkedHashMap(totalByPeer)

    /** Forget everything: a new call starts. */
    @Synchronized
    fun reset() {
        totalByPeer.clear()
        intervalByPeer.clear()
    }
}

/** The ledger of the call this process is in: the tiles write to it, the SFU
 *  session's media counters read it. */
internal val shownLayerLedger: ShownLayerLedger = ShownLayerLedger()

/**
 * What one remote peer's tile showed, in one line: the picture this device
 * asks for of them (`want=`), the one the SFU says it gives (`given=`, `-`
 * when it has not said), how often the tile switched to the light track, how
 * long the light track was the shown one, and the longest and the total
 * displayed stall. Whole milliseconds. [peerId] is the SFU's per-call peer
 * id, the same one the stream ids carry. The same field names as the web
 * `describeShown`.
 */
internal fun describeShown(
    label: String,
    peerId: String,
    shown: LayerDisplayCounters,
    want: TileVideoQuality,
    given: TileVideoQuality? = null,
): String =
    "$label shown peer=$peerId want=${want.wireName} given=${given?.wireName ?: "-"}" +
        " light_switches=${shown.lightSwitches}" +
        " light_ms=${shown.lightMs} stall_max_ms=${shown.stallMaxMs}" +
        " stall_total_ms=${shown.stallTotalMs}"

/** [describeShown] for every peer, in the map's order. [wantOf] says which
 *  picture is asked for of each, [givenOf] which one the SFU gives (`null`
 *  when it has not said). */
internal fun describeShownPeers(
    label: String,
    byPeer: Map<String, LayerDisplayCounters>,
    wantOf: (peerId: String) -> TileVideoQuality,
    givenOf: (peerId: String) -> TileVideoQuality? = { null },
): List<String> =
    byPeer.map { (peerId, shown) -> describeShown(label, peerId, shown, wantOf(peerId), givenOf(peerId)) }
