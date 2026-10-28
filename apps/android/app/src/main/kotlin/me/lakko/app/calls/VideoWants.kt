// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import uniffi.lakko_ffi_uniffi.TileVideoQuality

/**
 * Which picture of a person a tile shows, and what this device asks the SFU
 * for (`docs/ux/group-call-grid.md` section 7.6, ADR-0070 step 5): the twin
 * of the web `video-wants.ts`.
 *
 * A person whose camera is on sends up to three copies of it: the main one,
 * a middle one (half the size) and the light one (a quarter). The light one
 * always arrives. Of the two heavy ones the SFU forwards the one this device
 * says it shows, per person, in a `video-wants` frame; a person this device
 * shows no heavy picture of costs it the light stream alone.
 *
 * Which one a tile shows is the shared rule
 * (`lakko_ui_rules::call_group::tile_video_quality`, over UniFFI here): the
 * people in the call, the tile's width in physical pixels, and whether it is
 * the pinned person or the stage. This file is the Android side of it:
 *
 *   * [tileQualityOf] asks the rule;
 *   * [askedQuality] and [heavyTrackFor] turn its answer into what is asked
 *     of the SFU and which track the tile's heavy sink is on;
 *   * [VideoWantsBook] keeps, per person, the highest quality any drawn tile
 *     of them wants;
 *   * [VideoWantsSender] sends the whole set when it changes, at most once
 *     per [VIDEO_WANTS_MIN_INTERVAL_MS];
 *   * [SettledWidth] lets a tile's width settle before it is asked about
 *     again, so a rotation or a layout animation does not ask the sender for
 *     a key frame on every step.
 *
 * Pure: no Compose, no Android; timers and the clock are handed in. The
 * quality is the binding's own enum, a plain Kotlin enum the host JVM tests
 * can name without the native library.
 */

/** The name a quality has in the `video-wants` frame and in the call log. */
internal val TileVideoQuality.wireName: String
    get() = when (this) {
        TileVideoQuality.MAIN -> "main"
        TileVideoQuality.MIDDLE -> "middle"
        TileVideoQuality.LIGHT -> "light"
    }

private fun rank(quality: TileVideoQuality): Int = when (quality) {
    TileVideoQuality.LIGHT -> 0
    TileVideoQuality.MIDDLE -> 1
    TileVideoQuality.MAIN -> 2
}

/** The higher of two qualities: main over middle over light. */
internal fun higherQuality(a: TileVideoQuality, b: TileVideoQuality): TileVideoQuality =
    if (rank(a) >= rank(b)) a else b

/** The lower of two qualities: light under middle under main. */
internal fun lowerQuality(a: TileVideoQuality, b: TileVideoQuality): TileVideoQuality =
    if (rank(a) <= rank(b)) a else b

/**
 * A tile's width in dp as physical pixels: what a picture is actually
 * stretched over, and the unit the shared rule's thresholds are in (the web
 * client's CSS pixels times its device pixel ratio). A density the platform
 * does not give (or gives as nonsense) is read as 1.
 */
internal fun physicalTileWidth(widthDp: Int, density: Float): Int {
    val ratio = if (density.isFinite() && density > 0f) density else 1f
    if (widthDp <= 0) return 0
    return Math.round(widthDp * ratio)
}

/**
 * Which picture a tile shows: the shared rule's answer. [people] counts
 * everyone in the call, this device's own person included; [tileWidthPx] is
 * in physical pixels. While the rule cannot be asked the answer is `main`,
 * which is what the SFU sends a device that never asked.
 */
internal fun tileQualityOf(
    rules: GroupCallRules,
    people: Int,
    tileWidthPx: Int,
    pinnedOrStage: Boolean,
): TileVideoQuality =
    try {
        rules.tileVideoQuality(
            people.coerceAtLeast(0).toUInt(),
            tileWidthPx.coerceAtLeast(0).toUInt(),
            pinnedOrStage,
        )
    } catch (cause: RuntimeException) {
        TileVideoQuality.MAIN
    } catch (cause: LinkageError) {
        // The native library is not loaded.
        TileVideoQuality.MAIN
    }

/**
 * What a tile asks the SFU for, given the rule's answer. The rule's `light`
 * means the light stream alone, which is nothing at all for a person who
 * sends no light copy (a client from before ADR-0070): such a tile asks for
 * `middle`, which the SFU serves as the main track when there is no middle
 * one either.
 */
