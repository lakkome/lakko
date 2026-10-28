// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import uniffi.lakko_ffi_uniffi.CallBarPhase
import uniffi.lakko_ffi_uniffi.CallGrid
import uniffi.lakko_ffi_uniffi.StageCandidate
import uniffi.lakko_ffi_uniffi.TileVideoQuality

/**
 * How a group call's tiles are laid out (`docs/ux/group-call-grid.md`
 * sections 3 to 6) — the Kotlin twin of the web
 * `apps/web/src/calls/group-call-layout.ts`.
 *
 * Two layouts. **Grid**: everyone the same size, filling the space between
 * the status line and the control bar, with pages instead of a scroll.
 * **Focus**: one large tile (the stage) and everyone else in a strip above
 * it.
 *
 * The geometry, the choice of who is on the stage and whether the outgoing
 * tone plays are the shared rules of `crates/lakko-ui-rules` (`call_group`),
 * reached over UniFFI; the web client reaches the same ones over wasm, so the
 * three clients draw the same grid on the same width. What is here is this
 * client's side of it: the order of the tiles, which page a tile is on, and
 * which layout is showing.
 *
 * Pure: no Compose, no Android. The call screen measures its area and draws
 * the answer.
 */

/**
 * The shared rules this module asks.
 *
 * An interface because the host JVM cannot load `liblakko_ffi_uniffi.so`:
 * production passes [SharedGroupCallRules], which is the binding and nothing
 * else, and a host test passes a fake to check the wiring. The rules
 * themselves are tested where they are written, in
 * `crates/lakko-ui-rules/src/call_group.rs`.
 */
public interface GroupCallRules {
    public fun callGrid(count: UInt, width: UInt, height: UInt): CallGrid

    public fun callStage(people: List<StageCandidate>, pinned: UInt?): UInt?

    public fun ringbackPlays(groupCall: Boolean, phase: CallBarPhase): Boolean

    /** Which picture of a person a tile shows (section 7.6): the people in
     *  the call, the tile's width in physical pixels, and whether it is the
     *  pinned person or the stage. */
    public fun tileVideoQuality(people: UInt, tileWidthPx: UInt, pinnedOrStage: Boolean): TileVideoQuality
}

/** The rules as the core defines them. Never re-implemented here. */
public object SharedGroupCallRules : GroupCallRules {
    override fun callGrid(count: UInt, width: UInt, height: UInt): CallGrid =
        uniffi.lakko_ffi_uniffi.callGrid(count, width, height)

    override fun callStage(people: List<StageCandidate>, pinned: UInt?): UInt? =
        uniffi.lakko_ffi_uniffi.callStage(people, pinned)

    override fun ringbackPlays(groupCall: Boolean, phase: CallBarPhase): Boolean =
        uniffi.lakko_ffi_uniffi.ringbackPlays(groupCall, phase)

    override fun tileVideoQuality(people: UInt, tileWidthPx: UInt, pinnedOrStage: Boolean): TileVideoQuality =
        uniffi.lakko_ffi_uniffi.tileVideoQuality(people, tileWidthPx, pinnedOrStage)
}

/** The grid, as plain numbers in dp. */
public data class GridLayout(
    val cols: Int,
    val rows: Int,
    val tileWidth: Int,
    val tileHeight: Int,
    val gap: Int,
    val perPage: Int,
    val pages: Int,
)

/**
 * Lay [count] equal tiles into an area of [width] by [height] dp: the shared
 * rule's answer. The area is the space between the status line and the
 * control bar, inside the side margins.
 */
public fun gridLayout(rules: GroupCallRules, count: Int, width: Int, height: Int): GridLayout {
    val grid = rules.callGrid(
        count.coerceAtLeast(0).toUInt(),
        width.coerceAtLeast(0).toUInt(),
        height.coerceAtLeast(0).toUInt(),
    )
    return GridLayout(
        cols = grid.cols.toInt(),
        rows = grid.rows.toInt(),
        tileWidth = grid.tileWidth.toInt(),
        tileHeight = grid.tileHeight.toInt(),
        gap = grid.gap.toInt(),
        perPage = grid.perPage.toInt(),
        pages = grid.pages.toInt(),
    )
}

/** Which of the two layouts is showing. */
public enum class GroupLayoutKind { GRID, FOCUS }

/**
 * The layout on screen.
 *
 * A pin always means Focus: pinning someone is asking to see them large.
 * Otherwise the person's own choice for this call, and Grid until they make
 * one. Pinning also makes Focus that choice ([togglePin]), so an unpin
 * leaves the layout as it is.
 */