internal fun askedQuality(quality: TileVideoQuality, hasLight: Boolean): TileVideoQuality =
    if (quality == TileVideoQuality.LIGHT && !hasLight) TileVideoQuality.MIDDLE else quality

/**
 * The track a tile's heavy sink is on for the quality it shows: the main
 * track for `main`; the middle track for `middle`, or the main one while the
 * person has no middle track (the SFU forwards the main track then); none
 * for `light`, where the light track alone is shown.
 */
internal fun <T> heavyTrackFor(quality: TileVideoQuality, main: T?, middle: T?): T? = when (quality) {
    TileVideoQuality.LIGHT -> null
    TileVideoQuality.MIDDLE -> middle ?: main
    TileVideoQuality.MAIN -> main
}

/**
 * After the heavy picture of a tile was replaced by another (a different
 * quality, or the person's middle track arriving), the tile has nothing of
 * it to draw until the new track's first frame. `true` while the light copy
 * should cover that: the heavy source changed, no frame of the new one has
 * arrived yet, and there is a light copy to show.
 */
internal fun coversHeavySwap(awaitingFirstHeavyFrame: Boolean, hasLight: Boolean): Boolean =
    awaitingFirstHeavyFrame && hasLight

// ---- The set this device asks for -----------------------------------------

/** The most entries a `video-wants` frame carries
 *  (`lakko_sfu_proto::MAX_VIDEO_WANTS`). */
internal const val VIDEO_WANTS_MAX: Int = 64

/** The fewest milliseconds between two `video-wants` frames. Chosen. */
internal const val VIDEO_WANTS_MIN_INTERVAL_MS: Long = 200L

/**
 * What every drawn tile wants, by person.
 *
 * A person can be drawn by several tiles at once (the stage and the strip
 * while the layout changes), so each tile reports under its own key and the
 * person's want is the highest of them. A person of the call no tile reports
 * for (another page of the grid, a camera that is off, a collapsed call)
 * wants `light`: the SFU then forwards neither heavy track of theirs.
 *
 * Not thread-safe: one caller, one thread.
 */
internal class VideoWantsBook {
    private val peers = LinkedHashSet<String>()
    private val tiles = HashMap<Any, Pair<String, TileVideoQuality>>()

    /** The people of the call, other than this device: replaces the set. */
    fun setPeers(peerIds: Iterable<String>) {
        peers.clear()
        peers.addAll(peerIds)
    }

    /** A person joined. */
    fun addPeer(peerId: String) {
        peers.add(peerId)
    }

    /** A person left: they are in no later set. */
    fun removePeer(peerId: String) {
        peers.remove(peerId)
    }

    /** How many people are in the call, this device's own person included. */
    fun people(): Int = peers.size + 1

    /**
     * The tile [tile] (any object that is the same for the tile's lifetime)
     * shows [peerId] at [quality]; `null` when it stops showing a picture of
     * them. Returns whether anything changed.
     */
    fun setTile(tile: Any, peerId: String, quality: TileVideoQuality?): Boolean {
        if (quality == null) return tiles.remove(tile) != null
        val next = peerId to quality
        if (tiles[tile] == next) return false
        tiles[tile] = next
        return true
    }

    /** What this device shows of [peerId]: the highest of their drawn tiles,
     *  `light` with none. */
    fun qualityOf(peerId: String): TileVideoQuality {
        var best = TileVideoQuality.LIGHT
        for ((peer, quality) in tiles.values) {
            if (peer == peerId) best = higherQuality(best, quality)
        }
        return best
    }

    /** The whole set: every person of the call, in the order they joined. */
    fun wants(): Map<String, TileVideoQuality> {
        val out = LinkedHashMap<String, TileVideoQuality>()
        for (peerId in peers) out[peerId] = qualityOf(peerId)
        return out
    }

    /** Forget everything: the call is over. */
    fun clear() {
        peers.clear()
        tiles.clear()
    }
}

/**
 * [wants] as the entries of a frame within the protocol's bound. A frame
 * replaces the whole set and a person it leaves out is at `main`, so past
 * [max] people the `main` entries are the ones left out; if the rest still
 * does not fit, the last to join are left out (and are sent in full, as
 * before step 5).
 */
internal fun videoWantsFrame(
    wants: Map<String, TileVideoQuality>,
    max: Int = VIDEO_WANTS_MAX,
): List<Pair<String, TileVideoQuality>> {
    var entries = wants.toList()
    if (entries.size > max) entries = entries.filter { it.second != TileVideoQuality.MAIN }
    if (entries.size > max) entries = entries.take(max)
    return entries
}