public fun groupLayoutKind(
    choice: GroupLayoutKind?,
    pinnedId: String?,
    people: List<CallUiState.Participant>,
): GroupLayoutKind {
    // Alone, there is nobody to put in a strip.
    if (people.none { !it.isLocal }) return GroupLayoutKind.GRID
    if (pinnedId != null && people.any { it.id == pinnedId && !it.isLocal }) return GroupLayoutKind.FOCUS
    return choice ?: GroupLayoutKind.GRID
}

/**
 * The order tiles are drawn in: everyone else in the order they joined, then
 * this device's own tile last (section 5.3: a fixed place is how a person
 * finds themself without looking).
 */
public fun tileOrder(people: List<CallUiState.Participant>): List<CallUiState.Participant> =
    people.filter { !it.isLocal } + people.filter { it.isLocal }

/** One page of the grid. */
public data class GridPage(
    val tiles: List<CallUiState.Participant>,
    val page: Int,
    val pages: Int,
)

/**
 * The tiles of one page of the grid.
 *
 * Page 0 always ends with this device's own tile, so it holds one person
 * fewer than [perPage] when the call spills over; the later pages hold
 * everyone else in order. [page] is clamped, so a page that emptied because
 * someone left shows the last page that still has people.
 */
public fun gridPage(people: List<CallUiState.Participant>, perPage: Int, page: Int): GridPage {
    val ordered = tileOrder(people)
    if (perPage <= 0 || ordered.isEmpty()) return GridPage(emptyList(), 0, 0)
    if (ordered.size <= perPage) return GridPage(ordered, 0, 1)
    val own = ordered.filter { it.isLocal }
    val others = ordered.filter { !it.isLocal }
    val firstCount = (perPage - own.size).coerceAtLeast(0)
    val rest = others.drop(firstCount)
    val pages = 1 + (rest.size + perPage - 1) / perPage
    val at = page.coerceIn(0, pages - 1)
    if (at == 0) return GridPage(others.take(firstCount) + own, 0, pages)
    val from = (at - 1) * perPage
    return GridPage(rest.drop(from).take(perPage), at, pages)
}

/** What the stage rule needs beyond the roster. */
public data class StageSpeech(
    /** The one person holding the floor now, or `null`. */
    val activeSpeakerId: String? = null,
    /** When each person last held the floor, by id. */
    val lastSpokeAtMs: Map<String, Long> = emptyMap(),
) {
    public companion object {
        /** Nobody has spoken: the stage falls back to the roster. */
        public val None: StageSpeech = StageSpeech()
    }
}

/**
 * Who is on the stage in Focus, or `null` for an empty call.
 *
 * The shared rule decides: pinned, else speaking now, else the most recent
 * speaker, else the first person with a picture, else the first other
 * person; this device's own tile only when alone.
 */
public fun stagePerson(
    rules: GroupCallRules,
    people: List<CallUiState.Participant>,
    pinnedId: String?,
    speech: StageSpeech,
): CallUiState.Participant? {
    if (people.isEmpty()) return null
    val candidates = people.map { p ->
        StageCandidate(
            isLocal = p.isLocal,
            cameraOn = p.camOn,
            // A share's tile never speaks; its sharer's camera tile does.
            speaking = !p.isScreen && speech.activeSpeakerId == p.id,
            lastSpokeAtMs = if (p.isScreen) 0UL else (speech.lastSpokeAtMs[p.id] ?: 0L).coerceAtLeast(0L).toULong(),
            // ADR-0083: the shared rule ranks a share ahead of the speaker,
            // and a pin ahead of the share.
            isScreen = p.isScreen,
        )
    }
    val pinnedIndex = pinnedId?.let { id -> people.indexOfFirst { it.id == id } }?.takeIf { it >= 0 }
    val index = rules.callStage(candidates, pinnedIndex?.toUInt()) ?: return null
    return people.getOrNull(index.toInt())
}

/**
 * The tiles of a group call with the current share's own tile (ADR-0083,
 * `docs/ux/group-call-grid.md` section 6.3): a separate tile beside its
 * sharer's, first among the others so it is found at once. The sharer's
 * camera tile stays where it was. No tile while the sharer is not in the
 * roster (its state can arrive before it does), and none for this device's
 * own share, which this client never sends.
 *
 * Laid out with everyone else, but never counted as a person: what a tile's
 * picture is asked for counts the people alone.
 */