/** One line for a `video-wants` frame that went out: `call video wants`
 *  and every person as `peer=quality`. */
internal fun describeVideoWants(frame: List<Pair<String, TileVideoQuality>>): String =
    "call video wants " + frame.joinToString(" ") { (peer, quality) -> "$peer=${quality.wireName}" }

/** The timer and the clock [VideoWantsSender], [SettledWidth] and
 *  [TileControlsReveal] use; replaced in tests. */
internal interface CallTimers {
    fun nowMs(): Long

    /** Run [run] once after [ms]; returns what [cancel] takes. */
    fun schedule(ms: Long, run: () -> Unit): Any

    fun cancel(handle: Any)
}

/**
 * Sends the book's whole set when it changed, coalesced.
 *
 * Every send goes through a timer, so the reports of one pass (every tile of
 * a grid measuring itself in the same frame) become one frame, and two frames
 * are never closer than [minIntervalMs]. [changed] sends only a set that
 * differs from the last one sent; [resend] sends the set whatever it is
 * (after the connection is up, and after a person joins). [send] returns
 * whether the frame went out; one that did not (no connection yet) is tried
 * again at the next call. An empty set is never sent: with nobody else in the
 * call there is nothing to ask for.
 */
internal class VideoWantsSender(
    private val book: VideoWantsBook,
    private val send: (frame: List<Pair<String, TileVideoQuality>>) -> Boolean,
    private val timers: CallTimers,
    private val minIntervalMs: Long = VIDEO_WANTS_MIN_INTERVAL_MS,
) {
    private var lastKey: String? = null
    private var lastSentAt: Long? = null
    private var timer: Any? = null
    private var force = false

    /** The set may have changed. */
    fun changed() {
        schedule()
    }

    /** Send the set at the next opportunity even if it is the one last sent. */
    fun resend() {
        force = true
        schedule()
    }

    /** Stop: nothing is sent after this, and a later use starts afresh. */
    fun reset() {
        timer?.let { timers.cancel(it) }
        timer = null
        lastKey = null
        lastSentAt = null
        force = false
    }

    private fun schedule() {
        if (timer != null) return
        val last = lastSentAt
        val wait = if (last == null) 0L else (last + minIntervalMs - timers.nowMs()).coerceAtLeast(0L)
        // A timer may run its work before `schedule` returns: the call's
        // timers launch on the main thread's immediate dispatcher, where a
        // zero delay does not suspend. Kept as pending only if it has not run
        // yet; otherwise the finished handle stayed in [timer] and every later
        // change was dropped (seen on an emulator 2026-10-23: one `light`
        // frame in a call, then nothing, so a tile that wanted the middle
        // picture never got it).
        var ran = false
        val handle = timers.schedule(wait) {
            ran = true
            timer = null
            flush()
        }
        if (!ran) timer = handle
    }

    private fun flush() {
        val frame = videoWantsFrame(book.wants())
        val key = frame.joinToString(",") { (peer, quality) -> "$peer=${quality.wireName}" }
        if (frame.isEmpty()) {
            lastKey = key
            force = false
            return
        }
        if (!force && key == lastKey) return
        if (!send(frame)) return
        lastKey = key
        force = false
        lastSentAt = timers.nowMs()
    }
}

// ---- A tile's width --------------------------------------------------------

/** How long a tile's width must stand still before it is used, milliseconds.
 *  Chosen. */
internal const val TILE_WIDTH_SETTLE_MS: Long = 300L

/**
 * A tile's width, settled. The first width a tile measures is used at once (a
 * new tile has nothing to keep showing); every later one only after it has
 * stood for [settleMs].
 */
internal class SettledWidth(
    private val timers: CallTimers,
    private val settleMs: Long = TILE_WIDTH_SETTLE_MS,
    private val apply: (width: Int) -> Unit,
) {
    private var measured = false
    private var applied = 0
    private var timer: Any? = null

    /** The tile was measured at [width]. */
    fun observe(width: Int) {
        timer?.let { timers.cancel(it) }
        timer = null
        if (!measured) {
            measured = true
            applied = width
            apply(width)
            return
        }
        if (width == applied) return
        timer = timers.schedule(settleMs) {
            timer = null
            applied = width
            apply(width)
        }
    }

    /** The tile is gone: nothing is applied after this. */
    fun dispose() {
        timer?.let { timers.cancel(it) }
        timer = null
    }
}