public fun withShareTile(
    people: List<CallUiState.Participant>,
    share: ScreenShareView?,
): List<CallUiState.Participant> {
    if (share == null || share.isOwn) return people
    val sharer = people.firstOrNull { !it.isLocal && it.id == remoteParticipantId(share.peerId) } ?: return people
    val tile = CallUiState.Participant(
        id = screenShareTileId(share.peerId),
        displayName = sharer.displayName,
        isLocal = false,
        micOn = true,
        camOn = true,
        quality = CallUiState.QualityTier.Good,
        speaking = false,
        isScreen = true,
    )
    val firstRemote = people.indexOfFirst { !it.isLocal }.let { if (it < 0) people.size else it }
    return people.take(firstRemote) + tile + people.drop(firstRemote)
}

/** The sharer's roster id behind a share tile id ([screenShareTileId]). */
public fun sharerParticipantId(shareTileId: String): String =
    remoteParticipantId(shareTileId.removePrefix("screen-"))

/** The strip in Focus: everyone but the stage, in tile order. */
public fun stripPeople(
    people: List<CallUiState.Participant>,
    stage: CallUiState.Participant?,
): List<CallUiState.Participant> =
    tileOrder(people).filter { stage == null || it.id != stage.id }

/** The strip's tile size and the space between its tiles, in dp. */
public data class StripMetrics(val tileWidth: Int, val tileHeight: Int, val gap: Int)

/** The strip's tile size, by the width of the call area. Chosen, not measured. */
public fun stripMetrics(areaWidth: Int): StripMetrics =
    if (areaWidth < 600) StripMetrics(tileWidth = 104, tileHeight = 78, gap = 8)
    else StripMetrics(tileWidth = 160, tileHeight = 90, gap = 10)

/** The stage tile's size in dp. */
public data class StageSize(val width: Int, val height: Int)

/**
 * The stage tile's size: the largest tile of an allowed shape that fits
 * under the strip. Asked of the same shared rule as the grid, for one tile.
 */
public fun stageSize(rules: GroupCallRules, areaWidth: Int, areaHeight: Int): StageSize {
    val strip = stripMetrics(areaWidth)
    val height = (areaHeight - strip.tileHeight - strip.gap).coerceAtLeast(0)
    val grid = gridLayout(rules, 1, areaWidth, height)
    return StageSize(width = grid.tileWidth, height = grid.tileHeight)
}

/**
 * Where one tile of a grid page sits, in dp from the top left of the page's
 * own box (see [gridBoxSize]).
 *
 * A last row that is not full keeps the tile size of the rows above and is
 * centred (section 4.2).
 */
public data class TilePlace(val x: Int, val y: Int)

/** The place of the tile at [index] among [shown] tiles of [grid]. */
public fun tilePlace(grid: GridLayout, shown: Int, index: Int): TilePlace {
    if (grid.cols <= 0) return TilePlace(0, 0)
    val row = index / grid.cols
    val col = index % grid.cols
    val inThisRow = (shown - row * grid.cols).coerceIn(0, grid.cols)
    val rowWidth = inThisRow * grid.tileWidth + (inThisRow - 1).coerceAtLeast(0) * grid.gap
    val fullWidth = grid.cols * grid.tileWidth + (grid.cols - 1) * grid.gap
    return TilePlace(
        x = (fullWidth - rowWidth) / 2 + col * (grid.tileWidth + grid.gap),
        y = row * (grid.tileHeight + grid.gap),
    )
}

/** The size of the box [shown] tiles of [grid] take, in dp: every column
 *  wide, and as many rows tall as are in use. */
public fun gridBoxSize(grid: GridLayout, shown: Int): StageSize {
    if (grid.cols <= 0 || shown <= 0) return StageSize(0, 0)
    val rows = (shown + grid.cols - 1) / grid.cols
    return StageSize(
        width = grid.cols * grid.tileWidth + (grid.cols - 1) * grid.gap,
        height = rows * grid.tileHeight + (rows - 1) * grid.gap,
    )
}

/** Whether a tile shows its connection: fair or worse only (section 7.2). A
 *  dot on every healthy tile says nothing, and for other people it was never
 *  a measurement at all. */
public fun connectionIsWorthShowing(tier: CallUiState.QualityTier): Boolean =
    tier == CallUiState.QualityTier.Fair ||
        tier == CallUiState.QualityTier.Poor ||
        tier == CallUiState.QualityTier.Bad
